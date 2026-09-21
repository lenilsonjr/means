# Expense reports

Use `means report expenses` to total expenses by account class:

```sh
means report expenses --entity 1 --from 2026-09-01 --to 2026-09-30
means report expenses --entity 1 --tag trip:porto --json
```

The entity ID is required. Dates are optional and inclusive. The report rejects a
start date after the end date. Omitted dates leave that end of the range open.

Each expense posting contributes its booked amount in the entity's functional
currency. Current exchange rates do not change this amount. Drafts are excluded.
Refunds reduce expenses. Original void entries and their posted reversals both
contribute on their respective dates. Parent account totals are not added again.

Rows use the current class on each posting's expense account: `fixed`,
`committed`, `discretionary`, `savings`, or `not-spending`. An account without a
class appears as `Unclassified` in text output and an empty class in JSON. The
report includes all expense classes, including `not-spending`. It does not infer
cash spending from the class. Rows with a zero net amount are omitted. The total
is the sum of the displayed rows.

`--tag` accepts one exact entry tag. For example, `trip:porto` does not match
`trip:portugal`. A bare `trip` matches only a tag with an empty value. Input uses
the normal tag parser: lowercase text and an optional leading `#`. Multiple tags
are rejected. Selecting an entry includes all its expense splits once, even if
it has other tags.

JSON amounts are structured Money objects. The report includes the entity ID,
date range, normalized tag, class rows, and total. For example, EUR 12.34 is
`{"minor":"1234","commodity":"EUR","precision":2}`. The `ExpensesByClass`
gRPC method exposes the same calculation. Its amount strings use the existing
report wire format and the response states their currency.

New voids copy the original tag pairs exactly. The reversal, tag copy, and void
status update commit together. Migration 12 repairs older system reversals that have no tags and no recorded
tag edits. It copies the original entry’s current tag pairs and records each
change in the audit log. Existing tags and recorded manual edits, including
manual removal of all tags, are preserved. The repair and its audit records
commit together. Accounting hashes do not change. Migration 13 extends this
repair through chains of reversals, including databases that already ran
migration 12. A recorded manual tag removal stops propagation through that entry.

## Grouping by tag

Use `means report expenses --entity 1 --group-by tag` (optionally with the same
inclusive dates and exact `--tag` filter). Each entry contributes its full expense
amount under every tag it carries. Rows therefore overlap: a €10 expense tagged
`trip:porto with:friends` appears as €10 in both rows. The report's `total` counts
each posting once; it is not the sum of tag rows. Untagged entries get a separate
row (`tag: null` in JSON). JSON and `ExpensesByTag` RPC responses include
`overlapping: true` to identify this non-additive grouping. Zero-net rows are omitted.

In the TUI Reports screen, `c` selects classes, `g` selects tags, `f` edits the first
date, `o` edits the last date, and `/` edits the exact tag filter. Dates initially
cover the current month through today; clear either date to leave that end open.

## Budgets

Budgets are plans over expenses: they create no journal entries or transfers.
A limit covers one entire inclusive date range, with no rollover or monthly
reset inside that range. Omit both dates to use the current calendar month;
when choosing dates, supply both. Limits use the entity's functional currency,
round to its precision, and may be zero but not negative.

Choose one scope:

- Category: `--account ID`, including all current subcategories.
- Class: `--class committed` (or another expense class; `unclassified` selects none).
- Tag-only: `--tag trip:porto` without an account or class.

Category and class budgets may also include one exact tag filter. Spending uses
the same booked values, draft exclusion, refunds, and dated void/reversal semantics
as expense reports. Category membership, class assignments, and entry tags are
read from their current state. Budget rows can overlap and have no combined total.
A negative remaining amount means the limit has been exceeded; a net refund can
make remaining exceed the original limit.

```sh
means budget create --entity 1 --name Groceries --account 42 --amount 400
means budget create --entity 1 --name Commitments --class committed --amount 1500
means budget create --entity 1 --name Porto --tag trip:porto --amount 600 \
  --from 2026-09-01 --to 2026-11-30
means budget list --entity 1 --on 2026-10-01 --json
means budget list --entity 1 --tag trip:porto
means budget update 3 --entity 1 --name Porto --tag trip:porto --amount 650 \
  --from 2026-09-01 --to 2026-11-30
means budget delete 3
```

`update` replaces the complete definition. `list` includes every period unless
`--on` selects budgets valid on a particular day. This selector does not truncate
spending: each row always covers its own full period, including future-dated posted
entries within it. The list's `--tag` filters budget definitions by their stored tag,
not by adding another filter to their spending. JSON uses structured Money amounts.

In TUI Reports, press `u` for budgets. Press `n` to create, Enter to edit, `d` to
delete with confirmation, and `v` to switch grouping between class and tag. `/`
filters budgets by their stored tag. Select a row to see its period, target, and tag.
The editor uses Tab/up/down for fields, left/right for scope and target, typed text
to search categories, Ctrl-S to save, and Escape to cancel. Category trees spanning
classes and tag-only budgets appear under `mixed`; no arbitrary allocation of
the limit across classes is implied. The screen includes budgets from all periods.

Migration 14 adds budget storage. Creation, replacement, and deletion are audited
and atomic. Merging a leaf expense category transfers its budgets to the surviving
expense account and audits each change; a merge into income requires moving or
deleting those budgets first. A parent category that remains after a merge keeps
its budget. `SaveBudget`, `ListBudgets`, and `DeleteBudget` expose these operations
to the local TUI through gRPC.

Payee grouping is available through `means report expenses --entity ID --group-by payee` and TUI Reports `p`. Canonical payees and unresolved booked text form distinct groups; aliases, history previews and preserved booked text are described in [payee aliases](payee-aliases.md).
