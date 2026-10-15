//! `means intervault`: typed links between entries of different entities.
//!
//! With `--json`, each command prints one JSON object with `"schema_version": 1`. A refusal
//! prints `"ok": false` with an error code and exits with code 1.

use std::path::{Path, PathBuf};

use clap::Subcommand;
use means_core::intervault::{self, Link, LinkView};
use means_core::proposal::{AgentError, AgentResult};
use means_core::rusqlite::Connection;
use serde_json::{json, Value};

use crate::entry_cli::{emit, open, parse_date, refuse_agent, resolve_entity, Output};

#[derive(Subcommand)]
pub enum Command {
    /// Link two existing entries of different entities
    Link {
        /// The uid of entry A
        #[arg(long)]
        a: String,
        /// The uid of entry B
        #[arg(long)]
        b: String,
        /// transfer | capital | loan | interest | dividend | service | recharge | settlement | other
        #[arg(long, default_value = "transfer")]
        kind: String,
        /// Free text saved with the link
        #[arg(long, default_value = "")]
        note: String,
        #[arg(long)]
        json: bool,
    },
    /// Remove a link. A link of a posted entry needs --actor (a human action).
    Unlink {
        uid: String,
        /// Why the link goes; saved in the audit trail
        #[arg(long)]
        reason: String,
        /// The person who removes the link of a posted entry; saved in the audit trail
        #[arg(long)]
        actor: Option<String>,
        /// Remove the link although an entry is in a locked period of its entity (audited)
        #[arg(long)]
        force_locked: bool,
        #[arg(long)]
        json: bool,
    },
    /// List the links with an entry in a period, with the reconciled flag
    List {
        /// Group uid or name
        #[arg(long, conflicts_with = "entity")]
        group: Option<String>,
        /// Entity id, name or uid
        #[arg(long)]
        entity: Option<String>,
        /// First day, YYYY-MM-DD
        #[arg(long)]
        from: String,
        /// Last day, YYYY-MM-DD
        #[arg(long)]
        to: String,
        /// draft | posted | void
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// List the posted intercompany postings of an entity whose entry has no link
    Exceptions {
        /// Entity id, name or uid
        #[arg(long)]
        entity: String,
        /// First day, YYYY-MM-DD
        #[arg(long)]
        from: String,
        /// Last day, YYYY-MM-DD
        #[arg(long)]
        to: String,
        #[arg(long)]
        json: bool,
    },
}

pub fn run(db_path: PathBuf, command: Command) -> anyhow::Result<()> {
    emit(execute(&db_path, command))
}

fn execute(db_path: &Path, command: Command) -> AgentResult<Output> {
    match command {
        Command::Link { a, b, kind, note, json } => link(db_path, &a, &b, &kind, &note, json),
        Command::Unlink { uid, reason, actor, force_locked, json } => unlink(db_path, &uid, &reason, actor.as_deref(), force_locked, json),
        Command::List { group, entity, from, to, status, json } => list(db_path, group.as_deref(), entity.as_deref(), &from, &to, status.as_deref(), json),
        Command::Exceptions { entity, from, to, json } => exceptions(db_path, &entity, &from, &to, json),
    }
}

fn to_value<T: serde::Serialize>(value: &T) -> AgentResult<Value> {
    serde_json::to_value(value).map_err(|e| AgentError::from(means_core::Error::from(e)))
}

/// A link with both entries, the reconciled flag and the warnings of its creation.
pub(crate) fn link_json(conn: &Connection, link: &Link) -> AgentResult<Value> {
    let mut value = to_value(&intervault::view(conn, link)?)?;
    value["warnings"] = json!(link.warnings);
    Ok(value)
}

fn view_text(v: &LinkView) -> String {
    format!(
        "{} {:<10} {:<6} {} {} {} {}  <->  {} {} {} {}  rate {}{}\n",
        v.uid,
        v.kind,
        v.status,
        v.entry_a.date,
        v.entry_a.entity_name,
        means_core::money::plain(v.entry_a.current_amount.major()),
        v.entry_a.current_amount.commodity(),
        v.entry_b.date,
        v.entry_b.entity_name,
        means_core::money::plain(v.entry_b.current_amount.major()),
        v.entry_b.current_amount.commodity(),
        v.rate,
        if v.reconciled { "" } else { "  NOT RECONCILED" }
    )
}

fn link(db_path: &Path, a: &str, b: &str, kind: &str, note: &str, json: bool) -> AgentResult<Output> {
    let db = open(db_path)?;
    let mut conn = db.conn();
    let link = intervault::link(&mut conn, a, b, kind, note)?;
    if !json {
        let mut text = view_text(&intervault::view(&conn, &link)?);
        for w in &link.warnings {
            text.push_str(&format!("  warning: {w}\n"));
        }
        return Ok(Output::Text(text));
    }
    Ok(Output::Json(json!({"link": link_json(&conn, &link)?})))
}

/// `--actor` and `--force-locked` mark the human action on a posted entry. An agent session
/// refuses them before the command opens the ledger.
fn unlink(db_path: &Path, uid: &str, reason: &str, actor: Option<&str>, force_locked: bool, json: bool) -> AgentResult<Output> {
    if actor.is_some() || force_locked {
        refuse_agent("intervault unlink --actor")?;
    }
    let db = open(db_path)?;
    let mut conn = db.conn();
    let out = intervault::unlink(&mut conn, uid, actor.unwrap_or(""), reason, force_locked)?;
    if !json {
        let mut text = format!("removed link {} between {} and {}\n", out.link.uid, out.link.entry_a_uid, out.link.entry_b_uid);
        if !out.locked_entry_uids.is_empty() {
            text.push_str(&format!("  forced through the lock date: {}\n", out.locked_entry_uids.join(", ")));
        }
        text.push_str("  both entries are now not reconciled for intercompany purposes\n");
        return Ok(Output::Text(text));
    }
    Ok(Output::Json(json!({
        "link": to_value(&out.link)?,
        "reason": out.reason,
        "actor": out.actor,
        "force_locked": out.force_locked,
        "locked_entry_uids": out.locked_entry_uids,
        "audit_event_id": out.audit_event_id,
    })))
}

fn list(db_path: &Path, group: Option<&str>, entity: Option<&str>, from: &str, to: &str, status: Option<&str>, json: bool) -> AgentResult<Output> {
    let from = parse_date("from", from)?;
    let to = parse_date("to", to)?;
    let db = open(db_path)?;
    let conn = db.conn();
    let entity = entity.map(|e| resolve_entity(&conn, e)).transpose()?;
    let links = intervault::list_links(&conn, group, entity.as_ref().map(|e| e.uid.as_str()), from, to, status)?;
    if !json {
        let mut text: String = links.iter().map(view_text).collect();
        text.push_str(&format!("{} link(s)\n", links.len()));
        return Ok(Output::Text(text));
    }
    Ok(Output::Json(json!({
        "from": from.to_string(),
        "to": to.to_string(),
        "count": links.len(),
        "reconciled_count": links.iter().filter(|l| l.reconciled).count(),
        "links": to_value(&links)?,
    })))
}

fn exceptions(db_path: &Path, entity: &str, from: &str, to: &str, json: bool) -> AgentResult<Output> {
    let from = parse_date("from", from)?;
    let to = parse_date("to", to)?;
    let db = open(db_path)?;
    let conn = db.conn();
    let entity = resolve_entity(&conn, entity)?;
    let postings = intervault::unlinked_intercompany(&conn, entity.id, from, to)?;
    if !json {
        let mut text: String = postings
            .iter()
            .map(|p| format!("{} {} {:<40} {:>14} {} {}\n", p.date, p.entry_uid, p.account_path, means_core::money::plain(p.quantity.major()), p.quantity.commodity(), p.payee))
            .collect();
        text.push_str(&format!("{} unlinked intercompany posting(s)\n", postings.len()));
        return Ok(Output::Text(text));
    }
    Ok(Output::Json(json!({
        "entity_uid": entity.uid,
        "from": from.to_string(),
        "to": to.to_string(),
        "count": postings.len(),
        "postings": to_value(&postings)?,
    })))
}
