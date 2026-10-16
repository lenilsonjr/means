//! The address and the flags of an entity.
//!
//! A flag is a free key with a scalar value, for example `sg.gst_registered = true`. By
//! convention, a key starts with the jurisdiction code in lowercase and a dot. The engine does
//! not check the key against a list.
//!
//! A flag is undated or dated. An undated flag lives only in `entities.flags` and applies on
//! every date. A dated flag has rows in `entity_flag_history`. Each row holds a value for a
//! period that starts on `effective_from` (inclusive) and ends on `effective_to` (exclusive).
//! When a key has history rows, a read on a date uses only those rows. `entities.flags` then
//! holds the value on the date of the last change, as a cache for simple readers.
//! Every change goes to the audit log with its values before and after.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::entities::{check_period, covers, date_at, get_entity, normalize_country, opt_date_at, overlaps};
use crate::{audit, now_ts, Error, Result};

/// The keys of an entity address.
pub const ADDRESS_KEYS: &[&str] = &["street", "street2", "city", "region", "postal_code", "country"];

/// One dated value of a flag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlagPeriod {
    pub id: i64,
    pub entity_id: i64,
    pub key: String,
    pub value: Value,
    pub effective_from: NaiveDate,
    /// Exclusive. `None` keeps the period open.
    pub effective_to: Option<NaiveDate>,
    pub created_at: String,
}

/// A change to one flag. An empty `value` removes the flag.
#[derive(Debug, Clone, PartialEq)]
pub struct FlagChange {
    pub key: String,
    /// The text of the value. `true` and `false` become booleans and an integer becomes a
    /// number. Any other text stays a string.
    pub value: String,
    /// The first day of the new value. `None` makes an undated change.
    pub from: Option<NaiveDate>,
}

/// Parse `KEY=VALUE` for an address. The key becomes lowercase.
pub fn parse_address_pair(s: &str) -> Result<(String, String)> {
    let (k, v) = s.split_once('=').ok_or_else(|| Error::Invalid(format!("expected KEY=VALUE, got {s:?}")))?;
    Ok((k.trim().to_ascii_lowercase(), v.trim().to_string()))
}

/// Parse `KEY=VALUE` for a flag. The key becomes lowercase.
pub fn parse_flag(s: &str, from: Option<NaiveDate>) -> Result<FlagChange> {
    let (k, v) = s.split_once('=').ok_or_else(|| Error::Invalid(format!("expected KEY=VALUE, got {s:?}")))?;
    Ok(FlagChange { key: normalize_key(k)?, value: v.trim().to_string(), from })
}

/// Check a flag key. A key has lowercase letters, digits, `_`, `.` and `-`.
pub fn normalize_key(key: &str) -> Result<String> {
    let key = key.trim().to_ascii_lowercase();
    if key.is_empty() || key.len() > 64 || !key.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')) {
        return Err(Error::Invalid(format!("a flag key has letters, digits, '_', '.' and '-', for example sg.gst_registered; got {key:?}")));
    }
    Ok(key)
}

/// Turn the text of a flag value into a JSON scalar.
pub fn flag_value(text: &str) -> Value {
    let text = text.trim();
    match text {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => match text.parse::<i64>() {
            // Keep leading zeros and a plus sign as text, for example a code "007".
            Ok(n) if n.to_string() == text => Value::from(n),
            _ => Value::String(text.to_string()),
        },
    }
}

pub fn get_address(conn: &Connection, entity_id: i64) -> Result<BTreeMap<String, String>> {
    let text: String = conn.query_row("SELECT address FROM entities WHERE id = ?1", [entity_id], |r| r.get(0)).optional()?.ok_or_else(|| Error::NotFound(format!("entity {entity_id}")))?;
    Ok(serde_json::from_str(&text)?)
}

/// Merge `KEY=VALUE` pairs into the address and audit the values before and after. An empty
/// value removes its key. Use this function inside a transaction.
pub fn set_address_in(conn: &Connection, entity_id: i64, pairs: &[(String, String)]) -> Result<BTreeMap<String, String>> {
    let before = get_address(conn, entity_id)?;
    let mut after = before.clone();
    for (k, v) in pairs {
        let key = k.trim().to_ascii_lowercase();
        if !ADDRESS_KEYS.contains(&key.as_str()) {
            return Err(Error::Invalid(format!("address key must be one of {}, got {key:?}", ADDRESS_KEYS.join(", "))));
        }
        let value = if key == "country" { normalize_country(v, "address country")? } else { v.trim().to_string() };
        if value.is_empty() {
            after.remove(&key);
        } else {
            after.insert(key, value);
        }
    }
    if after != before {
        conn.execute("UPDATE entities SET address = ?2, updated_at = ?3 WHERE id = ?1", params![entity_id, serde_json::to_string(&after)?, now_ts()])?;
        audit::log(conn, "entities", entity_id, "set_address", Some(serde_json::to_value(&before)?), Some(serde_json::to_value(&after)?))?;
    }
    Ok(after)
}

/// Change the address in a transaction of its own.
pub fn set_address(conn: &mut Connection, entity_id: i64, pairs: &[(String, String)]) -> Result<BTreeMap<String, String>> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let after = set_address_in(&tx, entity_id, pairs)?;
    tx.commit()?;
    Ok(after)
}

/// The stored `flags` column: the undated flags and the cached values of the dated flags.
fn stored_flags(conn: &Connection, entity_id: i64) -> Result<BTreeMap<String, Value>> {
    let text: String = conn.query_row("SELECT flags FROM entities WHERE id = ?1", [entity_id], |r| r.get(0)).optional()?.ok_or_else(|| Error::NotFound(format!("entity {entity_id}")))?;
    Ok(serde_json::from_str(&text)?)
}

const HISTORY_COLUMNS: &str = "id, entity_id, key, value, effective_from, effective_to, created_at";

fn row_to_period(r: &rusqlite::Row<'_>) -> rusqlite::Result<FlagPeriod> {
    let text: String = r.get(3)?;
    Ok(FlagPeriod {
        id: r.get(0)?,
        entity_id: r.get(1)?,
        key: r.get(2)?,
        value: serde_json::from_str(&text).unwrap_or(Value::String(text)),
        effective_from: date_at(r, 4)?,
        effective_to: opt_date_at(r, 5)?,
        created_at: r.get(6)?,
    })
}

fn get_period(conn: &Connection, id: i64) -> Result<FlagPeriod> {
    conn.query_row(&format!("SELECT {HISTORY_COLUMNS} FROM entity_flag_history WHERE id = ?1"), [id], row_to_period).optional()?.ok_or_else(|| Error::NotFound(format!("flag period {id}")))
}

/// Every dated value of the flags of an entity, by key and start date.
pub fn flag_history(conn: &Connection, entity_id: i64) -> Result<Vec<FlagPeriod>> {
    get_entity(conn, entity_id)?;
    let mut stmt = conn.prepare(&format!("SELECT {HISTORY_COLUMNS} FROM entity_flag_history WHERE entity_id = ?1 ORDER BY key, effective_from, id"))?;
    let rows = stmt.query_map([entity_id], row_to_period)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// The flags of an entity on a date. An undated flag applies on every date. A dated flag
/// applies when one of its periods includes the date.
pub fn flags_as_of(conn: &Connection, entity_id: i64, on: NaiveDate) -> Result<BTreeMap<String, Value>> {
    let stored = stored_flags(conn, entity_id)?;
    let history = flag_history(conn, entity_id)?;
    let mut out: BTreeMap<String, Value> = stored.into_iter().filter(|(k, _)| !history.iter().any(|p| &p.key == k)).collect();
    for p in history.into_iter().filter(|p| covers(p.effective_from, p.effective_to, on)) {
        out.insert(p.key, p.value);
    }
    Ok(out)
}

/// The flags of an entity today.
pub fn current_flags(conn: &Connection, entity_id: i64) -> Result<BTreeMap<String, Value>> {
    flags_as_of(conn, entity_id, crate::today())
}

/// Apply flag changes and audit each one. Use this function inside a transaction.
///
/// - An undated change sets or removes an undated flag. A key with history needs a date.
/// - A dated change with a value adds a period from that date. An open period of the key that
///   starts earlier closes on that date. Any other overlap is a conflict.
/// - A dated change without a value closes the open period of the key on that date.
///
/// Returns the flags of the entity today.
pub fn set_flags_in(conn: &Connection, entity_id: i64, changes: &[FlagChange]) -> Result<BTreeMap<String, Value>> {
    get_entity(conn, entity_id)?;
    let before = stored_flags(conn, entity_id)?;
    let mut after = before.clone();
    for change in changes {
        let key = normalize_key(&change.key)?;
        let history: Vec<FlagPeriod> = flag_history(conn, entity_id)?.into_iter().filter(|p| p.key == key).collect();
        match change.from {
            None => {
                if !history.is_empty() {
                    return Err(Error::Conflict(format!("flag {key} has dated values; give a start date with --from")));
                }
                if change.value.trim().is_empty() {
                    after.remove(&key);
                } else {
                    after.insert(key, flag_value(&change.value));
                }
            }
            Some(from) => {
                if change.value.trim().is_empty() {
                    let open = history.iter().find(|p| p.effective_to.is_none()).ok_or_else(|| Error::NotFound(format!("open dated value of flag {key}")))?;
                    close_period(conn, open, from)?;
                } else {
                    for p in &history {
                        if !overlaps((p.effective_from, p.effective_to), (from, None)) {
                            continue;
                        }
                        if p.effective_to.is_none() && p.effective_from < from {
                            close_period(conn, p, from)?;
                        } else {
                            return Err(Error::Conflict(format!("flag {key} already has a value from {} (period #{})", p.effective_from, p.id)));
                        }
                    }
                    let value = flag_value(&change.value);
                    conn.execute(
                        "INSERT INTO entity_flag_history (entity_id, key, value, effective_from, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![entity_id, key, value.to_string(), from.to_string(), now_ts()],
                    )?;
                    let saved = get_period(conn, conn.last_insert_rowid())?;
                    audit::log(conn, "entity_flag_history", saved.id, "create", None, Some(serde_json::to_value(&saved)?))?;
                }
                // Cache the value of today in the column.
                let today = crate::today();
                match flag_history(conn, entity_id)?.into_iter().find(|p| p.key == key && covers(p.effective_from, p.effective_to, today)) {
                    Some(p) => after.insert(key, p.value),
                    None => after.remove(&key),
                };
            }
        }
    }
    if after != before {
        conn.execute("UPDATE entities SET flags = ?2, updated_at = ?3 WHERE id = ?1", params![entity_id, serde_json::to_string(&after)?, now_ts()])?;
        audit::log(conn, "entities", entity_id, "set_flags", Some(serde_json::to_value(&before)?), Some(serde_json::to_value(&after)?))?;
    }
    current_flags(conn, entity_id)
}

/// Apply flag changes in a transaction of their own.
pub fn set_flags(conn: &mut Connection, entity_id: i64, changes: &[FlagChange]) -> Result<BTreeMap<String, Value>> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let after = set_flags_in(&tx, entity_id, changes)?;
    tx.commit()?;
    Ok(after)
}

fn close_period(conn: &Connection, p: &FlagPeriod, to: NaiveDate) -> Result<FlagPeriod> {
    check_period(p.effective_from, Some(to))?;
    conn.execute("UPDATE entity_flag_history SET effective_to = ?2 WHERE id = ?1", params![p.id, to.to_string()])?;
    let after = get_period(conn, p.id)?;
    audit::log(conn, "entity_flag_history", p.id, "close", Some(serde_json::to_value(p)?), Some(serde_json::to_value(&after)?))?;
    Ok(after)
}
