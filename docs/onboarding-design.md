# First-run onboarding and TUI direction

Status: proposed for owner review. This document does not approve implementation.
Tracking: `means-2bi`. The owner asked to review the exact flow before it is built.

## What exists today

- The core and CLI can create an entity, called a vault in the product.
- One owned SQLite ledger file can contain several vaults.
- The CLI can open a chosen file with `--db`. The TUI connects to a running
  server; it has no file-opening launcher.
- The TUI imports Account Tracker backups and bank files, with previews and
  account mapping. These are data imports into owned books.
- Sharing can import an encrypted snapshot and open a separate read-only view.
  This is not a writable restore. It requires identity and trust setup.
- Beancount export is an output format. There is no Beancount import or general
  writable means vault-export restore flow.
- The server's `onboarded` flag derives from entity and account counts. It does
  not record whether someone has seen a tour. The TUI does not use it for one.
- Bare `means` currently starts the server. `means tui` opens the terminal UI.

## Proposed first experience

Use two short introductory screens, then a start chooser. No animation, timer,
sound, or forced tutorial tasks. The little cat carries a ledger. It appears
on the welcome and completion screens only. Accounting screens stay compact.

Target an 80-column by 24-row terminal. Keep the main card within 68 columns.
Use ASCII for the illustration and borders; make color optional. At smaller
sizes, hide the illustration, wrap text, and allow scrolling. Never hide the
focused field or action. Use a visible focus marker as well as color.

### Screen 1: welcome

```text
+------------------------------------------------------------------+
|                                                                  |
|       /\_/\                                                      |
|      ( o.o )    means                                            |
|       / >[_]    A little order for your money.                   |
|                                                                  |
|  Keep separate books for yourself and your businesses.           |
|  Bring in transactions, check what needs attention, and see      |
|  where you stand.                                                |
|                                                                  |
|  Your books stay on this computer. Bank connections are optional.|
|                                                                  |
|  [ Enter: Show me around ]     [ S: Skip introduction ]          |
|                                                    1 of 2        |
+------------------------------------------------------------------+
```

Enter opens screen 2. S opens the start chooser and records that the tour was
skipped. Escape offers Exit or Continue; it never silently creates a vault.

### Screen 2: the three concepts that matter

```text
+------------------------------------------------------------------+
|  A place for each set of books                          2 of 2   |
|                                                                  |
|  Vault                                                           |
|  Your personal books, or the books of one business.              |
|                                                                  |
|  Accounts                                                        |
|  Banks, cards, income, expenses, and other parts of those books. |
|                                                                  |
|  Accounting currency                                             |
|  The currency used for that vault's book values and reports.     |
|  Your EUR bank account can stay in EUR inside a USD vault.       |
|                                                                  |
|       Bring in data  -->  Review  -->  Understand                |
|                                                                  |
|  Each vault has its own totals. Company balances are not         |
|  automatically part of your personal net worth.                  |
|                                                                  |
|  [ Enter: Get started ]                         [ Esc: Back ]    |
+------------------------------------------------------------------+
```

Do not teach double entry, journal hashes, API sessions, or every screen here.
Introduce evidence links, pending imports, and posting status when the user
first encounters them. Help must remain available after the tour.

### Screen 3: choose a starting point

```text
+------------------------------------------------------------------+
|  How would you like to start?                                    |
|                                                                  |
|  > Create a new vault                                            |
|    Open existing means books                                     |
|    Import a file                                                 |
|    Open a shared vault                         read-only         |
|                                                                  |
|  Start with empty books for yourself or one business.            |
|                                                                  |
|  Up/Down: Choose    Enter: Continue    ?: Help    Esc: Exit      |
+------------------------------------------------------------------+
```

The description changes with the selection:

| Choice | Description |
|---|---|
| Create a new vault | Start with empty books for yourself or one business. |
| Open existing means books | Choose a means ledger or database backup. Your existing vaults stay together. |
| Import a file | Bring in an Account Tracker backup or bank statement. Preview before importing. |
| Open a shared vault | Open an encrypted read-only copy someone sent you. |

Do not use one ambiguous "Open export" action for all these operations. A file
picker can offer file-type detection, but it must show which operation follows.
An unsupported file stays untouched and gets a clear explanation.

### Branch A: new vault

One small form, followed by a review:

```text
  Your first vault

  Name                   [ Personal                         ]
  Books for              [ Me              v ]
  Accounting currency    [ Choose currency  v ]
  Country                [ Optional         v ]

  Each bank account keeps its own currency.
  Changing the accounting currency later requires a migration.

  Enter: Review     Tab: Next field     Esc: Back
```

- "Books for" offers Me or A business. The business choice suggests "Business"
  only while the name remains untouched.
- Require an explicit currency choice. Currency must not be inferred from the
  terminal locale or another company's vault. USD can be selected for Personal.
- Create the existing required system accounts. Do not apply the opinionated
  nomad chart without a separate choice. Offer chart templates later in Accounts.
- The review shows name, kind, currency, and storage location. Creating commits
  this setup once. Going back preserves the form. Cancel creates nothing.
- Finish with "Personal is ready" and actions: Connect a bank, Import a file,
  Add an account, or Explore my vault. Nothing requires credentials to finish.

### Branch B: existing means books

Pick a file using a browser or typed path. Inspect it read-only first. Show its
vault names and currencies, then confirm Open. When it contains several vaults,
ask which one to start in. Do not merge it into another database.

A database backup can be opened as a separate working copy. Make the copy
explicit; do not modify the only backup by default. If a schema upgrade is
needed, show that before opening and preserve a backup. A received replica must
open through the read-only sharing path. A missing server must never be treated
as proof that the user has no books.

### Branch C: import a file

Pick the file first, then inspect and explain its detected format:

- Account Tracker: choose or create destination vaults, then use the existing
  per-account mapping, preview, and balance-check flow. Do not infer that a
  whole backup belongs to one person.
- Bank statement: choose or create a vault and destination account. Show the
  account's currency. Reuse the existing parser, column mapping, and preview.
- Means database: offer Open existing means books. Do not feed it to an importer.
- Shared encrypted snapshot: offer Open a shared vault. Do not infer trust from
  the filename or import it as owned books.
- Beancount and unsupported exports: explain that this format cannot yet be
  imported, without attempting a generic conversion.

Show target vault, account, dates, transaction counts, and warnings before the
final import action. Finish with counts of new drafts, recorded entries awaiting
review, matched existing entries, and errors. Missing exchange rates stay visible;
do not describe the books as fully valued or reconciled if checks did not pass.

The route can create an empty vault before the import. Say so at that confirmation.
A later import failure leaves that vault available to retry; it must not create
another vault on retry. Cancellation never applies the import preview.

### Branch D: shared vault

Reuse the existing identity, fingerprint verification, snapshot import, and
signed-receipt requirements. If setup is missing, guide the user through those
steps with an option to return to the chooser. Do not hide trust steps to shorten
onboarding. Clearly label the opened vault Read-only and show how to return.

### Completion and later launches

The completed branch opens the relevant vault. Do not add a separate required
tour of all ten screens. Empty Home offers the same useful next actions.

First-run rules:

- Show the introduction automatically only when no books are configured and no
  local tour preference exists. An explicit existing-ledger or server launch goes
  straight to those books, with Welcome available in Help.
- Record completion or skipping in local UI preferences, not financial records
  or shared snapshots. Opening another vault does not replay it.
- Closing the app before completion resumes the tour at its last screen.
- Completing or skipping the introduction does not imply setup succeeded. If
  no books are available next launch, show the start chooser without the tour.
- Existing users with books get no automatic tour after an upgrade.
- Help > Welcome can replay it without resetting anything.
- Never change a vault merely by moving between onboarding screens.

## Launch behavior to approve

A file chooser needs a local launcher. It cannot be implemented only as another
screen in the current server-bound TUI. Proposed first release: add `means open`
to open the launcher and start or reuse the appropriate local engine. Preserve
the existing `means serve`, `means tui`, and bare `means` behavior. Decide whether
bare `means` should launch the UI in a separate compatibility change.

Use the platform's local configuration area for recent files and the tour state.
Keep those preferences separate from ledger backups and received snapshots.
The product flow shows vaults and files, not server ports or process setup.
Keep the explicit server connection command for users who need it.

## Broader navigation proposal

This is a separate proposal. It does not authorize changing shortcuts or moving
screens during the onboarding work.

| Main destination | User's question | Contents |
|---|---|---|
| Home | Where do I stand, and what needs me? | Selected-vault net worth, recent activity, next actions |
| Review | What needs checking? | Drafts, recorded entries awaiting review, matching decisions |
| Activity | What happened? | Journal, search, transaction detail and evidence |
| Accounts | Where is it held or classified? | Banks, cards, chart of accounts, holdings, reconciliation |
| Reports | What does it mean? | Financial reports, spending, budgets and trends |
| Data sources | How does information get here? | Connected banks, file import, import history and errors |

Add transaction becomes a global action. Payees, rules, templates, and chart
maintenance remain reachable through a visible Manage menu. Sharing belongs
with the vault menu. These are regroupings; existing capabilities remain.

Recommended language:

- Use Vault consistently in product navigation; explain Entity in accounting
  help. Keep existing CLI names compatible.
- Keep Accounts, Journal, Reconciliation, Debit, and Credit where they are
  accurate. Users should not need to translate invented accounting terms.
- Say "Needs an account", "Ready to review", or "Import failed" instead of
  using Pending for unrelated states.
- Keep record status, review status, and reconciliation status separate.
  A recorded entry can still await review. A matched statement is not proof
  that a whole account has been reconciled.
- Show a vault name and currency beside amounts whenever a screen spans vaults.
- Use actions such as "Review 8 transactions" instead of a bare drafts counter.
- Make Enter inspect or preview by default. Label actions that write records.
- Show progress during bank operations, retain form input on failure, and put
  full errors next to a retry action. Do not depend on a truncated status line.
- Keep screen names and common keys visible at normal terminal widths. Offer a
  command menu for less common actions rather than more number-key destinations.

The existing P1 audit issues should land before visual regrouping: mixed-vault
Review currencies (`means-x25.1`), vault names in import pickers (`means-x25.2`),
and stale rows after vault changes (`means-x25.3`). These affect trust in the UI.

## Research and validation

The short introduction plus in-context explanations follows the distinction in
[NN/g's onboarding guidance](https://www.nngroup.com/articles/onboarding-tutorials/).
The proposal keeps secondary tools available as needed, consistent with
[progressive disclosure](https://www.nngroup.com/articles/progressive-disclosure/).
Visible actions, clear feedback, and reversible navigation also fit the
[Command Line Interface Guidelines](https://clig.dev/). These sources inform
the proposal; they do not substitute for testing this accounting workflow.

Before implementation, review this flow and its launch behavior with the owner.
Then test a terminal prototype at 80x24 and 120x30, including monochrome.
Use these scenarios to test comprehension and operation:

1. Create personal USD books with an EUR bank account.
2. Open a file containing personal and company vaults without combining totals.
3. Import a statement, cancel its preview, then retry into the correct vault.
4. Open a shared read-only copy without mistaking it for owned books.
5. Exit during setup and return without another tour or a duplicate vault.
6. Recover from an unavailable engine, invalid file, and missing bank credentials.

Acceptance depends on choosing the correct vault, understanding currencies and
states, seeing each write before confirming it, and reaching useful work without
reading a manual. No implementation work starts until the owner approves the flow.
