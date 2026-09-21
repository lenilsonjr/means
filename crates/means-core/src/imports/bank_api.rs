//! Read-only Inter PJ and Wise statement envelopes. HTTP/authentication live in the server.
use super::{ParseOutput, ParsedLine};
use crate::{Error, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    str::FromStr,
};

pub const INTER: &str = "inter_pj_json";
pub const WISE: &str = "wise_json";
pub fn source(content: &[u8]) -> Option<&'static str> {
    let v: Value = serde_json::from_slice(content).ok()?;
    match v["channel"].as_str()? {
        "inter_pj" => Some(INTER),
        "wise" => Some(WISE),
        _ => None,
    }
}
fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or_default()
}
fn invalid(message: &str) -> Error {
    Error::Parse(message.into())
}
fn decimal(v: &Value) -> Result<Decimal> {
    let value = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return Err(invalid("statement amount must be a decimal")),
    };
    Decimal::from_str(&value).map_err(|_| invalid("invalid statement decimal"))
}
pub fn parse(content: &[u8]) -> Result<ParseOutput> {
    let v: Value = serde_json::from_slice(content)?;
    let source = source(content).ok_or_else(|| invalid("unknown bank API envelope"))?;
    if v["version"] != 1 {
        return Err(invalid("unsupported bank API envelope version"));
    }
    let account = &v["account"];
    let identity = text(account, "id");
    let currency = text(account, "currency");
    if identity.is_empty() || currency.len() != 3 || currency == "XXX" || !currency.bytes().all(|c| c.is_ascii_uppercase()) {
        return Err(invalid("statement requires a stable account identity and currency"));
    }
    if source == INTER && (currency != "BRL" || !identity.bytes().all(|c| c.is_ascii_digit())) {
        return Err(invalid("Inter PJ statements require a BRL current-account number"));
    }
    if source == WISE {
        let profile = account["profile_id"].as_i64().filter(|id| *id > 0).ok_or_else(|| invalid("missing Wise profile ID"))?;
        let balance = account["balance_id"].as_i64().filter(|id| *id > 0).ok_or_else(|| invalid("missing Wise balance ID"))?;
        if identity != format!("{profile}:{balance}") {
            return Err(invalid("Wise account identity does not match profile/balance"));
        }
    }
    let rows = v["transactions"].as_array().ok_or_else(|| invalid("statement transactions must be an array"))?;
    let mut out = ParseOutput { detected_source: source.into(), account_ref: identity.into(), currency: currency.into(), ..Default::default() };
    let mut references = std::collections::HashSet::new();
    for row in rows {
        let (reference, date, amount, description) = if source == INTER {
            let amount = decimal(&row["valor"])?;
            if amount.is_sign_negative() {
                return Err(invalid("Inter amount must be unsigned; tipoOperacao supplies its sign"));
            }
            let signed = match text(row, "tipoOperacao") {
                "C" => amount,
                "D" => -amount,
                _ => return Err(invalid("unknown Inter operation type")),
            };
            (
                text(row, "idTransacao"),
                crate::parse_date(text(row, "dataTransacao"))?,
                signed,
                [text(row, "titulo"), text(row, "descricao"), text(&row["detalhes"], "nomePagador"), text(&row["detalhes"], "nomeRecebedor")]
                    .join(" ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        } else {
            if text(&row["amount"], "currency") != currency {
                return Err(invalid("Wise transaction currency differs from balance currency"));
            }
            let amount = decimal(&row["amount"]["value"])?;
            if !matches!((text(row, "type"), amount.is_sign_negative()), ("DEBIT", true) | ("CREDIT", false)) {
                return Err(invalid("Wise type and signed amount disagree"));
            }
            let date = DateTime::parse_from_rfc3339(text(row, "date")).map_err(|_| invalid("invalid Wise statement date"))?.with_timezone(&Utc).date_naive();
            let details = &row["details"];
            (
                text(row, "referenceNumber"),
                date,
                amount,
                [text(details, "description"), text(&details["merchant"], "name"), text(details, "senderName"), text(details, "paymentReference")]
                    .join(" ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        };
        if reference.is_empty() || !references.insert(reference) {
            return Err(invalid("missing or duplicate statement reference; account not published"));
        }
        if amount.is_zero() {
            return Err(invalid("zero bank movement requires manual review"));
        }
        let precision = crate::money::Money::from_major(amount, currency, crate::money::default_precision(currency))?;
        if precision.major() != amount {
            return Err(invalid("statement amount contains fractional minor units"));
        }
        out.lines.push(ParsedLine { date: Some(date), amount: Some(amount), currency: currency.into(), description, reference: reference.into(), raw: row.clone(), ..Default::default() });
    }
    out.period_from = out.lines.iter().filter_map(|r| r.date).min();
    out.period_to = out.lines.iter().filter_map(|r| r.date).max();
    Ok(out)
}
pub fn validate_destination(content: &[u8], account: &crate::Account) -> Result<()> {
    let parsed = parse(content)?;
    if account.r#type != crate::AccountType::Asset || parsed.currency != account.commodity {
        return Err(Error::Invalid("bank API statements require an asset account in the statement currency".into()));
    }
    Ok(())
}
pub fn account_for_file(conn: &Connection, content: &[u8]) -> Result<Option<i64>> {
    let Ok(parsed) = parse(content) else { return Ok(None) };
    let channel = parsed.detected_source.trim_end_matches("_json");
    let id: Option<Option<i64>> =
        conn.query_row("SELECT account_id FROM channel_connections WHERE channel=?1 AND provider_account_id=?2", params![channel, parsed.account_ref], |r| r.get(0)).optional()?;
    match id.flatten() {
        Some(id) if validate_destination(content, &crate::accounts::get_account(conn, id)?).is_ok() => Ok(Some(id)),
        _ => Ok(None),
    }
}
pub(crate) fn learn(conn: &Connection, parsed: &ParseOutput, account: i64) -> Result<()> {
    conn.execute("INSERT INTO channel_connections(channel,item_id,provider_account_id,currency,account_id,created_at) VALUES(?1,'',?2,?3,?4,?5) ON CONFLICT(channel,provider_account_id) DO UPDATE SET account_id=excluded.account_id",params![parsed.detected_source.trim_end_matches("_json"),parsed.account_ref,parsed.currency,account,crate::now_ts()])?;
    Ok(())
}
pub fn publish(conn: &mut Connection, directory: &Path, envelope: &Value, dry_run: bool) -> Result<Option<PathBuf>> {
    use std::io::Write;
    let content = serde_json::to_vec(envelope)?;
    let parsed = parse(&content)?;
    if dry_run {
        return Ok(None);
    }
    let channel = parsed.detected_source.trim_end_matches("_json");
    let tx = conn.transaction()?;
    std::fs::create_dir_all(directory).map_err(|_| Error::Invalid("cannot create statement inbox".into()))?;
    let name = format!("{channel}-{}.json", crate::new_uid());
    let target = directory.join(&name);
    let temporary = directory.join(format!(".{name}.part"));
    let write = (|| -> std::io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&temporary)?;
        file.write_all(&content)?;
        file.sync_all()?;
        std::fs::rename(&temporary, &target)
    })();
    if write.is_err() {
        let _ = std::fs::remove_file(&temporary);
        return Err(Error::Invalid("cannot publish statement file".into()));
    }
    tx.execute("INSERT INTO channel_connections(channel,item_id,provider_account_id,currency,name,provider_type,last_pull_at,created_at) VALUES(?1,'',?2,?3,?4,'bank',?5,?5) ON CONFLICT(channel,provider_account_id) DO UPDATE SET last_pull_at=excluded.last_pull_at,name=excluded.name",params![channel,parsed.account_ref,parsed.currency,text(&envelope["account"],"name"),crate::now_ts()])?;
    crate::audit::log(
        &tx,
        "channel_connections",
        tx.query_row("SELECT id FROM channel_connections WHERE channel=?1 AND provider_account_id=?2", params![channel, parsed.account_ref], |r| r.get(0))?,
        "pull",
        None,
        Some(json!({"file":name,"records":parsed.lines.len(),"booked_from":envelope["booked_from"]})),
    )?;
    tx.commit()?;
    Ok(Some(target))
}
