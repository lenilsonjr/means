//! The filings register: returns and reports that an entity prepares for an authority.
//!
//! A filing records its period, its status, the digests of the snapshot and the artifact, and
//! the head of the entity hash chain when the filing was recorded. A filing in status
//! `submitted` or `accepted` freezes its period. `close::unlock` refuses to reopen a frozen
//! period unless the caller amends the filing. An amendment is a new `prepared` row that points
//! to the filing that it amends. The audit log records every change with its actor and reason.

use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::entities::date_at;
use crate::proposal::{AgentError, AgentResult};
use crate::{audit, entities, new_uid, now_ts};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FilingStatus {
    Prepared,
    Submitted,
    Accepted,
    Rejected,
    Amended,
}

impl FilingStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            FilingStatus::Prepared => "prepared",
            FilingStatus::Submitted => "submitted",
            FilingStatus::Accepted => "accepted",
            FilingStatus::Rejected => "rejected",
            FilingStatus::Amended => "amended",
        }
    }

    pub fn parse(s: &str) -> AgentResult<FilingStatus> {
        match s.trim().to_ascii_lowercase().as_str() {
            "prepared" => Ok(FilingStatus::Prepared),
            "submitted" => Ok(FilingStatus::Submitted),
            "accepted" => Ok(FilingStatus::Accepted),
            "rejected" => Ok(FilingStatus::Rejected),
            "amended" => Ok(FilingStatus::Amended),
            other => Err(AgentError::new("invalid_input", format!("unknown filing status {other:?}; use prepared, submitted, accepted, rejected or amended"))),
        }
    }

    /// True when the status freezes the period of the filing.
    pub fn freezes(self) -> bool {
        matches!(self, FilingStatus::Submitted | FilingStatus::Accepted)
    }

    /// The statuses that can follow this one.
    fn next(self) -> &'static [FilingStatus] {
        use FilingStatus::*;
        match self {
            Prepared => &[Submitted, Rejected],
            Submitted => &[Accepted, Rejected, Amended],
            Accepted => &[Amended],
            Rejected => &[Prepared, Submitted],
            Amended => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Filing {
    pub id: i64,
    pub uid: String,
    pub entity_id: i64,
    pub entity_uid: String,
    pub jurisdiction: String,
    pub filing_code: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub status: FilingStatus,
    pub snapshot_digest: String,
    pub chain_head_seq: Option<i64>,
    pub chain_head_hash: Option<String>,
    pub artifact_digest: String,
    pub receipt_ref: String,
    pub submitted_at: Option<String>,
    pub notes: String,
    pub amends_id: Option<i64>,
    pub amends_uid: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// The input of `record_filing`.
#[derive(Debug, Clone)]
pub struct NewFiling {
    pub entity_id: i64,
    pub jurisdiction: String,
    pub filing_code: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub status: FilingStatus,
    pub snapshot_digest: String,
    pub artifact_digest: String,
    pub notes: String,
}

/// The input of `update_filing_status`.
#[derive(Debug, Clone)]
pub struct StatusChange {
    pub status: FilingStatus,
    pub receipt_ref: Option<String>,
    /// The submission date or time. `submitted` without a value uses now.
    pub submitted_at: Option<String>,
    pub actor: String,
    pub reason: String,
}

const COLUMNS: &str = "f.id, f.uid, f.entity_id, e.uid, f.jurisdiction, f.filing_code, f.period_start, f.period_end, f.status, f.snapshot_digest,
    f.chain_head_seq, f.chain_head_hash, f.artifact_digest, f.receipt_ref, f.submitted_at, f.notes, f.amends_id, a.uid, f.created_at, f.updated_at";
const FROM: &str = "FROM filings f JOIN entities e ON e.id = f.entity_id LEFT JOIN filings a ON a.id = f.amends_id";

fn row_to_filing(r: &rusqlite::Row<'_>) -> rusqlite::Result<Filing> {
    let status: String = r.get(8)?;
    Ok(Filing {
        id: r.get(0)?,
        uid: r.get(1)?,
        entity_id: r.get(2)?,
        entity_uid: r.get(3)?,
        jurisdiction: r.get(4)?,
        filing_code: r.get(5)?,
        period_start: date_at(r, 6)?,
        period_end: date_at(r, 7)?,
        status: FilingStatus::parse(&status).map_err(|e| rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(e)))?,
        snapshot_digest: r.get(9)?,
        chain_head_seq: r.get(10)?,
        chain_head_hash: r.get(11)?,
        artifact_digest: r.get(12)?,
        receipt_ref: r.get(13)?,
        submitted_at: r.get(14)?,
        notes: r.get(15)?,
        amends_id: r.get(16)?,
        amends_uid: r.get(17)?,
        created_at: r.get(18)?,
        updated_at: r.get(19)?,
    })
}

pub fn get_filing(conn: &Connection, id: i64) -> AgentResult<Filing> {
    conn.query_row(&format!("SELECT {COLUMNS} {FROM} WHERE f.id = ?1"), [id], row_to_filing).optional()?.ok_or_else(|| AgentError::new("not_found", format!("no filing {id}")))
}

pub fn get_filing_by_uid(conn: &Connection, uid: &str) -> AgentResult<Filing> {
    conn.query_row(&format!("SELECT {COLUMNS} {FROM} WHERE f.uid = ?1"), [uid.trim()], row_to_filing)
        .optional()?
        .ok_or_else(|| AgentError::new("not_found", format!("no filing with uid {}", uid.trim())))
}

/// The filings of an entity, optionally for one jurisdiction and one code. Oldest period first.
pub fn list_filings(conn: &Connection, entity_id: i64, jurisdiction: Option<&str>, code: Option<&str>) -> AgentResult<Vec<Filing>> {
    entities::get_entity(conn, entity_id)?;
    let jurisdiction = jurisdiction.map(|j| j.trim().to_ascii_uppercase());
    let code = code.map(|c| c.trim().to_string());
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} {FROM} WHERE f.entity_id = ?1 AND (?2 IS NULL OR f.jurisdiction = ?2) AND (?3 IS NULL OR f.filing_code = ?3)
         ORDER BY f.period_start, f.jurisdiction, f.filing_code, f.id"
    ))?;
    let rows = stmt.query_map(params![entity_id, jurisdiction, code], row_to_filing)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// The frozen filings of an entity whose period shares at least one day with `[from, to]`.
pub fn frozen_overlapping(conn: &Connection, entity_id: i64, from: NaiveDate, to: NaiveDate) -> AgentResult<Vec<Filing>> {
    Ok(list_filings(conn, entity_id, None, None)?.into_iter().filter(|f| f.status.freezes() && f.period_start <= to && from <= f.period_end).collect())
}

fn check_digest(field: &str, value: &str) -> AgentResult<String> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty() || (value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit())) {
        return Ok(value);
    }
    Err(AgentError::new("invalid_input", format!("{field} must be a SHA-256 digest of 64 hex digits")))
}

fn check_jurisdiction(value: &str) -> AgentResult<String> {
    let j = value.trim().to_ascii_uppercase();
    if j.is_empty() || !j.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(AgentError::new("invalid_input", format!("jurisdiction must be a code such as US or SG, got {j:?}")));
    }
    Ok(j)
}

/// The latest chained entry of an entity: (seq, hash).
fn chain_head(conn: &Connection, entity_id: i64) -> AgentResult<(Option<i64>, Option<String>)> {
    let head = conn.query_row("SELECT seq, hash FROM journal_entries WHERE entity_id = ?1 AND seq IS NOT NULL ORDER BY seq DESC LIMIT 1", [entity_id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
    Ok(head.unwrap_or((None, None)))
}

/// Record a filing and audit it. The row keeps the head of the entity hash chain at this time.
pub fn record_filing(conn: &mut Connection, input: NewFiling, actor: &str, reason: &str) -> AgentResult<Filing> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let filing = record_in(&tx, &input, None, actor, reason)?;
    tx.commit()?;
    Ok(filing)
}

fn record_in(conn: &Connection, input: &NewFiling, amends: Option<&Filing>, actor: &str, reason: &str) -> AgentResult<Filing> {
    entities::get_entity(conn, input.entity_id)?;
    let jurisdiction = check_jurisdiction(&input.jurisdiction)?;
    let code = input.filing_code.trim();
    if code.is_empty() {
        return Err(AgentError::new("invalid_input", "a filing code is required"));
    }
    if input.period_end < input.period_start {
        return Err(AgentError::new("invalid_input", format!("the period must not end before it starts: {} to {}", input.period_start, input.period_end)));
    }
    if input.status == FilingStatus::Amended {
        return Err(AgentError::new("invalid_input", "a new filing cannot start as amended"));
    }
    let snapshot = check_digest("snapshot digest", &input.snapshot_digest)?;
    let artifact = check_digest("artifact digest", &input.artifact_digest)?;
    let (seq, hash) = chain_head(conn, input.entity_id)?;
    let ts = now_ts();
    let submitted_at = input.status.freezes().then(|| ts.clone());
    conn.execute(
        "INSERT INTO filings (uid, entity_id, jurisdiction, filing_code, period_start, period_end, status, snapshot_digest, chain_head_seq, chain_head_hash,
                              artifact_digest, submitted_at, notes, amends_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?15)",
        params![
            new_uid(),
            input.entity_id,
            jurisdiction,
            code,
            input.period_start.to_string(),
            input.period_end.to_string(),
            input.status.as_str(),
            snapshot,
            seq,
            hash,
            artifact,
            submitted_at,
            input.notes.trim(),
            amends.map(|a| a.id),
            ts
        ],
    )?;
    let filing = get_filing(conn, conn.last_insert_rowid())?;
    let action = if amends.is_some() { "amend" } else { "record" };
    audit::log_by(conn, "filings", filing.id, action, None, Some(serde_json::to_value(&filing).map_err(crate::Error::from)?), actor, reason)?;
    Ok(filing)
}

/// Change the status of a filing and audit it. `submitted` needs a receipt reference.
pub fn update_filing_status(conn: &mut Connection, filing_id: i64, change: StatusChange) -> AgentResult<Filing> {
    let actor = change.actor.trim();
    if actor.is_empty() {
        return Err(AgentError::new("invalid_input", "give the name of the person who changes the filing"));
    }
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let before = get_filing(&tx, filing_id)?;
    if !before.status.next().contains(&change.status) {
        return Err(AgentError::new("invalid_transition", format!("a filing in status {} cannot become {}", before.status.as_str(), change.status.as_str())));
    }
    let receipt = change.receipt_ref.map(|r| r.trim().to_string()).unwrap_or_else(|| before.receipt_ref.clone());
    if change.status == FilingStatus::Submitted && receipt.is_empty() {
        return Err(AgentError::new("invalid_input", "a submitted filing needs a receipt reference"));
    }
    let submitted_at = match (change.status, change.submitted_at.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())) {
        (_, Some(at)) => {
            let day = at.get(..10).unwrap_or(&at);
            crate::parse_date(day).map_err(|_| AgentError::new("invalid_input", format!("the submission time must start with YYYY-MM-DD, got {at:?}")))?;
            Some(at)
        }
        (FilingStatus::Submitted, None) => Some(now_ts()),
        (_, None) => before.submitted_at.clone(),
    };
    tx.execute("UPDATE filings SET status = ?2, receipt_ref = ?3, submitted_at = ?4, updated_at = ?5 WHERE id = ?1", params![filing_id, change.status.as_str(), receipt, submitted_at, now_ts()])?;
    let after = get_filing(&tx, filing_id)?;
    audit::log_by(
        &tx,
        "filings",
        filing_id,
        "status",
        Some(serde_json::to_value(&before).map_err(crate::Error::from)?),
        Some(serde_json::to_value(&after).map_err(crate::Error::from)?),
        actor,
        &change.reason,
    )?;
    tx.commit()?;
    Ok(after)
}

/// Create a `prepared` amendment of a frozen filing inside the caller's transaction.
/// An open amendment in status `prepared` already present is returned instead of a new one.
pub(crate) fn amend_in(conn: &Connection, original: &Filing, actor: &str, reason: &str) -> AgentResult<Filing> {
    let open: Option<i64> = conn.query_row("SELECT id FROM filings WHERE amends_id = ?1 AND status = 'prepared' ORDER BY id LIMIT 1", [original.id], |r| r.get(0)).optional()?;
    if let Some(id) = open {
        return get_filing(conn, id);
    }
    let input = NewFiling {
        entity_id: original.entity_id,
        jurisdiction: original.jurisdiction.clone(),
        filing_code: original.filing_code.clone(),
        period_start: original.period_start,
        period_end: original.period_end,
        status: FilingStatus::Prepared,
        snapshot_digest: String::new(),
        artifact_digest: String::new(),
        notes: format!("amends {}", original.uid),
    };
    record_in(conn, &input, Some(original), actor, reason)
}
