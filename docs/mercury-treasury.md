# Mercury Treasury evidence

Capture Treasury account and transaction evidence through the CLI. Automatic ledger conversion is pending.

## Available command

Set `MERCURY_TOKEN` to a read-only token. List accounts, then fetch one account:

```sh
means mercury treasury accounts
means mercury treasury fetch --account ACCOUNT_UUID --dry-run
means mercury treasury fetch --account ACCOUNT_UUID --output treasury-evidence.json
```

The output directory must exist. The command refuses to overwrite a file. It
writes a complete temporary file, flushes it, then publishes it. The evidence file
has mode 0600 on Unix. A failed fetch publishes nothing. A dry run creates no file.
The commands write evidence files. Ledger entries and account mappings stay unchanged.

The file has channel `mercury_treasury_evidence` and version 1. It contains a fetch
timestamp, the original account object, and all returned transaction objects.
The capture retains all event types, including failures, valuations and unknown
future types. Amounts, dates, and security labels remain as returned by Mercury.
Keep this evidence separately for review; the inbox scanner skips this format.

## Verified API boundaries

Mercury publishes separate endpoints for
[account discovery](https://docs.mercury.com/reference/gettreasury) and
[Treasury transactions](https://docs.mercury.com/reference/gettreasurytransactions).
The discovery response uses `accounts` and a `page.nextPage` UUID. The transaction
response uses `transactions` and an integer `cursor`. The implementation follows
both protocols to completion. A cursor on an empty page still requires another request. Invalid
cursors, repeated cursors, duplicate transaction IDs and account mismatches fail.
Each resource has a 10,000-page limit; reaching it fails without a partial file.

Discovery includes account balances and monthly `netReturns`. Each return can
include dividends, fees and a processing status. Transactions include an amount,
balance, date, event type, optional security label and optional details. The
published `TreasuryTxn` schema has no share quantity or unit-price field. Thus,
individual security positions cannot be reconstructed from this schema alone.
The schema check used Mercury's published OpenAPI document on 2026-09-19. Tests
use synthetic records and local HTTP servers.
