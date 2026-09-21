---
title: Terminal workflows
description: Navigate the TUI, review imported entries, split transactions, and sort account ledgers.
---

## Find your way around

Start the local server, then run `means tui`. The top bar shows the current vault and screen. Press `?` for help.

| Key | Screen      |
| --- | ----------- |
| `1` | Overview    |
| `2` | Accounts    |
| `3` | Journal     |
| `4` | Review      |
| `5` | Capture     |
| `6` | Imports     |
| `7` | Reports     |
| `8` | Connections |
| `9` | Payees      |
| `0` | Sharing     |

Use `j`/`k` or the arrow keys to move through lists. Read the shortcut bar for the current screen or modal.

## Review an import

Drafts need a destination account. Choose a category, then inspect the transaction preview. Press `y` to confirm. Press `n` or Escape to cancel.

Entries that rules posted appear as **unreviewed**. Enter marks their existing postings as reviewed. Press `o` to inspect an entry first.

Unmatched statement lines are evidence that still needs a posting link.

## Split a transaction

Press **Shift-S** on a draft or posted expense/income in Review. You can also use **Shift-S** in an account ledger, or `s` in a transaction detail modal.

In the split editor:

1. Press `a` to add a category.
2. Enter an amount for each part in the bank account's native currency.
3. Leave the final amount blank to use the remainder.
4. Press Enter on the split list to preview the transaction.
5. Read all the postings. Press `y` to confirm, or `n` to edit.

You can add more than two categories. Use `e` to edit an amount and `x` to remove a part. The bank posting and its evidence remain intact.

Splitting an existing posted entry changes its category allocation with an audit record. Locked periods, reconciled or evidenced category postings, paired transfers, reversals, and linked refunds are protected.

Lowercase `s` has other meanings outside the detail modal: it skips lines in Review and opens the filter-based category move in a category ledger.

## Capture something new

Open Capture with `5`. Choose the entry kind, bank account, amount, category, and date. Press `s` outside a text field to use the same split editor.

Enter in that editor saves the splits to the form. Ctrl-S opens the posting preview. Only `y` confirms the transaction. If the books change while a preview is open, return and obtain a fresh preview.

## Read and sort a ledger

Open Accounts, select an account, and press Enter. Account ledgers open with newest entries first.

Press `z` to select a sort order. Sorting is also available in Accounts, Journal, Review, and expense reports. Available choices depend on the view.

The account ledger loads the latest 2,000 postings. Running balances stay chronological even when the display order changes. Journal and Review sort their loaded rows; Review keeps drafts, unreviewed entries, and unmatched lines in separate groups.

## Next steps

- [Connect a bank](../guides/connections/).
- [Read reports and set budgets](../guides/expense-reports/).
- [Manage payees](../guides/payee-aliases/).
- [Share a read-only vault](../guides/vault-sharing-usage/).
