//! Account Tracker Pro backups (`.atb`): an `NSKeyedArchiver` binary plist holding
//! accounts, groups, transactions (with splits, foreign amounts and repeats), budgets and rates.
//!
//! Inspect first, then import with a mapping of accounts to entities. Every transaction becomes
//! a posted journal entry keyed by its Account Tracker id, so re-importing a newer backup only
//! adds what is new.

use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use chrono::{Datelike, Duration, NaiveDate, TimeZone, Utc, Weekday};
use plist::Value;
use rusqlite::{params, Connection, OptionalExtension};
use rust_decimal::Decimal;

use crate::accounts;
use crate::entities;
use crate::journal;
use crate::model::*;
use crate::money::{self, MinorUnits};
use crate::rates;
use crate::templates;
use crate::{new_uid, now_ts, Error, Result};

#[derive(Debug, Clone)]
pub struct AtAccount {
    pub id: i64,
    pub name: String,
    pub code: String,
    pub group: usize,
    pub hidden: bool,
    pub exclude: bool,
    pub opening: i64,
    pub balance: i64,
    pub reconciled: i64,
    pub min: i64,
    pub due: i64,
    pub closed: i64,
    pub todo: i64,
}

/// A per-occurrence edit of a repeat: a new amount, a new date, or a deletion.
/// A negative sequence applies the amount to that occurrence and all that follow.
#[derive(Debug, Clone)]
pub struct Override {
    pub sequence: i64,
    pub pence: Option<i64>,
    pub date: Option<NaiveDate>,
    pub deleted: bool,
}

#[derive(Debug, Clone)]
pub struct Repeat {
    pub unit: String,
    pub every: i64,
    pub end: String,
    pub after: i64,
    pub on: Option<NaiveDate>,
    pub weekend: String,
    pub overrides: Vec<Override>,
}

#[derive(Debug, Clone)]
pub struct AtTransaction {
    pub id: String,
    pub date: NaiveDate,
    pub pence: i64,
    pub from: i64,
    pub to: i64,
    pub category: String,
    pub details: String,
    pub notes: String,
    pub refund: bool,
    pub foreign: Option<i64>,
    pub code: Option<String>,
    pub splits: Vec<(String, i64)>,
    pub repeat: Option<Repeat>,
    pub raw: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct Backup {
    pub base_currency: String,
    pub timezone: String,
    pub exported_on: NaiveDate,
    pub rates: BTreeMap<String, f64>,
    pub groups: Vec<String>,
    pub accounts: Vec<AtAccount>,
    pub transactions: Vec<AtTransaction>,
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

fn class_name(objs: &[Value], dict: &plist::Dictionary) -> Option<String> {
    let uid = match dict.get("$class") {
        Some(Value::Uid(u)) => u.get() as usize,
        _ => return None,
    };
    match objs.get(uid) {
        Some(Value::Dictionary(d)) => d.get("$classname").and_then(|v| v.as_string()).map(|s| s.to_string()),
        _ => None,
    }
}

fn resolve(objs: &[Value], v: &Value, depth: usize) -> serde_json::Value {
    if depth > 64 {
        return serde_json::Value::Null;
    }
    match v {
        Value::Uid(u) => match objs.get(u.get() as usize) {
            Some(inner) => resolve(objs, inner, depth + 1),
            None => serde_json::Value::Null,
        },
        Value::Dictionary(d) => {
            let cls = class_name(objs, d).unwrap_or_default();
            match cls.as_str() {
                "NSMutableArray" | "NSArray" | "NSMutableSet" | "NSSet" | "NSMutableOrderedSet" | "NSOrderedSet" => {
                    let items = d.get("NS.objects").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                    serde_json::Value::Array(items.iter().map(|i| resolve(objs, i, depth + 1)).collect())
                }
                "NSMutableDictionary" | "NSDictionary" => {
                    let keys = d.get("NS.keys").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                    let vals = d.get("NS.objects").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                    let mut m = serde_json::Map::new();
                    for (k, val) in keys.iter().zip(vals.iter()) {
                        let key = match resolve(objs, k, depth + 1) {
                            serde_json::Value::String(s) => s,
                            other => other.to_string(),
                        };
                        m.insert(key, resolve(objs, val, depth + 1));
                    }
                    serde_json::Value::Object(m)
                }
                "NSMutableString" | "NSString" => serde_json::Value::String(d.get("NS.string").and_then(|v| v.as_string()).unwrap_or("").to_string()),
                "NSDate" => serde_json::json!({"__date": d.get("NS.time").and_then(|v| v.as_real()).unwrap_or(0.0)}),
                "NSNull" => serde_json::Value::Null,
                "NSMutableData" | "NSData" => serde_json::json!({"__data": d.get("NS.data").and_then(|v| v.as_data()).map(|b| b.len()).unwrap_or(0)}),
                _ => {
                    let mut m = serde_json::Map::new();
                    for (k, val) in d.iter() {
                        if k == "$class" {
                            continue;
                        }
                        m.insert(k.clone(), resolve(objs, val, depth + 1));
                    }
                    serde_json::Value::Object(m)
                }
            }
        }
        Value::Array(a) => serde_json::Value::Array(a.iter().map(|i| resolve(objs, i, depth + 1)).collect()),
        Value::Boolean(b) => serde_json::Value::Bool(*b),
        Value::Integer(i) => serde_json::json!(i.as_signed().unwrap_or(0)),
        Value::Real(r) => serde_json::json!(*r),
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::Data(d) => serde_json::json!({"__data": d.len()}),
        Value::Date(d) => {
            let st: std::time::SystemTime = (*d).into();
            let secs = st.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
            serde_json::json!({"__unix": secs})
        }
        _ => serde_json::Value::Null,
    }
}

fn ns_date_to_local(v: &serde_json::Value, tz: &chrono_tz::Tz) -> Option<NaiveDate> {
    let secs = v.get("__date")?.as_f64()?;
    let epoch = Utc.with_ymd_and_hms(2001, 1, 1, 0, 0, 0).single()?;
    let dt = epoch + Duration::milliseconds((secs * 1000.0) as i64);
    Some(dt.with_timezone(tz).date_naive())
}

fn as_i64(v: &serde_json::Value) -> i64 {
    match v {
        serde_json::Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.round() as i64)).unwrap_or(0),
        serde_json::Value::Bool(b) => *b as i64,
        serde_json::Value::String(s) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

fn as_str(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub fn parse(content: &[u8]) -> Result<Backup> {
    let value = Value::from_reader(Cursor::new(content)).map_err(|e| Error::Parse(format!("not an Account Tracker backup: {e}")))?;
    let top = value.as_dictionary().ok_or_else(|| Error::Parse("backup is not a keyed archive".into()))?;
    let objs = top.get("$objects").and_then(|v| v.as_array()).ok_or_else(|| Error::Parse("backup has no $objects".into()))?;
    let root_uid = top.get("$top").and_then(|v| v.as_dictionary()).and_then(|d| d.get("root")).ok_or_else(|| Error::Parse("backup has no root".into()))?;
    let root = resolve(objs, root_uid, 0);
    let obj = root.as_object().ok_or_else(|| Error::Parse("backup root is not a dictionary".into()))?;
    let timezone = obj.get("timezone").map(as_str).filter(|s| !s.is_empty()).unwrap_or_else(|| "Europe/Lisbon".into());
    let tz: chrono_tz::Tz = timezone.parse().unwrap_or(chrono_tz::Europe::Lisbon);
    let base_currency = obj.get("code").map(as_str).filter(|s| !s.is_empty()).unwrap_or_else(|| "USD".into());
    let exported_on = obj
        .get("timestamp")
        .and_then(|v| v.as_f64())
        .and_then(|ms| Utc.timestamp_millis_opt(ms as i64).single())
        .map(|dt| dt.with_timezone(&tz).date_naive())
        .or_else(|| obj.get("backedup").and_then(|v| ns_date_to_local(v, &tz)))
        .unwrap_or_else(crate::today);
    let mut rates = BTreeMap::new();
    if let Some(r) = obj.get("rates").and_then(|v| v.as_object()) {
        for (k, v) in r {
            if let Some(f) = v.as_f64() {
                rates.insert(k.to_ascii_uppercase(), f);
            }
        }
    }
    let groups: Vec<String> = obj.get("groups").and_then(|v| v.as_array()).map(|a| a.iter().map(|g| g.get("name").map(as_str).unwrap_or_default()).collect()).unwrap_or_default();
    let mut accounts = Vec::new();
    for a in obj.get("accounts").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
        let g = |k: &str| a.get(k).map(as_i64).unwrap_or(0);
        accounts.push(AtAccount {
            id: g("id"),
            name: a.get("name").map(as_str).unwrap_or_default(),
            code: a.get("code").map(as_str).unwrap_or_else(|| base_currency.clone()).to_ascii_uppercase(),
            group: g("group").max(0) as usize,
            hidden: g("hidden") != 0,
            exclude: g("exclude") != 0,
            opening: g("opening"),
            balance: g("balance"),
            reconciled: g("reconciled"),
            min: g("min"),
            due: g("due"),
            closed: g("closed"),
            todo: g("todo"),
        });
    }
    let mut transactions = Vec::new();
    for t in obj.get("transactions").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
        let g = |k: &str| t.get(k).map(as_i64).unwrap_or(0);
        let Some(date) = t.get("date").and_then(|v| ns_date_to_local(v, &tz)) else { continue };
        let id = match t.get("id") {
            Some(serde_json::Value::Number(n)) => n.as_f64().map(|f| format!("{}", f as i64)).unwrap_or_default(),
            Some(v) => as_str(v),
            None => String::new(),
        };
        let splits: Vec<(String, i64)> = t.get("splits").and_then(|v| v.as_object()).map(|m| m.iter().map(|(k, v)| (k.clone(), as_i64(v))).collect()).unwrap_or_default();
        let repeat = t.get("repeat").map(as_str).filter(|s| !s.is_empty()).map(|unit| Repeat {
            unit,
            every: g("every").max(1),
            end: t.get("end").map(as_str).unwrap_or_else(|| "Never".into()),
            after: g("after"),
            on: t.get("on").and_then(|v| ns_date_to_local(v, &tz)),
            weekend: t.get("weekend").map(as_str).unwrap_or_else(|| "None".into()),
            overrides: t
                .get("overrides")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .map(|o| {
                            let date_raw = o.get("date").map(as_i64);
                            Override {
                                sequence: o.get("sequence").map(as_i64).unwrap_or(0),
                                pence: o.get("pence").map(as_i64),
                                date: date_raw.filter(|d| *d > 0).and_then(|d| NaiveDate::parse_from_str(&d.to_string(), "%Y%m%d").ok()),
                                deleted: date_raw == Some(0),
                            }
                        })
                        .collect()
                })
                .unwrap_or_default(),
        });
        let mut raw = t.clone();
        raw["date"] = serde_json::json!(date.to_string());
        transactions.push(AtTransaction {
            id,
            date,
            pence: g("pence"),
            from: g("from"),
            to: g("to"),
            category: t.get("category").map(as_str).unwrap_or_default(),
            details: t.get("details").map(as_str).unwrap_or_default(),
            notes: t.get("notes").map(as_str).unwrap_or_default(),
            refund: g("refund") != 0,
            foreign: t.get("foreign").map(as_i64).filter(|v| *v != 0),
            code: t.get("code").map(as_str).filter(|s| !s.is_empty()).map(|s| s.to_ascii_uppercase()),
            splits,
            repeat,
            raw,
        });
    }
    transactions.sort_by(|a, b| a.date.cmp(&b.date).then(a.id.cmp(&b.id)));
    Ok(Backup { base_currency, timezone, exported_on, rates, groups, accounts, transactions })
}

// ---------------------------------------------------------------------------
// Recurrence
// ---------------------------------------------------------------------------

fn add_months(d: NaiveDate, months: i64) -> NaiveDate {
    let total = d.year() as i64 * 12 + (d.month0() as i64) + months;
    let year = total.div_euclid(12) as i32;
    let month = total.rem_euclid(12) as u32 + 1;
    let last = NaiveDate::from_ymd_opt(year, month + 1, 1).or_else(|| NaiveDate::from_ymd_opt(year + 1, 1, 1)).map(|n| n.pred_opt().unwrap()).unwrap();
    NaiveDate::from_ymd_opt(year, month, d.day().min(last.day())).unwrap()
}

fn shift_weekend(d: NaiveDate, rule: &str) -> NaiveDate {
    let rule = rule.to_ascii_lowercase();
    if rule.is_empty() || rule == "none" {
        return d;
    }
    let before = rule.starts_with("before") || rule.starts_with("friday");
    match (before, d.weekday()) {
        (true, Weekday::Sat) => d - Duration::days(1),
        (true, Weekday::Sun) => d - Duration::days(2),
        (false, Weekday::Sat) => d + Duration::days(2),
        (false, Weekday::Sun) => d + Duration::days(1),
        _ => d,
    }
}

/// Occurrences of a repeating transaction up to `until`: (sequence, date, pence).
pub fn occurrences(tx: &AtTransaction, until: NaiveDate) -> Vec<(i64, NaiveDate, i64)> {
    occurrences_with(tx, until, NEGATIVE_APPLIES_FORWARD.load(std::sync::atomic::Ordering::Relaxed))
}

/// Experiment switch: does a negative sequence change that occurrence and all following (true)
/// or that occurrence and all previous (false)?
pub static NEGATIVE_APPLIES_FORWARD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn occurrences_with(tx: &AtTransaction, until: NaiveDate, forward: bool) -> Vec<(i64, NaiveDate, i64)> {
    let Some(rep) = &tx.repeat else { return vec![(0, tx.date, tx.pence)] };
    let unit = rep.unit.to_lowercase();
    let mut out = Vec::new();
    let mut seq: i64 = 0;
    let max: i64 = if rep.end.eq_ignore_ascii_case("After") { rep.after.max(1) } else { 5000 };
    loop {
        let scheduled = if unit.starts_with("da") {
            tx.date + Duration::days(seq * rep.every)
        } else if unit.starts_with("week") {
            tx.date + Duration::days(7 * seq * rep.every)
        } else if unit.starts_with("fortnight") {
            tx.date + Duration::days(14 * seq * rep.every)
        } else if unit.starts_with("year") {
            add_months(tx.date, 12 * seq * rep.every)
        } else {
            add_months(tx.date, seq * rep.every)
        };
        let scheduled = shift_weekend(scheduled, &rep.weekend);
        if let Some(on) = rep.on {
            if rep.end.eq_ignore_ascii_case("On") && scheduled > on {
                break;
            }
        }
        if seq >= max {
            break;
        }
        let n = seq + 1;
        // Running amount from negative-sequence overrides at or before this occurrence.
        // `pence` is the current amount. A negative sequence -k records the amount that applied to
        // occurrences before k (the nearest boundary above n wins). Positive sequences are one-off amounts.
        let mut pence = if forward {
            rep.overrides.iter().filter(|o| o.sequence < 0 && -o.sequence <= n).max_by_key(|o| -o.sequence).and_then(|o| o.pence).unwrap_or(tx.pence)
        } else {
            rep.overrides.iter().filter(|o| o.sequence < 0 && -o.sequence > n).min_by_key(|o| -o.sequence).and_then(|o| o.pence).unwrap_or(tx.pence)
        };
        let mut date = scheduled;
        let mut deleted = false;
        for o in rep.overrides.iter().filter(|o| o.sequence == n) {
            if o.deleted {
                deleted = true;
            }
            if let Some(p) = o.pence {
                pence = p;
            }
            if let Some(d) = o.date {
                date = d;
            }
        }
        if scheduled > until && date > until {
            break;
        }
        if !deleted && pence != 0 && date <= until {
            out.push((n, date, pence));
        }
        seq += 1;
        if seq > 5000 {
            break;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Simulation: which reading of `pence` reproduces the app's balances?
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PenceSide {
    From,
    To,
}

/// Balance and transaction count per account under a reading, with repeats expanded to the export date.
pub fn simulate(b: &Backup, side: PenceSide, expand: bool) -> HashMap<i64, (i64, i64)> {
    let mut out: HashMap<i64, (i64, i64)> = b.accounts.iter().map(|a| (a.id, (a.opening, 0))).collect();
    let codes: HashMap<i64, &str> = b.accounts.iter().map(|a| (a.id, a.code.as_str())).collect();
    for tx in &b.transactions {
        let occ = if expand { occurrences(tx, b.exported_on) } else { vec![(0, tx.date, tx.pence)] };
        for (_, _, pence) in occ {
            let (from_amt, to_amt) = amounts_for(tx, pence, side, &codes);
            if tx.from != 0 {
                if let Some(e) = out.get_mut(&tx.from) {
                    e.0 -= from_amt;
                    e.1 += 1;
                }
            }
            if tx.to != 0 {
                if let Some(e) = out.get_mut(&tx.to) {
                    e.0 += to_amt;
                    e.1 += 1;
                }
            }
        }
    }
    out
}

/// (amount leaving `from`, amount arriving at `to`) in each account's own currency.
fn amounts_for(tx: &AtTransaction, pence: i64, side: PenceSide, codes: &HashMap<i64, &str>) -> (i64, i64) {
    let cross = tx.from != 0 && tx.to != 0 && codes.get(&tx.from) != codes.get(&tx.to);
    match (cross, tx.foreign) {
        (true, Some(foreign)) => match side {
            PenceSide::From => (pence, foreign),
            PenceSide::To => (foreign, pence),
        },
        _ => (pence, pence),
    }
}

pub fn best_side(b: &Backup) -> (PenceSide, usize, usize) {
    let score = |side| {
        let sim = simulate(b, side, true);
        b.accounts.iter().filter(|a| sim.get(&a.id).map(|(bal, _)| *bal != a.balance).unwrap_or(true)).count()
    };
    let f = score(PenceSide::From);
    let t = score(PenceSide::To);
    if t < f {
        (PenceSide::To, t, f)
    } else {
        (PenceSide::From, f, t)
    }
}

// ---------------------------------------------------------------------------
// Inspect
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
pub struct InspectedAccount {
    pub external_id: String,
    pub name: String,
    pub currency: String,
    pub group: String,
    pub hidden: bool,
    pub closed: bool,
    pub balance: Decimal,
    pub opening: Decimal,
    pub transactions: i64,
    pub credit_limit: Option<Decimal>,
    pub due_day: i64,
    pub exclude: bool,
    pub suggested_type: String,
    pub suggested_subtype: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct InspectedCategory {
    pub name: String,
    pub uses: i64,
    pub inflows: i64,
    pub refunds: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Inspection {
    pub accounts: Vec<InspectedAccount>,
    pub categories: Vec<InspectedCategory>,
    pub groups: Vec<String>,
    pub transactions: i64,
    pub first_date: Option<NaiveDate>,
    pub last_date: Option<NaiveDate>,
    pub base_currency: String,
    pub rates: BTreeMap<String, String>,
    pub recurring: i64,
    pub exported_on: NaiveDate,
    pub pence_side: String,
    pub balance_mismatches: usize,
}

fn pence_to_decimal(p: i64) -> Decimal {
    Decimal::new(p, 2)
}

pub fn suggest_type(a: &AtAccount, group: &str) -> (&'static str, &'static str) {
    let n = a.name.to_lowercase();
    let g = group.to_lowercase();
    if g.contains("credit") || n.ends_with(" cc") || n.contains(" cc ") || n.contains("card") {
        ("liability", "card")
    } else if n.contains("loan") || n.contains("mortgage") {
        ("liability", "loan")
    } else if n.contains("splitwise") || n.contains("owed") || n.contains("receivable") {
        ("asset", "receivable")
    } else if n.starts_with("wallet") || n.starts_with("cash") {
        ("asset", "cash")
    } else if g.contains("saving") {
        ("asset", "savings")
    } else {
        ("asset", "bank")
    }
}

pub fn inspect(content: &[u8]) -> Result<Inspection> {
    let b = parse(content)?;
    let (side, mismatches, _) = best_side(&b);
    let sim = simulate(&b, side, true);
    let mut accounts = Vec::new();
    for a in &b.accounts {
        let group = b.groups.get(a.group).cloned().unwrap_or_default();
        let (t, st) = suggest_type(a, &group);
        accounts.push(InspectedAccount {
            external_id: a.id.to_string(),
            name: a.name.clone(),
            currency: a.code.clone(),
            group: group.clone(),
            hidden: a.hidden,
            closed: a.closed > 0 || group.eq_ignore_ascii_case("Closed"),
            balance: pence_to_decimal(a.balance),
            opening: pence_to_decimal(a.opening),
            transactions: sim.get(&a.id).map(|(_, n)| *n).unwrap_or(0),
            credit_limit: if a.min < 0 { Some(pence_to_decimal(-a.min)) } else { None },
            due_day: a.due,
            exclude: a.exclude,
            suggested_type: t.into(),
            suggested_subtype: st.into(),
        });
    }
    let mut cats: BTreeMap<String, InspectedCategory> = BTreeMap::new();
    for t in &b.transactions {
        let names: Vec<String> = if t.splits.is_empty() { vec![t.category.clone()] } else { t.splits.iter().map(|(c, _)| c.clone()).collect() };
        for name in names {
            if name.is_empty() {
                continue;
            }
            let c = cats.entry(name.clone()).or_insert(InspectedCategory { name, uses: 0, inflows: 0, refunds: 0 });
            c.uses += 1;
            if t.from == 0 {
                if t.refund {
                    c.refunds += 1;
                } else {
                    c.inflows += 1;
                }
            }
        }
    }
    Ok(Inspection {
        accounts,
        categories: cats.into_values().collect(),
        groups: b.groups.clone(),
        transactions: b.transactions.len() as i64,
        first_date: b.transactions.first().map(|t| t.date),
        last_date: b.transactions.last().map(|t| t.date),
        base_currency: b.base_currency.clone(),
        rates: b.rates.iter().map(|(k, v)| (k.clone(), format!("{v}"))).collect(),
        recurring: b.transactions.iter().filter(|t| t.repeat.is_some()).count() as i64,
        exported_on: b.exported_on,
        pence_side: match side {
            PenceSide::From => "from".into(),
            PenceSide::To => "to".into(),
        },
        balance_mismatches: mismatches,
    })
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct Mapping {
    pub external_id: String,
    pub entity_id: i64,
    pub r#type: String,
    pub subtype: String,
    pub skip: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AccountCheck {
    pub name: String,
    pub expected: Decimal,
    pub actual: Decimal,
    pub ok: bool,
}

pub struct AtImportResult {
    pub import: Import,
    pub warnings: Vec<String>,
    pub checks: Vec<AccountCheck>,
    pub created: usize,
    pub skipped_existing: usize,
}

struct Target {
    account: Account,
    entity: Entity,
}

/// The placeholder an imported account sits under: the group name, or a typed variant when the
/// same group holds both assets and liabilities (Account Tracker groups are not typed).
fn group_placeholder(conn: &Connection, entity_id: i64, atype: AccountType, group: &str, currency: &str) -> Result<Account> {
    // Names are unique per parent and type, so one group name can hold an asset and a liability placeholder.
    let parent = accounts::ensure_account(conn, entity_id, atype, &[group], "placeholder", currency)?;
    if parent.placeholder {
        Ok(parent)
    } else {
        accounts::update_account(conn, parent.id, accounts::AccountUpdate { placeholder: Some(true), ..Default::default() })
    }
}

fn category_account(conn: &Connection, cache: &mut HashMap<(i64, bool, String), i64>, entity_id: i64, income: bool, name: &str, currency: &str) -> Result<i64> {
    let key = (entity_id, income, name.to_string());
    if let Some(id) = cache.get(&key) {
        return Ok(*id);
    }
    let id = if name.is_empty() {
        accounts::find_by_role(conn, entity_id, "suspense")?.id
    } else {
        let t = if income { AccountType::Income } else { AccountType::Expense };
        accounts::ensure_account(conn, entity_id, t, &[name], if income { "income" } else { "expense" }, currency)?.id
    };
    cache.insert(key, id);
    Ok(id)
}

/// Import a backup. Accounts are created (or reused by Account Tracker id), every transaction
/// becomes an entry, repeats are expanded to the export date and turned into scheduled templates.
pub fn import(conn: &mut Connection, content: &[u8], filename: &str, mappings: &[Mapping], default_entity_id: i64, expand_recurring: bool, schedules: bool) -> Result<AtImportResult> {
    let b = parse(content)?;
    let sum = super::checksum(content);
    let dup: Option<i64> = conn.query_row("SELECT id FROM imports WHERE source = 'account_tracker' AND checksum = ?1", [&sum], |r| r.get(0)).optional()?;
    if let Some(id) = dup {
        return Err(Error::Conflict(format!("this backup was already imported (import #{id})")));
    }
    let default_entity = entities::get_entity(conn, default_entity_id)?;
    let (side, mismatches, other) = best_side(&b);
    let mut warnings: Vec<String> = Vec::new();
    if mismatches > 0 {
        warnings.push(format!("{mismatches} account balances do not reproduce from the transactions alone (the other reading of cross-currency transfers gives {other}); the checks below show which"));
    }
    // Rates from the app, as of the export date.
    for (cur, rate) in &b.rates {
        if let Ok(d) = Decimal::try_from(*rate) {
            if !d.is_zero() && cur != &b.base_currency {
                let _ = rates::set_price(conn, cur, &b.base_currency, b.exported_on, d.round_dp(8), "import");
            }
        }
    }
    // Accounts.
    let map_by_id: HashMap<String, &Mapping> = mappings.iter().map(|m| (m.external_id.clone(), m)).collect();
    let mut targets: HashMap<i64, Target> = HashMap::new();
    let mut to_close: Vec<i64> = Vec::new();
    let mut entity_cache: HashMap<i64, Entity> = HashMap::new();
    entity_cache.insert(default_entity.id, default_entity.clone());
    for a in &b.accounts {
        let m = map_by_id.get(&a.id.to_string());
        if m.map(|m| m.skip).unwrap_or(false) {
            continue;
        }
        let entity_id = m.map(|m| m.entity_id).filter(|id| *id > 0).unwrap_or(default_entity.id);
        let entity = match entity_cache.get(&entity_id) {
            Some(e) => e.clone(),
            None => {
                let e = entities::get_entity(conn, entity_id)?;
                entity_cache.insert(entity_id, e.clone());
                e
            }
        };
        let group = b.groups.get(a.group).cloned().unwrap_or_else(|| "Accounts".into());
        let (st, ss) = suggest_type(a, &group);
        let t = m.map(|m| m.r#type.as_str()).filter(|s| !s.is_empty()).unwrap_or(st);
        let subtype = m.map(|m| m.subtype.as_str()).filter(|s| !s.is_empty()).unwrap_or(ss);
        let atype = AccountType::parse(t)?;
        // Reuse an account already carrying this Account Tracker id.
        let existing: Option<i64> = conn.query_row("SELECT id FROM accounts WHERE json_extract(external_ids, '$.account_tracker') = ?1", [a.id.to_string()], |r| r.get(0)).optional()?;
        let account = match existing {
            Some(id) => {
                let acc = accounts::get_account(conn, id)?;
                if acc.is_closed() {
                    accounts::close_account(conn, id, true)?;
                    to_close.push(id);
                }
                acc
            }
            None => {
                let parent = group_placeholder(conn, entity.id, atype, &group, &a.code)?;
                let created = accounts::create_account(
                    conn,
                    NewAccount {
                        entity_id: entity.id,
                        parent_id: Some(parent.id),
                        name: a.name.clone(),
                        r#type: Some(atype),
                        subtype: subtype.to_string(),
                        commodity: a.code.clone(),
                        placeholder: false,
                        in_net_worth: !a.exclude,
                        credit_limit: if a.min < 0 { Some(pence_to_decimal(-a.min)) } else { None },
                        due_day: if (1..=31).contains(&a.due) { Some(a.due as i32) } else { None },
                        external_ids: Some(serde_json::json!({"account_tracker": a.id.to_string()})),
                        notes: format!("Imported from Account Tracker group {group}"),
                        ..Default::default()
                    },
                )?;
                if a.closed > 0 || group.eq_ignore_ascii_case("Closed") {
                    to_close.push(created.id);
                }
                created
            }
        };
        targets.insert(a.id, Target { account, entity });
    }
    let codes: HashMap<i64, &str> = b.accounts.iter().map(|a| (a.id, a.code.as_str())).collect();
    // Opening balances, dated the day before the account's first transaction.
    let first_dates: HashMap<i64, NaiveDate> = {
        let mut m: HashMap<i64, NaiveDate> = HashMap::new();
        for t in &b.transactions {
            for id in [t.from, t.to] {
                if id != 0 {
                    let e = m.entry(id).or_insert(t.date);
                    if t.date < *e {
                        *e = t.date;
                    }
                }
            }
        }
        m
    };
    for a in b.accounts.iter().filter(|a| a.opening != 0) {
        let Some(t) = targets.get(&a.id) else { continue };
        let ext = format!("at:opening:{}", a.id);
        let exists: Option<i64> = conn.query_row("SELECT id FROM postings WHERE external_id = ?1 LIMIT 1", [&ext], |r| r.get(0)).optional()?;
        if exists.is_some() {
            continue;
        }
        let date = first_dates.get(&a.id).map(|d| *d - Duration::days(1)).unwrap_or(b.exported_on);
        let opening = accounts::find_by_role(conn, t.entity.id, "opening_balance")?;
        let mut input = EntryInput::new(t.entity.id, date);
        input.payee = "Opening balance".into();
        input.description = format!("Balance carried into Account Tracker for {}", a.name);
        input.origin = "migration".into();
        input.postings.push(PostingInput::new(t.account.id, pence_to_decimal(a.opening)).external(Some(ext), None));
        input.postings.push(PostingInput::balancing(opening.id));
        if let Err(e) = journal::create_entry(conn, input) {
            warnings.push(format!("opening balance of {}: {e}", a.name));
        }
    }
    // The import record.
    let ts = now_ts();
    conn.execute(
        "INSERT INTO imports (uid, source, account_id, filename, checksum, status, period_from, period_to, lines_count, options, content, created_at)
         VALUES (?1, 'account_tracker', NULL, ?2, ?3, 'done', ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            new_uid(),
            filename,
            sum,
            b.transactions.first().map(|t| t.date.to_string()),
            b.transactions.last().map(|t| t.date.to_string()),
            b.transactions.len() as i64,
            serde_json::json!({"exported_on": b.exported_on.to_string(), "pence_side": format!("{side:?}"), "default_entity_id": default_entity.id, "mappings": mappings.len()}).to_string(),
            content,
            ts
        ],
    )?;
    let import_id = conn.last_insert_rowid();
    let mut cat_cache: HashMap<(i64, bool, String), i64> = HashMap::new();
    let mut created = 0usize;
    let mut skipped_existing = 0usize;
    let mut position = 0i32;
    let mut recurring_templates = 0usize;
    for tx in &b.transactions {
        let occ = if expand_recurring { occurrences(tx, b.exported_on) } else { vec![(0, tx.date, tx.pence)] };
        let from_t = targets.get(&tx.from);
        let to_t = targets.get(&tx.to);
        if tx.from != 0 && from_t.is_none() && tx.to != 0 && to_t.is_none() {
            continue;
        }
        if (tx.from == 0 && to_t.is_none()) || (tx.to == 0 && from_t.is_none()) {
            continue;
        }
        for (seq, date, pence) in occ {
            if pence == 0 {
                continue;
            }
            position += 1;
            let ext = if tx.repeat.is_some() { format!("at:{}#{}", tx.id, seq) } else { format!("at:{}", tx.id) };
            let exists: Option<i64> = conn.query_row("SELECT id FROM postings WHERE external_id = ?1 LIMIT 1", [&ext], |r| r.get(0)).optional()?;
            let mut line_amount: Option<Decimal> = None;
            let mut line_account: Option<i64> = None;
            let result: Result<JournalEntry> = if let Some(pid) = exists {
                skipped_existing += 1;
                let eid: i64 = conn.query_row("SELECT journal_entry_id FROM postings WHERE id = ?1", [pid], |r| r.get(0))?;
                journal::get_entry(conn, eid)
            } else {
                created += 1;
                post_transaction(conn, TransactionPosting { tx, pence, date, ext: &ext, side, codes: &codes, from_t, to_t }, &mut cat_cache, &mut line_amount, &mut line_account)
            };
            match result {
                Ok(entry) => {
                    // The row evidences the entry (it is your own capture), not the bank leg: bank statements still get to match it.
                    let posting_id: Option<i64> = None;
                    let mut raw = tx.raw.clone();
                    raw["_occurrence"] = serde_json::json!(seq);
                    // The line is in the commodity of the posting it evidences, so its posting carries the precision (D14).
                    let line_posting = line_account.and_then(|a| entry.postings.iter().find(|p| p.account_id == a));
                    let line_minor = match (line_amount, line_posting) {
                        (Some(v), Some(p)) => Some(MinorUnits::from_major(v, p.quantity.precision())?.minor()),
                        (Some(_), None) => return Err(Error::Invalid(format!("no posting on the account of line {ext}"))),
                        (None, _) => None,
                    };
                    conn.execute(
                        "INSERT INTO statement_lines (import_id, account_id, position, raw, date, amount, currency, description, reference, fingerprint, posting_id, journal_entry_id, status, note)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, '', ?10, ?11, 'created', '')",
                        params![
                            import_id,
                            line_account,
                            position,
                            raw.to_string(),
                            date.to_string(),
                            line_minor,
                            line_posting.map(|p| p.quantity.commodity().to_owned()).unwrap_or_default(),
                            tx.details,
                            ext,
                            posting_id,
                            entry.id
                        ],
                    )?;
                }
                Err(e) => {
                    warnings.push(format!("{} {} {}: {e}", date, tx.details, ext));
                    conn.execute(
                        "INSERT INTO statement_lines (import_id, account_id, position, raw, date, amount, currency, description, reference, fingerprint, status, note) VALUES (?1, NULL, ?2, ?3, ?4, NULL, '', ?5, ?6, '', 'error', ?7)",
                        params![import_id, position, tx.raw.to_string(), date.to_string(), tx.details, ext, e.to_string()],
                    )?;
                }
            }
        }
        // Scheduled template for repeats still running after the export date.
        if schedules && expand_recurring {
            if let Some(rep) = &tx.repeat {
                let future = occurrences(tx, b.exported_on + Duration::days(3660));
                let next = future.iter().map(|(_, d, _)| *d).find(|d| *d > b.exported_on);
                if let Some(next) = next {
                    if let Ok(t) = schedule_template(conn, tx, rep, next, from_t, to_t, &mut cat_cache) {
                        let _ = t;
                        recurring_templates += 1;
                    }
                }
            }
        }
    }
    for id in to_close {
        let _ = accounts::close_account(conn, id, false);
    }
    // Checks: the app's balance per account against the ledger.
    let mut checks = Vec::new();
    let balances = accounts::raw_balances(conn, None, Some(b.exported_on))?;
    for a in &b.accounts {
        let Some(t) = targets.get(&a.id) else { continue };
        let actual = balances.get(&t.account.id).map(|(q, _, _)| *q).unwrap_or(Decimal::ZERO);
        let expected = pence_to_decimal(a.balance);
        checks.push(AccountCheck { name: a.name.clone(), expected, actual, ok: actual == expected });
    }
    let errors = warnings.len();
    conn.execute(
        "UPDATE imports SET created_count = ?2, duplicate_count = ?3, error_count = ?4, options = json_set(options, '$.templates', ?5, '$.checks_failed', ?6) WHERE id = ?1",
        params![import_id, created as i64, skipped_existing as i64, errors as i64, recurring_templates as i64, checks.iter().filter(|c| !c.ok).count() as i64],
    )?;
    crate::audit::log(conn, "imports", import_id, "create", None, Some(serde_json::json!({"source": "account_tracker", "filename": filename, "created": created})))?;
    let (import, _) = super::get_import(conn, import_id)?;
    Ok(AtImportResult { import, warnings, checks, created, skipped_existing })
}

struct TransactionPosting<'a> {
    tx: &'a AtTransaction,
    pence: i64,
    date: NaiveDate,
    ext: &'a str,
    side: PenceSide,
    codes: &'a HashMap<i64, &'a str>,
    from_t: Option<&'a Target>,
    to_t: Option<&'a Target>,
}

fn post_transaction(
    conn: &mut Connection,
    request: TransactionPosting<'_>,
    cat_cache: &mut HashMap<(i64, bool, String), i64>,
    line_amount: &mut Option<Decimal>,
    line_account: &mut Option<i64>,
) -> Result<JournalEntry> {
    let TransactionPosting { tx, pence, date, ext, side, codes, from_t, to_t } = request;
    let original = tx.foreign.zip(tx.code.clone()).map(|(f, c)| serde_json::json!({"quantity": money::plain(pence_to_decimal(f)), "commodity": c}));
    let payee = tx.details.trim().to_string();
    match (from_t, to_t) {
        (Some(from), Some(to)) => {
            // Transfer.
            let (from_amt, to_amt) = amounts_for(tx, pence, side, codes);
            let fq = pence_to_decimal(from_amt);
            let tq = pence_to_decimal(to_amt);
            *line_amount = Some(-fq);
            *line_account = Some(from.account.id);
            let (entry, _) = journal::create_transfer(
                conn,
                journal::Transfer {
                    date,
                    from_account_id: from.account.id,
                    to_account_id: to.account.id,
                    from_quantity: fq,
                    to_quantity: Some(tq),
                    fee: None,
                    fee_account_id: None,
                    payee: if payee.is_empty() { format!("Transfer to {}", to.account.name) } else { payee },
                    notes: tx.notes.clone(),
                    from_contra_account_id: None,
                    to_contra_account_id: None,
                    status: EntryStatus::Posted,
                    origin: "migration".into(),
                    external: None,
                },
            )?;
            // Tag the bank postings with the Account Tracker id (the transfer builder does not take references).
            conn.execute("UPDATE postings SET external_id = ?2 WHERE journal_entry_id = ?1 AND account_id = ?3 AND external_id IS NULL", params![entry.id, ext, from.account.id])?;
            if let Some(cp) = entry.counterpart_id {
                conn.execute("UPDATE postings SET external_id = ?2 WHERE journal_entry_id = ?1 AND account_id = ?3 AND external_id IS NULL", params![cp, format!("{ext}:to"), to.account.id])?;
            } else {
                conn.execute("UPDATE postings SET external_id = ?2 WHERE journal_entry_id = ?1 AND account_id = ?3 AND external_id IS NULL", params![entry.id, format!("{ext}:to"), to.account.id])?;
            }
            journal::get_entry(conn, entry.id)
        }
        (Some(from), None) => {
            // Money out: expense (or uncategorised).
            let q = pence_to_decimal(pence);
            *line_amount = Some(-q);
            *line_account = Some(from.account.id);
            let mut input = EntryInput::new(from.entity.id, date);
            input.payee = payee;
            input.notes = tx.notes.clone();
            input.origin = "migration".into();
            let mut bank = PostingInput::new(from.account.id, -q).external(Some(ext.to_string()), None);
            if let Some(o) = &original {
                bank = bank.meta("original", o.clone());
            }
            input.postings.push(bank);
            let splits: Vec<(String, i64)> = if tx.splits.is_empty() { vec![(tx.category.clone(), pence)] } else { tx.splits.clone() };
            let n = splits.len();
            for (i, (cat, p)) in splits.iter().enumerate() {
                let acc = category_account(conn, cat_cache, from.entity.id, false, cat, &from.entity.currency)?;
                if i == n - 1 {
                    input.postings.push(PostingInput::balancing(acc).memo(cat));
                } else {
                    input.postings.push(PostingInput::valued(acc, pence_to_decimal(*p), &from.account.commodity).memo(cat));
                }
            }
            journal::create_entry(conn, input)
        }
        (None, Some(to)) => {
            // Money in: opening balance, refund, or income.
            let q = pence_to_decimal(pence);
            *line_amount = Some(q);
            *line_account = Some(to.account.id);
            let mut input = EntryInput::new(to.entity.id, date);
            input.payee = payee.clone();
            input.notes = tx.notes.clone();
            input.origin = "migration".into();
            let mut bank = PostingInput::new(to.account.id, q).external(Some(ext.to_string()), None);
            if let Some(o) = &original {
                bank = bank.meta("original", o.clone());
            }
            input.postings.push(bank);
            let is_opening = tx.category.is_empty() && tx.details.trim().eq_ignore_ascii_case("Initial Deposit");
            if is_opening {
                let opening = accounts::find_by_role(conn, to.entity.id, "opening_balance")?;
                input.postings.push(PostingInput::balancing(opening.id));
            } else {
                let splits: Vec<(String, i64)> = if tx.splits.is_empty() { vec![(tx.category.clone(), pence)] } else { tx.splits.clone() };
                let n = splits.len();
                for (i, (cat, p)) in splits.iter().enumerate() {
                    // A refund credits the expense account; other money in is income.
                    let acc = category_account(conn, cat_cache, to.entity.id, !tx.refund && !cat.is_empty(), cat, &to.entity.currency)?;
                    if i == n - 1 {
                        input.postings.push(PostingInput::balancing(acc).memo(cat));
                    } else {
                        input.postings.push(PostingInput::valued(acc, -pence_to_decimal(*p), &to.account.commodity).memo(cat));
                    }
                }
            }
            journal::create_entry(conn, input)
        }
        (None, None) => Err(Error::Invalid("transaction touches no known account".into())),
    }
}

fn schedule_template(
    conn: &mut Connection,
    tx: &AtTransaction,
    rep: &Repeat,
    next: NaiveDate,
    from_t: Option<&Target>,
    to_t: Option<&Target>,
    cat_cache: &mut HashMap<(i64, bool, String), i64>,
) -> Result<EntryTemplate> {
    let unit = rep.unit.to_lowercase();
    let freq = if unit.starts_with("day") {
        "DAILY"
    } else if unit.starts_with("week") || unit.starts_with("fortnight") {
        "WEEKLY"
    } else if unit.starts_with("year") {
        "YEARLY"
    } else {
        "MONTHLY"
    };
    let interval = if unit.starts_with("fortnight") { 2 * rep.every } else { rep.every };
    let mut rrule = format!("FREQ={freq};INTERVAL={interval}");
    if rep.end.eq_ignore_ascii_case("On") {
        if let Some(on) = rep.on {
            rrule.push_str(&format!(";UNTIL={}", on.format("%Y%m%d")));
        }
    }
    let q = pence_to_decimal(tx.pence);
    let (entity_id, lines) = match (from_t, to_t) {
        (Some(from), Some(to)) if from.entity.id == to.entity.id => (
            from.entity.id,
            vec![
                TemplateLine { account_id: from.account.id, method: "input".into(), value: Some(-q), of_line: None, memo: String::new(), label: from.account.name.clone() },
                TemplateLine { account_id: to.account.id, method: "balance".into(), value: None, of_line: None, memo: String::new(), label: String::new() },
            ],
        ),
        (Some(from), None) => {
            let acc = category_account(conn, cat_cache, from.entity.id, false, &tx.category, &from.entity.currency)?;
            (
                from.entity.id,
                vec![
                    TemplateLine { account_id: from.account.id, method: "input".into(), value: Some(-q), of_line: None, memo: String::new(), label: from.account.name.clone() },
                    TemplateLine { account_id: acc, method: "balance".into(), value: None, of_line: None, memo: tx.category.clone(), label: String::new() },
                ],
            )
        }
        (None, Some(to)) => {
            let acc = category_account(conn, cat_cache, to.entity.id, !tx.refund && !tx.category.is_empty(), &tx.category, &to.entity.currency)?;
            (
                to.entity.id,
                vec![
                    TemplateLine { account_id: to.account.id, method: "input".into(), value: Some(q), of_line: None, memo: String::new(), label: to.account.name.clone() },
                    TemplateLine { account_id: acc, method: "balance".into(), value: None, of_line: None, memo: tx.category.clone(), label: String::new() },
                ],
            )
        }
        _ => return Err(Error::Invalid("repeat between entities is not scheduled".into())),
    };
    let name = if tx.details.trim().is_empty() { tx.category.clone() } else { tx.details.trim().to_string() };
    let t = EntryTemplate {
        id: 0,
        uid: String::new(),
        entity_id,
        name: format!("{} ({})", name, rep.unit.to_lowercase()),
        payee: tx.details.trim().to_string(),
        description: String::new(),
        lines,
        rrule,
        starts_on: Some(next),
        next_on: Some(next),
        ends_on: if rep.end.eq_ignore_ascii_case("On") { rep.on } else { None },
        auto_post: false,
        lead_days: 0,
        version: 1,
        active: true,
        created_at: String::new(),
    };
    templates::save_template(conn, &t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monthly_occurrences_with_overrides() {
        let ov = |sequence: i64, pence: Option<i64>, date: Option<&str>, deleted: bool| Override { sequence, pence, date: date.map(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").unwrap()), deleted };
        let tx = AtTransaction {
            id: "1".into(),
            date: NaiveDate::from_ymd_opt(2026, 1, 31).unwrap(),
            pence: 1000,
            from: 1,
            to: 0,
            category: "Rent".into(),
            details: String::new(),
            notes: String::new(),
            refund: false,
            foreign: None,
            code: None,
            splits: vec![],
            repeat: Some(Repeat {
                unit: "Monthly".into(),
                every: 1,
                end: "After".into(),
                after: 5,
                on: None,
                weekend: "None".into(),
                overrides: vec![ov(2, Some(1200), None, false), ov(3, None, None, true), ov(-4, Some(900), None, false), ov(5, None, Some("2026-06-03"), false)],
            }),
            raw: serde_json::json!({}),
        };
        let occ = occurrences(&tx, NaiveDate::from_ymd_opt(2026, 12, 31).unwrap());
        assert_eq!(occ.len(), 4);
        assert_eq!(occ[0], (1, NaiveDate::from_ymd_opt(2026, 1, 31).unwrap(), 900));
        assert_eq!(occ[1], (2, NaiveDate::from_ymd_opt(2026, 2, 28).unwrap(), 1200));
        assert_eq!(occ[2], (4, NaiveDate::from_ymd_opt(2026, 4, 30).unwrap(), 1000));
        assert_eq!(occ[3], (5, NaiveDate::from_ymd_opt(2026, 6, 3).unwrap(), 1000));
        let capped = occurrences(&tx, NaiveDate::from_ymd_opt(2026, 2, 1).unwrap());
        assert_eq!(capped.len(), 1);
    }
}
