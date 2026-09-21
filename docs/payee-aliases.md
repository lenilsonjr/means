# Payee names and aliases

Payees are scoped to one entity. A canonical payee has a stable identity, a display
name, an active/archive state and aliases. Press **9 → Payees** in the TUI.

Aliases match case-insensitive contained text. Matching trims and collapses
whitespace and preserves punctuation and accents. Several aliases for one payee
still produce one match; matches to different payees are ambiguous. Conflicts stay unresolved and appear in
previews and statement-line notes. Add a name as an alias when it should match
statement text.

## Set up and review

In the Payees screen, `n` creates a payee and Enter edits the selected payee.
Tab moves between the name, semicolon-separated aliases, and active state; space
toggles active. Archive a payee instead of deleting it. Existing links retain its
display name, while archived aliases stop matching new imports.

**Ctrl-P previews** a change. Inspect alias overlaps and affected statement lines,
including the old and new first matching rule. Up/down or PgUp/PgDn scroll the
preview; Backspace returns to editing. **Ctrl-Y applies the reviewed preview**;
Esc closes. Editing existing names and aliases uses the same preview gate. New
payees use candidate ID `0` in the preview until saved.

The preview checks existing evidence.
Even two disjoint aliases can appear together in a future description; those
matches will also remain unresolved. A changed plan or relevant state invalidates
the confirmation token and requires a fresh preview.

- `l` explicitly links an entry ID to the selected payee, after preview.
- `m` merges the selected payee into a target payee ID, after preview. Entry links
  and aliases transfer to the target; the source is archived. Conflicts and rule
  changes are previewed. Duplicate normalized aliases collapse to one.
- `h` previews historical linking for the current entity.

## Booked text and import rules

The entry's `payee` field remains its original booked text. `payee_id` is the
canonical link and `display_payee` is its current display name, falling back to
booked text when unlinked. Renames change historical displays without modifying
booked text, posting amounts, categories, source evidence or the ledger hash.
Payee changes and links have their own audit records.

For new imports, aliases resolve the raw statement description **before ordinary
and template rules**. A rule's `description` condition still sees raw text. Its
`payee` condition sees the resolved canonical name, falling back to raw text if
unresolved. An explicit rule payee action takes precedence. An explicit manual
payee text choice links only an exact canonical name; it is never overridden by
an inferred alias. Import deduplication and reconciliation still use unchanged
source evidence. An import matching an existing entry does not relink it.

TUI journal/review lists, entry details and account ledgers show current names;
entry details also show booked text. JSON retains `payee` alongside `payee_id`
and `display_payee`. Beancount uses the display name and preserves booked text in
`means_booked_payee` metadata.

In **7 → Reports**, `p` groups expense postings by canonical payee, with a separate
`[unresolved]` group for each distinct unlinked booked text. `f`/`o` set inclusive
dates and `/` filters an exact tag. Each expense posting is counted once at its
booked functional-currency value. Split expenses, refunds and void reversals
retain their signed values; totals equal the sum of groups. New refunds and voids
inherit the original entry's canonical link.

## Historical linking

To link history after adding aliases, preview `h`, inspect the entry
IDs, booked text, statement evidence and candidate IDs, then explicitly apply.
Applying also attests that the ledger's chart, classes, per-payee splits and ongoing
rules have been reviewed.

Historical inference uses statement evidence to identify the payee. Existing canonical links stay untouched. Unmatched and ambiguous
entries stay unresolved. Refunds and void reversals inherit the original entry's
canonical link or evidence-based candidate so they offset the same group.

The operation verifies the hash chain, takes a consistent pre-apply SQLite backup
beside the ledger (`LEDGER.payees-UUID.sqlite`), and applies all reviewed links and
audits in one transaction. Backup failure aborts the operation. A changed preview
is refused. Re-previewing already linked history produces no duplicate links or
link audits.

## CLI

The CLI prints full JSON previews. Repeat the **same command** with
`--confirm TOKEN` from its preview to apply it:

```sh
means payee list --entity 1
means payee save --entity 1 --name 'Coffee Shop' --alias 'cafe central'
means payee save --entity 1 --id 12 --name 'Coffee House' --alias 'cafe central'
means payee save --entity 1 --id 12 --name 'Coffee House' --alias 'cafe central' --archived
means payee link 456 --payee 12
means payee merge 12 34
means payee backfill --entity 1
means payee backfill --entity 1 --confirm TOKEN --chart-reviewed
means report expenses --entity 1 --group-by payee --from 2026-09-01 --to 2026-09-30 --json
```

`save` replaces the **complete alias list**; repeat `--alias` for each alias.
Omitting all aliases clears them. The TUI retains existing aliases when editing.
All CLI and TUI mutations use the same core preview and transaction logic, exposed
through `ListPayees`, `SavePayee`, `LinkPayees`, `ReassignPayee` and `PayeeExpenses`
on the local gRPC service.
