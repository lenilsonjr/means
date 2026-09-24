//! `means close`: check a period, lock it, reopen it, and list the year-end questions.
//!
//! With `--json`, each command prints one JSON object with `"schema_version": 1`. A refusal
//! prints `"ok": false` with an error code and exits with code 1. `lock` and `unlock` are human
//! actions: the engine refuses them while `MEANS_AGENT=1`.

use std::path::{Path, PathBuf};

use clap::Subcommand;
use means_core::close::{self, CloseReport, Severity};
use means_core::close_questions;
use means_core::proposal::{AgentError, AgentResult};
use serde_json::{json, Value};

use crate::entry_cli::{emit, open, parse_date, refuse_agent, resolve_entity, Output};
use crate::filing_cli::filing_json;

#[derive(Subcommand)]
pub enum Command {
    /// Check one period of an entity for a close. Writes nothing.
    Check {
        /// Entity id, name or uid
        #[arg(long)]
        entity: String,
        /// First day of the period, YYYY-MM-DD
        #[arg(long)]
        from: String,
        /// Last day of the period, YYYY-MM-DD
        #[arg(long)]
        to: String,
        #[arg(long)]
        json: bool,
    },
    /// Move the lock date forward after a clean check (a human action)
    Lock {
        /// Entity id, name or uid
        #[arg(long)]
        entity: String,
        /// Last day to lock, YYYY-MM-DD
        #[arg(long)]
        through: String,
        /// The person who closes the period
        #[arg(long)]
        actor: String,
        /// Why the period closes
        #[arg(long)]
        reason: String,
        /// Lock even when the check finds errors
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
    /// Move the lock date back (a human action)
    Unlock {
        /// Entity id, name or uid
        #[arg(long)]
        entity: String,
        /// The new lock date, YYYY-MM-DD
        #[arg(long)]
        to: String,
        /// The person who reopens the period
        #[arg(long)]
        actor: String,
        /// Why the period reopens
        #[arg(long)]
        reason: String,
        /// Create an amendment for each submitted or accepted filing in the reopened days
        #[arg(long)]
        amend: bool,
        #[arg(long)]
        json: bool,
    },
    /// List the year-end flags of a jurisdiction that the ledger has, and the ones it lacks. Writes nothing.
    Questions {
        /// Entity id, name or uid
        #[arg(long)]
        entity: String,
        /// Jurisdiction code, for example SG, US, ES or PT
        #[arg(long)]
        jurisdiction: String,
        /// Date of the flag values, YYYY-MM-DD (default: today)
        #[arg(long)]
        as_of: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

pub fn run(db_path: PathBuf, command: Command) -> anyhow::Result<()> {
    emit(execute(&db_path, command))
}

fn execute(db_path: &Path, command: Command) -> AgentResult<Output> {
    match command {
        Command::Check { entity, from, to, json } => check(db_path, &entity, &from, &to, json),
        Command::Lock { entity, through, actor, reason, force, json } => lock(db_path, &entity, &through, &actor, &reason, force, json),
        Command::Unlock { entity, to, actor, reason, amend, json } => unlock(db_path, &entity, &to, &actor, &reason, amend, json),
        Command::Questions { entity, jurisdiction, as_of, json } => questions(db_path, &entity, &jurisdiction, as_of.as_deref(), json),
    }
}

fn to_value<T: serde::Serialize>(value: &T) -> AgentResult<Value> {
    serde_json::to_value(value).map_err(|e| AgentError::from(means_core::Error::from(e)))
}

fn report_text(report: &CloseReport) -> String {
    let mut out = format!("close check {} to {}: {} error(s), {} warning(s)\n", report.period_start, report.period_end, report.error_count, report.warning_count);
    for f in &report.findings {
        let level = match f.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        out.push_str(&format!("  {level:<7} {:<28} {}\n", f.code, f.message));
    }
    out
}

fn check(db_path: &Path, entity: &str, from: &str, to: &str, json: bool) -> AgentResult<Output> {
    let from = parse_date("from", from)?;
    let to = parse_date("to", to)?;
    let db = open(db_path)?;
    let conn = db.conn();
    let entity = resolve_entity(&conn, entity)?;
    let report = close::close_check(&conn, entity.id, from, to)?;
    if !json {
        return Ok(Output::Text(report_text(&report)));
    }
    let digest = report.digest()?;
    Ok(Output::Json(json!({"report": to_value(&report)?, "report_digest": digest})))
}

fn lock(db_path: &Path, entity: &str, through: &str, actor: &str, reason: &str, force: bool, json: bool) -> AgentResult<Output> {
    refuse_agent("close lock")?;
    let through = parse_date("through", through)?;
    let db = open(db_path)?;
    let mut conn = db.conn();
    let entity = resolve_entity(&conn, entity)?;
    let out = close::close_lock(&mut conn, entity.id, through, actor, reason, force)?;
    if !json {
        let mut text = report_text(&out.report);
        text.push_str(&format!(
            "{} locked through {} (was {}){}\n",
            entity.name,
            out.lock_date,
            out.previous_lock.map(|d| d.to_string()).unwrap_or_else(|| "none".into()),
            if out.forced { ", forced" } else { "" }
        ));
        return Ok(Output::Text(text));
    }
    Ok(Output::Json(json!({
        "entity_uid": out.entity_uid,
        "previous_lock": out.previous_lock.map(|d| d.to_string()),
        "lock_date": out.lock_date.to_string(),
        "forced": out.forced,
        "actor": actor.trim(),
        "reason": reason.trim(),
        "finding_counts": out.report.counts(),
        "report": to_value(&out.report)?,
        "report_digest": out.report_digest,
        "audit_event_id": out.audit_event_id,
    })))
}

fn unlock(db_path: &Path, entity: &str, to: &str, actor: &str, reason: &str, amend: bool, json: bool) -> AgentResult<Output> {
    refuse_agent("close unlock")?;
    let to = parse_date("to", to)?;
    let db = open(db_path)?;
    let mut conn = db.conn();
    let entity = resolve_entity(&conn, entity)?;
    let out = close::unlock(&mut conn, entity.id, to, actor, reason, amend)?;
    if !json {
        let mut text = format!("{} unlocked to {} (was {})\n", entity.name, out.lock_date, out.previous_lock);
        for f in &out.amendments {
            text.push_str(&format!("  amendment {} {} {} {}..{}\n", f.uid, f.jurisdiction, f.filing_code, f.period_start, f.period_end));
        }
        return Ok(Output::Text(text));
    }
    Ok(Output::Json(json!({
        "entity_uid": out.entity_uid,
        "previous_lock": out.previous_lock.to_string(),
        "lock_date": out.lock_date.to_string(),
        "actor": actor.trim(),
        "reason": reason.trim(),
        "amendments": out.amendments.iter().map(filing_json).collect::<Vec<_>>(),
        "audit_event_id": out.audit_event_id,
    })))
}

fn questions(db_path: &Path, entity: &str, jurisdiction: &str, as_of: Option<&str>, json: bool) -> AgentResult<Output> {
    let as_of = as_of.map(|d| parse_date("as_of", d)).transpose()?.unwrap_or_else(means_core::today);
    let db = open(db_path)?;
    let conn = db.conn();
    let entity = resolve_entity(&conn, entity)?;
    let q = close_questions::questionnaire(&conn, entity.id, jurisdiction, as_of)?;
    if !json {
        let mut text = format!("{} year-end questions for {} on {}\n", q.jurisdiction, entity.name, q.as_of);
        for a in &q.answered {
            text.push_str(&format!("  answered {:<36} {}\n", a.key, a.value));
        }
        for m in &q.missing {
            text.push_str(&format!("  missing  {:<36} {}\n", m.key, m.question));
        }
        return Ok(Output::Text(text));
    }
    Ok(Output::Json(json!({
        "entity_uid": entity.uid,
        "jurisdiction": q.jurisdiction,
        "as_of": q.as_of.to_string(),
        "answered": to_value(&q.answered)?,
        "missing": to_value(&q.missing)?,
    })))
}
