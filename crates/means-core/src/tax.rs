//! Tax codes and tax events.
//!
//! A tax code is a dated rate version of an entity: a kind, a rate in basis points, an optional
//! tax account and the recoverable share. A period starts on `effective_from` (inclusive) and ends
//! on `effective_to` (exclusive).
//!
//! A tax event records, for one posting and one tax point, the base, the assessed tax, the
//! recoverable part and the withheld part in the functional currency. Signs follow the side of
//! the taxed posting: debit positive. Returns sum the events by code, kind and period. The mapping
//! of these sums to the boxes of a return stays outside the engine.
//!
//! Tax events follow the entry lifecycle. A void creates mirror events on the reversal, with
//! negated amounts and a link to the original events. A deleted draft deletes its events.

use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};

use crate::entities::{self, check_period, covers, date_at, opt_date_at, overlaps};
use crate::model::{Account, AccountType, Entity, TaxInput};
use crate::money::{self, Money};
use crate::{accounts, audit, new_uid, now_ts, Error, Result};

pub const TAX_KINDS: &[&str] = &["output", "input", "reverse_charge", "exempt", "out_of_scope", "withholding", "zero_rated"];
/// Kinds that carry no tax. Their codes must have a zero rate.
pub const ZERO_TAX_KINDS: &[&str] = &["exempt", "out_of_scope", "zero_rated"];
/// Kinds with a recoverable part. The other kinds record a zero recoverable amount.
pub const RECOVERABLE_KINDS: &[&str] = &["input", "reverse_charge"];
/// 100% in basis points.
pub const FULL_BP: i64 = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaxCode {
    pub id: i64,
    pub uid: String,
    pub entity_id: i64,
    pub code: String,
    pub name: String,
    pub kind: String,
    /// Basis points: 900 is 9%.
    pub rate_bp: i64,
    pub tax_account_id: Option<i64>,
    /// The recoverable share of the tax in basis points (input and reverse_charge kinds).
    pub recoverable_bp: i64,
    pub jurisdiction: String,
    pub effective_from: NaiveDate,
    /// Exclusive. `None` keeps the code open.
    pub effective_to: Option<NaiveDate>,
    pub active: bool,
    pub created_at: String,
}

impl TaxCode {
    /// The rate version that an event records: `CODE@effective_from`.
    pub fn rule_version(&self) -> String {
        format!("{}@{}", self.code, self.effective_from)
    }
}

#[derive(Debug, Clone, Default)]
pub struct NewTaxCode {
    pub entity_id: i64,
    pub code: String,
    pub name: String,
    pub kind: String,
    pub rate_bp: i64,
    pub tax_account_id: Option<i64>,
    /// Default: 10000 (all of the tax is recoverable).
    pub recoverable_bp: Option<i64>,
    pub jurisdiction: String,
    pub effective_from: NaiveDate,
}

const CODE_COLUMNS: &str = "id, uid, entity_id, code, name, kind, rate_bp, tax_account_id, recoverable_bp, jurisdiction, effective_from, effective_to, active, created_at";

fn row_to_code(r: &rusqlite::Row<'_>) -> rusqlite::Result<TaxCode> {
    Ok(TaxCode {
        id: r.get(0)?,
        uid: r.get(1)?,
        entity_id: r.get(2)?,
        code: r.get(3)?,
        name: r.get(4)?,
        kind: r.get(5)?,
        rate_bp: r.get(6)?,
        tax_account_id: r.get(7)?,
        recoverable_bp: r.get(8)?,
        jurisdiction: r.get(9)?,
        effective_from: date_at(r, 10)?,
        effective_to: opt_date_at(r, 11)?,
        active: r.get::<_, i64>(12)? == 1,
        created_at: r.get(13)?,
    })
}

fn normalize_code(code: &str) -> Result<String> {
    let code = code.trim().to_ascii_uppercase();
    if code.is_empty() || code.chars().any(char::is_whitespace) {
        return Err(Error::Invalid(format!("a tax code must be one word, such as SR, got {code:?}")));
    }
    Ok(code)
}

pub fn get_tax_code_by_id(conn: &Connection, id: i64) -> Result<TaxCode> {
    conn.query_row(&format!("SELECT {CODE_COLUMNS} FROM tax_codes WHERE id = ?1"), [id], row_to_code).optional()?.ok_or_else(|| Error::NotFound(format!("tax code {id}")))
}

/// Every version of every code of an entity, or only the active versions in effect on `as_of`.
pub fn list_tax_codes(conn: &Connection, entity_id: i64, as_of: Option<NaiveDate>) -> Result<Vec<TaxCode>> {
    entities::get_entity(conn, entity_id)?;
    let mut stmt = conn.prepare(&format!("SELECT {CODE_COLUMNS} FROM tax_codes WHERE entity_id = ?1 ORDER BY code, effective_from, id"))?;
    let all = stmt.query_map([entity_id], row_to_code)?.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(all.into_iter().filter(|c| as_of.is_none_or(|d| c.active && covers(c.effective_from, c.effective_to, d))).collect())
}

/// The version of a code in effect on `as_of`. An unknown code is `NotFound`. A code that is
/// closed, not yet open or inactive on the date is `Invalid`.
pub fn get_tax_code(conn: &Connection, entity_id: i64, code: &str, as_of: NaiveDate) -> Result<TaxCode> {
    let code = normalize_code(code)?;
    let versions: Vec<TaxCode> = list_tax_codes(conn, entity_id, None)?.into_iter().filter(|c| c.code == code).collect();
    if versions.is_empty() {
        return Err(Error::NotFound(format!("tax code {code} of entity {entity_id}")));
    }
    versions.into_iter().find(|c| c.active && covers(c.effective_from, c.effective_to, as_of)).ok_or_else(|| Error::Invalid(format!("tax code {code} is not active on {as_of}")))
}

/// Add a rate version of a code and audit it. A new version closes the open version of the
/// same code that starts earlier. Any other overlap is a conflict.
pub fn add_tax_code(conn: &mut Connection, new: NewTaxCode) -> Result<TaxCode> {
    let code = normalize_code(&new.code)?;
    let kind = new.kind.trim().to_ascii_lowercase();
    if !TAX_KINDS.contains(&kind.as_str()) {
        return Err(Error::Invalid(format!("tax kind must be one of {}, got {kind:?}", TAX_KINDS.join(", "))));
    }
    if !(0..=10 * FULL_BP).contains(&new.rate_bp) {
        return Err(Error::Invalid(format!("the rate must be 0 to {} basis points, got {}", 10 * FULL_BP, new.rate_bp)));
    }
    if ZERO_TAX_KINDS.contains(&kind.as_str()) && new.rate_bp != 0 {
        return Err(Error::Invalid(format!("a {kind} code carries no tax; set its rate to 0")));
    }
    let recoverable_bp = new.recoverable_bp.unwrap_or(FULL_BP);
    if !(0..=FULL_BP).contains(&recoverable_bp) {
        return Err(Error::Invalid(format!("the recoverable share must be 0 to {FULL_BP} basis points, got {recoverable_bp}")));
    }
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let entity = entities::get_entity(&tx, new.entity_id)?;
    if let Some(account_id) = new.tax_account_id {
        check_tax_account(&tx, &entity, &accounts::get_account(&tx, account_id)?)?;
    }
    let versions: Vec<TaxCode> = list_tax_codes(&tx, entity.id, None)?.into_iter().filter(|c| c.code == code).collect();
    for v in &versions {
        if v.effective_to.is_none() && v.effective_from < new.effective_from {
            continue;
        }
        if overlaps((v.effective_from, v.effective_to), (new.effective_from, None)) {
            return Err(Error::Conflict(format!("tax code {code} already has a version from {} that overlaps {}", v.effective_from, new.effective_from)));
        }
    }
    if let Some(open) = versions.iter().find(|v| v.effective_to.is_none() && v.effective_from < new.effective_from) {
        close_version(&tx, open, new.effective_from)?;
    }
    tx.execute(
        "INSERT INTO tax_codes (uid, entity_id, code, name, kind, rate_bp, tax_account_id, recoverable_bp, jurisdiction, effective_from, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            new_uid(),
            entity.id,
            code,
            new.name.trim(),
            kind,
            new.rate_bp,
            new.tax_account_id,
            recoverable_bp,
            new.jurisdiction.trim().to_ascii_uppercase(),
            new.effective_from.to_string(),
            now_ts()
        ],
    )?;
    let saved = get_tax_code_by_id(&tx, tx.last_insert_rowid())?;
    audit::log(&tx, "tax_codes", saved.id, "create", None, Some(serde_json::to_value(&saved)?))?;
    tx.commit()?;
    Ok(saved)
}

/// End the open version of a code on `to` (exclusive) and audit the change.
pub fn close_tax_code(conn: &mut Connection, entity_id: i64, code: &str, to: NaiveDate) -> Result<TaxCode> {
    let code = normalize_code(code)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let open = list_tax_codes(&tx, entity_id, None)?
        .into_iter()
        .find(|c| c.code == code && c.effective_to.is_none())
        .ok_or_else(|| Error::NotFound(format!("tax code {code} of entity {entity_id} has no open version")))?;
    let saved = close_version(&tx, &open, to)?;
    tx.commit()?;
    Ok(saved)
}

fn close_version(conn: &Connection, open: &TaxCode, to: NaiveDate) -> Result<TaxCode> {
    check_period(open.effective_from, Some(to))?;
    conn.execute("UPDATE tax_codes SET effective_to = ?2 WHERE id = ?1", params![open.id, to.to_string()])?;
    let saved = get_tax_code_by_id(conn, open.id)?;
    audit::log(conn, "tax_codes", open.id, "close", Some(serde_json::to_value(open)?), Some(serde_json::to_value(&saved)?))?;
    Ok(saved)
}

fn check_tax_account(conn: &Connection, entity: &Entity, account: &Account) -> Result<()> {
    if account.entity_id != entity.id {
        return Err(Error::Invalid(format!("tax account {} belongs to another entity", account.path)));
    }
    let has_children: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM accounts WHERE parent_id = ?1)", [account.id], |r| r.get(0))?;
    if account.placeholder || has_children {
        return Err(Error::Invalid(format!("tax account {} groups other accounts; use a leaf account", account.path)));
    }
    if account.commodity != entity.currency {
        return Err(Error::Invalid(format!("tax account {} holds {}; it must hold the functional currency {}", account.path, account.commodity, entity.currency)));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaxEvent {
    pub id: i64,
    pub uid: String,
    pub entity_id: i64,
    pub entry_id: i64,
    pub entry_status: String,
    pub posting_id: Option<i64>,
    pub tax_code_id: i64,
    pub code: String,
    pub kind: String,
    pub rate_bp: i64,
    pub tax_point: NaiveDate,
    pub base: Money,
    pub tax: Money,
    pub recoverable: Money,
    pub withheld: Money,
    pub native_base: Option<Money>,
    pub native_tax: Option<Money>,
    pub party_id: Option<i64>,
    pub document_ref: String,
    pub rule_version: String,
    pub corrects_event_id: Option<i64>,
    pub metadata: serde_json::Value,
    pub created_at: String,
}

/// A tax event given directly. Amounts are signed major units of the functional currency.
#[derive(Debug, Clone, Default)]
pub struct TaxEventInput {
    pub entity_id: i64,
    pub entry_id: i64,
    pub posting_id: Option<i64>,
    pub code: String,
    pub tax_point: NaiveDate,
    pub base_amount: Decimal,
    pub tax_amount: Decimal,
    /// Default: the tax times the recoverable share of the code (input and reverse_charge kinds).
    pub recoverable_amount: Option<Decimal>,
    /// Default: the tax for a withholding code, else zero.
    pub withheld_amount: Option<Decimal>,
    /// Must be the functional currency of the entity.
    pub commodity: String,
    pub native_base: Option<Decimal>,
    pub native_tax: Option<Decimal>,
    pub native_commodity: Option<String>,
    pub party_id: Option<i64>,
    pub document_ref: String,
    /// Default: the rate version of the code, `CODE@effective_from`.
    pub rule_version: Option<String>,
    pub corrects_event_id: Option<i64>,
    pub metadata: serde_json::Value,
}

/// One row to insert. Amounts are minor units.
struct EventRow {
    entity_id: i64,
    entry_id: i64,
    posting_id: Option<i64>,
    tax_code_id: i64,
    tax_point: NaiveDate,
    base: i64,
    tax: i64,
    recoverable: i64,
    withheld: i64,
    commodity: String,
    native: Option<(i64, i64, String)>,
    party_id: Option<i64>,
    document_ref: String,
    rule_version: String,
    corrects_event_id: Option<i64>,
    metadata: serde_json::Value,
}

fn insert_event(conn: &Connection, row: &EventRow) -> Result<i64> {
    conn.execute(
        "INSERT INTO tax_events (uid, entity_id, entry_id, posting_id, tax_code_id, tax_point, base_amount, tax_amount, recoverable_amount, withheld_amount, commodity,
                                 native_base, native_tax, native_commodity, party_id, document_ref, rule_version, corrects_event_id, metadata, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
        params![
            new_uid(),
            row.entity_id,
            row.entry_id,
            row.posting_id,
            row.tax_code_id,
            row.tax_point.to_string(),
            row.base,
            row.tax,
            row.recoverable,
            row.withheld,
            row.commodity,
            row.native.as_ref().map(|n| n.0),
            row.native.as_ref().map(|n| n.1),
            row.native.as_ref().map(|n| n.2.clone()),
            row.party_id,
            row.document_ref.trim(),
            row.rule_version,
            row.corrects_event_id,
            row.metadata.to_string(),
            now_ts()
        ],
    )?;
    let id = conn.last_insert_rowid();
    audit::log(
        conn,
        "tax_events",
        id,
        "record",
        None,
        Some(
            serde_json::json!({"entry_id": row.entry_id, "tax_code_id": row.tax_code_id, "tax_point": row.tax_point.to_string(), "base": row.base, "tax": row.tax, "corrects_event_id": row.corrects_event_id}),
        ),
    )?;
    Ok(id)
}

/// Record a tax event on an entry of the entity, in its own transaction.
pub fn record_tax_event(conn: &mut Connection, input: TaxEventInput) -> Result<TaxEvent> {
    let tx = conn.transaction()?;
    let event = record_tax_event_in(&tx, input)?;
    tx.commit()?;
    Ok(event)
}

/// Validate and record a tax event inside the caller's transaction.
pub fn record_tax_event_in(conn: &Connection, input: TaxEventInput) -> Result<TaxEvent> {
    let entity = entities::get_entity(conn, input.entity_id)?;
    let entry_entity: i64 = conn
        .query_row("SELECT entity_id FROM journal_entries WHERE id = ?1", [input.entry_id], |r| r.get(0))
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("journal entry {}", input.entry_id)))?;
    if entry_entity != entity.id {
        return Err(Error::Invalid(format!("journal entry {} belongs to another entity", input.entry_id)));
    }
    let commodity = input.commodity.trim().to_ascii_uppercase();
    if commodity != entity.currency {
        return Err(Error::Invalid(format!("a tax event is in the functional currency {}, not {commodity:?}; put other currencies in the native fields", entity.currency)));
    }
    let code = get_tax_code(conn, entity.id, &input.code, input.tax_point)?;
    let fprec = entities::functional_precision(conn, entity.id)?;
    let minor = |field: &str, v: Decimal| -> Result<i64> { Money::from_major(v, &entity.currency, fprec).map(|m| m.minor()).map_err(|_| Error::Invalid(format!("{field} is out of range"))) };
    let base = minor("base_amount", input.base_amount)?;
    let tax = minor("tax_amount", input.tax_amount)?;
    if let Some(posting_id) = input.posting_id {
        let (entry_id, amount): (i64, i64) = conn
            .query_row("SELECT journal_entry_id, amount FROM postings WHERE id = ?1", [posting_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("posting {posting_id}")))?;
        if entry_id != input.entry_id {
            return Err(Error::Invalid(format!("posting {posting_id} belongs to another entry")));
        }
        if !same_side(base, amount) {
            return Err(Error::Invalid(format!("the base must have the sign of posting {posting_id}")));
        }
    }
    if ZERO_TAX_KINDS.contains(&code.kind.as_str()) && tax != 0 {
        return Err(Error::Invalid(format!("tax code {} is {}; its tax must be zero", code.code, code.kind)));
    }
    let recoverable = match input.recoverable_amount {
        Some(v) => minor("recoverable_amount", v)?,
        None => default_recoverable(&code, tax)?,
    };
    let withheld = match input.withheld_amount {
        Some(v) => minor("withheld_amount", v)?,
        None if code.kind == "withholding" => tax,
        None => 0,
    };
    check_amounts(base, tax, recoverable, withheld)?;
    let native = match (input.native_base, input.native_tax, input.native_commodity.as_deref().map(str::trim).filter(|c| !c.is_empty())) {
        (None, None, None) => None,
        (Some(nb), nt, Some(nc)) => {
            let c = entities::get_commodity_by_code(conn, nc)?;
            let to_minor = |v: Decimal| Money::from_major(v, &c.code, c.precision).map(|m| m.minor());
            Some((to_minor(nb)?, to_minor(nt.unwrap_or_default())?, c.code))
        }
        _ => return Err(Error::Invalid("give native_base and native_commodity together".into())),
    };
    if let Some(party) = input.party_id {
        check_party(conn, entity.id, party)?;
    }
    if let Some(original) = input.corrects_event_id {
        let original_entity: i64 =
            conn.query_row("SELECT entity_id FROM tax_events WHERE id = ?1", [original], |r| r.get(0)).optional()?.ok_or_else(|| Error::NotFound(format!("tax event {original}")))?;
        if original_entity != entity.id {
            return Err(Error::Invalid(format!("tax event {original} belongs to another entity")));
        }
    }
    let metadata = match input.metadata {
        serde_json::Value::Null => serde_json::json!({}),
        v @ serde_json::Value::Object(_) => v,
        _ => return Err(Error::Invalid("tax event metadata must be an object".into())),
    };
    let id = insert_event(
        conn,
        &EventRow {
            entity_id: entity.id,
            entry_id: input.entry_id,
            posting_id: input.posting_id,
            tax_code_id: code.id,
            tax_point: input.tax_point,
            base,
            tax,
            recoverable,
            withheld,
            commodity,
            native,
            party_id: input.party_id,
            document_ref: input.document_ref,
            rule_version: input.rule_version.filter(|v| !v.trim().is_empty()).unwrap_or_else(|| code.rule_version()),
            corrects_event_id: input.corrects_event_id,
            metadata,
        },
    )?;
    get_tax_event(conn, id)
}

/// True when `value` is zero or has the sign of `reference`.
fn same_side(value: i64, reference: i64) -> bool {
    value == 0 || reference == 0 || (value < 0) == (reference < 0)
}

fn check_amounts(base: i64, tax: i64, recoverable: i64, withheld: i64) -> Result<()> {
    if !same_side(tax, base) {
        return Err(Error::Invalid("the tax must have the sign of the base".into()));
    }
    if !same_side(recoverable, tax) || recoverable.unsigned_abs() > tax.unsigned_abs() {
        return Err(Error::Invalid("the recoverable amount must have the sign of the tax and must not exceed it".into()));
    }
    if !same_side(withheld, base) {
        return Err(Error::Invalid("the withheld amount must have the sign of the base".into()));
    }
    Ok(())
}

fn check_party(conn: &Connection, entity_id: i64, payee_id: i64) -> Result<()> {
    let payee = crate::payees::get(conn, payee_id)?;
    if payee.entity_id != entity_id {
        return Err(Error::Invalid(format!("payee {payee_id} belongs to another entity")));
    }
    Ok(())
}

/// `minor` times `bp` / 10000, rounded half-even to a whole minor unit.
pub fn apply_bp(minor: i64, bp: i64) -> Result<i64> {
    (Decimal::from(minor) * Decimal::from(bp) / Decimal::from(FULL_BP))
        .round_dp_with_strategy(0, RoundingStrategy::MidpointNearestEven)
        .to_i64()
        .ok_or_else(|| Error::Invalid("tax amount out of range".into()))
}

fn default_recoverable(code: &TaxCode, tax: i64) -> Result<i64> {
    if RECOVERABLE_KINDS.contains(&code.kind.as_str()) {
        apply_bp(tax, code.recoverable_bp)
    } else {
        Ok(0)
    }
}

const EVENT_SELECT: &str = "SELECT t.id, t.uid, t.entity_id, t.entry_id, e.status, t.posting_id, t.tax_code_id, c.code, c.kind, c.rate_bp, t.tax_point,
        t.base_amount, t.tax_amount, t.recoverable_amount, t.withheld_amount, t.commodity, fc.precision,
        t.native_base, t.native_tax, t.native_commodity, nc.precision,
        t.party_id, t.document_ref, t.rule_version, t.corrects_event_id, t.metadata, t.created_at
     FROM tax_events t JOIN tax_codes c ON c.id = t.tax_code_id JOIN journal_entries e ON e.id = t.entry_id
          JOIN commodities fc ON fc.code = t.commodity LEFT JOIN commodities nc ON nc.code = t.native_commodity";

fn read_events(conn: &Connection, where_sql: &str, params: &[&dyn rusqlite::ToSql]) -> Result<Vec<TaxEvent>> {
    type Raw = (TaxEvent, [i64; 4], String, u32, Option<(i64, i64, String, u32)>);
    let mut stmt = conn.prepare(&format!("{EVENT_SELECT} {where_sql}"))?;
    let placeholder = Money::from_minor(0, "XXX", 0)?;
    let rows = stmt.query_map(params, |r| {
        let meta: String = r.get(25)?;
        let native = match (r.get::<_, Option<i64>>(17)?, r.get::<_, Option<i64>>(18)?, r.get::<_, Option<String>>(19)?, r.get::<_, Option<i64>>(20)?) {
            (Some(b), t, Some(c), Some(p)) => Some((b, t.unwrap_or(0), c, p as u32)),
            _ => None,
        };
        let event = TaxEvent {
            id: r.get(0)?,
            uid: r.get(1)?,
            entity_id: r.get(2)?,
            entry_id: r.get(3)?,
            entry_status: r.get(4)?,
            posting_id: r.get(5)?,
            tax_code_id: r.get(6)?,
            code: r.get(7)?,
            kind: r.get(8)?,
            rate_bp: r.get(9)?,
            tax_point: date_at(r, 10)?,
            base: placeholder,
            tax: placeholder,
            recoverable: placeholder,
            withheld: placeholder,
            native_base: None,
            native_tax: None,
            party_id: r.get(21)?,
            document_ref: r.get(22)?,
            rule_version: r.get(23)?,
            corrects_event_id: r.get(24)?,
            metadata: serde_json::from_str(&meta).unwrap_or(serde_json::json!({})),
            created_at: r.get(26)?,
        };
        Ok((event, [r.get(11)?, r.get(12)?, r.get(13)?, r.get(14)?], r.get::<_, String>(15)?, r.get::<_, i64>(16)? as u32, native))
    })?;
    let raws: Vec<Raw> = rows.collect::<std::result::Result<_, _>>()?;
    raws.into_iter()
        .map(|(mut e, amounts, commodity, precision, native)| {
            e.base = Money::from_minor(amounts[0], &commodity, precision)?;
            e.tax = Money::from_minor(amounts[1], &commodity, precision)?;
            e.recoverable = Money::from_minor(amounts[2], &commodity, precision)?;
            e.withheld = Money::from_minor(amounts[3], &commodity, precision)?;
            if let Some((b, t, c, p)) = native {
                e.native_base = Some(Money::from_minor(b, &c, p)?);
                e.native_tax = Some(Money::from_minor(t, &c, p)?);
            }
            Ok(e)
        })
        .collect()
}

pub fn get_tax_event(conn: &Connection, id: i64) -> Result<TaxEvent> {
    read_events(conn, "WHERE t.id = ?1", &[&id])?.pop().ok_or_else(|| Error::NotFound(format!("tax event {id}")))
}

/// The tax events of one entry, in the order of their creation.
pub fn tax_events_for_entry(conn: &Connection, entry_id: i64) -> Result<Vec<TaxEvent>> {
    read_events(conn, "WHERE t.entry_id = ?1 ORDER BY t.id", &[&entry_id])
}

/// The events of posted and void entries with a tax point from `from` to `to` (both inclusive).
pub fn tax_events_list(conn: &Connection, entity_id: i64, from: NaiveDate, to: NaiveDate, code: Option<&str>) -> Result<Vec<TaxEvent>> {
    entities::get_entity(conn, entity_id)?;
    let code = code.map(normalize_code).transpose()?;
    let (from, to) = (from.to_string(), to.to_string());
    read_events(
        conn,
        "WHERE t.entity_id = ?1 AND e.status IN ('posted', 'void') AND t.tax_point >= ?2 AND t.tax_point <= ?3 AND (?4 IS NULL OR c.code = ?4) ORDER BY t.tax_point, t.id",
        &[&entity_id, &from, &to, &code],
    )
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaxSummaryRow {
    pub code: String,
    pub kind: String,
    pub rate_bp: i64,
    pub base: Money,
    pub tax: Money,
    pub recoverable: Money,
    pub withheld: Money,
    pub count: i64,
}

/// Sum the events of posted and void entries by code, kind and rate, for the tax points from
/// `from` to `to` (both inclusive). A void and its reversal both count, each on its own tax point.
pub fn tax_summary(conn: &Connection, entity_id: i64, from: NaiveDate, to: NaiveDate) -> Result<Vec<TaxSummaryRow>> {
    entities::get_entity(conn, entity_id)?;
    if to < from {
        return Err(Error::Invalid(format!("the period must end on or after it starts: {from} to {to}")));
    }
    let mut stmt = conn.prepare(
        "SELECT c.code, c.kind, c.rate_bp, SUM(t.base_amount), SUM(t.tax_amount), SUM(t.recoverable_amount), SUM(t.withheld_amount), COUNT(*), t.commodity, fc.precision
         FROM tax_events t JOIN tax_codes c ON c.id = t.tax_code_id JOIN journal_entries e ON e.id = t.entry_id JOIN commodities fc ON fc.code = t.commodity
         WHERE t.entity_id = ?1 AND e.status IN ('posted', 'void') AND t.tax_point >= ?2 AND t.tax_point <= ?3
         GROUP BY c.code, c.kind, c.rate_bp, t.commodity ORDER BY c.code, c.rate_bp, c.kind",
    )?;
    let rows = stmt.query_map(params![entity_id, from.to_string(), to.to_string()], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            [r.get::<_, i64>(3)?, r.get::<_, i64>(4)?, r.get::<_, i64>(5)?, r.get::<_, i64>(6)?],
            r.get::<_, i64>(7)?,
            r.get::<_, String>(8)?,
            r.get::<_, i64>(9)? as u32,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (code, kind, rate_bp, sums, count, commodity, precision) = row?;
        let m = |v: i64| Money::from_minor(v, &commodity, precision);
        out.push(TaxSummaryRow { code, kind, rate_bp, base: m(sums[0])?, tax: m(sums[1])?, recoverable: m(sums[2])?, withheld: m(sums[3])?, count });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Engine integration: tax blocks on drafts, voids and deletes
// ---------------------------------------------------------------------------

/// The metadata key that marks a posting that the engine added for a tax block.
pub const TAX_LINE_KEY: &str = "tax_line";

/// True when the posting metadata marks an engine tax line.
pub fn is_tax_line(metadata: &serde_json::Value) -> bool {
    metadata.get(TAX_LINE_KEY).is_some()
}

/// A posting that the engine adds for a tax block. Amounts are minor units.
pub(crate) struct TaxLine {
    pub account: Account,
    /// In the functional currency.
    pub amount: i64,
    /// In the account commodity.
    pub quantity: i64,
    /// True when the line is on the account of the taxed posting and copies its rate.
    pub on_taxed_account: bool,
    pub memo: String,
    pub metadata: serde_json::Value,
}

/// A tax event that the engine records after it inserts the postings.
#[derive(Debug, Clone)]
pub(crate) struct PlannedEvent {
    pub posting_index: usize,
    pub code: TaxCode,
    pub tax_point: NaiveDate,
    pub base: i64,
    pub tax: i64,
    pub recoverable: i64,
    pub withheld: i64,
    pub native: Option<(i64, i64, String)>,
    pub party_id: Option<i64>,
    pub document_ref: String,
}

fn magnitude(field: &str, value: Option<Decimal>, commodity: &str, fprec: u32) -> Result<Option<i64>> {
    let Some(v) = value else { return Ok(None) };
    if v.is_sign_negative() && !v.is_zero() {
        return Err(Error::Invalid(format!("tax {field} is a magnitude; give it without a sign, got {}", money::plain(v))));
    }
    Ok(Some(Money::from_major(v, commodity, fprec)?.minor()))
}

/// Compute the event and the tax lines of one taxed posting.
///
/// The caller gives net amounts. The engine signs each amount like the posting and adds:
/// - output: the tax on the tax account;
/// - input: the recoverable part on the tax account and the rest on the posting account;
/// - reverse_charge: the tax negated and the recoverable part on the tax account, and the rest on
///   the posting account, so the lines sum to zero;
/// - withholding: the tax negated on the tax account, so the other party receives less;
/// - exempt, zero_rated and out_of_scope: no line.
///
/// A code without a tax account adds no line.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan(
    conn: &Connection,
    entity: &Entity,
    entry_date: NaiveDate,
    fprec: u32,
    index: usize,
    account: &Account,
    quantity: Money,
    amount: Money,
    block: &TaxInput,
) -> Result<(PlannedEvent, Vec<TaxLine>)> {
    let tax_point = block.tax_point.unwrap_or(entry_date);
    let code = get_tax_code(conn, entity.id, &block.code, tax_point)?;
    if amount.is_zero() {
        return Err(Error::Invalid(format!("posting on {} has a zero amount, so it cannot carry a tax block", account.path)));
    }
    let sign: i64 = if amount.is_negative() { -1 } else { 1 };
    let base = match magnitude("base_amount", block.base_amount, &entity.currency, fprec)? {
        Some(m) => m.checked_mul(sign).ok_or_else(|| Error::Invalid("tax base out of range".into()))?,
        None => amount.minor(),
    };
    let tax = match magnitude("tax_amount", block.tax_amount, &entity.currency, fprec)? {
        Some(m) => m.checked_mul(sign).ok_or_else(|| Error::Invalid("tax amount out of range".into()))?,
        None => apply_bp(base, code.rate_bp)?,
    };
    if ZERO_TAX_KINDS.contains(&code.kind.as_str()) && tax != 0 {
        return Err(Error::Invalid(format!("tax code {} is {}; its tax must be zero", code.code, code.kind)));
    }
    let recoverable = default_recoverable(&code, tax)?;
    let withheld = if code.kind == "withholding" { tax } else { 0 };
    check_amounts(base, tax, recoverable, withheld)?;
    let native = if account.commodity != entity.currency && !quantity.is_zero() {
        // Scale the native quantity by the share of the posting that the base covers.
        let native_base = if base == amount.minor() {
            quantity.minor()
        } else {
            (Decimal::from(quantity.minor()) * Decimal::from(base) / Decimal::from(amount.minor())).round_dp_with_strategy(0, RoundingStrategy::MidpointNearestEven).to_i64().unwrap_or(0)
        };
        let native_tax = if tax == apply_bp(base, code.rate_bp)? {
            apply_bp(native_base, code.rate_bp)?
        } else {
            (Decimal::from(native_base) * Decimal::from(tax) / Decimal::from(base)).round_dp_with_strategy(0, RoundingStrategy::MidpointNearestEven).to_i64().unwrap_or(0)
        };
        Some((native_base, native_tax, account.commodity.clone()))
    } else {
        None
    };

    let mut lines = Vec::new();
    if let (Some(tax_account_id), true) = (code.tax_account_id, tax != 0) {
        let tax_account = accounts::get_account(conn, tax_account_id)?;
        check_tax_account(conn, entity, &tax_account)?;
        let meta = |role: &str| serde_json::json!({ TAX_LINE_KEY: code.code, "tax_role": role });
        let on_tax = |amount: i64, memo: String, role: &str| TaxLine { account: tax_account.clone(), amount, quantity: amount, on_taxed_account: false, memo, metadata: meta(role) };
        let non_recoverable = tax - recoverable;
        match code.kind.as_str() {
            "output" => lines.push(on_tax(tax, format!("{} output tax", code.code), "output")),
            "input" => {
                if recoverable != 0 {
                    lines.push(on_tax(recoverable, format!("{} input tax", code.code), "input"));
                }
                if non_recoverable != 0 {
                    lines.push(non_recoverable_line(entity, account, quantity, amount, non_recoverable, &code)?);
                }
            }
            "reverse_charge" => {
                lines.push(on_tax(-tax, format!("{} reverse charge output", code.code), "reverse_charge_output"));
                if recoverable != 0 {
                    lines.push(on_tax(recoverable, format!("{} reverse charge input", code.code), "reverse_charge_input"));
                }
                if non_recoverable != 0 {
                    lines.push(non_recoverable_line(entity, account, quantity, amount, non_recoverable, &code)?);
                }
            }
            "withholding" => lines.push(on_tax(-tax, format!("{} withheld", code.code), "withholding")),
            _ => {}
        }
    }
    let event = PlannedEvent { posting_index: index, code, tax_point, base, tax, recoverable, withheld, native, party_id: block.party_id, document_ref: block.document_ref.clone() };
    Ok((event, lines))
}

/// The part of the tax that the entity cannot recover is a cost. It goes on the account of the
/// taxed posting, converted at the rate of that posting.
fn non_recoverable_line(entity: &Entity, account: &Account, quantity: Money, amount: Money, value: i64, code: &TaxCode) -> Result<TaxLine> {
    let line_quantity = if account.commodity == entity.currency {
        value
    } else {
        (Decimal::from(value) * Decimal::from(quantity.minor()) / Decimal::from(amount.minor()))
            .round_dp_with_strategy(0, RoundingStrategy::MidpointNearestEven)
            .to_i64()
            .ok_or_else(|| Error::Invalid("tax amount out of range".into()))?
    };
    if line_quantity == 0 {
        return Err(Error::Invalid(format!("the non-recoverable tax on {} rounds to zero in {}", account.path, account.commodity)));
    }
    Ok(TaxLine {
        account: account.clone(),
        amount: value,
        quantity: line_quantity,
        on_taxed_account: true,
        memo: format!("{} non-recoverable tax", code.code),
        metadata: serde_json::json!({ TAX_LINE_KEY: code.code, "tax_role": "non_recoverable" }),
    })
}

/// Record the planned events of a new or replaced entry. The posting at index `i` of the
/// prepared rows has position `i`. The party defaults to the canonical payee of the entry.
pub(crate) fn insert_planned(conn: &Connection, entity: &Entity, entry_id: i64, events: &[PlannedEvent]) -> Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let payee_id: Option<i64> = conn.query_row("SELECT payee_id FROM journal_entries WHERE id = ?1", [entry_id], |r| r.get(0))?;
    let posting_at =
        |position: usize| -> Result<i64> { Ok(conn.query_row("SELECT id FROM postings WHERE journal_entry_id = ?1 AND position = ?2", params![entry_id, position as i64], |r| r.get(0))?) };
    for e in events {
        if let Some(party) = e.party_id {
            check_party(conn, entity.id, party)?;
        }
        insert_event(
            conn,
            &EventRow {
                entity_id: entity.id,
                entry_id,
                posting_id: Some(posting_at(e.posting_index)?),
                tax_code_id: e.code.id,
                tax_point: e.tax_point,
                base: e.base,
                tax: e.tax,
                recoverable: e.recoverable,
                withheld: e.withheld,
                commodity: entity.currency.clone(),
                native: e.native.clone(),
                party_id: e.party_id.or(payee_id),
                document_ref: e.document_ref.clone(),
                rule_version: e.code.rule_version(),
                corrects_event_id: None,
                metadata: serde_json::json!({}),
            },
        )?;
    }
    Ok(())
}

/// Create the mirror events of a void on its reversal entry: negated amounts, the tax point of
/// the reversal, and a link to each original event.
pub(crate) fn mirror_for_reversal(conn: &Connection, original_entry_id: i64, reversal_entry_id: i64) -> Result<usize> {
    let originals = tax_events_for_entry(conn, original_entry_id)?;
    if originals.is_empty() {
        return Ok(0);
    }
    let reversal_date: String = conn.query_row("SELECT date FROM journal_entries WHERE id = ?1", [reversal_entry_id], |r| r.get(0))?;
    let tax_point = crate::parse_date(&reversal_date)?;
    let mut count = 0;
    for e in originals.iter().filter(|e| e.corrects_event_id.is_none()) {
        let posting_id: Option<i64> = match e.posting_id {
            Some(pid) => {
                conn.query_row("SELECT id FROM postings WHERE journal_entry_id = ?1 AND json_extract(metadata, '$.reverses_posting') = ?2", params![reversal_entry_id, pid], |r| r.get(0)).optional()?
            }
            None => None,
        };
        let neg = |m: Money| -> Result<i64> { Ok(m.checked_neg()?.minor()) };
        insert_event(
            conn,
            &EventRow {
                entity_id: e.entity_id,
                entry_id: reversal_entry_id,
                posting_id,
                tax_code_id: e.tax_code_id,
                tax_point,
                base: neg(e.base)?,
                tax: neg(e.tax)?,
                recoverable: neg(e.recoverable)?,
                withheld: neg(e.withheld)?,
                commodity: e.base.commodity().to_string(),
                native: match (e.native_base, e.native_tax) {
                    (Some(b), Some(t)) => Some((neg(b)?, neg(t)?, b.commodity().to_string())),
                    _ => None,
                },
                party_id: e.party_id,
                document_ref: e.document_ref.clone(),
                rule_version: e.rule_version.clone(),
                corrects_event_id: Some(e.id),
                metadata: serde_json::json!({ "reversal_of_entry": original_entry_id }),
            },
        )?;
        count += 1;
    }
    Ok(count)
}

/// Delete the events of an entry, for a deleted draft or an edit that replaces its tax blocks.
pub(crate) fn delete_for_entry(conn: &Connection, entry_id: i64) -> Result<()> {
    let ids: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT id FROM tax_events WHERE entry_id = ?1")?;
        let rows = stmt.query_map([entry_id], |r| r.get(0))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let referenced: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM tax_events WHERE corrects_event_id IN (SELECT id FROM tax_events WHERE entry_id = ?1))", [entry_id], |r| r.get(0))?;
    if referenced {
        return Err(Error::Locked(format!("the tax events of entry {entry_id} have corrections; void the entry instead")));
    }
    conn.execute("DELETE FROM tax_events WHERE entry_id = ?1", [entry_id])?;
    for id in ids {
        audit::log(conn, "tax_events", id, "delete", Some(serde_json::json!({"entry_id": entry_id})), None)?;
    }
    Ok(())
}

/// The tax event links of the postings of an entry, before the postings are replaced.
pub(crate) fn posting_links(conn: &Connection, entry_id: i64) -> Result<Vec<(i64, i64)>> {
    let mut stmt = conn.prepare("SELECT id, posting_id FROM tax_events WHERE entry_id = ?1 AND posting_id IS NOT NULL")?;
    let rows = stmt.query_map([entry_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// Move each tax event to the replacement of its posting. An event without a replacement
/// posting stays on the entry.
pub(crate) fn relink_postings(conn: &Connection, links: &[(i64, i64)], replaced: &std::collections::HashMap<i64, i64>) -> Result<()> {
    for (event_id, old_posting) in links {
        conn.execute("UPDATE tax_events SET posting_id = ?2 WHERE id = ?1", params![event_id, replaced.get(old_posting)])?;
    }
    Ok(())
}

/// True when the account type can take a default tax block of an entry.
pub(crate) fn takes_default_block(account: &Account) -> bool {
    matches!(account.r#type, AccountType::Income | AccountType::Expense)
}
