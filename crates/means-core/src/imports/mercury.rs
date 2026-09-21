//! Mercury API evidence. Amounts are signed USD; posting dates and provider IDs
//! identify booked movements. Keep provider objects intact for later review.
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::Value;
use std::str::FromStr;

use super::{ParseOutput, ParsedLine};
use crate::{Error, Result};

pub const SOURCE: &str = "mercury_json";
pub const CHANNEL: &str = "mercury";

pub fn is_file(content: &[u8]) -> bool {
    serde_json::from_slice::<Value>(content).is_ok_and(|v| v["channel"] == CHANNEL)
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn is_credit(file: &Value) -> Result<bool> {
    match file.get("account_type") {
        None => Ok(false), // Files from the first depository channel did not carry this field.
        Some(Value::String(kind)) if kind == "depository" => Ok(false),
        Some(Value::String(kind)) if kind == "credit" => Ok(true),
        _ => Err(Error::Parse("unsupported Mercury account_type".into())),
    }
}

pub(crate) fn validate_destination(content: &[u8], account: &crate::model::Account) -> Result<()> {
    if is_credit(&serde_json::from_slice(content)?)? && account.r#type != crate::model::AccountType::Liability {
        return Err(Error::Invalid("Mercury IO evidence requires a USD liability account".into()));
    }
    Ok(())
}

pub fn parse(content: &[u8]) -> Result<ParseOutput> {
    let file: Value = serde_json::from_slice(content)?;
    if file["channel"] != CHANNEL || file["version"] != 1 {
        return Err(Error::Parse("expected a Mercury file with version 1".into()));
    }
    is_credit(&file)?;
    let identity = text(&file["account"], "id");
    uuid::Uuid::parse_str(identity).map_err(|_| Error::Parse("Mercury file needs an account UUID".into()))?;
    if file["currency"] != "USD" {
        return Err(Error::Parse("Mercury API evidence must use USD".into()));
    }
    let transactions = file["transactions"].as_array().ok_or_else(|| Error::Parse("Mercury transactions must be an array".into()))?;
    let mut out = ParseOutput { detected_source: SOURCE.into(), currency: "USD".into(), account_ref: identity.into(), ..Default::default() };
    for tx in transactions {
        // Even non-posting evidence must belong to this account.
        if text(tx, "accountId") != identity {
            return Err(Error::Parse("Mercury transaction accountId does not match its envelope".into()));
        }
        let status = text(tx, "status");
        if status == "pending" {
            out.skipped_records += 1;
            continue;
        }
        let mut line = ParsedLine { currency: "USD".into(), reference: text(tx, "id").into(), raw: tx.clone(), ..Default::default() };
        let parsed = (|| -> Result<()> {
            if status != "sent" {
                return Err(Error::Parse(format!("non-booked Mercury status {status:?}")));
            }
            uuid::Uuid::parse_str(&line.reference).map_err(|_| Error::Parse("missing or invalid Mercury transaction ID".into()))?;
            // arbitrary_precision preserves JSON decimals without a binary float conversion.
            let number = tx["amount"].as_number().ok_or_else(|| Error::Parse("Mercury amount must be a JSON number".into()))?;
            let amount = Decimal::from_str(&number.to_string()).map_err(|_| Error::Parse("invalid Mercury decimal amount".into()))?;
            if amount.is_zero() || amount.round_dp(2) != amount {
                return Err(Error::Parse("Mercury amount must be nonzero whole cents".into()));
            }
            let date = DateTime::parse_from_rfc3339(text(tx, "postedAt")).map_err(|_| Error::Parse("missing or invalid Mercury postedAt".into()))?;
            line.date = Some(date.with_timezone(&Utc).date_naive());
            line.amount = Some(amount);
            line.description = [text(tx, "counterpartyName"), text(tx, "bankDescription"), text(tx, "externalMemo")].join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
            Ok(())
        })();
        if let Err(error) = parsed {
            line.skip = Some(error.to_string());
        }
        out.lines.push(line);
    }
    out.period_from = out.lines.iter().filter(|l| l.skip.is_none()).filter_map(|l| l.date).min();
    out.period_to = out.lines.iter().filter(|l| l.skip.is_none()).filter_map(|l| l.date).max();
    if out.skipped_records > 0 {
        out.warnings.push(format!("{} pending Mercury transactions were left out", out.skipped_records));
    }
    // An account's live balance is not a closing balance for this historical file.
    Ok(out)
}

pub fn account_for_file(conn: &rusqlite::Connection, content: &[u8]) -> Result<Option<i64>> {
    use rusqlite::OptionalExtension;
    let Ok(parsed) = parse(content) else { return Ok(None) };
    let account: Option<Option<i64>> =
        conn.query_row("SELECT account_id FROM channel_connections WHERE channel = ?1 AND provider_account_id = ?2", rusqlite::params![CHANNEL, parsed.account_ref], |r| r.get(0)).optional()?;
    let Some(id) = account.flatten() else { return Ok(None) };
    let ledger = crate::accounts::get_account(conn, id)?;
    if ledger.commodity != "USD" || validate_destination(content, &ledger).is_err() {
        return Ok(None);
    }
    Ok(Some(id))
}

pub(crate) fn learn(conn: &rusqlite::Connection, parsed: &ParseOutput, account_id: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO channel_connections (channel, item_id, provider_account_id, currency, account_id, created_at) VALUES (?1, '', ?2, ?3, ?4, ?5)
         ON CONFLICT (channel, provider_account_id) DO UPDATE SET account_id = excluded.account_id",
        rusqlite::params![CHANNEL, parsed.account_ref, parsed.currency, account_id, crate::now_ts()],
    )?;
    Ok(())
}

#[derive(Debug)]
pub struct PullFile {
    pub account_ref: String,
    pub records: usize,
    pub path: Option<std::path::PathBuf>,
}

/// Publish a completely fetched account before recording its last successful pull.
/// Replays are safe: the import pipeline deduplicates provider transaction IDs.
pub fn publish_account(conn: &mut rusqlite::Connection, directory: &std::path::Path, account: &Value, transactions: Vec<Value>, dry_run: bool) -> Result<PullFile> {
    publish(conn, directory, account, transactions, dry_run, false)
}

/// Credit API movements already use ledger signs: charges negative, repayments positive.
pub fn publish_credit_account(conn: &mut rusqlite::Connection, directory: &std::path::Path, account: &Value, transactions: Vec<Value>, dry_run: bool) -> Result<PullFile> {
    publish(conn, directory, account, transactions, dry_run, true)
}

fn publish(conn: &mut rusqlite::Connection, directory: &std::path::Path, account: &Value, transactions: Vec<Value>, dry_run: bool, credit: bool) -> Result<PullFile> {
    use std::io::Write;
    let pulled_at = crate::now_ts();
    let records = transactions.len();
    let content = serde_json::to_vec(
        &serde_json::json!({"channel":CHANNEL,"version":1,"currency":"USD","account_type":if credit { "credit" } else { "depository" },"pulled_at":pulled_at,"account":account,"transactions":transactions}),
    )?;
    let parsed = parse(&content)?;
    let mut result = PullFile { account_ref: parsed.account_ref, records, path: None };
    if dry_run {
        return Ok(result);
    }
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    std::fs::create_dir_all(directory).map_err(|e| Error::Invalid(format!("create Mercury inbox: {e}")))?;
    let name = format!("mercury-{}.json", crate::new_uid());
    let target = directory.join(&name);
    let temporary = directory.join(format!(".{name}.part"));
    let write = (|| -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut output = options.open(&temporary)?;
        output.write_all(&content)?;
        output.sync_all()?;
        std::fs::rename(&temporary, &target)?;
        Ok(())
    })();
    if let Err(error) = write {
        let _ = std::fs::remove_file(&temporary);
        return Err(Error::Invalid(format!("publish Mercury evidence: {error}")));
    }
    tx.execute(
        "INSERT INTO channel_connections (channel, item_id, provider_account_id, provider_type, name, currency, cursor, last_pull_at, created_at)
         VALUES (?1, '', ?2, ?3, ?4, 'USD', '', ?5, ?5)
         ON CONFLICT (channel, provider_account_id) DO UPDATE SET provider_type = excluded.provider_type, name = excluded.name,
         currency = excluded.currency, cursor = '', last_pull_at = excluded.last_pull_at",
        rusqlite::params![CHANNEL, result.account_ref, if credit { "credit" } else { text(account, "kind") }, text(account, "name"), pulled_at],
    )?;
    tx.commit()?;
    result.path = Some(target);
    Ok(result)
}
