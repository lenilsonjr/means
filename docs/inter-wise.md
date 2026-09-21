# Banco Inter PJ and Wise business statements

These connections fetch statements into the normal inbox. Map each provider account
in TUI **Connections**, then review imports and postings through the existing workflow.
Credentials stay in the environment; the TUI stores account mappings and booking
cutoffs only. Both connectors use read access.

## Banco Inter PJ

The first version covers the **BRL business current account**, using the enriched
statement API.

In Inter Empresas Internet Banking, create an integration and enable statement
reading (`extrato.read`), activate it, and download the client certificate and
private key. Set these variables in the environment running means:

```sh
export INTER_CLIENT_ID='your-client-id'
export INTER_CLIENT_SECRET='your-client-secret'
export INTER_CERT_FILE='/secure/path/client.crt'
export INTER_KEY_FILE='/secure/path/client.key'
export INTER_ACCOUNT_NUMBER='001234567'
```

The certificate and key must be PEM files. Keep the account number's leading zeros
and check digit; omit punctuation. The account is sent as `x-conta-corrente` on each
statement request. This version manages one Inter account per credential environment.

```sh
means inter-pj accounts
means inter-pj pull --booked-from 2026-09-19 --dry-run
means inter-pj pull --booked-from 2026-09-19 --inbox /path/to/rehearsal/inbox
```

`accounts` verifies access using the balance endpoint. Set opening entries through
the normal ledger workflow. In the TUI, select **Banco Inter PJ**, press Ctrl-D, map the
account, then use `f` to set the inclusive booking cutoff and `p` to pull.

Inter requires a date range, and the account's opening date is not available from
this discovery path. **The first pull requires `--booked-from` or a saved cutoff.**
An explicit CLI cutoff overrides the saved value for one run. Pulls cover that date
through today in São Paulo, in consecutive 30-day intervals. Every interval is
fully paginated before the account file is published. A missing ID, invalid date,
changing page counts or failed page stops publication of that account.

Amounts use `tipoOperacao` (`C` or `D`) for their sign; `dataTransacao` is the booking
date. `dataInclusao`, enriched details and bank transaction IDs remain in raw evidence.
The OAuth token is requested only with `extrato.read`, over mutual TLS, and is held
in memory. Expired/revoked certificates or provider errors require fixing setup
and retrying; there is no fallback to a weaker authentication method.

Protocol references: [Inter developer portal](https://developers.inter.co/) and
[Inter's official SDK](https://github.com/inter-co/pj-sdk-java), including its
`BankStatementClient`, `EnrichedTransaction`, OAuth and HTTP-header definitions.

## Wise business

This version uses a **business API token**, suitable for the confirmed US business
profile. Wise restricts token-based statement access by profile country. A 403
produces an access/SCA explanation; means does not bypass SCA or implement partner
OAuth. See [Wise token access](https://docs.wise.com/guides/developer/auth-and-security/personal-api-token).

Create a token under your Wise business profile's **Connect and manage apps → API
tokens**, with read access to profiles, balances and statements, and set:

```sh
export WISE_TOKEN='your-business-api-token'
means wise accounts
means wise pull --account PROFILE_ID:BALANCE_ID --booked-from 2026-09-19 --dry-run
means wise pull --account PROFILE_ID:BALANCE_ID --booked-from 2026-09-19 --inbox /path/to/rehearsal/inbox
```

Discovery lists business profiles' standard and savings/jar balances. Invested
balances are excluded because their holdings need a separate accounting model.
The stable `profile:balance` identity keeps profiles and same-currency jars separate.
Each balance maps to an asset account in its native currency; the vault may use a
different accounting currency.

In TUI Connections, select **Wise business**, press Ctrl-D, map each balance, then
use `f` for its cutoff and `p` to pull that balance. Without an explicit or saved
cutoff, means starts at the balance's `creationTime`. If Wise omits that date, set
an explicit cutoff. Without `--account`, CLI pulls all discovered business balances.

Pulls use the pinned `2026Q3` API and consecutive requests of at most 365 days,
below Wise's 469-day limit. Booking dates use UTC. COMPACT statements supply one
signed native movement per transaction. Fees, original foreign amounts, conversion
details and running balances remain in raw evidence. Use rules or review to
categorize fees, identify transfers, or split the movement. The posted native
amount equals the statement movement; `totalFees` stays in evidence.

References: [balance accounts](https://docs.wise.com/guides/product/accounts/balance-accounts)
and [balance statements](https://docs.wise.com/api-reference/balance-statement/balancestatementget).

## Review and recovery

`--dry-run` fetches and validates without publishing files or learning mappings.
An actual pull writes private JSON evidence only after fetching the whole account.
If another account fails later, already completed account files remain available.
Repeat pulls deduplicate through the bank's transaction references. Missing or
conflicting references stop the pull with an error.

Cutoffs limit future evidence. Earlier imports remain in the ledger; review
historical overlaps across different ledger accounts separately. Start with a rehearsal copy
and separate inbox. The `--inbox` option matters when the live server watches the
usual inbox: publishing there can trigger its normal rules and imports.

Tests cover synthetic
statements, native signs/currencies, retained metadata, mapping isolation, replay,
HTTP pagination/errors, TUI selection and a mutual-TLS handshake with synthetic
certificates. Rehearse real statement samples before using either source in live books.
