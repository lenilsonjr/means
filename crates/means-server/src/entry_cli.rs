//! `means entry`: the machine write interface. An agent drafts and reads; only a human posts.
//!
//! Each command writes exactly one JSON object to stdout. A refusal is an object with
//! `"ok": false` and a stable error code, and the process exits with code 1.

use std::io::Read;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use means_core::journal::{self, EntryFilter};
use means_core::model::{Entity, EntryStatus, JournalEntry};
use means_core::proposal::{self, AgentError, AgentResult, DraftDocument};
use means_core::Db;
use serde_json::{json, Value};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Subcommand)]
pub enum Command {
    /// Create a draft from a JSON document. It never posts.
    Draft {
        /// Entity id, name or uid
        #[arg(long)]
        entity: String,
        /// Read the document from this file
        #[arg(long, conflicts_with = "stdin", required_unless_present = "stdin")]
        json_file: Option<PathBuf>,
        /// Read the document from standard input
        #[arg(long)]
        stdin: bool,
    },
    /// Validate a draft and preview it with a proposal digest. Writes nothing.
    Propose { uid: String },
    /// Post a draft when the digest matches its current proposal (a human action)
    Confirm {
        uid: String,
        /// The proposal_digest that `means entry propose` returned
        #[arg(long)]
        digest: String,
        /// The person who confirms (default: $USER)
        #[arg(long)]
        actor: Option<String>,
    },
    /// Void a posted entry by reversal (a human action)
    Void {
        uid: String,
        /// Explanation saved in the reversal and the audit trail
        #[arg(long)]
        reason: String,
        /// Reversal date, YYYY-MM-DD (default: the original date)
        #[arg(long)]
        date: Option<String>,
        /// Also void the linked entries of other entities (inter-vault links)
        #[arg(long)]
        cascade: bool,
        /// The person who voids with --cascade; saved on each audit event of the cascade (default: $USER)
        #[arg(long, requires = "cascade")]
        actor: Option<String>,
        /// Preview the void and the linked entries; save nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// List the entries of one entity, newest first
    List {
        /// Entity id, name or uid
        #[arg(long)]
        entity: String,
        /// draft | unreviewed | posted | void
        #[arg(long)]
        status: Option<String>,
        /// Earliest date, YYYY-MM-DD
        #[arg(long)]
        since: Option<String>,
        /// Latest date, YYYY-MM-DD
        #[arg(long)]
        until: Option<String>,
        /// Most entries to return, from 1 to 2000
        #[arg(long, default_value_t = 50, conflicts_with = "all")]
        limit: i64,
        /// Return every matching entry (no limit). The snapshot export is the complete read of the books.
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Show one entry with its postings, evidence, review state and hash
    Show {
        uid: String,
        #[arg(long)]
        json: bool,
    },
}

/// What a command prints on success.
pub(crate) enum Output {
    Json(Value),
    Text(String),
}

pub fn run(db_path: PathBuf, command: Command) -> anyhow::Result<()> {
    emit(execute(&db_path, command))
}

/// Print the outcome. A refusal prints the error object and exits with code 1.
pub(crate) fn emit(outcome: AgentResult<Output>) -> anyhow::Result<()> {
    match outcome {
        Ok(Output::Json(mut value)) => {
            if let Some(object) = value.as_object_mut() {
                object.insert("schema_version".into(), json!(SCHEMA_VERSION));
                object.insert("ok".into(), json!(true));
            }
            println!("{}", serde_json::to_string(&value)?);
            Ok(())
        }
        Ok(Output::Text(text)) => {
            print!("{text}");
            Ok(())
        }
        Err(e) => fail(e.code, &e.message),
    }
}

/// Print the error object with a code of the vocabulary and exit with code 1.
pub(crate) fn fail(code: &str, message: &str) -> ! {
    println!("{}", json!({"schema_version": SCHEMA_VERSION, "ok": false, "error": {"code": proposal::normalize_code(code), "message": message}}));
    std::process::exit(1);
}

/// `MEANS_AGENT=1` marks an agent session. It is a workflow aid, not an access control.
/// This is the one check. Every command that posts, voids, deletes or reverts calls it,
/// directly or through `guard`.
pub(crate) fn refuse_agent(action: &str) -> AgentResult<()> {
    if std::env::var("MEANS_AGENT").is_ok_and(|v| v.trim() == "1") {
        return Err(AgentError::new("agent_not_allowed", format!("{action} is a human action; it is refused while MEANS_AGENT=1")));
    }
    Ok(())
}

/// `refuse_agent` for the older text commands: a refusal prints the error object and exits
/// with code 1 before the command opens the ledger.
pub(crate) fn guard(action: &str) {
    if let Err(e) = refuse_agent(action) {
        fail(e.code, &e.message);
    }
}

pub(crate) fn open(db_path: &Path) -> AgentResult<Db> {
    Ok(Db::open(db_path)?)
}

fn execute(db_path: &Path, command: Command) -> AgentResult<Output> {
    match command {
        Command::Draft { entity, json_file, stdin } => draft(db_path, &entity, json_file.as_deref(), stdin),
        Command::Propose { uid } => propose(db_path, &uid),
        Command::Confirm { uid, digest, actor } => confirm(db_path, &uid, &digest, actor),
        Command::Void { uid, reason, date, cascade, actor, dry_run } => void(db_path, &uid, &reason, date.as_deref(), cascade.then(|| actor_or_user(actor)), dry_run),
        Command::List { entity, status, since, until, limit, all, json } => list(db_path, &entity, status.as_deref(), since.as_deref(), until.as_deref(), (!all).then_some(limit), json),
        Command::Show { uid, json } => show(db_path, &uid, json),
    }
}

pub(crate) fn resolve_entity(conn: &means_core::rusqlite::Connection, key: &str) -> AgentResult<Entity> {
    let key = key.trim();
    let all = means_core::entities::list_entities(conn, true)?;
    let by_id = key.parse::<i64>().ok().and_then(|id| all.iter().find(|e| e.id == id).cloned());
    by_id.or_else(|| all.iter().find(|e| e.uid == key || e.name.eq_ignore_ascii_case(key)).cloned()).ok_or_else(|| AgentError::new("not_found", format!("no entity {key}")))
}

pub(crate) fn parse_date(field: &str, value: &str) -> AgentResult<chrono::NaiveDate> {
    means_core::parse_date(value).map_err(|_| AgentError::new("invalid_input", format!("{field} must be YYYY-MM-DD, got {value:?}")))
}

fn read_document(json_file: Option<&Path>, stdin: bool) -> AgentResult<DraftDocument> {
    let text = match json_file {
        Some(path) if !stdin => std::fs::read_to_string(path).map_err(|e| AgentError::new("invalid_input", format!("read {}: {e}", path.display())))?,
        _ => {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text).map_err(|e| AgentError::new("invalid_input", format!("read stdin: {e}")))?;
            text
        }
    };
    serde_json::from_str(&text).map_err(|e| AgentError::new("invalid_input", format!("draft document: {e}")))
}

/// The short form of an entry in write results.
fn entry_ref(conn: &means_core::rusqlite::Connection, entry: &JournalEntry) -> AgentResult<Value> {
    Ok(json!({
        "uid": entry.uid,
        "status": entry.status.as_str(),
        "date": entry.date.to_string(),
        "entity_uid": proposal::uid_of(conn, "entities", Some(entry.entity_id))?,
        "hash": entry.hash,
    }))
}

/// The full form of an entry in read results.
fn entry_view(conn: &means_core::rusqlite::Connection, entry: &JournalEntry) -> AgentResult<Value> {
    let mut postings = Vec::with_capacity(entry.postings.len());
    for p in &entry.postings {
        postings.push(json!({
            "uid": p.uid,
            "account_uid": proposal::uid_of(conn, "accounts", Some(p.account_id))?,
            "account_path": p.account_path,
            "quantity": p.quantity,
            "amount": p.amount,
            "rate": p.rate.map(means_core::money::plain),
            "rate_source": p.rate_source,
            "memo": p.memo,
            "metadata": p.metadata,
            "reconciled_at": p.reconciled_at,
        }));
    }
    let review_state = match entry.status {
        EntryStatus::Draft => "draft",
        EntryStatus::Void => "void",
        EntryStatus::Posted if entry.reviewed_at.is_some() => "reviewed",
        EntryStatus::Posted => "unreviewed",
    };
    Ok(json!({
        "uid": entry.uid,
        "entity_uid": proposal::uid_of(conn, "entities", Some(entry.entity_id))?,
        "date": entry.date.to_string(),
        "status": entry.status.as_str(),
        "origin": entry.origin,
        "payee": entry.payee,
        "description": entry.description,
        "notes": entry.notes,
        "tags": entry.tags,
        "hash": entry.hash,
        "posted_at": entry.posted_at,
        "created_at": entry.created_at,
        "review": {"state": review_state, "reviewed_at": entry.reviewed_at},
        "reverses_uid": proposal::uid_of(conn, "journal_entries", entry.reverses_id)?,
        "reversed_by_uid": proposal::uid_of(conn, "journal_entries", entry.reversed_by_id)?,
        "counterpart_uid": proposal::uid_of(conn, "journal_entries", entry.counterpart_id)?,
        "intervault_link_uids": means_core::intervault::links_for_entry(conn, entry.id)?.into_iter().map(|l| l.uid).collect::<Vec<_>>(),
        "postings": postings,
        "tax_events": means_core::tax::tax_events_for_entry(conn, entry.id)?.iter().map(|e| crate::tax_cli::event_json(conn, e)).collect::<means_core::Result<Vec<_>>>()?,
        "evidence": proposal::evidence(conn, entry.id)?,
    }))
}

fn entry_text(entry: &JournalEntry) -> String {
    let mut out = format!("{} {} {:<6} {}\n", entry.uid, entry.date, entry.status.as_str(), entry.display_payee);
    for p in &entry.postings {
        out.push_str(&format!("    {:<48} {:>14} {}\n", p.account_path, means_core::money::fmt(p.quantity.major(), p.quantity.precision()), p.quantity.commodity()));
    }
    out
}

fn draft(db_path: &Path, entity: &str, json_file: Option<&Path>, stdin: bool) -> AgentResult<Output> {
    let document = read_document(json_file, stdin)?;
    let db = open(db_path)?;
    let mut conn = db.conn();
    let entity = resolve_entity(&conn, entity)?;
    let drafted = proposal::create_draft(&mut conn, entity.id, &document)?;
    let mut out = json!({"entry": entry_ref(&conn, &drafted.entry)?, "audit_event_id": drafted.audit_event_id});
    if let Some(link) = &drafted.link {
        out["intervault_link"] = crate::intervault_cli::link_json(&conn, link)?;
    }
    Ok(Output::Json(out))
}

fn propose(db_path: &Path, uid: &str) -> AgentResult<Output> {
    let db = open(db_path)?;
    let conn = db.conn();
    let id = proposal::entry_id_by_uid(&conn, uid)?;
    let preview = proposal::propose(&conn, id)?;
    Ok(Output::Json(serde_json::to_value(&preview).map_err(means_core::Error::from)?))
}

fn confirm(db_path: &Path, uid: &str, digest: &str, actor: Option<String>) -> AgentResult<Output> {
    refuse_agent("entry confirm")?;
    let actor = actor.or_else(|| std::env::var("USER").ok()).filter(|a| !a.trim().is_empty()).unwrap_or_else(|| "unknown".into());
    let db = open(db_path)?;
    let mut conn = db.conn();
    let id = proposal::entry_id_by_uid(&conn, uid)?;
    let confirmed = proposal::confirm(&mut conn, id, digest, &actor)?;
    Ok(Output::Json(json!({
        "entry": entry_ref(&conn, &confirmed.entry)?,
        "actor": actor.trim(),
        "proposal_digest": digest.trim(),
        "audit_event_id": confirmed.audit_event_id,
    })))
}

/// The given actor, else `$USER`, else "unknown".
pub(crate) fn actor_or_user(actor: Option<String>) -> String {
    let given = |a: &String| !a.trim().is_empty();
    actor.filter(given).or_else(|| std::env::var("USER").ok().filter(given)).unwrap_or_else(|| "unknown".into()).trim().to_string()
}

/// A void writes a reversal. `--dry-run` previews it and saves nothing, so an agent can run it.
/// `cascade` holds the actor of a void with cascade.
fn void(db_path: &Path, uid: &str, reason: &str, date: Option<&str>, cascade: Option<String>, dry_run: bool) -> AgentResult<Output> {
    if !dry_run {
        refuse_agent("entry void")?;
    }
    if reason.trim().is_empty() {
        return Err(AgentError::new("invalid_input", "give a reason for the void"));
    }
    let date = date.map(|d| parse_date("date", d)).transpose()?;
    let db = open(db_path)?;
    let mut conn = db.conn();
    let id = proposal::entry_id_by_uid(&conn, uid)?;
    let review = journal::review_void_by(&mut conn, id, date, reason, !dry_run, cascade.is_some(), cascade.as_deref().unwrap_or(""))?;
    // A preview rolls back: the entry keeps its status, and the reversal does not exist.
    let saved = review.applied || review.already_void;
    let entry = if saved { review.entry.clone() } else { journal::get_entry(&conn, id)? };
    let reversal = if saved { entry_ref(&conn, &review.reversal)? } else { review.reversal_ref() };
    let linked = review.linked_entries.iter().map(|l| serde_json::to_value(l).map_err(|e| AgentError::from(means_core::Error::from(e)))).collect::<AgentResult<Vec<_>>>()?;
    Ok(Output::Json(json!({
        "entry": entry_ref(&conn, &entry)?,
        "reversal": reversal,
        "already_void": review.already_void,
        "applied": review.applied,
        "cascade": review.cascade,
        "actor": cascade,
        "needs_cascade": review.needs_cascade,
        "linked_entries": linked,
        "evidence_lines_returned": review.evidence_lines,
    })))
}

/// The largest page that `journal::list_entries` returns.
const PAGE: i64 = 2000;

/// `limit` is `None` for `--all`. The engine then reads every matching entry, page by page.
fn list(db_path: &Path, entity: &str, status: Option<&str>, since: Option<&str>, until: Option<&str>, limit: Option<i64>, json: bool) -> AgentResult<Output> {
    if limit.is_some_and(|l| !(1..=PAGE).contains(&l)) {
        return Err(AgentError::new("invalid_input", "limit must be from 1 to 2000; use --all for every entry"));
    }
    let (status, only_unreviewed) = match status.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        None => (None, false),
        Some("unreviewed") => (None, true),
        Some(s @ ("draft" | "posted" | "void")) => (Some(EntryStatus::parse(s)?), false),
        Some(other) => return Err(AgentError::new("invalid_input", format!("unknown status {other:?}; use draft, unreviewed, posted or void"))),
    };
    let from = since.map(|d| parse_date("since", d)).transpose()?;
    let to = until.map(|d| parse_date("until", d)).transpose()?;
    let db = open(db_path)?;
    let conn = db.conn();
    let entity = resolve_entity(&conn, entity)?;
    let mut filter = EntryFilter { entity_id: Some(entity.id), status, from, to, only_unreviewed, limit: limit.unwrap_or(PAGE), ..Default::default() };
    let (mut entries, total) = journal::list_entries(&conn, &filter)?;
    if limit.is_none() {
        while (entries.len() as i64) < total {
            filter.offset = entries.len() as i64;
            let (page, _) = journal::list_entries(&conn, &filter)?;
            if page.is_empty() {
                break;
            }
            entries.extend(page);
        }
    }
    if !json {
        let mut text: String = entries.iter().map(entry_text).collect();
        text.push_str(&format!("{} of {} entries\n", entries.len(), total));
        return Ok(Output::Text(text));
    }
    let views = entries.iter().map(|e| entry_view(&conn, e)).collect::<AgentResult<Vec<_>>>()?;
    Ok(Output::Json(json!({"entity_uid": entity.uid, "count": views.len(), "total": total, "entries": views})))
}

fn show(db_path: &Path, uid: &str, json: bool) -> AgentResult<Output> {
    let db = open(db_path)?;
    let conn = db.conn();
    let id = proposal::entry_id_by_uid(&conn, uid)?;
    let entry = journal::get_entry(&conn, id)?;
    if !json {
        return Ok(Output::Text(entry_text(&entry)));
    }
    Ok(Output::Json(json!({"entry": entry_view(&conn, &entry)?})))
}
