//! Reports: queries over postings, nothing stored.

use crate::accounts::{self, AccountBrief};
use crate::entities;
use crate::model::*;
use crate::money::{self, MinorUnits, Money};
use crate::rates;
use crate::{Error, Result};
use chrono::NaiveDate;
use rusqlite::{params, Connection};
use rust_decimal::Decimal;
use std::collections::HashMap;

fn normal(value: Money, kind: AccountType) -> Result<Money> {
    if kind.normal_sign() == 1 {
        Ok(value)
    } else {
        value.checked_neg()
    }
}

fn unit(conn: &Connection, code: &str) -> Result<Money> {
    let precision = match entities::get_commodity_by_code(conn, code) {
        Ok(c) => c.precision,
        Err(Error::NotFound(_)) => money::default_precision(code),
        Err(e) => return Err(e),
    };
    Money::from_minor(0, code, precision)
}

fn converted(value: Money, rate: Decimal, target: Money) -> Result<Money> {
    let major = value.major().checked_mul(rate).ok_or_else(|| Error::Invalid("report conversion overflow".into()))?;
    Money::from_major(major, target.commodity(), target.precision())
}

/// Exact sums retain both account and functional units at the SQL boundary.
fn sums(conn: &Connection, entity_id: Option<i64>, from: Option<NaiveDate>, to: Option<NaiveDate>, include_drafts: bool) -> Result<HashMap<i64, (Money, Money)>> {
    let mut stmt = conn.prepare(
        "SELECT p.account_id, COALESCE(SUM(p.quantity), 0), COALESCE(SUM(p.amount), 0), ac.precision, fc.precision, ac.code, fc.code
         FROM postings p JOIN journal_entries e ON e.id = p.journal_entry_id
              JOIN accounts a ON a.id = p.account_id
              JOIN commodities ac ON ac.id = a.commodity_id
              JOIN entities en ON en.id = a.entity_id
              JOIN commodities fc ON fc.code = en.currency
         WHERE (e.status IN ('posted','void') OR (?4 = 1 AND e.status = 'draft')) AND (?1 IS NULL OR e.entity_id = ?1)
           AND (?2 IS NULL OR e.date >= ?2) AND (?3 IS NULL OR e.date <= ?3)
         GROUP BY p.account_id",
    )?;
    let rows = stmt.query_map(params![entity_id, from.map(|d| d.to_string()), to.map(|d| d.to_string()), include_drafts as i64], |r| {
        Ok((r.get::<_, i64>(0)?, (money::from_row(r, 1, 5, 3)?, money::from_row(r, 2, 6, 4)?)))
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn ordered(index: &HashMap<i64, AccountBrief>, entity_id: Option<i64>) -> Vec<AccountBrief> {
    let mut v: Vec<_> = index.values().filter(|a| entity_id.map(|e| a.entity_id == e).unwrap_or(true)).cloned().collect();
    v.sort_by(|x, y| (x.entity_id, type_order(x.r#type), &x.path).cmp(&(y.entity_id, type_order(y.r#type), &y.path)));
    v
}
fn type_order(t: AccountType) -> u8 {
    match t {
        AccountType::Asset => 0,
        AccountType::Liability => 1,
        AccountType::Equity => 2,
        AccountType::Income => 3,
        AccountType::Expense => 4,
    }
}
fn account_sums(sums: &HashMap<i64, (Money, Money)>, a: &AccountBrief, functional: Money) -> Result<(Money, Money)> {
    match sums.get(&a.id) {
        Some(values) => Ok(*values),
        None => Ok((Money::from_minor(0, &a.commodity, a.precision)?, functional)),
    }
}
fn row(a: &AccountBrief, quantity: Money, amount: Money, zero: Money) -> ReportRow {
    ReportRow {
        account_id: a.id,
        path: a.path.clone(),
        name: a.name.clone(),
        r#type: a.r#type,
        depth: a.depth,
        placeholder: a.placeholder,
        quantity,
        amount,
        debit: zero,
        credit: zero,
        market_value: None,
    }
}
fn synthetic(id: i64, name: String, path: String, amount: Money, zero: Money) -> ReportRow {
    ReportRow { account_id: id, name, path, r#type: AccountType::Equity, depth: 0, placeholder: false, quantity: amount, amount, debit: zero, credit: zero, market_value: None }
}

/// Sum rounded account values. Mixed-commodity parents retain only matching
/// quantities, while their book and market amounts share the reporting unit.
fn rollup(rows: &mut [ReportRow], index: &HashMap<i64, AccountBrief>) -> Result<()> {
    let pos: HashMap<_, _> = rows.iter().enumerate().map(|(i, r)| (r.account_id, i)).collect();
    let leaves: Vec<_> = rows.iter().map(|r| (r.account_id, r.quantity, r.amount, r.market_value)).collect();
    for (id, q, amount, market) in leaves {
        let mut parent = index.get(&id).and_then(|b| b.parent_id);
        while let Some(pid) = parent {
            if let Some(&i) = pos.get(&pid) {
                let r = &mut rows[i];
                r.amount = r.amount.checked_add(amount)?;
                if r.quantity.commodity() == q.commodity() {
                    r.quantity = r.quantity.checked_add(q)?;
                }
                r.market_value = match (r.market_value, market) {
                    (Some(total), value) => Some(total.checked_add(value.unwrap_or(amount))?),
                    (None, Some(value)) => Some(r.amount.checked_add(value.checked_sub(amount)?)?),
                    (None, None) => None,
                };
            }
            parent = index.get(&pid).and_then(|b| b.parent_id);
        }
    }
    Ok(())
}

pub struct Report {
    pub rows: Vec<ReportRow>,
    pub total_debit: Money,
    pub total_credit: Money,
    pub net: Money,
    pub currency: String,
    pub summary: Vec<ReportRow>,
}

pub fn trial_balance(conn: &Connection, entity_id: i64, as_of: Option<NaiveDate>) -> Result<Report> {
    let entity = entities::get_entity(conn, entity_id)?;
    let zero = unit(conn, &entity.currency)?;
    let index = accounts::path_index(conn)?;
    let sums = sums(conn, Some(entity_id), None, as_of, false)?;
    let mut rows = Vec::new();
    for a in ordered(&index, Some(entity_id)).into_iter().filter(|a| !a.placeholder) {
        let (q, amount) = account_sums(&sums, &a, zero)?;
        if q.is_zero() && amount.is_zero() {
            continue;
        }
        let mut r = row(&a, normal(q, a.r#type)?, normal(amount, a.r#type)?, zero);
        if amount.is_negative() {
            r.credit = amount.checked_neg()?;
        } else {
            r.debit = amount;
        }
        rows.push(r);
    }
    let debit = zero.checked_sum(rows.iter().map(|r| r.debit))?;
    let credit = zero.checked_sum(rows.iter().map(|r| r.credit))?;
    Ok(Report { rows, total_debit: debit, total_credit: credit, net: debit.checked_sub(credit)?, currency: entity.currency, summary: vec![] })
}

/// Convert and round each account before aggregation so totals equal displayed rows.
pub fn balance_sheet(conn: &Connection, entity_id: Option<i64>, as_of: Option<NaiveDate>, currency: &str) -> Result<Report> {
    let index = accounts::path_index(conn)?;
    let on = as_of.unwrap_or_else(crate::today);
    let entities = match entity_id {
        Some(id) => vec![entities::get_entity(conn, id)?],
        None => entities::list_entities(conn, false)?,
    };
    let code = if currency.trim().is_empty() { entities.first().map(|e| e.currency.clone()).unwrap_or_else(|| "EUR".into()) } else { entities::normalize_code(currency)? };
    let zero = unit(conn, &code)?;
    let mut rows = Vec::new();
    let mut summary = Vec::new();
    for entity in &entities {
        let functional = unit(conn, &entity.currency)?;
        let sums = sums(conn, Some(entity.id), None, as_of, false)?;
        let rate = if entity.currency == code || sums.values().all(|(quantity, amount)| quantity.is_zero() && amount.is_zero()) {
            Decimal::ONE
        } else {
            rates::rate_for(conn, &entity.currency, &code, on)?
                .ok_or_else(|| Error::Invalid(format!("missing exchange rate from {} to {code} on {on} for entity {}", entity.currency, entity.name)))?
                .rate
        };
        let mut entity_rows = Vec::new();
        let mut profits = Vec::new();
        for a in ordered(&index, Some(entity.id)) {
            let (q, amount) = account_sums(&sums, &a, functional)?;
            let amount = converted(amount, rate, zero)?;
            match a.r#type {
                AccountType::Income | AccountType::Expense => profits.push(amount.checked_neg()?),
                _ => {
                    let mut r = row(&a, normal(q, a.r#type)?, normal(amount, a.r#type)?, zero);
                    if a.subtype == "holding" && !q.is_zero() {
                        if let Some(price) = rates::rate_for(conn, &a.commodity, &entity.currency, on)? {
                            let price = price.rate.checked_mul(rate).ok_or_else(|| Error::Invalid("report conversion overflow".into()))?;
                            r.market_value = Some(normal(converted(q, price, zero)?, a.r#type)?);
                        }
                    }
                    entity_rows.push(r);
                }
            }
        }
        rollup(&mut entity_rows, &index)?;
        let retained = zero.checked_sum(profits)?;
        let assets = zero.checked_sum(entity_rows.iter().filter(|r| r.depth == 0 && r.r#type == AccountType::Asset).map(|r| r.market_value.unwrap_or(r.amount)))?;
        let liabilities = zero.checked_sum(entity_rows.iter().filter(|r| r.depth == 0 && r.r#type == AccountType::Liability).map(|r| r.amount))?;
        let gains = entity_rows.iter().filter(|r| r.depth == 0).filter_map(|r| r.market_value.map(|v| v.checked_sub(r.amount))).collect::<Result<Vec<_>>>()?;
        let unrealised = zero.checked_sum(gains)?;
        entity_rows.retain(|r| !r.quantity.is_zero() || !r.amount.is_zero() || r.market_value.is_some_and(|v| !v.is_zero()));
        rows.extend(entity_rows);
        rows.push(synthetic(0, "Retained earnings".into(), format!("Equity:Retained earnings ({})", entity.name), retained, zero));
        if !unrealised.is_zero() {
            rows.push(synthetic(0, "Unrealised gains".into(), format!("Equity:Unrealised gains ({})", entity.name), unrealised, zero));
        }
        let mut total = synthetic(entity.id, entity.name.clone(), entity.name.clone(), assets.checked_sub(liabilities)?, zero);
        total.debit = assets;
        total.credit = liabilities;
        summary.push(total);
    }
    summary.sort_by(|a, b| a.name.cmp(&b.name));
    let assets = zero.checked_sum(summary.iter().map(|r| r.debit))?;
    let liabilities = zero.checked_sum(summary.iter().map(|r| r.credit))?;
    Ok(Report { rows, total_debit: assets, total_credit: liabilities, net: assets.checked_sub(liabilities)?, currency: code, summary })
}

pub fn income_statement(conn: &Connection, entity_id: i64, from: Option<NaiveDate>, to: Option<NaiveDate>) -> Result<Report> {
    let entity = entities::get_entity(conn, entity_id)?;
    let zero = unit(conn, &entity.currency)?;
    let index = accounts::path_index(conn)?;
    let sums = sums(conn, Some(entity_id), from, to, false)?;
    let mut rows = Vec::new();
    for a in ordered(&index, Some(entity_id)).into_iter().filter(|a| matches!(a.r#type, AccountType::Income | AccountType::Expense)) {
        let (q, amount) = account_sums(&sums, &a, zero)?;
        rows.push(row(&a, normal(q, a.r#type)?, normal(amount, a.r#type)?, zero));
    }
    rollup(&mut rows, &index)?;
    let income = zero.checked_sum(rows.iter().filter(|r| r.depth == 0 && r.r#type == AccountType::Income).map(|r| r.amount))?;
    let expenses = zero.checked_sum(rows.iter().filter(|r| r.depth == 0 && r.r#type == AccountType::Expense).map(|r| r.amount))?;
    rows.retain(|r| !r.amount.is_zero() || !r.quantity.is_zero());
    Ok(Report { rows, total_debit: expenses, total_credit: income, net: income.checked_sub(expenses)?, currency: entity.currency, summary: vec![] })
}

pub struct Ledger {
    pub rows: Vec<LedgerRow>,
    pub opening_balance: Money,
    pub closing_balance: Money,
    pub commodity: String,
}

/// Every posting on one account in a period, with a running balance in the account's commodity.
pub fn general_ledger(conn: &Connection, account_id: i64, from: Option<NaiveDate>, to: Option<NaiveDate>, limit: i64, include_drafts: bool) -> Result<Ledger> {
    general_ledger_ordered(conn, account_id, from, to, limit, include_drafts, false)
}

/// Recent windows retain chronological running balances, even when returned newest first.
pub fn general_ledger_ordered(conn: &Connection, account_id: i64, from: Option<NaiveDate>, to: Option<NaiveDate>, limit: i64, include_drafts: bool, newest_first: bool) -> Result<Ledger> {
    let account = accounts::get_account(conn, account_id)?;
    let index = accounts::path_index(conn)?;
    let fprec = entities::functional_precision(conn, account.entity_id)?;
    let entity = entities::get_entity(conn, account.entity_id)?;
    let normal = |value: Money| if account.r#type.normal_sign() == 1 { Ok(value) } else { value.checked_neg() };
    let zero = Money::from_minor(0, &account.commodity, account.precision)?;
    let mut opening: i64 = match from {
        Some(f) => conn.query_row(
            "SELECT COALESCE(SUM(p.quantity), 0) FROM postings p JOIN journal_entries e ON e.id = p.journal_entry_id WHERE p.account_id = ?1 AND e.status IN ('posted','void') AND e.date < ?2",
            params![account_id, f.to_string()],
            |r| r.get(0),
        )?,
        None => 0,
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT p.id, e.id, e.date, e.payee, e.description, e.status, p.quantity, p.amount, p.reconciled_at IS NOT NULL,
                (SELECT s.id FROM statement_lines s WHERE s.posting_id = p.id LIMIT 1),
                (SELECT COUNT(*) FROM postings o WHERE o.journal_entry_id = e.id),
                (SELECT o.account_id FROM postings o WHERE o.journal_entry_id = e.id AND o.id <> p.id ORDER BY ABS(o.amount) DESC LIMIT 1)
         FROM postings p JOIN journal_entries e ON e.id = p.journal_entry_id
         WHERE p.account_id = ?1 AND (e.status IN ('posted','void') OR (?4 = 1 AND e.status = 'draft'))
           AND (?2 IS NULL OR e.date >= ?2) AND (?3 IS NULL OR e.date <= ?3)
         ORDER BY e.date {order}, e.id {order}, p.position {order} LIMIT ?5",
        order = if newest_first { "DESC" } else { "ASC" }
    ))?;
    let rows = stmt.query_map(params![account_id, from.map(|d| d.to_string()), to.map(|d| d.to_string()), include_drafts as i64, if limit <= 0 { 5000 } else { limit }], |r| {
        let d: String = r.get(2)?;
        let st: String = r.get(5)?;
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            NaiveDate::parse_from_str(&d, "%Y-%m-%d").unwrap_or_default(),
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            EntryStatus::parse(&st).unwrap_or(EntryStatus::Posted),
            r.get::<_, i64>(6)?,
            r.get::<_, i64>(7)?,
            r.get::<_, i64>(8)? != 0,
            r.get::<_, Option<i64>>(9)?,
            r.get::<_, i64>(10)?,
            r.get::<_, Option<i64>>(11)?,
        ))
    })?;
    let mut rows = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    if newest_first {
        let total: i64 = conn.query_row(
            "SELECT COALESCE(SUM(p.quantity),0) FROM postings p JOIN journal_entries e ON e.id=p.journal_entry_id WHERE p.account_id=?1 AND e.status IN ('posted','void') AND (?2 IS NULL OR e.date>=?2) AND (?3 IS NULL OR e.date<=?3)",
            params![account_id, from.map(|d|d.to_string()), to.map(|d|d.to_string())], |r|r.get(0))?;
        let selected: i128 = rows.iter().filter(|r| r.5 != EntryStatus::Draft).map(|r| i128::from(r.6)).sum();
        opening = i64::try_from(i128::from(opening) + i128::from(total) - selected).map_err(|_| crate::Error::Invalid("ledger balance overflow".into()))?;
        rows.reverse();
    }
    let opening_balance = normal(Money::from_minor(opening, &account.commodity, account.precision)?)?;
    let mut running = opening_balance;
    let mut out = Vec::new();
    for r in rows {
        let (pid, eid, date, payee, description, status, raw_q, raw_amt, reconciled, sl, n, contra) = r;
        let q = Money::from_minor(raw_q, &account.commodity, account.precision)?;
        let amt = Money::from_minor(raw_amt, &entity.currency, fprec)?;
        if status != EntryStatus::Draft {
            running = running.checked_add(normal(q)?)?;
        }
        let contra_path = if n > 2 { "(split)".to_string() } else { contra.and_then(|c| index.get(&c)).map(|b| b.path.clone()).unwrap_or_default() };
        out.push(LedgerRow {
            posting_id: pid,
            journal_entry_id: eid,
            date,
            payee,
            display_payee: conn.query_row("SELECT COALESCE((SELECT name FROM payees WHERE id=e.payee_id),e.payee) FROM journal_entries e WHERE e.id=?1", [eid], |r| r.get(0))?,
            description,
            status,
            contra_path,
            debit: if !q.is_negative() { q } else { zero },
            credit: if q.is_negative() { q.checked_neg()? } else { zero },
            running_balance: running,
            amount: amt,
            reconciled,
            statement_line_id: sl,
        });
    }
    let closing = out.last().map(|r| r.running_balance).unwrap_or(opening_balance);
    if newest_first {
        out.reverse();
    }
    Ok(Ledger { rows: out, opening_balance, closing_balance: closing, commodity: account.commodity })
}

pub struct Reconciliation {
    pub statement_balance: Option<Decimal>,
    pub statement_date: Option<NaiveDate>,
    pub ledger_balance: Decimal,
    pub difference: Option<Decimal>,
    pub unmatched_lines: Vec<StatementLine>,
    pub unreconciled: Vec<LedgerRow>,
}

/// Statement balance versus ledger balance for one account.
pub fn reconciliation(conn: &Connection, account_id: i64) -> Result<Reconciliation> {
    let account = accounts::get_account(conn, account_id)?;
    let latest: Option<(String, Option<String>)> = {
        use rusqlite::OptionalExtension;
        conn.query_row(
            "SELECT closing_balance, options FROM imports WHERE account_id = ?1 AND closing_balance IS NOT NULL ORDER BY COALESCE(period_to, created_at) DESC, id DESC LIMIT 1",
            [account_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
    };
    let (statement_balance, statement_date) = match latest {
        Some((bal, options)) => {
            let date = options
                .and_then(|o| serde_json::from_str::<serde_json::Value>(&o).ok())
                .and_then(|v| v.get("closing_date").and_then(|d| d.as_str()).and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()));
            (money::parse(&bal).ok(), date)
        }
        None => (None, None),
    };
    let as_of = statement_date;
    let raw: i64 = conn.query_row(
        "SELECT COALESCE(SUM(p.quantity), 0) FROM postings p JOIN journal_entries e ON e.id = p.journal_entry_id WHERE p.account_id = ?1 AND e.status IN ('posted','void') AND (?2 IS NULL OR e.date <= ?2)",
        params![account_id, as_of.map(|d| d.to_string())],
        |r| r.get(0),
    )?;
    // Statement balances are shown from the bank's point of view: positive when the bank owes you.
    // For a card, the bank shows what you owe as negative; the ledger's normal balance is positive, so flip.
    let ledger_balance = MinorUnits::from_minor(raw, account.precision)?.major();
    let difference = statement_balance.map(|s| s - ledger_balance);
    let unmatched_lines = crate::imports::list_lines(conn, Some(account_id), "unmatched", None, 500)?;
    let ledger = general_ledger(conn, account_id, None, as_of, 5000, false)?;
    let unreconciled: Vec<LedgerRow> = ledger.rows.into_iter().filter(|r| !r.reconciled).collect();
    let _ = account;
    Ok(Reconciliation { statement_balance, statement_date, ledger_balance, difference, unmatched_lines, unreconciled })
}

#[derive(Debug, Clone)]
pub struct CashflowDay {
    pub date: NaiveDate,
    pub posted_in: Decimal,
    pub posted_out: Decimal,
    pub draft_in: Decimal,
    pub draft_out: Decimal,
    pub balance: Decimal,
}

/// Day by day movement on bank, cash and card accounts, posted and draft, with a projected balance.
pub fn cashflow(conn: &Connection, entity_id: i64, from: NaiveDate, to: NaiveDate) -> Result<Vec<CashflowDay>> {
    let fprec = entities::functional_precision(conn, entity_id)?;
    let opening: i64 = conn.query_row(
        "SELECT COALESCE(SUM(p.amount), 0) FROM postings p JOIN journal_entries e ON e.id = p.journal_entry_id JOIN accounts a ON a.id = p.account_id
         WHERE e.entity_id = ?1 AND e.status IN ('posted','void') AND e.date < ?2 AND a.subtype IN ('bank','cash','card','wallet')",
        params![entity_id, from.to_string()],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare(
        "SELECT e.date, e.status, COALESCE(SUM(CASE WHEN p.amount > 0 THEN p.amount ELSE 0 END), 0), COALESCE(SUM(CASE WHEN p.amount < 0 THEN -p.amount ELSE 0 END), 0)
         FROM postings p JOIN journal_entries e ON e.id = p.journal_entry_id JOIN accounts a ON a.id = p.account_id
         WHERE e.entity_id = ?1 AND e.status IN ('posted','void','draft') AND e.date >= ?2 AND e.date <= ?3 AND a.subtype IN ('bank','cash','card','wallet')
         GROUP BY e.date, e.status ORDER BY e.date",
    )?;
    let rows = stmt.query_map(params![entity_id, from.to_string(), to.to_string()], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?)))?;
    let mut days: std::collections::BTreeMap<NaiveDate, CashflowDay> = std::collections::BTreeMap::new();
    for r in rows {
        let (d, status, inn, out) = r?;
        let date = NaiveDate::parse_from_str(&d, "%Y-%m-%d").unwrap_or_default();
        let day = days.entry(date).or_insert(CashflowDay { date, posted_in: Decimal::ZERO, posted_out: Decimal::ZERO, draft_in: Decimal::ZERO, draft_out: Decimal::ZERO, balance: Decimal::ZERO });
        if status != "draft" {
            day.posted_in += MinorUnits::from_minor(inn, fprec)?.major();
            day.posted_out += MinorUnits::from_minor(out, fprec)?.major();
        } else {
            day.draft_in += MinorUnits::from_minor(inn, fprec)?.major();
            day.draft_out += MinorUnits::from_minor(out, fprec)?.major();
        }
    }
    let mut balance = MinorUnits::from_minor(opening, fprec)?.major();
    let mut out = Vec::new();
    for (_, mut day) in days {
        balance += day.posted_in - day.posted_out + day.draft_in - day.draft_out;
        day.balance = balance;
        out.push(day);
    }
    Ok(out)
}

pub struct NetWorth {
    pub by_entity: Vec<ReportRow>,
    pub by_account: Vec<ReportRow>,
    pub total: Money,
    pub currency: String,
}

/// Net worth of one vault at a date. Other vaults do not imply ownership.
pub fn net_worth(conn: &Connection, entity_id: i64, currency: &str, as_of: Option<NaiveDate>) -> Result<NetWorth> {
    if entity_id <= 0 {
        return Err(Error::Invalid("select a vault for net worth".into()));
    }
    let bs = balance_sheet(conn, Some(entity_id), as_of, currency)?;
    let by_account: Vec<ReportRow> = bs.rows.iter().filter(|r| r.account_id != 0 && !r.placeholder && matches!(r.r#type, AccountType::Asset | AccountType::Liability)).cloned().collect();
    let total = bs.net;
    Ok(NetWorth { by_entity: bs.summary, by_account, total, currency: bs.currency })
}

#[derive(Debug, serde::Serialize)]
pub struct ClassExpenseRow {
    /// Empty means the account has no class.
    pub class: String,
    pub amount: Money,
}

#[derive(Debug, serde::Serialize)]
pub struct ClassExpenseReport {
    pub entity_id: i64,
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub tag: Option<String>,
    pub rows: Vec<ClassExpenseRow>,
    pub total: Money,
}

#[derive(Debug, serde::Serialize)]
pub struct TagExpenseRow {
    /// None is the untagged group, not a literal tag name.
    pub tag: Option<String>,
    pub amount: Money,
}

#[derive(Debug, serde::Serialize)]
pub struct TagExpenseReport {
    pub entity_id: i64,
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub tag: Option<String>,
    pub rows: Vec<TagExpenseRow>,
    /// Each expense posting counted once, regardless of its tags.
    pub total: Money,
    /// Tag rows count the full amount under every tag and are not additive.
    pub overlapping: bool,
}

/// Group expenses by exact entry tag, including a separate untagged group.
pub fn expenses_by_tag(conn: &Connection, entity_id: i64, from: Option<NaiveDate>, to: Option<NaiveDate>, tag: Option<&str>) -> Result<TagExpenseReport> {
    let base = expenses_by_class(conn, entity_id, from, to, tag)?;
    let filter = base.tag.as_deref().map(|tag| tag.split_once(':').unwrap_or((tag, "")));
    let mut stmt = conn.prepare(
        "SELECT t.key, t.value, SUM(p.amount)
         FROM postings p JOIN journal_entries e ON e.id = p.journal_entry_id
         JOIN accounts a ON a.id = p.account_id
         LEFT JOIN entry_tags t ON t.entry_id = e.id
         WHERE e.entity_id = ?1 AND a.entity_id = ?1 AND a.type = 'expense'
           AND e.status IN ('posted', 'void')
           AND (?2 IS NULL OR e.date >= ?2) AND (?3 IS NULL OR e.date <= ?3)
           AND (?4 IS NULL OR EXISTS (SELECT 1 FROM entry_tags f WHERE f.entry_id = e.id AND f.key = ?4 AND f.value = ?5))
         GROUP BY t.key, t.value ORDER BY t.key, t.value",
    )?;
    let amounts = stmt.query_map(params![entity_id, from.map(|d| d.to_string()), to.map(|d| d.to_string()), filter.map(|f| f.0), filter.map(|f| f.1)], |r| {
        Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, i64>(2)?))
    })?;
    let mut rows = Vec::new();
    for row in amounts {
        let (key, value, minor) = row?;
        if minor != 0 {
            let tag = key.map(|key| match value {
                Some(value) if !value.is_empty() => format!("{key}:{value}"),
                _ => key,
            });
            rows.push(TagExpenseRow { tag, amount: Money::from_minor(minor, base.total.commodity(), base.total.precision())? });
        }
    }
    Ok(TagExpenseReport { entity_id, from, to, tag: base.tag, rows, total: base.total, overlapping: true })
}

/// Sum expense postings once, at their booked functional values.
/// The tag filter selects entries without multiplying their postings.
pub fn expenses_by_class(conn: &Connection, entity_id: i64, from: Option<NaiveDate>, to: Option<NaiveDate>, tag: Option<&str>) -> Result<ClassExpenseReport> {
    if from.zip(to).is_some_and(|(a, b)| a > b) {
        return Err(Error::Invalid("report start date must not follow its end date".into()));
    }
    let entity = entities::get_entity(conn, entity_id)?;
    let zero = unit(conn, &entity.currency)?;
    let filter = tag
        .map(|spec| {
            let mut tags = crate::tags::parse(spec)?;
            if tags.len() != 1 {
                return Err(Error::Invalid("report tag filter requires one tag".into()));
            }
            Ok::<_, Error>(tags.remove(0))
        })
        .transpose()?;
    let mut stmt = conn.prepare(
        "SELECT a.class, COALESCE(SUM(p.amount), 0)
         FROM postings p JOIN journal_entries e ON e.id = p.journal_entry_id
         JOIN accounts a ON a.id = p.account_id
         WHERE e.entity_id = ?1 AND a.entity_id = ?1 AND a.type = 'expense'
           AND e.status IN ('posted', 'void')
           AND (?2 IS NULL OR e.date >= ?2) AND (?3 IS NULL OR e.date <= ?3)
           AND (?4 IS NULL OR EXISTS (SELECT 1 FROM entry_tags t WHERE t.entry_id = e.id AND t.key = ?4 AND t.value = ?5))
         GROUP BY a.class",
    )?;
    let amounts = stmt
        .query_map(params![entity_id, from.map(|d| d.to_string()), to.map(|d| d.to_string()), filter.as_ref().map(|(k, _)| k), filter.as_ref().map(|(_, v)| v)], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut rows = Vec::new();
    let mut total = zero;
    for (class, minor) in amounts {
        let amount = Money::from_minor(minor, zero.commodity(), zero.precision())?;
        total = total.checked_add(amount)?;
        if !amount.is_zero() {
            rows.push(ClassExpenseRow { class, amount });
        }
    }
    let order = ["fixed", "committed", "discretionary", "savings", "not-spending", ""];
    rows.sort_by_key(|r| order.iter().position(|c| *c == r.class).unwrap_or(order.len()));
    let tag = filter.map(|(key, value)| if value.is_empty() { key } else { format!("{key}:{value}") });
    Ok(ClassExpenseReport { entity_id, from, to, tag, rows, total })
}
