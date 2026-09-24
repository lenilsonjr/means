//! `means invoice`: invoices received or issued elsewhere, their open items and settlements.
//!
//! The engine does not issue invoices. It records them as facts and links them to the ledger.
//! With `--json`, each command prints one JSON object with `"schema_version": 1`. A refusal
//! prints `"ok": false` with an error code and exits with code 1. `cancel` is a human action:
//! the engine refuses it while `MEANS_AGENT=1`.

use std::io::Read;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use means_core::invoices::{self, Direction, Invoice, InvoiceInput, SettlementInput, SettlementKind};
use means_core::proposal::{AgentError, AgentResult};
use means_core::rusqlite::Connection;
use serde_json::{json, Value};

use crate::entry_cli::{emit, open, parse_date, refuse_agent, resolve_entity, Output};

#[derive(Subcommand)]
pub enum Command {
    /// Record an invoice from a JSON document; optionally draft its recognition entry
    Add {
        /// Entity id, name or uid
        #[arg(long)]
        entity: String,
        /// Read the document from this file
        #[arg(long, conflicts_with = "stdin", required_unless_present = "stdin")]
        json_file: Option<PathBuf>,
        /// Read the document from standard input
        #[arg(long)]
        stdin: bool,
        /// Create the recognition entry as a draft
        #[arg(long)]
        recognize: bool,
        #[arg(long)]
        json: bool,
    },
    /// List the invoices of an entity
    List {
        /// Entity id, name or uid
        #[arg(long)]
        entity: String,
        /// Only invoices with an open balance
        #[arg(long)]
        open: bool,
        /// sales | purchase
        #[arg(long)]
        direction: Option<String>,
        /// Report date, YYYY-MM-DD (default: today)
        #[arg(long)]
        as_of: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show an invoice with its lines, settlements, open balance, entries and documents
    Show {
        /// Invoice uid
        uid: String,
        #[arg(long)]
        json: bool,
    },
    /// Link an entry or a posting to an invoice and reduce its open balance
    Settle {
        /// Invoice uid
        uid: String,
        /// Journal entry uid that settles the invoice
        #[arg(long, alias = "from-entry")]
        entry: Option<String>,
        /// Posting uid inside the entry
        #[arg(long)]
        posting: Option<String>,
        /// Settlement date, YYYY-MM-DD
        #[arg(long)]
        date: String,
        /// Amount in the invoice commodity, as a decimal string
        #[arg(long, allow_hyphen_values = true)]
        amount: String,
        /// payment | withholding | fee | fx_difference | write_off | refund
        #[arg(long, default_value = "payment")]
        kind: String,
        /// Account for the exchange difference; without it the difference is only reported
        #[arg(long)]
        fx_account: Option<String>,
        /// Control account (payable or receivable) when the invoice keeps none
        #[arg(long)]
        control_account: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Record a credit note that settles an invoice
    CreditNote {
        /// Entity id, name or uid
        #[arg(long)]
        entity: String,
        /// Uid of the invoice that the credit note credits
        #[arg(long)]
        credits: String,
        #[arg(long, conflicts_with = "stdin", required_unless_present = "stdin")]
        json_file: Option<PathBuf>,
        #[arg(long)]
        stdin: bool,
        /// Create the recognition entry of the credit note as a draft
        #[arg(long)]
        recognize: bool,
        #[arg(long)]
        json: bool,
    },
    /// Cancel an invoice that nothing settles (a human action)
    Cancel {
        /// Invoice uid
        uid: String,
        /// Explanation saved in the audit trail
        #[arg(long)]
        reason: String,
        /// The person who cancels (default: $USER)
        #[arg(long)]
        actor: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

pub fn run(db_path: PathBuf, command: Command) -> anyhow::Result<()> {
    emit(execute(&db_path, command))
}

fn execute(db_path: &Path, command: Command) -> AgentResult<Output> {
    match command {
        Command::Add { entity, json_file, stdin, recognize, json } => {
            let mut input = read_input(json_file.as_deref(), stdin)?;
            input.recognize |= recognize;
            let db = open(db_path)?;
            let mut conn = db.conn();
            let entity = resolve_entity(&conn, &entity)?;
            let recorded = invoices::record_invoice(&mut conn, entity.id, &input)?;
            let value = json!({
                "invoice": to_value(&recorded.invoice)?,
                "lines": to_value(&recorded.lines)?,
                "recognition_entry": recorded.recognition.as_ref().map(entry_ref),
                "audit_event_id": recorded.audit_event_id,
            });
            Ok(if json { Output::Json(value) } else { Output::Text(invoice_text(&recorded.invoice)) })
        }
        Command::List { entity, open: open_only, direction, as_of, json } => {
            let direction = direction.as_deref().map(Direction::parse).transpose()?;
            let as_of = as_of.map(|d| parse_date("as-of", &d)).transpose()?;
            let db = open(db_path)?;
            let conn = db.conn();
            let entity = resolve_entity(&conn, &entity)?;
            let list = invoices::list_invoices(&conn, entity.id, direction, open_only, as_of)?;
            let day = as_of.unwrap_or_else(means_core::today);
            if !json {
                return Ok(Output::Text(list.iter().map(invoice_text).collect()));
            }
            let rows = list
                .iter()
                .map(|i| {
                    let mut v = to_value(i)?;
                    v["days_overdue"] = json!(days_overdue(i, day));
                    Ok(v)
                })
                .collect::<AgentResult<Vec<_>>>()?;
            Ok(Output::Json(json!({"entity_uid": entity.uid, "as_of": as_of.map(|d| d.to_string()), "count": rows.len(), "invoices": rows})))
        }
        Command::Show { uid, json } => {
            let db = open(db_path)?;
            let conn = db.conn();
            let invoice = invoices::get_invoice_by_uid(&conn, &uid)?;
            if !json {
                return Ok(Output::Text(invoice_text(&invoice)));
            }
            Ok(Output::Json(show(&conn, &invoice)?))
        }
        Command::Settle { uid, entry, posting, date, amount, kind, fx_account, control_account, json } => {
            let kind = SettlementKind::parse(&kind)?;
            let date = parse_date("date", &date)?;
            let amount = means_core::money::parse(&amount).map_err(|_| AgentError::new("invalid_input", format!("amount must be a decimal string, got {amount:?}")))?;
            let db = open(db_path)?;
            let mut conn = db.conn();
            let input = SettlementInput { invoice_uid: uid, entry_uid: entry, posting_uid: posting, date, amount, kind, fx_account, control_account };
            let settled = invoices::settle(&mut conn, input)?;
            let value = json!({
                "invoice": to_value(&settled.invoice)?,
                "settlement": to_value(&settled.settlement)?,
                "converted": settled.converted,
                "fx_difference": settled.fx_difference,
                "fx_difference_entry": settled.fx_entry.as_ref().map(entry_ref),
                "audit_event_id": settled.audit_event_id,
            });
            Ok(if json { Output::Json(value) } else { Output::Text(invoice_text(&settled.invoice)) })
        }
        Command::CreditNote { entity, credits, json_file, stdin, recognize, json } => {
            let mut input = read_input(json_file.as_deref(), stdin)?;
            input.recognize |= recognize;
            let db = open(db_path)?;
            let mut conn = db.conn();
            let entity = resolve_entity(&conn, &entity)?;
            let original = invoices::get_invoice_by_uid(&conn, &credits)?;
            if original.entity_id != entity.id {
                return Err(AgentError::new("invalid_input", format!("invoice {credits} belongs to another entity")));
            }
            let noted = invoices::credit_note(&mut conn, &credits, &input)?;
            let value = json!({
                "credit_note": to_value(&noted.credit_note.invoice)?,
                "lines": to_value(&noted.credit_note.lines)?,
                "recognition_entry": noted.credit_note.recognition.as_ref().map(entry_ref),
                "settlement": to_value(&noted.settlement)?,
                "invoice": to_value(&noted.invoice)?,
                "audit_event_id": noted.credit_note.audit_event_id,
            });
            Ok(if json { Output::Json(value) } else { Output::Text(invoice_text(&noted.credit_note.invoice)) })
        }
        Command::Cancel { uid, reason, actor, json } => {
            refuse_agent("invoice cancel")?;
            let actor = actor.or_else(|| std::env::var("USER").ok()).filter(|a| !a.trim().is_empty()).unwrap_or_else(|| "unknown".into());
            let db = open(db_path)?;
            let mut conn = db.conn();
            let invoice = invoices::cancel(&mut conn, &uid, &actor, &reason)?;
            let value = json!({"invoice": to_value(&invoice)?, "actor": actor.trim(), "reason": reason.trim()});
            Ok(if json { Output::Json(value) } else { Output::Text(invoice_text(&invoice)) })
        }
    }
}

fn read_input(json_file: Option<&Path>, stdin: bool) -> AgentResult<InvoiceInput> {
    let text = match json_file {
        Some(path) if !stdin => std::fs::read_to_string(path).map_err(|e| AgentError::new("invalid_input", format!("read {}: {e}", path.display())))?,
        _ => {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text).map_err(|e| AgentError::new("invalid_input", format!("read stdin: {e}")))?;
            text
        }
    };
    serde_json::from_str(&text).map_err(|e| AgentError::new("invalid_input", format!("invoice document: {e}")))
}

fn to_value<T: serde::Serialize>(value: &T) -> AgentResult<Value> {
    Ok(serde_json::to_value(value).map_err(means_core::Error::from)?)
}

fn days_overdue(invoice: &Invoice, day: chrono::NaiveDate) -> i64 {
    match invoice.due_date {
        Some(due) if !invoice.open.is_zero() => (day - due).num_days().max(0),
        _ => 0,
    }
}

/// The short form of an entry in write results.
fn entry_ref(entry: &means_core::JournalEntry) -> Value {
    json!({"uid": entry.uid, "status": entry.status.as_str(), "date": entry.date.to_string(), "hash": entry.hash})
}

fn show(conn: &Connection, invoice: &Invoice) -> AgentResult<Value> {
    let lines = invoices::lines(conn, invoice.id)?;
    let settlements = invoices::settlements(conn, invoice.id)?;
    let credit_notes = invoices::credit_notes(conn, invoice.id)?;
    let mut linked = Vec::new();
    let mut add = |id: Option<i64>, role: &str| -> AgentResult<()> {
        if let Some(id) = id {
            let entry = means_core::journal::get_entry(conn, id)?;
            let mut v = entry_ref(&entry);
            v["role"] = json!(role);
            linked.push(v);
        }
        Ok(())
    };
    add(invoice.recognition_entry_id, "recognition")?;
    for s in &settlements {
        add(s.entry_id, s.kind.as_str())?;
        add(s.fx_difference_entry_id, "fx_difference")?;
    }
    let mut documents = Vec::new();
    if let Some(uid) = &invoice.document_uid {
        documents.push(json!({"uid": uid, "role": "invoice"}));
    }
    if let Some(entry_id) = invoice.recognition_entry_id {
        for link in means_core::documents::document_links_for_entry(conn, entry_id)? {
            if invoice.document_uid.as_deref() != Some(link.document_uid.as_str()) {
                documents.push(json!({"uid": link.document_uid, "role": link.role}));
            }
        }
    }
    let duplicates = invoices::invoice_duplicates(conn, invoice.id)?;
    let mut value = to_value(invoice)?;
    value["days_overdue"] = json!(days_overdue(invoice, means_core::today()));
    Ok(json!({
        "invoice": value,
        "lines": to_value(&lines)?,
        "settlements": to_value(&settlements)?,
        "credit_notes": to_value(&credit_notes)?,
        "linked_entries": linked,
        "documents": documents,
        "possible_duplicates": to_value(&duplicates)?,
    }))
}

fn invoice_text(i: &Invoice) -> String {
    format!(
        "{} {:<8} {:<16} {} {:<24} {:>12} open {:>12} {} {}\n",
        i.uid,
        i.direction.as_str(),
        i.label(),
        i.issue_date,
        i.party_name.as_deref().unwrap_or("-"),
        means_core::money::fmt(i.gross.major(), i.gross.precision()),
        means_core::money::fmt(i.open.major(), i.open.precision()),
        i.commodity,
        i.status.as_str()
    )
}
