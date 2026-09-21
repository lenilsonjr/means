//! Full refunds preserve the purchase and its evidence, reversing its booked
//! expense splits in a new entry. FX changes belong to FX gain/loss.
use crate::{accounts, entities, imports, journal, model::*, money::Money, rates, Error, Result};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};

pub const DEFAULT_WINDOW_DAYS: i64 = 90;

fn original_bank<'a>(conn: &Connection, original: &'a JournalEntry, bank: &Account, amount: Money, date: NaiveDate) -> Result<&'a Posting> {
    if original.entity_id != bank.entity_id || original.status != EntryStatus::Posted || original.date > date || original.refund_of_id.is_some() {
        return Err(Error::Invalid("choose a posted expense in this entity dated no later than the refund".into()));
    }
    let banks: Vec<_> = original.postings.iter().filter(|p| matches!(p.account_type, AccountType::Asset | AccountType::Liability)).collect();
    if banks.len() != 1 || banks[0].quantity != amount.checked_neg()? {
        return Err(Error::Invalid("a full refund must equal the original debit in the same currency".into()));
    }
    let mut expenses = 0;
    for p in &original.postings {
        if p.rate_source == "missing" {
            return Err(Error::Invalid("value the original expense before refunding it".into()));
        }
        if p.id != banks[0].id {
            if p.account_type != AccountType::Expense {
                return Err(Error::Invalid("the original must contain only one bank debit and expense splits".into()));
            }
            expenses += 1;
        }
    }
    if expenses == 0 {
        return Err(Error::Invalid("the original has no expense splits".into()));
    }
    let existing: Option<i64> = conn.query_row("SELECT id FROM journal_entries WHERE refund_of_id = ?1 AND status <> 'void'", [original.id], |r| r.get(0)).optional()?;
    if existing.is_some() {
        return Err(Error::Conflict("the original expense already has a full refund".into()));
    }
    Ok(banks[0])
}

/// Recent opposite-sign purchases. Reconciled original debits remain eligible;
/// refund evidence is attached to the new credit, never to the original debit.
pub fn candidates(conn: &Connection, line_id: i64, window_days: i64) -> Result<Vec<JournalEntry>> {
    if !(0..=36500).contains(&window_days) {
        return Err(Error::Invalid("refund lookback must be between 0 and 36500 days".into()));
    }
    let line = imports::get_line(conn, line_id)?;
    let (Some(account_id), Some(amount), Some(date)) = (line.account_id, line.amount, line.date) else { return Ok(vec![]) };
    if amount.is_negative() || amount.is_zero() || matches!(line.status.as_str(), "skipped" | "duplicate" | "matched") {
        return Ok(vec![]);
    }
    let bank = accounts::get_account(conn, account_id)?;
    let from = date.checked_sub_signed(chrono::Duration::days(window_days)).ok_or_else(|| Error::Invalid("refund lookback date is out of range".into()))?;
    let ids = {
        let mut query = conn.prepare(
            "SELECT DISTINCT e.id FROM journal_entries e JOIN postings p ON p.journal_entry_id = e.id JOIN accounts a ON a.id = p.account_id JOIN commodities c ON c.id = a.commodity_id
            WHERE e.entity_id = ?1 AND p.quantity = ?2 AND c.code = ?5 AND e.status = 'posted' AND e.date BETWEEN ?3 AND ?4
            ORDER BY e.date DESC, e.id DESC",
        )?;
        let ids = query
            .query_map(params![bank.entity_id, amount.checked_neg()?.minor(), from.to_string(), date.to_string(), bank.commodity], |r| r.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids
    };
    let mut result = Vec::new();
    for id in ids {
        let entry = journal::get_entry(conn, id)?;
        match original_bank(conn, &entry, &bank, amount, date) {
            Ok(_) => result.push(entry),
            Err(Error::Invalid(_) | Error::Conflict(_)) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(result)
}

/// Replace only this line's automatic suspense draft, or consume an unmatched
/// line. Everything, including evidence reassignment, commits as one transaction.
pub fn link(conn: &mut Connection, line_id: i64, original_id: i64) -> Result<JournalEntry> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let line = imports::get_line(&tx, line_id)?;
    let (Some(account_id), Some(amount), Some(date)) = (line.account_id, line.amount, line.date) else { return Err(Error::Invalid("refund line needs an account, date and amount".into())) };
    if amount.is_negative() || amount.is_zero() || !matches!(line.status.as_str(), "unmatched" | "error" | "created") {
        return Err(Error::Invalid("choose an unfinished positive statement line for the refund".into()));
    }
    let bank = accounts::get_account(&tx, account_id)?;
    if !matches!(bank.r#type, AccountType::Asset | AccountType::Liability) {
        return Err(Error::Invalid("refund evidence must belong to a bank or card account".into()));
    }
    Money::from_minor(0, &bank.commodity, bank.precision)?.checked_add(amount)?;
    let original = journal::get_entry(&tx, original_id)?;
    let original_bank_id = original_bank(&tx, &original, &bank, amount, date)?.id;
    let entity = entities::get_entity(&tx, bank.entity_id)?;
    if rates::rate_for(&tx, &bank.commodity, &entity.currency, date)?.is_none() {
        return Err(Error::Invalid("add the refund-date exchange rate before linking this refund".into()));
    }
    let mut replaced_draft = None;
    if let Some(current_id) = line.journal_entry_id {
        let current = journal::get_entry(&tx, current_id)?;
        let suspense = accounts::find_by_role(&tx, bank.entity_id, "suspense")?;
        let evidence_count: i64 = tx.query_row("SELECT COUNT(*) FROM statement_lines WHERE journal_entry_id = ?1", [current_id], |r| r.get(0))?;
        if current.status != EntryStatus::Draft
            || current.origin != "import"
            || current.refund_of_id.is_some()
            || current.postings.len() != 2
            || evidence_count != 1
            || !current.postings.iter().any(|p| p.account_id == bank.id && p.quantity == amount)
            || !current.postings.iter().any(|p| p.account_id == suspense.id)
        {
            return Err(Error::Invalid("this line already evidences an entry; only its automatic suspense draft can be replaced by a refund".into()));
        }
        tx.execute("UPDATE statement_lines SET journal_entry_id = NULL, posting_id = NULL WHERE id = ?1", [line.id])?;
        // Keep the row occupied until the new entry has an ID; SQLite otherwise
        // reuses a deleted maximum integer primary key.
        tx.execute("DELETE FROM postings WHERE journal_entry_id = ?1", [current_id])?;
        replaced_draft = Some(current_id);
        crate::audit::log(&tx, "journal_entries", current_id, "delete", Some(serde_json::to_value(current)?), Some(serde_json::json!({"reason":"replace suspense draft with refund"})))?;
    } else if line.posting_id.is_some() {
        return Err(Error::Invalid("refund line has inconsistent existing evidence".into()));
    }
    let mut input = EntryInput::new(bank.entity_id, date);
    input.payee = original.payee.clone();
    input.description = line.description.clone();
    input.origin = "refund".into();
    input.absorb_fx = true;
    let mut credit = PostingInput::new(bank.id, amount.major()).external(Some(line.reference.clone()).filter(|s| !s.is_empty()), Some(line.fingerprint.clone()).filter(|s| !s.is_empty()));
    if let Some(value) = line.raw.get("_original") {
        credit = credit.meta("original", value.clone());
    }
    input.postings.push(credit);
    for posting in original.postings.iter().filter(|p| p.id != original_bank_id) {
        input.postings.push(PostingInput {
            account_id: posting.account_id,
            quantity: posting.quantity.checked_neg()?.major(),
            amount: Some(posting.amount.checked_neg()?.major()),
            memo: posting.memo.clone(),
            metadata: serde_json::json!({"refund_of_entry":original.id,"refund_of_posting":posting.id}),
            ..Default::default()
        });
    }
    let refund = journal::create_entry_in_transaction(&tx, input)?;
    crate::payees::link(&tx, refund.id, original.payee_id)?;
    if let Some(id) = replaced_draft {
        tx.execute("DELETE FROM journal_entries WHERE id = ?1", [id])?;
    }
    tx.execute("UPDATE journal_entries SET refund_of_id = ?2 WHERE id = ?1", params![refund.id, original.id])?;
    // Copy the stored pairs verbatim; re-parsing display strings can change case
    // or split an existing value containing whitespace.
    let original_tags = {
        let mut query = tx.prepare("SELECT key, value FROM entry_tags WHERE entry_id = ?1 ORDER BY key, value")?;
        let rows = query.query_map([original.id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    crate::tags::set_tags_in_transaction(&tx, refund.id, &original_tags)?;
    let credit = refund.postings.iter().find(|p| p.account_id == bank.id && p.quantity == amount).ok_or_else(|| Error::Invalid("refund lost its bank credit".into()))?;
    tx.execute("UPDATE postings SET reconciled_at = ?2 WHERE id = ?1", params![credit.id, crate::now_ts()])?;
    tx.execute(
        "UPDATE statement_lines SET status = 'created', posting_id = ?2, journal_entry_id = ?3, note = ?4 WHERE id = ?1",
        params![line.id, credit.id, refund.id, format!("refund of #{}", original.id)],
    )?;
    crate::audit::log(&tx, "journal_entries", refund.id, "refund", None, Some(serde_json::json!({"original_entry_id":original.id,"statement_line_id":line.id})))?;
    let result = journal::get_entry(&tx, refund.id)?;
    tx.commit()?;
    Ok(result)
}
