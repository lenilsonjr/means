//! Period close: checks, the lock and the unlock.
//!
//! `close_check` reads the books of one entity for one period and lists the findings that block
//! a final close (errors) or need attention (warnings). It writes nothing.
//! `close_lock` moves the lock date of the entity forward. It refuses when the check finds an
//! error, unless the caller forces it. `unlock` moves the lock date back. Both record an audit
//! event with the actor and the reason. `entities::update_entity` refuses to move the lock back.
//!
//! A filing in status `submitted` or `accepted` freezes its period. `unlock` refuses to reopen
//! that period unless the caller amends the filing (see `filings`).

use std::collections::BTreeMap;

use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::filings::{self, Filing};
use crate::money::Money;
use crate::proposal::{AgentError, AgentResult};
use crate::{accounts, audit, entities, now_ts};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

/// One result of a close check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub code: String,
    pub severity: Severity,
    pub message: String,
    pub entry_uid: Option<String>,
    pub account_uid: Option<String>,
    pub count: Option<i64>,
    /// A balance in the functional currency, when the finding is about a balance.
    pub amount: Option<Money>,
}

impl Finding {
    fn new(code: &str, severity: Severity, message: String) -> Finding {
        Finding { code: code.into(), severity, message, entry_uid: None, account_uid: None, count: None, amount: None }
    }
}

/// Control totals of the period. Booked entries are `posted` and `void` entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CloseTotals {
    pub booked_entries: i64,
    pub draft_entries: i64,
    pub postings: i64,
    pub statement_lines: i64,
    pub debit: Money,
    pub credit: Money,
    pub balance: Money,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CloseReport {
    pub entity_uid: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub lock_date: Option<NaiveDate>,
    pub ok: bool,
    pub error_count: i64,
    pub warning_count: i64,
    pub findings: Vec<Finding>,
    pub totals: CloseTotals,
}

impl CloseReport {
    /// sha256 over the canonical JSON of the report. `serde_json::Value` keeps object keys
    /// sorted, so the bytes do not depend on field order.
    pub fn digest(&self) -> AgentResult<String> {
        let value = serde_json::to_value(self).map_err(crate::Error::from)?;
        let bytes = serde_json::to_vec(&value).map_err(crate::Error::from)?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }

    /// Finding counts by code.
    pub fn counts(&self) -> BTreeMap<String, i64> {
        let mut counts = BTreeMap::new();
        for f in &self.findings {
            *counts.entry(f.code.clone()).or_insert(0) += 1;
        }
        counts
    }
}

/// True when the account holds items that wait for a category: the system role `suspense`,
/// or a path segment named Uncategorized or Suspense.
fn is_suspense(a: &accounts::AccountBrief) -> bool {
    a.system_role == "suspense" || a.path.split(':').any(|s| s.eq_ignore_ascii_case("uncategorized") || s.eq_ignore_ascii_case("suspense"))
}

/// Check the books of an entity for the period `[period_start, period_end]`. Writes nothing.
pub fn close_check(conn: &Connection, entity_id: i64, period_start: NaiveDate, period_end: NaiveDate) -> AgentResult<CloseReport> {
    if period_end < period_start {
        return Err(AgentError::new("invalid_input", format!("the period must not end before it starts: {period_start} to {period_end}")));
    }
    let entity = entities::get_entity(conn, entity_id)?;
    let fprec = entities::functional_precision(conn, entity_id)?;
    let money = |minor: i64| Money::from_minor(minor, &entity.currency, fprec).map_err(AgentError::from);
    let (from, to) = (period_start.to_string(), period_end.to_string());
    let mut findings = Vec::new();

    // Drafts wait for a booking decision.
    let mut stmt = conn.prepare("SELECT uid, date FROM journal_entries WHERE entity_id = ?1 AND status = 'draft' AND date BETWEEN ?2 AND ?3 ORDER BY date, id")?;
    for row in stmt.query_map(params![entity_id, from, to], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (uid, date) = row?;
        let mut f = Finding::new("unresolved_draft", Severity::Error, format!("draft entry dated {date} waits for a booking decision"));
        f.entry_uid = Some(uid);
        findings.push(f);
    }

    // Posted entries that no human reviewed.
    let mut stmt = conn.prepare("SELECT uid, date FROM journal_entries WHERE entity_id = ?1 AND status = 'posted' AND reviewed_at IS NULL AND date BETWEEN ?2 AND ?3 ORDER BY date, id")?;
    for row in stmt.query_map(params![entity_id, from, to], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (uid, date) = row?;
        let mut f = Finding::new("unreviewed_entry", Severity::Error, format!("posted entry dated {date} has no review"));
        f.entry_uid = Some(uid);
        findings.push(f);
    }

    // A missing rate, or a foreign posting without a rate.
    let mut stmt = conn.prepare(
        "SELECT e.uid, e.date, COUNT(*) FROM postings p
           JOIN journal_entries e ON e.id = p.journal_entry_id
           JOIN accounts a ON a.id = p.account_id
           JOIN commodities c ON c.id = a.commodity_id
         WHERE e.entity_id = ?1 AND e.date BETWEEN ?2 AND ?3
           AND (p.rate_source = 'missing' OR (c.code <> ?4 AND p.quantity <> 0 AND (p.rate IS NULL OR trim(p.rate) = '')))
         GROUP BY e.id ORDER BY e.date, e.id",
    )?;
    for row in stmt.query_map(params![entity_id, from, to, entity.currency], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))? {
        let (uid, date, n) = row?;
        let mut f = Finding::new("missing_rate", Severity::Error, format!("entry dated {date} has {n} posting(s) without a known exchange rate"));
        f.entry_uid = Some(uid);
        f.count = Some(n);
        findings.push(f);
    }

    // Period balances of booked entries, by account.
    let mut stmt = conn.prepare(
        "SELECT a.id, a.uid, a.intercompany, COALESCE(SUM(p.amount), 0) FROM postings p
           JOIN journal_entries e ON e.id = p.journal_entry_id
           JOIN accounts a ON a.id = p.account_id
         WHERE e.entity_id = ?1 AND e.status IN ('posted','void') AND e.date BETWEEN ?2 AND ?3
         GROUP BY a.id ORDER BY a.id",
    )?;
    let balances = stmt
        .query_map(params![entity_id, from, to], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)? != 0, r.get::<_, i64>(3)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let index = accounts::path_index(conn)?;
    for (id, uid, intercompany, minor) in &balances {
        if *minor == 0 {
            continue;
        }
        let Some(account) = index.get(id) else { continue };
        let amount = money(*minor)?;
        let shown = crate::money::fmt(amount.major(), fprec);
        if is_suspense(account) {
            let mut f = Finding::new("suspense_balance", Severity::Error, format!("{} holds {shown} {} in the period", account.path, entity.currency));
            f.account_uid = Some(uid.clone());
            f.amount = Some(amount);
            findings.push(f);
        }
        if *intercompany {
            let mut f = Finding::new(
                "intercompany_out_of_balance",
                Severity::Warning,
                format!("intercompany account {} moved {shown} {} in the period; reconcile it with the other entity", account.path, entity.currency),
            );
            f.account_uid = Some(uid.clone());
            f.amount = Some(amount);
            findings.push(f);
        }
    }

    // Statement lines without a posting link. Skipped and duplicate lines are decisions.
    let mut stmt = conn.prepare(
        "SELECT a.uid, a.id, COUNT(*) FROM statement_lines s JOIN accounts a ON a.id = s.account_id
         WHERE a.entity_id = ?1 AND s.date BETWEEN ?2 AND ?3 AND s.posting_id IS NULL AND s.journal_entry_id IS NULL
           AND s.status NOT IN ('skipped','duplicate')
         GROUP BY a.id ORDER BY a.id",
    )?;
    for row in stmt.query_map(params![entity_id, from, to], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))? {
        let (uid, id, n) = row?;
        let path = index.get(&id).map(|a| a.path.clone()).unwrap_or_default();
        let mut f = Finding::new("unmatched_evidence", Severity::Warning, format!("{n} statement line(s) on {path} have no posting link"));
        f.account_uid = Some(uid);
        f.count = Some(n);
        findings.push(f);
    }

    // The lock date must never move back without an unlock event.
    if let Some(message) = lock_regression(conn, entity_id, entity.lock_date)? {
        findings.push(Finding::new("lock_moved_back", Severity::Error, message));
    }

    // Totals, and the defensive balance check.
    let (booked, postings, debit, credit): (i64, i64, i64, i64) = conn.query_row(
        "SELECT COUNT(DISTINCT e.id), COUNT(p.id), COALESCE(SUM(CASE WHEN p.amount > 0 THEN p.amount END), 0), COALESCE(SUM(CASE WHEN p.amount < 0 THEN -p.amount END), 0)
         FROM journal_entries e LEFT JOIN postings p ON p.journal_entry_id = e.id
         WHERE e.entity_id = ?1 AND e.status IN ('posted','void') AND e.date BETWEEN ?2 AND ?3",
        params![entity_id, from, to],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let drafts: i64 = conn.query_row("SELECT COUNT(*) FROM journal_entries WHERE entity_id = ?1 AND status = 'draft' AND date BETWEEN ?2 AND ?3", params![entity_id, from, to], |r| r.get(0))?;
    let lines: i64 =
        conn.query_row("SELECT COUNT(*) FROM statement_lines s JOIN accounts a ON a.id = s.account_id WHERE a.entity_id = ?1 AND s.date BETWEEN ?2 AND ?3", params![entity_id, from, to], |r| {
            r.get(0)
        })?;
    let debit_m = money(debit)?;
    let credit_m = money(credit)?;
    let balance = debit_m.checked_sub(credit_m)?;
    if !balance.is_zero() {
        let mut f = Finding::new(
            "unbalanced_period",
            Severity::Error,
            format!(
                "debits {} and credits {} {} differ by {}",
                crate::money::fmt(debit_m.major(), fprec),
                crate::money::fmt(credit_m.major(), fprec),
                entity.currency,
                crate::money::fmt(balance.major(), fprec)
            ),
        );
        f.amount = Some(balance);
        findings.push(f);
    }

    let error_count = findings.iter().filter(|f| f.severity == Severity::Error).count() as i64;
    let warning_count = findings.len() as i64 - error_count;
    Ok(CloseReport {
        entity_uid: entity.uid,
        period_start,
        period_end,
        lock_date: entity.lock_date,
        ok: error_count == 0,
        error_count,
        warning_count,
        findings,
        totals: CloseTotals { booked_entries: booked, draft_entries: drafts, postings, statement_lines: lines, debit: debit_m, credit: credit_m, balance },
    })
}

/// Find the highest lock date that the audit log records since the last `unlock` event. An
/// `unlock` authorises a move back, so it resets the mark. A current lock date before the mark
/// is a regression: some route moved the lock back without an unlock.
fn lock_regression(conn: &Connection, entity_id: i64, current: Option<NaiveDate>) -> AgentResult<Option<String>> {
    let mut stmt = conn.prepare("SELECT action, after FROM audit_log WHERE table_name = 'entities' AND row_id = ?1 AND after IS NOT NULL ORDER BY id")?;
    let events = stmt.query_map([entity_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<std::result::Result<Vec<_>, _>>()?;
    let mut mark: Option<NaiveDate> = None;
    for (action, after) in events {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&after) else { continue };
        let Some(field) = value.get("lock_date") else { continue };
        let lock = field.as_str().and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
        mark = if action == "unlock" { lock } else { mark.max(lock) };
    }
    let back = match (mark, current) {
        (Some(m), Some(c)) => c < m,
        (Some(_), None) => true,
        _ => false,
    };
    if back {
        return Ok(Some(format!("the lock date {} is before the lock date {} in the audit log, and no unlock explains the change", show(current), show(mark))));
    }
    Ok(None)
}

fn show(d: Option<NaiveDate>) -> String {
    d.map(|d| d.to_string()).unwrap_or_else(|| "none".into())
}

/// The result of a lock.
#[derive(Debug, Clone, Serialize)]
pub struct LockOutcome {
    pub entity_uid: String,
    pub previous_lock: Option<NaiveDate>,
    pub lock_date: NaiveDate,
    pub forced: bool,
    pub report: CloseReport,
    pub report_digest: String,
    pub audit_event_id: i64,
}

/// The result of an unlock.
#[derive(Debug, Clone, Serialize)]
pub struct UnlockOutcome {
    pub entity_uid: String,
    pub previous_lock: NaiveDate,
    pub lock_date: NaiveDate,
    pub amendments: Vec<Filing>,
    pub audit_event_id: i64,
}

fn require(field: &str, value: &str) -> AgentResult<()> {
    if value.trim().is_empty() {
        return Err(AgentError::new("invalid_input", format!("give the {field}")));
    }
    Ok(())
}

/// The first day that a lock through `through` would close: the day after the current lock,
/// or the earliest dated record of the entity.
fn first_open_day(conn: &Connection, entity_id: i64, lock: Option<NaiveDate>, through: NaiveDate) -> AgentResult<NaiveDate> {
    if let Some(lock) = lock {
        return Ok(lock.succ_opt().unwrap_or(lock));
    }
    let earliest: Option<String> = conn
        .query_row(
            "SELECT MIN(d) FROM (SELECT MIN(date) AS d FROM journal_entries WHERE entity_id = ?1
               UNION ALL SELECT MIN(s.date) FROM statement_lines s JOIN accounts a ON a.id = s.account_id WHERE a.entity_id = ?1)",
            [entity_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    Ok(earliest.and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()).map(|d| d.min(through)).unwrap_or(through))
}

/// Move the lock date of an entity forward to `through`. The close check runs over the days
/// that the lock closes. An error finding refuses the lock unless `force` is true. The audit
/// event records the actor, the reason, both lock dates, the finding counts and the report digest.
pub fn close_lock(conn: &mut Connection, entity_id: i64, through: NaiveDate, actor: &str, reason: &str, force: bool) -> AgentResult<LockOutcome> {
    require("name of the person who closes the period", actor)?;
    require("reason for the close", reason)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let entity = entities::get_entity(&tx, entity_id)?;
    if let Some(lock) = entity.lock_date {
        if through <= lock {
            return Err(AgentError::new("lock_not_forward", format!("{} is locked through {lock}; a close must move the lock date forward. Use unlock to move it back", entity.name)));
        }
    }
    let start = first_open_day(&tx, entity_id, entity.lock_date, through)?;
    let report = close_check(&tx, entity_id, start, through)?;
    if !report.ok && !force {
        let summary: Vec<String> = report
            .findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
            .fold(BTreeMap::<&str, i64>::new(), |mut m, f| {
                *m.entry(f.code.as_str()).or_insert(0) += 1;
                m
            })
            .into_iter()
            .map(|(code, n)| format!("{code} ({n})"))
            .collect();
        return Err(AgentError::new(
            "close_blocked",
            format!("the close check found {} error(s) from {start} to {through}: {}. Resolve them, or force the lock", report.error_count, summary.join(", ")),
        ));
    }
    let digest = report.digest()?;
    tx.execute("UPDATE entities SET lock_date = ?2, updated_at = ?3 WHERE id = ?1", params![entity_id, through.to_string(), now_ts()])?;
    let audit_event_id = audit::log_by(
        &tx,
        "entities",
        entity_id,
        "close_lock",
        Some(serde_json::json!({"lock_date": entity.lock_date.map(|d| d.to_string())})),
        Some(serde_json::json!({
            "lock_date": through.to_string(),
            "period_start": start.to_string(),
            "forced": force,
            "error_count": report.error_count,
            "warning_count": report.warning_count,
            "finding_counts": report.counts(),
            "report_digest": digest,
        })),
        actor,
        reason,
    )?;
    tx.commit()?;
    Ok(LockOutcome { entity_uid: entity.uid, previous_lock: entity.lock_date, lock_date: through, forced: force, report, report_digest: digest, audit_event_id })
}

/// Move the lock date of an entity back to `to`. It always records an audit event with the
/// actor and the reason. A filing in status `submitted` or `accepted` whose period overlaps the
/// reopened days refuses the unlock, unless `amend` is true. Then each such filing gets a new
/// `prepared` amendment in the same transaction.
pub fn unlock(conn: &mut Connection, entity_id: i64, to: NaiveDate, actor: &str, reason: &str, amend: bool) -> AgentResult<UnlockOutcome> {
    require("name of the person who reopens the period", actor)?;
    require("reason for the unlock", reason)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let entity = entities::get_entity(&tx, entity_id)?;
    let Some(previous) = entity.lock_date else {
        return Err(AgentError::new("invalid_input", format!("{} has no lock date", entity.name)));
    };
    if to >= previous {
        return Err(AgentError::new("invalid_input", format!("{} is locked through {previous}; an unlock must move the lock date back", entity.name)));
    }
    let reopened_from = to.succ_opt().unwrap_or(to);
    let frozen = filings::frozen_overlapping(&tx, entity_id, reopened_from, previous)?;
    if !frozen.is_empty() && !amend {
        let list: Vec<String> = frozen.iter().map(|f| format!("{} {} {}..{} ({})", f.jurisdiction, f.filing_code, f.period_start, f.period_end, f.status.as_str())).collect();
        return Err(AgentError::new("filed_period", format!("filed periods overlap the days from {reopened_from} to {previous}: {}. Amend the filings to reopen them", list.join("; "))));
    }
    let mut amendments = Vec::with_capacity(frozen.len());
    for f in &frozen {
        amendments.push(filings::amend_in(&tx, f, actor, reason)?);
    }
    tx.execute("UPDATE entities SET lock_date = ?2, updated_at = ?3 WHERE id = ?1", params![entity_id, to.to_string(), now_ts()])?;
    let audit_event_id = audit::log_by(
        &tx,
        "entities",
        entity_id,
        "unlock",
        Some(serde_json::json!({"lock_date": previous.to_string()})),
        Some(
            serde_json::json!({"lock_date": to.to_string(), "amended_filings": frozen.iter().map(|f| f.uid.clone()).collect::<Vec<_>>(), "amendments": amendments.iter().map(|f| f.uid.clone()).collect::<Vec<_>>()}),
        ),
        actor,
        reason,
    )?;
    tx.commit()?;
    Ok(UnlockOutcome { entity_uid: entity.uid, previous_lock: previous, lock_date: to, amendments, audit_event_id })
}
