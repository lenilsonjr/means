//! `means filing`: the register of returns and reports filed for an entity.
//!
//! With `--json`, each command prints one JSON object with `"schema_version": 1`. A refusal
//! prints `"ok": false` with an error code and exits with code 1. A move to `submitted` or
//! `accepted` is a human action: the engine refuses it while `MEANS_AGENT=1`.

use std::path::{Path, PathBuf};

use clap::Subcommand;
use means_core::filings::{self, Filing, FilingStatus, NewFiling, StatusChange};
use means_core::proposal::AgentResult;
use serde_json::{json, Value};

use crate::entry_cli::{emit, open, parse_date, refuse_agent, resolve_entity, Output};

#[derive(Subcommand)]
pub enum Command {
    /// Record a filing for one period of an entity
    Record {
        /// Entity id, name or uid
        #[arg(long)]
        entity: String,
        /// Jurisdiction code, for example US or SG
        #[arg(long)]
        jurisdiction: String,
        /// Filing code of the authority, for example an annual return form
        #[arg(long)]
        code: String,
        /// First day of the period, YYYY-MM-DD
        #[arg(long)]
        from: String,
        /// Last day of the period, YYYY-MM-DD
        #[arg(long)]
        to: String,
        /// prepared | submitted | accepted | rejected
        #[arg(long, default_value = "prepared")]
        status: String,
        /// SHA-256 of the books snapshot that the filing used
        #[arg(long, default_value = "")]
        snapshot_digest: String,
        /// SHA-256 of the filed artifact
        #[arg(long, default_value = "")]
        artifact_digest: String,
        #[arg(long, default_value = "")]
        notes: String,
        /// The person who records the filing (default: $USER)
        #[arg(long)]
        actor: Option<String>,
        #[arg(long, default_value = "")]
        reason: String,
        #[arg(long)]
        json: bool,
    },
    /// Change the status of a filing. submitted and accepted are human actions
    Status {
        /// Filing uid
        uid: String,
        /// prepared | submitted | accepted | rejected | amended
        #[arg(long)]
        status: String,
        /// Receipt reference from the authority
        #[arg(long)]
        receipt: Option<String>,
        /// Submission date or time, starting with YYYY-MM-DD
        #[arg(long)]
        at: Option<String>,
        /// The person who changes the filing (default: $USER)
        #[arg(long)]
        actor: Option<String>,
        #[arg(long, default_value = "")]
        reason: String,
        #[arg(long)]
        json: bool,
    },
    /// List the filings of an entity
    List {
        /// Entity id, name or uid
        #[arg(long)]
        entity: String,
        #[arg(long)]
        jurisdiction: Option<String>,
        #[arg(long)]
        code: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

pub fn run(db_path: PathBuf, command: Command) -> anyhow::Result<()> {
    emit(execute(&db_path, command))
}

fn execute(db_path: &Path, command: Command) -> AgentResult<Output> {
    match command {
        Command::Record { entity, jurisdiction, code, from, to, status, snapshot_digest, artifact_digest, notes, actor, reason, json } => {
            let status = FilingStatus::parse(&status)?;
            if status.freezes() {
                refuse_agent(&format!("filing record with status {}", status.as_str()))?;
            }
            let input_from = parse_date("from", &from)?;
            let input_to = parse_date("to", &to)?;
            let actor = actor_or_user(actor);
            let db = open(db_path)?;
            let mut conn = db.conn();
            let entity = resolve_entity(&conn, &entity)?;
            let input = NewFiling { entity_id: entity.id, jurisdiction, filing_code: code, period_start: input_from, period_end: input_to, status, snapshot_digest, artifact_digest, notes };
            let filing = filings::record_filing(&mut conn, input, &actor, &reason)?;
            Ok(if json { Output::Json(json!({"filing": filing_json(&filing)})) } else { Output::Text(filing_text(&filing)) })
        }
        Command::Status { uid, status, receipt, at, actor, reason, json } => {
            let status = FilingStatus::parse(&status)?;
            if status.freezes() {
                refuse_agent(&format!("filing status {}", status.as_str()))?;
            }
            let db = open(db_path)?;
            let mut conn = db.conn();
            let filing = filings::get_filing_by_uid(&conn, &uid)?;
            let previous = filing.status;
            let change = StatusChange { status, receipt_ref: receipt, submitted_at: at, actor: actor_or_user(actor), reason };
            let filing = filings::update_filing_status(&mut conn, filing.id, change)?;
            Ok(if json { Output::Json(json!({"filing": filing_json(&filing), "previous_status": previous.as_str()})) } else { Output::Text(filing_text(&filing)) })
        }
        Command::List { entity, jurisdiction, code, json } => {
            let db = open(db_path)?;
            let conn = db.conn();
            let entity = resolve_entity(&conn, &entity)?;
            let list = filings::list_filings(&conn, entity.id, jurisdiction.as_deref(), code.as_deref())?;
            if !json {
                return Ok(Output::Text(list.iter().map(filing_text).collect()));
            }
            Ok(Output::Json(json!({"entity_uid": entity.uid, "count": list.len(), "filings": list.iter().map(filing_json).collect::<Vec<_>>()})))
        }
    }
}

fn actor_or_user(actor: Option<String>) -> String {
    actor.or_else(|| std::env::var("USER").ok()).filter(|a| !a.trim().is_empty()).unwrap_or_else(|| "unknown".into()).trim().to_string()
}

/// The JSON form of a filing: uids, never internal ids.
pub(crate) fn filing_json(f: &Filing) -> Value {
    json!({
        "uid": f.uid,
        "entity_uid": f.entity_uid,
        "jurisdiction": f.jurisdiction,
        "filing_code": f.filing_code,
        "period_start": f.period_start.to_string(),
        "period_end": f.period_end.to_string(),
        "status": f.status.as_str(),
        "snapshot_digest": f.snapshot_digest,
        "chain_head_seq": f.chain_head_seq,
        "chain_head_hash": f.chain_head_hash,
        "artifact_digest": f.artifact_digest,
        "receipt_ref": f.receipt_ref,
        "submitted_at": f.submitted_at,
        "notes": f.notes,
        "amends_uid": f.amends_uid,
        "created_at": f.created_at,
        "updated_at": f.updated_at,
    })
}

fn filing_text(f: &Filing) -> String {
    format!("{} {} {:<12} {}..{} {:<9} {}\n", f.uid, f.jurisdiction, f.filing_code, f.period_start, f.period_end, f.status.as_str(), f.receipt_ref)
}
