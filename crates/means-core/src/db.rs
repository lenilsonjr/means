//! SQLite connection, pragmas and migrations.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use rusqlite::Connection;

use crate::Result;

/// A migration is a script, or code when a row needs a decision that SQL cannot make.
enum Migration {
    Sql(&'static str),
    Code(fn(&Connection) -> Result<()>),
}

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
];

/// Schema of version-1 signed snapshots. Keep this stable when owned-book migrations advance.
pub const REPLICA_SCHEMA_VERSION: i64 = 16;

pub struct Db {
    conn: Mutex<Connection>,
    path: PathBuf,
    replica: bool,
}

impl Db {
    /// Open (or create) the ledger file and apply pending migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Db> {
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
        migrate(&conn)?;
        Ok(Db { conn: Mutex::new(conn), path, replica: false })
    }

    /// An in-memory ledger, for tests.
    pub fn open_memory() -> Result<Db> {
        let conn = Connection::open_in_memory()?;
        configure(&conn)?;
        migrate(&conn)?;
        Ok(Db { conn: Mutex::new(conn), path: PathBuf::from(":memory:"), replica: false })
    }

    /// A trusted snapshot path selected by the sharing trust store. Never migrates.
    pub fn open_replica(path: impl AsRef<Path>) -> Result<Db> {
        let path = path.as_ref().to_path_buf();
        let conn = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        if !replica_marker(&conn)? {
            return Err(crate::Error::Invalid("not a shared vault snapshot".into()));
        }
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version != REPLICA_SCHEMA_VERSION {
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

fn migrate(conn: &Connection) -> Result<()> {
    migrate_to(conn, MIGRATIONS.last().map(|(v, _)| *v).unwrap_or(0))
}

/// Apply pending migrations up to and including `target`. A migration that fails is rolled back
/// whole, and `PRAGMA user_version` stays at the version before it.
pub fn migrate_to(conn: &Connection, target: i64) -> Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (version, migration) in MIGRATIONS {
        if *version > current && *version <= target {
            conn.execute_batch("BEGIN")?;
            let applied = match migration {
                Migration::Sql(sql) => conn.execute_batch(sql).map_err(Into::into),
                Migration::Code(f) => f(conn),
            };
            if let Err(e) = applied {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(e);
            }
            conn.execute_batch(&format!("PRAGMA user_version = {version}"))?;
            conn.execute_batch("COMMIT")?;
            tracing::info!(version, "applied migration");
        }
    }
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
