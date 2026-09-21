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

A split distributes a movement across several accounts. For an imported expense, the bank posting and its statement link stay intact while the expense postings change. Preview the complete transaction before confirming it in the TUI.

A void retains the original postings and creates a linked reversal. The reversal negates native quantities and book values. The original entry’s evidence returns to the unmatched queue. Lock dates apply to the correction date.

Lock dates protect closed periods. Reconciled postings retain their account and quantity during recategorization. Operations that would alter protected accounting data fail with an explanation.

## Imports and statement evidence

An import records the source, file checksum, options, result counts, and statement lines. Each statement line retains the provider’s raw record, date, amount, currency, description, and reference. Its processing state identifies whether it was matched, created, skipped, marked as a duplicate, or left for review.

Matching links a statement line to a posting. The posting holds the accounting decision; the statement line holds the bank evidence. Several confirmed sources can evidence one posting through cross-source reconciliation.

Deduplication uses the source’s identity rules. Enable Banking lines with a stable reference use that account-scoped reference; lines without one fall back to a fingerprint. Other formats can use fingerprints when their export IDs change. A fingerprint incorporates the date, amount, description, account, and occurrence within the file.

Rules choose categories, payees, tags, or templates from statement data. Refund candidates and ambiguous cross-source matches require review. See [import formats and recovery](import-formats.md) for each source’s behavior and confirmation steps.

## Payees, tags, templates, and budgets

A canonical payee gives related entries a shared display name. The original booked payee text stays in the journal. [Payee aliases](payee-aliases.md) describe matching, conflicts, merges, and history previews.

Tags attach context to entries. Expense classes group categories for reports. A tag-grouped report counts a tagged expense under every matching tag and labels the overlapping totals.

Templates generate balanced entries from fixed amounts, percentages, and a balancing remainder. Percentage splits use largest remainder allocation with template order breaking ties. Manual splits assign rounding differences to the last leg. Scheduled templates run through an explicit schedule command.

A budget applies one limit to an inclusive date range. It can cover a category and its children, an expense class, or a tag. The default period is a calendar month. See [reports and budgets](expense-reports.md).

## Reports and exports

Balances, trial balances, income statements, and net worth are derived from the ledger. Reports keep entity scope explicit. Expense reports use stored book values. Currency-converted account reports round each account first, then sum the displayed rows. A missing required rate stops the report with an error.

Ledger running balances follow chronological order. Sorting the rows changes their presentation while preserving each posting’s balance.

[Beancount exports](export.md) reproduce functional-currency book values and keep original quantities and commodities in metadata.

## Audit and sharing

Entries have stable identifiers and a hash chain. Audited changes retain their before-and-after values. Use verification, backups, and independent review together to check the books.

[Read-only vault sharing](vault-sharing-usage.md) exchanges signed, encrypted snapshots through files. Each received vault uses a separate database. Recipient views stay separate from owned-vault totals. The [wire format](vault-sharing-wire-v1.md) defines signatures, receipts, and validation.
