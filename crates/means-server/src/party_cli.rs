//! Commands for payee facts and party relationships.
//!
//! Each command prints one JSON object with `"schema_version": 1`. A failure prints
//! `{"schema_version":1,"ok":false,"error":{"code":..,"message":..}}` and exits with code 1.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};
use means_core::parties::{self, NewRelationship, PartyFacts, PartyFactsPatch, Relationship};
use means_core::payees::Payee;
use means_core::rusqlite::Connection;
use means_core::{entities, Db};
use serde_json::{json, Map, Value};

use crate::entity_cli::{date, emit, entity_ref, fields, find_entity, opt_date, Output};

#[derive(Args)]
pub struct PayeeSetArgs {
    /// Payee id, uid or name
    pub payee: String,
    /// Entity id, uid or name; it limits the payee search to that entity
    #[arg(long)]
    pub entity: Option<String>,
    /// ISO 3166-1 alpha-2 country code; an empty value clears it
    #[arg(long)]
    pub country: Option<String>,
    /// Tax id as KEY=VALUE, for example VAT=...; repeat it; an empty VALUE removes KEY
    #[arg(long = "tax-id")]
    pub tax_ids: Vec<String>,
    /// person | company | authority | other
    #[arg(long)]
    pub kind: Option<String>,
    #[command(flatten)]
    pub output: Output,
}

#[derive(Subcommand)]
pub enum PartyCommand {
    /// Record a dated relationship between a payee and its entity
    Relate {
        /// Payee id, uid or name
        #[arg(long)]
        payee: String,
        /// Entity id, uid or name
        #[arg(long)]
        entity: String,
        /// owner | subsidiary | director | related_party | contractor | employee | customer | supplier | lender | borrower
        #[arg(long)]
        kind: String,
        /// Basis points held (10000 is 100%)
        #[arg(long)]
        ownership_bp: Option<i64>,
        /// First day of the relationship (YYYY-MM-DD)
        #[arg(long)]
        from: String,
        /// Evidence for the relationship, for example a register or an agreement
        #[arg(long, default_value = "")]
        basis: String,
        #[command(flatten)]
        output: Output,
    },
    /// End the open relationship of a kind
    Unrelate {
        /// Payee id, uid or name
        #[arg(long)]
        payee: String,
        /// Entity id, uid or name
        #[arg(long)]
        entity: String,
        #[arg(long)]
        kind: String,
        /// Day after the last day of the relationship (YYYY-MM-DD)
        #[arg(long)]
        to: String,
        #[command(flatten)]
        output: Output,
    },
    /// List the payees of an entity with their facts and relationships on a date
    List {
        /// Entity id, uid or name
        #[arg(long)]
        entity: String,
        /// Date of the relationships (default: today)
        #[arg(long)]
        as_of: Option<String>,
        /// Only owners, subsidiaries, directors and other related parties
        #[arg(long)]
        related_only: bool,
        #[command(flatten)]
        output: Output,
    },
}

fn party_json(conn: &Connection, payee: &Payee, facts: &PartyFacts) -> means_core::Result<Value> {
    let entity = entities::get_entity(conn, payee.entity_id)?;
    Ok(json!({
        "uid": payee.uid,
        "name": payee.name,
        "entity_uid": entity.uid,
        "active": payee.active,
        "country": facts.country,
        "kind": facts.kind,
        "tax_ids": facts.tax_ids,
    }))
}

fn relationship_json(conn: &Connection, r: &Relationship) -> means_core::Result<Value> {
    let payee = means_core::payees::get(conn, r.payee_id)?;
    let entity = entities::get_entity(conn, r.entity_id)?;
    Ok(json!({
        "id": r.id,
        "payee_uid": payee.uid,
        "payee_name": payee.name,
        "entity_uid": entity.uid,
        "kind": r.kind,
        "ownership_bp": r.ownership_bp,
        "related_party": parties::is_related_kind(&r.kind),
        "effective_from": r.effective_from,
        "effective_to": r.effective_to,
        "basis": r.basis,
    }))
}

pub fn payee_set(path: PathBuf, args: PayeeSetArgs) -> Result<()> {
    emit(
        args.output,
        (|| -> Result<Map<String, Value>> {
            let db = Db::open(path)?;
            let mut conn = db.conn();
            let entity = args.entity.as_deref().map(|e| find_entity(&conn, e)).transpose()?;
            let payee = parties::find_payee(&conn, entity.map(|e| e.id), &args.payee)?;
            let tax_ids = args.tax_ids.iter().map(|s| entities::parse_pair(s)).collect::<means_core::Result<Vec<_>>>()?;
            let facts = parties::set_facts(&mut conn, payee.id, PartyFactsPatch { country: args.country, tax_ids, kind: args.kind })?;
            Ok(fields(json!({"payee": party_json(&conn, &payee, &facts)?})))
        })(),
    )
}

pub fn run(path: PathBuf, command: PartyCommand) -> Result<()> {
    let output = match &command {
        PartyCommand::Relate { output, .. } | PartyCommand::Unrelate { output, .. } | PartyCommand::List { output, .. } => *output,
    };
    emit(
        output,
        (|| -> Result<Map<String, Value>> {
            let db = Db::open(path)?;
            let mut conn = db.conn();
            Ok(match command {
                PartyCommand::Relate { payee, entity, kind, ownership_bp, from, basis, .. } => {
                    let entity = find_entity(&conn, &entity)?;
                    let payee = parties::find_payee(&conn, Some(entity.id), &payee)?;
                    let saved = parties::relate(&mut conn, NewRelationship { payee_id: payee.id, entity_id: entity.id, kind, ownership_bp, effective_from: date(&from)?, basis })?;
                    fields(json!({"relationship": relationship_json(&conn, &saved)?}))
                }
                PartyCommand::Unrelate { payee, entity, kind, to, .. } => {
                    let entity = find_entity(&conn, &entity)?;
                    let payee = parties::find_payee(&conn, Some(entity.id), &payee)?;
                    let saved = parties::unrelate(&mut conn, payee.id, entity.id, &kind, date(&to)?)?;
                    fields(json!({"relationship": relationship_json(&conn, &saved)?}))
                }
                PartyCommand::List { entity, as_of, related_only, .. } => {
                    let entity = find_entity(&conn, &entity)?;
                    let as_of = opt_date(as_of.as_deref())?;
                    let mut rows = vec![];
                    for party in parties::list_parties(&conn, entity.id, as_of, related_only)? {
                        let mut v = party_json(&conn, &party.payee, &party.facts)?;
                        v["related_party"] = json!(party.related_party);
                        v["relationships"] = Value::Array(party.relationships.iter().map(|r| relationship_json(&conn, r)).collect::<means_core::Result<Vec<_>>>()?);
                        rows.push(v);
                    }
                    fields(json!({"entity": entity_ref(&entity), "as_of": as_of, "related_only": related_only, "parties": rows}))
                }
            })
        })(),
    )
}
