//! Change history: who changed what, before and after.

use rusqlite::{params, Connection};

use crate::{now_ts, Result};

pub fn log(conn: &Connection, table: &str, row_id: i64, action: &str, before: Option<serde_json::Value>, after: Option<serde_json::Value>) -> Result<()> {
    conn.execute(
        "INSERT INTO audit_log (at, table_name, row_id, action, before, after) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![now_ts(), table, row_id, action, before.map(|v| v.to_string()), after.map(|v| v.to_string())],
    )?;
    Ok(())
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
}

pub fn history(conn: &Connection, table: &str, row_id: i64) -> Result<Vec<AuditRow>> {
    let mut stmt = conn.prepare("SELECT id, at, table_name, row_id, action, before, after FROM audit_log WHERE table_name = ?1 AND row_id = ?2 ORDER BY id")?;
    let rows =
        stmt.query_map(params![table, row_id], |r| Ok(AuditRow { id: r.get(0)?, at: r.get(1)?, table_name: r.get(2)?, row_id: r.get(3)?, action: r.get(4)?, before: r.get(5)?, after: r.get(6)? }))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}
