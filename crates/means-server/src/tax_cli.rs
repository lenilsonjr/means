//! `means tax`: tax codes, tax events and the tax summary of a period.
//!
//! Each command prints one JSON object with `"schema_version": 1`. A failure prints
//! `{"schema_version":1,"ok":false,"error":{"code":..,"message":..}}` and exits with code 1.

use std::path::PathBuf;

use anyhow::Result;
use clap::Subcommand;
use means_core::rusqlite::{Connection, OptionalExtension};
use means_core::tax::{self, NewTaxCode, TaxCode, TaxEvent, TaxSummaryRow};
use means_core::{accounts, Db, Error};
use serde_json::{json, Map, Value};

use crate::entity_cli::{date, emit, entity_ref, fields, find_entity, Output};

#[derive(Subcommand)]
pub enum TaxCommand {
    /// Tax codes: dated rate versions of an entity
    Code {
        #[command(subcommand)]
        command: CodeCommand,
    },
    /// Sum the tax events of posted and void entries by code, kind and rate
    Summary {
        /// Entity id, uid or name
        #[arg(long)]
        entity: String,
        /// First tax point (YYYY-MM-DD, inclusive)
        #[arg(long)]
        from: String,
        /// Last tax point (YYYY-MM-DD, inclusive)
        #[arg(long)]
        to: String,
        #[command(flatten)]
        output: Output,
    },
    /// List the tax events of posted and void entries
    Events {
        /// Entity id, uid or name
        #[arg(long)]
        entity: String,
        /// First tax point (YYYY-MM-DD, inclusive)
        #[arg(long)]
        from: String,
        /// Last tax point (YYYY-MM-DD, inclusive)
        #[arg(long)]
        to: String,
        /// Only this tax code
        #[arg(long)]
        code: Option<String>,
        #[command(flatten)]
        output: Output,
    },
}

#[derive(Subcommand)]
pub enum CodeCommand {
    /// Add a rate version of a tax code; it closes the open version of the same code
    Add {
        /// Entity id, uid or name
        #[arg(long)]
        entity: String,
        /// One word, for example SR
        #[arg(long)]
        code: String,
        #[arg(long, default_value = "")]
        name: String,
        /// output | input | reverse_charge | exempt | out_of_scope | withholding | zero_rated
        #[arg(long)]
        kind: String,
        /// Rate in basis points: 900 is 9%
        #[arg(long, default_value_t = 0)]
        rate_bp: i64,
        /// Account path or uid that takes the tax lines (functional currency)
        #[arg(long)]
        tax_account: Option<String>,
        /// Recoverable share in basis points (default 10000)
        #[arg(long)]
        recoverable_bp: Option<i64>,
        /// Jurisdiction, for example SG
        #[arg(long, default_value = "")]
        jurisdiction: String,
        /// First day of the version (YYYY-MM-DD)
        #[arg(long)]
        from: String,
        #[command(flatten)]
        output: Output,
    },
    /// List the tax codes of an entity
    List {
        /// Entity id, uid or name
        #[arg(long)]
        entity: String,
        /// Only the active versions in effect on this date (default: every version)
        #[arg(long)]
        as_of: Option<String>,
        #[command(flatten)]
        output: Output,
    },
    /// End the open version of a code
    Close {
        /// Entity id, uid or name
        #[arg(long)]
        entity: String,
        #[arg(long)]
        code: String,
        /// Day after the last day of the version (YYYY-MM-DD)
        #[arg(long)]
        to: String,
        #[command(flatten)]
        output: Output,
    },
}

fn uid(conn: &Connection, table: &str, id: Option<i64>) -> means_core::Result<Option<String>> {
    let Some(id) = id else { return Ok(None) };
    Ok(conn.query_row(&format!("SELECT uid FROM {table} WHERE id = ?1"), [id], |r| r.get(0)).optional()?)
}

pub fn code_json(conn: &Connection, c: &TaxCode) -> means_core::Result<Value> {
    let account = c.tax_account_id.map(|id| accounts::get_account(conn, id)).transpose()?;
    Ok(json!({
        "uid": c.uid,
        "entity_uid": uid(conn, "entities", Some(c.entity_id))?,
        "code": c.code,
        "name": c.name,
        "kind": c.kind,
        "rate_bp": c.rate_bp,
        "recoverable_bp": c.recoverable_bp,
        "tax_account_uid": account.as_ref().map(|a| a.uid.clone()),
        "tax_account_path": account.as_ref().map(|a| a.path.clone()),
        "jurisdiction": c.jurisdiction,
        "effective_from": c.effective_from,
        "effective_to": c.effective_to,
        "active": c.active,
        "rule_version": c.rule_version(),
        "created_at": c.created_at,
    }))
}

pub fn event_json(conn: &Connection, e: &TaxEvent) -> means_core::Result<Value> {
    Ok(json!({
        "uid": e.uid,
        "entry_uid": uid(conn, "journal_entries", Some(e.entry_id))?,
        "entry_status": e.entry_status,
        "posting_uid": uid(conn, "postings", e.posting_id)?,
        "tax_code_uid": uid(conn, "tax_codes", Some(e.tax_code_id))?,
        "code": e.code,
        "kind": e.kind,
        "rate_bp": e.rate_bp,
        "tax_point": e.tax_point,
        "base": e.base,
        "tax": e.tax,
        "recoverable": e.recoverable,
        "withheld": e.withheld,
        "native_base": e.native_base,
        "native_tax": e.native_tax,
        "party_uid": uid(conn, "payees", e.party_id)?,
        "document_ref": e.document_ref,
        "rule_version": e.rule_version,
        "corrects_event_uid": uid(conn, "tax_events", e.corrects_event_id)?,
        "metadata": e.metadata,
        "created_at": e.created_at,
    }))
}

pub(crate) fn summary_json(r: &TaxSummaryRow) -> Value {
    json!({
        "code": r.code,
        "kind": r.kind,
        "rate_bp": r.rate_bp,
        "base": r.base,
        "tax": r.tax,
        "recoverable": r.recoverable,
        "withheld": r.withheld,
        "count": r.count,
    })
}

fn find_account(conn: &Connection, entity_id: i64, key: &str) -> means_core::Result<i64> {
    let key = key.trim();
    let by_uid: Option<i64> = conn.query_row("SELECT id FROM accounts WHERE uid = ?1 AND entity_id = ?2", means_core::rusqlite::params![key, entity_id], |r| r.get(0)).optional()?;
    match by_uid {
        Some(id) => Ok(id),
        None => accounts::find_by_path(conn, entity_id, key)?.map(|a| a.id).ok_or_else(|| Error::NotFound(format!("account {key}"))),
    }
}

pub fn run(path: PathBuf, command: TaxCommand) -> Result<()> {
    let output = match &command {
        TaxCommand::Code { command: CodeCommand::Add { output, .. } | CodeCommand::List { output, .. } | CodeCommand::Close { output, .. } } => *output,
        TaxCommand::Summary { output, .. } | TaxCommand::Events { output, .. } => *output,
    };
    emit(
        output,
        (|| -> Result<Map<String, Value>> {
            let db = Db::open(path)?;
            let mut conn = db.conn();
            Ok(match command {
                TaxCommand::Code { command: CodeCommand::Add { entity, code, name, kind, rate_bp, tax_account, recoverable_bp, jurisdiction, from, .. } } => {
                    let entity = find_entity(&conn, &entity)?;
                    let tax_account_id = tax_account.as_deref().map(|a| find_account(&conn, entity.id, a)).transpose()?;
                    let saved =
                        tax::add_tax_code(&mut conn, NewTaxCode { entity_id: entity.id, code, name, kind, rate_bp, tax_account_id, recoverable_bp, jurisdiction, effective_from: date(&from)? })?;
                    fields(json!({"entity": entity_ref(&entity), "tax_code": code_json(&conn, &saved)?}))
                }
                TaxCommand::Code { command: CodeCommand::List { entity, as_of, .. } } => {
                    let entity = find_entity(&conn, &entity)?;
                    let as_of = as_of.as_deref().map(date).transpose()?;
                    let codes = tax::list_tax_codes(&conn, entity.id, as_of)?.iter().map(|c| code_json(&conn, c)).collect::<means_core::Result<Vec<_>>>()?;
                    fields(json!({"entity": entity_ref(&entity), "as_of": as_of, "count": codes.len(), "tax_codes": codes}))
                }
                TaxCommand::Code { command: CodeCommand::Close { entity, code, to, .. } } => {
                    let entity = find_entity(&conn, &entity)?;
                    let saved = tax::close_tax_code(&mut conn, entity.id, &code, date(&to)?)?;
                    fields(json!({"entity": entity_ref(&entity), "tax_code": code_json(&conn, &saved)?}))
                }
                TaxCommand::Summary { entity, from, to, .. } => {
                    let entity = find_entity(&conn, &entity)?;
                    let (from, to) = (date(&from)?, date(&to)?);
                    let rows: Vec<Value> = tax::tax_summary(&conn, entity.id, from, to)?.iter().map(summary_json).collect();
                    fields(json!({"entity": entity_ref(&entity), "from": from, "to": to, "functional_currency": entity.currency, "rows": rows}))
                }
                TaxCommand::Events { entity, from, to, code, .. } => {
                    let entity = find_entity(&conn, &entity)?;
                    let (from, to) = (date(&from)?, date(&to)?);
                    let events = tax::tax_events_list(&conn, entity.id, from, to, code.as_deref())?.iter().map(|e| event_json(&conn, e)).collect::<means_core::Result<Vec<_>>>()?;
                    fields(json!({"entity": entity_ref(&entity), "from": from, "to": to, "code": code, "count": events.len(), "tax_events": events}))
                }
            })
        })(),
    )
}
