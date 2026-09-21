---
title: Your books, on your machine
description: Meet means, a local double-entry accounting app for the terminal.
---

means is a local double-entry accounting app with a terminal UI and a CLI. It stores your ledger in SQLite and links imported transactions to their bank evidence.

You can split a purchase across categories, check an account against its statement, or send a read-only copy of a vault to your accountant. The bank record stays attached when you change how an imported expense is categorized.

Personal and company books use separate vaults. Each vault has an accounting currency; its accounts keep their native currencies. A EUR bank account can sit in a USD vault, with both quantities and book values stored on each posting.

> **Start small.** means is in early development. Try a sample ledger or a copy of your books first. Keep a backup before you change existing books.

## A typical day

1. Pull bank activity or import a statement.
2. Review drafts and entries that rules posted.
3. Assign categories or split a transaction.
4. Confirm the posting preview.
5. Reconcile the account against its statement.
6. Read the reports for the selected vault.

[Install means and create a vault](../getting-started/).

## What stays local

The CLI uses the accounting core directly. The TUI connects to a server on your machine. That server accepts native gRPC on a loopback address for local use.

Bank connections contact their providers when you discover accounts, authorize access, or pull data. Credentials stay in the server environment. The documentation website has no access to your ledger.

## What you can bring

- Account Tracker Pro `.atb` backups.
- Bank CSV and OFX files, including configurable CSV mapping.
- Pluggy, Enable Banking, Mercury, Wise business, and Banco Inter PJ connections.
- Statement attachments from a dedicated IMAP mailbox or folder.

Provider support has account and region limits. Read [bank connection setup](../guides/connections/) before you configure a provider.

## What you can take with you

Export booked values to [Beancount](../guides/export/), or send an accountant an encrypted, signed [read-only vault snapshot](../guides/vault-sharing-usage/). The code is also open to inspection and contribution under [Apache-2.0](https://github.com/lenilsonjr/means/blob/main/LICENSE).
