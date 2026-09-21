# Change a vault's accounting currency

`means entity migrate-currency` converts historical book values for one vault.
Account currencies stay unchanged. A USD vault can keep EUR
bank accounts, categories, and subaccounts. Each existing posting keeps its
native quantity, account, ID, and statement links.

The conversion applies to the vault’s historical book values.
Other vaults keep their accounting currencies and postings. Overview shows
only the selected vault.

## Valuation rules

The preview lists every posting's old and new book value and its source:

1. A native amount in the target currency remains exact. An imported
   posting can use a supported, retained original transaction amount in that currency.
   An explicit `value_in` in that currency is also evidence.
2. A consistent transaction conversion applies to the entry's category splits.
   Otherwise, other book values use the entry's booking-date historical rate.
   The final balancing split absorbs conversion rounding. Evidenced target amounts remain exact; exchange differences stay explicit.
3. A refund keeps the migrated original expense's book value. The bank credit
   uses its own evidence or refund-date rate. The difference goes to FX gain/loss.
   A void reversal negates the migrated original, including new FX postings.
4. Each budget limit uses the historical rate on its start date. Dates and
   category, class, and tag filters stay unchanged. Each limit covers its full date range.

Historical rates can be direct, inverse, or cross rates. The command accepts
the latest quote on or before the booking date, up to seven days earlier for
weekends and holidays. The preview identifies the actual quote date and source.
Export-date snapshot rates have source `import`. Historical conversion requires
booking-date evidence or eligible historical quotes.

Missing rates stop the command. A provisional old book rate marked `missing`
cannot establish a conversion or reliable category proportions. If all target
amounts are not evidenced, repair that entry on the rehearsal copy first.
Broken refund links, conflicting native quantities, and a broken hash chain
also stop the command. The conversion commits atomically.

## Accountant's rehearsal

Use a copy first. Stop processes that can write the source ledger while you
create and check the copy. Use SQLite's backup command, which includes committed
WAL content. Do not copy only a database file while a writer runs.

```sh
sqlite3 /path/owned-ledger.db ".backup '/path/rehearsal.db'"
means --db /path/rehearsal.db entity list
```

If historical rates are missing, fetch them into the copy. Set `--from` to
the earliest entry or budget date that needs conversion. `--no-revalue` stores
rates without changing existing entry book values.

```sh
means --db /path/rehearsal.db rates fetch --from 2020-01-01 --no-revalue
means --db /path/rehearsal.db entity migrate-currency \
  --entity Personal --to USD > /path/currency-preview.json
```

The migration preview opens the existing ledger schema read-only. Review all posting sources, budget limits, FX adjustments, and the
counts of locked entries and reconciled postings. The JSON uses structured
Money values; `minor: "12345"` with precision 2 means 123.45.

Copy the `confirmation` token from that preview. Apply it to the copy:

```sh
means --db /path/rehearsal.db entity migrate-currency \
  --entity Personal --to USD \
  --apply TOKEN_FROM_PREVIEW \
  --backup /path/rehearsal-before-currency-change.db
```

The backup path must be new. The command creates a private backup file, checks
SQLite integrity and its contents against the source, and syncs it before
writing the migration. A change to the ledger, its rates, evidence, or settings
invalidates the token. Generate and review a new preview in that case.

If the preview includes locked entries, apply also requires `--include-locked`.
Use it only after reviewing those periods. The command keeps the lock date and
all native reconciliation data. It records the old and new entries, budget
values, chain heads, preview, and backup path in the audit log.

Check the copy's journal, expense reports, budgets, trial balance, and Overview.
Bank balances must retain their original quantities and currencies. Expense
refunds must cancel their original expense book values. Review FX separately.
Run the existing `means verify` command against the copy to check the hash chain.

Validate the rehearsal copy before running the same process on the owned
ledger. Make a fresh preview there: each token applies to one ledger state.

## Account and audit details

The old FX account keeps its currency, quantities, and postings. Its role becomes
`fx_gain_loss_legacy`. The command creates a target-currency FX account for new
adjustments and future entries, unless the active FX account already uses that
currency. The preview names that account. Historical FX entries keep their
classification.

Templates retain their native account quantities and percentages. Account credit
limits, statement amounts, and reconciliation links stay unchanged. Existing
posting metadata retains the prior rates and each migration's valuation source.
New book values require a new hash chain; the audit and backup retain the old
chain head. Keep that backup with the reviewed preview.

To inspect or recover the old books, stop writers and open the backup through
`--db`. Do not replace a live SQLite database underneath a running server.

Read-only shared replicas cannot be migrated. Send a new signed snapshot of the
owned vault after the accountant has approved its converted books.
