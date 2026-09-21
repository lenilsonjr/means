---
title: Core concepts
description: Understand vaults, native quantities, book values, evidence, and double-entry accounting.
---

Vaults separate sets of books. Accounts track assets, liabilities, equity, income, and expenses. Journal entries record changes; statement lines keep the bank evidence.

## Vaults and entities

A **vault** is one entity's set of books. Your personal finances and a company each have their own chart of accounts, accounting currency, and lock date.

Overview shows net worth for the selected vault. Press `e` to change vaults. Personal and company vaults each report their own balances.

## Company examples

A vault can represent a Singapore Pte. Ltd., a US C-corporation, or an Estonian OÜ. All use `kind=company`; the country and accounting currency are separate settings.

| Fictional entity         | Country | Example accounting currency |
| ------------------------ | ------- | --------------------------- |
| Example Studio Pte. Ltd. | SG      | SGD                         |
| Example Robotics Inc.    | US      | USD                         |
| Example Software OÜ      | EE      | EUR                         |

These currency choices are examples. Each company can hold accounts in other currencies. The shared `charts/company-services.json` template supplies generic operating income and expense accounts. Statutory reporting and tax setup are separate from the entity label.

## Accounts and categories

An account belongs to one of five types:

| Type      | Examples                  | Increases with |
| --------- | ------------------------- | -------------- |
| Asset     | Bank account, cash        | Debit          |
| Liability | Credit card, loan         | Credit         |
| Equity    | Opening balances, capital | Credit         |
| Income    | Salary, consulting        | Credit         |
| Expense   | Groceries, software       | Debit          |

The categories you choose for spending are expense accounts. Paths such as `Expenses:Groceries` place accounts in a hierarchy.

## An expense credits the bank

Debit and credit describe the two sides of a posting.

For a €90.50 purchase split across two categories:

| Account            |  Debit | Credit |
| ------------------ | -----: | -----: |
| Assets:Bank        |        | €90.50 |
| Expenses:Groceries | €72.40 |        |
| Expenses:Household | €18.10 |        |

The bank asset decreases. Expenses increase. The entry balances.

A ledger row shows the running balance **after** that posting. Balances are calculated in chronological order, including when you sort newest first.

## Native quantities and book values

Each account has a commodity, such as EUR or USD. Each vault has an accounting currency, also called its functional currency.

A USD vault can contain a EUR bank account. A posting keeps both the native EUR quantity and its USD book value. Entries balance in the vault's accounting currency.

To change an existing vault’s accounting currency, use the [preview and backup workflow](../guides/currency-migration/).

## Imports are evidence

A statement line preserves what the bank supplied. A posting records how the movement is booked. Matching links the two. For example, one bank debit can remain linked to a purchase split between groceries and household expenses.

Reviewing, reconciling, and posting are separate actions:

- A **draft** still needs a booking decision.
- An **unreviewed** entry is posted, but has not had human review.
- **Reconciliation** compares the ledger with statement evidence and balances.

Different sources can use different dates or references, so some overlaps need manual review. Preview [cross-source matches](../guides/import-formats/#overlap-between-csv-and-bank-connections) before you confirm them.

## Changes leave a trail

means records audited changes and maintains a hash chain of entries. Lock dates protect closed periods. Use the hash chain to check for inconsistencies, and keep backups and independent reviews as part of your routine.

Read the [accounting model](../guides/data-model/) for the ledger’s records and accounting rules.
