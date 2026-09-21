//! Advisory overlap detection. It changes neither matching nor posting decisions.
use chrono::NaiveDate;
use rusqlite::{params, Connection};

use crate::{model::StatementLine, Result};

use super::MATCH_WINDOW_DAYS;

pub(super) fn dates(conn: &Connection, account: i64, exclude_import: i64) -> Result<Vec<NaiveDate>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT e.date FROM postings p JOIN journal_entries e ON e.id=p.journal_entry_id
         WHERE p.account_id=?1 AND p.quantity<>0 AND e.status='posted'
           AND NOT EXISTS (SELECT 1 FROM postings q JOIN accounts a ON a.id=q.account_id WHERE q.journal_entry_id=e.id AND a.type='equity')
           AND e.reverses_id IS NULL
           AND NOT EXISTS (SELECT 1 FROM statement_lines s WHERE s.import_id=?2 AND s.journal_entry_id=e.id)
         ORDER BY e.date",
    )?;
    let rows = stmt.query_map(params![account, exclude_import], |r| r.get::<_, String>(0))?;
    rows.map(|r| crate::parse_date(&r?)).collect()
}

pub(super) fn warning(dates: &[NaiveDate], lines: &[StatementLine], preview: bool) -> Option<String> {
    let affected: Vec<_> = lines
        .iter()
        .filter(|line| {
            if !(line.status == "created" || preview && line.status == "unmatched") {
                return false;
            }
            line.date.is_some_and(|date| {
                let i = dates.partition_point(|old| (*old - date).num_days() < -MATCH_WINDOW_DAYS);
                dates.get(i).is_some_and(|old| (*old - date).num_days().abs() <= MATCH_WINDOW_DAYS)
            })
        })
        .filter_map(|line| line.date)
        .collect();
    let first = affected.iter().min()?;
    let last = affected.iter().max()?;
    let scope = if preview { "incoming lines" } else { "newly created entries" };
    Some(format!(
        "Possible coverage overlap: {} {scope} dated {first} through {last} fall within {MATCH_WINDOW_DAYS} days of earlier posted activity on this account. Older split or aggregated bookings may evade exact-amount matching. Review these entries before accepting the import; use a booking cutoff for future pulls. This is a heuristic, not proof of duplicates.",
        affected.len()
    ))
}
