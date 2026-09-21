//! Prices and exchange rates: lookup with fallbacks, ECB import, revaluation.

use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};
use rust_decimal::prelude::*;
use rust_decimal::Decimal;

use crate::entities;
use crate::model::Price;
use crate::money;
use crate::{Error, Result};

/// How far back a "nearest" rate may be before it counts as missing.
const MAX_STALE_DAYS: i64 = 400;

#[derive(Debug, Clone)]
pub struct RateLookup {
    pub rate: Decimal,
    /// exact | nearest | inverse | cross | same
    pub source: String,
}

/// Price of one unit of `commodity` in `currency` on `date` (or the latest before it).
pub fn rate_for(conn: &Connection, commodity: &str, currency: &str, date: NaiveDate) -> Result<Option<RateLookup>> {
    let commodity = commodity.trim().to_ascii_uppercase();
    let currency = currency.trim().to_ascii_uppercase();
    if commodity == currency {
        return Ok(Some(RateLookup { rate: Decimal::ONE, source: "same".into() }));
    }
    if let Some((p, on)) = latest_price(conn, &commodity, &currency, date)? {
        let source = if on == date { "exact" } else { "nearest" };
        return Ok(Some(RateLookup { rate: p, source: source.into() }));
    }
    if let Some((p, _)) = latest_price(conn, &currency, &commodity, date)? {
        if !p.is_zero() {
            return Ok(Some(RateLookup { rate: (Decimal::ONE / p).round_dp(12), source: "inverse".into() }));
        }
    }
    // Cross through any currency that quotes both (EUR for ECB data).
    let mut stmt = conn.prepare(
        "SELECT DISTINCT cur.code FROM prices p JOIN commodities cur ON cur.id = p.currency_id
         JOIN commodities c ON c.id = p.commodity_id WHERE c.code IN (?1, ?2) ORDER BY cur.code",
    )?;
    let vias: Vec<String> = stmt.query_map(params![commodity, currency], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
    for via in vias {
        if via == commodity || via == currency {
            continue;
        }
        let a = price_in(conn, &commodity, &via, date)?;
        let b = price_in(conn, &currency, &via, date)?;
        if let (Some(a), Some(b)) = (a, b) {
            if !b.is_zero() {
                return Ok(Some(RateLookup { rate: (a / b).round_dp(12), source: "cross".into() }));
            }
        }
    }
    Ok(None)
}

/// Price of `commodity` in `currency`, direct or inverse, latest on or before `date`.
fn price_in(conn: &Connection, commodity: &str, currency: &str, date: NaiveDate) -> Result<Option<Decimal>> {
    if commodity == currency {
        return Ok(Some(Decimal::ONE));
    }
    if let Some((p, _)) = latest_price(conn, commodity, currency, date)? {
        return Ok(Some(p));
    }
    if let Some((p, _)) = latest_price(conn, currency, commodity, date)? {
        if !p.is_zero() {
            return Ok(Some(Decimal::ONE / p));
        }
    }
    Ok(None)
}

fn latest_price(conn: &Connection, commodity: &str, currency: &str, date: NaiveDate) -> Result<Option<(Decimal, NaiveDate)>> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT p.price, p.on_date FROM prices p
             JOIN commodities c ON c.id = p.commodity_id JOIN commodities cur ON cur.id = p.currency_id
             WHERE c.code = ?1 AND cur.code = ?2 AND p.on_date <= ?3 ORDER BY p.on_date DESC LIMIT 1",
            params![commodity, currency, date.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match row {
        Some((p, on)) => {
            let on = NaiveDate::parse_from_str(&on, "%Y-%m-%d").map_err(|e| Error::Parse(e.to_string()))?;
            if (date - on).num_days() > MAX_STALE_DAYS {
                return Ok(None);
            }
            Ok(Some((money::parse(&p)?, on)))
        }
        None => Ok(None),
    }
}

pub fn set_price(conn: &Connection, commodity: &str, currency: &str, on: NaiveDate, price: Decimal, source: &str) -> Result<()> {
    if price <= Decimal::ZERO {
        return Err(Error::Invalid("price must be positive".into()));
    }
    let cid = match entities::get_commodity_by_code(conn, commodity) {
        Ok(c) => c.id,
        Err(Error::NotFound(_)) => entities::ensure_currency(conn, commodity)?,
        Err(e) => return Err(e),
    };
    let curid = entities::ensure_currency(conn, currency)?;
    conn.execute(
        "INSERT INTO prices (commodity_id, currency_id, on_date, price, source) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(commodity_id, currency_id, on_date) DO UPDATE SET price = excluded.price, source = excluded.source",
        params![cid, curid, on.to_string(), money::plain(price), source],
    )?;
    Ok(())
}

pub fn list_prices(conn: &Connection, commodity: &str, currency: &str, from: Option<NaiveDate>, to: Option<NaiveDate>, limit: i64) -> Result<Vec<Price>> {
    let mut stmt = conn.prepare(
        "SELECT p.id, c.code, cur.code, p.on_date, p.price, p.source FROM prices p
         JOIN commodities c ON c.id = p.commodity_id JOIN commodities cur ON cur.id = p.currency_id
         WHERE (?1 = '' OR c.code = ?1) AND (?2 = '' OR cur.code = ?2) AND (?3 IS NULL OR p.on_date >= ?3) AND (?4 IS NULL OR p.on_date <= ?4)
         ORDER BY p.on_date DESC, c.code LIMIT ?5",
    )?;
    let rows = stmt.query_map(
        params![commodity.trim().to_ascii_uppercase(), currency.trim().to_ascii_uppercase(), from.map(|d| d.to_string()), to.map(|d| d.to_string()), if limit <= 0 { 500 } else { limit }],
        |r| {
            let on: String = r.get(3)?;
            let price: String = r.get(4)?;
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, on, price, r.get::<_, String>(5)?))
        },
    )?;
    let mut out = Vec::new();
    for row in rows {
        let (id, commodity, currency, on, price, source) = row?;
        out.push(Price { id, commodity, currency, on: NaiveDate::parse_from_str(&on, "%Y-%m-%d").map_err(|e| Error::Parse(e.to_string()))?, price: money::parse(&price)?, source });
    }
    Ok(out)
}

pub fn price_count(conn: &Connection) -> Result<(i64, Option<String>)> {
    Ok(conn.query_row("SELECT COUNT(*), MAX(on_date) FROM prices", [], |r| Ok((r.get(0)?, r.get(1)?)))?)
}

/// Import the ECB euro reference rates history (eurofxref-hist.xml).
/// Stores the price of each currency in EUR (1 / rate). Returns the number of rows written.
pub fn import_ecb_xml(conn: &mut Connection, xml: &str, from: Option<NaiveDate>) -> Result<usize> {
    let mut count = 0usize;
    let tx = conn.transaction()?;
    let mut current: Option<NaiveDate> = None;
    for raw in xml.split('<') {
        let tag = raw.trim_start();
        if !tag.starts_with("Cube") {
            continue;
        }
        if let Some(t) = attr(tag, "time") {
            let d = NaiveDate::parse_from_str(&t, "%Y-%m-%d").ok();
            current = d;
            continue;
        }
        let (Some(date), Some(cur), Some(rate)) = (current, attr(tag, "currency"), attr(tag, "rate")) else { continue };
        if let Some(f) = from {
            if date < f {
                continue;
            }
        }
        let Ok(rate) = Decimal::from_str(&rate) else { continue };
        if rate.is_zero() {
            continue;
        }
        let price = (Decimal::ONE / rate).round_dp(10);
        set_price(&tx, &cur, "EUR", date, price, "ecb")?;
        count += 1;
    }
    tx.commit()?;
    Ok(count)
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let key = format!("{name}=\"");
    let start = tag.find(&key)? + key.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// What a revaluation pass did.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Revalued {
    pub fixed: usize,
    pub failed: usize,
    pub first_error: Option<String>,
}

/// Recompute functional amounts of postings that were booked without a rate, now that prices exist.
/// Entries are re-balanced through their functional-currency postings, or the FX account as a last resort.
/// Hash chains are recomputed once per entity at the end.
pub fn revalue_missing(conn: &mut Connection) -> Result<Revalued> {
    let ids: Vec<(i64, i64, Option<i64>)> = {
        let mut stmt = conn.prepare("SELECT DISTINCT e.id, e.entity_id, e.seq FROM postings p JOIN journal_entries e ON e.id = p.journal_entry_id WHERE p.rate_source = 'missing' ORDER BY e.id")?;
        let v: Vec<(i64, i64, Option<i64>)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<std::result::Result<_, _>>()?;
        v
    };
    let mut report = Revalued::default();
    let mut min_seq: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    for (id, entity_id, seq) in ids {
        match crate::journal::revalue_entry(conn, id, false) {
            Ok(true) => {
                report.fixed += 1;
                if let Some(s) = seq {
                    let e = min_seq.entry(entity_id).or_insert(s);
                    if s < *e {
                        *e = s;
                    }
                }
            }
            Ok(false) => {}
            Err(e) => {
                report.failed += 1;
                if report.first_error.is_none() {
                    report.first_error = Some(format!("entry #{id}: {e}"));
                }
                tracing::warn!(entry = id, "revaluation failed: {e}");
            }
        }
    }
    for (entity_id, seq) in min_seq {
        crate::hashchain::rechain(conn, entity_id, seq)?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn ecb_and_cross_rates() {
        let db = Db::open_memory().unwrap();
        let mut conn = db.conn();
        let xml = r#"<gesmes:Envelope><Cube><Cube time="2026-08-28"><Cube currency="USD" rate="1.1400"/><Cube currency="BRL" rate="6.2000"/></Cube>
            <Cube time="2026-08-27"><Cube currency="USD" rate="1.1300"/></Cube></Cube></gesmes:Envelope>"#;
        let n = import_ecb_xml(&mut conn, xml, None).unwrap();
        assert_eq!(n, 3);
        let d = NaiveDate::from_ymd_opt(2026, 8, 28).unwrap();
        let usd = rate_for(&conn, "USD", "EUR", d).unwrap().unwrap();
        assert_eq!(usd.source, "exact");
        assert_eq!(usd.rate.round_dp(6), Decimal::from_str("0.877193").unwrap());
        let eur_in_usd = rate_for(&conn, "EUR", "USD", d).unwrap().unwrap();
        assert_eq!(eur_in_usd.source, "inverse");
        assert_eq!(eur_in_usd.rate.round_dp(4), Decimal::from_str("1.1400").unwrap());
        let usd_in_brl = rate_for(&conn, "USD", "BRL", d).unwrap().unwrap();
        assert_eq!(usd_in_brl.source, "cross");
        assert_eq!(usd_in_brl.rate.round_dp(4), Decimal::from_str("5.4386").unwrap());
        let later = rate_for(&conn, "USD", "EUR", NaiveDate::from_ymd_opt(2026, 9, 10).unwrap()).unwrap().unwrap();
        assert_eq!(later.source, "nearest");
        assert!(rate_for(&conn, "USD", "EUR", NaiveDate::from_ymd_opt(2020, 1, 1).unwrap()).unwrap().is_none());
    }
}
