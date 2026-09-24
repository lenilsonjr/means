# Agent interface

An AI agent or a script can write to the ledger through `means entry`. The agent creates drafts and reads entries. A human posts each draft after a review. The `means policy` commands control whether automatic routes can post.

The commands use the accounting core directly. They do not need a running server.

## Workflow

1. The agent creates a draft with `means entry draft`.
2. The agent or the human runs `means entry propose` to validate the draft and preview it.
3. The human reads the preview.
4. The human runs `means entry confirm` with the digest from the preview. The engine posts the draft only when the digest still matches.

A change to the draft after the proposal changes the digest. The confirmation then fails with `stale_proposal`. Propose the draft again and review the new preview.

## Commands

| Command | Writes | Human only |
| --- | --- | --- |
| `means entry draft --entity <name\|id\|uid> (--json-file <path> \| --stdin)` | A draft | No |
| `means entry propose <uid>` | Nothing | No |
| `means entry confirm <uid> --digest <proposal_digest> [--actor <name>]` | Posts the draft | Yes |
| `means entry void <uid> --reason <text> [--date YYYY-MM-DD]` | A reversal | Yes |
| `means entry list --entity <e> [--status draft\|unreviewed\|posted\|void] [--since D] [--until D] [--limit N] [--json]` | Nothing | No |
| `means entry show <uid> [--json]` | Nothing | No |
| `means policy show [--json]` | Nothing | No |
| `means policy set normal\|draft-only [--json]` | The policy | Yes |
| `means close check --entity <e> --from D --to D [--json]` | Nothing | No |
| `means close lock --entity <e> --through D --actor A --reason R [--force] [--json]` | The lock date and an audit event | Yes |
| `means close unlock --entity <e> --to D --actor A --reason R [--amend] [--json]` | The lock date, an audit event and amendments | Yes |
| `means filing record --entity <e> --jurisdiction J --code C --from D --to D [--status prepared] [--snapshot-digest X] [--artifact-digest Y] [--notes T] [--json]` | A filing | Only for `submitted` and `accepted` |
| `means filing status <uid> --status S [--receipt R] [--at D] [--actor A] [--reason T] [--json]` | The filing status | Only for `submitted` and `accepted` |
| `means filing list --entity <e> [--jurisdiction J] [--code C] [--json]` | Nothing | No |
| `means invoice add --entity <e> (--json-file <path> \| --stdin) [--recognize] [--json]` | An invoice and a draft recognition entry | No |
| `means invoice list --entity <e> [--open] [--direction sales\|purchase] [--as-of D] [--json]` | Nothing | No |
| `means invoice show <uid> [--json]` | Nothing | No |
| `means invoice settle <uid> --entry <uid> [--posting <uid>] --date D --amount X [--kind K] [--fx-account <path>] [--control-account <path>] [--json]` | A settlement, an entry conversion and a draft FX entry | No |
| `means invoice credit-note --entity <e> --credits <uid> (--json-file <path> \| --stdin) [--recognize] [--json]` | A credit note and its settlement | No |
| `means invoice cancel <uid> --reason R [--actor A] [--json]` | The invoice status and an audit event | Yes |

`--actor` defaults to `$USER`, except on `close lock` and `close unlock`, which need it. See [close, lock and filings](data-model.md#close-lock-and-filings) for the findings and the filing statuses. The `unreviewed` status selects posted entries that no human has confirmed.

## Draft document

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

- Give net amounts. The engine adds the tax lines, and the balancing posting absorbs them. In the example, the bank takes 109.00 and `Liabilities:GST output` takes -9.00.
- `base_amount` and `tax_amount` are unsigned decimal strings in the functional currency. The engine gives them the sign of the posting.
- A posting without `quantity` cannot carry a tax block.
- See [tax codes and tax events](data-model.md#tax-codes-and-tax-events) for the lines of each kind.

`propose` then also returns `tax_events`. Each event has `tax_code_uid`, `code`, `kind`, `rate_bp`, `tax_point`, `posting_index` (an index into `postings`), `base`, `tax`, `recoverable`, `withheld`, `native_base`, `native_tax`, `party_uid`, `document_ref` and `rule_version`. The tax lines are in `postings`. Their metadata has `tax_line`. The digest covers the tax lines and the events. `show --json` returns the events of the entry in `tax_events`.

## Output

Each command writes exactly one JSON object to stdout. Each object has `"schema_version": 1` and `"ok"`. Records use `uid` strings. Money uses the `Money` form: `{"minor": "-2340", "commodity": "USD", "precision": 2}`. Rates are decimal strings. Statement lines have no uid, so evidence uses `statement_line_id`.

`draft`:

```json
{"schema_version": 1, "ok": true, "entry": {"uid": "…", "status": "draft", "date": "2026-04-02", "entity_uid": "…", "hash": null}, "audit_event_id": 41}
```

`propose` returns the preview at the top level: `entry_uid`, `entity_uid`, `date`, `status`, `payee`, `description`, `notes`, `tags`, `functional_currency`, `postings`, `tax_events`, `totals`, `warnings` and `proposal_digest`. Each posting has `account_uid`, `account_path`, `quantity`, `amount`, `rate`, `rate_source`, `memo` and `metadata`. `totals` has `debit`, `credit` and `balance` in the functional currency.

`proposal_digest` is the SHA-256 of the canonical JSON of the preview without the digest. Canonical JSON has sorted object keys and no whitespace.

`confirm` returns `entry`, `actor`, `proposal_digest` and `audit_event_id`. The audit event records the actor, the action and the digest. The entry gets its review mark.

`void` returns `entry`, `reversal`, `already_void` and `evidence_lines_returned`.

`close check --json` returns `report` and `report_digest`. The report has `entity_uid`, `period_start`, `period_end`, `lock_date`, `ok`, `error_count`, `warning_count`, `findings` and `totals`. Each finding has `code`, `severity` (`error` or `warning`), `message`, `entry_uid`, `account_uid`, `count` and `amount`. Its `ok` field tells whether the period can close. `close lock` returns `entity_uid`, `previous_lock`, `lock_date`, `forced`, `actor`, `reason`, `finding_counts`, `report`, `report_digest` and `audit_event_id`. `close unlock` returns `entity_uid`, `previous_lock`, `lock_date`, `actor`, `reason`, `amendments` and `audit_event_id`.

`filing record` and `filing status` return `filing`. `filing status` also returns `previous_status`. `filing list` returns `entity_uid`, `count` and `filings`. A filing has `uid`, `entity_uid`, `jurisdiction`, `filing_code`, `period_start`, `period_end`, `status`, `snapshot_digest`, `chain_head_seq`, `chain_head_hash`, `artifact_digest`, `receipt_ref`, `submitted_at`, `notes`, `amends_uid`, `created_at` and `updated_at`.

`list --json` returns `entity_uid`, `count`, `total` and `entries`. `show --json` returns `entry`. Each entry has its postings, `evidence`, `review` (`state` and `reviewed_at`), `hash`, and the uids of linked entries. Without `--json`, `list` and `show` print text.

## Invoice document

`means invoice add` and `means invoice credit-note` read one JSON object. See [invoices, open items and settlements](data-model.md#invoices-open-items-and-settlements) for the accounting rules.

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
- `account`, `receivable_account` and `rate` are optional. `rate` is the functional currency per unit of the invoice commodity.
- A credit note can omit `direction` and `commodity`. It takes them from the invoice that it credits.
- The engine refuses unknown fields.

`add` returns `invoice`, `lines`, `recognition_entry` and `audit_event_id`. An invoice has `uid`, `entity_uid`, `direction`, `party_uid`, `party_name`, `number`, `series`, `issue_date`, `tax_point`, `due_date`, `commodity`, `net`, `tax`, `gross`, `withheld`, `settled`, `open`, `status`, `kind`, `credits_invoice_uid`, `recognition_entry_uid`, `document_uid`, `external_ref`, `metadata`, `created_at` and `updated_at`. `list` returns `entity_uid`, `as_of`, `count` and `invoices`; each invoice also has `days_overdue`. `show` returns `invoice`, `lines`, `settlements`, `credit_notes`, `linked_entries`, `documents` and `possible_duplicates`. `settle` returns `invoice`, `settlement`, `converted`, `fx_difference`, `fx_difference_entry` and `audit_event_id`. `credit-note` returns `credit_note`, `lines`, `recognition_entry`, `settlement`, `invoice` and `audit_event_id`. `cancel` returns `invoice`, `actor` and `reason`.

## Errors

A refusal writes one JSON object to stdout and exits with code 1:

```json
{"schema_version": 1, "ok": false, "error": {"code": "stale_proposal", "message": "…"}}
```

| Code | Meaning |
| --- | --- |
| `unbalanced` | The postings do not sum to zero in the functional currency. |
| `missing_rate` | A posting needs an exchange rate that the ledger does not have. Add the rate, then revalue the draft. |
| `locked_period` | The date is on or before the lock date of the entity. |
| `unknown_account` | The account does not exist in the entity. |
| `not_leaf_account` | The account groups other accounts. |
| `not_found` | The entry or the entity does not exist. |
| `stale_proposal` | The draft changed after the proposal, or the digest is wrong. |
| `agent_not_allowed` | The command is a human action and `MEANS_AGENT=1` is set. |
| `close_blocked` | The close check found errors. Resolve them, or lock with `--force`. |
| `lock_not_forward` | A lock must move the lock date forward. Use `close unlock` to move it back. |
| `filed_period` | A submitted or accepted filing covers a day that the unlock reopens. Unlock with `--amend`. |
| `invalid_transition` | The filing cannot move from its status to the requested status. |
| `duplicate` | The entity already has an invoice with this direction, series and number, or the entry already settles the invoice. |
| `over_settlement` | The settlement or the credit note is larger than the open balance, or the withholding is larger than the withheld amount. |
| `settled` | The invoice has settlements, so it cannot be cancelled. |
| `invalid_input` | The input is not valid. The message tells you why. |
| `internal` | The engine failed for an unexpected reason, such as a database error. |

Command-line usage errors, such as a missing argument, come from the argument parser. They print text and exit with code 2.

## Agent sessions

Set `MEANS_AGENT=1` in the environment of an agent. The engine then refuses `entry confirm`, `entry void`, `policy set`, `close lock`, `close unlock` and `invoice cancel` with `agent_not_allowed`. It also refuses `filing record` and `filing status` when the status is `submitted` or `accepted`.

`MEANS_AGENT=1` is a workflow aid. It is not an access control. A restricted client needs a separate service boundary.

## Posting policy

The ledger-wide setting `posting_policy` has two values. The engine stores it in the `settings` table. A ledger without the setting uses `normal`.

- `normal`: rules, schedules, the matcher and imports can post, as before.
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
