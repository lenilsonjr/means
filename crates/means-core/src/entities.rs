//! Entities (sets of books) and commodities.

use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};

use crate::accounts;
use crate::model::{AccountType, Commodity, Entity, NewAccount};
use crate::money;
use crate::{new_uid, now_ts, Error, Result};

pub fn list_entities(conn: &Connection, include_archived: bool) -> Result<Vec<Entity>> {
    let mut stmt = conn.prepare(
        "SELECT id, uid, name, kind, country, currency, lock_date, archived_at, created_at
         FROM entities WHERE (?1 = 1 OR archived_at IS NULL) ORDER BY id",
    )?;
    let rows = stmt.query_map([include_archived as i64], row_to_entity)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub fn get_entity(conn: &Connection, id: i64) -> Result<Entity> {
    conn.query_row("SELECT id, uid, name, kind, country, currency, lock_date, archived_at, created_at FROM entities WHERE id = ?1", [id], row_to_entity)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("entity {id}")))
}

fn row_to_entity(r: &rusqlite::Row<'_>) -> rusqlite::Result<Entity> {
    let lock: Option<String> = r.get(6)?;
    Ok(Entity {
        id: r.get(0)?,
        uid: r.get(1)?,
        name: r.get(2)?,
        kind: r.get(3)?,
        country: r.get(4)?,
        currency: r.get(5)?,
        lock_date: lock.and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()),
        archived_at: r.get(7)?,
        created_at: r.get(8)?,
    })
}

/// Create an entity with its functional currency and the system accounts every set of books needs.
pub fn create_entity(conn: &mut Connection, name: &str, kind: &str, country: &str, currency: &str) -> Result<Entity> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::Invalid("entity name is required".into()));
    }
    let kind = match kind.trim().to_ascii_lowercase().as_str() {
        "" | "person" => "person",
        "company" | "business" => "company",
        other => return Err(Error::Invalid(format!("entity kind must be person or company, got {other:?}"))),
    };
    let currency = normalize_code(currency)?;
    let tx = conn.transaction()?;
    ensure_currency(&tx, &currency)?;
    let ts = now_ts();
    tx.execute(
        "INSERT INTO entities (uid, name, kind, country, currency, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![new_uid(), name, kind, country.trim().to_ascii_uppercase(), currency, ts],
    )
    .map_err(|e| match e {
        rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => Error::Conflict(format!("an entity named {name:?} already exists")),
        other => Error::Db(other),
    })?;
    let id = tx.last_insert_rowid();
    create_system_accounts(&tx, id, kind, &currency)?;
    crate::audit::log(&tx, "entities", id, "create", None, Some(serde_json::json!({"name": name, "kind": kind, "currency": currency})))?;
    tx.commit()?;
    get_entity(conn, id)
}

pub fn update_entity(conn: &mut Connection, id: i64, name: &str, kind: &str, country: &str, lock_date: Option<NaiveDate>, archived: bool) -> Result<Entity> {
    let before = get_entity(conn, id)?;
    let name = if name.trim().is_empty() { before.name.clone() } else { name.trim().to_string() };
    let kind = if kind.trim().is_empty() { before.kind.clone() } else { kind.trim().to_ascii_lowercase() };
    if kind != "person" && kind != "company" {
        return Err(Error::Invalid("entity kind must be person or company".into()));
    }
    let archived_at = match (archived, &before.archived_at) {
        (true, Some(a)) => Some(a.clone()),
        (true, None) => Some(now_ts()),
        (false, _) => None,
    };
    conn.execute(
        "UPDATE entities SET name = ?2, kind = ?3, country = ?4, lock_date = ?5, archived_at = ?6, updated_at = ?7 WHERE id = ?1",
        params![id, name, kind, country.trim().to_ascii_uppercase(), lock_date.map(|d| d.to_string()), archived_at, now_ts()],
    )?;
    let after = get_entity(conn, id)?;
    crate::audit::log(conn, "entities", id, "update", Some(serde_json::to_value(&before)?), Some(serde_json::to_value(&after)?))?;
    Ok(after)
}

fn create_system_accounts(conn: &Connection, entity_id: i64, kind: &str, currency: &str) -> Result<()> {
    let mk = |name: &str, t: AccountType, subtype: &str, role: &str| NewAccount {
        entity_id,
        parent_id: None,
        code: String::new(),
        name: name.to_string(),
        r#type: Some(t),
        subtype: subtype.to_string(),
        commodity: currency.to_string(),
        system_role: role.to_string(),
        placeholder: false,
        in_net_worth: true,
        credit_limit: None,
        statement_day: None,
        due_day: None,
        external_ids: None,
        notes: String::new(),
        position: 900,
    };
    accounts::create_account(conn, mk("Opening balances", AccountType::Equity, "equity", "opening_balance"))?;
    accounts::create_account(conn, mk("Uncategorized", AccountType::Expense, "expense", "suspense"))?;
    accounts::create_account(conn, mk("FX gain/loss", AccountType::Expense, "expense", "fx_gain_loss"))?;
    if kind == "company" {
        accounts::create_account(conn, mk("Owner contributions", AccountType::Equity, "equity", "owner_contributions"))?;
        accounts::create_account(conn, mk("Owner distributions", AccountType::Equity, "equity", "owner_distributions"))?;
    } else {
        accounts::create_account(conn, mk("Distributions received", AccountType::Income, "income", "distributions_received"))?;
        accounts::create_account(conn, mk("Investments in entities", AccountType::Asset, "investment", "investment_in_entity"))?;
    }
    Ok(())
}

pub fn normalize_code(code: &str) -> Result<String> {
    let c = code.trim().to_ascii_uppercase();
    if c.is_empty() || c.len() > 12 || !c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_') {
        return Err(Error::Invalid(format!("invalid commodity code {code:?}")));
    }
    Ok(c)
}

/// Make sure a currency commodity exists; returns its id.
pub fn ensure_currency(conn: &Connection, code: &str) -> Result<i64> {
    let code = normalize_code(code)?;
    if let Some(id) = conn.query_row("SELECT id FROM commodities WHERE code = ?1", [&code], |r| r.get::<_, i64>(0)).optional()? {
        return Ok(id);
    }
    let kind = if money::default_precision(&code) == 8 { "crypto" } else { "currency" };
    conn.execute("INSERT INTO commodities (code, kind, name, precision) VALUES (?1, ?2, ?3, ?4)", params![code, kind, currency_name(&code), money::default_precision(&code)])?;
    Ok(conn.last_insert_rowid())
}

pub fn create_commodity(conn: &Connection, code: &str, kind: &str, name: &str, precision: Option<u32>, isin: &str) -> Result<Commodity> {
    let code = normalize_code(code)?;
    let kind = match kind.trim().to_ascii_lowercase().as_str() {
        "" | "currency" => "currency",
        "security" | "stock" | "fund" | "etf" => "security",
        "crypto" => "crypto",
        other => return Err(Error::Invalid(format!("commodity kind must be currency, security or crypto, got {other:?}"))),
    };
    let stored: Option<u32> = conn.query_row("SELECT precision FROM commodities WHERE code = ?1", [&code], |r| r.get::<_, i64>(0)).optional()?.map(|p| p as u32);
    // D14: the precision says what every integer stored in this commodity counts, so it is fixed
    // when the commodity is created. Changing it would reinterpret every quantity and amount
    // already written in it by a power of ten.
    if let (Some(have), Some(want)) = (stored, precision) {
        if have != want {
            return Err(Error::Invalid(format!("commodity {code} was created with precision {have} and it cannot change; asked for {want}")));
        }
    }
    let precision = stored.or(precision).unwrap_or_else(|| match kind {
        "crypto" => 8,
        "security" => 4,
        _ => money::default_precision(&code),
    });
    money::check_precision(precision)?;
    conn.execute(
        "INSERT INTO commodities (code, kind, name, precision, isin) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(code) DO UPDATE SET kind = excluded.kind, name = CASE WHEN excluded.name <> '' THEN excluded.name ELSE commodities.name END,
           isin = CASE WHEN excluded.isin <> '' THEN excluded.isin ELSE commodities.isin END",
        params![code, kind, if name.trim().is_empty() { currency_name(&code) } else { name.trim().to_string() }, precision, isin.trim()],
    )?;
    get_commodity_by_code(conn, &code)
}

pub fn list_commodities(conn: &Connection) -> Result<Vec<Commodity>> {
    let mut stmt = conn.prepare("SELECT id, code, kind, name, precision, isin FROM commodities ORDER BY kind, code")?;
    let rows = stmt.query_map([], row_to_commodity)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub fn get_commodity(conn: &Connection, id: i64) -> Result<Commodity> {
    conn.query_row("SELECT id, code, kind, name, precision, isin FROM commodities WHERE id = ?1", [id], row_to_commodity).optional()?.ok_or_else(|| Error::NotFound(format!("commodity {id}")))
}

/// The precision of an entity's functional currency: the decimals of every `amount` it stores
/// (D14). The one answer to this question, so a write and a read of the same `amount` cannot
/// differ by a power of ten; the grouped queries in `reports` and `accounts` join
/// `commodities.code = entities.currency` to reach the same number a whole column at a time.
pub fn functional_precision(conn: &Connection, entity_id: i64) -> Result<u32> {
    conn.query_row("SELECT c.precision FROM entities e JOIN commodities c ON c.code = e.currency WHERE e.id = ?1", [entity_id], |r| r.get::<_, i64>(0))
        .optional()?
        .map(|p| p as u32)
        .ok_or_else(|| Error::NotFound(format!("functional currency of entity {entity_id}")))
}

pub fn get_commodity_by_code(conn: &Connection, code: &str) -> Result<Commodity> {
    let code = normalize_code(code)?;
    conn.query_row("SELECT id, code, kind, name, precision, isin FROM commodities WHERE code = ?1", [&code], row_to_commodity).optional()?.ok_or_else(|| Error::NotFound(format!("commodity {code}")))
}

fn row_to_commodity(r: &rusqlite::Row<'_>) -> rusqlite::Result<Commodity> {
    Ok(Commodity { id: r.get(0)?, code: r.get(1)?, kind: r.get(2)?, name: r.get(3)?, precision: r.get::<_, i64>(4)? as u32, isin: r.get(5)? })
}

fn currency_name(code: &str) -> String {
    match code {
        "EUR" => "Euro",
        "USD" => "US dollar",
        "BRL" => "Brazilian real",
        "GBP" => "Pound sterling",
        "CHF" => "Swiss franc",
        "IDR" => "Indonesian rupiah",
        "JPY" => "Japanese yen",
        "CAD" => "Canadian dollar",
        "AUD" => "Australian dollar",
        "MXN" => "Mexican peso",
        "ARS" => "Argentine peso",
        "BTC" => "Bitcoin",
        "ETH" => "Ether",
        other => other,
    }
    .to_string()
}
