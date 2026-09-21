//! Budget limits apply once to an inclusive period, in the entity's functional currency.
use crate::{accounts, entities, money::Money, AccountType, Error, Result};
use chrono::{Datelike, NaiveDate};
use rusqlite::{params, Connection, OptionalExtension};
use rust_decimal::Decimal;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Budget {
    pub id: i64,
    pub entity_id: i64,
    pub name: String,
    pub scope: String,
    pub account_id: Option<i64>,
    pub class: Option<String>,
    pub tag: Option<String>,
    pub starts_on: NaiveDate,
    pub ends_on: NaiveDate,
    pub limit: Money,
}

pub struct BudgetInput {
    pub entity_id: i64,
    pub name: String,
    pub scope: String,
    pub account_id: Option<i64>,
    pub class: Option<String>,
    pub tag: Option<String>,
    pub starts_on: Option<NaiveDate>,
    pub ends_on: Option<NaiveDate>,
    pub amount: Decimal,
}

#[derive(Debug, Serialize)]
pub struct BudgetProgress {
    pub budget: Budget,
    pub spent: Money,
    pub remaining: Money,
    /// Category trees crossing classes and tag-only budgets use "mixed".
    pub group: String,
    pub target: String,
}

pub fn month_range(day: NaiveDate) -> (NaiveDate, NaiveDate) {
    let first = day.with_day(1).expect("valid first day");
    let last = (28..=31).rev().find_map(|d| day.with_day(d)).expect("valid last day");
    (first, last)
}

const SELECT: &str = "SELECT b.id, b.entity_id, b.name, b.scope, b.account_id, b.class, b.tag, b.starts_on, b.ends_on, b.amount, e.currency, c.precision FROM budgets b JOIN entities e ON e.id=b.entity_id JOIN commodities c ON c.code=e.currency";
fn date_row(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<NaiveDate> {
    let value: String = r.get(i)?;
    value.parse().map_err(|e| rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e)))
}
fn read(r: &rusqlite::Row<'_>) -> rusqlite::Result<Budget> {
    Ok(Budget {
        id: r.get(0)?,
        entity_id: r.get(1)?,
        name: r.get(2)?,
        scope: r.get(3)?,
        account_id: r.get(4)?,
        class: r.get(5)?,
        tag: r.get(6)?,
        starts_on: date_row(r, 7)?,
        ends_on: date_row(r, 8)?,
        limit: crate::money::from_row(r, 9, 10, 11)?,
    })
}
pub fn get(conn: &Connection, id: i64) -> Result<Budget> {
    conn.query_row(&format!("{SELECT} WHERE b.id=?1"), [id], read).optional()?.ok_or_else(|| Error::NotFound(format!("budget {id}")))
}

/// Both dates omitted defaults to this calendar month; a partial range is rejected.
pub fn save(conn: &mut Connection, id: Option<i64>, input: BudgetInput) -> Result<Budget> {
    let tx = conn.transaction()?;
    let before = id.map(|id| get(&tx, id)).transpose()?;
    if before.as_ref().is_some_and(|b| b.entity_id != input.entity_id) {
        return Err(Error::Invalid("a budget cannot move between entities".into()));
    }
    let entity = entities::get_entity(&tx, input.entity_id)?;
    let currency = entities::get_commodity_by_code(&tx, &entity.currency)?;
    let limit = Money::from_major(input.amount, &entity.currency, currency.precision)?;
    if input.amount < Decimal::ZERO {
        return Err(Error::Invalid("budget limit must be nonnegative".into()));
    }
    let name = input.name.trim();
    if name.is_empty() {
        return Err(Error::Invalid("budget needs a name".into()));
    }
    let (from, to) = match (input.starts_on, input.ends_on) {
        (Some(a), Some(b)) if a <= b => (a, b),
        (None, None) => month_range(crate::today()),
        _ => return Err(Error::Invalid("supply both budget dates, with start on or before end".into())),
    };
    let tag = input
        .tag
        .map(|s| {
            let tags = crate::tags::parse(&s)?;
            if tags.len() != 1 {
                return Err(Error::Invalid("budget tag filter requires one tag".into()));
            }
            let (k, v) = &tags[0];
            Ok(if v.is_empty() { k.clone() } else { format!("{k}:{v}") })
        })
        .transpose()?;
    match input.scope.as_str() {
        "category" if input.class.is_none() && input.account_id.is_some() => {
            let a = accounts::get_account(&tx, input.account_id.unwrap())?;
            if a.entity_id != input.entity_id || a.r#type != AccountType::Expense {
                return Err(Error::Invalid("budget category must be an expense account in this entity".into()));
            }
        }
        "class" if input.account_id.is_none() && input.class.as_deref().is_some_and(|c| ["", "fixed", "committed", "discretionary", "savings", "not-spending"].contains(&c)) => {}
        "tag" if input.account_id.is_none() && input.class.is_none() && tag.is_some() => {}
        _ => return Err(Error::Invalid("budget scope requires a category, a valid class, or a tag-only filter".into())),
    }
    let now = crate::now_ts();
    let id = if let Some(id) = id {
        tx.execute(
            "UPDATE budgets SET name=?2, scope=?3, account_id=?4, class=?5, tag=?6, starts_on=?7, ends_on=?8, amount=?9, updated_at=?10 WHERE id=?1",
            params![id, name, input.scope, input.account_id, input.class, tag, from.to_string(), to.to_string(), limit.minor(), now],
        )?;
        id
    } else {
        tx.execute(
            "INSERT INTO budgets (uid,entity_id,name,scope,account_id,class,tag,starts_on,ends_on,amount,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?11)",
            params![crate::new_uid(), input.entity_id, name, input.scope, input.account_id, input.class, tag, from.to_string(), to.to_string(), limit.minor(), now],
        )?;
        tx.last_insert_rowid()
    };
    let after = get(&tx, id)?;
    crate::audit::log(&tx, "budgets", id, if before.is_some() { "update" } else { "create" }, before.map(serde_json::to_value).transpose()?, Some(serde_json::to_value(&after)?))?;
    tx.commit()?;
    Ok(after)
}

pub fn delete(conn: &mut Connection, id: i64) -> Result<()> {
    let tx = conn.transaction()?;
    let before = get(&tx, id)?;
    tx.execute("DELETE FROM budgets WHERE id=?1", [id])?;
    crate::audit::log(&tx, "budgets", id, "delete", Some(serde_json::to_value(before)?), None)?;
    tx.commit()?;
    Ok(())
}

/// The as-of selector chooses budgets valid on that day; spending always covers their full period.
/// Each budget stands alone: overlapping limits or spending must not be summed.
pub fn list(conn: &Connection, entity_id: i64, as_of: Option<NaiveDate>, tag: Option<&str>) -> Result<Vec<BudgetProgress>> {
    entities::get_entity(conn, entity_id)?;
    let tag = tag
        .map(|s| {
            let t = crate::tags::parse(s)?;
            if t.len() != 1 {
                return Err(Error::Invalid("budget list filter requires one tag".into()));
            }
            Ok(if t[0].1.is_empty() { t[0].0.clone() } else { format!("{}:{}", t[0].0, t[0].1) })
        })
        .transpose()?;
    let mut stmt = conn.prepare(&format!("{SELECT} WHERE b.entity_id=?1 AND (?2 IS NULL OR (b.starts_on<=?2 AND b.ends_on>=?2)) AND (?3 IS NULL OR b.tag=?3) ORDER BY b.starts_on,b.name,b.id"))?;
    let budgets = stmt.query_map(params![entity_id, as_of.map(|d| d.to_string()), tag], read)?.collect::<std::result::Result<Vec<_>, _>>()?;
    budgets.into_iter().map(|b| progress(conn, b)).collect()
}

fn progress(conn: &Connection, b: Budget) -> Result<BudgetProgress> {
    let filter = b.tag.as_deref().map(|s| s.split_once(':').unwrap_or((s, "")));
    let minor: i64 = conn.query_row(
        "WITH RECURSIVE subtree(id) AS (SELECT id FROM accounts WHERE id=?2 UNION SELECT a.id FROM accounts a JOIN subtree s ON a.parent_id=s.id)
         SELECT COALESCE(SUM(p.amount),0) FROM postings p JOIN journal_entries e ON e.id=p.journal_entry_id JOIN accounts a ON a.id=p.account_id
         WHERE e.entity_id=?1 AND a.entity_id=?1 AND a.type='expense' AND e.status IN ('posted','void') AND e.date>=?3 AND e.date<=?4
         AND (?2 IS NULL OR a.id IN (SELECT id FROM subtree)) AND (?5 IS NULL OR a.class=?5)
         AND (?6 IS NULL OR EXISTS(SELECT 1 FROM entry_tags t WHERE t.entry_id=e.id AND t.key=?6 AND t.value=?7))",
        params![b.entity_id, b.account_id, b.starts_on.to_string(), b.ends_on.to_string(), b.class, filter.map(|f| f.0), filter.map(|f| f.1)],
        |r| r.get(0),
    )?;
    let spent = Money::from_minor(minor, b.limit.commodity(), b.limit.precision())?;
    let remaining = b.limit.checked_sub(spent)?;
    let (group, target) = if let Some(id) = b.account_id {
        let a = accounts::get_account(conn, id)?;
        let mut stmt=conn.prepare("WITH RECURSIVE subtree(id,class) AS (SELECT id,class FROM accounts WHERE id=?1 UNION SELECT a.id,a.class FROM accounts a JOIN subtree s ON a.parent_id=s.id) SELECT DISTINCT class FROM subtree")?;
        let classes = stmt.query_map([id], |r| r.get::<_, String>(0))?.collect::<std::result::Result<Vec<_>, _>>()?;
        (if classes.len() == 1 { classes[0].clone() } else { "mixed".into() }, a.path)
    } else if let Some(class) = &b.class {
        (class.clone(), if class.is_empty() { "Unclassified".into() } else { class.clone() })
    } else {
        ("mixed".into(), b.tag.clone().unwrap_or_default())
    };
    Ok(BudgetProgress { budget: b, spent, remaining, group, target })
}
