# Account Tracker Pro backups (`.atb`)

The backup format, parser behavior, and balance checks used by the importer. Examples use synthetic data.

## Container

An `NSKeyedArchiver` binary plist (`bplist00`). The root object is one dictionary with settings and five collections:

| Key | Content |
|---|---|
| `code` | The app's base currency (for example, USD). |
| `rates` | `{currency: price in base currency}` at export time. |
| `timezone` | IANA zone used to render dates (for example, `Etc/UTC`). |
| `timestamp` | Export time, milliseconds since the Unix epoch. |
| `groups` | Ordered list of `{id, name}`; accounts reference a group by **index**. |
| `accounts` | See below. |
| `transactions` | See below. |
| `budgets` | Per-category amounts (retained in the backup; not imported). |

Dates inside objects are `NSDate` values: seconds since 2001-01-01 UTC, converted to the local calendar date in `timezone`.

## Accounts

| Field | Meaning | means |
|---|---|---|
| `id` | Stable id | `external_ids.account_tracker` |
| `name`, `code` | Name and currency | account name, commodity |
| `group` | Index into `groups` | placeholder parent account, one per type when a group holds both assets and liabilities |
| `opening` | Opening balance, minor units | opening entry against Opening balances |
| `balance` | Balance at export, minor units, sign from the account's own view (a card you owe is negative) | the acceptance check |
| `min` | Negative credit limit | `credit_limit` |
| `due` | Card due day | `due_day` |
| `exclude` | Excluded from totals | `in_net_worth = false` |
| `closed` | `yyyymmdd` when closed | `closed_at` |
| `todo` | Number of transactions touching the account, repeats expanded | the count check |

Type and subtype are suggested from the name and group (`CC`/`Card`/Credit Cards → liability card; `Loan` → liability loan; `Splitwise` → receivable; `Wallet`/`Cash` → cash; Savings group → savings; else bank) and can be overridden per account at import.

## Transactions

| Field | Meaning |
|---|---|
| `id` | Millisecond timestamp, used as the stable id (`at:<id>`; occurrences of a repeat add `#<n>`). |
| `date` | `NSDate` |
| `from`, `to` | Account ids; `0` is the outside world. Both set: a transfer. `to = 0`: money out. `from = 0`: money in. |
| `pence` | Amount in minor units, always positive. On a cross-currency transfer it is the amount in the **receiving** account's currency and `foreign` is the sending amount. On money in/out with `foreign` + `code`, `pence` is in the account's currency and `foreign` the merchant's currency. |
| `category` | Flat category name. Empty on transfers and opening deposits. |
| `details`, `notes` | Payee and notes. |
| `refund` | Money in that reduces a category (credited to the expense account) instead of income. |
| `splits` | `{category: pence}` for several categories on one transaction. |
| `repeat`, `every`, `end`, `after`, `on`, `weekend`, `overrides` | A repeating transaction, see below. |

`details == "Initial Deposit"` with `from = 0` and no category is the opening balance.

## Repeats

A repeat is one row plus rules; the app materialises occurrences on the fly. `means` expands them up to the export date (occurrence `n` is `at:<id>#<n>`) and creates a scheduled template for what comes after.

| Field | Values |
|---|---|
| `repeat` | `Daily`, `Weekly`, `Monthly`, `Yearly` |
| `every` | Interval multiplier (`Daily` every 30, `Weekly` every 2, ...) |
| `end` | `After` (with `after` = total number of occurrences, deleted ones included), `Never`, `On` (with `on` date) |
| `weekend` | `None`, or a shift rule (`Monday` = move to the next Monday; `Before`/`Friday` = the Friday before) |
| `overrides` | List of per-occurrence edits, 1-based `sequence` |

Override records:

| Record | Meaning |
|---|---|
| `{sequence: n, pence: p}` | Occurrence `n` has amount `p`. |
| `{sequence: n, date: yyyymmdd}` | Occurrence `n` moved to that date (optionally with `pence`). |
| `{sequence: n, date: 0}` | Occurrence `n` deleted. |
| `{sequence: -k, pence: p}` | **History**: occurrences before `k` had amount `p`. `pence` on the row is the current amount; when the user changes the amount "from now on", the app keeps the old amount here. The nearest boundary above `n` wins when several exist. |

Monthly and yearly repeats keep the day of month, clamped to the month's length.

## Migration into means

- Every occurrence becomes a posted journal entry with origin `migration`. Bank-side postings carry the Account Tracker id as `external_id`, so a newer backup only adds what is new.
- Transfers inside one entity are one entry; transfers between entities are two linked entries through the owner/distribution accounts.
- Categories become expense accounts; a category used for money in without the refund flag also gets an income account; the empty category maps to Uncategorized.
- Amounts in foreign currencies are valued into the entity's functional currency at the day's rate; when no rate is known yet the posting is flagged and revalued after `FetchRates`.
- Acceptance: after the import, each account's ledger balance equals the app's `balance` and each transaction count equals `todo`. The importer returns these checks.
