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

pub struct RateLookup {
    pub rate: Decimal,
    /// exact | nearest | inverse | same | cross:<SOURCE>
    ///
    /// A cross rate goes through a third currency that quotes both sides, for example EUR for
    /// ECB rates. Its source names the source of the stored rows, for example `cross:ECB`. A
    /// stored row whose own source starts with `cross:` keeps that source.
    pub source: String,
}

/// The prefix of the source of a derived (cross) rate.
pub const CROSS_PREFIX: &str = "cross:";

/// True when a rate source marks a rate derived through a third currency.
pub fn is_cross(source: &str) -> bool {
    source == "cross" || source.starts_with(CROSS_PREFIX)
}

/// The source of a cross rate made from two legs with these stored sources.
fn cross_source(a: &str, b: &str) -> String {
    let strip = |s: &str| s.strip_prefix(CROSS_PREFIX).unwrap_or(s).trim().to_ascii_uppercase();
    let (a, b) = (strip(a), strip(b));
    if a == b || b.is_empty() {
        format!("{CROSS_PREFIX}{a}")
    } else if a.is_empty() {
        format!("{CROSS_PREFIX}{b}")
    } else {
        format!("{CROSS_PREFIX}{a}+{b}")
    }
}

/// Price of one unit of `commodity` in `currency` on `date` (or the latest before it).
///
/// The lookup tries a direct row, then an inverse row, then a cross rate through a currency
/// that quotes both sides. Each leg uses the latest row on or before `date`, within the
/// staleness limit. EUR comes first as the common quote, because ECB rates quote EUR.
pub fn rate_for(conn: &Connection, commodity: &str, currency: &str, date: NaiveDate) -> Result<Option<RateLookup>> {
    let commodity = commodity.trim().to_ascii_uppercase();
    let currency = currency.trim().to_ascii_uppercase();
    if commodity == currency {
        return Ok(Some(RateLookup { rate: Decimal::ONE, source: "same".into() }));
    }
    if let Some((p, on, stored)) = latest_price(conn, &commodity, &currency, date)? {
        let source = if stored.starts_with(CROSS_PREFIX) {
            // A stored cross row of an earlier day gives way to a fresh derivation.
            if on != date {
                if let Some((rate, source)) = cross_rate(conn, &commodity, &currency, date)? {
                    return Ok(Some(RateLookup { rate, source }));
                }
            }
            stored
        } else if on == date {
            "exact".into()
        } else {
            "nearest".into()
        };
        return Ok(Some(RateLookup { rate: p, source }));
    }
    if let Some((p, _, _)) = latest_price(conn, &currency, &commodity, date)? {
        if !p.is_zero() {
            return Ok(Some(RateLookup { rate: (Decimal::ONE / p).round_dp(12), source: "inverse".into() }));
        }
    }
    Ok(cross_rate(conn, &commodity, &currency, date)?.map(|(rate, source)| RateLookup { rate, source }))
}

/// A cross rate of `commodity` in `currency` through a currency that quotes both, with its
/// source. `None` when no common quote has both legs on or before `date`.
fn cross_rate(conn: &Connection, commodity: &str, currency: &str, date: NaiveDate) -> Result<Option<(Decimal, String)>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT code FROM (
           SELECT cur.code AS code FROM prices p JOIN commodities cur ON cur.id = p.currency_id
             JOIN commodities c ON c.id = p.commodity_id WHERE c.code IN (?1, ?2)
           UNION
           SELECT c.code FROM prices p JOIN commodities cur ON cur.id = p.currency_id
             JOIN commodities c ON c.id = p.commodity_id WHERE cur.code IN (?1, ?2))
         ORDER BY code <> 'EUR', code",
    )?;
    let vias: Vec<String> = stmt.query_map(params![commodity, currency], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
    for via in vias {
        if via == commodity || via == currency {
            continue;
        }
        let a = price_in(conn, commodity, &via, date)?;
        let b = price_in(conn, currency, &via, date)?;
        if let (Some((a, source_a)), Some((b, source_b))) = (a, b) {
            if !b.is_zero() {
                return Ok(Some(((a / b).round_dp(12), cross_source(&source_a, &source_b))));
            }
        }
    }
    Ok(None)
}

/// Price of `commodity` in `currency`, direct or inverse, latest on or before `date`, with the
/// source of the stored row.
fn price_in(conn: &Connection, commodity: &str, currency: &str, date: NaiveDate) -> Result<Option<(Decimal, String)>> {
    if let Some((p, _, source)) = latest_price(conn, commodity, currency, date)? {
        return Ok(Some((p, source)));
    }
    if let Some((p, _, source)) = latest_price(conn, currency, commodity, date)? {
        if !p.is_zero() {
            return Ok(Some((Decimal::ONE / p, source)));
        }
    }
    Ok(None)
}

fn latest_price(conn: &Connection, commodity: &str, currency: &str, date: NaiveDate) -> Result<Option<(Decimal, NaiveDate, String)>> {
    let row: Option<(String, String, String)> = conn
        .query_row(
            "SELECT p.price, p.on_date, p.source FROM prices p
             JOIN commodities c ON c.id = p.commodity_id JOIN commodities cur ON cur.id = p.currency_id
             WHERE c.code = ?1 AND cur.code = ?2 AND p.on_date <= ?3 ORDER BY p.on_date DESC LIMIT 1",
            params![commodity, currency, date.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    match row {
        Some((p, on, source)) => {
            let on = NaiveDate::parse_from_str(&on, "%Y-%m-%d").map_err(|e| Error::Parse(e.to_string()))?;
            if (date - on).num_days() > MAX_STALE_DAYS {
                return Ok(None);
            }
            Ok(Some((money::parse(&p)?, on, source)))
        }
        None => Ok(None),
    }
}

/// Store cross rates for `pairs` (base, quote) on each day from `from` to `to` that has a
/// stored rate. The engine stores a pair only on a day where the lookup derives it through a
/// common quote. A day with a direct or an inverse row keeps that row. Each stored row has the
/// source of the cross rate, for example `cross:ECB`, so reports and snapshots see it as a
/// derived rate. Returns the number of rows written.
pub fn store_cross_pairs(conn: &mut Connection, pairs: &[(String, String)], from: NaiveDate, to: NaiveDate) -> Result<usize> {
    if to < from {
        return Err(Error::Invalid(format!("the range ends on {to}, before it starts on {from}")));
    }
    let mut checked = Vec::with_capacity(pairs.len());
    for (base, quote) in pairs {
        let base = currency_code("base", base)?;
        let quote = currency_code("quote", quote)?;
        if base == quote {
            return Err(Error::Invalid(format!("base and quote are the same currency ({base})")));
        }
        checked.push((base, quote));
    }
    let tx = conn.transaction()?;
    let days: Vec<String> = {
        let mut stmt = tx.prepare("SELECT DISTINCT on_date FROM prices WHERE on_date >= ?1 AND on_date <= ?2 ORDER BY on_date")?;
        let rows = stmt.query_map(params![from.to_string(), to.to_string()], |r| r.get(0))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let mut written = 0usize;
    for day in days {
        let day = NaiveDate::parse_from_str(&day, "%Y-%m-%d").map_err(|e| Error::Parse(e.to_string()))?;
        for (base, quote) in &checked {
            // A stored cross row of an earlier run must not hide a fresh derivation.
            if latest_price(&tx, base, quote, day)?.is_some_and(|(_, on, source)| on == day && !source.starts_with(CROSS_PREFIX)) {
                continue;
            }
            if latest_price(&tx, quote, base, day)?.is_some_and(|(_, on, _)| on == day) {
                continue;
            }
            if let Some((rate, source)) = cross_rate(&tx, base, quote, day)? {
                set_price(&tx, base, quote, day, rate, &source)?;
                written += 1;
            }
        }
    }
    tx.commit()?;
    Ok(written)
}

/// Parse a list of pairs such as `USD/SGD,GBP/SGD`.
pub fn parse_pairs(text: &str) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let Some((base, quote)) = part.split_once('/') else {
            return Err(Error::Invalid(format!("a pair looks like USD/SGD, got {part:?}")));
        };
        out.push((currency_code("base", base)?, currency_code("quote", quote)?));
    }
    if out.is_empty() {
        return Err(Error::Invalid("give at least one pair, such as USD/SGD".into()));
    }
    Ok(out)
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

/// Store one exchange rate entered by hand: one unit of `base` costs `rate` units of `quote` on `on`.
/// The row goes in `prices` with the base as the commodity and the quote as the currency, so
/// `rate_for` finds it directly, as the inverse, or through a cross rate.
pub fn set_rate(conn: &Connection, on: NaiveDate, base: &str, quote: &str, rate: Decimal, source: &str) -> Result<()> {
    let base = currency_code("base", base)?;
    let quote = currency_code("quote", quote)?;
    if base == quote {
        return Err(Error::Invalid(format!("base and quote are the same currency ({base})")));
    }
    let source = source.trim();
    if source.is_empty() {
        return Err(Error::Invalid("the source must not be empty".into()));
    }
    set_price(conn, &base, &quote, on, rate, source)
}

fn currency_code(field: &str, code: &str) -> Result<String> {
    let code = code.trim().to_ascii_uppercase();
    if code.is_empty() || !code.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(Error::Invalid(format!("{field} must be a currency code such as EUR, got {code:?}")));
    }
    Ok(code)
}

/// Import exchange rates from CSV text with the header `date,base,quote,rate`. The columns can
/// come in any order. Each row is one `set_rate`. The import writes all rows or none.
/// Returns the number of rows written.
pub fn import_rates_csv(conn: &mut Connection, text: &str, source: &str) -> Result<usize> {
    let mut reader = csv::ReaderBuilder::new().trim(csv::Trim::All).flexible(false).from_reader(text.as_bytes());
    let headers: Vec<String> = reader.headers().map_err(|e| Error::Parse(format!("rates CSV header: {e}")))?.iter().map(|h| h.trim().to_ascii_lowercase()).collect();
    let column = |name: &str| headers.iter().position(|h| h == name).ok_or_else(|| Error::Invalid(format!("rates CSV needs the columns date,base,quote,rate; {name} is missing")));
    let (date_i, base_i, quote_i, rate_i) = (column("date")?, column("base")?, column("quote")?, column("rate")?);
    let tx = conn.transaction()?;
    let mut count = 0usize;
    for (i, record) in reader.records().enumerate() {
        let line = i + 2;
        let record = record.map_err(|e| Error::Parse(format!("rates CSV line {line}: {e}")))?;
        if record.iter().all(|f| f.is_empty()) {
            continue;
        }
        let on = crate::parse_date(&record[date_i]).map_err(|_| Error::Invalid(format!("rates CSV line {line}: date must be YYYY-MM-DD, got {:?}", &record[date_i])))?;
        let rate = money::parse(&record[rate_i]).map_err(|_| Error::Invalid(format!("rates CSV line {line}: rate must be a decimal number, got {:?}", &record[rate_i])))?;
        set_rate(&tx, on, &record[base_i], &record[quote_i], rate, source).map_err(|e| Error::Invalid(format!("rates CSV line {line}: {e}")))?;
        count += 1;
    }
    tx.commit()?;
    Ok(count)
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
        assert_eq!(usd_in_brl.source, "cross:ECB");
        assert_eq!(usd_in_brl.rate.round_dp(4), Decimal::from_str("5.4386").unwrap());
        let later = rate_for(&conn, "USD", "EUR", NaiveDate::from_ymd_opt(2026, 9, 10).unwrap()).unwrap().unwrap();
        assert_eq!(later.source, "nearest");
        assert!(rate_for(&conn, "USD", "EUR", NaiveDate::from_ymd_opt(2020, 1, 1).unwrap()).unwrap().is_none());
    }

    #[test]
    fn cross_rate_through_eur_needs_both_legs() {
        let db = Db::open_memory().unwrap();
        let mut conn = db.conn();
        // ECB quotes: 1 EUR = 1.10 USD and 1 EUR = 1.45 SGD on 2026-03-02. USD only on 2026-03-03.
        let xml = r#"<gesmes:Envelope><Cube><Cube time="2026-03-03"><Cube currency="USD" rate="1.1200"/></Cube>
            <Cube time="2026-03-02"><Cube currency="USD" rate="1.1000"/><Cube currency="SGD" rate="1.4500"/></Cube></Cube></gesmes:Envelope>"#;
        import_ecb_xml(&mut conn, xml, None).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 3, 2).unwrap();
        let usd_sgd = rate_for(&conn, "USD", "SGD", d).unwrap().unwrap();
        assert_eq!(usd_sgd.source, "cross:ECB");
        assert_eq!(usd_sgd.rate.round_dp(6), Decimal::from_str("1.318182").unwrap());
        // On 2026-03-03 the SGD leg comes from the nearest earlier day.
        let next = rate_for(&conn, "USD", "SGD", d.succ_opt().unwrap()).unwrap().unwrap();
        assert_eq!(next.rate.round_dp(6), Decimal::from_str("1.294643").unwrap());
        // A missing leg gives no rate.
        assert!(rate_for(&conn, "USD", "IDR", d).unwrap().is_none());
        assert!(rate_for(&conn, "USD", "SGD", NaiveDate::from_ymd_opt(2026, 3, 1).unwrap()).unwrap().is_none());

        // Stored pairs keep the cross source, and the snapshot of prices sees them as rows.
        let pairs = parse_pairs("usd/sgd, SGD/USD").unwrap();
        let written = store_cross_pairs(&mut conn, &pairs, d, d.succ_opt().unwrap()).unwrap();
        assert_eq!(written, 2, "SGD/USD is skipped: the stored USD/SGD row is its inverse");
        let rows = list_prices(&conn, "USD", "SGD", None, None, 0).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.source == "cross:ECB"));
        let stored = rate_for(&conn, "USD", "SGD", d).unwrap().unwrap();
        assert_eq!((stored.source.as_str(), stored.rate.round_dp(6)), ("cross:ECB", Decimal::from_str("1.318182").unwrap()));
        assert!(parse_pairs("USD").is_err());
        assert!(store_cross_pairs(&mut conn, &parse_pairs("USD/USD").unwrap(), d, d).is_err());
    }

    #[test]
    fn manual_rates_and_csv_import() {
        let db = Db::open_memory().unwrap();
        let mut conn = db.conn();
        let d = NaiveDate::from_ymd_opt(2026, 3, 2).unwrap();
        set_rate(&conn, d, "eur", "usd", Decimal::from_str("1.0850").unwrap(), "manual").unwrap();
        let direct = rate_for(&conn, "EUR", "USD", d).unwrap().unwrap();
        assert_eq!((direct.rate, direct.source.as_str()), (Decimal::from_str("1.0850").unwrap(), "exact"));
        let inverse = rate_for(&conn, "USD", "EUR", d).unwrap().unwrap();
        assert_eq!(inverse.source, "inverse");
        assert!(set_rate(&conn, d, "EUR", "EUR", Decimal::ONE, "manual").is_err());
        assert!(set_rate(&conn, d, "EUR", "USD", Decimal::ZERO, "manual").is_err());

        let csv = "date,base,quote,rate\n2026-03-03,EUR,USD,1.0900\n2026-03-03,EUR,GBP,0.8400\n\n";
        assert_eq!(import_rates_csv(&mut conn, csv, "csv").unwrap(), 2);
        let listed = list_prices(&conn, "EUR", "", None, None, 0).unwrap();
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0].source, "csv");
        // A bad row stops the import, and the import writes nothing.
        let bad = "date,base,quote,rate\n2026-03-04,EUR,USD,1.1\n2026-03-05,EUR,USD,abc\n";
        let err = import_rates_csv(&mut conn, bad, "csv").unwrap_err().to_string();
        assert!(err.contains("line 3"), "{err}");
        assert_eq!(list_prices(&conn, "EUR", "USD", None, None, 0).unwrap().len(), 2);
        assert!(import_rates_csv(&mut conn, "day,base,quote,rate\n", "csv").is_err());
    }
}
