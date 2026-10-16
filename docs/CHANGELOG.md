# Changelog

This file lists the changes in behaviour that a user, an agent or a gRPC client must know before an upgrade.

## Unreleased

### Schema

- Migration 28 rebuilds the `entities` table. An entity name is unique among the active entities, without case. The migration refuses to run when two active entities have names that differ only in case. Rename one of them with the older build, then open the ledger again.
- Migration 29 adds archive guard triggers. After it, the books of an archived entity are read-only through every route: the CLI, gRPC and the TUI. Each write fails with the error code `entity_archived`, and the message names the entity.
- Migration 30 adds the counterparty of an account (`accounts.counterparty_payee_id`).
- A shared vault replica that an older build installed at schema 27 is refused with "unsupported replica schema; obtain a new snapshot". Install a new snapshot.

### Entities

- `means entity list` shows the active entities only. Give `--all` to see the archived entities too.
- `entity archive` is refused while the entity has inter-vault links that are not void to entries of active entities. The error lists each link.
- An entity name that matches more than one archived entity, and no active entity, is ambiguous. The command fails and lists the ids. Give the id or the uid.
- gRPC `UpdateEntity` refuses a change of `archived`. Use `means entity archive` or `means entity unarchive`. A rename through gRPC checks the active names without case.
- Scheduled templates of an archived entity do not run.
- `means export` leaves the archived entities out by default, as before. It now names them on stderr, and `export --format snapshot --json` lists them in `skipped_archived`. `entities.json` has `archived` for each entity.

### Entries and voids

- `means entries --json` prints one JSON object: `{"ok": true, "entries": [...], "count": N, "total": T, "schema_version": 1}`. Give `--jsonl` for the old format with one JSON object per line.
- A void of an entry with inter-vault links that are not void is refused without cascade. The CLI gives `invalid_input`. gRPC `VoidJournalEntry` gives `FAILED_PRECONDITION`; set the new field `cascade` to void the link group.
- A cascade void takes `--actor` (default `$USER`). The actor goes on each audit event of the cascade.
- A void preview never applies a cascade. The preview of `means void --json` and of `means entry void --dry-run` gives the reversal as `{"uid": null, "status": "posted", "date": D}`.

### Rates and rules

- `rates set`, `rates import` and `rates fetch` also revalue drafts. A draft with tax events stays as it is and is listed in `skipped_tax_events`. A revalued draft makes an open proposal fail with `stale_proposal`.
- `means rule apply` skips a draft that a human edited, and a rule with a template.

### Accounts

- `holding` is an asset subtype that `chart new --subtype` accepts.
- `chart new --statutory-code` checks the code as `account set` does.
