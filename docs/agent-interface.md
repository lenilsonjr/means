# Agent interface

An AI agent or a script can read the ledger and write drafts through the `means` CLI. A human posts each draft after a review. The `means policy` commands control whether automatic routes can post.

The commands use the accounting core directly. They do not need a running server. Each command takes `--db <path>` (default: `~/.means/ledger.db`, or `$MEANS_DB`).

## Workflow

1. The agent creates a draft with `means entry draft`.
2. The agent or the human runs `means entry propose` to validate the draft and preview it.
3. The human reads the preview.
4. The human runs `means entry confirm` with the digest from the preview. The engine posts the draft only when the digest still matches.

A change to the draft after the proposal changes the digest. The confirmation then fails with `stale_proposal`. Propose the draft again and review the new preview.

## JSON contract

Most commands in this document write exactly one JSON object to stdout. The object has `"schema_version": 1` and `"ok"`. Records use `uid` strings. Money uses the `Money` form: `{"minor": "-2340", "commodity": "USD", "precision": 2}`. The minor units are a string. Rates are decimal strings. Statement lines have no uid, so evidence uses `statement_line_id`.

The commands have three output styles:

| Style | Commands | Success | Refusal |
| --- | --- | --- | --- |
| Compact | `entry`, `policy`, `rates set\|list\|import`, `close`, `filing`, `invoice`, `intervault`, `report` (except `expenses`) | One line of JSON with `--json`. Text without it. `entry draft`, `entry propose`, `entry confirm` and `entry void` always print JSON. | Always one line of JSON with an error object. |
| Indented | `entity set\|archive\|unarchive\|classify\|facts`, `group`, `party`, `payee set`, `account set`, `tax` | Always JSON. Indented by default, one line with `--json`. | Always JSON with an error object. |
| Evidence | `doc`, `export --format snapshot`, `export verify` | JSON with `--json`, text without it. | The error object with `--json`. A text error on stderr without it. |

A refusal exits with code 1. See [Errors](#errors). Command-line usage errors, such as a missing argument, come from the argument parser. They print text and exit with code 2.

`accounts`, `entity list`, `status`, `entries`, `report expenses`, `budget list`, `import lines` and `refund` are older commands. Their JSON has no `schema_version`. `import`, `post`, `void`, `rule`, `chart` and the bank pull commands print text. They are not part of this contract, except for their `MEANS_AGENT=1` refusal, which is the JSON error object.

In the tables below, the column `MEANS_AGENT=1` tells whether the engine refuses the command in an agent session. See [Agent sessions](#agent-sessions).

## Reading

| Command | Writes | `MEANS_AGENT=1` |
| --- | --- | --- |
| `means entry list --entity <e> [--status draft\|unreviewed\|posted\|void] [--since D] [--until D] [--limit N \| --all] [--json]` | Nothing | Allowed |
| `means entry show <uid> [--json]` | Nothing | Allowed |
| `means entity list [--all] [--json]` | Nothing | Allowed |
| `means entity facts <entity> [--as-of D] [--json]` | Nothing | Allowed |
| `means group list [--json]` | Nothing | Allowed |
| `means group show <group> [--as-of D] [--json]` | Nothing | Allowed |
| `means party list --entity <e> [--as-of D] [--related-only] [--json]` | Nothing | Allowed |
| `means party missing --entity <e> --need KEY [--json]` | Nothing | Allowed |
| `means accounts [--entity <id>] [--json]` | Nothing | Allowed |
| `means entries [--entity <e>] [--account <path>] [--status S] [--from D] [--to D] [--search T] [--limit N] [--json]` | Nothing | Allowed |
| `means status [--json]` | Nothing | Allowed |
| `means verify` | Nothing | Allowed |

`<e>` and `<entity>` take an entity id, name or uid. A name matches an active entity before an archived entity with the same name. `D` is a date `YYYY-MM-DD`.

`entry list` returns at most 2000 entries. `--all` removes the limit: the engine reads every matching entry, page by page. The `unreviewed` status selects posted entries that no human has confirmed. The snapshot export is the complete read of the books. Use it for a full copy.

`entry list --json` returns `entity_uid`, `count`, `total` and `entries`. `entry show --json` returns `entry`. Each entry has `uid`, `entity_uid`, `date`, `status`, `origin`, `payee`, `description`, `notes`, `tags`, `hash`, `posted_at`, `created_at`, `review` (`state` and `reviewed_at`), `reverses_uid`, `reversed_by_uid`, `counterpart_uid`, `intervault_link_uids`, `postings`, `tax_events` and `evidence`. Without `--json`, `list` and `show` print text.

`entity facts` returns `as_of`, `entity`, `flag_history`, `classifications` and `group`. `entity` has the facts, the registrations, the `address` and the `flags` on the `--as-of` date. `flag_history` lists every dated flag value: `id`, `key`, `value`, `effective_from` and `effective_to`. `group show` returns the ownership tree of the group on the date. `party list` returns `entity`, `as_of`, `related_only` and `parties`. Each party has `uid`, `name`, `entity_uid`, `active`, `country`, `kind`, every tax id in `tax_ids`, `residence`, `citizenship`, `related_party` and `relationships`.

`party missing` lists the payees of the entity that have entries (except void entries) or invoices, but no tax id under `--need`, for example `GST`, `VAT` or `EIN`. The key ignores case. It returns `entity`, `need` and `parties`. Each party has the fields of `party list` without the relationships, and `entry_count`, `invoice_count` and `last_date`.

`accounts --json` returns an array. Each account has `id` (the integer that `import --account` takes), `uid`, `entity_id`, `path`, `code`, `type`, `subtype`, `placeholder`, `commodity`, `statutory_code`, `tax_treatment`, `intercompany`, `country`, `iban`, `bic`, `institution`, `location` and `balance`. `entity list --json` returns an array of the active entities. `--all` adds the archived entities, after the active ones. Each entity has `id`, `uid`, `name`, `kind`, `country`, `currency`, `fiscal_year_end`, `lock_date` and `archived`. `status` prints one JSON object, with or without `--json`. `entries --json` prints one JSON object per line and the count on stderr.

## Entity facts, groups and parties

These commands describe the books. They never change postings. See [entity facts, groups and parties](data-model.md#entity-facts-groups-and-parties).

| Command | Writes | `MEANS_AGENT=1` |
| --- | --- | --- |
| `means entity set <entity> [--name N] [--legal-form T] [--fye MM-DD] [--incorporated-on D] [--registration KEY=VALUE]... [--address KEY=VALUE]... [--flag KEY=VALUE]... [--from D] [--json]` | Entity facts, the address, the flags and audit events | Allowed |
| `means entity archive <entity> --actor A --reason R [--json]` | The archive time and an audit event | Refused |
| `means entity unarchive <entity> --actor A --reason R [--name N] [--json]` | The archive time, the name and an audit event | Refused |
| `means entity classify <entity> --jurisdiction J --classification C --from D [--to D] [--note T] [--json]` | A classification and an audit event | Allowed |
| `means group add <name> [--json]` | A group | Allowed |
| `means group member add --group G --entity <e> [--parent <e>] [--ownership-bp N] [--no-control] [--from D] [--json]` | A membership | Allowed |
| `means group member close --group G --entity <e> --to D [--json]` | The end of a membership | Allowed |
| `means payee set <payee> [--entity <e>] [--country CC] [--tax-id KEY=VALUE]... [--kind person\|company\|authority\|other] [--residence CC] [--citizenship CC] [--json]` | Payee facts and an audit event | Allowed |
| `means party set <payee> ...` | The same as `payee set` | Allowed |
| `means account set <account> --entity <e> [--country CC] [--iban X] [--bic X] [--institution T] [--location T] [--statutory-code CODE] [--counterparty P] [--json]` | Account identifiers, the counterparty and an audit event | Allowed |
| `means party relate --payee P --entity <e> --kind K --from D [--ownership-bp N] [--basis T] [--json]` | A relationship | Allowed |
| `means party unrelate --payee P --entity <e> --kind K --to D [--json]` | The end of a relationship | Allowed |

`entity set` returns `entity` and `flag_history`. Its `flags` are the values of today. `entity archive` and `entity unarchive` return `entity` (`uid`, `name`, `kind`, `currency`, `archived` and `archived_at`), `actor`, `reason` and `audit_event_id`. An archive of an archived entity, or an unarchive of an active entity, is refused with `invalid_input`. See [archived entities](data-model.md#archived-entities). `entity classify` returns `entity` and `classification`. `group add` returns `group`. `group member add` and `close` return `group` and `member`. `payee set` and `party set` return `payee`. `account set` returns `entity` and `account`. The account has `id`, `uid`, `path`, `type`, `commodity`, `country`, `iban`, `bic`, `institution`, `location`, `statutory_code`, `counterparty_uid` and `counterparty_name`. `party relate` and `unrelate` return `relationship`. `--to` is the day after the last day of the period.

`entity set` changes all its facts in one transaction. A refusal changes nothing.

- `--name N` renames the entity. Another active entity cannot have the name, ignoring case. An archived entity can.

- `--address KEY=VALUE` sets one part of the address. The keys are `street`, `street2`, `city`, `region`, `postal_code` and `country`. `country` is an ISO 3166-1 alpha-2 code. Another key is refused. An empty value removes the part.
- `--flag KEY=VALUE` sets one flag. A key has lowercase letters, digits, `_`, `.` and `-`, for example `sg.gst_registered`. `true` and `false` become booleans. An integer becomes a number. Any other value stays a string. See the [flag key convention](data-model.md#entity-address-and-flags).
- Without `--from`, a flag is undated and applies on every date. An empty value removes it.
- With `--from D`, each `--flag` of the command starts a dated value on D. An open value of the same key that starts earlier ends on D. Another overlap is a `conflict`. An empty value ends the open value on D. To give flags different dates, run the command once for each date.
- A key with dated values refuses an undated change.

`account set` takes an account path, a uid or an id. The IBAN loses its spaces and must pass the check digits. The BIC has 8 or 11 characters, and the first six are letters. An empty value clears an identifier. `location` is free text, for example the state where an asset is. `--statutory-code` sets the code of the account in a national chart or a reporting taxonomy. The code must not contain whitespace. A group account (a placeholder) refuses a code that is not empty; an empty value clears the code on any account. The engine does not check it against a chart. An empty value clears it. `--counterparty` takes a payee id, uid or name of the entity. An empty value clears the counterparty. The close check reports an intercompany account whose counterparty is outside the group as `related_party_balance`, not as `intercompany_out_of_balance`. See [close, lock and filings](data-model.md#close-lock-and-filings).

## Drafting

| Command | Writes | `MEANS_AGENT=1` |
| --- | --- | --- |
| `means entry draft --entity <e> (--json-file <path> \| --stdin)` | A draft, its tax events, an optional inter-vault link and an audit event | Allowed |
| `means entry propose <uid>` | Nothing | Allowed |
| `means entry confirm <uid> --digest <proposal_digest> [--actor <name>]` | Posts the draft, the review mark and an audit event | Refused |
| `means entry void <uid> --reason <text> [--date D] [--cascade [--actor A]] [--dry-run]` | A reversal. With `--cascade`, also the reversals of the linked entries. With `--dry-run`, nothing. | Refused, except with `--dry-run` |
| `means rates set --date D --base B --quote Q --rate R [--source S] [--no-revalue] [--json]` | A rate and revalued postings | Allowed |
| `means rates list [--base B] [--quote Q] [--from D] [--to D] [--limit N] [--json]` | Nothing | Allowed |
| `means rates list --derive B/Q --date D [--json]` | Nothing | Allowed |
| `means rates fetch [--from D] [--pairs B/Q,...] [--to D] [--no-revalue]` | ECB rates, stored cross rates and revalued postings | Allowed |
| `means rates import <csv> [--source S] [--no-revalue] [--json]` | Rates and revalued postings | Allowed |

`--actor` defaults to `$USER`.

`draft` returns `entry` and `audit_event_id`, and `intervault_link` when the document has an [inter-vault block](#inter-vault-block):

```json
{"schema_version": 1, "ok": true, "entry": {"uid": "…", "status": "draft", "date": "2026-04-02", "entity_uid": "…", "hash": null}, "audit_event_id": 41}
```

`propose` returns the preview at the top level: `entry_uid`, `entity_uid`, `date`, `status`, `payee`, `description`, `notes`, `tags`, `functional_currency`, `postings`, `tax_events`, `totals`, `warnings` and `proposal_digest`. Each posting has `account_uid`, `account_path`, `quantity`, `amount`, `rate`, `rate_source`, `memo` and `metadata`. `totals` has `debit`, `credit` and `balance` in the functional currency. Each tax event has `tax_code_uid`, `code`, `kind`, `rate_bp`, `tax_point`, `posting_index` (an index into `postings`), `base`, `tax`, `recoverable`, `withheld`, `native_base`, `native_tax`, `party_uid`, `document_ref` and `rule_version`. The tax lines are in `postings`. Their metadata has `tax_line`.

`proposal_digest` is the SHA-256 of the canonical JSON of the preview without the digest. Canonical JSON has sorted object keys and no whitespace. The digest covers the tax lines and the tax events.

`confirm` returns `entry`, `actor`, `proposal_digest` and `audit_event_id`. The audit event records the actor, the action and the digest. The entry gets its review mark.

`void` returns `entry`, `reversal`, `already_void`, `applied`, `cascade`, `needs_cascade`, `linked_entries` and `evidence_lines_returned`. A `--dry-run` result has `applied` false, the entry as it is now, and a `reversal` with the date only (its `uid` is null). With `--cascade`, the result also has `actor`. See [void of a linked entry](#void-of-a-linked-entry).

`means void <id> --json` returns `entry`, `reversal`, `evidence_lines`, `applied`, `already_void`, `cascade`, `needs_cascade` and `linked_entries`. Without `--yes`, the command is a preview and saves nothing. `applied` is then false, and `entry` is the stored entry with its real status. The preview rolls the reversal back, so `reversal` has no id and no uid: it is `{"uid": null, "status": "posted", "date": D}`, as in `means entry void --dry-run`. The preview also lists the linked entries in `linked_entries`. With `--yes`, `applied` is true, and `entry` has the status `void` and `reversed_by_id`.

`rates set` stores one rate: one unit of the base costs `rate` units of the quote on the date. The row goes in the prices table, which `means rates fetch` also fills. The default source is `manual`. Drafts, imports and invoices find the row through the rate lookup: directly, as the inverse, or through a cross rate. A direct row comes before an inverse row. `rates import` reads a CSV file with the header `date,base,quote,rate`. It writes all rows or none, and a refusal names the bad line. `set` and `import` then revalue the postings booked without a rate, unless you give `--no-revalue`.

`rates set` returns `rate` and `revalued`. A rate has `id`, `date`, `base`, `quote`, `rate` (a decimal string) and `source`. `revalued` has `fixed`, `failed` and `first_error`. `rates list` returns `count` and `rates`, newest first. `rates import` returns `written`, `source` and `revalued`.

The rate lookup uses the latest row on or before the date. A row older than 400 days does not count. When the ledger has no direct row and no inverse row, the lookup derives a cross rate through a currency that quotes both sides. EUR comes first, because ECB rates quote EUR. Each leg uses its latest row within the same 400 days. A missing leg gives no rate, and the posting gets `rate_source` `missing`. A cross rate has the `rate_source` `cross:<SOURCE>`, where `<SOURCE>` is the source of the legs in capitals, for example `cross:ECB`. Two legs with different sources give `cross:<A>+<B>`.

`rates list --derive B/Q --date D` returns `rate` with `date`, `base`, `quote`, `rate`, `source` and `derived`. `source` is `exact`, `nearest`, `inverse` or `cross:<SOURCE>`. `derived` is true for a cross rate. When the lookup finds no rate, the command fails with `missing_rate`.

`rates fetch` stores the ECB rate of every ECB currency against EUR from `--from`. `--pairs USD/SGD,GBP/SGD` also stores a cross rate for each pair on each day from `--from` to `--to` (default: today) that has a stored rate. The command skips a day where the pair has a direct or an inverse row. A stored cross row keeps its `cross:<SOURCE>` source, so reports, packs and snapshots see the pair. `rates fetch` needs a network, and it prints text.

### Draft document

```json
{
  "date": "2026-04-02",
  "payee": "Sample Taxi",
  "description": "Airport transfer",
  "notes": "",
  "tags": ["trip=sample"],
  "postings": [
    { "account": "Assets:Bank:Operating", "quantity": "-23.40", "commodity": "USD", "memo": "card" },
    { "account": "Expenses:Travel" }
  ],
  "metadata": { "source": "receipt-scan" }
}
```

- `account` is an account path in the entity, or an account uid. The account must be a leaf account and open.
- Exactly one posting can omit `quantity`. That posting balances the entry.
- Write every number as a decimal string. The engine refuses JSON numbers.
- `quantity` is in the account commodity. When `commodity` names another commodity, the engine converts the value at the entry-date rate.
- `amount` is the value in the functional currency of the entity. `rate` is the functional currency per unit of the account commodity. When you give both, the quantity times the rate must equal the amount after rounding.
- Tags use `key=value` or `key:value`. The engine stores them as `key:value`.
- The posting `metadata` goes on the posting. The entry `metadata` goes in the audit event of the draft.
- The engine refuses unknown fields.

The draft has the origin `agent`. It waits for review.

### Tax block

A posting can carry a `tax` block. The document can also carry a `tax` block at the top level. That block applies to each income or expense posting without its own block.

```json
{
  "date": "2026-03-10",
  "payee": "Rio Verde SL",
  "postings": [
    { "account": "Assets:Bank:Operating" },
    { "account": "Income:Sales", "quantity": "-100.00",
      "tax": { "code": "SR", "document_ref": "INV-0001", "party": "Rio Verde SL" } }
  ]
}
```

| Field | Required | Meaning |
| --- | --- | --- |
| `code` | Yes | A tax code of the entity. It must be in effect on the tax point. |
| `base_amount` | No | The taxable base. Default: the functional amount of the posting. |
| `tax_amount` | No | The tax. Default: base × rate / 10000, rounded half-even to the currency precision. |
| `tax_point` | No | `YYYY-MM-DD`. Default: the entry date. |
| `party` | No | A payee id, uid or name of the entity. Default: the payee of the entry. |
| `document_ref` | No | The invoice or document number. |

- Give net amounts. The engine adds the tax lines, and the balancing posting absorbs them. In the example, with a 9% output code, the bank takes 109.00 and the tax account takes -9.00.
- `base_amount` and `tax_amount` are unsigned decimal strings in the functional currency. The engine gives them the sign of the posting.
- A posting without `quantity` cannot carry a tax block.
- See [tax codes and tax events](data-model.md#tax-codes-and-tax-events) for the lines of each kind.

### Inter-vault block

The document can carry an `intervault` block at the top level. The engine then links the new draft to an existing entry of another entity, in the same transaction. The new draft is entry A of the link.

```json
{
  "date": "2026-03-02",
  "payee": "Blue Harbor LLC",
  "postings": [
    { "account": "Assets:Loan to Blue Harbor", "quantity": "1350.00" },
    { "account": "Assets:Bank:Operating" }
  ],
  "intervault": { "with": "<entry uid in the other vault>", "kind": "loan", "note": "Working capital" }
}
```

| Field | Required | Meaning |
| --- | --- | --- |
| `with` | Yes | The uid of an existing entry of another entity. It cannot be void. |
| `kind` | No | `transfer`, `capital`, `loan`, `interest`, `dividend`, `service`, `recharge`, `settlement` or `other`. Default: `transfer`. |
| `note` | No | Free text saved with the link. |

- An entry of the same entity, a void entry or an existing link refuses the draft with `invalid_input`. The engine then writes nothing.
- The link is `draft`. It becomes `posted` when both entries are posted. See [inter-vault links](data-model.md#inter-vault-links) for the pair lifecycle.

## Evidence

| Command | Writes | `MEANS_AGENT=1` |
| --- | --- | --- |
| `means doc add <file> --entity <e> [--source email\|upload\|api\|filing\|import\|other] [--received D] [--link-entry <uid>] [--link-line <id>] [--role R] [--meta KEY=VALUE]... [--json]` | A stored file, a document, its links and audit events | Allowed |
| `means doc list --entity <e> [--unlinked] [--since D] [--json]` | Nothing | Allowed |
| `means doc link <document uid> (--entry <uid> \| --line <id>) [--role R] [--json]` | A link and an audit event | Allowed |
| `means doc show <document uid> [--json]` | Nothing | Allowed |
| `means doc verify [--json]` | Nothing | Allowed |
| `means import --account <id> [--source generic_csv] --map date=<col>,amount=<col>,description=<col>[,counterparty=<col>][,external_id=<col>] [--date-format <strftime>] [--decimal-comma] <file>` | An import, its statement lines and entries | Allowed |
| `means import lines <import id> [--status S] [--json]` | Nothing | Allowed |

`doc add` returns `created`, `document`, `links` and `stored_path`. The same bytes twice return the first document with `created: false`. `doc list` returns `entity_uid`, `count` and `documents`. `doc link` returns `link`. `doc show` returns `document`, `links`, `stored_path` and `exists`. `doc verify` returns `checked`, `passed`, `missing` and `mismatched`. It exits with code 1 and an error when a file is missing (`not_found`) or has another hash (`hash_mismatch`). See [documents](data-model.md#documents).

`import --map` reads the file as `generic_csv` and builds the same column mapping as the TUI. See [column maps](import-formats.md#column-maps-for-other-csv-files). `import` prints text.

## Tax

| Command | Writes | `MEANS_AGENT=1` |
| --- | --- | --- |
| `means tax code add --entity <e> --code C --kind K --from D [--name T] [--rate-bp N] [--tax-account <path\|uid>] [--recoverable-bp N] [--jurisdiction J] [--json]` | A tax code version and an audit event | Allowed |
| `means tax code list --entity <e> [--as-of D] [--json]` | Nothing | Allowed |
| `means tax code close --entity <e> --code C --to D [--json]` | The end of the open version | Allowed |
| `means tax summary --entity <e> --from D --to D [--json]` | Nothing | Allowed |
| `means tax events --entity <e> --from D --to D [--code C] [--json]` | Nothing | Allowed |

`K` is `output`, `input`, `reverse_charge`, `exempt`, `out_of_scope`, `withholding` or `zero_rated`. `tax code add` and `close` return `entity` and `tax_code`. `tax code list` returns `entity`, `as_of`, `count` and `tax_codes`. A tax code has `uid`, `entity_uid`, `code`, `name`, `kind`, `rate_bp`, `recoverable_bp`, `tax_account_uid`, `tax_account_path`, `jurisdiction`, `effective_from`, `effective_to`, `active`, `rule_version` and `created_at`.

`tax summary` returns `entity`, `from`, `to`, `functional_currency` and `rows`. Each row has `code`, `kind`, `rate_bp`, `base`, `tax`, `recoverable`, `withheld` and `count`. `tax events` returns `entity`, `from`, `to`, `code`, `count` and `tax_events`. Each event has `uid`, `entry_uid`, `entry_status`, `posting_uid`, `tax_code_uid`, `code`, `kind`, `rate_bp`, `tax_point`, `base`, `tax`, `recoverable`, `withheld`, `native_base`, `native_tax`, `party_uid`, `document_ref`, `rule_version`, `corrects_event_uid`, `metadata` and `created_at`. Both commands read posted and void entries only. `entry show --json` returns the same events in `tax_events`.

## Invoices

| Command | Writes | `MEANS_AGENT=1` |
| --- | --- | --- |
| `means invoice add --entity <e> (--json-file <path> \| --stdin) [--recognize] [--json]` | An invoice, its lines, an optional draft recognition entry and an audit event | Allowed |
| `means invoice list --entity <e> [--open] [--direction sales\|purchase] [--as-of D] [--json]` | Nothing | Allowed |
| `means invoice show <uid> [--json]` | Nothing | Allowed |
| `means invoice settle <uid> --date D --amount X [--entry <uid>] [--posting <uid>] [--kind K] [--fx-account <path>] [--control-account <path>] [--json]` | A settlement, an optional entry conversion and an optional draft FX entry | Allowed |
| `means invoice credit-note --entity <e> --credits <uid> (--json-file <path> \| --stdin) [--recognize] [--json]` | A credit note, its settlement and an optional draft recognition entry | Allowed |
| `means invoice cancel <uid> --reason R [--actor A] [--json]` | The invoice status and an audit event | Refused |

`K` is `payment`, `withholding`, `fee`, `fx_difference`, `write_off` or `refund`. The default is `payment`. A `payment` and a `refund` need `--entry`. See [invoices, open items and settlements](data-model.md#invoices-open-items-and-settlements) for the accounting rules.

### Invoice document

`invoice add` and `invoice credit-note` read one JSON object.

```json
{
  "direction": "purchase",
  "party": "Sample Supplier Ltd",
  "series": "A",
  "number": "2026-017",
  "issue_date": "2026-03-02",
  "tax_point": "2026-03-02",
  "due_date": "2026-04-01",
  "commodity": "EUR",
  "net": "1000.00",
  "tax": "210.00",
  "gross": "1210.00",
  "withheld": "0.00",
  "lines": [
    { "description": "Annual licence", "quantity": "1", "unit_price": "1000.00", "tax_code": "VAT21", "tax": "210.00", "account": "Expenses:Software" }
  ],
  "payable_account": "Liabilities:Accounts Payable",
  "tax_account": "Assets:Tax:Input VAT",
  "document": "<document uid>",
  "external_ref": "",
  "metadata": {},
  "recognize": true
}
```

- `party` is a payee uid or a payee name in the entity.
- Write every amount as a decimal string in the invoice commodity. An amount with more decimals than the commodity allows fails.
- `tax_point` defaults to the issue date. The recognition entry uses it as its date.
- A line needs `net` or `unit_price`. `quantity` defaults to 1.
- A line `tax_code` that is a tax code of the entity on the tax point puts a tax block on the line posting of the recognition entry. The engine records one tax event for the line and adds the tax lines. `tax_account` is then not necessary for that line. An absent or unknown code uses `tax_account`.
- `account`, `receivable_account` and `rate` are optional. `rate` is the functional currency per unit of the invoice commodity.
- A credit note can omit `direction` and `commodity`. It takes them from the invoice that it credits.
- The engine refuses unknown fields.

`add` returns `invoice`, `lines`, `recognition_entry` and `audit_event_id`. An invoice has `uid`, `entity_uid`, `direction`, `party_uid`, `party_name`, `number`, `series`, `issue_date`, `tax_point`, `due_date`, `commodity`, `net`, `tax`, `gross`, `withheld`, `settled`, `open`, `status`, `kind`, `credits_invoice_uid`, `recognition_entry_uid`, `document_uid`, `external_ref`, `metadata`, `created_at` and `updated_at`. `list` returns `entity_uid`, `as_of`, `count` and `invoices`. Each invoice in a list also has `days_overdue`. `show` returns `invoice`, `lines`, `settlements`, `credit_notes`, `linked_entries`, `documents` and `possible_duplicates`. `settle` returns `invoice`, `settlement`, `converted`, `fx_difference`, `fx_difference_entry` and `audit_event_id`. `credit-note` returns `credit_note`, `lines`, `recognition_entry`, `settlement`, `invoice` and `audit_event_id`. `cancel` returns `invoice`, `actor` and `reason`.

## Inter-vault links

| Command | Writes | `MEANS_AGENT=1` |
| --- | --- | --- |
| `means intervault link --a <entry uid> --b <entry uid> [--kind K] [--note T] [--json]` | A link and an audit event | Allowed |
| `means intervault unlink <link uid> --reason R [--actor A] [--force-locked] [--json]` | Removes a link, and an audit event | Allowed for two drafts. Refused with `--actor` or `--force-locked`. |
| `means intervault list --from D --to D [--group G \| --entity <e>] [--status draft\|posted\|void] [--json]` | Nothing | Allowed |
| `means intervault exceptions --entity <e> --from D --to D [--json]` | Nothing | Allowed |

`K` is one of the kinds of the [inter-vault block](#inter-vault-block). The default is `transfer`.

`link` returns `link`. `list` returns `from`, `to`, `count`, `reconciled_count` and `links`. A link has `uid`, `kind`, `status`, `rate`, `note`, `entry_a`, `entry_b`, `reconciled`, `created_at` and `updated_at`. `link` and the draft result also have `warnings`. Each side has `entry_uid`, `entity_uid`, `entity_name`, `date`, `entry_status`, `amount` (stored with the link) and `current_amount` (on the entry now). `unlink` returns the removed `link` with `entry_a_uid`, `entry_b_uid`, `amount_a` and `amount_b`, and `reason`, `actor`, `force_locked`, `locked_entry_uids` and `audit_event_id`. `exceptions` returns `entity_uid`, `from`, `to`, `count` and `postings`. Each posting has `entry_uid`, `posting_uid`, `date`, `payee`, `account_uid`, `account_path`, `quantity`, `amount` and `memo`.

### Remove a link

`unlink` needs `--reason`. While both entries are drafts, it needs nothing more. When an entry is posted or void, the removal is a human action:

- Give `--actor A`. Without it, the command fails with `invalid_input`.
- When a posted or void entry is dated on or before the lock date of its entity, the command fails with `locked_period`. Give `--force-locked` to remove the link anyway. `locked_entry_uids` then names those entries.
- The audit event of the link records the actor, the reason, both entry uids, `force_locked` and `locked_entry_uids`.

The entries keep their postings. Without the link, they are not reconciled for intercompany purposes: `close check` reports them in `intercompany_out_of_balance`, and `intervault exceptions` lists their intercompany postings. A void of one entry then does not touch the other.

### Void of a linked entry

A void of an entry with an inter-vault link that is not void reaches the other entries of the link group. The group can include entries of more than one other entity.

- The preview (`means void <ID>` without `--yes`, or `means entry void <uid> --dry-run`) lists them in `linked_entries`. It reverses the requested entry only, and `needs_cascade` is true. A preview with `--cascade` never applies the cascade either: it lists the linked entries with their `locked` flag and saves nothing.
- Without `--cascade`, the apply fails with `invalid_input`. The message names each linked entry and says "void the linked entries with cascade, or remove each link first to void only this entry". Nothing changes. Over gRPC, `VoidJournalEntry` without `cascade` fails with `FAILED_PRECONDITION` and the same message. Set `cascade` to true in the request to void the link group.
- With `--cascade`, the engine voids each posted linked entry in the same transaction, with the same reason, and marks the links `void`. A linked draft stays a draft. The lock date of each entity applies to its own reversal: a locked period fails with `locked_period` and names the entity.
- To void one entry only, remove each link first with `means intervault unlink`. The lock date of the other entity then does not apply.

Each linked entry has `link_uid`, `link_kind`, `entity_uid`, `entity_name`, `entry_id`, `entry_uid`, `date`, `status`, `amount` (the intercompany side now), `cascade_action` (`void` for a posted entry, `keep` for a draft), `locked` (a cascade reversal falls in a locked period) and `voided` (this void voided the entry).

The engine computes the link group once. Each entry of the group gets one reversal, and each link gets one audit event, also when the links make a cycle. The audit event of a cascade void has `cascade` true and `linked_entry_uids`. The void event of each linked entry has `cascaded_from`, and the event of each link has `cascade` true and `voided_from`. `--actor A` (default `$USER`) goes on each of these events. A gRPC void records the actor of the `means-actor` request metadata, or `grpc` without it. Each void event also records the reason.

## Close and filings

| Command | Writes | `MEANS_AGENT=1` |
| --- | --- | --- |
| `means close check --entity <e> --from D --to D [--json]` | Nothing | Allowed |
| `means close lock --entity <e> --through D --actor A --reason R [--force] [--json]` | The lock date and an audit event | Refused |
| `means close unlock --entity <e> --to D --actor A --reason R [--amend] [--json]` | The lock date, an audit event and amendments | Refused |
| `means close questions --entity <e> --jurisdiction J [--as-of D] [--json]` | Nothing | Allowed |
| `means filing record --entity <e> --jurisdiction J --code C --from D --to D [--status S] [--snapshot-digest X] [--artifact-digest Y] [--notes T] [--actor A] [--reason T] [--json]` | A filing and an audit event | Refused for `submitted` and `accepted` |
| `means filing status <uid> --status S [--receipt R] [--at D] [--actor A] [--reason T] [--json]` | The filing status and an audit event | Refused for `submitted` and `accepted` |
| `means filing list --entity <e> [--jurisdiction J] [--code C] [--json]` | Nothing | Allowed |

`close lock` and `close unlock` need `--actor`. On `filing record` and `filing status`, `--actor` defaults to `$USER`. The default status of `filing record` is `prepared`. See [close, lock and filings](data-model.md#close-lock-and-filings) for the findings and the filing statuses.

`close check --json` returns `report` and `report_digest`. The report has `entity_uid`, `period_start`, `period_end`, `lock_date`, `ok`, `error_count`, `warning_count`, `findings` and `totals`. Each finding has `code`, `severity` (`error`, `warning` or `info`), `message`, `entry_uid`, `account_uid`, `count` and `amount`. A finding about inter-vault links also has `link_uids`. `warning_count` does not count info findings. Its `ok` field tells whether the period can close. `close lock` returns `entity_uid`, `previous_lock`, `lock_date`, `forced`, `actor`, `reason`, `finding_counts`, `report`, `report_digest` and `audit_event_id`. `close unlock` returns `entity_uid`, `previous_lock`, `lock_date`, `actor`, `reason`, `amendments` and `audit_event_id`.

`close questions` reads the year-end question list of the jurisdiction from `crates/means-core/src/close_questions.json`. Lists exist for `SG`, `US`, `ES` and `PT`. Another code is refused with `invalid_input`. With `--json`, it returns `entity_uid`, `jurisdiction`, `as_of`, `answered` and `missing`. `answered` lists every flag of the entity on the date whose key starts with the lowercase code and a dot, with `key`, `value` and `question` (`null` for a key that is not in the list). `missing` lists the questions without a flag on the date, with `key`, `kind` (`bool`, `date`, `integer` or `text`) and `question`. Ask a human the missing questions once. Then store each answer with `means entity set <e> --flag KEY=VALUE [--from D]`.

`filing record` and `filing status` return `filing`. `filing status` also returns `previous_status`. `filing list` returns `entity_uid`, `count` and `filings`. A filing has `uid`, `entity_uid`, `jurisdiction`, `filing_code`, `period_start`, `period_end`, `status`, `snapshot_digest`, `chain_head_seq`, `chain_head_hash`, `artifact_digest`, `receipt_ref`, `submitted_at`, `notes`, `amends_uid`, `created_at` and `updated_at`.

## Reports

| Command | Writes | `MEANS_AGENT=1` |
| --- | --- | --- |
| `means report trial-balance --entity <e> --from D --to D [--json]` | Nothing | Allowed |
| `means report balance-sheet --entity <e> --as-of D [--book-values \| --market-values] [--json]` | Nothing | Allowed |
| `means report income --entity <e> --from D --to D [--json]` | Nothing | Allowed |
| `means report cash-flow --entity <e> --from D --to D [--json]` | Nothing | Allowed |
| `means report general-ledger --entity <e> --from D --to D [--account <path\|uid>] [--json]` | Nothing | Allowed |
| `means report open-items --entity <e> --as-of D [--direction sales\|purchase] [--json]` | Nothing | Allowed |
| `means report tax-summary --entity <e> --from D --to D [--json]` | Nothing | Allowed |
| `means report net-worth --entity <e> --as-of D [--json]` | Nothing | Allowed |
| `means report expenses --entity <e> [--from D] [--to D] [--tag T] [--group-by class\|tag\|payee] [--json]` | Nothing | Allowed |

Dates are inclusive. Without `--json`, each command prints an aligned table. `report expenses` is older. Its JSON has no `schema_version`.

Each report object has `report`, `entity` (`uid`, `name` and `currency`), `functional_currency` and `basis`. `basis` has `valuation`, `statuses` and `note`. `valuation` is `book`, `market` or `invoice`. `statuses` lists the entry statuses that the report reads. Every report reads `posted` and `void` entries, so a void entry and its reversal cancel each other. Only `cash-flow` also reads `draft` entries. Amounts are booked values in the functional currency, unless the field says otherwise. Account rows have `uid` and `path`.

| Report | Fields |
| --- | --- |
| `trial_balance` | `from`, `to`, `count`, `accounts`, `totals` and `balanced`. Each account has `uid`, `path`, `type`, `statutory_code`, `opening`, `debits`, `credits` and `closing`. Balances are debit positive. `opening` sums every entry before `from`. `totals` has the same four amounts. `balanced` is true when the debits equal the credits and the balances sum to zero. |
| `balance_sheet` | `as_of`, `assets`, `liabilities`, `equity`, `total_assets`, `total_liabilities`, `total_equity`, `liabilities_and_equity` and `balanced`. Each section has `accounts` and `total`. Each account has `uid`, `path`, `name`, `type`, `depth`, `placeholder`, `statutory_code`, `quantity` (account commodity), `book_value`, `market_value` and `value`. A parent row includes its children, so the totals add the rows with depth 0. Equity has a `Retained earnings` row without a uid: every income and expense entry up to the date. The default is `book`. `--market-values` gives `market`: holding accounts use the market price on the date when a price exists, and an `Unrealised gains` row takes the difference. |
| `income_statement` | `from`, `to`, `revenue`, `expenses` and `result`. `revenue` and `expenses` have `accounts` and `total`. `expenses` also has `by_class`: `class` and `amount`, with `unclassified` for accounts without a class. Each account has `uid`, `path`, `name`, `depth`, `placeholder`, `class`, `statutory_code` and `amount`. Amounts are positive in the normal direction of the account. `result` is revenue minus expenses. |
| `cash_flow` | `from`, `to`, `method` (`direct`), `opening`, `days`, `totals`, `posted_closing` and `projected_closing`. The report sums the postings on accounts with the subtype `bank`, `cash`, `card` or `wallet`. Each day has `date`, `posted_in`, `posted_out`, `draft_in`, `draft_out` and `projected_balance`. A transfer between two such accounts shows as both in and out. |
| `general_ledger` | `from`, `to`, `count` and `accounts`. Each account has `uid`, `path`, `type`, `commodity`, `statutory_code`, `opening_balance`, `debits`, `credits`, `closing_balance` and `postings`. Each posting has `entry_uid`, `posting_uid`, `date`, `status`, `payee`, `description`, `contra_path`, `debit`, `credit`, `amount` and `running_balance`. Quantities and balances use the account commodity. `amount` is the functional value. Without `--account`, the report lists each leaf account with postings or an opening balance. |
| `open_items` | `as_of`, `direction`, `count`, `items` and `totals`. Each item has `invoice_uid`, `direction`, `series`, `number`, `party_uid`, `party_name`, `issue_date`, `due_date`, `gross`, `settled`, `open` and `days_overdue`, in the invoice commodity. `totals` has one row for each direction and commodity. Settlements dated after `as_of` do not count. |
| `tax_summary` | `from`, `to`, `count` and `rows`, as in `means tax summary`. |
| `net_worth` | `as_of`, `count`, `accounts`, `assets`, `liabilities` and `net_worth`. The valuation is `market`: holding accounts use the market price when a price exists. |

## Export

| Command | Writes | `MEANS_AGENT=1` |
| --- | --- | --- |
| `means export --format snapshot --output DIR [--entity <e>]... [--include-archived] [--as-of D] [--json]` | A new snapshot directory. The ledger does not change. | Allowed |
| `means export verify DIR [--json]` | Nothing | Allowed |
| `means export [--format beancount] [--output FILE] [--entity <e>]... [--include-archived] [--as-of D]` | A new Beancount file, or stdout | Allowed |

`export --format snapshot --json` returns `output` and `counts`. `export verify --json` returns `ok`, `files_checked` and `problems`. `export verify` exits with code 1 when a check fails. A failed snapshot export with `--json` prints the error object and exits with code 1. A failed export without `--json` prints a text error on stderr and exits with code 1. See the [snapshot format](snapshot-format.md) and [Beancount export](export.md).

## Posting policy

| Command | Writes | `MEANS_AGENT=1` |
| --- | --- | --- |
| `means policy show [--json]` | Nothing | Allowed |
| `means policy set normal\|draft-only [--json]` | The policy | Refused |

`policy show --json` returns `posting_policy`. `policy set --json` returns `posting_policy` and `previous`.

The ledger-wide setting `posting_policy` has two values. The engine stores it in the `settings` table. A ledger without the setting uses `normal`.

- `normal`: rules, schedules, the matcher and imports can post.
- `draft_only`: no automatic route posts. Each route creates or keeps a draft instead.

Under `draft_only`:

- A rule that matches an imported statement line creates a draft with the rule's account.
- A rule that runs again on an Uncategorized draft keeps it a draft.
- A scheduled template with automatic posting creates drafts.
- The matcher links a statement line to a draft and keeps the entry a draft.
- An import that would create or update a posted entry creates or keeps a draft.
- `CreateJournalEntry`, `CreateSimpleEntry`, `UpdateJournalEntry` and capture over gRPC store a posted request as a draft. The status in the response shows the stored status.
- `CreateTransfer` creates a draft pair. Both halves are written in one transaction.
- A refund link creates a draft refund.
- The audit event of each held entry records `requested_status` and `posting_policy`.

The engine reads the policy inside the transaction that writes each entry. An unknown stored value stops the write, so a damaged setting never permits posting.

These routes still post an existing draft under `draft_only`, because each one is a human decision on one entry: `means entry confirm`, `means post`, `PostJournalEntry`, `PostDraft` and the categorize proposal of `ReviewPosting`. A void reversal also stays posted, because it corrects an entry that a human already posted.

## Errors

A refusal writes one JSON object to stdout and exits with code 1:

```json
{"schema_version": 1, "ok": false, "error": {"code": "stale_proposal", "message": "…"}}
```

Every command group uses the same codes: the compact commands, the indented commands, `doc`, `export --format snapshot --json` and the `MEANS_AGENT=1` refusal of the older commands. A command prints no other code.

| Code | Meaning |
| --- | --- |
| `invalid_input` | The input is not valid. The message tells you why. This code includes a void of an entry with inter-vault links without `--cascade`, a lock that does not move the lock date forward, a filing status change that the steps do not permit, and a record that exists already outside invoices and documents. |
| `not_found` | The entry, the entity or another record does not exist. |
| `unbalanced` | The postings do not sum to zero in the functional currency. |
| `missing_rate` | A posting needs an exchange rate that the ledger does not have, or `rates list --derive` finds no rate. Add the rate, then revalue the draft. |
| `locked_period` | The date is on or before the lock date of the entity. A void with `--cascade` also gives this code when the period of a linked entity is locked. `intervault unlink` gives it for an entry in a locked period without `--force-locked`. The message names the entity. |
| `unknown_account` | The account does not exist in the entity. |
| `not_leaf_account` | The account groups other accounts. |
| `stale_proposal` | The draft changed after the proposal, or the digest is wrong. |
| `agent_not_allowed` | The command is a human action and `MEANS_AGENT=1` is set. See [Agent sessions](#agent-sessions). |
| `duplicate` | The entity already has an invoice with this direction, series and number, the entry already settles the invoice, or the document already has this link. |
| `over_settlement` | The settlement or the credit note is larger than the open balance, or the withholding is larger than the withheld amount. |
| `settled` | The invoice has settlements, so it cannot be cancelled. |
| `close_blocked` | The close check found errors. Resolve them, or lock with `--force`. |
| `filed_period` | A submitted or accepted filing covers a day that the unlock reopens. Unlock with `--amend`. |
| `entity_archived` | The entity is archived, so its books refuse every write. Run `means entity unarchive` first. The older text commands print the same code at the start of their error text. |
| `invalid_path` | A file name is not safe for the document store, or the snapshot directory exists already. |
| `hash_mismatch` | A stored file does not have the hash that the ledger records. |
| `io_error` | A file could not be read or written. |
| `internal` | The engine failed for an unexpected reason, such as a database error. |

Older versions printed other codes. They map to this table: `invalid_argument`, `conflict`, `parse_error`, `json_error`, `lock_not_forward` and `invalid_transition` are now `invalid_input`. `locked` is now `locked_period`. `database_error` is now `internal`. The messages did not change.

## Agent sessions

Set `MEANS_AGENT=1` in the environment of an agent. The engine then refuses each command that posts, voids, deletes or reverts, with `agent_not_allowed`. The refusal comes before the command opens the ledger. One check in the engine does this for every command:

- `entry confirm` and `entry void` (a void with `--dry-run` is allowed)
- `post`
- `void --yes` (a preview without `--yes` is allowed)
- `draft delete`
- `rollback`
- `rematch` and `rematch --cross-source --apply` (a cross-source preview is allowed)
- `refund link`
- `chart remap` without `--dry-run`, and `chart merge`
- `entity migrate-currency --apply` (a preview is allowed)
- `entity archive` and `entity unarchive`
- `policy set`
- `close lock` and `close unlock`
- `invoice cancel`
- `intervault unlink` with `--actor` or `--force-locked`
- `filing record` and `filing status` with the status `submitted` or `accepted`

The older commands print text on success. Their `MEANS_AGENT=1` refusal is the JSON error object. `entity set` has no lock date option, so the lock date moves only through `close lock` and `close unlock`.

The engine does not refuse other commands. `import` can post through rules. Set the posting policy to `draft-only` to hold those entries as drafts.

`MEANS_AGENT=1` is a workflow aid. It is not an access control. A restricted client needs a separate service boundary.
