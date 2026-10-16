# Changelog

This file lists the changes in behaviour that a user, an agent or a gRPC client must know before an upgrade.

## Unreleased

### Schema

- Migration 28 rebuilds the `entities` table. An entity name is unique among the active entities, not among all entities.
- Migration 29 adds archive guard triggers. After it, the books of an archived entity are read-only through every route: the CLI, gRPC and the TUI. Each write fails with the error code `entity_archived`. For accounts and entries, the message names the entity.
- Migration 30 adds the counterparty of an account (`accounts.counterparty_payee_id`).
- Migration 31 repairs migrations 28 and 29 on every ledger. It makes the active names unique without case, and it refuses to run when two active entities have names that differ only in case: rename one of them with the older build, then open the ledger again. It creates the archive guards again with literal messages, so the system SQLite 3.43 and `scripts/gen_erd.py` can read the ledger. It logs each archived entity with inter-vault links that are not void to entries of active entities.
- A shared vault replica that an older build installed at schema 27 is refused with "unsupported replica schema; obtain a new snapshot". Install a new snapshot.

### Entities

- `means entity list` shows the active entities only. Give `--all` to see the archived entities too.
- `entity archive` is refused while the entity has inter-vault links that are not void to entries of active entities. The error lists each link.
- An entity name that matches more than one archived entity, and no active entity, is ambiguous. The command fails and lists the ids. Give the id or the uid.
- gRPC `UpdateEntity` refuses a change of `archived`. Use `means entity archive` or `means entity unarchive`. A rename through gRPC checks the active names without case.
- Scheduled templates of an archived entity do not run.
- `means export` without `--entity` exported every entity, archived entities too. It now leaves the archived entities out. Give `--include-archived` to export them, or name one with `--entity`. The export names the entities that it leaves out on stderr, and `export --format snapshot --json` lists them in `skipped_archived`. When every entity is archived, the export without `--entity` and without `--include-archived` is refused. `entities.json` has `archived` for each entity.
- `entity unarchive <name>` takes the archived entity when an active entity has the same name.
- `entity unarchive` returns `live_links`. A cascade void refuses a link group with an entry of an archived entity.
- A bank pull skips a connection whose mapped account belongs to an archived entity.

### Entries and voids

- `means entries --json` prints one JSON object: `{"ok": true, "entries": [...], "count": N, "total": T, "schema_version": 1}`. Give `--jsonl` for the old format with one JSON object per line.
- A void of an entry with inter-vault links that are not void is refused without cascade. The CLI gives `invalid_input`. gRPC `VoidJournalEntry` gives `FAILED_PRECONDITION`; set the new field `cascade` to void the link group.
- A cascade void takes `--actor` (default `$USER`). The actor goes on each audit event of the cascade, with the reason. A gRPC void or rename records the actor of the `means-actor` request metadata, else `grpc`.
- A void preview never applies a cascade. The preview of `means void --json` and of `means entry void --dry-run` gives the reversal as `{"uid": null, "status": "posted", "date": D}`.

### Rates and rules

- `rates set`, `rates import` and `rates fetch` also revalue drafts. A draft with tax events stays as it is and is listed in `skipped_tax_events`. A revalued draft makes an open proposal fail with `stale_proposal`.
- `means rule apply` skips a draft that a human edited, and a rule with a template. A draft whose change fails goes to `failed`, and the run goes on.
- `import --from/--to`: when the range skips a line, the import stores no balance and no closing date from the file, and its period is the one of the drafted lines.
- `close check --json` and `close questions --json` return `generated_at`, `chain_head` and `entry_count`.

### Accounts

- `holding` is an asset subtype that `chart new --subtype` accepts.
- `chart new` takes the kind from the root of the path. A path under `Assets:` without `--kind` is now an asset account, not an expense account. A `--kind` that contradicts the root is refused. A path without a root that names a type is still an expense account.
- `chart new --statutory-code` checks the code as `account set` does. Both refuse a code that is not empty on a group account.
