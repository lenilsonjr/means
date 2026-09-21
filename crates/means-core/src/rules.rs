//! Bank rules: patterns on statement lines that draft journal entries.

use std::collections::HashMap;

use rusqlite::{params, Connection, OptionalExtension};
use rust_decimal::Decimal;

use crate::accounts;
use crate::journal;
use crate::model::*;
use crate::templates;
use crate::{now_ts, Error, Result};

const SELECT: &str = "SELECT id, entity_id, name, position, enabled, conditions, account_id, template_id, payee, hits_count, created_at, tags FROM rules";

fn row_to_rule(r: &rusqlite::Row<'_>) -> rusqlite::Result<Rule> {
    let conds: String = r.get(5)?;
    Ok(Rule {
        id: r.get(0)?,
        entity_id: r.get(1)?,
        name: r.get(2)?,
        position: r.get(3)?,
        enabled: r.get::<_, i64>(4)? != 0,
        conditions: serde_json::from_str(&conds).unwrap_or_default(),
        account_id: r.get(6)?,
        template_id: r.get(7)?,
        payee: r.get(8)?,
        hits_count: r.get(9)?,
        created_at: r.get(10)?,
        tags: r.get(11)?,
    })
}

pub fn list_rules(conn: &Connection, entity_id: Option<i64>) -> Result<Vec<Rule>> {
    let sql = format!("{SELECT} WHERE (?1 IS NULL OR entity_id = ?1) ORDER BY position, id");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![entity_id], row_to_rule)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub fn get_rule(conn: &Connection, id: i64) -> Result<Rule> {
    let sql = format!("{SELECT} WHERE id = ?1");
    conn.query_row(&sql, [id], row_to_rule).optional()?.ok_or_else(|| Error::NotFound(format!("rule {id}")))
}

pub fn save_rule(conn: &Connection, rule: &Rule) -> Result<Rule> {
    if rule.name.trim().is_empty() {
        return Err(Error::Invalid("rule name is required".into()));
    }
    if rule.conditions.is_empty() {
        return Err(Error::Invalid("a rule needs at least one condition".into()));
    }
    for c in &rule.conditions {
        if !matches!(c.field.as_str(), "description" | "payee" | "reference" | "amount" | "account") {
            return Err(Error::Invalid(format!("unknown condition field {:?}", c.field)));
        }
        if !matches!(c.op.as_str(), "contains" | "equals" | "starts_with" | "regex" | "gt" | "lt" | "eq") {
            return Err(Error::Invalid(format!("unknown condition op {:?}", c.op)));
        }
        if c.op == "regex" {
            regex::Regex::new(&c.value).map_err(|e| Error::Invalid(format!("bad regex: {e}")))?;
        }
    }
    if rule.account_id.is_none() && rule.template_id.is_none() {
        return Err(Error::Invalid("a rule posts to an account or applies a template".into()));
    }
    crate::tags::parse(&rule.tags)?;
    if let Some(a) = rule.account_id {
        let acc = accounts::get_account(conn, a)?;
        if acc.entity_id != rule.entity_id {
            return Err(Error::Invalid("the contra account belongs to another entity".into()));
        }
        if acc.placeholder {
            return Err(Error::Invalid("the contra account cannot be a placeholder".into()));
        }
    }
    if let Some(t) = rule.template_id {
        let tpl = templates::get_template(conn, t)?;
        if tpl.entity_id != rule.entity_id {
            return Err(Error::Invalid("the template belongs to another entity".into()));
        }
    }
    let conds = serde_json::to_string(&rule.conditions)?;
    if rule.id == 0 {
        let position =
            if rule.position == 0 { conn.query_row("SELECT COALESCE(MAX(position), 0) + 10 FROM rules WHERE entity_id = ?1", [rule.entity_id], |r| r.get::<_, i32>(0))? } else { rule.position };
        conn.execute(
            "INSERT INTO rules (entity_id, name, position, enabled, conditions, account_id, template_id, payee, created_at, tags) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![rule.entity_id, rule.name.trim(), position, rule.enabled as i64, conds, rule.account_id, rule.template_id, rule.payee.trim(), now_ts(), rule.tags.trim()],
        )?;
        get_rule(conn, conn.last_insert_rowid())
    } else {
        conn.execute(
            "UPDATE rules SET name = ?2, position = ?3, enabled = ?4, conditions = ?5, account_id = ?6, template_id = ?7, payee = ?8, tags = ?9 WHERE id = ?1",
            params![rule.id, rule.name.trim(), rule.position, rule.enabled as i64, conds, rule.account_id, rule.template_id, rule.payee.trim(), rule.tags.trim()],
        )?;
        get_rule(conn, rule.id)
    }
}

pub fn delete_rule(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM rules WHERE id = ?1", [id])?;
    Ok(())
}

fn field_value(sl: &StatementLine, field: &str, payee: &str) -> String {
    match field {
        "description" => sl.description.clone(),
        "payee" => payee.to_string(),
        "reference" => sl.reference.clone(),
        "amount" => sl.amount.map(|a| a.major().to_string()).unwrap_or_default(),
        "account" => sl.account_id.map(|a| a.to_string()).unwrap_or_default(),
        _ => String::new(),
    }
}

pub fn matches(rule: &Rule, sl: &StatementLine) -> bool {
    matches_with_payee(rule, sl, &sl.description)
}
pub fn matches_with_payee(rule: &Rule, sl: &StatementLine, payee: &str) -> bool {
    rule.enabled
        && rule.conditions.iter().all(|c| {
            let v = field_value(sl, &c.field, payee);
            let vl = v.to_lowercase();
            let cl = c.value.to_lowercase();
            match c.op.as_str() {
                "contains" => vl.contains(cl.trim()),
                "equals" => vl.trim() == cl.trim(),
                "starts_with" => vl.trim_start().starts_with(cl.trim()),
                "regex" => regex::RegexBuilder::new(&c.value).case_insensitive(true).build().map(|re| re.is_match(&v)).unwrap_or(false),
                "gt" | "lt" | "eq" => {
                    let a = sl.amount.map(|a| a.major()).unwrap_or(Decimal::ZERO);
                    let b = crate::money::parse(&c.value).unwrap_or(Decimal::ZERO);
                    match c.op.as_str() {
                        "gt" => a > b,
                        "lt" => a < b,
                        _ => a == b,
                    }
                }
                _ => false,
            }
        })
}

pub fn first_match<'a>(rules: &'a [Rule], sl: &StatementLine) -> Option<&'a Rule> {
    rules.iter().find(|r| matches(r, sl))
}

pub fn first_match_with_payee<'a>(rules: &'a [Rule], sl: &StatementLine, payee: &str) -> Option<&'a Rule> {
    rules.iter().find(|r| matches_with_payee(r, sl, payee))
}
fn resolved_rule<'a>(conn: &Connection, entity: i64, rules: &'a [Rule], sl: &StatementLine) -> Result<Option<&'a Rule>> {
    let resolved = crate::payees::resolve(conn, entity, &sl.description)?;
    Ok(first_match_with_payee(rules, sl, resolved.as_ref().map(|p| p.name.as_str()).unwrap_or(&sl.description)))
}

/// Voided history retains its original external ID. The statement line remains
/// the authoritative bank reference when that ID cannot be reused on a new posting.
fn posting_reference(conn: &Connection, account_id: i64, reference: &str) -> Result<Option<String>> {
    if reference.is_empty() {
        return Ok(None);
    }
    let held_by_void: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM postings p JOIN journal_entries e ON e.id=p.journal_entry_id
         WHERE p.account_id=?1 AND p.external_id=?2 AND e.status='void')",
        params![account_id, reference],
        |row| row.get(0),
    )?;
    Ok((!held_by_void).then(|| reference.to_owned()))
}

/// Draft (or post) an entry for an unmatched line: the bank posting plus the contra posting
/// from the first matching rule, or Suspense when no rule matches.
pub fn draft_from_line(conn: &mut Connection, account: &Account, sl: &mut StatementLine, rules: &[Rule]) -> Result<()> {
    let (Some(date), Some(amount)) = (sl.date, sl.amount) else {
        return Err(Error::Invalid("line has no date or amount".into()));
    };
    let tx = conn.transaction()?;
    let resolved = crate::payees::resolve(&tx, account.entity_id, &sl.description)?;
    let rule = first_match_with_payee(rules, sl, resolved.as_ref().map(|p| p.name.as_str()).unwrap_or(&sl.description));
    let entry = match rule {
        Some(r) if r.template_id.is_some() => {
            let tid = r.template_id.unwrap();
            let tpl = templates::get_template(&tx, tid)?;
            // The template's line on this bank account takes the statement amount.
            let mut inputs: HashMap<usize, Decimal> = HashMap::new();
            for (i, l) in tpl.lines.iter().enumerate() {
                if l.account_id == account.id && l.method == "input" {
                    inputs.insert(i + 1, amount.major());
                }
            }
            let payee = if r.payee.is_empty() { sl.description.clone() } else { r.payee.clone() };
            templates::apply_template_in_transaction(&tx, tid, date, &inputs, &payee, EntryStatus::Posted, "rule")?
        }
        _ => {
            let (contra, status, origin, payee) = match rule {
                Some(r) => (r.account_id.unwrap(), EntryStatus::Posted, "rule", if r.payee.is_empty() { sl.description.clone() } else { r.payee.clone() }),
                None => (accounts::find_by_role(&tx, account.entity_id, "suspense")?.id, EntryStatus::Draft, "import", sl.description.clone()),
            };
            let mut input = EntryInput::new(account.entity_id, date);
            input.payee = payee;
            input.description = sl.description.clone();
            input.status = status;
            input.origin = origin.into();
            let mut bank = PostingInput::new(account.id, amount.major()).external(posting_reference(&tx, account.id, &sl.reference)?, Some(sl.fingerprint.clone()));
            if let Some(o) = sl.raw.get("_original") {
                bank = bank.meta("original", o.clone());
            }
            input.postings.push(bank);
            input.postings.push(PostingInput::balancing(contra));
            journal::create_entry_in_transaction(&tx, input)?
        }
    };
    let canonical = match rule.filter(|r| !r.payee.trim().is_empty()) {
        Some(r) => crate::payees::explicit(&tx, account.entity_id, &r.payee)?,
        None => resolved,
    };
    crate::payees::link(&tx, entry.id, canonical.map(|p| p.id))?;
    if let Some(r) = rule {
        if !r.tags.trim().is_empty() {
            crate::tags::set_tags_in_transaction(&tx, entry.id, &crate::tags::parse(&r.tags)?)?;
        }
    }
    let bank_posting = entry
        .postings
        .iter()
        .find(|p| p.account_id == account.id && p.quantity == amount)
        .map(|p| p.id)
        .ok_or_else(|| Error::Invalid("the rule must preserve the statement account and amount".into()))?;
    tx.execute("UPDATE postings SET reconciled_at = ?2 WHERE id = ?1", params![bank_posting, now_ts()])?;
    let mut note = rule.map(|r| format!("rule: {}", r.name)).unwrap_or_default();
    if entry.payee_id.is_none() {
        let all = crate::payees::list(&tx, account.entity_id)?;
        let candidates = crate::payees::candidates(&all, &sl.description);
        if candidates.len() > 1 && rule.is_none_or(|r| r.payee.trim().is_empty()) {
            if !note.is_empty() {
                note.push_str("; ");
            }
            note.push_str(&format!("ambiguous payee: {}", candidates.iter().map(|p| format!("#{} {}", p.id, p.name)).collect::<Vec<_>>().join(", ")));
        }
    }
    tx.execute("UPDATE statement_lines SET status = 'created', journal_entry_id = ?2, posting_id = ?3, note = ?4 WHERE id = ?1", params![sl.id, entry.id, bank_posting, note])?;
    if let Some(r) = rule {
        tx.execute("UPDATE rules SET hits_count = hits_count + 1 WHERE id = ?1", [r.id])?;
    }
    tx.commit()?;
    sl.note = note;
    sl.status = "created".into();
    sl.journal_entry_id = Some(entry.id);
    sl.posting_id = Some(bank_posting);
    Ok(())
}

/// Apply rules to unmatched lines, and re-draft Suspense drafts that a rule now matches.
pub fn run_rules(conn: &mut Connection, entity_id: Option<i64>, account_id: Option<i64>, include_drafts: bool) -> Result<(usize, usize)> {
    let rules = list_rules(conn, entity_id)?;
    let lines = crate::imports::list_lines(conn, account_id, "unmatched", None, 100_000)?;
    let mut drafted = 0;
    for mut sl in lines {
        let Some(acc_id) = sl.account_id else { continue };
        let account = accounts::get_account(conn, acc_id)?;
        if entity_id.map(|e| e != account.entity_id).unwrap_or(false) {
            continue;
        }
        let rules_for: Vec<Rule> = rules.iter().filter(|r| r.entity_id == account.entity_id).cloned().collect();
        if resolved_rule(conn, account.entity_id, &rules_for, &sl)?.is_some() && draft_from_line(conn, &account, &mut sl, &rules_for).is_ok() {
            drafted += 1;
        }
    }
    let mut redrafted = 0;
    if include_drafts {
        // Suspense drafts created from lines: re-run the rules on their lines.
        let lines = crate::imports::list_lines(conn, account_id, "created", None, 100_000)?;
        for sl in lines {
            let Some(eid) = sl.journal_entry_id else { continue };
            let entry = journal::get_entry(conn, eid)?;
            if entry.status != EntryStatus::Draft {
                continue;
            }
            let Some(acc_id) = sl.account_id else { continue };
            let account = accounts::get_account(conn, acc_id)?;
            let rules_for: Vec<Rule> = rules.iter().filter(|r| r.entity_id == account.entity_id).cloned().collect();
            let Some(rule) = resolved_rule(conn, account.entity_id, &rules_for, &sl)? else { continue };
            let Some(contra) = rule.account_id else { continue };
            let mut input = EntryInput::new(entry.entity_id, entry.date);
            input.payee = if rule.payee.is_empty() { entry.payee.clone() } else { rule.payee.clone() };
            input.description = entry.description.clone();
            input.notes = entry.notes.clone();
            input.status = EntryStatus::Posted;
            input.origin = "rule".into();
            for p in &entry.postings {
                if p.account_id == account.id {
                    input.postings.push(PostingInput {
                        account_id: p.account_id,
                        quantity: p.quantity.major(),
                        amount: Some(p.amount.major()),
                        memo: p.memo.clone(),
                        metadata: p.metadata.clone(),
                        external_id: p.external_id.clone(),
                        fingerprint: p.fingerprint.clone(),
                        ..Default::default()
                    });
                } else {
                    input.postings.push(PostingInput::balancing(contra));
                }
            }
            let tx = conn.transaction()?;
            if journal::update_entry_in_transaction(&tx, eid, input).is_ok() {
                let canonical = if rule.payee.trim().is_empty() {
                    entry.payee_id.or(crate::payees::resolve(&tx, account.entity_id, &sl.description)?.map(|p| p.id))
                } else {
                    crate::payees::explicit(&tx, account.entity_id, &rule.payee)?.map(|p| p.id)
                };
                crate::payees::link(&tx, eid, canonical)?;
                // A rule's re-draft is a machine decision: it waits for review again.
                tx.execute("UPDATE journal_entries SET reviewed_at = NULL WHERE id = ?1", [eid])?;
                if !rule.tags.trim().is_empty() {
                    crate::tags::set_tags_in_transaction(&tx, eid, &crate::tags::parse(&rule.tags)?)?;
                }
                tx.execute("UPDATE rules SET hits_count = hits_count + 1 WHERE id = ?1", [rule.id])?;
                tx.execute("UPDATE statement_lines SET note = ?2 WHERE id = ?1", params![sl.id, format!("rule: {}", rule.name)])?;
                tx.commit()?;
                redrafted += 1;
            }
        }
    }
    Ok((drafted, redrafted))
}

/// Create (or post) the entry a statement line evidences, with the contra account, splits or template chosen on the review screen.
pub fn create_entry_from_line(
    conn: &mut Connection,
    line_id: i64,
    contra_account_id: Option<i64>,
    payee: &str,
    template_id: Option<i64>,
    post: bool,
    splits: &[(i64, Decimal, String)],
) -> Result<JournalEntry> {
    let mut sl = crate::imports::get_line(conn, line_id)?;
    if sl.status == "matched" || sl.status == "created" {
        return Err(Error::Invalid("this line already evidences an entry".into()));
    }
    let account = accounts::get_account(conn, sl.account_id.ok_or_else(|| Error::Invalid("line has no account".into()))?)?;
    let (Some(date), Some(amount)) = (sl.date, sl.amount) else { return Err(Error::Invalid("line has no date or amount".into())) };
    let status = if post { EntryStatus::Posted } else { EntryStatus::Draft };
    let tx = conn.transaction()?;
    let entry = if let Some(tid) = template_id {
        let tpl = templates::get_template(&tx, tid)?;
        let mut inputs: HashMap<usize, Decimal> = HashMap::new();
        for (i, l) in tpl.lines.iter().enumerate() {
            if l.account_id == account.id && l.method == "input" {
                inputs.insert(i + 1, amount.major());
            }
        }
        templates::apply_template_in_transaction(&tx, tid, date, &inputs, if payee.is_empty() { &sl.description } else { payee }, status, "import")?
    } else {
        let mut input = EntryInput::new(account.entity_id, date);
        input.payee = if payee.trim().is_empty() { sl.description.clone() } else { payee.trim().to_string() };
        input.description = sl.description.clone();
        input.status = status;
        input.origin = "import".into();
        let mut bank = PostingInput::new(account.id, amount.major()).external(posting_reference(&tx, account.id, &sl.reference)?, Some(sl.fingerprint.clone()));
        if let Some(o) = sl.raw.get("_original") {
            bank = bank.meta("original", o.clone());
        }
        input.postings.push(bank);
        if splits.is_empty() {
            let contra = contra_account_id.ok_or_else(|| Error::Invalid("choose a contra account".into()))?;
            input.postings.push(PostingInput::balancing(contra));
        } else {
            let total = splits.iter().try_fold(Decimal::ZERO, |total, (_, q, _)| total.checked_add(*q).ok_or_else(|| Error::Invalid("split total overflow".into())))?;
            if total != amount.major().abs() {
                return Err(Error::Invalid(format!("splits sum to {} but the line is {}", crate::money::plain(total), crate::money::plain(amount.major().abs()))));
            }
            let n = splits.len();
            for (i, (acc, q, memo)) in splits.iter().enumerate() {
                if i == n - 1 {
                    input.postings.push(PostingInput::balancing(*acc).memo(memo));
                } else {
                    // The contra side has the opposite sign of the bank posting.
                    let value = crate::money::Money::from_major(*q, &account.commodity, account.precision)?;
                    let signed = if amount.is_negative() { value } else { value.checked_neg()? };
                    input.postings.push(PostingInput::valued(*acc, signed.major(), signed.commodity()).memo(memo));
                }
            }
        }
        journal::create_entry_in_transaction(&tx, input)?
    };
    let bank_posting = entry.postings.iter().find(|p| p.account_id == account.id).map(|p| p.id);
    let resolved = if payee.trim().is_empty() { crate::payees::resolve(&tx, account.entity_id, &sl.description)? } else { crate::payees::explicit(&tx, account.entity_id, payee)? };
    crate::payees::link(&tx, entry.id, resolved.map(|p| p.id))?;
    if let Some(pid) = bank_posting {
        tx.execute("UPDATE postings SET reconciled_at = ?2 WHERE id = ?1", params![pid, now_ts()])?;
    }
    tx.execute("UPDATE statement_lines SET status = 'created', journal_entry_id = ?2, posting_id = ?3, note = 'by hand' WHERE id = ?1", params![sl.id, entry.id, bank_posting])?;
    tx.commit()?;
    sl.status = "created".into();
    journal::get_entry(conn, entry.id)
}
