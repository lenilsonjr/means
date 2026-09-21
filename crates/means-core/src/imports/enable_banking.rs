//! Enable Banking evidence, independent of the connection/authorization workflow.
//! One file contains one stable account identity and one currency. API session UIDs
//! and transaction_id are deliberately not used as persistent evidence identities.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde_json::Value;
use std::str::FromStr;

use super::{ParseOutput, ParsedLine};
use crate::{Error, Result};

pub const SOURCE: &str = "enable_banking_json";
pub const CHANNEL: &str = "enable_banking";

pub fn is_file(content: &[u8]) -> bool {
    serde_json::from_slice::<Value>(content).is_ok_and(|v| v["channel"] == CHANNEL)
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn amount(value: &Value) -> Option<Decimal> {
    value.get("amount").and_then(Value::as_str).and_then(|s| Decimal::from_str(s).ok())
}

pub fn parse(content: &[u8]) -> Result<ParseOutput> {
    let file: Value = serde_json::from_slice(content).map_err(|e| Error::Parse(format!("invalid Enable Banking file: {e}")))?;
    if file["channel"] != CHANNEL || file["version"] != 1 {
        return Err(Error::Parse("expected an Enable Banking file with version 1".into()));
    }
    let account = &file["account"];
    let identity = text(account, "identification_hash");
    let currency = text(account, "currency");
    if identity.is_empty() || currency.len() != 3 || !currency.bytes().all(|b| b.is_ascii_uppercase()) || currency == "XXX" {
        return Err(Error::Parse("Enable Banking file needs an account identification_hash and a known currency; split multi-currency accounts into separate files".into()));
    }
    let transactions = file["transactions"].as_array().ok_or_else(|| Error::Parse("Enable Banking transactions must be an array".into()))?;
    let mut out = ParseOutput { detected_source: SOURCE.into(), currency: currency.into(), account_ref: format!("{identity}:{currency}"), ..Default::default() };
    for tx in transactions {
        let mut line = ParsedLine { currency: currency.into(), reference: text(tx, "entry_reference").into(), raw: tx.clone(), ..Default::default() };
        let status = text(tx, "status");
        if status == "PDNG" {
            out.skipped_records += 1;
            continue;
        }
        let parsed = (|| -> Result<()> {
            if status != "BOOK" {
                return Err(Error::Parse(format!("unrecognised or non-booked transaction status {status:?}")));
            }
            let flow = text(tx, "credit_debit_indicator");
            let magnitude = amount(&tx["transaction_amount"]).ok_or_else(|| Error::Parse("missing or invalid decimal-string transaction amount".into()))?;
            if text(&tx["transaction_amount"], "currency") != currency {
                return Err(Error::Parse("transaction currency does not match the file account currency".into()));
            }
            line.amount = Some(match flow {
                "DBIT" => -magnitude.abs(),
                "CRDT" => magnitude.abs(),
                _ => return Err(Error::Parse("missing or invalid credit_debit_indicator".into())),
            });
            // Booking date is the date this movement entered the bank's books.
            let date = text(tx, "booking_date");
            line.date = Some(NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|_| Error::Parse("missing or invalid booking_date".into()))?);
            let party = if flow == "DBIT" { &tx["creditor"] } else { &tx["debtor"] };
            let mut description = vec![text(party, "name").to_owned()];
            if let Some(remittance) = tx.get("remittance_information").and_then(Value::as_array) {
                for piece in remittance {
                    let piece = piece.as_str().ok_or_else(|| Error::Parse("remittance_information must contain strings".into()))?;
                    description.push(piece.into());
                }
            }
            line.description = description.join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
            if let Some(balance) = tx.get("balance_after_transaction").filter(|v| !v.is_null()) {
                if text(balance, "currency") == currency {
                    line.balance_after = Some(amount(balance).ok_or_else(|| Error::Parse("invalid decimal-string balance_after_transaction".into()))?);
                }
            }
            Ok(())
        })();
        if let Err(error) = parsed {
            line.skip = Some(error.to_string());
        }
        out.lines.push(line);
    }
    if out.skipped_records > 0 {
        out.warnings.push(format!("{} pending Enable Banking transactions were left out", out.skipped_records));
    }
    let missing_refs = out.lines.iter().filter(|l| l.skip.is_none() && l.reference.is_empty()).count();
    if missing_refs > 0 {
        out.warnings.push(format!("{missing_refs} booked transactions have no entry_reference; deduplication uses the import pipeline's date/amount/description fingerprint"));
    }
    let mut dated: Vec<_> = out.lines.iter().filter(|l| l.skip.is_none()).collect();
    dated.sort_by_key(|l| l.date);
    out.period_from = dated.first().and_then(|l| l.date);
    out.period_to = dated.last().and_then(|l| l.date);
    // Transactions on one date have no guaranteed chronological ordering. Only use a
    // closing balance when the latest date has exactly one transaction with a balance.
    let latest: Vec<_> = dated.iter().filter(|l| l.date == out.period_to).collect();
    if latest.len() == 1 && latest[0].balance_after.is_some() {
        out.closing_balance = latest[0].balance_after;
        out.closing_date = latest[0].date;
    }
    Ok(out)
}

/// Route by provider identity and currency, never a source-wide filename default.
pub fn account_for_file(conn: &rusqlite::Connection, content: &[u8]) -> Result<Option<i64>> {
    use rusqlite::OptionalExtension;
    let Ok(parsed) = parse(content) else { return Ok(None) };
    let account: Option<Option<i64>> =
        conn.query_row("SELECT account_id FROM channel_connections WHERE channel = ?1 AND provider_account_id = ?2", rusqlite::params![CHANNEL, parsed.account_ref], |r| r.get(0)).optional()?;
    Ok(account.flatten())
}

pub(crate) fn learn(conn: &rusqlite::Connection, parsed: &ParseOutput, account_id: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO channel_connections (channel, item_id, provider_account_id, currency, account_id, created_at) VALUES (?1, '', ?2, ?3, ?4, ?5)
         ON CONFLICT (channel, provider_account_id) DO UPDATE SET account_id = excluded.account_id",
        rusqlite::params![CHANNEL, parsed.account_ref, parsed.currency, account_id, crate::now_ts()],
    )?;
    Ok(())
}

#[derive(Debug, serde::Serialize)]
pub struct Session {
    pub id: String,
    pub bank: String,
    pub country: String,
    pub valid_until: String,
}

/// Save only the consent identifier after a successful code exchange.
pub fn save_session(conn: &rusqlite::Connection, bank: &str, country: &str, response: &Value) -> Result<Session> {
    let id = text(response, "session_id");
    uuid::Uuid::parse_str(id).map_err(|_| Error::Parse("Enable Banking returned an invalid session_id".into()))?;
    let valid_until = text(&response["access"], "valid_until");
    let expiry = chrono::DateTime::parse_from_rfc3339(valid_until).map_err(|_| Error::Parse("Enable Banking returned an invalid consent expiry".into()))?;
    if expiry <= chrono::Utc::now() {
        return Err(Error::Invalid("Enable Banking returned an expired consent; connect again".into()));
    }
    let accounts = response["accounts"].as_array().ok_or_else(|| Error::Parse("Enable Banking returned no accounts array".into()))?;
    if accounts.is_empty() {
        return Err(Error::Invalid("no accessible accounts: link each account to your production application in the Enable Banking Control Panel, then connect again".into()));
    }
    conn.execute(
        "INSERT INTO enable_banking_sessions (id, bank, country, valid_until, created_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (id) DO UPDATE SET valid_until = excluded.valid_until",
        rusqlite::params![id, bank, country, valid_until, crate::now_ts()],
    )?;
    Ok(Session { id: id.into(), bank: bank.into(), country: country.into(), valid_until: valid_until.into() })
}

pub fn sessions(conn: &rusqlite::Connection) -> Result<Vec<Session>> {
    let mut stmt = conn.prepare("SELECT id, bank, country, valid_until FROM enable_banking_sessions ORDER BY bank, country, created_at")?;
    let rows = stmt.query_map([], |r| Ok(Session { id: r.get(0)?, bank: r.get(1)?, country: r.get(2)?, valid_until: r.get(3)? }))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

#[derive(Debug)]
pub struct PullFile {
    pub account_ref: String,
    pub currency: String,
    pub records: usize,
    pub path: Option<std::path::PathBuf>,
}

fn known_currency(currency: &str) -> bool {
    currency.len() == 3 && currency != "XXX" && currency.bytes().all(|b| b.is_ascii_uppercase())
}

/// Publish only a completely fetched account. Files precede connection metadata so
/// a crash can replay evidence, but cannot advance past evidence that was never saved.
/// Full-history pulls keep no incremental date cursor.
/// The first hash must be the provider's primary identification_hash.
/// Other hashes remain evidence only. They need not identify an account uniquely.
pub fn publish_account(
    conn: &mut rusqlite::Connection,
    directory: &std::path::Path,
    session_id: &str,
    account: &Value,
    hashes: &[String],
    transactions: Vec<Value>,
    dry_run: bool,
) -> Result<Vec<PullFile>> {
    use std::io::Write;
    if hashes.is_empty() || hashes.iter().any(|h| h.trim().is_empty()) {
        return Err(Error::Parse("Enable Banking account needs a primary identification hash".into()));
    }
    let mut groups: std::collections::BTreeMap<String, Vec<Value>> = std::collections::BTreeMap::new();
    for record in transactions {
        let currency = text(&record["transaction_amount"], "currency");
        if !known_currency(currency) {
            return Err(Error::Parse("Enable Banking transaction has no known currency; the account was not published".into()));
        }
        groups.entry(currency.into()).or_default().push(record);
    }
    if groups.is_empty() && known_currency(text(account, "currency")) {
        groups.insert(text(account, "currency").into(), vec![]);
    }
    // A read transaction suffices for dry-run; it creates no folder or connection.
    let behavior = if dry_run { rusqlite::TransactionBehavior::Deferred } else { rusqlite::TransactionBehavior::Immediate };
    let tx = conn.transaction_with_behavior(behavior)?;
    let pulled_at = crate::now_ts();
    let mut files = Vec::new();
    for (currency, records) in groups {
        // Old alias rows must not route a new primary to another account.
        let account_ref = format!("{}:{currency}", hashes[0]);
        let count = records.len();
        if dry_run {
            files.push(PullFile { account_ref, currency, records: count, path: None });
            continue;
        }
        let hash = account_ref.strip_suffix(&format!(":{currency}")).ok_or_else(|| Error::Invalid("invalid saved Enable Banking account identity".into()))?;
        let file = serde_json::json!({
            "channel":CHANNEL,"version":1,"pulled_at":pulled_at,"session_id":session_id,
            "account":{"identification_hash":hash,"currency":currency,"uid":account["uid"],"name":account["name"]},
            "provider_account":account,"identification_hashes":hashes,"transactions":records
        });
        let content = serde_json::to_vec(&file)?;
        parse(&content)?;
        std::fs::create_dir_all(directory).map_err(|e| Error::Invalid(format!("create inbox: {e}")))?;
        let name = format!("enable-banking-{}.json", crate::new_uid());
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
            return Err(Error::Invalid(format!("publish Enable Banking evidence: {error}")));
        }
        tx.execute(
            "INSERT INTO channel_connections (channel, item_id, provider_account_id, provider_type, name, currency, cursor, last_pull_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', ?7, ?7)
             ON CONFLICT (channel, provider_account_id) DO UPDATE SET item_id = excluded.item_id, provider_type = excluded.provider_type,
             name = excluded.name, currency = excluded.currency, cursor = '', last_pull_at = excluded.last_pull_at",
            rusqlite::params![CHANNEL, session_id, account_ref, text(account, "cash_account_type"), text(account, "name"), currency, pulled_at],
        )?;
        files.push(PullFile { account_ref, currency, records: count, path: Some(target) });
    }
    tx.commit()?;
    Ok(files)
}
