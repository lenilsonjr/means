# Bank connections in the TUI

Open `means tui` and press **8** for Connections. This screen lists providers and
linked accounts across all entities. Setup and pulls use the local server's
credentials and inbox. Configure secrets in the server environment.

Export credentials in the environment that starts `means serve`, then restart
that server.

| Provider | Server environment |
|---|---|
| Pluggy / MeuPluggy | `PLUGGY_CLIENT_ID`, `PLUGGY_CLIENT_SECRET` |
| Mercury checking/savings and IO | `MERCURY_TOKEN`, with read-only API access |
| Wise business | `WISE_TOKEN` (US business token statement access) |
| Banco Inter PJ | `INTER_CLIENT_ID`, `INTER_CLIENT_SECRET`, `INTER_CERT_FILE`, `INTER_KEY_FILE`, `INTER_ACCOUNT_NUMBER` |
| Enable Banking | `ENABLE_BANKING_APP_ID`, `ENABLE_BANKING_KEY_FILE` pointing to the RSA private key |

The screen checks whether required variables are present. Discovery checks that
the credentials work. Provider errors appear in operation status. Credentials
are not saved in the ledger, sent through TUI RPCs, or included in job history.
The API still binds to loopback and rejects browser requests.

## Set up a bank

Select a provider and press Enter or `n`. Use Tab/up/down to change fields and
left/right to change a provider or bank choice. Escape closes setup; an operation
that has already started continues in the background.

**Pluggy / MeuPluggy:** press Ctrl-O to open `https://meu.pluggy.ai`. Approve a bank
there, then enter its item UUID in setup. Each bank needs its own item. Press
Ctrl-D to discover accounts. Discovery reads the item and account list. Use a
pull to fetch transactions. Repeat for each bank.

**Mercury:** choose checking/savings or IO credit, then press Ctrl-D. This lists
accounts accessible to the server's token. Treasury remains a separate
[evidence-capture CLI workflow](mercury-treasury.md).

**Enable Banking:** enter a country code and press Ctrl-B to fetch supported
personal banks. Type in the bank search field and use left/right to select a
result. Set the callback port registered on your application; the default is
53682, with redirect `http://127.0.0.1:53682/callback`. Press Ctrl-A to authorize.
The server opens your browser and captures the callback on loopback. The status
area also shows the authorization URL while waiting. Consent expires after the
bank's permitted period. Setup again renews it. Ctrl-D discovers accounts from
saved, unexpired consents without another authorization.

If your application needs an HTTPS callback through Tailscale or another proxy,
authorize once through the CLI with the same ledger file:

```sh
means --db /path/ledger.db enable-banking connect \
  --bank 'BANK_NAME' --country PT \
  --callback-port 53682 \
  --redirect https://means.example-tailnet.ts.net/callback
```

Register that exact HTTPS URL on the Enable Banking application. Forward its
`/callback` path and query string to `http://127.0.0.1:53682/callback` in the
environment where this command runs. For a container, the proxy must reach that
container's loopback listener. Configure this callback port separately from
the main means server proxy.

The callback accepts the configured URL's host or the exact local upstream
host. It does not trust `Forwarded` or `X-Forwarded-Host` to add accepted hosts.
The advertised URL must end in `/callback`, with no query, fragment, or embedded
credentials. Non-loopback URLs require HTTPS. The listener remains loopback-only.

After CLI authorization succeeds, use Ctrl-D in TUI Connections to discover
the saved consent. The TUI authorization button still uses the default local
redirect. No callback URL or authorization code needs to be pasted into means.

Browser authorization can take up to ten minutes. The TUI stays usable. You can
close setup or switch screens while the operation runs. The server accepts one
connection operation at a time. It also holds mapping edits until that operation
finishes. Existing CLI commands remain available.

## Map accounts before pulling

Select a discovered account and press Enter. Choose its destination ledger
account. The picker includes open asset and liability accounts across entities,
with the matching currency. Credit cards require liability accounts. Create a
missing destination in Accounts, then return to Connections.

Mappings apply to future imports. Past entries retain their accounts. Use Imports
to assign a pending file. Press `x` and confirm to remove a mapping; bank consent
and stored evidence stay in place.

For any supported bank connection, press `f` to set an inclusive booking-date cutoff. If CSV coverage ends
on August 24, set August 25 to exclude that covered window. This preference is
saved per provider account. Both TUI and CLI pulls use it. An explicit CLI
`--booked-from` overrides it for that pull; clearing the TUI field removes the
saved preference. Pluggy date-limited pulls do not advance the unrestricted cursor.

## Pull and review

Select an account, press `p`, and confirm the displayed scope and cutoff.

- Pluggy pulls the selected account. MeuPluggy items skip refresh and still
  require `UPDATED`; freshness comes from Meu Pluggy's own syncing.
- Mercury pulls the selected checking/savings or IO account using its saved
  booking cutoff, or all available history when no cutoff is set.
- Enable Banking pulls the selected account using its saved booking cutoff, or all
  available history when no cutoff is set.

Pulls run in the background. Files go to the server's configured inbox, including
when it was set with `means serve --inbox PATH`. The existing importer handles
matching, rules, duplicate detection, and drafts. Unmapped files stay pending.
A completed pull means the provider fetch finished; check each file's import
result before treating its postings as complete. Partial provider failure can
leave already fetched accounts in the inbox.

Press `i` for Imports and `v` for Review. In Review, categorize and post drafts,
inspect evidence, and confirm automatically posted entries. Cross-source matches
require a separate preview and confirmation. `r` reloads local status; only `p` pulls bank data.

The screen shows last-fetch timestamps and recent operation status. Press `o`
to read the full status or error for the selected account. Job history
survives a server restart. A job left running by a stopped server is marked
interrupted; inspect Imports before retrying. Pull retries use existing import
deduplication. History stores no credentials or authorization URLs. Bank-choice
lists and the active authorization URL are held only in server memory.

Wise business and Banco Inter PJ use Ctrl-D discovery, explicit account mapping,
`f` booking cutoffs and `p` account-only pulls. Inter needs a cutoff before its first
pull. See [Inter and Wise setup](inter-wise.md) for credentials and scope.
