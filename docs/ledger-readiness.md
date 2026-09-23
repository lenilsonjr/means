# Ledger readiness review

Date: 2026-09-24. Baseline: `9a4ae85`.

This review covers ledger controls for external accounting tools.
It does not certify the books or an external report.
The current engine has useful controls.
It does not yet provide all the controls below.

## Findings

| Priority | Finding | Evidence | Required result |
|---|---|---|---|
| High | A transfer between entities used two commits. A failure could keep both entries without reciprocal links. A failed second entry could leave a false audit record. | `journal::create_transfer` in [journal.rs](../crates/means-core/src/journal.rs). | **Fixed in this change.** Create both entries and both links in one transaction. Include audit writes in that transaction. |
| High | Local clients can create posted entries. An origin string does not prove who sent a request. | `create_journal_entry`, `create_transfer`, and `confirm_entries` in [grpc.rs](../crates/means-server/src/grpc.rs). `create_entry_in_transaction` sets review state from `origin`. | A restricted client needs a separate access boundary. It must not receive database access or access to the trusted local service. |
| High | Imports can post through rules. Matching can post a draft. Templates can post on a schedule. | `rules::draft_from_line`, `matcher::attach_in_transaction`, and `templates::run_schedules`. | A draft-only policy must cover every route that can post. A check on one command is insufficient. |
| High | Missing exchange rates use a value of one. The engine can post these amounts with `rate_source='missing'`. | `value_functional`, `convert`, and the missing-rate test in [journal.rs](../crates/means-core/src/journal.rs). | Final reports must refuse unresolved values. A balanced journal can still contain wrong values. |
| High | A void entry still contributes to booked balances. Its reversal contributes on the reversal date. | `reports::sums` and [export.rs](../crates/means-core/src/export.rs). | External readers must include both `posted` and `void` entries. Keep the original and the reversal on their own dates. |
| High | The chain does not cover all report inputs. | `hashchain::canonical` in [hashchain.rs](../crates/means-core/src/hashchain.rs). It omits account classification, metadata, tags, review state, rates, and evidence links. | Hash the complete export payload. Preserve the manifest outside the working database. Do not use a chain head as proof of all report inputs. |
| High | Import rollback can delete posted entries and source evidence. It then repairs the chain. | `imports::delete_import` in [imports/mod.rs](../crates/means-core/src/imports/mod.rs). | A retention policy needs explicit controls on rollback, evidence deletion, and corrections. Current evidence retention is not unconditional. |
| High | Lock dates can move back. Several audit events have no full before/after data. Review marks have no actor record. | `entities::update_entity`, `journal::mark_reviewed`, and [audit.rs](../crates/means-core/src/audit.rs). | Record the actor, reason, approved input digest, and result. Keep filed revisions and later corrections separate. |
| Medium | Transfer links do not define a complete lifecycle. Posting or voiding an entry acts on one entry. | `post_entry` and `void_entry_in_transaction` in [journal.rs](../crates/means-core/src/journal.rs). | Define pair creation, independent source entries, matching, and corrections before using links for group reports. |
| Medium | Entry lists have a limit of 2,000. Other lists have separate limits. | `journal::list_entries`, `imports::list_lines`, and `reports::ledger`. | Use one consistent database snapshot for complete exports. Report record counts. Do not build an export from UI pages. |
| Medium | The overview balance sheet can show market values for holdings. It is not a complete set of financial statements. | `reports::balance_sheet` in [reports.rs](../crates/means-core/src/reports.rs). | Use explicit book-value reports for close. Define comparatives, cash flows, notes, and all required adjustment schedules. |
| Medium | Signed sharing copies omit import files and statement lines. The replica schema has a fixed version. | `SPECS` in [snapshot.rs](../crates/means-sharing/src/snapshot.rs), and `REPLICA_SCHEMA_VERSION` in [db.rs](../crates/means-core/src/db.rs). | Specify an evidence export separately. Test schema changes against old replicas and the sharing protocol. |

## Export contract

An external accounting export needs the following properties.

1. Read all selected entities in one database transaction.
2. Include archived entities and closed accounts when they belong to the selected scope.
3. Include all booked entries, including void entries and reversals.
4. Keep unresolved drafts in a separate section.
5. Include commodity precision and exact money values.
6. Use stable identifiers for all links.
7. Include account classifications, source evidence, rates, and review state.
8. Hash every payload file and evidence file.
9. Include record counts and control totals for each entity.
10. Reject missing files, broken links, unsupported versions, and mixed snapshots.
11. Write to a temporary destination.
12. Publish the complete export with one atomic rename.

Use the existing `Money` JSON form for monetary amounts.
Its minor units are strings.
Do not convert money to floating point.
Use decimal strings for rates.

A selected entity must not expose another entity's documents.
Cross-entity references need explicit scope and redaction rules.
The export must not include credentials or local source paths.

## Review and close

A preview digest detects stale input.
It does not prove human approval.
A service with authority to post must check a separate approval record.
That record must bind the actor, action, entities, input digest, and expiry.
It must permit one use.
The service must recheck the input and the lock state in the write transaction.

A restricted client must not write the ledger file, its WAL, or its directory.
It must not reach an unrestricted service that can perform the same action.
An environment variable is a workflow aid.
It is not an access control.

A final close must check these conditions:

- No unresolved draft, unreviewed entry, or suspense balance affects the period.
- All required rates and source records exist.
- Opening balances and bank coverage are complete.
- Each control account agrees with its supporting schedule.
- All adjustments have approval and evidence.
- The complete snapshot and its manifest have verified backups.

An unknown result must block final output.
A user can still generate a preview with a clear list of unresolved items.
An empty bank queue does not prove a complete accounting period.

## Failure tests

The transfer regression tests use fictional entities.
They inject failures on the second entry, the second audit write, and the final link.
They also test a locked destination and successful draft and posted pairs.
Each failure must preserve both books and the audit records.

The remaining controls need these tests before release:

| Case | Expected result |
|---|---|
| Original in one period; reversal in the next | Both period balances agree with the core reports. |
| More than 2,000 entries | Export contains every booked entry. |
| Account classification changes without a chain change | Export digest changes. |
| Missing rate with balanced postings | Final close fails. |
| Unmatched source period or missing opening balance | Final close fails. |
| Approval reused or input changed | Apply fails with no writes. |
| Restricted client calls a rule, template, import, or match operation | No entry posts without approval. |
| Document write fails after data validation | No committed document points to a missing file. |
| Restore after a failed write | Database and document hashes agree. |
| Old signed replica after a schema change | The reader follows the stated compatibility contract. |

## Scope

This change fixes transfer creation.
It also simplifies two equivalent boolean expressions to satisfy Clippy.
It does not change pair posting or pair reversal policy.
It does not add a restricted client, complete snapshot export, or close service.
Those changes need separate contracts and migration tests.

Validation passed with `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace`.
The workspace result was 353 passed, zero failed, and four ignored tests that require private input.
