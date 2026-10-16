//! SQLite connection, pragmas and migrations.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use rusqlite::Connection;

use crate::Result;

/// A migration is a script, or code when a row needs a decision that SQL cannot make.
/// `Rebuild` is a script that recreates a table. It runs with foreign keys off, as the SQLite
/// procedure for a table rebuild requires. `check` runs first and can refuse the migration with
/// a message for the user. After the script, the runner refuses the migration when it finds more
/// foreign keys without a parent row than before the script.
enum Migration {
    Sql(&'static str),
    Code(fn(&Connection) -> Result<()>),
    Rebuild { sql: &'static str, check: fn(&Connection) -> Result<()> },
}

// Rules for a new migration:
//
// - Migration 29 adds the triggers `archive_guard_*`. They refuse each write to the rows of an
//   archived entity. A `Code` or a `Rebuild` migration runs without them: the runner drops them
//   before the migration and creates them again after it, in the same transaction. A data
//   migration can then change the rows of an archived entity, and a table rebuild keeps the
//   guards of its table.
// - A `Sql` migration runs with the guards. A `Sql` migration that writes rows of an entity
//   must be a `Code` migration, or it fails on a ledger with an archived entity.
// - A migration that adds a table with rows of an entity adds its guards too.
// - The runner recreates only the `archive_guard_*` triggers. It does not recreate a guard whose
//   table the migration dropped and did not create again. A `Rebuild` of a table also drops the
//   other triggers of that table, for example `account_counterparty_*` of migration 30 on
//   `accounts`: the rebuild script must create them again itself.
// - Never change a shipped migration file. A test pins the hash of each file. Repair an old
//   migration with a new one, as migration 31 repairs 28 and 29.
const MIGRATIONS: &[(i64, Migration)] = &[
    (1, Migration::Sql(include_str!("../migrations/0001_init.sql"))),
    (2, Migration::Sql(include_str!("../migrations/0002_indexes.sql"))),
    (3, Migration::Sql(include_str!("../migrations/0003_import_profiles.sql"))),
    (4, Migration::Sql(include_str!("../migrations/0004_entry_tags.sql"))),
    (5, Migration::Sql(include_str!("../migrations/0005_account_class.sql"))),
    (6, Migration::Sql(include_str!("../migrations/0006_reviewed.sql"))),
    (7, Migration::Code(crate::minor_units::rescale)),
    (8, Migration::Sql(include_str!("../migrations/0008_channel_connections.sql"))),
    (9, Migration::Sql(include_str!("../migrations/0009_enable_banking_sessions.sql"))),
    (10, Migration::Sql(include_str!("../migrations/0010_enable_banking_aliases.sql"))),
    (11, Migration::Sql(include_str!("../migrations/0011_refunds.sql"))),
    (12, Migration::Code(crate::tags::repair_reversal_tags)),
    (13, Migration::Code(crate::tags::repair_reversal_chains)),
    (14, Migration::Sql(include_str!("../migrations/0014_budgets.sql"))),
    (15, Migration::Sql(include_str!("../migrations/0015_connections_ui.sql"))),
    (16, Migration::Sql(include_str!("../migrations/0016_payees.sql"))),
    (18, Migration::Sql(include_str!("../migrations/0018_entity_facts.sql"))),
    (19, Migration::Sql(include_str!("../migrations/0019_party_facts.sql"))),
    (20, Migration::Sql(include_str!("../migrations/0020_documents.sql"))),
    (21, Migration::Sql(include_str!("../migrations/0021_statutory_codes.sql"))),
    (22, Migration::Sql(include_str!("../migrations/0022_tax_events.sql"))),
    (24, Migration::Sql(include_str!("../migrations/0024_close_and_filings.sql"))),
    (25, Migration::Sql(include_str!("../migrations/0025_intervault_links.sql"))),
    (26, Migration::Sql(include_str!("../migrations/0026_invoices.sql"))),
    (27, Migration::Sql(include_str!("../migrations/0027_entity_flags.sql"))),
    (28, Migration::Rebuild { sql: include_str!("../migrations/0028_entity_active_names.sql"), check: no_check }),
    (29, Migration::Sql(include_str!("../migrations/0029_archive_guards.sql"))),
    (30, Migration::Sql(include_str!("../migrations/0030_account_counterparty.sql"))),
    (31, Migration::Code(crate::entities::repair_names_and_guards)),
];

/// Schema of version-1 signed snapshots. Keep this stable when owned-book migrations advance.
/// Snapshot records hold the columns of this schema. Newer columns take their defaults on install.
pub const REPLICA_SCHEMA_VERSION: i64 = 16;

pub struct Db {
    conn: Mutex<Connection>,
    path: PathBuf,
    replica: bool,
}

impl Db {
    /// Open (or create) the ledger file and apply pending migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Db> {
        Db::open_with(path, MIGRATIONS)
    }

    /// `open` with a list of migrations. Tests give a list with a later migration.
    fn open_with(path: impl AsRef<Path>, migrations: &[(i64, Migration)]) -> Result<Db> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).map_err(|e| anyhow::anyhow!("create {}: {e}", dir.display()))?;
            }
        }
        let conn = Connection::open(&path)?;
        if replica_marker(&conn)? {
            return Err(crate::Error::Locked("shared vault is read-only; open it through sharing".into()));
        }
        configure(&conn)?;
        let target = migrations.last().map(|(v, _)| *v).unwrap_or(0).max(schema_version());
        migrate_with(&conn, migrations, target)?;
        Ok(Db { conn: Mutex::new(conn), path, replica: false })
    }

    /// An in-memory ledger, for tests.
    pub fn open_memory() -> Result<Db> {
        let conn = Connection::open_in_memory()?;
        configure(&conn)?;
        migrate(&conn)?;
        Ok(Db { conn: Mutex::new(conn), path: PathBuf::from(":memory:"), replica: false })
    }

    /// A new, empty staging file for a shared vault snapshot. It migrates to today's schema.
    /// The version-1 wire format fills the tables of `REPLICA_SCHEMA_VERSION`; later columns keep
    /// their defaults, so reads through today's API work on a replica.
    pub fn open_replica_staging(path: impl AsRef<Path>) -> Result<Db> {
        let path = path.as_ref().to_path_buf();
        let conn = Connection::open(&path)?;
        configure(&conn)?;
        migrate_to(&conn, schema_version())?;
        Ok(Db { conn: Mutex::new(conn), path, replica: false })
    }

    /// A trusted snapshot path selected by the sharing trust store. Never migrates.
    pub fn open_replica(path: impl AsRef<Path>) -> Result<Db> {
        let path = path.as_ref().to_path_buf();
        let conn = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        if !replica_marker(&conn)? {
            return Err(crate::Error::Invalid("not a shared vault snapshot".into()));
        }
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        // A replica installs into a freshly migrated book, and reads use the columns of today's schema.
        // A replica that an older build installed has an older schema and needs a new snapshot.
        if version != schema_version() {
            return Err(crate::Error::Invalid("unsupported replica schema; obtain a new snapshot".into()));
        }
        conn.execute_batch("PRAGMA query_only=ON; PRAGMA foreign_keys=ON;")?;
        Ok(Db { conn: Mutex::new(conn), path, replica: true })
    }
    pub fn is_replica(&self) -> bool {
        self.replica
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Exclusive access to the connection. Hold it for the duration of one operation.
    pub fn conn(&self) -> MutexGuard<'_, Connection> {
        match self.conn.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

fn configure(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;
         PRAGMA temp_store = MEMORY;",
    )?;
    Ok(())
}

/// The schema version that `Db::open` migrates an owned ledger to.
pub fn schema_version() -> i64 {
    MIGRATIONS.last().map(|(v, _)| *v).unwrap_or(0)
}

fn migrate(conn: &Connection) -> Result<()> {
    migrate_to(conn, schema_version())
}

/// Apply pending migrations up to and including `target`. A migration that fails is rolled back
/// whole, and `PRAGMA user_version` stays at the version before it.
pub fn migrate_to(conn: &Connection, target: i64) -> Result<()> {
    migrate_with(conn, MIGRATIONS, target)
}

fn migrate_with(conn: &Connection, migrations: &[(i64, Migration)], target: i64) -> Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (version, migration) in migrations {
        if *version > current && *version <= target {
            if matches!(migration, Migration::Rebuild { .. }) {
                // PRAGMA foreign_keys has no effect inside a transaction, so set it before BEGIN.
                // The guard turns foreign keys on again on every path out of this block.
                let _keys = ForeignKeysOff::new(conn)?;
                apply_one(conn, *version, migration)?;
            } else {
                apply_one(conn, *version, migration)?;
            }
        }
    }
    Ok(())
}

/// Foreign keys off while the value lives. The drop turns them on again, also after an error.
struct ForeignKeysOff<'a>(&'a Connection);

impl<'a> ForeignKeysOff<'a> {
    fn new(conn: &'a Connection) -> Result<ForeignKeysOff<'a>> {
        conn.execute_batch("PRAGMA foreign_keys = OFF")?;
        Ok(ForeignKeysOff(conn))
    }
}

impl Drop for ForeignKeysOff<'_> {
    fn drop(&mut self) {
        // A transaction that is still open ignores the pragma, so end it first.
        if !self.0.is_autocommit() {
            let _ = self.0.execute_batch("ROLLBACK");
        }
        if let Err(e) = self.0.execute_batch("PRAGMA foreign_keys = ON") {
            tracing::error!("cannot turn foreign keys on again after a migration: {e}");
        }
    }
}

/// Apply one migration in its own transaction. Any error rolls the transaction back.
fn apply_one(conn: &Connection, version: i64, migration: &Migration) -> Result<()> {
    // Another process can open the same file at the same time. Take the write lock first,
    // then read the version again, so each migration runs once.
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let outcome = (|| -> Result<bool> {
        let now: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if now >= version {
            return Ok(false);
        }
        match migration {
            Migration::Sql(sql) => conn.execute_batch(sql)?,
            Migration::Code(f) => without_archive_guards(conn, || f(conn))?,
            Migration::Rebuild { sql, check } => {
                check(conn)?;
                let before = foreign_key_violations(conn)?;
                without_archive_guards(conn, || Ok(conn.execute_batch(sql)?))?;
                foreign_keys_hold(conn, before)?;
            }
        }
        conn.execute_batch(&format!("PRAGMA user_version = {version}"))?;
        Ok(true)
    })()
    .and_then(|applied| {
        conn.execute_batch("COMMIT")?;
        Ok(applied)
    });
    match outcome {
        Ok(applied) => {
            if applied {
                tracing::info!(version, "applied migration");
            }
            Ok(())
        }
        Err(e) => {
            if !conn.is_autocommit() {
                let _ = conn.execute_batch("ROLLBACK");
            }
            Err(e)
        }
    }
}

/// Run `f` without the archive guard triggers of migration 29, inside the caller's
/// transaction. The triggers come back after `f`, also when `f` dropped their table and created
/// it again. A guard whose table no longer exists stays dropped.
fn without_archive_guards(conn: &Connection, f: impl FnOnce() -> Result<()>) -> Result<()> {
    let guards: Vec<(String, String, String)> = {
        let mut stmt = conn.prepare("SELECT name, tbl_name, sql FROM sqlite_master WHERE type = 'trigger' AND name LIKE 'archive_guard_%'")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    for (name, _, _) in &guards {
        conn.execute_batch(&format!("DROP TRIGGER IF EXISTS \"{name}\""))?;
    }
    f()?;
    for (name, table, sql) in &guards {
        let table_exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)", [table], |r| r.get(0))?;
        let trigger_exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'trigger' AND name = ?1)", [name], |r| r.get(0))?;
        if table_exists && !trigger_exists {
            conn.execute_batch(sql)?;
        }
    }
    Ok(())
}

/// The number of foreign keys without a parent row.
/// One foreign key without a parent row: the table, the row id, the parent table and the index
/// of the foreign key in the table (`fkid`). Two foreign keys of one row to the same parent
/// table differ in `fkid`.
type Orphan = (String, Option<i64>, String, i64);

/// The foreign keys without a parent row.
fn foreign_key_violations(conn: &Connection) -> Result<std::collections::BTreeSet<Orphan>> {
    let mut stmt = conn.prepare("SELECT \"table\", rowid, parent, fkid FROM pragma_foreign_key_check")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<std::result::Result<_, _>>()?;
    Ok(rows)
}

/// Refuse a rebuild that leaves a row without a parent that was not there before it. A ledger
/// can have such rows from before the migration; the rebuild does not cause them. The runner
/// compares the rows, not their number, so a rebuild that repairs one row and breaks another
/// fails too.
fn foreign_keys_hold(conn: &Connection, before: std::collections::BTreeSet<Orphan>) -> Result<()> {
    let after = foreign_key_violations(conn)?;
    let new: Vec<String> =
        after.difference(&before).map(|(table, rowid, parent, fkid)| format!("{table} row {} -> {parent} (foreign key {fkid})", rowid.map(|r| r.to_string()).unwrap_or_default())).collect();
    if !new.is_empty() {
        return Err(crate::Error::Invalid(format!(
            "the table rebuild left {} new foreign key(s) without a parent row ({} existed before the rebuild, {} after it): {}",
            new.len(),
            before.len(),
            after.len(),
            new.join(", ")
        )));
    }
    if !before.is_empty() {
        tracing::warn!(count = before.len(), "the ledger has foreign keys without a parent row from before the migration");
    }
    Ok(())
}

/// A rebuild without a check.
fn no_check(_: &Connection) -> Result<()> {
    Ok(())
}

/// Read a setting.
pub fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    use rusqlite::OptionalExtension;
    Ok(conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional()?)
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute("INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", [key, value])?;
    Ok(())
}

pub fn replica_marker(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='means_replica_marker')", [], |r| r.get(0))?)
}

/// Alias of `schema_version`.
pub fn latest_version() -> i64 {
    schema_version()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("means-db-{name}-{}", crate::new_uid()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("ledger.db")
    }

    fn archived_entity(path: &Path) -> i64 {
        let db = Db::open(path).unwrap();
        let mut c = db.conn();
        let e = crate::entities::create_entity(&mut c, "Example Archive Ltd", "company", "GB", "GBP").unwrap();
        crate::entities::archive(&mut c, e.id, "Test User", "test").unwrap();
        e.id
    }

    fn touch_archived(conn: &Connection) -> Result<()> {
        conn.execute("UPDATE entities SET updated_at = 'migrated' WHERE archived_at IS NOT NULL", [])?;
        conn.execute("UPDATE accounts SET notes = 'migrated' WHERE entity_id IN (SELECT id FROM entities WHERE archived_at IS NOT NULL)", [])?;
        Ok(())
    }

    fn guard_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND name LIKE 'archive_guard_%'", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn a_later_data_migration_can_touch_an_archived_entity() {
        let path = temp_path("data-migration");
        let id = archived_entity(&path);
        let guards = guard_count(&Connection::open(&path).unwrap());
        assert!(guards > 0);
        let next = schema_version() + 1;
        let db = Db::open_with(&path, &[(next, Migration::Code(touch_archived))]).unwrap();
        let c = db.conn();
        let version: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, next);
        let notes: String = c.query_row("SELECT updated_at FROM entities WHERE id = ?1", [id], |r| r.get(0)).unwrap();
        assert_eq!(notes, "migrated");
        // The guards are back after the migration, and they refuse writes again.
        assert_eq!(guard_count(&c), guards);
        assert!(touch_archived(&c).is_err());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_rebuild_that_moves_an_orphan_to_another_foreign_key_of_the_row_fails() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        // One row with two foreign keys to the same parent table. Before the rebuild, the first
        // key has no parent row and the second key has one.
        conn.execute_batch(
            "CREATE TABLE fk_parent (id INTEGER PRIMARY KEY);
             CREATE TABLE fk_child (id INTEGER PRIMARY KEY, a INTEGER REFERENCES fk_parent(id), b INTEGER REFERENCES fk_parent(id));
             INSERT INTO fk_parent (id) VALUES (1);
             PRAGMA foreign_keys = OFF;
             INSERT INTO fk_child (id, a, b) VALUES (1, 99, 1);
             PRAGMA foreign_keys = ON;",
        )
        .unwrap();
        // The rebuild repairs the first key and breaks the second: one orphan before and after,
        // on the same row and parent table, but on another foreign key.
        let sql = "UPDATE fk_child SET a = 1, b = 99 WHERE id = 1";
        let err = migrate_with(&conn, &[(1, Migration::Rebuild { sql, check: no_check })], 1).unwrap_err().to_string();
        assert!(err.contains("1 new foreign key") && err.contains("fk_child row 1 -> fk_parent"), "{err}");
        let b: i64 = conn.query_row("SELECT b FROM fk_child WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(b, 1, "the failed rebuild rolls back");
    }

    #[test]
    fn a_rebuild_fails_only_on_new_orphans_and_turns_foreign_keys_on_again() {
        let mut conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        migrate(&conn).unwrap();
        let parent = crate::entities::create_entity(&mut conn, "Example Parent", "company", "GB", "GBP").unwrap();
        let commodity: i64 = conn.query_row("SELECT id FROM commodities WHERE code = 'GBP'", [], |r| r.get(0)).unwrap();
        // An orphan from before the migration: an account of an entity that does not exist.
        conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
        conn.execute("INSERT INTO accounts (uid, entity_id, name, type, commodity_id, created_at, updated_at) VALUES ('orphan', 99, 'Lost', 'asset', ?1, 'now', 'now')", [commodity]).unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        let v = schema_version();
        // A rebuild that adds no orphan passes, although the old orphan is there.
        migrate_with(&conn, &[(v + 1, Migration::Rebuild { sql: "CREATE TABLE scratch_rebuild (id INTEGER PRIMARY KEY)", check: no_check })], v + 1).unwrap();
        // A rebuild that adds an orphan fails, and the message counts only the new one.
        conn.execute("INSERT INTO accounts (uid, entity_id, name, type, commodity_id, created_at, updated_at) VALUES ('child', ?1, 'Child', 'asset', ?2, 'now', 'now')", [parent.id, commodity])
            .unwrap();
        let err = migrate_with(
            &conn,
            &[(
                v + 2,
                Migration::Rebuild {
                    sql: "DELETE FROM accounts WHERE entity_id = (SELECT id FROM entities WHERE name = 'Example Parent') AND uid <> 'child'; DELETE FROM entities WHERE name = 'Example Parent'",
                    check: no_check,
                },
            )],
            v + 2,
        )
        .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("1 new foreign key") && message.contains("1 existed before"), "{message}");
        // The failure rolls back, ends the transaction and turns foreign keys on again.
        assert!(conn.is_autocommit());
        let on: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap();
        assert_eq!(on, 1);
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, v + 1);
        // A script error is a failure path too.
        assert!(migrate_with(&conn, &[(v + 2, Migration::Rebuild { sql: "NOT SQL", check: no_check })], v + 2).is_err());
        let on: i64 = conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap();
        assert_eq!(on, 1);
        assert!(conn.is_autocommit());
    }
}
