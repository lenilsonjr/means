---
title: CLI quick reference
description: Common means commands for accounts, imports, review, reconciliation, reports, and exports.
---

The CLI operates on the accounting core directly. Run `means --help` and `means COMMAND --help` for the complete interface of your installed version.

Use `--db PATH` or `MEANS_DB` to choose a ledger. The examples below use the default path, `~/.means/ledger.db`. Replace the sample IDs and dates with those from your ledger.

## Inspect the books

```sh
means status
means entity list
means accounts --entity Personal
means entries --entity Personal --status draft
means entries --entity Personal --json
means verify
```

JSON represents posting quantities and booked values as Money objects. For example, `{"minor":"125","commodity":"EUR","precision":2}` means EUR 1.25. Integer minor units are strings to preserve their exact value in JavaScript.

## Import and recover

```sh
means import --account 12 statement.csv
means import --entity 1 backup.atb
means import retry 42
means rematch 42 --cross-source --preview
means reconcile --account 12
```

`import retry` resumes unfinished or error lines from stored evidence. Apply cross-source matches by confirming the returned preview token. Read [import recovery and overlap rules](../guides/import-formats/) before you change existing imports.

## Categorize a draft

```sh
means post 4213 --to Expenses:Domains
means post 4214 --to "Expenses:Groceries=72.40" --to "Expenses:Household"
means draft delete 123
```

The final split may take the remainder. These commands apply directly. `draft delete` deletes a draft only. Its statement evidence remains unmatched.

## Void a posted entry

```sh
means void 4213 --reason "Duplicate purchase"
means void 4213 --reason "Duplicate purchase" --yes
# Use an open-period correction date when needed:
means void 4213 --date 2026-10-01 --reason "Correction" --yes
```

By default, the command shows a read-only preview of the full reversal.
`--dry-run` makes that preview explicit. `--json` emits the proposed or applied
entry, reversal, and number of affected statement lines.

The reversal negates every native quantity and booked value. It uses the original
booking date unless you pass `--date`. The original postings stay in the journal,
its status becomes `void`, and its statement evidence returns to the unmatched
queue. The reversal retains the entry's tags. Lock dates apply to the reversal
date. Repeating the command for an already-void entry creates no second reversal.

Use `means draft delete ID` to remove a draft.
A confirmed command acts on the current entry at the time you run it.

## Report and budget

```sh
means report expenses --entity 1 --from 2026-09-01 --to 2026-09-30
means report expenses --entity 1 --group-by tag
means budget create --entity 1 --name Porto --tag trip:porto --amount 600 --from 2026-09-01 --to 2026-11-30
means budget list --entity 1 --json
```

A budget has one limit for its whole date range. There is no monthly reset inside that range and no rollover. See [reports and budgets](../guides/expense-reports/).

## Export

```sh
means export --format beancount --output books.beancount
```

This is a [book-value export](../guides/export/). It uses functional-currency postings and keeps original quantities and currencies as metadata.
