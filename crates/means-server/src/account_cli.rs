//! `means account`: the identifiers of one account.
//!
//! Each command prints one JSON object with `"schema_version": 1`. A failure prints
//! `{"schema_version":1,"ok":false,"error":{"code":..,"message":..}}` and exits with code 1.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};
use means_core::accounts::{self, AccountUpdate};
use means_core::model::Account;
use means_core::rusqlite::Connection;
use means_core::{Db, Error};
use serde_json::{json, Map, Value};

use crate::entity_cli::{emit, entity_ref, fields, find_entity, Output};

#[derive(Subcommand)]
pub enum Command {
    /// Set the country, IBAN, BIC, institution and location of an account
    Set(SetArgs),
}

#[derive(Args)]
pub struct SetArgs {
    /// Account path (for example Assets:Bank:Operating), uid or id
    pub account: String,
    /// Entity id, uid or name
    #[arg(long)]
    pub entity: String,
    /// ISO 3166-1 alpha-2 country of the account; an empty value clears it
    #[arg(long)]
    pub country: Option<String>,
    /// IBAN; spaces are allowed; an empty value clears it
    #[arg(long)]
    pub iban: Option<String>,
    /// BIC (SWIFT code); an empty value clears it
    #[arg(long)]
    pub bic: Option<String>,
    /// The bank, broker or other institution that holds the account
    #[arg(long)]
    pub institution: Option<String>,
    /// Where the asset is, as free text, for example a state
    #[arg(long)]
    pub location: Option<String>,
    #[command(flatten)]
    pub output: Output,
}

/// Find an account of an entity by uid, id or path.
fn find_account(conn: &Connection, entity_id: i64, key: &str) -> means_core::Result<Account> {
    let key = key.trim();
    let all = accounts::list_accounts(conn, Some(entity_id), true)?;
    if let Some(a) = all.iter().find(|a| a.uid == key || key.parse::<i64>().ok() == Some(a.id)) {
        return Ok(a.clone());
    }
    accounts::find_by_path(conn, entity_id, key)?.ok_or_else(|| Error::NotFound(format!("account {key} in entity {entity_id}")))
}

pub fn account_json(a: &Account) -> Value {
    json!({
        "id": a.id,
        "uid": a.uid,
        "path": a.path,
        "type": a.r#type.as_str(),
        "commodity": a.commodity,
        "country": a.country,
        "iban": a.iban,
        "bic": a.bic,
        "institution": a.institution,
        "location": a.location,
    })
}

pub fn run(path: PathBuf, command: Command) -> Result<()> {
    match command {
        Command::Set(args) => set(path, args),
    }
}

fn set(path: PathBuf, args: SetArgs) -> Result<()> {
    emit(
        args.output,
        (|| -> Result<Map<String, Value>> {
            let db = Db::open(path)?;
            let mut conn = db.conn();
            let entity = find_entity(&conn, &args.entity)?;
            let account = find_account(&conn, entity.id, &args.account)?;
            let upd = AccountUpdate { country: args.country, iban: args.iban, bic: args.bic, institution: args.institution, location: args.location, ..Default::default() };
            let changed = upd.country.is_some() || upd.iban.is_some() || upd.bic.is_some() || upd.institution.is_some() || upd.location.is_some();
            let after = if changed {
                // `update_account` records the account before and after in the audit log. The
                // transaction keeps the change and its audit event together.
                let tx = conn.transaction_with_behavior(means_core::rusqlite::TransactionBehavior::Immediate)?;
                let after = accounts::update_account(&tx, account.id, upd)?;
                tx.commit()?;
                after
            } else {
                account
            };
            Ok(fields(json!({"entity": entity_ref(&entity), "account": account_json(&after)})))
        })(),
    )
}
