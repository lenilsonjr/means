# Accounting model

means keeps a chart of accounts, journal, statement evidence, and audit trail for each vault. Reports read those records to show balances, income, expenses, and net worth.

The sections follow the schema: entities and their facts, accounts, the journal, evidence, tax, invoices, inter-vault links, close and filings, audit, and export. The [schema diagram](erd.md) shows every table and foreign key. [Tables](#tables) at the end names the section of each table.

## Vaults and entities

A vault is one entity’s set of books. The `entities` table holds each entity with its kind (`person` or `company`), country, functional currency and lock date. Personal and company vaults report their own balances. Overview shows the selected vault; consolidated reports are an explicit reporting choice.

Historical changes to a vault’s functional currency use the [currency migration workflow](currency-migration.md). It preserves native account quantities and requires a reviewed preview and backup.

## Entity facts, groups and parties

These records describe the books. They never change postings.

Each entity keeps its legal form, fiscal year end (`MM-DD`), incorporation date and registrations. Registrations are a JSON object of `KEY: value` pairs, for example a registration number or a tax number. Keys are uppercase free text. An empty value removes a key.

`entity_classifications` records the tax classification of an entity in a jurisdiction, for example `US` `disregarded_entity` or `SG` `company`. One classification applies per jurisdiction on a date. A new open classification closes the earlier open one in the same jurisdiction on its start date.

`entity_groups` names a group of entities. `entity_group_members` puts an entity in a group, with an optional parent entity, the ownership that the parent holds in basis points (10000 is 100%) and a control flag. An entity is in one group at a time. A parent must be a member of the same group for the whole period of its child. The group tree shows the effective ownership along the path from the root, rounded down.

Each payee keeps its country (ISO 3166-1 alpha-2), tax ids as a JSON object, and a kind: `person`, `company`, `authority` or `other`. `party_relationships` links a payee to the entity that keeps it, with a kind such as `owner`, `director`, `customer` or `lender`, an optional ownership in basis points and the basis for the record. A related party for reporting is a payee with an open `owner`, `subsidiary`, `director` or `related_party` relationship to the entity on the date.

Classifications, memberships and relationships use dated periods. A period includes its `effective_from` date and excludes its `effective_to` date. An empty `effective_to` keeps the period open. An "as of" query returns the rows whose period includes that date. To close a period, set `effective_to` to the first day that the fact no longer applies. Two periods of the same fact cannot overlap. Every change goes to the audit log with its values before and after.

The commands are `means entity set|classify|facts`, `means group` and `means party`, and `means payee set`. See the [agent interface](agent-interface.md#entity-facts-groups-and-parties).

## Accounts and statutory codes

Every account belongs to one entity and one of five types:

| Type | Typical use | Increases with |
| --- | --- | --- |
| Asset | Bank account, cash, investment | Debit |
| Liability | Credit card, loan | Credit |
| Equity | Opening balances, capital | Credit |
| Income | Salary, sales | Credit |
| Expense | Groceries, rent, software | Debit |

Accounts form a hierarchy. Placeholder accounts group children; postings use leaf accounts. Spending categories are expense accounts. An expense account can have a class (`fixed`, `committed`, `discretionary`, `savings` or `not-spending`) for reports and budgets. A system role marks the accounts that the engine creates with the entity, such as `Uncategorized` and `FX gain/loss`.

Each account has a commodity. `commodities` holds each currency, security or crypto asset with its precision. `prices` holds the price of one commodity in another on a date, with its source. ECB rates (`means rates fetch`), hand-entered rates (`means rates set`) and CSV rates (`means rates import`) all go in this table. A posting in a commodity without a direct or an inverse rate takes a cross rate through a common quote, such as EUR. Its `rate_source` is then `cross:<SOURCE>`, for example `cross:ECB`. See [the rate lookup](agent-interface.md#drafting).

Three columns map an account to statutory reporting. They never change postings:

- `statutory_code`: a code from a national chart or a reporting taxonomy, as free text, for example a PGC account number.
- `tax_treatment`: a free-text tag for tax reports, for example `output_tax`, `input_tax` or `owner_capital`. Invoices use `input_tax` and `output_tax` to find the tax account.
- `intercompany`: 1 when the account holds balances with a related party. Inter-vault links and the close check use it.

A [chart template](../charts/README.md) sets these columns when it creates the accounts.

## Journal entries and postings

`journal_entries` holds each dated event. `postings` holds its lines. The postings of an entry belong to accounts in the same entity. The sum of their book values is zero: debits are positive and credits are negative.

Each posting stores:

- Its account and native quantity.
- Its amount in the entity’s functional currency.
- The conversion rate and source when applicable.
- A memo and retained metadata.
- External references, fingerprints, and reconciliation state when supplied.

A USD vault can hold a EUR bank account. Its postings retain EUR quantities and USD book values. Quantities and book values use integer minor units with explicit commodity precision. For example, 549 minor units with precision 2 means 5.49.

A transfer between entities uses linked entries, one in each set of books. The engine writes both entries, both counterpart links and the [inter-vault link](#inter-vault-links) in one transaction. Currency differences post to FX gain or loss. A full refund reverses the original expense splits at their booked values and records any exchange difference separately. `refund_of_id` links the refund to the original entry.

### Entry lifecycle

Draft entries wait for a booking decision. Posting confirms their accounting structure. Human review is a separate state (`reviewed_at`), so a rule can post an entry that still waits for review.

The `draft_only` posting policy stops rules, schedules, the matcher and imports from posting. An agent can create drafts through `means entry`, and a human confirms them. See [posting policy](agent-interface.md#posting-policy).

A split distributes a movement across several accounts. For an imported expense, the bank posting and its statement link stay intact while the expense postings change. Preview the complete transaction before confirming it in the TUI.

A void retains the original postings and creates a linked reversal (`reverses_id`). The reversal negates native quantities and book values. The original entry’s evidence returns to the unmatched queue. Lock dates apply to the correction date.

Lock dates protect closed periods. Reconciled postings retain their account and quantity during recategorization. Operations that would alter protected accounting data fail with an explanation.

### Payees, tags, templates, rules and budgets

`payees` holds the canonical payees of an entity. `payee_aliases` holds the texts that match each payee. A canonical payee gives related entries a shared display name (`payee_id`). The original booked payee text stays in the journal. [Payee aliases](payee-aliases.md) describe matching, conflicts, merges, and history previews.

`entry_tags` attaches `key:value` tags to entries. Tags are outside the hash chain, so a retag does not rewrite it. A tag-grouped report counts a tagged expense under every matching tag and labels the overlapping totals.

`entry_templates` generates balanced entries from fixed amounts, percentages, and a balancing remainder. Percentage splits use largest remainder allocation with template order breaking ties. Manual splits assign rounding differences to the last leg. A template can have a recurrence rule, and scheduled templates run through an explicit schedule command. An entry keeps the template and its version.

`rules` choose an account, a payee, tags or a template from statement data. Rules run in their position order. Refund candidates and ambiguous cross-source matches require review.

`budgets` applies one limit to an inclusive date range. It can cover a category and its children, an expense class, or a tag. The default period is a calendar month. A budget never creates postings. See [reports and budgets](expense-reports.md).

## Evidence: statement lines and documents

### Imports and statement lines

`imports` records the source, file checksum, file content, options, result counts, and the period and balances of the statement. `statement_lines` holds each line: the provider’s raw record, date, amount, currency, description, reference and running balance. Its processing state identifies whether it was matched, created, skipped, marked as a duplicate, or left for review.

Matching links a statement line to a posting. The posting holds the accounting decision; the statement line holds the bank evidence. Several confirmed sources can evidence one posting through cross-source reconciliation.

Deduplication uses the source’s identity rules. Enable Banking lines with a stable reference use that account-scoped reference; lines without one fall back to a fingerprint. Other formats can use fingerprints when their export IDs change. A fingerprint incorporates the date, amount, description, account, and occurrence within the file.

See [import formats and recovery](import-formats.md) for each source’s behavior and confirmation steps.

### Documents

A document is evidence that is not a bank line: an invoice, a receipt, a contract, a statement, or a filed form. Each document belongs to one entity. Its row in `documents` records the SHA-256 hash, the original file name, the MIME type, the size, the source, the received date, and free metadata. The source is `email`, `upload`, `api`, `filing`, `import`, or `other`.

The file content lives in a content-addressed store next to the ledger file:

```
<ledger directory>/documents/<first two hex digits of the hash>/<sha256>
```

The row keeps this path relative to the ledger directory. The original file name is metadata only. The store rejects a file name that contains a path separator or `..`. The MIME type comes from the file extension; an unknown extension gives `application/octet-stream`.

The store writes a file in this order:

1. Write the bytes to a temporary file in the target directory.
2. Flush and sync the temporary file.
3. Read the file back and compare its hash with the expected hash.
4. Rename the file into place.
5. Insert the row and its audit record in one transaction.

If the insert fails, the store removes the file, unless the file existed before. The store never overwrites a stored file. One entity keeps one row per hash: a second add of the same bytes returns the first row and reports `created: false`. Two entities can share one stored file.

`document_links` attaches a document to a journal entry, a posting, or a statement line of the same entity. A link has an optional role, for example `invoice` or `receipt`. The same document, target, and role can occur only once. The evidence of an entry includes links to the entry, to its postings, and to the statement lines that match it. The audit log records every new document and every new link.

A link follows its target. An entry edit moves a posting link to the replacement posting. If the posting has no replacement, the link moves to the entry. A deleted draft, a rolled-back import, or a merged duplicate removes its links. The document stays, and `means doc list --unlinked` shows it again.

No command deletes a document. `means doc verify` reads every stored file again and reports missing files and hash mismatches. Include the `documents/` directory in every backup of the ledger file.

| Command | Result |
| --- | --- |
| `means doc add <file> --entity <e>` | Stores the file and optionally links it (`--link-entry`, `--link-line`, `--role`, `--meta KEY=VALUE`). |
| `means doc list --entity <e>` | Lists documents, newest received first (`--unlinked`, `--since`). |
| `means doc link <uid> --entry <uid>` or `--line <id>` | Adds a link. |
| `means doc show <uid>` | Shows the row, its links, and the stored path. |
| `means doc verify` | Checks every stored file against its hash. |

See the [agent interface](agent-interface.md#evidence) for the JSON output and the error codes.

## Tax codes and tax events

One tax code with one rate cannot feed a real return. means records a tax event for each taxed posting instead. Returns sum the events by code, kind and period. The mapping of these sums to the boxes of a return stays outside the engine.

`tax_codes` holds dated rate versions of an entity. Each version has a code (one uppercase word, for example `SR`), a name, a kind, a rate in basis points (900 is 9%), an optional tax account, a recoverable share in basis points (default 10000) and a jurisdiction. The kinds are:

| Kind | Tax lines that the engine adds |
| --- | --- |
| `output` | The tax on the tax account. |
| `input` | The recoverable part on the tax account. The rest goes on the account of the taxed posting as a cost. |
| `reverse_charge` | The tax negated and the recoverable part on the tax account. The rest goes on the account of the taxed posting. The lines sum to zero. |
| `withholding` | The tax negated on the tax account. The other party receives less. |
| `exempt`, `zero_rated`, `out_of_scope` | None. The rate must be 0. The event records the base. |

A version applies from `effective_from` (inclusive) to `effective_to` (exclusive). A new version closes the open version of the same code on its start date. Other overlaps fail. The tax account must be a leaf account of the entity in the functional currency. A code without a tax account adds no lines. Every change goes to the audit log.

`tax_events` holds one row per taxed posting and tax point:

- The entry, the posting and the tax code version.
- The tax point. It decides the period. The default is the entry date.
- The base, the tax, the recoverable part and the withheld part, in minor units of the functional currency.
- The native base and native tax, when the taxed posting holds another commodity.
- The other party (a payee), a document reference, the rule version (`CODE@effective_from`), a correction link and metadata.

Amounts follow the side of the taxed posting: debit positive. A sale on an income account gives a negative base and a negative tax. A purchase gives positive amounts. The recoverable part never exceeds the tax.

### Tax blocks on entries

A posting input can carry a tax block: a code, and optionally a base, a tax, a tax point, a party and a document reference. An entry input can carry a default block for each income or expense posting without its own block. The rule is:

1. The caller gives net amounts.
2. The engine takes the base from the functional amount of the posting, unless the block gives one.
3. The engine computes the tax as base × rate / 10000, rounded half-even to the currency precision, unless the block gives one.
4. The engine records the event and adds the tax lines of the kind. The balancing posting absorbs the tax lines, so the entry balances.

Block amounts are magnitudes. The engine gives them the sign of the posting. A balancing posting cannot carry a block. Each tax line has `tax_line` (the code) and `tax_role` in its metadata. See the [tax block](agent-interface.md#tax-block) of a draft document.

### Tax event lifecycle

- A void creates mirror events on the reversal. They have negated amounts, the tax point of the reversal and `corrects_event_id` set to the original event. The original stays in its period. The correction falls in the period of the reversal.
- A deleted draft deletes its events.
- An edit with tax blocks replaces the events. The engine drops the input postings marked `tax_line` and adds new tax lines.
- An edit without tax blocks keeps the events and moves them to the replacement postings. Send the blocks again to compute new amounts.

The tax summary and the event list read the events of posted and void entries. Drafts do not count.

| Command | Result |
| --- | --- |
| `means tax code add --entity <e> --code SR --name "Standard rated" --kind output --rate-bp 900 --tax-account <path> --from D` | Adds a version (`--recoverable-bp N`, `--jurisdiction J`). |
| `means tax code list --entity <e> [--as-of D]` | Lists every version, or the versions in effect on the date. |
| `means tax code close --entity <e> --code C --to D` | Ends the open version. |
| `means tax summary --entity <e> --from D --to D` | Sums base, tax, recoverable and withheld by code, kind and rate. |
| `means tax events --entity <e> --from D --to D [--code C]` | Lists the events. |

## Invoices, open items and settlements

The engine does not issue invoices. It records an invoice that the entity received or issued elsewhere as a fact, and links it to the ledger. The cash date, the tax date and the accrual date can differ, so the invoice keeps its own dates.

An invoice belongs to one entity. Its row in `invoices` has the direction (`sales` or `purchase`), the party (a payee of the same entity), the series and the number, the issue date, the tax point, the due date, the commodity, and the net, tax, gross and withheld amounts in minor units of that commodity. The net plus the tax equals the gross. The withheld amount is the part of the gross that the payer keeps back and pays to an authority. It is from zero to the gross. One entity has one invoice per direction, series and number. `invoice_lines` keeps the description, the quantity as a decimal string, the unit price, the net, the tax code, the tax and an optional account of each line. The lines add up to the net and the tax of the invoice. An invoice can point to a stored document.

The recognition entry books the invoice on its tax point. The engine creates it as a draft with the origin `invoice`. A human posts it.

| Direction | Debit | Credit |
| --- | --- | --- |
| `purchase` | Each line account, and the tax account | The payable control account (gross) |
| `sales` | The receivable control account (gross) | Each line account, and the tax account |

The input names the control account (`payable_account` or `receivable_account`). The invoice keeps it for later settlements. A line without an account uses the `account` of the invoice.

A line whose `tax_code` is a tax code of the entity on the tax point gets a tax block on its posting. The engine then records one tax event for the line and adds the tax lines, as for a [draft with a tax block](agent-interface.md#tax-block). The event has the line tax, the invoice party and the invoice label as `document_ref`. The block applies when the code kind fits the invoice: `input` for a purchase, `output` for a sale. A zero-rated, exempt or out-of-scope code applies when the line tax is zero. A reverse-charge code applies to a purchase line with zero tax, and the engine computes the tax from the rate. A code without a tax account records the event, and the tax goes to the tax account below.

For the other lines, the engine makes one tax posting for each tax code. The tax account is the `tax_account` of the input. Without it, the engine uses the one open leaf account of the entity with the tax treatment `input_tax` (purchase) or `output_tax` (sales). An invoice in a foreign commodity uses the `rate` of the input, or the rate on the tax point. The control posting takes the rounding difference, so the entry always balances.

`settlements` links an entry, or one posting of it, to an invoice. Its amount is in the invoice commodity. A positive amount reduces the open balance. The kind is `payment`, `credit_note`, `withholding`, `fee`, `fx_difference`, `write_off` or `refund`. A `payment` and a `refund` need an entry. Only a `refund` and an `fx_difference` can be negative.

- The settled amount is the sum of the settlements whose entry is not void. The open balance is the gross less the settled amount.
- The status follows: `open` with nothing settled, `partially_settled` with part settled, `settled` when the settled amount reaches the gross. A cancelled invoice stays `cancelled`.
- A settlement that takes the settled amount above the gross fails with `over_settlement`. A `write_off` is the exception.
- The `withholding` settlements of an invoice cannot exceed its withheld amount. For a supplier invoice with withholding, the bank pays the gross less the withheld amount. A second entry moves the withheld amount from the payable to the withholding liability.
- A credit note is an invoice of kind `credit_note` with the direction, the commodity and the control account of the invoice that it credits. Its recognition entry turns every side around. It settles the invoice by its gross, with a settlement of kind `credit_note`. It cannot exceed the open balance.
- A cancel needs a reason and an invoice without settlements. It deletes a draft recognition entry. A posted recognition entry must be void first. A cancelled credit note stops settling its invoice.
- An entry edit moves a settlement to the replacement of its posting. A deleted entry removes its settlements.

An exchange difference applies to a `payment` or a `refund` with a posting. The value paid is the functional amount of the posting. When the posting holds the invoice commodity, the engine takes the part of it that the settlement covers. The booked value is the part of the control posting of the recognition entry that the settlement covers, or the amount at the invoice rate without a recognition entry. The difference is the value paid less the booked value. With `--fx-account`, the engine creates a draft entry between the control account and that account:

| Direction | Positive difference | Negative difference |
| --- | --- | --- |
| `purchase` | Debit the FX account, credit the payable | Debit the payable, credit the FX account |
| `sales` | Debit the receivable, credit the FX account | Debit the FX account, credit the receivable |

Without the FX account, the settlement reports the difference and books nothing.

### The duplicate rule

A bank import can book an expense before the invoice arrives. The recognition entry of the invoice then books the same expense again. The engine applies this rule:

1. `find_duplicate_expense` returns bank entries of the entity that look like the payment: a draft or posted entry within 3 days of the date, with the same party, a bank posting of the amount (money out), at least one expense posting, and no settlement. A bank entry has a statement line, or the origin `import` or `rule`. `means invoice show` lists these entries as `possible_duplicates`. For an invoice, the amount is the gross less the withheld amount, and the window runs from 3 days before the issue date to 3 days after the due date.
2. A `payment` or a `refund` from such an entry converts it when the invoice has a recognition entry that is not void. The settlement moves the expense postings (revenue postings for a sale) of the bank entry to the control account through the ordinary entry edit, in the same transaction. The amounts, the bank posting and its evidence stay.
3. The expense then stays once, on the recognition entry. The bank entry becomes a payment of the payable.

An invoice without a recognition entry converts nothing: the bank entry is then the only booking of the expense.

| Command | Result |
| --- | --- |
| `means invoice add --entity <e> (--json-file F \| --stdin) [--recognize]` | Records an invoice, and optionally its draft recognition entry. |
| `means invoice list --entity <e> [--open] [--direction sales\|purchase] [--as-of D]` | Lists the invoices of an entity. |
| `means invoice show <uid>` | Shows the lines, the settlements, the open balance, the linked entries and the documents. |
| `means invoice settle <uid> --date D --amount X [--entry <uid>] [--posting <uid>] [--kind K] [--fx-account <path>]` | Records a settlement. |
| `means invoice credit-note --entity <e> --credits <uid> (--json-file F \| --stdin)` | Records a credit note that settles the invoice. |
| `means invoice cancel <uid> --reason R` | Cancels an invoice without settlements. |

## Inter-vault links

An inter-vault link joins two journal entries of different entities. Group reporting uses the links to find the two sides of each intercompany flow. `counterpart_id` stays on the entries of a transfer, but the link carries the kind, the amounts and the status of the pair.

`intervault_links` has one row per pair:

- `entry_a_id` and `entry_b_id`: two different entries. The engine refuses two entries of the same entity. One pair has one link, in either order.
- `kind`: `transfer`, `capital`, `loan`, `interest`, `dividend`, `service`, `recharge`, `settlement` or `other`.
- `amount_a`, `commodity_a`, `amount_b`, `commodity_b`: the native amount of the intercompany side of each entry, in minor units. The side is the sum of the postings on accounts with `intercompany = 1`. An entry without such postings uses its largest posting, and the result has a warning.
- `rate`: the units of commodity B per unit of commodity A, from the two amounts. Two amounts in one commodity give `1`.
- `note` and `status`: `draft`, `posted` or `void`.

The audit log records every new link, every status change and every removal.

### Pair lifecycle

| Event | Result |
| --- | --- |
| Link, when either entry is a draft | The link is `draft`. |
| Link, when both entries are posted | The link is `posted`. |
| Post one entry of a draft link | The link stays `draft` until the other entry is posted too. Then it becomes `posted`. |
| Void one entry | The engine voids the other posted entry in the same transaction, with the same reason. The link becomes `void`. A linked draft stays a draft. |
| Void, when the period of the other entity is locked | The engine refuses the void with `locked_period` and names the entity. Nothing changes. Give a void date after the lock date, or unlock the period. |
| Delete a linked draft | The engine deletes the link. |
| Unlink | Allowed only while both entries are drafts. It needs a reason. For posted entries, void the pair. |

A void without a date reverses each entry on its own date. A void with a date reverses both entries on that date.

`create_transfer` between two entities writes a link. Its kind is `transfer`, unless the caller gives another kind. A transfer inside one entity has no link. Transfers from before migration 25 keep `counterpart_id` and have no link. Link them with `means intervault link` when a group report needs them.

### Reconciliation and exceptions

A link is reconciled when the current amount of entry A at the stored rate equals the current amount of entry B, within one minor unit of commodity B. The engine compares magnitudes. An edit of a linked entry after the link can make the link unreconciled.

The exception list of an entity shows posted postings on intercompany accounts whose entry has no link. Void entries and their reversals cancel each other, so the list leaves them out. A group close resolves each row: link the entry to its other side, or explain it.

| Command | Result |
| --- | --- |
| `means intervault link --a <entry uid> --b <entry uid> --kind loan [--note T]` | Links two entries. |
| `means intervault unlink <link uid> --reason R` | Removes a link of two drafts. |
| `means intervault list [--group G \| --entity E] --from D --to D [--status S]` | Lists the links with an entry in the period, with the `reconciled` flag. A group keeps the links where an entity is a member on the date of its entry. |
| `means intervault exceptions --entity E --from D --to D` | Lists the unlinked intercompany postings. |

An agent can also link a new draft with the `intervault` block of `means entry draft`. See the [agent interface](agent-interface.md#inter-vault-block).

## Close, lock and filings

A close check reads one period of one entity and lists findings. It writes nothing. An error blocks a final close. A warning needs attention but does not block it. An info finding needs no action.

| Code | Severity | Condition |
| --- | --- | --- |
| `unresolved_draft` | error | A draft entry is dated in the period. |
| `unreviewed_entry` | error | A posted entry in the period has no review mark. |
| `missing_rate` | error | A posting has `rate_source = 'missing'`, or a posting in a foreign commodity has no rate. |
| `suspense_balance` | error | A suspense account has a non-zero balance in the period. A suspense account has the system role `suspense` (Uncategorized) or a path segment named `Uncategorized` or `Suspense`. |
| `lock_moved_back` | error | The lock date is before the highest lock date that the audit log records since the last unlock. |
| `unbalanced_period` | error | The debits and the credits of the booked entries in the period differ. |
| `unmatched_evidence` | warning | Statement lines in the period have no posting link. Skipped and duplicate lines do not count. |
| `intercompany_out_of_balance` | warning | An account with `intercompany = 1` moved in the period, and posted, reconciled inter-vault links do not cover the movement. `amount` is the part without such a link. `link_uids` names the links that are not posted or do not reconcile. Reconcile the account with the other entity in a group close. |
| `intercompany_linked` | info | Posted, reconciled inter-vault links cover this movement of an intercompany account. `amount` is the linked total and `link_uids` names the links. A report without `intercompany_out_of_balance` can say that all intercompany flows are linked. |

An entry counts as linked when it has at least one link that is not void, and each such link is `posted` and reconciled. The check reads the movement of each entry of the entity in the period. It then subtracts the movement of the linked entries. A void link does not count, because a void entry and its reversal cancel each other.

A report has the findings, their counts, and control totals: booked entries, drafts, postings, statement lines, debits, credits and the balance. Booked entries are `posted` and `void` entries. The `report_digest` is the SHA-256 of the canonical JSON of the report.

A lock moves the lock date of the entity forward. The check runs over the days that the lock closes: from the day after the old lock date to the new lock date. An error finding refuses the lock. The `--force` option locks the period with the errors. The audit event of the lock records the actor, the reason, the old and the new lock date, the finding counts, the force flag and the report digest.

An unlock moves the lock date back. It needs an actor and a reason, and it always writes an audit event. The gRPC call `UpdateEntity` can only move the lock date forward. A move back through it fails with a locked error.

`filings` records a return or a report for an authority. Each row has the entity, the jurisdiction, the filing code, the period (both days included) and a status: `prepared`, `submitted`, `accepted`, `rejected` or `amended`. It also keeps the digest of the books snapshot, the digest of the filed artifact, the receipt reference, the submission time, notes, and the sequence and hash of the last chained entry of the entity when the filing was recorded.

The status follows these steps:

| From | To |
| --- | --- |
| `prepared` | `submitted`, `rejected` |
| `submitted` | `accepted`, `rejected`, `amended` |
| `accepted` | `amended` |
| `rejected` | `prepared`, `submitted` |

A move to `submitted` needs a receipt reference. A filing in status `submitted` or `accepted` freezes its period. An unlock that reopens a day of a frozen period fails with `filed_period`. With `--amend`, the unlock creates a new filing in status `prepared` for each frozen filing. The new row points to the original filing through `amends_id`. The original filing keeps its status. A second unlock uses an amendment that is still `prepared`. The audit log records every filing change with its actor and reason.

| Command | Result |
| --- | --- |
| `means close check --entity <e> --from D --to D` | Lists the findings of the period. |
| `means close lock --entity <e> --through D --actor A --reason R [--force]` | Moves the lock date forward. |
| `means close unlock --entity <e> --to D --actor A --reason R [--amend]` | Moves the lock date back. |
| `means filing record --entity <e> --jurisdiction J --code C --from D --to D [--status S]` | Records a filing (`--snapshot-digest`, `--artifact-digest`, `--notes`). |
| `means filing status <uid> --status S [--receipt R] [--at D] [--actor A]` | Changes the status of a filing. |
| `means filing list --entity <e> [--jurisdiction J] [--code C]` | Lists the filings of an entity. |

## Audit

Entries have stable identifiers and a hash chain. Each chained entry has a sequence number, the hash of the previous entry and its own hash. `means verify` recomputes the chain of every entity. A permitted edit recomputes the hashes of the later entries of the chain. The chain does not cover every report input, for example tags, review marks and evidence links. The [snapshot export](snapshot-format.md) hashes the complete payload.

`audit_log` records audited changes with the table, the row, the action and the values before and after. It has an `actor` and a `reason` column. A lock, an unlock, a filing change and `means entry confirm` fill them. Use verification, backups, and independent review together to check the books.

`settings` holds ledger-wide values as key and value pairs, for example `posting_policy`.

## Reports and exports

Balances, trial balances, income statements, and net worth are derived from the ledger. Reports keep entity scope explicit. Expense reports use stored book values. Currency-converted account reports round each account first, then sum the displayed rows. A missing required rate stops the report with an error.

Ledger running balances follow chronological order. Sorting the rows changes their presentation while preserving each posting’s balance.

The `means report` commands give the trial balance, the balance sheet, the income statement, the cash flow, the general ledger, open items, the tax summary and net worth. They read posted and void entries at booked values. See [reports](agent-interface.md#reports).

[Beancount exports](export.md) reproduce functional-currency book values and keep original quantities and commodities in metadata.

A [snapshot export](snapshot-format.md) writes every record, the evidence files, the counts and the control totals into one hashed directory.

## Sharing

[Read-only vault sharing](vault-sharing-usage.md) exchanges signed, encrypted snapshots through files. Each received vault uses a separate database. Recipient views stay separate from owned-vault totals. The [wire format](vault-sharing-wire-v1.md) defines signatures, receipts, and validation.

A received vault installs into a new database that migrates to the current schema (`Db::open_replica_staging` in `db.rs`). The version-1 wire format fills the tables of schema version 16 (`REPLICA_SCHEMA_VERSION`). Newer columns take their defaults. The replica reader opens a replica only when its schema is the current schema. A replica that an older build installed needs a new snapshot.

## Operational tables

These tables hold connection state. They hold no credentials and no accounting data.

- `import_profiles`: the account that an inbox file goes to, by source and file-name pattern.
- `channel_connections`: one provider account of a pull channel (Pluggy, Enable Banking, Mercury, Wise or Inter PJ), its means account, its cursor and its booking cutoff.
- `connection_jobs`: the state of the background pulls that the TUI starts.
- `enable_banking_sessions`: the saved Enable Banking consents and their expiry dates.
- `enable_banking_account_aliases`: the stable account hashes that keep a route after a new consent.

## Tables

| Table | Section |
| --- | --- |
| `entities` | [Vaults and entities](#vaults-and-entities) |
| `entity_classifications`, `entity_groups`, `entity_group_members`, `party_relationships` | [Entity facts, groups and parties](#entity-facts-groups-and-parties) |
| `accounts`, `commodities`, `prices` | [Accounts and statutory codes](#accounts-and-statutory-codes) |
| `journal_entries`, `postings` | [Journal entries and postings](#journal-entries-and-postings) |
| `payees`, `payee_aliases`, `entry_tags`, `entry_templates`, `rules`, `budgets` | [Payees, tags, templates, rules and budgets](#payees-tags-templates-rules-and-budgets) |
| `imports`, `statement_lines` | [Imports and statement lines](#imports-and-statement-lines) |
| `documents`, `document_links` | [Documents](#documents) |
| `tax_codes`, `tax_events` | [Tax codes and tax events](#tax-codes-and-tax-events) |
| `invoices`, `invoice_lines`, `settlements` | [Invoices, open items and settlements](#invoices-open-items-and-settlements) |
| `intervault_links` | [Inter-vault links](#inter-vault-links) |
| `filings` | [Close, lock and filings](#close-lock-and-filings) |
| `audit_log`, `settings` | [Audit](#audit) |
| `import_profiles`, `channel_connections`, `connection_jobs`, `enable_banking_sessions`, `enable_banking_account_aliases` | [Operational tables](#operational-tables) |
