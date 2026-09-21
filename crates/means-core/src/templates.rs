//! Entry templates: journal entries with formulas, applied by hand, by rules, or on a schedule.

use std::collections::HashMap;

use chrono::{NaiveDate, TimeZone, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use rust_decimal::Decimal;

use crate::accounts;
use crate::journal;
use crate::model::*;
use crate::money::Money;
use crate::{new_uid, now_ts, Error, Result};

const SELECT: &str = "SELECT id, uid, entity_id, name, payee, description, lines, rrule, starts_on, next_on, ends_on, auto_post, lead_days, version, active, created_at FROM entry_templates";

fn row_to_template(r: &rusqlite::Row<'_>) -> rusqlite::Result<EntryTemplate> {
    let lines: String = r.get(6)?;
    let pd = |s: Option<String>| s.and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok());
    Ok(EntryTemplate {
        id: r.get(0)?,
        uid: r.get(1)?,
        entity_id: r.get(2)?,
        name: r.get(3)?,
        payee: r.get(4)?,
        description: r.get(5)?,
        lines: serde_json::from_str(&lines).unwrap_or_default(),
        rrule: r.get(7)?,
        starts_on: pd(r.get(8)?),
        next_on: pd(r.get(9)?),
        ends_on: pd(r.get(10)?),
        auto_post: r.get::<_, i64>(11)? != 0,
        lead_days: r.get(12)?,
        version: r.get(13)?,
        active: r.get::<_, i64>(14)? != 0,
        created_at: r.get(15)?,
    })
}

pub fn list_templates(conn: &Connection, entity_id: Option<i64>, include_inactive: bool) -> Result<Vec<EntryTemplate>> {
    let sql = format!("{SELECT} WHERE (?1 IS NULL OR entity_id = ?1) AND (?2 = 1 OR active = 1) ORDER BY name");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![entity_id, include_inactive as i64], row_to_template)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub fn get_template(conn: &Connection, id: i64) -> Result<EntryTemplate> {
    let sql = format!("{SELECT} WHERE id = ?1");
    conn.query_row(&sql, [id], row_to_template).optional()?.ok_or_else(|| Error::NotFound(format!("template {id}")))
}

fn validate_lines(conn: &Connection, entity_id: i64, lines: &[TemplateLine]) -> Result<()> {
    if lines.len() < 2 {
        return Err(Error::Invalid("a template needs at least two lines".into()));
    }
    let mut balances = 0;
    for (i, l) in lines.iter().enumerate() {
        let acc = accounts::get_account(conn, l.account_id)?;
        if acc.entity_id != entity_id {
            return Err(Error::Invalid(format!("line {}: account {} belongs to another entity", i + 1, acc.path)));
        }
        match l.method.as_str() {
            "fixed" => {
                if l.value.is_none() {
                    return Err(Error::Invalid(format!("line {}: fixed lines need a value", i + 1)));
                }
            }
            "input" => {}
            "percent_of" => {
                let of = l.of_line.ok_or_else(|| Error::Invalid(format!("line {}: percent_of needs of_line", i + 1)))?;
                if of == 0 || of > lines.len() || of == i + 1 {
                    return Err(Error::Invalid(format!("line {}: of_line out of range", i + 1)));
                }
                if l.value.is_none() {
                    return Err(Error::Invalid(format!("line {}: percent_of needs a percentage", i + 1)));
                }
            }
            "balance" => balances += 1,
            other => return Err(Error::Invalid(format!("line {}: unknown method {other:?}", i + 1))),
        }
    }
    if balances > 1 {
        return Err(Error::Invalid("only one line can be the balance".into()));
    }
    Ok(())
}

pub fn save_template(conn: &Connection, t: &EntryTemplate) -> Result<EntryTemplate> {
    if t.name.trim().is_empty() {
        return Err(Error::Invalid("template name is required".into()));
    }
    validate_lines(conn, t.entity_id, &t.lines)?;
    if !t.rrule.trim().is_empty() {
        parse_rrule(&t.rrule, t.starts_on.unwrap_or_else(crate::today))?;
    }
    let lines = serde_json::to_string(&t.lines)?;
    let next_on = if t.rrule.trim().is_empty() {
        None
    } else {
        let start = t.starts_on.unwrap_or_else(crate::today);
        Some(t.next_on.unwrap_or(start))
    };
    let ts = now_ts();
    if t.id == 0 {
        conn.execute(
            "INSERT INTO entry_templates (uid, entity_id, name, payee, description, lines, rrule, starts_on, next_on, ends_on, auto_post, lead_days, version, active, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 1, ?13, ?14, ?14)",
            params![
                new_uid(),
                t.entity_id,
                t.name.trim(),
                t.payee.trim(),
                t.description.trim(),
                lines,
                t.rrule.trim(),
                t.starts_on.map(|d| d.to_string()),
                next_on.map(|d| d.to_string()),
                t.ends_on.map(|d| d.to_string()),
                t.auto_post as i64,
                t.lead_days,
                t.active as i64,
                ts
            ],
        )?;
        let id = conn.last_insert_rowid();
        crate::audit::log(conn, "entry_templates", id, "create", None, Some(serde_json::to_value(t)?))?;
        get_template(conn, id)
    } else {
        let before = get_template(conn, t.id)?;
        let bump = serde_json::to_string(&before.lines)? != lines || before.payee != t.payee.trim();
        conn.execute(
            "UPDATE entry_templates SET name = ?2, payee = ?3, description = ?4, lines = ?5, rrule = ?6, starts_on = ?7, next_on = ?8, ends_on = ?9, auto_post = ?10, lead_days = ?11,
                version = version + ?12, active = ?13, updated_at = ?14 WHERE id = ?1",
            params![
                t.id,
                t.name.trim(),
                t.payee.trim(),
                t.description.trim(),
                lines,
                t.rrule.trim(),
                t.starts_on.map(|d| d.to_string()),
                next_on.map(|d| d.to_string()),
                t.ends_on.map(|d| d.to_string()),
                t.auto_post as i64,
                t.lead_days,
                bump as i64,
                t.active as i64,
                ts
            ],
        )?;
        crate::audit::log(conn, "entry_templates", t.id, "update", Some(serde_json::to_value(&before)?), Some(serde_json::to_value(t)?))?;
        get_template(conn, t.id)
    }
}

pub fn delete_template(conn: &Connection, id: i64) -> Result<()> {
    let t = get_template(conn, id)?;
    let used: i64 = conn.query_row("SELECT COUNT(*) FROM journal_entries WHERE template_id = ?1", [id], |r| r.get(0))?;
    if used > 0 {
        conn.execute("UPDATE entry_templates SET active = 0, updated_at = ?2 WHERE id = ?1", params![id, now_ts()])?;
    } else {
        conn.execute("UPDATE rules SET template_id = NULL WHERE template_id = ?1", [id])?;
        conn.execute("DELETE FROM entry_templates WHERE id = ?1", [id])?;
    }
    crate::audit::log(conn, "entry_templates", id, "delete", Some(serde_json::to_value(&t)?), None)?;
    Ok(())
}

/// Resolve the lines of a template into posting inputs. `inputs` are 1-based line index -> value
/// (in the line account's commodity, debit positive).
pub fn resolve_lines(conn: &Connection, t: &EntryTemplate, inputs: &HashMap<usize, Decimal>) -> Result<Vec<PostingInput>> {
    validate_lines(conn, t.entity_id, &t.lines)?;
    let accounts = t.lines.iter().map(|line| accounts::get_account(conn, line.account_id)).collect::<Result<Vec<_>>>()?;
    let mut values: Vec<Option<Money>> = vec![None; t.lines.len()];
    // Resolve input boundaries once, in the actual account commodity.
    for (i, l) in t.lines.iter().enumerate() {
        let value = match l.method.as_str() {
            "fixed" => l.value,
            "input" => {
                Some(inputs.get(&(i + 1)).copied().or(l.value).ok_or_else(|| Error::Invalid(format!("line {} ({}) needs a value", i + 1, if l.label.is_empty() { &l.memo } else { &l.label })))?)
            }
            _ => None,
        };
        values[i] = value.map(|v| Money::from_major(v, &accounts[i].commodity, accounts[i].precision)).transpose()?;
    }
    // Percentages retain the unit of the resolved value they reference. A chain
    // through another currency does not silently relabel that value.
    for _ in 0..t.lines.len() {
        let mut changed = false;
        for (i, l) in t.lines.iter().enumerate() {
            if l.method == "percent_of" && values[i].is_none() {
                let of = l.of_line.unwrap_or(0);
                if let Some(base) = values.get(of.wrapping_sub(1)).copied().flatten() {
                    // Opposite-sign percentages represent separate debit/credit
                    // distributions. Allocate each group without netting them away.
                    let negative = l.value.unwrap_or_default().is_sign_negative();
                    let group: Vec<_> = t
                        .lines
                        .iter()
                        .enumerate()
                        .filter(|(_, line)| line.method == "percent_of" && line.of_line == Some(of) && line.value.unwrap_or_default().is_sign_negative() == negative)
                        .collect();
                    let percentages: Vec<_> = group.iter().map(|(_, line)| line.value.unwrap_or_default()).collect();
                    for ((index, _), value) in group.into_iter().zip(allocate_percentages(base, &percentages)?) {
                        values[index] = Some(value);
                    }
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    let mut postings = Vec::with_capacity(t.lines.len());
    for (i, l) in t.lines.iter().enumerate() {
        let p = if l.method == "balance" {
            PostingInput::balancing(l.account_id).memo(&l.memo)
        } else {
            let v = values[i].ok_or_else(|| Error::Invalid(format!("line {} could not be resolved", i + 1)))?;
            if v.is_zero() {
                let mut posting = PostingInput::new(l.account_id, Decimal::ZERO).memo(&l.memo);
                posting.amount = Some(Decimal::ZERO);
                posting
            } else if v.commodity() != accounts[i].commodity {
                PostingInput::valued(l.account_id, v.major(), v.commodity()).memo(&l.memo)
            } else {
                PostingInput::new(l.account_id, v.major()).memo(&l.memo)
            }
        };
        postings.push(p);
    }
    Ok(postings)
}

/// Allocate the rounded group total in exact ratios. Decimal percentages are
/// reduced to integer weights without floating-point conversion.
fn allocate_percentages(base: Money, percentages: &[Decimal]) -> Result<Vec<Money>> {
    let overflow = || Error::Invalid("template percentage allocation out of range".into());
    let percent = percentages.iter().try_fold(Decimal::ZERO, |sum, p| sum.checked_add(*p).ok_or_else(overflow))?;
    let major = base.major().checked_mul(percent / Decimal::from(100)).ok_or_else(overflow)?;
    let total = Money::from_major(major, base.commodity(), base.precision())?;
    let scale = percentages.iter().map(|p| p.normalize().scale()).max().unwrap_or(0);
    let weights = percentages
        .iter()
        .map(|p| {
            let p = p.normalize();
            p.mantissa().unsigned_abs().checked_mul(10u128.pow(scale - p.scale())).ok_or_else(overflow)
        })
        .collect::<Result<Vec<_>>>()?;
    fn gcd(mut a: u128, mut b: u128) -> u128 {
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a
    }
    let divisor = weights.iter().copied().fold(0, gcd);
    if divisor == 0 {
        return Ok(vec![total; percentages.len()]);
    }
    let ratios = weights.into_iter().map(|w| u64::try_from(w / divisor).map_err(|_| overflow())).collect::<Result<Vec<_>>>()?;
    if ratios.iter().all(|r| *r == ratios[0]) {
        total.split(ratios.len())
    } else {
        total.allocate(&ratios)
    }
}

/// Apply a template on a date, producing a draft (or posted) entry.
pub fn apply_template(conn: &mut Connection, template_id: i64, date: NaiveDate, inputs: &HashMap<usize, Decimal>, payee: &str, status: EntryStatus, origin: &str) -> Result<JournalEntry> {
    let tx = conn.transaction()?;
    let result = apply_template_in_transaction(&tx, template_id, date, inputs, payee, status, origin)?;
    tx.commit()?;
    Ok(result)
}

pub(crate) fn apply_template_in_transaction(
    conn: &rusqlite::Transaction<'_>,
    template_id: i64,
    date: NaiveDate,
    inputs: &HashMap<usize, Decimal>,
    payee: &str,
    status: EntryStatus,
    origin: &str,
) -> Result<JournalEntry> {
    let t = get_template(conn, template_id)?;
    let postings = resolve_lines(conn, &t, inputs)?;
    let mut input = EntryInput::new(t.entity_id, date);
    input.payee = if payee.trim().is_empty() { t.payee.clone() } else { payee.trim().to_string() };
    input.description = t.description.clone();
    input.status = status;
    input.template_id = Some(t.id);
    input.template_version = Some(t.version);
    input.origin = origin.to_string();
    input.postings = postings;
    input.absorb_fx = !input.postings.iter().any(|p| p.balance);
    journal::create_entry_in_transaction(conn, input)
}

fn parse_rrule(rule: &str, start: NaiveDate) -> Result<rrule::RRuleSet> {
    let text = rule.trim();
    let body = text.strip_prefix("RRULE:").unwrap_or(text);
    let dtstart = Utc.from_utc_datetime(&start.and_hms_opt(12, 0, 0).unwrap()).with_timezone(&rrule::Tz::UTC);
    let r: rrule::RRule<rrule::Unvalidated> = body.parse().map_err(|e| Error::Invalid(format!("invalid recurrence {text:?}: {e}")))?;
    let set = r.build(dtstart).map_err(|e| Error::Invalid(format!("invalid recurrence {text:?}: {e}")))?;
    Ok(set)
}

/// Occurrence dates of a template's schedule in (from, until], at most `limit`.
pub fn occurrences(t: &EntryTemplate, from: NaiveDate, until: NaiveDate, limit: u16) -> Result<Vec<NaiveDate>> {
    if t.rrule.trim().is_empty() {
        return Ok(vec![]);
    }
    let start = t.starts_on.unwrap_or(from);
    let set = parse_rrule(&t.rrule, start)?;
    let after = Utc.from_utc_datetime(&from.and_hms_opt(12, 0, 1).unwrap()).with_timezone(&rrule::Tz::UTC);
    let before = Utc.from_utc_datetime(&until.and_hms_opt(23, 59, 59).unwrap()).with_timezone(&rrule::Tz::UTC);
    let result = set.after(after).before(before).all(limit);
    let mut dates: Vec<NaiveDate> = result.dates.into_iter().map(|d| d.date_naive()).collect();
    if let Some(end) = t.ends_on {
        dates.retain(|d| *d <= end);
    }
    Ok(dates)
}

/// Materialize scheduled occurrences up to `until` (default today + lead days).
/// Stop on failure without advancing past it. Earlier entries remain; a retry
/// skips them and resumes unfinished occurrences.
pub fn run_schedules(conn: &mut Connection, until: Option<NaiveDate>) -> Result<usize> {
    let templates = list_templates(conn, None, false)?;
    let today = crate::today();
    let mut created = 0usize;
    for t in templates.iter().filter(|t| !t.rrule.trim().is_empty()) {
        let horizon = until.unwrap_or(today + chrono::Duration::days(t.lead_days.max(0) as i64));
        let from = t.next_on.map(|d| d - chrono::Duration::days(1)).unwrap_or(t.starts_on.unwrap_or(today) - chrono::Duration::days(1));
        let dates = occurrences(t, from, horizon, 400)?;
        let mut last: Option<NaiveDate> = None;
        for d in dates {
            let exists: i64 = conn.query_row("SELECT COUNT(*) FROM journal_entries WHERE template_id = ?1 AND date = ?2 AND origin = 'schedule'", params![t.id, d.to_string()], |r| r.get(0))?;
            if exists == 0 {
                let status = if t.auto_post { EntryStatus::Posted } else { EntryStatus::Draft };
                match apply_template(conn, t.id, d, &HashMap::new(), "", status, "schedule") {
                    Ok(_) => created += 1,
                    Err(e) => {
                        tracing::warn!(template = t.name, date = %d, "schedule stopped; occurrence remains retryable: {e}");
                        return Err(e);
                    }
                }
            }
            last = Some(d);
        }
        if let Some(l) = last {
            let next = occurrences(t, l, l + chrono::Duration::days(3660), 1)?.first().copied();
            conn.execute("UPDATE entry_templates SET next_on = ?2 WHERE id = ?1", params![t.id, next.map(|d| d.to_string())])?;
        }
    }
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::entities;
    use rust_decimal::prelude::FromStr;

    #[test]
    fn percentage_splits_conserve_cents_and_chain_the_original_commodity() {
        let db = Db::open_memory().unwrap();
        let mut conn = db.conn();
        let entity = entities::create_entity(&mut conn, "Personal", "person", "PT", "EUR").unwrap();
        let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
        let expense = accounts::ensure_account(&conn, entity.id, AccountType::Expense, &["Food"], "expense", "EUR").unwrap();
        let foreign = accounts::ensure_account(&conn, entity.id, AccountType::Expense, &["Foreign"], "expense", "USD").unwrap();
        let line = |account_id, method: &str, value: &str, of_line| TemplateLine {
            account_id,
            method: method.into(),
            value: Some(Decimal::from_str(value).unwrap()),
            of_line,
            memo: String::new(),
            label: String::new(),
        };
        let mut template = EntryTemplate {
            id: 0,
            uid: String::new(),
            entity_id: entity.id,
            name: "Split".into(),
            payee: String::new(),
            description: String::new(),
            lines: vec![line(bank.id, "input", "-10.01", None), line(expense.id, "percent_of", "-50", Some(1)), line(expense.id, "percent_of", "-50", Some(1))],
            rrule: String::new(),
            starts_on: None,
            next_on: None,
            ends_on: None,
            auto_post: false,
            lead_days: 0,
            version: 1,
            active: true,
            created_at: String::new(),
        };
        let saved = save_template(&conn, &template).unwrap();
        let entry = apply_template(&mut conn, saved.id, NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(), &HashMap::new(), "", EntryStatus::Posted, "manual").unwrap();
        assert_eq!(entry.postings.iter().map(|p| p.quantity.major()).collect::<Vec<_>>(), ["-10.01", "5.01", "5"].map(|v| Decimal::from_str(v).unwrap()));
        assert!(crate::hashchain::verify(&conn, entity.id).unwrap().first_bad_seq.is_none());
        let reversal = resolve_lines(&conn, &template, &HashMap::from([(1, Decimal::from_str("10.01").unwrap())])).unwrap();
        assert_eq!(reversal[1].quantity, Decimal::from_str("-5.01").unwrap());
        assert_eq!(reversal[2].quantity, Decimal::from(-5));
        for amount in ["-0.01", "0.01"] {
            let entry = apply_template(&mut conn, saved.id, NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(), &HashMap::from([(1, Decimal::from_str(amount).unwrap())]), "", EntryStatus::Posted, "manual")
                .unwrap();
            assert_eq!(entry.postings.len(), 3);
            assert_eq!(entry.postings[1].quantity.minor(), -entry.postings[0].quantity.minor());
            assert!(entry.postings[2].quantity.is_zero());
            assert!(entry.postings[2].amount.is_zero());
        }
        template.lines = vec![line(bank.id, "input", "10.01", None), line(foreign.id, "percent_of", "50", Some(1)), line(expense.id, "percent_of", "50", Some(2))];
        let chained = resolve_lines(&conn, &template, &HashMap::new()).unwrap();
        assert_eq!(chained[1].value_in, Some((Decimal::from(5), "EUR".into())));
        assert_eq!(chained[2].quantity, Decimal::from_str("2.5").unwrap());
        assert!(chained[2].value_in.is_none());
        template.lines[1].of_line = Some(3);
        assert!(resolve_lines(&conn, &template, &HashMap::new()).is_err());
    }

    #[test]
    fn percentage_allocation_obeys_weights_without_normalizing_the_percentage_total() {
        let total = Money::from_minor(1001, "EUR", 2).unwrap();
        for (percentages, expected) in [
            (vec!["33.3", "33.3", "33.4"], vec![333, 333, 335]),
            (vec!["20", "30"], vec![200, 300]),
            (vec!["0", "100"], vec![0, 1001]),
            (vec!["0", "0"], vec![0, 0]),
            (vec!["-50", "-50"], vec![-501, -500]),
        ] {
            let parts = allocate_percentages(total, &percentages.iter().map(|p| Decimal::from_str(p).unwrap()).collect::<Vec<_>>()).unwrap();
            assert_eq!(parts.iter().map(Money::minor).collect::<Vec<_>>(), expected);
            assert!(parts.iter().all(|p| p.commodity() == "EUR"));
        }
    }

    #[test]
    fn template_with_percent_and_balance() {
        let db = Db::open_memory().unwrap();
        let mut conn = db.conn();
        let e = entities::create_entity(&mut conn, "MEI", "company", "BR", "BRL").unwrap();
        let inter = accounts::ensure_account(&conn, e.id, AccountType::Asset, &["Inter"], "bank", "BRL").unwrap();
        let inss = accounts::ensure_account(&conn, e.id, AccountType::Expense, &["Taxes", "INSS"], "expense", "BRL").unwrap();
        let iss = accounts::ensure_account(&conn, e.id, AccountType::Expense, &["Taxes", "ISS"], "expense", "BRL").unwrap();
        let t = EntryTemplate {
            id: 0,
            uid: String::new(),
            entity_id: e.id,
            name: "DAS".into(),
            payee: "Receita Federal".into(),
            description: String::new(),
            lines: vec![
                TemplateLine { account_id: inss.id, method: "input".into(), value: Some(Decimal::from_str("81.05").unwrap()), of_line: None, memo: "INSS".into(), label: "INSS".into() },
                TemplateLine { account_id: iss.id, method: "fixed".into(), value: Some(Decimal::from_str("5").unwrap()), of_line: None, memo: "ISS".into(), label: String::new() },
                TemplateLine { account_id: inter.id, method: "balance".into(), value: None, of_line: None, memo: String::new(), label: String::new() },
            ],
            rrule: "FREQ=MONTHLY;BYMONTHDAY=20".into(),
            starts_on: Some(NaiveDate::from_ymd_opt(2026, 1, 20).unwrap()),
            next_on: None,
            ends_on: None,
            auto_post: false,
            lead_days: 30,
            version: 1,
            active: true,
            created_at: String::new(),
        };
        let saved = save_template(&conn, &t).unwrap();
        assert_eq!(saved.next_on, Some(NaiveDate::from_ymd_opt(2026, 1, 20).unwrap()));
        let entry = apply_template(&mut conn, saved.id, NaiveDate::from_ymd_opt(2026, 2, 20).unwrap(), &HashMap::new(), "", EntryStatus::Draft, "manual").unwrap();
        assert_eq!(entry.postings[2].quantity.major(), Decimal::from_str("-86.05").unwrap());
        assert_eq!(entry.status, EntryStatus::Draft);
        let occ = occurrences(&saved, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), NaiveDate::from_ymd_opt(2026, 4, 30).unwrap(), 10).unwrap();
        assert_eq!(occ.len(), 4);
        assert_eq!(occ[0], NaiveDate::from_ymd_opt(2026, 1, 20).unwrap());
        let n = run_schedules(&mut conn, Some(NaiveDate::from_ymd_opt(2026, 3, 31).unwrap())).unwrap();
        assert_eq!(n, 3);
        let again = run_schedules(&mut conn, Some(NaiveDate::from_ymd_opt(2026, 3, 31).unwrap())).unwrap();
        assert_eq!(again, 0);

        let mut inactive = get_template(&conn, saved.id).unwrap();
        inactive.active = false;
        save_template(&conn, &inactive).unwrap();
        // A later failure keeps prior entries and does not skip the failed date.
        let mut failing = t.clone();
        failing.name = "Retry schedule".into();
        failing.starts_on = Some(NaiveDate::from_ymd_opt(2027, 1, 20).unwrap());
        let failing = save_template(&conn, &failing).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_schedule BEFORE INSERT ON journal_entries WHEN NEW.origin = 'schedule' AND NEW.date = '2027-02-20' BEGIN SELECT RAISE(ABORT, 'temporary schedule failure'); END;",
        )
        .unwrap();
        let until = NaiveDate::from_ymd_opt(2027, 3, 31).unwrap();
        assert!(run_schedules(&mut conn, Some(until)).is_err());
        let dates: Vec<String> = conn
            .prepare("SELECT date FROM journal_entries WHERE template_id = ?1 AND origin = 'schedule' ORDER BY date")
            .unwrap()
            .query_map([failing.id], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(dates, vec!["2027-01-20"]);
        assert_eq!(get_template(&conn, failing.id).unwrap().next_on, failing.next_on);
        conn.execute_batch("DROP TRIGGER fail_schedule").unwrap();
        assert_eq!(run_schedules(&mut conn, Some(until)).unwrap(), 2);
        let dates: Vec<String> = conn
            .prepare("SELECT date FROM journal_entries WHERE template_id = ?1 AND origin = 'schedule' ORDER BY date")
            .unwrap()
            .query_map([failing.id], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(dates, vec!["2027-01-20", "2027-02-20", "2027-03-20"]);
        assert_eq!(run_schedules(&mut conn, Some(until)).unwrap(), 0);
    }
}
