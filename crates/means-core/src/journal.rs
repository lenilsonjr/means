//! The posting service: journal entries and their postings.
//!
//! Enforces the principles of the design page:
//! A1 entries balance in functional currency; A2 postings stay inside one entity;
//! A3 quantities are valued at the entry-date rate and the rate is stored;
//! A4 locked or reconciled entries do not change, corrections are reversals.

use std::collections::HashMap;

use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};
use rust_decimal::Decimal;

use crate::accounts;
use crate::entities;
use crate::hashchain;
use crate::model::*;
use crate::money::{self, Money};
use crate::rates;
use crate::{new_uid, now_ts, Error, Result};

struct Prepared {
    account: Account,
    quantity: Money,
    amount: Money,
    rate: Option<Decimal>,
    rate_source: String,
    memo: String,
    metadata: serde_json::Value,
    external_id: Option<String>,
    fingerprint: Option<String>,
}

/// Validate and value the postings of an entry. Returns prepared postings, balanced.
fn prepare(conn: &Connection, entity: &Entity, date: NaiveDate, inputs: &[PostingInput], absorb_fx: bool, keep_closed: &std::collections::HashSet<i64>) -> Result<Vec<Prepared>> {
    if inputs.len() < 2 {
        return Err(Error::Invalid("a journal entry needs at least two postings".into()));
    }
    let fprec = entities::functional_precision(conn, entity.id)?;
    let mut prepared: Vec<Prepared> = Vec::with_capacity(inputs.len() + 1);
    let mut balance_index: Option<usize> = None;
    let mut cache: HashMap<i64, Account> = HashMap::new();

    for (i, p) in inputs.iter().enumerate() {
        let account = match cache.get(&p.account_id) {
            Some(a) => a.clone(),
            None => {
                let a = accounts::get_account(conn, p.account_id)?;
                cache.insert(p.account_id, a.clone());
                a
            }
        };
        if account.entity_id != entity.id {
            return Err(Error::Invalid(format!("account {} belongs to another entity", account.path)));
        }
        if account.placeholder {
            return Err(Error::Invalid(format!("{} is a placeholder and cannot take postings", account.path)));
        }
        if account.is_closed() && !keep_closed.contains(&account.id) {
            return Err(Error::Invalid(format!("{} is closed", account.path)));
        }
        let mut metadata = if p.metadata.is_object() { p.metadata.clone() } else { serde_json::json!({}) };
        let mut pp = Prepared {
            account: account.clone(),
            quantity: Money::from_minor(0, &account.commodity, account.precision)?,
            amount: Money::from_minor(0, &entity.currency, fprec)?,
            rate: None,
            rate_source: String::new(),
            memo: p.memo.clone(),
            metadata: serde_json::json!({}),
            external_id: p.external_id.clone().filter(|s| !s.is_empty()),
            fingerprint: p.fingerprint.clone().filter(|s| !s.is_empty()),
        };
        if p.balance {
            if balance_index.is_some() {
                return Err(Error::Invalid("only one posting per entry can balance it".into()));
            }
            balance_index = Some(i);
            metadata["balance"] = serde_json::json!(true);
        } else if let Some((value, commodity)) = &p.value_in {
            // Value given in another commodity: convert to functional, then to the account's commodity.
            let commodity = commodity.trim().to_ascii_uppercase();
            let value_c = entities::get_commodity_by_code(conn, &commodity)?;
            let value = money::round_to(*value, value_c.precision);
            let (amount, rate, source) = value_functional(conn, &commodity, &entity.currency, date, value, fprec)?;
            pp.amount = Money::from_major(amount, &entity.currency, fprec)?;
            pp.rate = rate;
            pp.rate_source = source;
            let quantity = if account.commodity == entity.currency {
                amount
            } else if account.commodity == commodity {
                value
            } else {
                let (q, _, _) = convert(conn, &entity.currency, &account.commodity, date, amount, account.precision)?;
                q
            };
            pp.quantity = Money::from_major(quantity, &account.commodity, account.precision)?;
            metadata["value_in"] = serde_json::json!({"quantity": money::plain(value), "commodity": commodity});
        } else {
            let quantity = money::round_to(p.quantity, account.precision);
            if quantity.is_zero() && p.amount.is_none() {
                return Err(Error::Invalid(format!("posting on {} has a zero quantity", account.path)));
            }
            pp.quantity = Money::from_major(quantity, &account.commodity, account.precision)?;
            match p.amount {
                Some(a) => {
                    pp.amount = Money::from_major(a, &entity.currency, fprec)?;
                    if account.commodity == entity.currency && pp.amount != pp.quantity {
                        return Err(Error::Invalid(format!("posting on {} must have equal quantity and amount in {}", account.path, entity.currency)));
                    }
                    if !quantity.is_zero() && !pp.amount.is_zero() && pp.amount.is_negative() != quantity.is_sign_negative() {
                        return Err(Error::Invalid(format!("posting on {} must have quantity and amount with the same sign", account.path)));
                    }
                    if account.commodity != entity.currency {
                        pp.rate = (!quantity.is_zero()).then(|| (pp.amount.major() / quantity).round_dp(12));
                        pp.rate_source = "input".into();
                    }
                }
                None => {
                    let (amount, rate, source) = value_functional(conn, &account.commodity, &entity.currency, date, quantity, fprec)?;
                    pp.amount = Money::from_major(amount, &entity.currency, fprec)?;
                    pp.rate = rate;
                    pp.rate_source = source;
                }
            }
        }
        pp.metadata = metadata;
        prepared.push(pp);
    }

    // A1 is exact: the amounts are counted in minor units of the functional currency and must sum to zero (D14).
    let total_money = Money::from_minor(0, &entity.currency, fprec)?.checked_sum(prepared.iter().enumerate().filter(|(i, _)| Some(*i) != balance_index).map(|(_, p)| p.amount))?;
    let total_minor = total_money.minor();
    let total = total_money.major();
    if let Some(bi) = balance_index {
        let amount = -total;
        let (acc_commodity, acc_precision, acc_path) = {
            let a = &prepared[bi].account;
            (a.commodity.clone(), a.precision, a.path.clone())
        };
        if acc_commodity == entity.currency {
            prepared[bi].quantity = Money::from_major(amount, &acc_commodity, acc_precision)?;
        } else {
            let (q, rate, source) = convert(conn, &entity.currency, &acc_commodity, date, amount, acc_precision)?;
            prepared[bi].quantity = Money::from_major(q, &acc_commodity, acc_precision)?;
            prepared[bi].rate = rate.map(|r| if r.is_zero() { r } else { (Decimal::ONE / r).round_dp(12) });
            prepared[bi].rate_source = source;
        }
        prepared[bi].amount = Money::from_major(amount, &entity.currency, fprec)?;
        if prepared[bi].quantity.is_zero() && !amount.is_zero() {
            return Err(Error::Invalid(format!("balancing posting on {} rounds to zero", acc_path)));
        }
    } else if total_minor != 0 {
        if absorb_fx {
            let fx = accounts::find_by_role(conn, entity.id, "fx_gain_loss")?;
            Money::from_minor(0, &fx.commodity, fx.precision)?.checked_add(total_money)?;
            prepared.push(Prepared {
                account: fx,
                quantity: total_money.checked_neg()?,
                amount: total_money.checked_neg()?,
                rate: None,
                rate_source: String::new(),
                memo: "exchange difference".into(),
                metadata: serde_json::json!({"fx_auto": true}),
                external_id: None,
                fingerprint: None,
            });
        } else {
            let detail: Vec<String> = prepared.iter().map(|p| format!("{} {} ({} {})", p.account.path, money::plain(p.quantity.major()), money::plain(p.amount.major()), entity.currency)).collect();
            return Err(Error::Unbalanced(format!("postings sum to {} {}: {}", money::plain(total), entity.currency, detail.join("; "))));
        }
    }
    if prepared.iter().all(|p| p.quantity.is_zero()) {
        return Err(Error::Invalid("all postings are zero".into()));
    }
    Ok(prepared)
}

/// Value `quantity` of `commodity` in `functional` at `date`. Returns (amount, rate, source).
fn value_functional(conn: &Connection, commodity: &str, functional: &str, date: NaiveDate, quantity: Decimal, fprec: u32) -> Result<(Decimal, Option<Decimal>, String)> {
    if commodity == functional {
        return Ok((money::round_to(quantity, fprec), None, String::new()));
    }
    match rates::rate_for(conn, commodity, functional, date)? {
        Some(l) => Ok((money::round_to(quantity * l.rate, fprec), Some(l.rate), l.source)),
        None => Ok((money::round_to(quantity, fprec), Some(Decimal::ONE), "missing".into())),
    }
}

/// Convert an amount in `from` to `to`. Returns (converted, rate from->to, source).
fn convert(conn: &Connection, from: &str, to: &str, date: NaiveDate, amount: Decimal, prec: u32) -> Result<(Decimal, Option<Decimal>, String)> {
    if from == to {
        return Ok((money::round_to(amount, prec), None, String::new()));
    }
    match rates::rate_for(conn, from, to, date)? {
        Some(l) => Ok((money::round_to(amount * l.rate, prec), Some(l.rate), l.source)),
        None => Ok((money::round_to(amount, prec), Some(Decimal::ONE), "missing".into())),
    }
}

pub(crate) fn check_lock(entity: &Entity, date: NaiveDate) -> Result<()> {
    if let Some(lock) = entity.lock_date {
        if date <= lock {
            return Err(Error::Locked(format!("{} is on or before the lock date {} of {}", date, lock, entity.name)));
        }
    }
    Ok(())
}

/// Drafts remain editable; posted and void entries retain their period protection.
pub(crate) fn check_mutation_lock(conn: &Connection, entry: &JournalEntry) -> Result<()> {
    if entry.status != EntryStatus::Draft {
        check_lock(&entities::get_entity(conn, entry.entity_id)?, entry.date)?;
    }
    Ok(())
}

fn insert_postings(conn: &Connection, entry_id: i64, prepared: &[Prepared]) -> Result<()> {
    insert_postings_at(conn, entry_id, prepared, 0)
}

fn insert_postings_at(conn: &Connection, entry_id: i64, prepared: &[Prepared], offset: i64) -> Result<()> {
    for (i, p) in prepared.iter().enumerate() {
        conn.execute(
            "INSERT INTO postings (uid, journal_entry_id, account_id, quantity, amount, rate, rate_source, memo, metadata, external_id, fingerprint, position)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                new_uid(),
                entry_id,
                p.account.id,
                p.quantity.minor(),
                p.amount.minor(),
                p.rate.map(money::plain),
                p.rate_source,
                p.memo,
                p.metadata.to_string(),
                p.external_id,
                p.fingerprint,
                i as i64 + offset
            ],
        )
        .map_err(|e| match e {
            rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => {
                Error::Conflict(format!("a posting with reference {:?} already exists on {}", p.external_id.clone().unwrap_or_default(), p.account.path))
            }
            other => Error::Db(other),
        })?;
    }
    Ok(())
}

/// Create a journal entry (draft or posted).
pub fn create_entry(conn: &mut Connection, input: EntryInput) -> Result<JournalEntry> {
    let tx = conn.transaction()?;
    let result = create_entry_in_transaction(&tx, input)?;
    tx.commit()?;
    Ok(result)
}

pub(crate) fn create_entry_in_transaction(conn: &rusqlite::Transaction<'_>, input: EntryInput) -> Result<JournalEntry> {
    let entity = entities::get_entity(conn, input.entity_id)?;
    if input.status == EntryStatus::Void {
        return Err(Error::Invalid("cannot create a void entry".into()));
    }
    if input.status == EntryStatus::Posted {
        check_lock(&entity, input.date)?;
    }
    let prepared = prepare(conn, &entity, input.date, &input.postings, input.absorb_fx, &std::collections::HashSet::new())?;
    let ts = now_ts();
    conn.execute(
        "INSERT INTO journal_entries (uid, entity_id, date, payee, description, notes, status, reverses_id, counterpart_id, template_id, template_version, origin, posted_at, created_at, updated_at, reviewed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14, ?15)",
        params![
            new_uid(),
            entity.id,
            input.date.to_string(),
            input.payee.trim(),
            input.description.trim(),
            input.notes.trim(),
            input.status.as_str(),
            input.reverses_id,
            input.counterpart_id,
            input.template_id,
            input.template_version,
            if input.origin.is_empty() { "capture" } else { input.origin.as_str() },
            if input.status == EntryStatus::Posted { Some(ts.clone()) } else { None },
            ts,
            // A machine's decision waits for a human; a human's own entry is reviewed by making it.
            if matches!(input.origin.as_str(), "rule" | "import" | "schedule") { None } else { Some(ts.clone()) }
        ],
    )?;
    let id = conn.last_insert_rowid();
    insert_postings(conn, id, &prepared)?;
    let canonical = if let Some(original) = input.reverses_id { get_entry(conn, original)?.payee_id } else { crate::payees::explicit(conn, input.entity_id, &input.payee)?.map(|p| p.id) };
    crate::payees::link(conn, id, canonical)?;
    if input.status == EntryStatus::Posted {
        let entry = get_entry(conn, id)?;
        hashchain::append(conn, &entry)?;
    }
    crate::audit::log(conn, "journal_entries", id, "create", None, Some(serde_json::json!({"status": input.status.as_str(), "origin": input.origin, "date": input.date.to_string()})))?;
    get_entry(conn, id)
}

struct ReplacementLink {
    position: usize,
    reconciled_at: Option<String>,
    line_ids: Vec<i64>,
}

// Match replacements one-to-one before writing. Anonymous equal postings retain occurrence order;
// bank references and fingerprints take precedence when supplied by the client.
fn replacement_links(conn: &Connection, before: &JournalEntry, rows: &mut [Prepared], edited_date: Option<NaiveDate>) -> Result<Vec<ReplacementLink>> {
    let mut links = Vec::new();
    let mut used = std::collections::HashSet::new();
    let mut old_postings: Vec<_> = before.postings.iter().collect();
    // Reserve identifiable replacements first, so an anonymous equal-valued leg cannot
    // consume another leg's bank reference before that leg is visited.
    old_postings.sort_by_key(|p| (p.external_id.is_none(), p.fingerprint.is_none(), p.reconciled_at.is_none()));
    for old in old_postings {
        let candidates: Vec<usize> = rows.iter().enumerate().filter(|(i, p)| !used.contains(i) && p.account.id == old.account_id && p.quantity == old.quantity).map(|(i, _)| i).collect();
        let identity = candidates
            .iter()
            .copied()
            .find(|i| old.external_id.is_some() && rows[*i].external_id == old.external_id)
            .or_else(|| candidates.iter().copied().find(|i| old.fingerprint.is_some() && rows[*i].fingerprint == old.fingerprint));
        let replacement = identity.or_else(|| {
            candidates.iter().copied().find(|i| {
                let p = &rows[*i];
                !(old.external_id.is_some() && p.external_id.is_some() && old.external_id != p.external_id)
                    && !(old.fingerprint.is_some() && p.fingerprint.is_some() && old.fingerprint != p.fingerprint)
            })
        });
        let Some(index) = replacement else {
            if old.reconciled_at.is_some() {
                return Err(Error::Locked(format!("the reconciled posting on {} must survive unchanged; void the entry instead", old.account_path)));
            }
            continue;
        };
        used.insert(index);
        let p = &mut rows[index];
        // Re-sending an unchanged placeholder never makes it a manual valuation, even
        // when correcting the date. A real revaluation passes None and replaces provenance.
        if p.amount == old.amount && edited_date.is_some_and(|date| date == before.date || old.rate_source == "missing") {
            p.rate = old.rate;
            p.rate_source = old.rate_source.clone();
        }
        let mut stmt = conn.prepare("SELECT id FROM statement_lines WHERE posting_id = ?1")?;
        let line_ids: Vec<i64> = stmt.query_map([old.id], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
        links.push(ReplacementLink { position: index, reconciled_at: old.reconciled_at.clone(), line_ids });
    }
    Ok(links)
}

fn replace_postings(conn: &Connection, id: i64, prepared: &[Prepared], links: Vec<ReplacementLink>) -> Result<()> {
    conn.execute("DELETE FROM postings WHERE journal_entry_id = ?1", [id])?;
    insert_postings(conn, id, prepared)?;
    for link in links {
        let new_id: i64 = conn.query_row("SELECT id FROM postings WHERE journal_entry_id = ?1 AND position = ?2", params![id, link.position as i64], |r| r.get(0))?;
        conn.execute("UPDATE postings SET reconciled_at = ?2 WHERE id = ?1", params![new_id, link.reconciled_at])?;
        for line_id in link.line_ids {
            conn.execute("UPDATE statement_lines SET posting_id = ?2 WHERE id = ?1", params![line_id, new_id])?;
        }
    }
    Ok(())
}

/// Replace the content of an entry. Drafts change freely; posted entries only while unlocked and unreconciled.
pub fn update_entry(conn: &mut Connection, id: i64, input: EntryInput) -> Result<JournalEntry> {
    let tx = conn.transaction()?;
    let entry = update_entry_in_transaction(&tx, id, input)?;
    tx.commit()?;
    Ok(entry)
}

/// Replace an entry as part of a larger atomic workflow, with the same validation as an ordinary edit.
pub(crate) fn update_entry_in_transaction(conn: &rusqlite::Transaction<'_>, id: i64, input: EntryInput) -> Result<JournalEntry> {
    let before = get_entry(conn, id)?;
    let entity = entities::get_entity(conn, before.entity_id)?;
    if input.entity_id != before.entity_id {
        return Err(Error::Invalid("an entry cannot move between entities".into()));
    }
    if before.status == EntryStatus::Void {
        return Err(Error::Locked("a void entry cannot change".into()));
    }
    let content_changed = before.date != input.date || before.payee != input.payee.trim() || before.description != input.description.trim() || postings_differ(&before, &input);
    if before.status == EntryStatus::Posted && content_changed {
        check_lock(&entity, before.date)?;
        check_lock(&entity, input.date)?;
    }
    let new_status = match (before.status, input.status) {
        (EntryStatus::Posted, _) => EntryStatus::Posted,
        (EntryStatus::Draft, EntryStatus::Posted) => EntryStatus::Posted,
        (EntryStatus::Draft, _) => EntryStatus::Draft,
        (EntryStatus::Void, _) => EntryStatus::Void,
    };
    if new_status == EntryStatus::Posted && before.status == EntryStatus::Draft {
        check_lock(&entity, input.date)?;
    }
    let keep_closed: std::collections::HashSet<i64> = before.postings.iter().map(|p| p.account_id).collect();
    let mut prepared = if content_changed || before.status == EntryStatus::Draft { Some(prepare(conn, &entity, input.date, &input.postings, input.absorb_fx, &keep_closed)?) } else { None };
    let links = match prepared.as_mut() {
        Some(rows) => replacement_links(conn, &before, rows, Some(input.date))?,
        None => Vec::new(),
    };
    let ts = now_ts();
    let posted_at: Option<String> = if new_status == EntryStatus::Posted { before.posted_at.clone().or(Some(ts.clone())) } else { None };
    conn.execute(
        "UPDATE journal_entries SET date = ?2, payee = ?3, description = ?4, notes = ?5, status = ?6, posted_at = ?7, updated_at = ?8, template_id = COALESCE(?9, template_id) WHERE id = ?1",
        params![id, input.date.to_string(), input.payee.trim(), input.description.trim(), input.notes.trim(), new_status.as_str(), posted_at, ts, input.template_id],
    )?;
    if before.payee != input.payee.trim() {
        crate::payees::link(conn, id, crate::payees::explicit(conn, input.entity_id, &input.payee)?.map(|p| p.id))?;
    }
    if let Some(prepared) = prepared {
        replace_postings(conn, id, &prepared, links)?;
    }

    if new_status == EntryStatus::Posted {
        let entry = get_entry(conn, id)?;
        match entry.seq {
            Some(seq) => {
                hashchain::rechain(conn, entity.id, seq)?;
            }
            None => {
                hashchain::append(conn, &entry)?;
            }
        }
    }
    crate::audit::log(conn, "journal_entries", id, "update", Some(serde_json::to_value(&before)?), None)?;
    get_entry(conn, id)
}

fn postings_differ(before: &JournalEntry, input: &EntryInput) -> bool {
    if before.postings.len() != input.postings.len() {
        return true;
    }
    for (a, b) in before.postings.iter().zip(input.postings.iter()) {
        if a.account_id != b.account_id {
            return true;
        }
        if b.balance || b.value_in.is_some() {
            continue;
        }
        if a.quantity.major() != money::round_to(b.quantity, a.quantity.precision()) {
            return true;
        }
        if let Some(amt) = b.amount {
            if a.amount.major() != amt {
                return true;
            }
        }
        if a.memo != b.memo {
            return true;
        }
    }
    false
}

/// Categorize a two-leg draft without replacing its bank posting or evidence.
/// Split quantities are in the bank's native currency. Book values are allocated
/// at its already-booked rate; the last leg absorbs manual rounding differences.
pub fn post_draft_to(conn: &mut Connection, id: i64, legs: &[crate::splits::Leg], payee: Option<&str>) -> Result<JournalEntry> {
    let tx = conn.transaction()?;
    let result = categorize_in_transaction(&tx, id, legs, payee, false)?;
    tx.commit()?;
    Ok(result)
}

fn categorize_in_transaction(tx: &rusqlite::Transaction<'_>, id: i64, legs: &[crate::splits::Leg], payee: Option<&str>, existing: bool) -> Result<JournalEntry> {
    let before = get_entry(tx, id)?;
    if before.status == EntryStatus::Void || (!existing && before.status != EntryStatus::Draft) {
        return Err(Error::Invalid(format!("#{id} is {}, not a draft", before.status.as_str())));
    }
    let suspense = accounts::find_by_role(tx, before.entity_id, "suspense")?;
    let removed: Vec<_> = if existing {
        if before.counterpart_id.is_some() || before.reverses_id.is_some() || before.refund_of_id.is_some() {
            return Err(Error::Invalid("paired transfers, reversals and linked refunds cannot be split here".into()));
        }
        let refunded: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM journal_entries WHERE refund_of_id=?1)", [id], |r| r.get(0))?;
        if refunded {
            return Err(Error::Invalid("this entry has a linked refund; its booked expense splits are protected".into()));
        }
        if before.postings.iter().filter(|p| matches!(p.account_type, AccountType::Asset | AccountType::Liability)).count() != 1 {
            return Err(Error::Invalid("splitting requires one bank/card posting and category postings".into()));
        }
        let categories: Vec<_> = before.postings.iter().filter(|p| matches!(p.account_type, AccountType::Expense | AccountType::Income)).collect();
        if categories.is_empty() || categories.len() + 1 != before.postings.len() {
            return Err(Error::Invalid("only income/expense category postings can be split".into()));
        }
        categories
    } else {
        before.postings.iter().filter(|p| p.account_id == suspense.id).collect()
    };
    if removed.is_empty() || (!existing && removed.len() != 1) {
        return Err(Error::Invalid(format!("#{id} must have exactly one posting on {}", suspense.path)));
    }
    let old = removed[0];
    for p in &removed {
        let evidenced: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM statement_lines WHERE posting_id=?1)", [p.id], |r| r.get(0))?;
        if evidenced || p.reconciled_at.is_some() || p.external_id.is_some() || p.fingerprint.is_some() {
            return Err(Error::Invalid("cannot replace an evidenced or reconciled category posting".into()));
        }
    }
    let unchanged: Vec<_> = before.postings.iter().filter(|p| !removed.iter().any(|r| r.id == p.id)).collect();
    let bank = *unchanged.first().ok_or_else(|| Error::Invalid("entry has no bank posting".into()))?;
    let entity = entities::get_entity(tx, before.entity_id)?;
    check_lock(&entity, before.date)?;
    let keep_closed = unchanged.iter().map(|p| p.account_id).collect();
    let prepared = if unchanged.len() != 1 {
        if legs.len() != 1 || legs[0].quantity.is_some() {
            return Err(Error::Invalid("splitting requires a draft with one bank posting and one Uncategorized posting".into()));
        }
        if legs[0].account_id == suspense.id {
            return Err(Error::Invalid("choose a target other than Uncategorized".into()));
        }
        let mut inputs: Vec<_> = unchanged.iter().map(|p| PostingInput { amount: Some(p.amount.major()), ..PostingInput::new(p.account_id, p.quantity.major()) }).collect();
        inputs.push(PostingInput::balancing(legs[0].account_id).memo(&old.memo));
        prepare(tx, &entity, before.date, &inputs, false, &keep_closed)?
    } else {
        let resolved = crate::splits::resolve(bank.quantity.major().abs(), legs)?;
        let mut inputs = vec![PostingInput { amount: Some(bank.amount.major()), ..PostingInput::new(bank.account_id, bank.quantity.major()) }];
        let mut used_amount = Decimal::ZERO;
        let mut used_native = Decimal::ZERO;
        let total_amount = -bank.amount.major();
        let total_native = -bank.quantity.major();
        for (i, (account_id, quantity, memo)) in resolved.iter().enumerate() {
            let account = accounts::get_account(tx, *account_id)?;
            if existing && !matches!(account.r#type, AccountType::Income | AccountType::Expense) {
                return Err(Error::Invalid("existing transaction splits must use income or expense categories".into()));
            }
            if *account_id == suspense.id || *account_id == bank.account_id {
                return Err(Error::Invalid("choose a target other than the bank or Uncategorized account".into()));
            }
            let last = i + 1 == resolved.len();
            let native = if last { total_native - used_native } else { money::round_to(if total_native.is_sign_negative() { -*quantity } else { *quantity }, bank.quantity.precision()) };
            let amount = if last {
                total_amount - used_amount
            } else {
                let value = total_amount.checked_mul(native).and_then(|v| v.checked_div(total_native)).ok_or_else(|| Error::Invalid("split amount overflow".into()))?;
                money::round_to(value, bank.amount.precision())
            };
            if amount.is_zero()
                || native.is_zero()
                || amount.is_sign_negative() != (total_amount.is_sign_negative() ^ quantity.is_sign_negative())
                || native.is_sign_negative() != (total_native.is_sign_negative() ^ quantity.is_sign_negative())
            {
                return Err(Error::Invalid("split rounds to zero or exceeds the total; adjust the amounts".into()));
            }
            let target_quantity = if account.commodity == entity.currency {
                amount
            } else if account.commodity == bank.quantity.commodity() {
                native
            } else {
                let rate = rates::rate_for(tx, &entity.currency, &account.commodity, before.date)?
                    .ok_or_else(|| Error::Invalid(format!("missing exchange rate {} to {} on {}", entity.currency, account.commodity, before.date)))?;
                amount.checked_mul(rate.rate).ok_or_else(|| Error::Invalid("split conversion overflow".into()))?
            };
            let mut metadata = old.metadata.clone();
            if let Some(object) = metadata.as_object_mut() {
                object.remove("balance");
            }
            if last {
                metadata["balance"] = serde_json::json!(true);
            }
            inputs.push(PostingInput { amount: Some(amount), memo: if memo.is_empty() { old.memo.clone() } else { memo.clone() }, metadata, ..PostingInput::new(*account_id, target_quantity) });
            used_amount += amount;
            used_native += native;
        }
        prepare(tx, &entity, before.date, &inputs, false, &keep_closed)?
    };
    for p in &removed {
        tx.execute("DELETE FROM postings WHERE id=?1", [p.id])?;
    }
    // Keep the bank row unchanged, including its position, rates and reconciliation.
    insert_postings_at(tx, id, &prepared[unchanged.len()..], before.postings.iter().map(|p| i64::from(p.position)).max().unwrap_or(0) + 1)?;
    if let Some(payee) = payee {
        tx.execute("UPDATE journal_entries SET payee=?2 WHERE id=?1", params![id, payee.trim()])?;
        crate::payees::link(tx, id, crate::payees::explicit(tx, entity.id, payee)?.map(|p| p.id))?;
    }
    crate::audit::log(tx, "journal_entries", id, "split", Some(serde_json::to_value(&before)?), None)?;
    if before.status == EntryStatus::Posted {
        tx.execute("UPDATE journal_entries SET updated_at=?2 WHERE id=?1", params![id, now_ts()])?;
        if let Some(seq) = before.seq {
            hashchain::rechain(tx, before.entity_id, seq)?;
        }
    } else {
        post_entry_in_transaction(tx, id)?;
    }
    mark_reviewed(tx, &[id])?;
    let result = get_entry(tx, id)?;
    Ok(result)
}

/// Draft -> posted.
pub fn post_entry(conn: &mut Connection, id: i64) -> Result<JournalEntry> {
    let tx = conn.transaction()?;
    let entry = post_entry_in_transaction(&tx, id)?;
    tx.commit()?;
    Ok(entry)
}

/// Let reconciliation post a draft in the same transaction as its evidence link.
pub(crate) fn post_entry_in_transaction(conn: &rusqlite::Transaction<'_>, id: i64) -> Result<JournalEntry> {
    let entry = get_entry(conn, id)?;
    match entry.status {
        EntryStatus::Posted => return Ok(entry),
        EntryStatus::Void => return Err(Error::Locked("a void entry cannot be posted".into())),
        EntryStatus::Draft => {}
    }
    let entity = entities::get_entity(conn, entry.entity_id)?;
    check_lock(&entity, entry.date)?;
    let ts = now_ts();
    conn.execute("UPDATE journal_entries SET status = 'posted', posted_at = ?2, updated_at = ?2 WHERE id = ?1", params![id, ts])?;
    let entry = get_entry(conn, id)?;
    hashchain::append(conn, &entry)?;
    crate::audit::log(conn, "journal_entries", id, "post", None, None)?;
    get_entry(conn, id)
}

/// Void by reversal: a new posted entry with every posting negated, dated `date`.
pub fn void_entry(conn: &mut Connection, id: i64, date: Option<NaiveDate>, reason: &str) -> Result<JournalEntry> {
    Ok(review_void(conn, id, date, reason, true)?.entry)
}

#[derive(Debug, serde::Serialize)]
pub struct VoidReview {
    pub entry: JournalEntry,
    pub reversal: JournalEntry,
    pub evidence_lines: i64,
    pub applied: bool,
    pub already_void: bool,
}

/// Compute the full reversal inside one transaction. Preview rolls it back.
/// Drafts are never deleted by a void request.
pub fn review_void(conn: &mut Connection, id: i64, date: Option<NaiveDate>, reason: &str, apply: bool) -> Result<VoidReview> {
    let tx = conn.transaction()?;
    let before = get_entry(&tx, id)?;
    if before.status == EntryStatus::Draft {
        return Err(Error::Invalid(format!("#{id} is a draft; use draft delete instead")));
    }
    if before.status == EntryStatus::Void {
        let reversal_id = before.reversed_by_id.ok_or_else(|| Error::Invalid(format!("void entry #{id} has no reversal")))?;
        let reversal = get_entry(&tx, reversal_id)?;
        return Ok(VoidReview { entry: before, reversal, evidence_lines: 0, applied: false, already_void: true });
    }
    let evidence_lines = tx.query_row("SELECT COUNT(*) FROM statement_lines WHERE journal_entry_id=?1", [id], |r| r.get(0))?;
    let entry = void_entry_in_transaction(&tx, id, date, reason)?;
    let reversal = get_entry(&tx, entry.reversed_by_id.ok_or_else(|| Error::Invalid("void did not create a reversal".into()))?)?;
    if apply {
        tx.commit()?;
    } else {
        tx.rollback()?;
    }
    Ok(VoidReview { entry, reversal, evidence_lines, applied: apply, already_void: false })
}

/// Void a posted entry within a larger atomic operation (such as cross-source reconciliation).
pub(crate) fn void_entry_in_transaction(tx: &rusqlite::Transaction<'_>, id: i64, date: Option<NaiveDate>, reason: &str) -> Result<JournalEntry> {
    let entry = get_entry(tx, id)?;
    if entry.status != EntryStatus::Posted {
        return Err(Error::Conflict(format!("entry #{id} is not posted")));
    }
    let entity = entities::get_entity(tx, entry.entity_id)?;
    let date = date.unwrap_or(entry.date);
    check_lock(&entity, date)?;
    let mut input = EntryInput::new(entry.entity_id, date);
    input.payee = entry.payee.clone();
    input.description = if reason.trim().is_empty() { format!("Reversal of #{}", entry.id) } else { format!("Reversal of #{}: {}", entry.id, reason.trim()) };
    input.origin = "system".into();
    input.reverses_id = Some(entry.id);
    input.postings = entry
        .postings
        .iter()
        .map(|p| {
            Ok(PostingInput {
                account_id: p.account_id,
                quantity: p.quantity.checked_neg()?.major(),
                amount: Some(p.amount.checked_neg()?.major()),
                memo: p.memo.clone(),
                metadata: serde_json::json!({"reverses_posting": p.id}),
                ..Default::default()
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let reversal = create_entry_in_transaction(tx, input)?;
    tx.execute("INSERT INTO entry_tags (entry_id, key, value) SELECT ?1, key, value FROM entry_tags WHERE entry_id = ?2", params![reversal.id, id])?;
    tx.execute("UPDATE journal_entries SET status = 'void', updated_at = ?2 WHERE id = ?1", params![id, now_ts()])?;
    // Statement lines that evidenced the voided entry go back to the queue.
    tx.execute("UPDATE statement_lines SET status = 'unmatched', posting_id = NULL, journal_entry_id = NULL, note = 'entry voided' WHERE journal_entry_id = ?1", [id])?;
    crate::audit::log(tx, "journal_entries", id, "void", None, Some(serde_json::json!({"reversal": reversal.id, "reason": reason})))?;
    get_entry(tx, id)
}

/// Delete a draft.
pub fn delete_entry(conn: &mut Connection, id: i64) -> Result<()> {
    let entry = get_entry(conn, id)?;
    if entry.status != EntryStatus::Draft {
        return Err(Error::Locked("only drafts can be deleted; void posted entries instead".into()));
    }
    let tx = conn.transaction()?;
    tx.execute("UPDATE statement_lines SET status = 'unmatched', posting_id = NULL, journal_entry_id = NULL WHERE journal_entry_id = ?1", [id])?;
    tx.execute("DELETE FROM journal_entries WHERE id = ?1", [id])?;
    crate::audit::log(&tx, "journal_entries", id, "delete", Some(serde_json::to_value(&entry)?), None)?;
    tx.commit()?;
    Ok(())
}

/// Re-value postings booked without a rate now that prices exist. Returns true when something changed.
pub fn revalue_entry(conn: &mut Connection, id: i64, rechain: bool) -> Result<bool> {
    let entry = get_entry(conn, id)?;
    if entry.status == EntryStatus::Void || !entry.postings.iter().any(|p| p.rate_source == "missing") {
        return Ok(false);
    }
    check_mutation_lock(conn, &entry)?;
    let entity = entities::get_entity(conn, entry.entity_id)?;
    let inputs: Vec<PostingInput> = entry
        .postings
        .iter()
        .filter(|p| p.metadata.get("fx_auto").and_then(|v| v.as_bool()) != Some(true))
        .map(|p| {
            let mut pi = PostingInput {
                account_id: p.account_id,
                quantity: p.quantity.major(),
                memo: p.memo.clone(),
                metadata: p.metadata.clone(),
                external_id: p.external_id.clone(),
                fingerprint: p.fingerprint.clone(),
                ..Default::default()
            };
            if p.metadata.get("balance").and_then(|v| v.as_bool()) == Some(true) {
                pi.balance = true;
            } else if let Some(v) = p.metadata.get("value_in") {
                let q = v.get("quantity").and_then(|x| x.as_str()).and_then(|s| money::parse(s).ok());
                let c = v.get("commodity").and_then(|x| x.as_str()).map(|s| s.to_string());
                if let (Some(q), Some(c)) = (q, c) {
                    pi.value_in = Some((q, c));
                }
            } else if p.rate_source == "input" {
                pi.amount = Some(p.amount.major());
            }
            pi
        })
        .collect();
    let has_balance = inputs.iter().any(|p| p.balance);
    let keep_closed: std::collections::HashSet<i64> = entry.postings.iter().map(|p| p.account_id).collect();
    let mut prepared = prepare(conn, &entity, entry.date, &inputs, !has_balance, &keep_closed)?;
    if prepared.iter().any(|p| p.rate_source == "missing") {
        return Ok(false);
    }
    let tx = conn.transaction()?;
    let links = replacement_links(&tx, &entry, &mut prepared, None)?;
    replace_postings(&tx, id, &prepared, links)?;
    if rechain {
        if let Some(seq) = entry.seq {
            hashchain::rechain(&tx, entity.id, seq)?;
        }
    }
    crate::audit::log(&tx, "journal_entries", id, "revalue", None, None)?;
    tx.commit()?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

const ENTRY_SELECT: &str = "SELECT e.id, e.uid, e.entity_id, e.date, e.payee, e.description, e.notes, e.status, e.reverses_id, e.counterpart_id,
        e.template_id, e.template_version, e.origin, e.posted_at, e.created_at, e.seq, e.hash,
        (SELECT r.id FROM journal_entries r WHERE r.reverses_id = e.id LIMIT 1),
        (SELECT s.id FROM statement_lines s WHERE s.journal_entry_id = e.id ORDER BY s.id LIMIT 1),
        e.reviewed_at, e.refund_of_id, e.payee_id, COALESCE((SELECT name FROM payees WHERE id=e.payee_id),e.payee)
     FROM journal_entries e";

fn row_to_entry(r: &rusqlite::Row<'_>) -> rusqlite::Result<JournalEntry> {
    let date: String = r.get(3)?;
    let status: String = r.get(7)?;
    Ok(JournalEntry {
        id: r.get(0)?,
        uid: r.get(1)?,
        entity_id: r.get(2)?,
        date: NaiveDate::parse_from_str(&date, "%Y-%m-%d").unwrap_or_default(),
        payee: r.get(4)?,
        description: r.get(5)?,
        notes: r.get(6)?,
        status: EntryStatus::parse(&status).unwrap_or(EntryStatus::Posted),
        reverses_id: r.get(8)?,
        counterpart_id: r.get(9)?,
        template_id: r.get(10)?,
        template_version: r.get(11)?,
        origin: r.get(12)?,
        posted_at: r.get(13)?,
        created_at: r.get(14)?,
        seq: r.get(15)?,
        hash: r.get(16)?,
        reversed_by_id: r.get(17)?,
        statement_line_id: r.get(18)?,
        reviewed_at: r.get(19)?,
        refund_of_id: r.get(20)?,
        payee_id: r.get(21)?,
        display_payee: r.get(22)?,
        postings: Vec::new(),
        kind: String::new(),
        tags: Vec::new(),
        amount_functional: Decimal::ZERO,
    })
}

fn load_postings(conn: &Connection, entry_ids: &[i64]) -> Result<HashMap<i64, Vec<Posting>>> {
    let mut out: HashMap<i64, Vec<Posting>> = HashMap::new();
    if entry_ids.is_empty() {
        return Ok(out);
    }
    // Paths need the tree; one lean pass over the chart.
    let mut paths: HashMap<i64, (String, AccountType)> = HashMap::new();
    for (id, a) in accounts::path_index(conn)? {
        paths.insert(id, (a.path, a.r#type));
    }
    for chunk in entry_ids.chunks(500) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            "SELECT p.id, p.uid, p.journal_entry_id, p.account_id, p.quantity, p.amount, p.rate, p.rate_source, p.memo, p.metadata, p.external_id, p.fingerprint, p.reconciled_at, p.position,
                    ac.precision, fc.precision, ac.code, fc.code
             FROM postings p JOIN accounts a ON a.id = p.account_id JOIN commodities ac ON ac.id = a.commodity_id
                  JOIN journal_entries e ON e.id = p.journal_entry_id JOIN entities en ON en.id = e.entity_id
                  JOIN commodities fc ON fc.code = en.currency
             WHERE p.journal_entry_id IN ({placeholders}) ORDER BY p.journal_entry_id, p.position, p.id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
            let rate: Option<String> = r.get(6)?;
            let meta: String = r.get(9)?;
            let posting = Posting {
                id: r.get(0)?,
                uid: r.get(1)?,
                journal_entry_id: r.get(2)?,
                account_id: r.get(3)?,
                account_path: String::new(),
                account_type: AccountType::Asset,
                quantity: money::from_row(r, 4, 16, 14)?,
                amount: money::from_row(r, 5, 17, 15)?,
                rate: rate.and_then(|s| money::parse(&s).ok()),
                rate_source: r.get(7)?,
                memo: r.get(8)?,
                metadata: serde_json::from_str(&meta).unwrap_or(serde_json::json!({})),
                external_id: r.get(10)?,
                fingerprint: r.get(11)?,
                reconciled_at: r.get(12)?,
                position: r.get(13)?,
            };
            Ok(posting)
        })?;
        for row in rows {
            let mut p = row?;
            if let Some((path, t)) = paths.get(&p.account_id) {
                p.account_path = path.clone();
                p.account_type = *t;
            }
            out.entry(p.journal_entry_id).or_default().push(p);
        }
    }
    Ok(out)
}

fn finish(entry: &mut JournalEntry, postings: Vec<Posting>, roles: &HashMap<i64, String>, subtypes: &HashMap<i64, String>) {
    entry.amount_functional = postings.iter().filter(|p| !p.amount.is_negative() && !p.amount.is_zero()).map(|p| p.amount.major()).sum();
    entry.kind = if entry.refund_of_id.is_some() { "refund".into() } else { derive_kind(&postings, roles, subtypes) };
    entry.postings = postings;
}

fn derive_kind(postings: &[Posting], roles: &HashMap<i64, String>, subtypes: &HashMap<i64, String>) -> String {
    let has = |f: &dyn Fn(&Posting) -> bool| postings.iter().any(f);
    if has(&|p| roles.get(&p.account_id).map(|r| r == "opening_balance").unwrap_or(false)) {
        return "opening".into();
    }
    let is_fx = |p: &Posting| roles.get(&p.account_id).map(|r| matches!(r.as_str(), "fx_gain_loss" | "fx_gain_loss_legacy")).unwrap_or(false);
    let income = has(&|p| p.account_type == AccountType::Income);
    let expense = has(&|p| p.account_type == AccountType::Expense && !is_fx(p));
    let equity = has(&|p| p.account_type == AccountType::Equity);
    let liability = has(&|p| p.account_type == AccountType::Liability);
    let asset = has(&|p| p.account_type == AccountType::Asset);
    let holding = has(&|p| subtypes.get(&p.account_id).map(|s| s == "holding").unwrap_or(false));
    if holding {
        return "trade".into();
    }
    if income && !expense {
        return "income".into();
    }
    if expense {
        let refund = postings.iter().filter(|p| p.account_type == AccountType::Expense && !is_fx(p)).all(|p| p.quantity.is_negative());
        return if refund { "refund".into() } else { "expense".into() };
    }
    if equity {
        return "equity".into();
    }
    let commodities: std::collections::HashSet<&str> = postings.iter().map(|p| p.quantity.commodity()).collect();
    if commodities.len() > 1 {
        return "exchange".into();
    }
    if liability && asset {
        return "payment".into();
    }
    "transfer".into()
}

fn roles_and_subtypes(conn: &Connection) -> Result<(HashMap<i64, String>, HashMap<i64, String>)> {
    let mut stmt = conn.prepare("SELECT id, system_role, subtype FROM accounts")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
    let mut roles = HashMap::new();
    let mut subtypes = HashMap::new();
    for row in rows {
        let (id, role, sub) = row?;
        roles.insert(id, role);
        subtypes.insert(id, sub);
    }
    Ok((roles, subtypes))
}

pub fn get_entry(conn: &Connection, id: i64) -> Result<JournalEntry> {
    let sql = format!("{ENTRY_SELECT} WHERE e.id = ?1");
    let mut entry = conn.query_row(&sql, [id], row_to_entry).optional()?.ok_or_else(|| Error::NotFound(format!("journal entry {id}")))?;
    let mut postings = load_postings(conn, &[id])?;
    let (roles, subtypes) = roles_and_subtypes(conn)?;
    finish(&mut entry, postings.remove(&id).unwrap_or_default(), &roles, &subtypes);
    entry.tags = crate::tags::strings_for(conn, id)?;
    Ok(entry)
}

#[derive(Debug, Clone, Default)]
pub struct EntryFilter {
    pub entity_id: Option<i64>,
    pub account_id: Option<i64>,
    pub status: Option<EntryStatus>,
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub query: String,
    pub origin: String,
    /// Only posted entries no human has confirmed yet.
    pub only_unreviewed: bool,
    pub limit: i64,
    pub offset: i64,
}

pub fn list_entries(conn: &Connection, f: &EntryFilter) -> Result<(Vec<JournalEntry>, i64)> {
    let q = format!("%{}%", f.query.trim());
    let status = f.status.map(|s| s.as_str().to_string());
    let where_sql = "WHERE (?1 IS NULL OR e.entity_id = ?1)
          AND (?2 IS NULL OR EXISTS (SELECT 1 FROM postings p WHERE p.journal_entry_id = e.id AND p.account_id = ?2))
          AND (?3 IS NULL OR e.status = ?3)
          AND (?4 IS NULL OR e.date >= ?4) AND (?5 IS NULL OR e.date <= ?5)
          AND (?6 = '%%' OR e.payee LIKE ?6 OR EXISTS(SELECT 1 FROM payees cp WHERE cp.id=e.payee_id AND cp.name LIKE ?6) OR e.description LIKE ?6 OR e.notes LIKE ?6 OR EXISTS (SELECT 1 FROM postings p WHERE p.journal_entry_id = e.id AND (p.memo LIKE ?6 OR p.external_id LIKE ?6))
               OR EXISTS (SELECT 1 FROM entry_tags t WHERE t.entry_id = e.id AND (t.key || ':' || t.value LIKE ?6 OR t.key LIKE ?6)))
          AND (?7 = '' OR e.origin = ?7)
          AND (?8 = 0 OR (e.reviewed_at IS NULL AND e.status = 'posted'))";
    let params = params![
        f.entity_id,
        f.account_id,
        status,
        f.from.map(|d| d.to_string()),
        f.to.map(|d| d.to_string()),
        q,
        f.origin,
        f.only_unreviewed as i64,
        if f.limit <= 0 { 100 } else { f.limit.min(2000) },
        f.offset.max(0)
    ];
    let total: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM journal_entries e {where_sql}"), &params[..8], |r| r.get(0))?;
    // Pick the page of ids first so the per-row lookups in ENTRY_SELECT run only for the rows returned.
    let sql = format!("{ENTRY_SELECT} WHERE e.id IN (SELECT e.id FROM journal_entries e {where_sql} ORDER BY e.date DESC, e.id DESC LIMIT ?9 OFFSET ?10) ORDER BY e.date DESC, e.id DESC");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params, row_to_entry)?;
    let mut entries: Vec<JournalEntry> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    let ids: Vec<i64> = entries.iter().map(|e| e.id).collect();
    let mut postings = load_postings(conn, &ids)?;
    let (roles, subtypes) = roles_and_subtypes(conn)?;
    let mut tag_map = crate::tags::for_entries(conn, &ids)?;
    for e in entries.iter_mut() {
        finish(e, postings.remove(&e.id).unwrap_or_default(), &roles, &subtypes);
        e.tags = tag_map.remove(&e.id).unwrap_or_default();
    }
    Ok((entries, total))
}

pub fn count_by_status(conn: &Connection, status: &str) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM journal_entries WHERE status = ?1", [status], |r| r.get(0))?)
}

// ---------------------------------------------------------------------------
// Builders used by the capture screen and importers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SimpleEntry {
    pub entity_id: i64,
    pub date: NaiveDate,
    /// expense | income | transfer
    pub kind: String,
    pub account_id: i64,
    pub contra_account_id: Option<i64>,
    pub quantity: Decimal,
    pub contra_quantity: Option<Decimal>,
    pub payee: String,
    pub notes: String,
    pub splits: Vec<(i64, Decimal, String)>,
    pub status: EntryStatus,
    pub fee: Option<Decimal>,
    pub fee_account_id: Option<i64>,
    pub origin: String,
}

pub fn create_simple(conn: &mut Connection, s: SimpleEntry) -> Result<JournalEntry> {
    let input = simple_input(conn, s)?;
    create_entry(conn, input)
}
fn simple_input(conn: &Connection, s: SimpleEntry) -> Result<EntryInput> {
    let account = accounts::get_account(conn, s.account_id)?;
    if s.quantity <= Decimal::ZERO {
        return Err(Error::Invalid("amount must be positive".into()));
    }
    let mut input = EntryInput::new(s.entity_id, s.date);
    input.payee = s.payee.clone();
    input.notes = s.notes.clone();
    input.status = s.status;
    input.origin = if s.origin.is_empty() { "capture".into() } else { s.origin.clone() };
    match s.kind.as_str() {
        "expense" | "income" => {
            let sign = if s.kind == "expense" { Decimal::NEGATIVE_ONE } else { Decimal::ONE };
            input.postings.push(PostingInput::new(account.id, s.quantity * sign));
            if s.splits.is_empty() {
                let contra = s.contra_account_id.ok_or_else(|| Error::Invalid("a category account is required".into()))?;
                input.postings.push(PostingInput::balancing(contra));
            } else {
                crate::splits::resolve(s.quantity, &s.splits.iter().map(|(id, q, memo)| crate::splits::Leg { account_id: *id, quantity: Some(*q), memo: memo.clone() }).collect::<Vec<_>>())?;
                let n = s.splits.len();
                for (i, (acc, q, memo)) in s.splits.iter().enumerate() {
                    if i == n - 1 {
                        input.postings.push(PostingInput::balancing(*acc).memo(memo));
                    } else {
                        let value = Money::from_major(*q, &account.commodity, account.precision)?;
                        let value = if s.kind == "expense" { value } else { value.checked_neg()? };
                        input.postings.push(PostingInput::valued(*acc, value.major(), value.commodity()).memo(memo));
                    }
                }
            }
        }
        "transfer" => {
            let to_id = s.contra_account_id.ok_or_else(|| Error::Invalid("a destination account is required".into()))?;
            let to = accounts::get_account(conn, to_id)?;
            if to.entity_id != account.entity_id {
                return Err(Error::Invalid("use CreateTransfer for money between entities".into()));
            }
            let to_q = s.contra_quantity.unwrap_or(s.quantity);
            input.postings.push(PostingInput::new(account.id, -s.quantity));
            if let (Some(fee), Some(fee_acc)) = (s.fee.filter(|f| !f.is_zero()), s.fee_account_id) {
                input.postings.push(PostingInput::new(account.id, -fee).memo("fee"));
                input.postings.push(PostingInput::valued(fee_acc, fee, &account.commodity).memo("fee"));
            }
            if to.commodity == account.commodity {
                if to_q != s.quantity {
                    return Err(Error::Invalid("a transfer in one currency must receive what was sent; record a fee for the difference".into()));
                }
                input.postings.push(PostingInput::new(to.id, to_q));
            } else {
                input.postings.push(PostingInput::new(to.id, to_q));
                input.absorb_fx = true;
            }
        }
        other => return Err(Error::Invalid(format!("unknown simple entry kind {other:?}"))),
    }
    Ok(input)
}

#[derive(Debug, Clone)]
pub struct Transfer {
    pub date: NaiveDate,
    pub from_account_id: i64,
    pub to_account_id: i64,
    pub from_quantity: Decimal,
    pub to_quantity: Option<Decimal>,
    pub fee: Option<Decimal>,
    pub fee_account_id: Option<i64>,
    pub payee: String,
    pub notes: String,
    pub from_contra_account_id: Option<i64>,
    pub to_contra_account_id: Option<i64>,
    pub status: EntryStatus,
    pub origin: String,
    pub external: Option<(Option<String>, Option<String>)>,
}

/// A transfer inside one entity (one entry) or between entities (two linked entries).
pub fn create_transfer(conn: &mut Connection, t: Transfer) -> Result<(JournalEntry, Option<JournalEntry>)> {
    let from = accounts::get_account(conn, t.from_account_id)?;
    let to = accounts::get_account(conn, t.to_account_id)?;
    if t.from_quantity <= Decimal::ZERO {
        return Err(Error::Invalid("amount must be positive".into()));
    }
    let to_q = t.to_quantity.unwrap_or(t.from_quantity);
    let origin = if t.origin.is_empty() { "transfer".to_string() } else { t.origin.clone() };
    if from.entity_id == to.entity_id {
        let entry = create_simple(
            conn,
            SimpleEntry {
                entity_id: from.entity_id,
                date: t.date,
                kind: "transfer".into(),
                account_id: from.id,
                contra_account_id: Some(to.id),
                quantity: t.from_quantity,
                contra_quantity: Some(to_q),
                payee: t.payee,
                notes: t.notes,
                splits: vec![],
                status: t.status,
                fee: t.fee,
                fee_account_id: t.fee_account_id,
                origin,
            },
        )?;
        return Ok((entry, None));
    }
    let from_entity = entities::get_entity(conn, from.entity_id)?;
    let to_entity = entities::get_entity(conn, to.entity_id)?;
    let from_contra = match t.from_contra_account_id {
        Some(id) => accounts::get_account(conn, id)?,
        None => {
            let role = if from_entity.kind == "company" { "owner_distributions" } else { "investment_in_entity" };
            accounts::find_by_role(conn, from_entity.id, role)?
        }
    };
    let to_contra = match t.to_contra_account_id {
        Some(id) => accounts::get_account(conn, id)?,
        None => {
            let role = if to_entity.kind == "company" { "owner_contributions" } else { "distributions_received" };
            accounts::find_by_role(conn, to_entity.id, role)?
        }
    };
    let mut a = EntryInput::new(from_entity.id, t.date);
    a.payee = if t.payee.is_empty() { format!("Transfer to {}", to_entity.name) } else { t.payee.clone() };
    a.notes = t.notes.clone();
    a.status = t.status;
    a.origin = origin.clone();
    a.postings.push(PostingInput::new(from.id, -t.from_quantity));
    if let (Some(fee), Some(fee_acc)) = (t.fee.filter(|f| !f.is_zero()), t.fee_account_id) {
        a.postings.push(PostingInput::new(from.id, -fee).memo("fee"));
        a.postings.push(PostingInput::valued(fee_acc, fee, &from.commodity).memo("fee"));
    }
    a.postings.push(PostingInput::balancing(from_contra.id));
    let mut b = EntryInput::new(to_entity.id, t.date);
    b.payee = if t.payee.is_empty() { format!("Transfer from {}", from_entity.name) } else { t.payee.clone() };
    b.notes = t.notes.clone();
    b.status = t.status;
    b.origin = origin;
    b.postings.push(PostingInput::new(to.id, to_q));
    b.postings.push(PostingInput::balancing(to_contra.id));
    let first = create_entry(conn, a)?;
    b.counterpart_id = Some(first.id);
    let second = match create_entry(conn, b) {
        Ok(e) => e,
        Err(e) => {
            // Roll back the first half so the books do not keep a one-sided transfer.
            let _ = conn.execute("DELETE FROM journal_entries WHERE id = ?1", [first.id]);
            return Err(e);
        }
    };
    conn.execute("UPDATE journal_entries SET counterpart_id = ?2 WHERE id = ?1", params![first.id, second.id])?;
    Ok((get_entry(conn, first.id)?, Some(second)))
}

/// A human confirmed these entries (or touched them): stamp the review mark. Outside the chain.
pub fn mark_reviewed(conn: &Connection, ids: &[i64]) -> Result<usize> {
    let mut n = 0;
    let ts = now_ts();
    for chunk in ids.chunks(500) {
        let marks = vec!["?"; chunk.len()].join(",");
        let mut p: Vec<&dyn rusqlite::ToSql> = vec![&ts];
        p.extend(chunk.iter().map(|x| x as &dyn rusqlite::ToSql));
        n += conn.execute(&format!("UPDATE journal_entries SET reviewed_at = ?1 WHERE id IN ({marks}) AND reviewed_at IS NULL"), &p[..])?;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use rust_decimal::prelude::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }
    fn date(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn setup() -> (Db, Entity, Account, Account, Account) {
        let db = Db::open_memory().unwrap();
        let (entity, n26, food, inter) = {
            let mut conn = db.conn();
            let entity = entities::create_entity(&mut conn, "Personal", "person", "PT", "EUR").unwrap();
            let n26 = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank", "N26"], "bank", "EUR").unwrap();
            let food = accounts::ensure_account(&conn, entity.id, AccountType::Expense, &["Food"], "expense", "EUR").unwrap();
            let inter = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank", "Inter"], "bank", "BRL").unwrap();
            rates::set_price(&conn, "BRL", "EUR", date("2026-01-01"), d("0.17"), "manual").unwrap();
            (entity, n26, food, inter)
        };
        (db, entity, n26, food, inter)
    }

    #[test]
    fn simple_expense_balances_and_hashes() {
        let (db, entity, n26, food, _) = setup();
        let mut conn = db.conn();
        let e = create_simple(
            &mut conn,
            SimpleEntry {
                entity_id: entity.id,
                date: date("2026-02-01"),
                kind: "expense".into(),
                account_id: n26.id,
                contra_account_id: Some(food.id),
                quantity: d("3.20"),
                contra_quantity: None,
                payee: "Cafe".into(),
                notes: String::new(),
                splits: vec![],
                status: EntryStatus::Posted,
                fee: None,
                fee_account_id: None,
                origin: String::new(),
            },
        )
        .unwrap();
        assert_eq!(e.postings.len(), 2);
        assert_eq!(e.postings[0].quantity.major(), d("-3.20"));
        assert_eq!(e.postings[1].quantity.major(), d("3.20"));
        assert_eq!(e.kind, "expense");
        assert_eq!(e.seq, Some(1));
        assert!(e.hash.is_some());
        let report = hashchain::verify(&conn, entity.id).unwrap();
        assert_eq!(report.checked, 1);
        assert!(report.first_bad_seq.is_none());
        let accs = accounts::list_accounts_with_balances(&conn, Some(entity.id), false, None).unwrap();
        let n26b = accs.iter().find(|a| a.id == n26.id).unwrap();
        assert_eq!(n26b.balance, d("-3.20"));
        let foodb = accs.iter().find(|a| a.id == food.id).unwrap();
        assert_eq!(foodb.balance, d("3.20"));
    }

    #[test]
    fn foreign_expense_is_valued_in_functional_currency() {
        let (db, entity, _, food, inter) = setup();
        let mut conn = db.conn();
        let e = create_simple(
            &mut conn,
            SimpleEntry {
                entity_id: entity.id,
                date: date("2026-02-01"),
                kind: "expense".into(),
                account_id: inter.id,
                contra_account_id: Some(food.id),
                quantity: d("34.04"),
                contra_quantity: None,
                payee: "Mercado".into(),
                notes: String::new(),
                splits: vec![],
                status: EntryStatus::Posted,
                fee: None,
                fee_account_id: None,
                origin: String::new(),
            },
        )
        .unwrap();
        let bank = &e.postings[0];
        assert_eq!(bank.quantity.major(), d("-34.04"));
        assert_eq!(bank.quantity.commodity(), "BRL");
        assert_eq!(bank.amount.commodity(), "EUR");
        assert!(bank.quantity.checked_add(bank.amount).is_err());
        assert_eq!(bank.amount.major(), d("-5.79"));
        assert_eq!(bank.rate_source, "nearest");
        let exp = &e.postings[1];
        assert_eq!(exp.quantity.major(), d("5.79"));
        assert_eq!(exp.amount.major(), d("5.79"));
        let total: Decimal = e.postings.iter().map(|p| p.amount.major()).sum();
        assert_eq!(total, Decimal::ZERO);
    }

    #[test]
    fn unbalanced_entry_is_rejected_and_exchange_absorbs_fx() {
        let (db, entity, n26, _, inter) = setup();
        let mut conn = db.conn();
        let mut input = EntryInput::new(entity.id, date("2026-02-01"));
        input.postings.push(PostingInput::new(n26.id, d("-10")));
        input.postings.push(PostingInput::new(inter.id, d("50")));
        assert!(matches!(create_entry(&mut conn, input.clone()), Err(Error::Unbalanced(_))));
        input.absorb_fx = true;
        let e = create_entry(&mut conn, input).unwrap();
        assert_eq!(e.postings.len(), 3);
        assert_eq!(e.kind, "exchange");
        let fx = &e.postings[2];
        assert_eq!(fx.account_path, "Expenses:FX gain/loss");
        assert_eq!(fx.amount.major(), d("1.50")); // 10 EUR sent, 50 BRL = 8.50 EUR received, 1.50 EUR lost
    }

    #[test]
    fn lock_date_and_reversal() {
        let (db, entity, n26, food, _) = setup();
        let mut conn = db.conn();
        let e = create_simple(
            &mut conn,
            SimpleEntry {
                entity_id: entity.id,
                date: date("2026-02-01"),
                kind: "expense".into(),
                account_id: n26.id,
                contra_account_id: Some(food.id),
                quantity: d("10"),
                contra_quantity: None,
                payee: "x".into(),
                notes: String::new(),
                splits: vec![],
                status: EntryStatus::Posted,
                fee: None,
                fee_account_id: None,
                origin: String::new(),
            },
        )
        .unwrap();
        entities::update_entity(&mut conn, entity.id, "", "", "", Some(date("2026-02-15")), false).unwrap();
        let mut upd = EntryInput::new(entity.id, date("2026-02-01"));
        upd.payee = "changed".into();
        upd.postings = vec![PostingInput::new(n26.id, d("-11")), PostingInput::balancing(food.id)];
        assert!(matches!(update_entry(&mut conn, e.id, upd), Err(Error::Locked(_))));
        assert!(matches!(void_entry(&mut conn, e.id, Some(date("2026-02-10")), ""), Err(Error::Locked(_))));
        let voided = void_entry(&mut conn, e.id, Some(date("2026-03-01")), "wrong amount").unwrap();
        assert_eq!(voided.status, EntryStatus::Void);
        let rev = get_entry(&conn, voided.reversed_by_id.unwrap()).unwrap();
        assert_eq!(rev.postings[0].quantity.major(), d("10"));
        assert_eq!(rev.reverses_id, Some(e.id));
        let accs = accounts::list_accounts_with_balances(&conn, Some(entity.id), false, None).unwrap();
        assert_eq!(accs.iter().find(|a| a.id == n26.id).unwrap().balance, Decimal::ZERO);
        assert!(hashchain::verify(&conn, entity.id).unwrap().first_bad_seq.is_none());
    }

    #[test]
    fn cross_entity_transfer_makes_two_balanced_entries() {
        let (db, personal, n26, _, _) = setup();
        let mut conn = db.conn();
        let llc = entities::create_entity(&mut conn, "LLC", "company", "US", "USD").unwrap();
        let mercury = accounts::ensure_account(&conn, llc.id, AccountType::Asset, &["Mercury"], "bank", "USD").unwrap();
        rates::set_price(&conn, "USD", "EUR", date("2026-01-01"), d("0.88"), "manual").unwrap();
        let (a, b) = create_transfer(
            &mut conn,
            Transfer {
                date: date("2026-02-01"),
                from_account_id: mercury.id,
                to_account_id: n26.id,
                from_quantity: d("1000"),
                to_quantity: Some(d("870")),
                fee: None,
                fee_account_id: None,
                payee: String::new(),
                notes: String::new(),
                from_contra_account_id: None,
                to_contra_account_id: None,
                status: EntryStatus::Posted,
                origin: String::new(),
                external: None,
            },
        )
        .unwrap();
        let b = b.unwrap();
        assert_eq!(a.entity_id, llc.id);
        assert_eq!(b.entity_id, personal.id);
        assert_eq!(a.counterpart_id, Some(b.id));
        assert_eq!(b.counterpart_id, Some(a.id));
        assert_eq!(a.postings[1].account_path, "Equity:Owner distributions");
        assert_eq!(b.postings[1].account_path, "Income:Distributions received");
        assert_eq!(b.postings[0].quantity.major(), d("870"));
        for e in [&a, &b] {
            let total: Decimal = e.postings.iter().map(|p| p.amount.major()).sum();
            assert_eq!(total, Decimal::ZERO);
        }
    }

    #[test]
    fn splits_in_foreign_currency() {
        let (db, entity, _, food, inter) = setup();
        let mut conn = db.conn();
        let house = accounts::ensure_account(&conn, entity.id, AccountType::Expense, &["Household"], "expense", "EUR").unwrap();
        let e = create_simple(
            &mut conn,
            SimpleEntry {
                entity_id: entity.id,
                date: date("2026-02-01"),
                kind: "expense".into(),
                account_id: inter.id,
                contra_account_id: None,
                quantity: d("41"),
                contra_quantity: None,
                payee: "Mercado".into(),
                notes: String::new(),
                splits: vec![(food.id, d("30"), String::new()), (house.id, d("11"), String::new())],
                status: EntryStatus::Posted,
                fee: None,
                fee_account_id: None,
                origin: String::new(),
            },
        )
        .unwrap();
        assert_eq!(e.postings.len(), 3);
        assert_eq!(e.postings[1].amount.major(), d("5.10"));
        assert_eq!(e.postings[2].amount.major(), d("1.87"));
        let total: Decimal = e.postings.iter().map(|p| p.amount.major()).sum();
        assert_eq!(total, Decimal::ZERO);
    }

    #[test]
    fn missing_rate_is_flagged_and_revalued_later() {
        let (db, entity, _, food, inter) = setup();
        let mut conn = db.conn();
        let e = create_simple(
            &mut conn,
            SimpleEntry {
                entity_id: entity.id,
                date: date("2024-01-10"),
                kind: "expense".into(),
                account_id: inter.id,
                contra_account_id: Some(food.id),
                quantity: d("100"),
                contra_quantity: None,
                payee: "old".into(),
                notes: String::new(),
                splits: vec![],
                status: EntryStatus::Posted,
                fee: None,
                fee_account_id: None,
                origin: String::new(),
            },
        )
        .unwrap();
        assert_eq!(e.postings[0].rate_source, "missing");
        assert_eq!(e.postings[0].amount.major(), d("-100"));
        rates::set_price(&conn, "BRL", "EUR", date("2024-01-10"), d("0.18"), "manual").unwrap();
        let n = rates::revalue_missing(&mut conn).unwrap();
        assert_eq!(n.fixed, 1);
        let e = get_entry(&conn, e.id).unwrap();
        assert_eq!(e.postings[0].amount.major(), d("-18"));
        assert_eq!(e.postings[1].quantity.major(), d("18"));
        assert_eq!(e.postings[0].rate_source, "exact");
    }
}

/// Operations proposed by the UI. Preview runs exactly the same accounting code
/// inside a rolled-back transaction; confirmation recomputes it before committing.
#[derive(Debug, Clone)]
pub enum PostingProposal {
    Capture { entry: SimpleEntry, tags: String },
    Categorize { id: i64, legs: Vec<crate::splits::Leg>, payee: Option<String>, existing: bool },
}
pub fn confirm_posting(conn: &mut Connection, proposal: PostingProposal, confirmation: Option<&str>) -> Result<(JournalEntry, String)> {
    use sha2::{Digest, Sha256};
    let tx = conn.transaction()?;
    let stamp: (i64, i64, i64) =
        tx.query_row("SELECT (SELECT COALESCE(MAX(id),0) FROM audit_log), (SELECT COALESCE(MAX(id),0) FROM journal_entries), (SELECT COUNT(*) FROM journal_entries)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
    let mut entry = match proposal {
        PostingProposal::Capture { entry, tags } => {
            let tags = crate::tags::parse(&tags)?;
            let input = simple_input(&tx, entry)?;
            let entry = create_entry_in_transaction(&tx, input)?;
            crate::tags::set_tags_in_transaction(&tx, entry.id, &tags)?;
            get_entry(&tx, entry.id)?
        }
        PostingProposal::Categorize { id, legs, payee, existing } => categorize_in_transaction(&tx, id, &legs, payee.as_deref(), existing)?,
    };
    let mut canonical = serde_json::to_value(&entry)?;
    let object = canonical.as_object_mut().unwrap();
    for field in ["uid", "hash", "seq", "posted_at", "created_at", "reviewed_at"] {
        object.remove(field);
    }
    for posting in canonical["postings"].as_array_mut().unwrap() {
        posting.as_object_mut().unwrap().remove("uid");
    }
    let token = hex::encode(Sha256::digest(serde_json::to_vec(&(stamp, canonical))?));
    match confirmation {
        Some(expected) if expected == token => tx.commit()?,
        Some(_) => return Err(Error::Conflict("transaction changed since preview; preview it again before confirming".into())),
        None => {
            tx.rollback()?;
            entry.seq = None;
            entry.hash = None;
        }
    }
    Ok((entry, token))
}
