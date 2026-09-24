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

## Reports and exports

Balances, trial balances, income statements, and net worth are derived from the ledger. Reports keep entity scope explicit. Expense reports use stored book values. Currency-converted account reports round each account first, then sum the displayed rows. A missing required rate stops the report with an error.

Ledger running balances follow chronological order. Sorting the rows changes their presentation while preserving each posting’s balance.

[Beancount exports](export.md) reproduce functional-currency book values and keep original quantities and commodities in metadata.

## Audit and sharing

Entries have stable identifiers and a hash chain. Audited changes retain their before-and-after values. Use verification, backups, and independent review together to check the books.

[Read-only vault sharing](vault-sharing-usage.md) exchanges signed, encrypted snapshots through files. Each received vault uses a separate database. Recipient views stay separate from owned-vault totals. The [wire format](vault-sharing-wire-v1.md) defines signatures, receipts, and validation.
