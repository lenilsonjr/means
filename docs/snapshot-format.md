# Snapshot format

A snapshot is a complete, hashed and atomic read of the books for external tools. Use it to give an auditor, an accountant or a script the full ledger with its evidence.

```sh
means export --format snapshot --output DIR [--entity E]... [--as-of YYYY-MM-DD] [--json]
means export verify DIR [--json]
```

`--entity` takes a name, an id or a uid. Repeat it to select more entities. Without it, the snapshot holds every entity. `DIR` must not exist.

## Files in the output directory

| File | Content |
|---|---|
| `manifest.json` | `{"schema_version":1,"generator":{"name":"means","version":"<crate version>","commit":"<git hash if available via env or "unknown">"},"exported_at":"<RFC3339 UTC>","scope":{"entity_uids":[...],"as_of":null\|"YYYY-MM-DD"},"posting_policy":"normal\|draft_only","files":{"<name>":{"sha256":"...","bytes":N}, ... including every document file},"counts":{"entities","accounts","entries","postings","drafts","parties","documents","statement_lines","tax_codes","tax_events","invoices","settlements","filings","intervault_links"},"chain_heads":{"<entity_uid>":{"seq":N,"hash":"..."}},"control_totals":{"<entity_uid>":{"debits":Money,"credits":Money}}}` |
| `entities.json` | array of `{"uid","name","kind","legal_form","country","currency","fiscal_year_end","lock_date","incorporated_on","registrations":{},"archived_at","classifications":[{"jurisdiction","classification","effective_from","effective_to","note"}],"group":{"group_uid","group_name","parent_entity_uid","ownership_bp","control","effective_from","effective_to"}\|null}` |
| `accounts.json` | array of `{"uid","entity_uid","parent_uid","path","name","type","subtype","class","commodity","precision","placeholder","closed_at","statutory_code","tax_treatment","intercompany","external_ids":{}}` |
| `entries.jsonl` | one object per line, status `posted` or `void` only: `{"uid","entity_uid","date","payee","payee_uid","description","notes","status","origin","reverses_uid","counterpart_uid","reviewed_at","reviewed_by","seq","prev_hash","hash","tags":[],"metadata":{},"postings":[{"uid","account_uid","quantity":Money,"amount":Money,"rate","rate_source","memo","metadata":{},"external_id","fingerprint","reconciled_at","statement_line_ids":[],"document_uids":[],"tax_event_uids":[]}],"document_uids":[]}` — `amount` is the functional book value, `quantity` the native quantity; `reviewed_by` from the audit event of the confirmation when available, else null |
| `drafts.jsonl` | same shape, status `draft` |
| `parties.json` | array of `{"uid","name","kind","country","tax_ids":{},"aliases":[],"relationships":[{"entity_uid","kind","ownership_bp","effective_from","effective_to","basis"}]}` (payees; if payees have no uid column, use `"uid": "payee-<id>"` and say so in the doc) |
| `documents.json` + `documents/<ab>/<sha256>` | array of `{"uid","entity_uid","sha256","filename","mime","size","source","received_at","metadata":{},"path","links":[{"entry_uid","posting_uid","statement_line_id","role"}]}`; copy each stored file into the snapshot |
| `statement_lines.json` | array of `{"id","import_uid","account_uid","entity_uid","date","amount":Money,"description","counterparty","external_id","fingerprint","posting_uids":[]}` |
| `rates.json` | array of `{"date","base","quote","rate","source"}` from the prices table |
| `tax_codes.json` | array of the tax code rows with uids and account uids |
| `tax_events.jsonl` | one per line with `uid, entity_uid, entry_uid, posting_uid, tax_code_uid, code, kind, tax_point, base:Money, tax:Money, recoverable:Money, withheld:Money, native_base, native_tax, native_commodity, party_uid, document_ref, rule_version, corrects_event_uid, metadata` |
| `invoices.json` | array of invoices with lines and settlements (uids, Money values, dates, status, party_uid, document_uid, recognition_entry_uid) |
| `filings.json` | array of filing rows with uids and `amends_uid` |
| `intervault_links.json` | array of links `{"uid","entry_a_uid","entry_b_uid","kind","amount_a":Money,"amount_b":Money,"rate","note","status"}` — only if a table `intervault_links` exists in the database (check `sqlite_master`); otherwise omit the file and set the count to 0 |
| `books.beancount` | the existing Beancount export of the same scope |

Money is `{"minor":"<integer string>","commodity":"<code>","precision":N}`. This is the existing `Money` JSON form of the engine. Rates are decimal strings. No floats anywhere.

## Rules

- One read transaction (`BEGIN` deferred read; `rusqlite` transaction) for everything, including counts and totals; no pagination limits (read every row).
- Scope: all entities by default, or `--entity` repeated; `--as-of DATE` limits entries, drafts, tax events, statement lines and documents to that date and marks it in the manifest. Archived entities and closed accounts are included when in scope.
- Write into `DIR.tmp-<random>`, fsync files, then rename to `DIR` (refuse if `DIR` exists). Compute sha256 of every written file for the manifest; write the manifest last.
- `control_totals` per entity: sum of positive functional amounts (debits) and sum of negative (credits, as a positive number) over booked entries in scope; `chain_heads` from the entity's last chained entry.
- A `--verify` subcommand `means export verify DIR --json` rehashes every file and checks the manifest, counts and that debits equal credits per entity; exit 1 on any mismatch.

## Field notes

These notes make the contract exact where the table is short.

### Manifest

- `files` lists every file of the directory except `manifest.json`. Keys are paths relative to `DIR` with `/` separators, for example `documents/3f/3f9a…`.
- `generator.version` is the version of the `means-core` crate. `generator.commit` is the `MEANS_GIT_COMMIT` environment variable at build time, or `"unknown"`.
- `scope.entity_uids` lists the uids of the exported entities, also when the export selects every entity.
- `counts.entries` counts the lines of `entries.jsonl`. `counts.postings` counts the postings in `entries.jsonl` only. Draft postings do not count. `counts.settlements` counts the settlements in `invoices.json`.
- `chain_heads` has the sequence and the hash of the last chained entry of each entity. It uses the whole chain, also with `--as-of`. An entity without a chained entry has no key.
- `control_totals` has one key for each exported entity. An entity without booked entries has zero totals. Booked entries are the `posted` and `void` entries in `entries.jsonl`. A void entry and its reversal both count, so they cancel out in the balances but not in the totals.

### Entities and accounts

- `group` is the group membership in effect on the scope date: the `--as-of` date, or the export date. It is `null` when the entity has no membership on that date.
- `classifications` lists every classification period of the entity.
- `path` is the full account path from the type root, for example `Assets:Bank:Operating`.

### Entries and drafts

- The files sort the entries by date, then by internal id.
- `metadata` of an entry holds the fields of the entry that the contract does not name: `refund_of_uid`, `template_uid`, `template_version`, `posted_at` and `created_at`. A field without a value is absent.
- `tags` are `key:value` strings, or `key` for a tag without a value.
- `rate` is the stored decimal string, or `null`.
- `reviewed_by` is the actor of the last `confirm` audit event of the entry. It is `null` when no such event exists.
- `document_uids` of an entry lists the documents linked to the entry. `document_uids` of a posting lists the documents linked to the posting.
- `statement_line_ids` and `tax_event_uids` list only records that are in the snapshot.

### Parties

- A party is a payee of an exported entity. Payees have a `uid` column, so `uid` is that value.
- `aliases` lists the alias texts of the payee.

### Documents

- A document is in the snapshot when its entity is in scope. With `--as-of`, the first ten characters of `received_at` must be on or before the date.
- `path` is the path of the copied file in the snapshot. The snapshot never holds a local path of the ledger.
- Two documents with the same content share one file.
- `links` lists only links whose targets are in the snapshot. A link to an entry, a posting or a statement line outside the snapshot is left out.
- The export reads each stored file and compares its hash with the row. A missing file or a different hash stops the export.

### Statement lines

- A statement line is in the snapshot when its account belongs to an exported entity. A line without an account is left out.
- With `--as-of`, a line without a date is left out.
- `amount` is in the commodity of the account.
- `external_id` is the bank reference of the line. `counterparty` is the `counterparty` field of the raw record, or an empty string.
- `posting_uids` holds the uid of the matched posting when that posting is in the snapshot.

### Tax codes and tax events

- A tax code row has `uid`, `entity_uid`, `code`, `name`, `kind`, `rate_bp`, `tax_account_uid`, `recoverable_bp`, `jurisdiction`, `effective_from`, `effective_to` and `active`.
- A tax event is in the snapshot when its entry is in `entries.jsonl` or `drafts.jsonl`. With `--as-of`, its `tax_point` must be on or before the date.
- `native_base` and `native_tax` are Money in `native_commodity`, or `null`.

### Invoices, filings and links

- An invoice has `uid`, `entity_uid`, `kind`, `direction`, `party_uid`, `series`, `number`, `issue_date`, `tax_point`, `due_date`, `commodity`, `net`, `tax`, `gross`, `withheld`, `status`, `credits_invoice_uid`, `recognition_entry_uid`, `document_uid`, `external_ref`, `metadata`, `lines` and `settlements`.
- An invoice line has `position`, `description`, `quantity` (a decimal string), `unit_price`, `net`, `tax_code`, `tax` and `account_uid`.
- A settlement has `uid`, `entry_uid`, `posting_uid`, `date`, `amount`, `kind`, `fx_difference_entry_uid` and `credit_note_uid`.
- `--as-of` does not filter invoices, filings, tax codes, rates or links. Their references can point to entries after the date.
- A filing row has `uid`, `entity_uid`, `jurisdiction`, `filing_code`, `period_start`, `period_end`, `status`, `snapshot_digest`, `chain_head_seq`, `chain_head_hash`, `artifact_digest`, `receipt_ref`, `submitted_at`, `notes`, `amends_uid`, `created_at` and `updated_at`.
- An intervault link is in the snapshot when one of its two entries belongs to an exported entity.
- `rates.json` holds every row of the prices table. `base` is the priced commodity and `quote` is the currency of the price.

## Verify

`means export verify DIR` does these checks:

1. The manifest has `schema_version` 1. Other versions fail.
2. Each file in `files` exists, and its SHA-256 and size agree with the manifest.
3. No other file is in the directory.
4. The counts agree with the files.
5. `entries.jsonl` holds only `posted` and `void` entries. `drafts.jsonl` holds only drafts.
6. Each document in `documents.json` has its file in the snapshot.
7. Each entry in `entries.jsonl` balances.
8. For each entity, the debits equal the credits, and `entries.jsonl` gives the same totals.

With `--json`, the command prints one object: `{"schema_version":1,"ok":true|false,"files_checked":N,"problems":[...]}`. The exit code is 1 when a check fails.
