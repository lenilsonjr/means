//! Migration 0007: rescale the four money columns in place, from the one scale of 10^8 that
//! D3 used for every commodity to minor units of the commodity each value is in (D14).
//!
//! The hash chain does not move: `hashchain::canonical` renders decimals, so the same books
//! produce the same hashes before and after. A value that is not a whole number of its minor
//! units is a refusal, not a rounding: the migration fails and the ledger stays at version 6.

use rusqlite::{params, Connection};

use crate::money;
use crate::{Error, Result};

/// The decimals every quantity and amount was stored with before D14.
const UNIFORM_DIGITS: u32 = 8;

pub fn rescale(conn: &Connection) -> Result<()> {
    check_precisions(conn)?;
    rescale_postings(conn)?;
    rescale_statement_lines(conn)
}

fn to_minor(stored: i64, precision: u32, code: &str, what: &str) -> Result<i64> {
    let divisor = 10i64.pow(UNIFORM_DIGITS - precision);
    if stored % divisor != 0 {
        return Err(Error::Invalid(format!("{what} is {stored} at the old scale of 10^{UNIFORM_DIGITS}, which is not a whole number of {code} minor units")));
    }
    Ok(stored / divisor)
}

fn check_precisions(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("SELECT code, precision FROM commodities")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    for row in rows {
        let (code, precision) = row?;
        if !(0..=i64::from(money::MAX_PRECISION)).contains(&precision) {
            return Err(Error::Invalid(format!("commodity {code} has precision {precision}; a commodity has 0 to {} decimals", money::MAX_PRECISION)));
        }
    }
    Ok(())
}

/// A commodity of a value, as far as the joins could resolve it.
struct Unit {
    precision: Option<u32>,
    code: Option<String>,
}

impl Unit {
    fn read(r: &rusqlite::Row<'_>, precision: usize, code: usize) -> rusqlite::Result<Unit> {
        Ok(Unit { precision: r.get::<_, Option<i64>>(precision)?.map(|p| p as u32), code: r.get(code)? })
    }

    fn resolved(self, missing: impl FnOnce() -> String) -> Result<(u32, String)> {
        self.precision.zip(self.code).ok_or_else(|| Error::Invalid(missing()))
    }
}

struct PostingRow {
    id: i64,
    quantity: i64,
    amount: i64,
    account: Unit,
    functional: Unit,
}

/// A posting's quantity is in its account's commodity, its amount in the functional currency
/// of the entity the entry belongs to.
fn rescale_postings(conn: &Connection) -> Result<()> {
    let rows: Vec<PostingRow> = {
        let mut stmt = conn.prepare(
            "SELECT p.id, p.quantity, p.amount, ac.precision, ac.code, fc.precision, fc.code
             FROM postings p
             LEFT JOIN accounts a ON a.id = p.account_id
             LEFT JOIN commodities ac ON ac.id = a.commodity_id
             LEFT JOIN journal_entries e ON e.id = p.journal_entry_id
             LEFT JOIN entities en ON en.id = e.entity_id
             LEFT JOIN commodities fc ON fc.code = en.currency",
        )?;
        let rows = stmt.query_map([], |r| Ok(PostingRow { id: r.get(0)?, quantity: r.get(1)?, amount: r.get(2)?, account: Unit::read(r, 3, 4)?, functional: Unit::read(r, 5, 6)? }))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut update = conn.prepare("UPDATE postings SET quantity = ?2, amount = ?3 WHERE id = ?1")?;
    for row in rows {
        let id = row.id;
        let (qprec, qcode) = row.account.resolved(|| format!("posting {id} has no account commodity"))?;
        let (aprec, acode) = row.functional.resolved(|| format!("posting {id} has no functional currency"))?;
        let quantity = to_minor(row.quantity, qprec, &qcode, &format!("the quantity of posting {id}"))?;
        let amount = to_minor(row.amount, aprec, &acode, &format!("the amount of posting {id}"))?;
        update.execute(params![id, quantity, amount])?;
    }
    Ok(())
}

struct LineRow {
    id: i64,
    amount: Option<i64>,
    balance_after: Option<i64>,
    unit: Unit,
}

/// A statement line is in its account's commodity; a line with no account names it in `currency`.
fn rescale_statement_lines(conn: &Connection) -> Result<()> {
    let rows: Vec<LineRow> = {
        let mut stmt = conn.prepare(
            "SELECT s.id, s.amount, s.balance_after, COALESCE(ac.precision, cc.precision), COALESCE(ac.code, cc.code)
             FROM statement_lines s
             LEFT JOIN accounts a ON a.id = s.account_id
             LEFT JOIN commodities ac ON ac.id = a.commodity_id
             LEFT JOIN commodities cc ON cc.code = UPPER(TRIM(s.currency))
             WHERE s.amount IS NOT NULL OR s.balance_after IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| Ok(LineRow { id: r.get(0)?, amount: r.get(1)?, balance_after: r.get(2)?, unit: Unit::read(r, 3, 4)? }))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut update = conn.prepare("UPDATE statement_lines SET amount = ?2, balance_after = ?3 WHERE id = ?1")?;
    for row in rows {
        let id = row.id;
        let (precision, code) = row.unit.resolved(|| format!("statement line {id} has a value but no commodity: it has neither an account nor a currency that names one"))?;
        let amount = row.amount.map(|v| to_minor(v, precision, &code, &format!("the amount of statement line {id}"))).transpose()?;
        let balance_after = row.balance_after.map(|v| to_minor(v, precision, &code, &format!("the balance after statement line {id}"))).transpose()?;
        update.execute(params![id, amount, balance_after])?;
    }
    Ok(())
}
