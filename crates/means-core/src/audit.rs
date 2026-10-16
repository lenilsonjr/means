//! Change history: who changed what, before and after, and why.

use rusqlite::{params, Connection};

use crate::{now_ts, Result};

/// Record an event without an actor or a reason.
/// Code migrations before version 24 call this function, so it names only the columns of
/// version 1. The actor and the reason take their empty defaults.
pub fn log(conn: &Connection, table: &str, row_id: i64, action: &str, before: Option<serde_json::Value>, after: Option<serde_json::Value>) -> Result<()> {
    conn.execute(
        "INSERT INTO audit_log (at, table_name, row_id, action, before, after) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![now_ts(), table, row_id, action, before.map(|v| v.to_string()), after.map(|v| v.to_string())],
    )?;
    Ok(())
}

/// Record an event with the person who acted and the reason. Returns the id of the event.
#[allow(clippy::too_many_arguments)]
pub fn log_by(conn: &Connection, table: &str, row_id: i64, action: &str, before: Option<serde_json::Value>, after: Option<serde_json::Value>, actor: &str, reason: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO audit_log (at, table_name, row_id, action, before, after, actor, reason) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![now_ts(), table, row_id, action, before.map(|v| v.to_string()), after.map(|v| v.to_string()), actor.trim(), reason.trim()],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Record an event with the actor and the reason when the ledger has these columns. A ledger
/// before migration 24 has no such columns: a code migration of such a ledger gets the event of
/// version 1, without the actor and the reason.
#[allow(clippy::too_many_arguments)]
pub fn log_with(conn: &Connection, table: &str, row_id: i64, action: &str, before: Option<serde_json::Value>, after: Option<serde_json::Value>, actor: &str, reason: &str) -> Result<()> {
    if has_actor(conn)? {
        log_by(conn, table, row_id, action, before, after, actor, reason)?;
    } else {
        log(conn, table, row_id, action, before, after)?;
    }
    Ok(())
}

fn has_actor(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('audit_log') WHERE name = 'actor')", [], |r| r.get(0))?)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditRow {
    pub id: i64,
    pub at: String,
    pub table_name: String,
    pub row_id: i64,
    pub action: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub actor: String,
    pub reason: String,
}

/// The columns of an event. A ledger before migration 24 has no actor or reason column; code
/// migrations read the history of such a ledger, so the reader gives empty values there.
fn columns(conn: &Connection) -> Result<&'static str> {
    Ok(if has_actor(conn)? { "id, at, table_name, row_id, action, before, after, actor, reason" } else { "id, at, table_name, row_id, action, before, after, '', ''" })
}

fn row_to_audit(r: &rusqlite::Row<'_>) -> rusqlite::Result<AuditRow> {
    Ok(AuditRow { id: r.get(0)?, at: r.get(1)?, table_name: r.get(2)?, row_id: r.get(3)?, action: r.get(4)?, before: r.get(5)?, after: r.get(6)?, actor: r.get(7)?, reason: r.get(8)? })
}

pub fn history(conn: &Connection, table: &str, row_id: i64) -> Result<Vec<AuditRow>> {
    let mut stmt = conn.prepare(&format!("SELECT {} FROM audit_log WHERE table_name = ?1 AND row_id = ?2 ORDER BY id", columns(conn)?))?;
    let rows = stmt.query_map(params![table, row_id], row_to_audit)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Read one event by its id.
pub fn get(conn: &Connection, id: i64) -> Result<AuditRow> {
    use rusqlite::OptionalExtension;
    conn.query_row(&format!("SELECT {} FROM audit_log WHERE id = ?1", columns(conn)?), [id], row_to_audit).optional()?.ok_or_else(|| crate::Error::NotFound(format!("audit event {id}")))
}
