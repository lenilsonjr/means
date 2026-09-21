//! Matching statement lines to postings: the act of reconciliation.

use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};

use crate::accounts;
use crate::journal;
use crate::model::*;
use crate::money::Money;
use crate::{now_ts, Error, Result};

#[derive(Debug, Clone)]
pub struct Candidate {
    pub posting_id: i64,
    pub journal_entry_id: i64,
    pub date: NaiveDate,
    pub status: EntryStatus,
    pub origin: String,
    pub payee: String,
    pub days_off: i64,
}

/// Postings on the account with the same quantity within the window, not yet evidenced by a bank line.
/// Lines from the Account Tracker migration are the capture, not the bank, so they do not count as evidence.
pub fn candidates(conn: &Connection, account_id: i64, amount: Money, date: NaiveDate, window_days: i64) -> Result<Vec<Candidate>> {
    candidates_for(conn, account_id, amount, date, window_days, "")
}

/// Like `candidates`, ranked for a specific statement line description.
pub fn candidates_for(conn: &Connection, account_id: i64, amount: Money, date: NaiveDate, window_days: i64, description: &str) -> Result<Vec<Candidate>> {
    let from = date - chrono::Duration::days(window_days);
    let account = accounts::get_account(conn, account_id)?;
    Money::from_minor(0, &account.commodity, account.precision)?.checked_add(amount)?;
    let to = date + chrono::Duration::days(window_days);
    let mut stmt = conn.prepare(
        "SELECT p.id, e.id, e.date, e.status, e.origin, e.payee, e.description,
                (SELECT COUNT(*) FROM postings o JOIN accounts oa ON oa.id = o.account_id WHERE o.journal_entry_id = e.id AND o.id <> p.id AND oa.type IN ('asset','liability'))
         FROM postings p JOIN journal_entries e ON e.id = p.journal_entry_id
         WHERE p.account_id = ?1 AND p.quantity = ?2 AND p.reconciled_at IS NULL AND e.status IN ('draft','posted')
           AND e.reverses_id IS NULL
           AND e.date >= ?3 AND e.date <= ?4
           AND NOT EXISTS (SELECT 1 FROM statement_lines s JOIN imports i ON i.id = s.import_id WHERE s.posting_id = p.id AND i.source <> 'account_tracker')
         ORDER BY e.date, e.id",
    )?;
    let rows = stmt.query_map(params![account_id, amount.minor(), from.to_string(), to.to_string()], |r| {
        let d: String = r.get(2)?;
        let s: String = r.get(3)?;
        Ok((
            Candidate {
                posting_id: r.get(0)?,
                journal_entry_id: r.get(1)?,
                date: NaiveDate::parse_from_str(&d, "%Y-%m-%d").unwrap_or_default(),
                status: EntryStatus::parse(&s).unwrap_or(EntryStatus::Posted),
                origin: r.get(4)?,
                payee: r.get(5)?,
                days_off: 0,
            },
            r.get::<_, String>(6)?,
            r.get::<_, i64>(7)? > 0,
        ))
    })?;
    let line_words = words(description);
    let line_is_transfer = looks_like_transfer(description);
    let mut scored: Vec<(i64, Candidate)> = Vec::new();
    for row in rows {
        let (mut c, desc, is_transfer) = row?;
        c.days_off = (c.date - date).num_days().abs();
        // Closest date wins; a shared word between the bank text and the entry helps; an own-account transfer
        // should only match a line that reads like one; drafts and captured entries beat other imports on a tie.
        let mut score = c.days_off * 10;
        let entry_words = words(&format!("{} {}", c.payee, desc));
        if !line_words.is_empty() && line_words.iter().any(|w| entry_words.contains(w)) {
            score -= 4;
        }
        if is_transfer != line_is_transfer {
            score += 6;
        }
        if c.status == EntryStatus::Posted {
            score += 1;
        }
        if c.origin == "import" || c.origin == "rule" {
            score += 1;
        }
        scored.push((score, c));
    }
    scored.sort_by_key(|(score, c)| (*score, c.journal_entry_id));
    Ok(scored.into_iter().map(|(_, c)| c).collect())
}

fn words(s: &str) -> std::collections::HashSet<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 4 && !w.chars().all(|c| c.is_ascii_digit()) && !["pending", "sent", "from", "with", "card", "payment", "transfer", "online"].contains(w))
        .map(|w| w.to_string())
        .collect()
}

/// Bank text that reads like money moving between the user's own accounts.
pub fn looks_like_transfer(description: &str) -> bool {
    let d = description.to_lowercase();
    ["transfer", "sent from", "revolut", "wise", "n26 spaces", "savings", "top-up", "top up", "topup", "to own account", "own account", "between accounts"].iter().any(|k| d.contains(k))
}

pub fn best_match(conn: &Connection, account_id: i64, amount: Money, date: NaiveDate, window_days: i64) -> Result<Option<i64>> {
    Ok(candidates(conn, account_id, amount, date, window_days)?.first().map(|c| c.posting_id))
}

/// The best candidate for a statement line, using its text to break ties.
pub fn best_match_for(conn: &Connection, account_id: i64, amount: Money, date: NaiveDate, window_days: i64, description: &str) -> Result<Option<i64>> {
    Ok(candidates_for(conn, account_id, amount, date, window_days, description)?.first().map(|c| c.posting_id))
}

/// Link a line to a posting: the posting is reconciled, the entry posted if it was a draft.
pub fn attach(conn: &mut Connection, sl: &mut StatementLine, posting_id: i64) -> Result<()> {
    let tx = conn.transaction()?;
    let mut updated = sl.clone();
    attach_in_transaction(&tx, &mut updated, posting_id)?;
    tx.commit()?;
    *sl = updated;
    Ok(())
}

pub(crate) fn attach_in_transaction(tx: &rusqlite::Transaction<'_>, sl: &mut StatementLine, posting_id: i64) -> Result<()> {
    let (entry_id, account_id, already, quantity): (i64, i64, Option<String>, i64) = tx
        .query_row("SELECT journal_entry_id, account_id, reconciled_at, quantity FROM postings WHERE id = ?1", [posting_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("posting {posting_id}")))?;
    if Some(account_id) != sl.account_id {
        return Err(Error::Invalid("the posting is on another account".into()));
    }
    let amount = sl.amount.ok_or_else(|| Error::Invalid("the statement line has no amount".into()))?;
    let account = accounts::get_account(tx, account_id)?;
    if amount.commodity() != account.commodity || amount.precision() != account.precision {
        return Err(Error::Invalid("statement amount has the wrong commodity".into()));
    }
    if amount.minor() != quantity {
        return Err(Error::Invalid("the statement amount must equal the posting quantity".into()));
    }
    if already.is_some() {
        let other: Option<i64> = tx.query_row("SELECT id FROM statement_lines WHERE posting_id = ?1 AND id <> ?2", params![posting_id, sl.id], |r| r.get(0)).optional()?;
        if other.is_some() {
            return Err(Error::Conflict("that posting is already reconciled with another line".into()));
        }
    }
    let entry = journal::get_entry(tx, entry_id)?;
    if entry.status == EntryStatus::Void {
        return Err(Error::Invalid("a void entry cannot receive statement evidence".into()));
    }
    if entry.status == EntryStatus::Draft {
        journal::post_entry_in_transaction(tx, entry_id)?;
    }
    let ts = now_ts();
    tx.execute("UPDATE postings SET reconciled_at = ?2 WHERE id = ?1", params![posting_id, ts])?;
    if !sl.reference.is_empty() {
        // Keep the bank's reference on the posting when it has none (ignore conflicts).
        let _ = tx.execute("UPDATE postings SET external_id = ?2 WHERE id = ?1 AND external_id IS NULL", params![posting_id, sl.reference]);
    }
    if let Some(d) = sl.date {
        if d != entry.date {
            let _ = tx.execute("UPDATE postings SET metadata = json_set(metadata, '$.booked_on', ?2) WHERE id = ?1", params![posting_id, d.to_string()]);
        }
    }
    tx.execute("UPDATE statement_lines SET status = 'matched', posting_id = ?2, journal_entry_id = ?3, note = '' WHERE id = ?1", params![sl.id, posting_id, entry_id])?;
    sl.status = "matched".into();
    sl.posting_id = Some(posting_id);
    sl.journal_entry_id = Some(entry_id);
    Ok(())
}

/// Match by hand from the review screen.
pub fn match_line(conn: &mut Connection, line_id: i64, posting_id: i64) -> Result<StatementLine> {
    let mut sl = crate::imports::get_line(conn, line_id)?;
    if sl.status == "matched" || sl.status == "created" {
        return Err(Error::Invalid("this line already evidences an entry".into()));
    }
    attach(conn, &mut sl, posting_id)?;
    crate::imports::get_line(conn, line_id)
}

/// The most used contra account for entries with a similar payee or description.
pub fn suggest_account(conn: &Connection, entity_id: i64, bank_account_id: i64, description: &str) -> Result<Option<i64>> {
    let words: Vec<&str> = description.split_whitespace().filter(|w| w.len() >= 4 && !w.chars().all(|c| c.is_ascii_digit())).take(3).collect();
    if words.is_empty() {
        return Ok(None);
    }
    let pattern = format!("%{}%", words[0].to_lowercase());
    let row: Option<i64> = conn
        .query_row(
            "SELECT p.account_id FROM postings p JOIN journal_entries e ON e.id = p.journal_entry_id JOIN accounts a ON a.id = p.account_id
             WHERE e.entity_id = ?1 AND e.status = 'posted' AND p.account_id <> ?2 AND a.type IN ('income','expense') AND a.system_role = ''
               AND (LOWER(e.payee) LIKE ?3 OR LOWER(e.description) LIKE ?3)
             GROUP BY p.account_id ORDER BY COUNT(*) DESC LIMIT 1",
            params![entity_id, bank_account_id, pattern],
            |r| r.get(0),
        )
        .optional()?;
    Ok(row)
}
