# Accounting model

means keeps a chart of accounts, journal, statement evidence, and audit trail for each vault. Reports read those records to show balances, income, expenses, and net worth.

## Vaults and accounts

A vault is one entity’s set of books. Each entity has a functional currency and a lock date. Personal and company vaults report their own balances. Overview shows the selected vault; consolidated reports are an explicit reporting choice.

Every account belongs to one entity and one of five types:

| Type | Typical use | Increases with |
| --- | --- | --- |
| Asset | Bank account, cash, investment | Debit |
| Liability | Credit card, loan | Credit |
| Equity | Opening balances, capital | Credit |
| Income | Salary, sales | Credit |
| Expense | Groceries, rent, software | Debit |

Accounts form a hierarchy. Placeholder accounts group children; postings use leaf accounts. Spending categories are expense accounts. Each account has a commodity and its precision. Commodities can represent currencies or assets.

## Journal entries and postings

A journal entry records one dated event. Its postings belong to accounts in the same entity. The sum of their book values is zero: debits are positive and credits are negative.

Each posting stores:

- Its account and native quantity.
- Its amount in the entity’s functional currency.
- The conversion rate and source when applicable.
- A memo and retained metadata.
- External references, fingerprints, and reconciliation state when supplied.

A USD vault can hold a EUR bank account. Its postings retain EUR quantities and USD book values. Quantities and book values use integer minor units with explicit commodity precision. For example, 549 minor units with precision 2 means 5.49.

A transfer between entities uses linked entries, one in each set of books. Currency differences post to FX gain or loss. A full refund reverses the original expense splits at their booked values and records any exchange difference separately.

Historical changes to a vault’s functional currency use the [currency migration workflow](currency-migration.md). It preserves native account quantities and requires a reviewed preview and backup.

## Entry lifecycle

Draft entries wait for a booking decision. Posting confirms their accounting structure. Human review is a separate state, so a rule can post an entry that still waits for review.

The `draft_only` posting policy stops rules, schedules, the matcher and imports from posting. An agent can create drafts through `means entry`, and a human confirms them. See the [agent interface](agent-interface.md).

A split distributes a movement across several accounts. For an imported expense, the bank posting and its statement link stay intact while the expense postings change. Preview the complete transaction before confirming it in the TUI.

A void retains the original postings and creates a linked reversal. The reversal negates native quantities and book values. The original entry’s evidence returns to the unmatched queue. Lock dates apply to the correction date.

Lock dates protect closed periods. Reconciled postings retain their account and quantity during recategorization. Operations that would alter protected accounting data fail with an explanation.

## Imports and statement evidence

An import records the source, file checksum, options, result counts, and statement lines. Each statement line retains the provider’s raw record, date, amount, currency, description, and reference. Its processing state identifies whether it was matched, created, skipped, marked as a duplicate, or left for review.

Matching links a statement line to a posting. The posting holds the accounting decision; the statement line holds the bank evidence. Several confirmed sources can evidence one posting through cross-source reconciliation.

Deduplication uses the source’s identity rules. Enable Banking lines with a stable reference use that account-scoped reference; lines without one fall back to a fingerprint. Other formats can use fingerprints when their export IDs change. A fingerprint incorporates the date, amount, description, account, and occurrence within the file.

Rules choose categories, payees, tags, or templates from statement data. Refund candidates and ambiguous cross-source matches require review. See [import formats and recovery](import-formats.md) for each source’s behavior and confirmation steps.

## Documents

A document is evidence that is not a bank line: an invoice, a receipt, a contract, a statement, or a filed form. Each document belongs to one entity. Its row records the SHA-256 hash, the original file name, the MIME type, the size, the source, the received date, and free metadata. The source is `email`, `upload`, `api`, `filing`, `import`, or `other`.

The file content lives in a content-addressed store next to the ledger file:

```
<ledger directory>/documents/<first two hex digits of the hash>/<sha256>
```

The row keeps this path relative to the ledger directory. The original file name is metadata only. The store rejects a file name that contains a path separator or `..`. The MIME type comes from the file extension; an unknown extension gives `application/octet-stream`.

The store writes a file in this order:

1. Write the bytes to a temporary file in the target directory.
2. Flush and sync the temporary file.
3. Read the file back and compare its hash with the expected hash.
4. Rename the file into place.
5. Insert the row and its audit record in one transaction.

If the insert fails, the store removes the file, unless the file existed before. The store never overwrites a stored file. One entity keeps one row per hash: a second add of the same bytes returns the first row and reports `created: false`. Two entities can share one stored file.

A document link attaches a document to a journal entry, a posting, or a statement line of the same entity. A link has an optional role, for example `invoice` or `receipt`. The same document, target, and role can occur only once. The evidence of an entry includes links to the entry, to its postings, and to the statement lines that match it. The audit log records every new document and every new link.

A link follows its target. An entry edit moves a posting link to the replacement posting. If the posting has no replacement, the link moves to the entry. A deleted draft, a rolled-back import, or a merged duplicate removes its links. The document stays, and `means doc list --unlinked` shows it again.

No command deletes a document. `means doc verify` reads every stored file again and reports missing files and hash mismatches. Include the `documents/` directory in every backup of the ledger file.

| Command | Result |
| --- | --- |
| `means doc add <file> --entity <e>` | Stores the file and optionally links it (`--link-entry`, `--link-line`, `--role`, `--meta KEY=VALUE`). |
| `means doc list --entity <e>` | Lists documents, newest received first (`--unlinked`, `--since`). |
| `means doc link <uid> --entry <uid>` or `--line <id>` | Adds a link. |
| `means doc show <uid>` | Shows the row, its links, and the stored path. |
| `means doc verify` | Checks every stored file against its hash. |

With `--json`, each command prints one JSON object with `"schema_version": 1`. A failure prints `"ok": false` and an error code: `not_found`, `invalid_path`, `hash_mismatch`, `duplicate`, `io_error`, or `invalid_input`. The exit code is then 1.

## Payees, tags, templates, and budgets

A canonical payee gives related entries a shared display name. The original booked payee text stays in the journal. [Payee aliases](payee-aliases.md) describe matching, conflicts, merges, and history previews.

Tags attach context to entries. Expense classes group categories for reports. A tag-grouped report counts a tagged expense under every matching tag and labels the overlapping totals.

Templates generate balanced entries from fixed amounts, percentages, and a balancing remainder. Percentage splits use largest remainder allocation with template order breaking ties. Manual splits assign rounding differences to the last leg. Scheduled templates run through an explicit schedule command.

A budget applies one limit to an inclusive date range. It can cover a category and its children, an expense class, or a tag. The default period is a calendar month. See [reports and budgets](expense-reports.md).

## Entity facts, groups and parties

These records describe the books. They never change postings.

Each entity keeps its legal form, fiscal year end (`MM-DD`), incorporation date and registrations. Registrations are a JSON object of `KEY: value` pairs, for example a registration number or a tax number. Keys are uppercase free text. An empty value removes a key.

`entity_classifications` records the tax classification of an entity in a jurisdiction, for example `US` `disregarded_entity` or `SG` `company`. One classification applies per jurisdiction on a date. A new open classification closes the earlier open one in the same jurisdiction on its start date.

`entity_groups` names a group of entities. `entity_group_members` puts an entity in a group, with an optional parent entity, the ownership that the parent holds in basis points (10000 is 100%) and a control flag. An entity is in one group at a time. A parent must be a member of the same group for the whole period of its child. The group tree shows the effective ownership along the path from the root, rounded down.

Each payee keeps its country (ISO 3166-1 alpha-2), tax ids as a JSON object, and a kind: `person`, `company`, `authority` or `other`. `party_relationships` links a payee to the entity that keeps it, with a kind such as `owner`, `director`, `customer` or `lender`, an optional ownership in basis points and the basis for the record. A related party for reporting is a payee with an open `owner`, `subsidiary`, `director` or `related_party` relationship to the entity on the date.

Classifications, memberships and relationships use dated periods. A period includes its `effective_from` date and excludes its `effective_to` date. An empty `effective_to` keeps the period open. An "as of" query returns the rows whose period includes that date. To close a period, set `effective_to` to the first day that the fact no longer applies. Two periods of the same fact cannot overlap. Every change goes to the audit log with its values before and after.

## Close, lock and filings

A close check reads one period of one entity and lists findings. It writes nothing. An error blocks a final close. A warning needs attention but does not block it.

| Code | Severity | Condition |
| --- | --- | --- |
| `unresolved_draft` | error | A draft entry is dated in the period. |
| `unreviewed_entry` | error | A posted entry in the period has no review mark. |
| `missing_rate` | error | A posting has `rate_source = 'missing'`, or a posting in a foreign commodity has no rate. |
| `suspense_balance` | error | A suspense account has a non-zero balance in the period. A suspense account has the system role `suspense` (Uncategorized) or a path segment named `Uncategorized` or `Suspense`. |
| `lock_moved_back` | error | The lock date is before the highest lock date that the audit log records since the last unlock. |
| `unbalanced_period` | error | The debits and the credits of the booked entries in the period differ. |
| `unmatched_evidence` | warning | Statement lines in the period have no posting link. Skipped and duplicate lines do not count. |
| `intercompany_out_of_balance` | warning | An account with `intercompany = 1` has a non-zero balance in the period. Reconcile it with the other entity in a group close. |

A report has the findings, their counts, and control totals: booked entries, drafts, postings, statement lines, debits, credits and the balance. Booked entries are `posted` and `void` entries. The `report_digest` is the SHA-256 of the canonical JSON of the report.

A lock moves the lock date of the entity forward. The check runs over the days that the lock closes: from the day after the old lock date to the new lock date. An error finding refuses the lock. The `--force` option locks the period with the errors. The audit event of the lock records the actor, the reason, the old and the new lock date, the finding counts, the force flag and the report digest.

An unlock moves the lock date back. It needs an actor and a reason, and it always writes an audit event. The gRPC call `UpdateEntity` can only move the lock date forward. A move back through it fails with a locked error.

The audit log has an `actor` and a `reason` column. A lock, an unlock, a filing change and `means entry confirm` fill them.

A filing records a return or a report for an authority. Each row has the entity, the jurisdiction, the filing code, the period (both days included) and a status: `prepared`, `submitted`, `accepted`, `rejected` or `amended`. It also keeps the digest of the books snapshot, the digest of the filed artifact, the receipt reference, the submission time, notes, and the sequence and hash of the last chained entry of the entity when the filing was recorded.

The status follows these steps:

| From | To |
| --- | --- |
| `prepared` | `submitted`, `rejected` |
| `submitted` | `accepted`, `rejected`, `amended` |
| `accepted` | `amended` |
| `rejected` | `prepared`, `submitted` |

A move to `submitted` needs a receipt reference. A filing in status `submitted` or `accepted` freezes its period. An unlock that reopens a day of a frozen period fails with `filed_period`. With `--amend`, the unlock creates a new filing in status `prepared` for each frozen filing. The new row points to the original filing through `amends_id`. The original filing keeps its status. A second unlock uses an amendment that is still `prepared`. The audit log records every filing change with its actor and reason.

| Command | Result |
| --- | --- |
| `means close check --entity <e> --from D --to D` | Lists the findings of the period. |
| `means close lock --entity <e> --through D --actor A --reason R [--force]` | Moves the lock date forward. |
| `means close unlock --entity <e> --to D --actor A --reason R [--amend]` | Moves the lock date back. |
| `means filing record --entity <e> --jurisdiction J --code C --from D --to D [--status S]` | Records a filing (`--snapshot-digest`, `--artifact-digest`, `--notes`). |
| `means filing status <uid> --status S [--receipt R] [--at D] [--actor A]` | Changes the status of a filing. |
| `means filing list --entity <e> [--jurisdiction J] [--code C]` | Lists the filings of an entity. |
## Tax codes and tax events

One tax code with one rate cannot feed a real return. means records a tax event for each taxed posting instead. Returns sum the events by code, kind and period. The mapping of these sums to the boxes of a return stays outside the engine.

`tax_codes` holds dated rate versions of an entity. Each version has a code (one uppercase word, for example `SR`), a name, a kind, a rate in basis points (900 is 9%), an optional tax account, a recoverable share in basis points (default 10000) and a jurisdiction. The kinds are:

| Kind | Tax lines that the engine adds |
| --- | --- |
| `output` | The tax on the tax account. |
| `input` | The recoverable part on the tax account. The rest goes on the account of the taxed posting as a cost. |
| `reverse_charge` | The tax negated and the recoverable part on the tax account. The rest goes on the account of the taxed posting. The lines sum to zero. |
| `withholding` | The tax negated on the tax account. The other party receives less. |
| `exempt`, `zero_rated`, `out_of_scope` | None. The rate must be 0. The event records the base. |

A version applies from `effective_from` (inclusive) to `effective_to` (exclusive). A new version closes the open version of the same code on its start date. Other overlaps fail. The tax account must be a leaf account of the entity in the functional currency. A code without a tax account adds no lines. Every change goes to the audit log.

`tax_events` holds one row per taxed posting and tax point:

- The entry, the posting and the tax code version.
- The tax point. It decides the period. The default is the entry date.
- The base, the tax, the recoverable part and the withheld part, in minor units of the functional currency.
- The native base and native tax, when the taxed posting holds another commodity.
- The other party (a payee), a document reference, the rule version (`CODE@effective_from`), a correction link and metadata.

Amounts follow the side of the taxed posting: debit positive. A sale on an income account gives a negative base and a negative tax. A purchase gives positive amounts. The recoverable part never exceeds the tax.

### Tax blocks on entries

A posting input can carry a tax block: a code, and optionally a base, a tax, a tax point, a party and a document reference. An entry input can carry a default block for each income or expense posting without its own block. The rule is:

1. The caller gives net amounts.
2. The engine takes the base from the functional amount of the posting, unless the block gives one.
3. The engine computes the tax as base × rate / 10000, rounded half-even to the currency precision, unless the block gives one.
4. The engine records the event and adds the tax lines of the kind. The balancing posting absorbs the tax lines, so the entry balances.

Block amounts are magnitudes. The engine gives them the sign of the posting. A balancing posting cannot carry a block. Each tax line has `tax_line` (the code) and `tax_role` in its metadata.

### Lifecycle

- A void creates mirror events on the reversal. They have negated amounts, the tax point of the reversal and `corrects_event_id` set to the original event. The original stays in its period. The correction falls in the period of the reversal.
- A deleted draft deletes its events.
- An edit with tax blocks replaces the events. The engine drops the input postings marked `tax_line` and adds new tax lines.
- An edit without tax blocks keeps the events and moves them to the replacement postings. Send the blocks again to compute new amounts.

`tax_summary` and `tax_events_list` read the events of posted and void entries. Drafts do not count.

| Command | Result |
| --- | --- |
| `means tax code add --entity <e> --code SR --name "Standard rated" --kind output --rate-bp 900 --tax-account <path> --from D` | Adds a version (`--recoverable-bp N`, `--jurisdiction J`). |
| `means tax code list --entity <e> [--as-of D]` | Lists every version, or the versions in effect on the date. |
| `means tax code close --entity <e> --code C --to D` | Ends the open version. |
| `means tax summary --entity <e> --from D --to D` | Sums base, tax, recoverable and withheld by code, kind and rate. |
| `means tax events --entity <e> --from D --to D [--code C]` | Lists the events. |

Each command prints one JSON object with `"schema_version": 1`. Money uses the `Money` form. A failure prints `"ok": false` with an error code and exits with code 1.

## Reports and exports

Balances, trial balances, income statements, and net worth are derived from the ledger. Reports keep entity scope explicit. Expense reports use stored book values. Currency-converted account reports round each account first, then sum the displayed rows. A missing required rate stops the report with an error.

Ledger running balances follow chronological order. Sorting the rows changes their presentation while preserving each posting’s balance.

[Beancount exports](export.md) reproduce functional-currency book values and keep original quantities and commodities in metadata.

## Audit and sharing

Entries have stable identifiers and a hash chain. Audited changes retain their before-and-after values. Use verification, backups, and independent review together to check the books.

[Read-only vault sharing](vault-sharing-usage.md) exchanges signed, encrypted snapshots through files. Each received vault uses a separate database. Recipient views stay separate from owned-vault totals. The [wire format](vault-sharing-wire-v1.md) defines signatures, receipts, and validation.
