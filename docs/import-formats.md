# Bank export formats

What each importer in `crates/means-core/src/imports/` expects, what was checked against real
exports, and what is still a guess. The fixtures under `crates/means-core/tests/fixtures/` are
synthetic files written to these layouts; `crates/means-core/tests/formats.rs` parses and imports
every one of them, and `crates/means-core/tests/pluggy.rs` does the same for the channel that
fetches its own files.

Conventions shared by every source:

- `detect_source(filename, content)` looks at the extension first (`.ofx`/`.qfx`, `.atb`, `.json`),
  then at the content (`<OFX>`, and `"channel": "pluggy"` inside a `.json` file), then at the CSV
  header row (`presets::detect`). The header row is found automatically, so bank preambles before it
  are fine.
- Amounts are kept as the bank shows them: positive is money in, negative is money out.
- `reference` is the bank's own transaction id when the file has one; `balance_after` is the running
  balance when the file has one. Both feed duplicate detection and the closing balance.
- Rows that never moved money (pending, reverted, failed, cancelled, balance rows) are kept as
  statement lines with status `skipped` and a note saying why, so nothing is silently dropped. The
  Pluggy channel is the exception: a transaction that is not settled yet has no line at all, because
  it settles under another id. It is counted among the import's `skipped_count`, and the file the
  import keeps still holds it verbatim.
- A multi-currency file imported into an account keeps the lines in that account's currency and
  skips the rest with the note `USD line in a EUR account`; a file with nothing in the account's
  currency is refused as the wrong file.

"Verified" below means the header and conventions were read from a real export quoted by an
official page or by an open-source importer whose tests carry real-looking samples. "Assumed" means
the parser accepts it, but nobody has held a file that proves it.

## N26 (`n26_csv`)

Two layouts, both comma-separated, all fields double-quoted, `YYYY-MM-DD` dates, `.` decimals,
signed amounts, no id column, no running balance.

Current export (since 2023):

```
"Booking Date","Value Date","Partner Name","Partner Iban","Type","Payment Reference","Account Name","Amount (EUR)","Original Amount","Original Currency","Exchange Rate"
```

Older export (2017–2022), with an optional `Category` column in the later years:

```
"Date","Payee","Account number","Transaction type","Payment reference","Category","Amount (EUR)","Amount (Foreign Currency)","Type Foreign Currency","Exchange Rate"
```

Mapping: date = `Booking Date` / `Date`; amount = `Amount (EUR)`; description = `Partner Name` /
`Payee` followed by the payment reference; currency EUR. A foreign card payment keeps
`Original Amount` + `Original Currency` (or `Amount (Foreign Currency)` + `Type Foreign Currency`)
as the line's original amount, signed like the EUR amount.

Verified:

- Both header lines, and `Category` being optional: [beancount-n26](https://github.com/siddhantgoel/beancount-n26/blob/main/beancount_n26/__init__.py)
  (`HEADER_FIELDS`), which also lists the German and French translations.
- Current header plus a sample row (comma delimiter, quoting, ISO dates, `.` decimals):
  [homebanking-hilfe.de thread](https://homebanking-hilfe.de/forum/topic.php?t=27517).
- N26 offers the CSV as an "account activity report" over a custom date range:
  [N26 support](https://support.n26.com/en-eu/payments-transfers-and-withdrawals/balance-and-limits/how-to-get-bank-statement-n26).

Assumed:

- The sign of `Original Amount` (the parser makes it follow the EUR amount either way).
- That the export contains booked transactions only, so no row needs skipping.
- German-language exports (`Datum`, `Betrag (EUR)`, …) are not recognised; map them by hand.

## Revolut personal statement (`revolut_csv`)

```
Type,Product,Started Date,Completed Date,Description,Amount,Fee,Currency,State,Balance
```

Comma-separated, `YYYY-MM-DD HH:MM:SS` timestamps (the hour is not always zero-padded), `.`
decimals, signed `Amount`, positive `Fee`, per-row `Currency`, `Balance` after the row. No id column.

Mapping: date = `Completed Date`; amount = `Amount`; description = `Description`; balance =
`Balance`; currency = `Currency`. Rows whose `State` is not `COMPLETED` are skipped (`state pending`,
`state reverted`, …); PENDING rows have no completed date and no balance. A zero amount with no fee
(a card authorisation hold such as "Google *temporary Hold") is skipped. A non-zero `Fee` becomes
its own line `Fee: <description>` with the negative fee, because the balance moves by
`Amount − Fee`.

Verified:

- Header, timestamps, PENDING rows with empty completed date and balance, only COMPLETED rows
  counted, fees as separate lines: [ofxstatement-revolut](https://github.com/mlaitinen/ofxstatement-revolut/blob/master/src/ofxstatement/plugins/revolut.py)
  and its samples [2021-october.csv](https://github.com/mlaitinen/ofxstatement-revolut/blob/master/tests/samples/2021-october.csv),
  [2022-january.csv](https://github.com/mlaitinen/ofxstatement-revolut/blob/master/tests/samples/2022-january.csv).
- `Balance = previous + Amount − Fee` (the fee is not inside `Amount`):
  [mcuelenaere/finance revolut parser](https://github.com/mcuelenaere/finance/blob/c63bf19527cad82ae48757dcacd1b3c8606fd1cb/finance/parsers/revolut.py).
- State values COMPLETED, PENDING, DECLINED, FAILED, REVERTED:
  [Revolut developer docs](https://developer.revolut.com/docs/business/get-transactions).

Assumed:

- A statement downloaded for "all accounts" mixes currencies; the per-line currency skip handles it,
  but the closing balance then belongs to whichever currency's row came last (import one currency
  at a time to keep it meaningful).
- Crypto/commodity statements (`Fiat amount`, `Base currency` columns) detect as Revolut but are not
  meaningful as bank lines.
- Revolut Business exports use different headers and are not recognised.

## Wise balance statement (`wise_csv`)

One file per currency balance, downloaded from Statements. Comma-separated, `DD-MM-YYYY` dates,
`.` decimals, signed `Amount`, `Running Balance` after the row, `TransferWise ID` as a stable id.

Current header (2025–2026):

```
"TransferWise ID","Date","Date Time","Amount","Currency","Description","Payment Reference","Running Balance","Exchange From","Exchange To","Exchange Rate","Payer Name","Payee Name","Payee Account Number","Merchant","Card Last Four Digits","Card Holder Full Name","Attachment","Note","Total fees","Exchange To Amount","Transaction Type","Transaction Details Type"
```

Older header (same columns up to `Total fees`, without `Date Time`, `Exchange To Amount`,
`Transaction Type`, `Transaction Details Type`).

Mapping: date = `Date`; amount = `Amount`; description = `Description` plus `Payment Reference`;
reference = `TransferWise ID`; balance = `Running Balance`; currency = `Currency`. Nothing is split:
`Total fees` is a breakdown of `Amount` (the running balance moves by exactly `Amount`), and card
fees arrive as their own `FEE-CARD-…` rows. When `Exchange To` names another currency (a card
payment abroad, a conversion), `Exchange To Amount` in that currency is kept as the line's
original amount.

Verified:

- Column list: [Dativery](https://www.dativery.com/en/apps/wise-csv/); statement formats offered
  (CSV, XLSX, PDF, MT940, QIF, CAMT.053): [Wise help](https://www.wise.com/help/articles/2736049/how-do-i-download-a-statement),
  [Wise Platform balance statement API](https://docs.wise.com/api-reference/balance-statement).
- The 22/23-column header, `DD-MM-YYYY` dates, `FEE-CARD` rows, card rows with `Exchange To`,
  `Exchange Rate`, `Exchange To Amount`:
  [finvestlens CSVDetectionTests.swift](https://github.com/hellotham/finvestlens/blob/15b44766019463d2452a6868ad14468903f55d68/Packages/Interchange/Tests/FinvestLensInterchangeTests/CSVDetectionTests.swift),
  [acacia sample statement](https://github.com/baradhili/acacia/blob/bd6b067c0dc7f8268a5025e2a24ffb03495b8ba2/tests/statement_test_AUD_2026-07-01_2026-08-06.csv).
- `Amount` already nets the fee and the running balance moves by `Amount`:
  [erp-mafia/accounted wise-statement.ts](https://github.com/erp-mafia/accounted/blob/7e76961da10549582f17756f27e6d3ff16653a4f/lib/import/bank-file/formats/wise-statement.ts)
  (the parser also carries a continuity check for it).

Assumed:

- `Transaction Type` (CREDIT/DEBIT) and `Transaction Details Type` values are informational only.
- The date column stays `DD-MM-YYYY` in every locale; `DD/MM/YYYY` is accepted as a fallback.

## Wise transaction history (`wise_history_csv`)

The multi-currency export from the Transactions page. Comma-separated, quoted where needed,
`YYYY-MM-DD HH:MM:SS` timestamps, `.` decimals, unsigned amounts, no running balance.

```
ID,Status,Direction,"Created on","Finished on","Source fee amount","Source fee currency","Target fee amount","Target fee currency","Source name","Source amount (after fees)","Source currency","Target name","Target amount (after fees)","Target currency","Exchange rate",Reference,Batch,"Created by",Category,Note
```

Mapping: date = `Finished on` (falling back to `Created on`); reference = `ID`; then per row:

- `Direction = IN`: `+Target amount (after fees)` in `Target currency`, description = source name
  plus reference.
- `Direction = OUT`: `−Source amount (after fees)` in `Source currency`, description = target name
  plus reference; a non-zero `Source fee amount` becomes a `Fee: …` line in `Source fee currency`.
- `Direction = NEUTRAL` (a conversion between the user's own balances): one line
  `−Source amount` in the source currency (with the target side as the original amount), a fee
  line, and a second line `+Target amount` in the target currency carrying the same `ID`.
- `Status` other than `COMPLETED` (`CANCELLED`, `PENDING`, …) is skipped.

Each balance is its own account: importing the file into the EUR account keeps the EUR lines and
skips the USD ones, and the other way round.

Verified:

- Header, sample rows, IN/OUT semantics, fees charged on top of the "(after fees)" amounts, only
  COMPLETED rows settled, `CANCELLED`/`PENDING` statuses:
  [erp-mafia/accounted wise.ts](https://github.com/erp-mafia/accounted/blob/7e76961da10549582f17756f27e6d3ff16653a4f/lib/import/bank-file/formats/wise.ts)
  and its [parser tests](https://github.com/erp-mafia/accounted/blob/7e76961da10549582f17756f27e6d3ff16653a4f/lib/import/bank-file/__tests__/parser.test.ts).
- The export exists next to statements and "includes all activities":
  [Wise help](https://www.wise.com/help/articles/2736049/how-do-i-download-a-statement).

Assumed:

- `NEUTRAL` as the direction of conversions, and that a target-side fee is already netted out of
  `Target amount (after fees)` (no fee line is made for it).
- Do not import both the balance statement and the history for the same period: they describe the
  same movements with different ids, so only the fingerprint (date, amount, description) can catch
  the overlap.

## Enable Banking (`enable_banking_json`)

The parser accepts version-1 evidence files for one account and currency. The inbox learns routing from the envelope’s stable account identity and currency, independent
of filenames. An unplaced account waits for assignment instead of inheriting another bank’s route.
Use `means enable-banking banks`, `connect`, and `sessions` for authorization, then
`means enable-banking pull` to fetch statements into the inbox.
The envelope has `channel: "enable_banking"`, `version: 1`, an `account` object with
`identification_hash` and `currency`, and a `transactions` array containing the original API
records. Split multi-currency accounts into separate files with known currencies; `XXX` is not
a bookable currency.

Only `BOOK` transactions become eligible statement lines. `PDNG` transactions are counted as
skipped. Invalid booked records retain their raw evidence and a skip reason. Amounts use decimal
strings; `DBIT` is negative and `CRDT` positive. The date is `booking_date`. The parser uses
the stable `entry_reference` for deduplication. The session-dependent `transaction_id` stays in raw evidence. Missing
references use the existing import fingerprint and produce a warning. A new nonempty reference
on the ledger account identifies a separate movement, even when the date, amount, and description
match an earlier movement. Repeated references remain duplicates. This preserves separate charges
and re-charges across partial pulls. Other import formats keep their existing fallback rules.
A closing balance is
inferred only when the latest booking date contains a single eligible transaction with a balance.

Prerequisites verified on 2026-09-19: Enable Banking's
[January 2026 changelog](https://enablebanking.com/blog/2026/01/15/enable-banking-changelog-december-2025)
confirms that personal use remains free. The [current terms](https://enablebanking.com/terms/)
limit restricted production access to linked accounts for personal use or evaluation. Each
account must be [linked to the application](https://enablebanking.com/docs/api/linked-accounts)
before it can be accessed. Consent lifetime is bank-dependent, commonly 180 days; use the
returned expiry, not a fixed timer. See the [API FAQ](https://enablebanking.com/docs/faq/) and
[transaction schema](https://enablebanking.com/docs/api/reference/#transaction).

### Enable Banking browser connection

For an HTTPS callback through a proxy, `connect` also accepts
`--redirect https://HOST/callback`. Register that exact URL with the application
and forward the path and query to the local `--callback-port` (default 53682).
The listener remains on `127.0.0.1`. See [proxy callback setup](connections.md).

Create a production application in the Enable Banking Control Panel, upload an RSA public
certificate, and link each personal bank account there. Keep the matching private key outside the
ledger and backups. Register `http://127.0.0.1:53682/callback` as the application's redirect URL.
The [authentication instructions](https://enablebanking.com/docs/api/reference/#authentication)
explain certificate generation and registration.

```sh
export ENABLE_BANKING_APP_ID='your-application-uuid'
export ENABLE_BANKING_KEY_FILE="$HOME/.config/means/enable-banking-private.pem"
means enable-banking banks --country DE
means enable-banking connect --country DE --bank 'exact name printed by banks'
means enable-banking sessions
```

`connect` opens the bank's authorization page and waits up to ten minutes for a callback on
127.0.0.1. `--no-open` prints the URL for opening manually. `--callback-port` changes the port;
register the corresponding URL in the Control Panel first. Session expiry follows the bank's
reported maximum consent lifetime and the expiry returned after authorization. To renew access,
run `connect` for that bank again.

The callback checks its host and a random state token, rejects duplicate parameters and replay,
and exchanges the received code once. A bank cancellation ends the wait with an error. Only the
session identifier, bank, country and expiry are saved in the ledger. Application keys stay in
the selected PEM file; JWTs and authorization codes are not persisted. The HTTP client signs
requests with RS256 and does not follow redirects. `--api` changes the API origin for testing;
it requires HTTPS except for a loopback HTTP endpoint.

### Enable Banking pulls and scheduling

```sh
means enable-banking pull --dry-run
means enable-banking pull
# To select one consent or a different watched folder:
means enable-banking pull --session SESSION_UUID --inbox /path/to/inbox
```

By default, pulls request all history currently exposed by the bank using `strategy=longest`, with no
rolling date window. The bank may restrict older history after authorization; “all available”
does not guarantee the account's entire lifetime. Empty pages with continuation keys still lead
to another request. Repeated keys, invalid responses, HTTP errors, and a 1,000-page guard stop the
account's fetch before any of its files are published. Earlier accounts completed in the same run
remain in the inbox and can be imported; rerunning safely deduplicates their evidence.

To fill a gap after books complete through **2026-08-29**, use the next day as the
inclusive cutoff and select the main account:

```sh
means enable-banking pull --session SESSION_UUID --account ACCOUNT_UID --booked-from 2026-08-30 --dry-run
means enable-banking pull --session SESSION_UUID --account ACCOUNT_UID --booked-from 2026-08-30
```

`--account` accepts a session account UID or its stable `identification_hash:currency`
(shown in pull output and used by Connections). It does not accept a ledger account
name or ID. Unselected accounts are not fetched. An unknown selection fails clearly.
Without `--account`, an explicit cutoff applies to every selected consent/account.

A cutoff uses the API's `strategy=default` and inclusive `date_from` on every page;
returned transactions are also filtered by **booking_date**, not value date or when
the provider recorded them. See the [Enable Banking API reference](https://enablebanking.com/docs/api/reference/).
Missing or invalid booking dates stop publication of the affected account when a
cutoff is active; pending rows are excluded. Provider date-range errors are reported
without falling back to an unrestricted fetch. Output reports excluded records
returned by the provider (it cannot count records the provider omitted).

Connections supports a saved cutoff per Enable Banking account via `f`; CLI and
TUI pulls honor it. An explicit CLI cutoff overrides the saved value for that run;
clear the saved field for unrestricted history. TUI `p` pulls only the selected
account. Cutoffs affect future evidence files; they do not remove existing duplicates
or move transactions between old space accounts. Use a rehearsal inbox for the first
actual pull if the normal inbox is watched by the live importer.

One file per account/currency is written under a temporary hidden name and renamed into the inbox
only after the full account has been fetched. Multi-currency accounts are split by transaction
currency. Missing or unknown transaction currencies stop publication. Files use private permissions
(`0600`) on Unix. The primary `identification_hash` and currency identify each account.
Secondary hashes remain in the evidence file. They do not control routing or account deduplication.
The pull rejects an account with no primary hash, even if secondary hashes are present.
A renewed session keeps its route when its primary hash stays the same. A new primary hash requires
account assignment again. Stored aliases from older versions do not override this rule.

Older versions could merge accounts through a shared secondary hash. This change does not repair
past imports or postings. Review those records and account assignments if you used those versions.
The original account resource and transaction records remain in each evidence file.
There is no incremental cursor.

`--dry-run` fetches and reports the same history without publishing files or updating account
mappings. It still consumes API requests. Expired or inactive consents are skipped on an all-session
pull; selecting one explicitly reports an error. If none remain active, reconnect the bank.
A running `means serve` watches the default inbox next to its ledger; first-time accounts wait for
assignment on the Imports screen.

Run the pull from your scheduler with the same `--db`, `ENABLE_BANKING_APP_ID`, and
`ENABLE_BANKING_KEY_FILE` as the connection step. For example, a private wrapper script can set
those variables and execute `/absolute/path/to/means --db /absolute/path/to/ledger.db enable-banking pull`. A cron entry `0 */6 * * * /absolute/path/to/wrapper` runs it every six hours.
The pull uses background access and sends no fabricated PSU headers. Banks may still return 429;
let the next scheduled run retry. Renew
consent with `connect` when needed. No scheduler job is installed automatically.

## Mercury API evidence (`mercury_json`)

`means mercury pull` fetches checking and savings history with a read-only API token.
Add `--credit` to fetch IO credit history instead. Treasury has a separate [evidence capture command](mercury-treasury.md).
The parser also accepts prepared version-1 evidence envelopes:

```json
{
  "channel": "mercury",
  "version": 1,
  "account_type": "depository",
  "currency": "USD",
  "account": {"id": "11111111-1111-4111-8111-111111111111"},
  "transactions": [
    {
      "id": "22222222-2222-4222-8222-222222222222",
      "accountId": "11111111-1111-4111-8111-111111111111",
      "amount": -3.21,
      "status": "sent",
      "postedAt": "2020-01-02T00:00:00Z",
      "counterpartyName": "Shop"
    }
  ]
}
```

Keep original account and transaction objects in the envelope. Each file belongs to one
Mercury account UUID, and every transaction's `accountId` must match it. Import the first
file into the corresponding USD ledger account; subsequent inbox files route using that
identity, regardless of their filenames. Unknown identities require an account selection.

Only `sent` records with valid `postedAt` timestamps and transaction UUIDs can create
movements. Dates use the UTC posting date, not `createdAt`. Signed numeric USD amounts
are parsed exactly, without binary floating-point conversion; zero amounts and fractions
of a cent are skipped. Pending records remain in the original file and can be imported
once booked. Other statuses and malformed records remain skipped evidence. Repeated
transaction IDs use the existing import deduplication, even if descriptions change.
Descriptions combine `counterpartyName`, `bankDescription`, and `externalMemo`. Account
balances are retained as raw data, not treated as historical closing balances. A later
reversal does not automatically undo an existing journal entry; review it against the
bank statement.

Verified against Mercury's published OpenAPI schemas for
[transactions](https://docs.mercury.com/reference/listtransactions) and
[accounts](https://docs.mercury.com/reference/getaccounts), its
[posting and amount definitions](https://docs.mercury.com/reference/events), and
[successful payment statuses](https://docs.mercury.com/docs/send-money).
Tests use synthetic evidence and local HTTP stubs; no live Mercury account has been exercised.

### IO credit accounts

Use the same read-only token with an explicit credit selection:

```sh
means mercury accounts --credit
means mercury pull --credit --dry-run
means mercury pull --credit
means mercury pull --credit --account CREDIT_ACCOUNT_UUID
```

Credit discovery uses [`GET /api/v1/credit`](https://docs.mercury.com/reference/listcredit).
This endpoint returns a complete `accounts` list without a page object. IO account
objects differ from depository objects: they do not require a name, kind, or type.
The evidence keeps the original object and sets `account_type` to `credit` outside it.
The importer accepts older depository envelopes that lack this field. Unknown
account types are rejected.

Credit pulls use the shared transaction endpoint, filtered by the credit account
UUID. They fetch all available history and follow every transaction page. An
unexpected credit-discovery cursor stops account discovery with an error.

Assign each IO account to a **USD liability account**. An asset account or a different
currency is rejected before import writes. A saved asset mapping leaves new credit
files pending for correction. Charges and fees keep their negative amounts; refunds
and repayments keep their positive amounts. The parser does not invert these signs.
A repayment can match the credit side of an existing bank-to-card payment. A refund
can use the normal refund review flow.

These signs follow Mercury's [transaction and balance definitions](https://docs.mercury.com/reference/events).
The [credit statement change](https://docs.mercury.com/changelog/credit-statement-endpoint-updated-balance-and-transaction-behavior)
uses positive charges and separate autopay data for statements generated after
June 15, 2026. This importer does not consume that statement endpoint or apply its
sign rules to transaction data. Live account balances stay as evidence; they are
not used as statement closing balances.

### Connection and recurring pulls

Create a **read-only** token in Mercury's organization settings and provide it through
`MERCURY_TOKEN` in your shell or scheduler environment. means never persists the token
in the ledger or evidence files. Mercury documents token scopes and Bearer authentication
in [Getting started](https://docs.mercury.com/docs/getting-started).

```sh
means mercury accounts
means mercury pull --dry-run
means mercury pull
means mercury pull --account ACCOUNT_UUID
means mercury pull --inbox /absolute/path/to/inbox
```

Without `--credit`, `accounts` lists supported account UUIDs, kinds, statuses and names. The pull selects
Mercury-owned accounts whose `kind` is `checking` or `savings`, including archived
accounts returned by the API; it reports excluded account counts. `--account` must name
one of those accounts. The first evidence file for an unmapped account waits in Review
for assignment. Subsequent files follow the saved provider-account identity.

By default, pulls fetch **all history currently available** through `/api/v1/transactions`,
filtered by `accountId`, with no starting date or status filter. That endpoint defaults
to the first transaction, whereas `/api/v1/account/{id}/transactions` defaults to a
30-day window. Full pulls revisit old pending transactions and backdated bookings;
provider retention and token access still bound what can be fetched. No incremental
date cursor is stored. Repeated settled records are retained as duplicate evidence by
the importer, not posted twice.

For books reconciled through September 18, fetch only the gap using an inclusive
September 19 cutoff:

```sh
means mercury pull --account ACCOUNT_UUID --booked-from 2026-09-19 --dry-run
means mercury pull --account ACCOUNT_UUID --booked-from 2026-09-19
```

This works for checking/savings and `--credit` IO pulls. The cutoff uses Mercury's
[`postedStart` filter](https://docs.mercury.com/reference/listtransactions) at UTC
midnight on every page, plus local validation of `postedAt` before publishing.
It does not filter by `createdAt`: a transaction created months earlier but posted
inside the gap remains included. Dates use UTC, matching the importer. Pending
rows are excluded with a cutoff; missing or invalid posting timestamps on other
rows stop publication of that account. Provider errors do not trigger an
unrestricted retry. Output counts older/pending records returned and excluded
locally, not records the provider omitted.

In TUI Connections, press `f` to save or clear a cutoff for a Mercury account.
Both CLI and TUI pulls honor that preference; explicit `--booked-from` overrides
it for one run. Without `--account`, the explicit cutoff applies to every selected
account. Clearing the saved cutoff restores full-history pulls. The preference
does not change already-imported history or repair old reconciliation differences.
Use a rehearsal inbox for the first actual pull if your usual inbox is watched by
the live importer.

Both account discovery and transaction fetching follow `page.nextPage` with
`start_after`, even after an empty page. Missing page metadata, malformed or repeated
cursors, repeated record IDs, HTTP failures and the 10,000-page safety limit stop the
pull with an error instead of publishing a truncated account. A completed account is
written to a hidden temporary file, flushed and renamed into the inbox before its
last-pull metadata is updated. Files for earlier completed accounts can remain if a
later account fails. Retry with another full pull. Dry-run fetches and validates the
same data without publishing files or modifying account mappings; opening a new ledger
path can still initialize its database.

The client uses GET requests only, refuses redirects and limits each request to 90
seconds. `--api` supports an alternative trusted HTTPS origin or loopback HTTP test
server; do not point it at an untrusted service, since it receives the token. Error
messages omit provider response bodies and request URLs. A read-only token must be
selected in Mercury: means does not inspect or downgrade the token's granted scope.

For scheduling, use a private wrapper that obtains `MERCURY_TOKEN` from your secret
store or protected environment file, then executes
`/absolute/path/to/means --db /absolute/path/to/ledger.db mercury pull`.
A cron entry `0 */6 * * * /absolute/path/to/wrapper` runs it every six hours. No scheduler
job is installed automatically. On 429 or other failure, let the next scheduled run
retry. Inbox processing remains the normal `means inbox`/watched-inbox workflow;
pulling evidence alone does not post entries.

## Mercury (`mercury_csv`)

The CSV from the Transactions page ("Export all" / "Export filtered"). Comma-separated, `MM-DD-YYYY`
dates, `.` decimals, signed `Amount`, USD only, no running balance, no id column.

Verified header (2022):

```
Date (UTC),Description,Amount,Status,Bank Description,Reference,Note
```

Before November 2022 the date column was called `Date`. Current exports add columns such as
`Source Account`, `Last Four Digits`, `Name On Card`, `Category`, `GL Code`, `Timestamp`,
`Original Currency`, `Check Number`, `Tags`; the fixture carries them, the parser ignores them.

Mapping: date = `Date (UTC)` / `Date`; amount = `Amount`; description = `Description` followed by
`Bank Description`; currency USD. `Status` other than `Sent` (`Pending`, `Failed`, `Cancelled`,
`Reversed`, `Blocked`) is skipped, as is a zero amount. `MM/DD/YYYY` is accepted as a fallback
without flipping day and month.

Verified:

- Header, `%m-%d-%Y` dates, legacy `Date` column, `Failed` rows, `$` possible inside amounts:
  [beancount-mercury checking.py](https://github.com/mtlynch/beancount-mercury/blob/master/beancount_mercury/checking.py)
  and [checking_test.py](https://github.com/mtlynch/beancount-mercury/blob/master/beancount_mercury/checking_test.py).
- Status values `pending, sent, cancelled, failed, reversed, blocked`:
  [Mercury API, list account transactions](https://docs.mercury.com/reference/listaccounttransactions).
- Where the export lives (Transactions page, .csv/.xlsx; QuickBooks and NetSuite CSVs are separate,
  pre-formatted files): [Mercury support](https://support.mercury.com/hc/en-us/articles/28768700685844).

Assumed:

- The names of the newer columns, and that foreign card purchases expose `Original Currency` /
  `Original Amount` (used as the original amount when both are present).
- The QuickBooks/NetSuite exports are not recognised.

## Banco Inter extrato CSV (`inter_csv`)

Semicolon-separated, no quoting (descriptions contain literal double quotes), `dd/mm/yyyy` dates,
`1.234,56` numbers with a leading `-`, BRL, running `Saldo`, a preamble before the header.

Current export (2026, UTF-8 without BOM):

```
Extrato Conta Corrente
Conta ;12345678
Período ;01/03/2026 a 31/03/2026
Saldo: ;2.415,86

Data Lançamento;Descrição;Valor;Saldo
02/03/2026;Pix recebido: "Cp :12345678-ACME COMERCIO LTDA";1.200,00;3.581,55
```

Older export (2020, ISO-8859-1, header on the seventh line, upper-case names, `R$` inside the
amounts, `- R$ 34,04` for debits):

```
DATA LANÇAMENTO;HISTÓRICO;VALOR;SALDO
```

A variant with both `Histórico` and `Descrição` (also seen tab-separated) is accepted: the
description is `Histórico` followed by `Descrição`.

Mapping: date = `Data Lançamento`; amount = `Valor`; description = `Histórico` / `Descrição`;
balance = `Saldo`; currency BRL. Rows whose description starts with `SALDO ANTERIOR`,
`SALDO DO DIA` or `SALDO FINAL` are skipped. Latin-1 files are decoded as such when they are not
valid UTF-8.

Verified:

- The 2026 layout measured on a real file (encoding, separator, no quotes, 4 preamble lines + 1
  blank, header, number format):
  [app-financeiro-2.0 formatos-de-extrato.md](https://github.com/davilucas156/app-financeiro-2.0/blob/4839a1635d6f7ee565281948a51dbdca12c78f97/references/formatos-de-extrato.md)
  and the same layout in [lupa-ui-design tests](https://github.com/MarcosJanczeski/lupa-ui-design/blob/ebc69bda70d03ebdd8dcd356e53fadb565594cf6/src/application/useCases/parseStatementImportCsv.test.ts).
- The 2020 layout (ISO-8859-1, `;`, header at line 7, `DATA LANÇAMENTO`/`HISTÓRICO`/`VALOR`, `R$`
  prefix): [extratoBancoInter Arquivo.py](https://github.com/marcoantoniosouza/extratoBancoInter/blob/master/Arquivo.py).
- The `Histórico` + `Descrição` variant: [pingo-financas tests](https://github.com/PedroVinicins/pingo-financas/blob/5b407a8abff124029c46eb40e2e62bc35f76eb61/src/services/__tests__/bankStatement.spec.ts).
- Inter offers PDF, CSV and OFX exports of the extrato:
  [Contabilizei](https://suporte.contabilizei.com.br/hc/pt-br/articles/360014137319-Como-importar-extrato-banc%C3%A1rio-do-banco-Inter).

Assumed:

- Whether the 2020 file had a `SALDO` column and a `SALDO ANTERIOR` row (the fixture has both).
- The `Saldo:` preamble line is the balance at export time; it is not used.
- The credit-card invoice CSV (`"Data","Lançamento","Categoria","Tipo","Valor"`, UTF-8 with BOM) is
  a different file and is not recognised.

## Banco Inter OFX (`inter_ofx`)

An OFX 1.02 SGML header (`OFXHEADER:100`, `CHARSET:1252`) followed by XML-style tags with closing
tags, `BANKID` 077, `CURDEF` BRL, `TRNTYPE` CREDIT/DEBIT/PAYMENT, `DTPOSTED` as
`YYYYMMDDHHMMSS[-03:BRT]`, `TRNAMT` with `.` decimals, `FITID`, `CHECKNUM`, `REFNUM`, `NAME` and
`MEMO`, a `LEDGERBAL` closing balance.

Mapping: date = `DTPOSTED`; amount = `TRNAMT`; reference = `FITID` (falling back to `REFNUM`);
description = `NAME` + `MEMO`, or the `MEMO` alone when it starts with the `NAME`; currency =
`CURDEF`; closing balance and date from `LEDGERBAL`; period from `DTSTART`/`DTEND`; account from
`ACCTID`. Both SGML (no closing tags) and XML styles parse; cp1252 files are decoded as Latin-1.

Verified:

- `BANKID` 077, closing tags, `PAYMENT` type, `CHECKNUM`, `REFNUM`, `NAME`/`MEMO` pairs:
  [my-coins OfxParserTest.php](https://github.com/mauricio-nunes/my-coins/blob/647bddd7a03d9f88e273d065f75afe126b03d07e/tests/Unit/OfxParserTest.php)
  and [OfxParser.php](https://github.com/mauricio-nunes/my-coins/blob/647bddd7a03d9f88e273d065f75afe126b03d07e/app/Services/OfxParser.php).
- Where the export lives (Extrato > Exportar > OFX):
  [Conta Azul](https://ajuda.contaazul.com/hc/pt-br/articles/360041036872-Banco-Inter-como-exportar-o-extrato-em-OFX).

Assumed:

- The signon block (`ORG`, `FID`), `BRANCHID`/`ACCTTYPE`, and the exact `FITID` shape.
- `cp1252` characters outside Latin-1 (curly quotes, `€`) decode to the wrong glyph; accented
  letters are fine.

## Remessa Online extrato CSV (`remessa_csv`)

Each row is one exchange. The help centre lists the columns of the PJ export:

```
Data de operação;Direção;Tipo de operação;Contraparte;Valor da moeda estrangeira;Valor na moeda em real;Spread;IOF;VET
```

Mapping: date = `Data de operação` (also `Data`, `Date`, `Criado em`); direction = `Direção`
(`Envio` / `Recebimento`; without it a negative BRL amount or a "recebimento" operation type means
inbound); foreign amount = `Valor da moeda estrangeira` (also `Valor enviado`, `Valor recebido`,
`Amount`, …); BRL amount = `Valor na moeda em real` (also `Valor em reais`, `Valor total`, `Total`,
…); currency = a `Moeda`/`Currency` column, else a code or symbol inside the foreign amount
(`USD 1.000,00`, `1,000.00 USD`, `US$ …`, `€ …`), else the mapping's `currency`; `IOF`; `Spread`;
`VET` (or `Taxa de câmbio`/`Cotação`/`Rate`); `Tarifa`/`Fee`; `Contrato`/`ID` as the reference;
`Contraparte` and `Tipo de operação` go into the description.

The BRL line is the movement on the bank account, signed by direction. When a VET is present the
parser checks whether the BRL column is already `VET × foreign amount` (the all-in total); if the
column is the principal instead, IOF and fee are added (outbound) or removed (inbound) to reach it.
Without a VET the BRL column is taken as the total. Delimiter, decimal separator (per column) and
date format are detected.

`run_import` turns each row into a draft exchange entry between the BRL account and the foreign
account given as `to_account_id` (IOF and fee to their expense accounts); rows in another currency
than that account are skipped with `EUR exchange; choose the EUR account`.

Verified:

- The nine column names and their meaning, that the export is a CSV filtered by period and
  direction: [Remessa Online help, extrato PJ](https://ajuda.remessaonline.com.br/como-acessar-o-extrato-pj-das-minhas-transferencias-na-remessa-online).
- VET as the all-in effective rate: [Remessa Online glossary](https://www.remessaonline.com.br/blog/glossario-das-transferencias-internacionais/).

Assumed:

- Delimiter (`;` in the fixture), `dd/mm/yyyy` dates, `1.234,56` numbers, `R$` prefixes, the spread
  as a percentage string, and how the currency appears (the fixture embeds it in the foreign
  amount; a `Moeda` column is also accepted).
- Whether `Valor na moeda em real` is the total or the principal (both are handled).
- The personal (PF) export: its help page describes the on-screen extrato only.
- No contract number in the PJ export; `Contrato`/`Número do contrato`/`ID` are read when present.

## Pluggy (`pluggy_json`)

`means pluggy pull` fetches this evidence file. Pluggy is a Brazilian aggregator that
already holds the accounts (Banco Inter, BTG, Nubank) through Open Finance. Meu Pluggy is its free
tier: the user creates a development application in the Pluggy Dashboard for a `client_id` and a
`client_secret`, adds the MeuPluggy connector to it, and connects each bank once by OAuth at
meu.pluggy.ai. Each MeuPluggy OAuth approval links one bank and creates a separate item: five
banks means five items in the ledger. Run `means pluggy pull --item <item id>` once for each bank;
subsequent pulls without `--item` fetch all remembered items. The connect flow is not in means;
the pull is.

What the pull does, in order:

- `POST /auth` with `{clientId, clientSecret}` answers an API key valid two hours, sent on every
  later call as the `X-API-KEY` header. The two credentials are read from `PLUGGY_CLIENT_ID` and
  `PLUGGY_CLIENT_SECRET`; a missing one names itself. They reach no file, no log line, no error
  message and no ledger row, and a refused authentication quotes nothing of what it sent. The base
  they are sent to is checked before they leave: `--api` must be an absolute `https` URL, or plain
  `http` to a loopback host (a test stub, a local proxy). Pagination must keep that exact origin
  (scheme, host and port); opaque cursors are encoded as query values. The Pluggy HTTP client
  refuses redirects, including authentication redirects, so credentials cannot follow them.
- `GET /items/{id}` identifies the connector and current status. MeuPluggy (connector ID `200`)
  does not support refreshing via `PATCH`, so the pull uses its current data; freshness depends on
  Meu Pluggy's own syncing. Other connectors receive `PATCH /items/{id}` to request fresh data.
  The pull requires `status` to be `UPDATED`, polling `GET /items/{id}` while it is `UPDATING`.
  `LOGIN_ERROR`, `WAITING_USER_INPUT` and `OUTDATED` stop the pull and say to connect that bank
  again at meu.pluggy.ai.
- `GET /accounts?itemId=` lists the accounts of that item.
- `GET /v2/transactions?accountId=…&createdAtFrom=…` pages through the transactions and follows the
  `next` cursor of each `{results, next}` answer to the end (a page holds 500 by default). Pluggy
  writes `next` as a whole URL; a bare cursor is accepted too and goes back as `after`.
- One file per Pluggy account lands in the inbox folder as
  `pluggy-<when>-<pluggy account id>.json`. Nothing else happens: the inbox imports the file, and
  the account of a Pluggy account that no import has placed yet is not guessed, so its file waits
  as a `pending` import like an unrecognised bank file. A `.json` file in the folder that is not a
  channel payload is ignored and left where it lies.

`createdAtFrom` is the cursor of the last pull (the moment it started, so a transaction recorded
while it ran is fetched again and the dedupe drops it), or the date of `--since`. It filters by when
Pluggy recorded the transaction, not by the day the bank booked it. `--dry-run` reports what it
would fetch and changes nothing at either end: no file, no cursor, and no item refreshed.

To avoid a period already covered by CSV imports, use `--booked-from YYYY-MM-DD`.
The cutoff is inclusive and uses the transaction's bank `date`, without a timezone
shift. If the CSV covers August 24, use August 25. `--account` selects a **Pluggy
account UUID**, not a means account ID. Set each account's cutoff from its own
coverage; checking and card accounts need not have the same last covered date.

```sh
means --db /tmp/rehearsal.db pluggy pull --item ITEM_UUID --account ACCOUNT_UUID --booked-from 2026-08-25 --dry-run
means --db /tmp/rehearsal.db pluggy pull --item ITEM_UUID --account ACCOUNT_UUID --booked-from 2026-08-25 --inbox /tmp/rehearsal-inbox
```

A booking cutoff fetches all available recorded history, then excludes older
booking dates locally. It ignores the saved recording cursor. An explicit
`--since` adds a recording-date restriction as well; omit it when avoiding a CSV
overlap. The report shows the excluded count and provider account UUID, and the
file records `bookedFrom`. A filtered pull does not advance the normal recording
cursor. A cutoff passed on the CLI applies to that invocation. Save a per-account
cutoff with `f` on the TUI Connections screen to reuse it in future TUI and CLI
pulls; an explicit `--booked-from` overrides that preference. Missing or invalid
booking dates stop that account's export.
The cutoff avoids the known overlap window; it does not prove a CSV was complete
or recover older backdated bookings outside that window.

The file, and what the parser reads back:

```json
{
  "channel": "pluggy",
  "pulledAt": "2026-09-18T09:12:30.412Z",
  "itemId": "a1b2c3d4-…",
  "createdAtFrom": null,
  "account": { "id": "b8e1…", "type": "BANK", "name": "Conta Corrente", "currencyCode": "BRL", … },
  "transactions": [ … ]
}
```

`account` and `transactions` preserve the JSON values Pluggy answered. JSON numbers retain their
full decimal precision through the pull, inbox file, and parsing into `Decimal`; they do not pass
through floating point. Numeric strings are accepted for transaction amounts and balances too.

Mapping: reference = `id` (a UUID, usually stable across syncs); date = the day of `date`, taken as Pluggy
wrote it with no timezone shift; description = `description`, or `descriptionRaw` when `description`
is empty; amount = `amount`; balance = `balance` when the transaction carries one; currency = the
transaction's `currencyCode`, else the account's. `raw` keeps the whole transaction object, so
`category`, `merchant`, `paymentData`, `providerCode` and `providerId` stay with the line.

The description also includes the counterparty from `paymentData`: `payer` for
money entering the account, `receiver` for money leaving it, after applying the
card sign convention below. Both plain strings and objects with a `name` are
accepted. For example, `Bankslip` becomes `Bankslip — EXAMPLE HEALTH`. The original text
stays in the description, and a name already present is not repeated. Missing
counterparties leave the text unchanged. Rules can match the enriched description.
Transaction IDs and raw payloads stay unchanged, so description enrichment does
not replace the provider identity used for deduplication. See Pluggy's
[payment data schema](https://docs.pluggy.ai/en/docs/products/transactions).

The sign follows the account `type`:

- `BANK`: Pluggy signs the amount the way the bank does, and a debit is already negative. It passes
  through.
- `CREDIT`: a new charge is positive and a payment negative, which is the other way round from
  "positive is money in". Both `amount` and `balance` are inverted, so a charge is money out of the
  card account and a payment is money in.

Skipped: a transaction with a nonempty `status` other than `POSTED` makes no statement line.
This includes purchases in an open credit-card invoice and future installments, so current card
expenses can arrive late. Pluggy usually keeps the ID when `PENDING` becomes `POSTED`, but may
delete and recreate a transaction when its amount, date, or description changes
([transaction lifecycle](https://docs.pluggy.ai/en/docs/products/transactions), checked 2026-09-19).
Import card purchases after settlement so the booked amount and bank reference are available.
The current `createdAtFrom` cursor also does not guarantee an in-place settlement update is fetched;
an explicit `--since` covering the original creation time can revisit that transaction. Skipped
transactions count toward `skipped_count`, and the inbox file retains them.

Each Pluggy file identifies its destination account through its envelope (every file has the same
shape) but the `channel_connections` row of that Pluggy account: the pull records the item, the
account, the cursor and when it last ran, and the first finished import writes which means account
it is. Connect a second bank and its files stay pending until they are placed, instead of landing in
the account of the first one.

Verified (at the desk, 2026-09-18, from Pluggy's API reference and the Meu Pluggy pages; the brief
of this cell records the reading):

- `POST /auth` and the two-hour API key on `X-API-KEY`; `/connect_token` is for client-side widgets
  and answers 403 on product data.
- Connector → item → account → transaction; `GET /accounts?itemId=`; accounts typed `BANK` (with
  `bankData`) or `CREDIT` (with `creditData`, `availableCreditLimit`, `balanceCloseDate`,
  `balanceDueDate`).
- `GET /v2/transactions` with `dateFrom`, `dateTo`, `createdAtFrom` and the `after` cursor, answering
  `{results, next}` with 500 per page; the transaction fields above, `providerId` only on Open
  Finance connections.
- The sign rule per account type, the item states, that automatic syncing needs production
  credentials, and that webhooks refuse localhost so a local engine polls.
- Pluggy's sandbox is "not intended for automated tests", so the fixtures here are written from the
  documentation, not recorded from an account.

Assumed:

- That `POSTED` and `PENDING` are the whole of `status`, and that a transaction without a `status`
  is settled (some Open Finance connectors leave the field out).
- That the `balance` of a `CREDIT` transaction is signed like its `amount`, so it is inverted with
  it. A real card payload will say.
- The shape of `next` when it is not a whole URL (a bare cursor is accepted).
- Post-trial rate and item limits are undocumented. An HTTP 429 stops the pull and says so; nothing
  retries around it.
- A transaction the bank later changes or deletes is not seen again: the pull only asks for what is
  new, and there are no webhooks on this tier.

### Overlap between CSV and bank connections

The normal matcher does not share a posting between bank sources. Use the
explicit cross-source workflow for overlap already imported into a rehearsal:

```sh
means --db /tmp/rehearsal.db rematch IMPORT_ID --cross-source --preview
means --db /tmp/rehearsal.db rematch IMPORT_ID --cross-source --apply PREVIEW_TOKEN
```

The preview lists both dates, their distance, the older entry to keep, and whether
confirmation will delete a draft or void a posted duplicate. Copy its token only
after reviewing every pair. Apply checks the preview against the current ledger in
one transaction; changed books require a new preview.

Matching requires the same bank account, signed native amount, and currency, with
dates at most **five days apart** (the normal import matching window). This covers
CSV value dates differing from Enable Banking booking dates. Both the older entry
and its supporting bank line must fall within that window. There must be one
eligible target posting and only one incoming line claiming it; even an exact-date
candidate is refused if another candidate exists in the window. The target must be
posted with evidence from another bank source. A posting cannot acquire a second
line from the same source through this command.

The older entry, categories, splits, tags, booked values and prior evidence stay
intact. An untouched, uncategorized import draft is deleted. An unreviewed entry
posted immediately by an import rule can instead be **voided with an equal reversal
on its own booking date**, including its original native quantities, booked values
and tags. Its rule-assigned category does not replace the older category. The newer
statement line moves to the older posting, retaining both sources of evidence.
This works across CSV, Enable Banking, Pluggy and other bank imports; it does not
widen automatic matching during an ordinary pull.

Human-reviewed or edited duplicates, entries linked to refunds/transfers or other
bank evidence, closed accounts, ambiguous matches and locked periods stay unchanged
with a note for manual review. Initial rule tags are allowed when their audit
history establishes that they were assigned during the import; later tag edits
are preserved by refusing the merge. Unmatched lines can attach without retiring
an entry. Every applied match is audited, and a failed application rolls back the
whole operation, including reversals.

For an existing overlap, run the preview on the **newer import ID** on a rehearsal
copy first. Do not roll back the older CSV import: an entry shared with another
import cannot be rolled back, and removing old entries would lose their categories.
After reviewing the rehearsal, make a backup and generate a fresh preview on the
intended ledger before applying its token. Removing only the added import evidence
does not clear the older evidence's reconciliation or resurrect a voided duplicate.

`means draft delete ID` deletes only a draft and retains its bank lines as
unmatched evidence. Posted and void entries are refused. Retrying the unmatched line may create a draft again.

## Double-check with a real file

- N26: the sign of `Original Amount`; whether pending card transactions appear at all.
- Revolut: that `Balance` equals the previous balance plus `Amount` minus `Fee` on a row with a fee.
- Wise statement: the date column format in your locale, and that the running balance walks by
  `Amount` across a card row with `Total fees`.
- Wise history: the `Direction` value of a conversion (`NEUTRAL` is assumed).
- Mercury: the current column names beyond the seven verified ones, and the date format of a
  recent export.
- Inter CSV: whether your export has `Descrição` only or `Histórico` + `Descrição`, and its encoding.
- Inter OFX: the `FITID` values are unique per transaction (they drive duplicate detection).
- Remessa Online: delimiter, number format, where the currency is written, and whether
  `Valor na moeda em real` is the total charged.
- Pluggy: the `status` values a real connection sends, whether a `CREDIT` transaction carries a
  `balance` at all, and what `next` looks like on a second page.

## Retrying an interrupted bank import

Inspect failed evidence before retrying:

```sh
means import lines ID --status error
means import lines ID --status error --json
```

The read-only command lists every matching line with its stored failure message,
date, amount, description and reference. Omit `--status` to see all lines, or use
`unmatched`, `created`, `matched`, `duplicate` or `skipped`. JSON includes the
stored evidence and links. Inspection never reprocesses the import.

Run `means import retry ID` after fixing a failed line's cause (for example a closed
account or a locked posting date). The command reads stored statement lines and retries
only unfinished or error lines without an entry or posting link. Completed, duplicate,
and deliberately skipped lines stay untouched; uploading the same file still reports
that it was already imported.

Automatic matching excludes voided entries and their generated reversals. If a voided
posting still owns a bank reference, its history stays intact; the replacement entry's
statement evidence retains that reference. After updating means, retry can recover
lines that previously failed with `a void entry cannot receive statement evidence`.
Preview the result on a ledger copy before using it in your books.

Each newly created bank entry, its rule tags, evidence link, reconciliation and rule-hit
count commit together. A failure leaves the line available for retry without an orphan
entry. Retry rebuilds import counts from the persisted lines, including after a process
stops between lines. Remessa retries retain their original exchange-account options.
Account Tracker backups use their dedicated import workflow.

## Full refunds

A positive bank line can refund an earlier expense without becoming income. During
import, recent equal-and-opposite purchases in the same entity and currency are offered
as refund candidates. The default lookback is 90 days. An original debit can already be
reconciled or in a locked period; it remains unchanged. Refunds may arrive in a different
bank/card account of the same entity, provided the native currency and full amount match.
Candidates are suggestions that require confirmation. On the initial import a candidate
causes a suspense draft instead of allowing a broad income rule to post the credit.

In TUI Review, select the credit's imported draft (or an unmatched credit) and press `f`.
Choose the original expense, or enter an older expense's entry ID, then confirm. The
entry detail shows `refund of #ID`. The CLI provides the same workflow:

```sh
means refund candidates LINE_ID
means refund candidates LINE_ID --days 365 --json
means refund link LINE_ID --original EXPENSE_ENTRY_ID --json
```

Linking posts a new credit on the statement account and reverses every original expense
split at its booked quantity and functional-currency amount, retaining the original tags.
The bank credit uses the refund-date rate; any difference goes to FX gain/loss. For example,
a USD 50 purchase booked at EUR 45 and refunded at EUR 46 reduces expense by EUR 45 and
records EUR 1 FX gain. A missing rate must be supplied first; means will not guess it.

The original purchase and its evidence remain posted and unchanged. The refund receives
its own bank evidence, audit event and `refund_of_id` link (also retained by JSON and
Beancount export). Creation, replacement of the credit's automatic suspense draft, and
evidence assignment commit together. Failure leaves the previous draft/evidence intact.
A purchase can have only one active full refund; duplicate bank evidence cannot create a
second one. The refund date must be in an unlocked period.

This link operation supports full refunds of purchases containing one bank/card debit
and expense splits, including signed discounts. It does not infer allocations for partial
refunds or mixed expense/asset purchases. Those still require manual postings. A credit
already posted or manually categorized cannot be replaced by this action; correct that
entry first. Explicit rule reruns and manual categorization remain available, so inspect
a suggested refund before accepting another category.

## Email attachments through IMAP

Use a dedicated IMAP mailbox or folder. Set `MEANS_IMAP_USERNAME` and
`MEANS_IMAP_PASSWORD` in the process environment. Use a provider app password
where required. The first receiver supports password authentication over TLS;
OAuth-only mailboxes are not supported.

```sh
means email pull --host imap.example.com --mailbox Receipts --dry-run
means email pull --host imap.example.com --mailbox Receipts
means email pull --host imap.example.com --mailbox Receipts --inbox /path/to/inbox
```

`pull` polls once. Run it periodically with your local scheduler. The default
TLS port is 993 (`--port` overrides it); certificates and hostnames are verified.
The default folder is `INBOX`. The default output is `inbox/` next to the ledger.
The receiver does not open the ledger. Run `means serve` to consume supported
attachments through the normal inbox pipeline.

Polling uses IMAP `EXAMINE`, `UID SEARCH ALL`, and `BODY.PEEK`. Messages stay on
the server, with their read flags unchanged. Every poll fetches current messages,
including previously read messages. Receipts prevent duplicate publication. There
is no UID checkpoint, so a mailbox rebuild or UID reset cannot skip messages.
Use a dedicated folder to keep repeated downloads small. Connection and socket
operations have 30-second timeouts.

The command prints JSON counts and failed UIDs. A malformed, oversized, or
undeliverable message does not block other messages. Any failed UID makes the
command exit with an error; fix the message or destination and retry. Connection
errors stop the poll. Previously completed deliveries remain safe to retry.
Server error text and message bodies are not printed. A dry run fetches and
validates messages but writes no files and changes no server state.

Transfer decoding preserves the attachment bytes. It does not convert character
sets or line endings. Named inline parts count as attachments. Message bodies do
not. Archives and attached messages stay as files. The normal inbox scanner
ignores formats it cannot import, such as PDF and ZIP.

Each stored filename includes the message hash and part number. The original
basename still selects learned account routes. The scanner checks that name and
content against the stored message before it uses the route. A missing or changed
message leaves the attachment in the inbox with an error.

The hidden `.email` directory holds the original message and a JSON receipt.
These files contain private mail data. Keep them with the inbox backup. The
receipt is written after all attachments. A completed receipt prevents delivery
of the same message again, even after the scanner moves its attachments. A retry
after a partial write checks existing files and publishes missing files. Normal
import checksums prevent duplicate postings. Existing files with different bytes
are never replaced.

The limits are 50 MiB per message, 50 MiB of decoded attachment data, 256 MIME
parts, 100 attachments, 100 nesting levels, 4096 headers per message, and 64 KiB
of headers per part. A preflight checks part and header limits before the MIME
parser allocates the message tree. Invalid transfer encodings and incomplete multipart
messages fail before any attachment is published. Dry runs write no files.

## TUI bank connections

The Connections screen (`8`) supports provider discovery, account mapping, saved
Pluggy, Enable Banking and Mercury booking cutoffs, background pulls, and navigation to Imports and Review.
It uses credentials from the server environment. See [connection setup and pull
semantics](connections.md). Saved Pluggy, Enable Banking and Mercury cutoffs also apply to CLI pulls; an explicit
`--booked-from` overrides the saved cutoff for that invocation.


## Inter PJ and Wise API evidence

`inter_pj_json` contains enriched BRL current-account statements; `wise_json`
contains business balance statements, with separate `profile:balance` identities.
Both are detected from their version-1 JSON envelopes. Source-wide filename routes
cannot assign these files: map the exact provider account and native currency.

See [Inter PJ and Wise setup](inter-wise.md) for CLI/TUI use, credentials, cutoffs,
pagination, fee metadata and rehearsal instructions.

## Splitting imported drafts

In **Review**, select a draft and press **Shift-S** to open the split editor shared
with **Capture** (`s` outside text fields). Use `a` to add a category, `e` to edit
an amount, and `x` to remove a leg. The category picker supports text filtering
and Ctrl-N to create a category. Leave only the last amount blank for the
remainder, which is shown before submission. Enter on the split list opens a
transaction preview; `y` confirms and `n` or Esc returns to editing without saving.
Capture saves splits to its form first; Ctrl-S then opens the same preview.
Single-category Review posting and batch posting also preview each entry before
confirmation. If the books change during confirmation, obtain a fresh preview. Review's lowercase `s` still skips statement lines.

The CLI accepts repeated targets:

```sh
means post 4214 --to "Expenses:Groceries=72.40" --to "Expenses:Household=18.10"
# Or let the final target take the remainder:
means post 4214 --to "Expenses:Groceries=72.40" --to "Expenses:Household"
```

Amounts are portions of the movement in the **bank account's native currency**,
for both payments and receipts. Enter positive portions normally, and explicit
negative amounts for discounts. They must cover the total exactly;
only the last leg may omit its amount. Single-account `means post ID --to PATH`
continues to work. Splitting requires one bank posting and one Uncategorized
posting for the CLI draft command.

In the TUI, **Shift-S** in an account ledger or Review, or **s** in an entry
detail modal, also splits an existing posted expense or income. Enter the new
allocation, preview every posting, then confirm. There is no two-leg limit.
This edits the category allocation with an audit record, retaining the bank
posting and evidence. Locked periods, evidenced/reconciled category postings,
paired transfers, reversals and linked refunds are protected.

Posting replaces only Uncategorized. The original bank posting keeps its ID,
quantity, booked value, exchange-rate provenance, reconciliation, metadata, and
statement evidence. The entry keeps its date, tags, notes and origin. Foreign
splits allocate the existing booked value, so a later rate change does not
revalue the bank movement. Earlier manual legs round independently and the last
absorbs rounding differences. Targets must belong to the same entity and accept
postings; locked periods and invalid allocations fail atomically.

## Existing-coverage warnings

An import warns when a newly created entry falls within five days of earlier
posted activity on the same ledger account. This includes rules that post new
entries immediately. The warning helps find overlaps where, for example, an old
payment was booked as two entries and the bank supplies one combined amount.

The check is conservative: one earlier posted movement is enough to flag nearby
new entries. It uses dates and account identity, not amount sums, and can flag
legitimate new activity. Drafts, void entries, reversal entries, and entries with equity postings do not establish prior coverage. Entries created by the current
import do not establish its own coverage. Exact matches and duplicate lines do
not count as new entries.

File previews show potential overlap before import. Completed imports retain the
warning in their options. The CLI and inbox summaries print it; TUI Imports marks
the row with `!` and shows the selected import's warning below the table. It is an
advisory recorded at import time, so it remains visible after later review.

Review the affected dates and the old bookings before accepting the import.
Set a per-account booking cutoff for future pulls when the earlier books are
complete. The warning leaves posting rules and matching behavior unchanged;
sum-aware reconciliation still needs a separate review workflow.
