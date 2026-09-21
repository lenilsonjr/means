//! Explicit reconciliation of overlapping bank sources. Normal matching stays unchanged.

use std::collections::HashMap;

use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::json;

use crate::{accounts, audit, entities, imports, journal, EntryStatus, Error, JournalEntry, Result, StatementLine};

#[derive(Debug, Serialize)]
pub struct Match {
    pub line_id: i64,
    pub date: String,
    pub amount: String,
    pub currency: String,
    pub description: String,
    pub posting_id: i64,
    pub kept_entry_id: i64,
    pub kept_payee: String,
    pub kept_date: String,
    pub days_apart: i64,
    pub removed_draft_id: Option<i64>,
    pub voided_entry_id: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub import_id: i64,
    pub source: String,
    pub matches: Vec<Match>,
    pub notes: Vec<String>,
    pub token: String,
    pub applied: bool,
}

/// Only machine-created entries belonging to this line may be retired. Rule categories
/// and initial tags are allowed; a subsequent human edit or another dependency is not.
fn disposable_entry(conn: &Connection, line: &StatementLine) -> Result<Option<JournalEntry>> {
    let Some(id) = line.journal_entry_id else { return Ok(None) };
    let own = journal::get_entry(conn, id)?;
    let rule_posted = own.status == EntryStatus::Posted && own.origin == "rule" && line.note.starts_with("rule: ");
    let import_draft = own.status == EntryStatus::Draft && own.origin == "import" && line.note.is_empty() && own.notes.is_empty() && own.tags.is_empty() && own.postings.len() == 2;
    if line.status != "created"
        || Some(own.date) != line.date
        || (!rule_posted && !import_draft)
        || own.reviewed_at.is_some()
        || own.reverses_id.is_some()
        || own.reversed_by_id.is_some()
        || own.refund_of_id.is_some()
        || own.counterpart_id.is_some()
    {
        return Err(Error::Conflict(format!("entry #{id} is not an untouched import draft or unreviewed rule-posted entry; review it manually")));
    }
    let history = audit::history(conn, "journal_entries", id)?;
    // Imports log completion after creating their entries. This also recognizes initial
    // rule tags in older ledgers without treating later tag edits as machine decisions.
    let completion: Option<i64> = conn.query_row("SELECT MIN(id) FROM audit_log WHERE table_name='imports' AND row_id=?1 AND action='create'", [line.import_id], |r| r.get(0))?;
    let created = history.iter().find(|a| a.action == "create");
    let initial_rule = created.is_some_and(|a| a.after.as_deref().and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()).is_some_and(|v| v["origin"] == "rule" && v["status"] == "posted"));
    let initial_tags = history.iter().filter(|a| a.action == "tags").count() <= 1;
    let clean_history = created.is_some()
        && history
            .iter()
            .all(|a| a.action == "create" || (rule_posted && initial_rule && initial_tags && a.action == "tags" && a.before.as_deref() == Some("[]") && completion.is_some_and(|end| a.id < end)));
    let payee_edits = audit::history(conn, "entry_payees", id)?.iter().any(|a| completion.is_none_or(|end| a.id > end));
    let evidence: i64 = conn.query_row("SELECT COUNT(*) FROM statement_lines WHERE journal_entry_id=?1 AND id<>?2", params![id, line.id], |r| r.get(0))?;
    let dependents: i64 = conn.query_row("SELECT COUNT(*) FROM journal_entries WHERE refund_of_id=?1 OR counterpart_id=?1 OR reverses_id=?1", [id], |r| r.get(0))?;
    let bank = own.postings.iter().find(|p| Some(p.id) == line.posting_id && Some(p.account_id) == line.account_id && Some(p.quantity) == line.amount);
    let contra = own.postings.iter().find(|p| Some(p.account_id) != line.account_id);
    let suspense = contra.map(|p| accounts::get_account(conn, p.account_id)).transpose()?.is_some_and(|a| a.system_role == "suspense");
    if !clean_history
        || (rule_posted && (!initial_rule || payee_edits))
        || evidence != 0
        || dependents != 0
        || bank.is_none()
        || (import_draft && !suspense)
        || own.postings.iter().any(|p| Some(p.id) != line.posting_id && p.reconciled_at.is_some())
    {
        return Err(Error::Conflict(format!("entry #{id} has edits or other evidence/dependencies; review it manually")));
    }
    journal::check_lock(&entities::get_entity(conn, own.entity_id)?, own.date).map_err(|e| Error::Conflict(e.to_string()))?;
    for p in &own.postings {
        if accounts::get_account(conn, p.account_id)?.closed_at.is_some() {
            return Err(Error::Conflict(format!("entry #{id} uses a closed account; review it manually")));
        }
    }
    Ok(Some(own))
}

/// Preview returns a token for this exact plan and accounting state. Applying requires that token.
/// Ambiguity on either side is refused. The whole application is atomic.
pub fn rematch(conn: &mut Connection, import_id: i64, confirmation: Option<&str>) -> Result<Report> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let (import, lines) = imports::get_import(&tx, import_id)?;
    if import.source == "account_tracker" {
        return Err(Error::Invalid("cross-source reconciliation requires a bank import".into()));
    }
    let mut report = Report { import_id, source: import.source.clone(), matches: vec![], notes: vec![], token: String::new(), applied: false };
    let mut evidence_snapshot = vec![];
    for line in &lines {
        if !matches!(line.status.as_str(), "created" | "unmatched" | "error") {
            continue;
        }
        let (Some(account_id), Some(amount), Some(date)) = (line.account_id, line.amount, line.date) else { continue };
        let account = accounts::get_account(&tx, account_id)?;
        if line.currency != account.commodity || amount.commodity() != account.commodity || amount.precision() != account.precision {
            report.notes.push(format!("line #{}: currency does not agree with its bank account", line.id));
            continue;
        }
        let own = match disposable_entry(&tx, line) {
            Ok(own) => own,
            Err(Error::Conflict(reason)) => {
                report.notes.push(format!("line #{}: {reason}", line.id));
                continue;
            }
            Err(e) => return Err(e),
        };
        if own.is_none() && line.posting_id.is_some() {
            continue;
        }
        let candidates: Vec<(i64, i64)> = {
            let mut query = tx.prepare(
                "SELECT p.id, e.id FROM postings p JOIN journal_entries e ON e.id=p.journal_entry_id
                 WHERE p.account_id=?1 AND p.quantity=?2 AND ABS(julianday(e.date)-julianday(?3))<=?6 AND e.status='posted'
                 AND EXISTS (SELECT 1 FROM statement_lines s JOIN imports i ON i.id=s.import_id
                    WHERE s.posting_id=p.id AND s.status IN ('matched','created') AND i.source<>?4 AND i.source<>'account_tracker'
                    AND s.account_id=?1 AND s.amount=?2 AND ABS(julianday(s.date)-julianday(?3))<=?6 AND s.currency=?5)
                 AND NOT EXISTS (SELECT 1 FROM statement_lines s JOIN imports i ON i.id=s.import_id WHERE s.posting_id=p.id AND i.source=?4)
                 ORDER BY p.id",
            )?;
            let rows = query
                .query_map(params![account_id, amount.minor(), date.to_string(), import.source, account.commodity, imports::MATCH_WINDOW_DAYS], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<std::result::Result<_, _>>()?;
            rows
        };
        if candidates.len() != 1 {
            report.notes.push(format!("line #{}: {} cross-source candidates within five days; left unchanged", line.id, candidates.len()));
            continue;
        }
        let (posting_id, kept_entry_id) = candidates[0];
        let kept = journal::get_entry(&tx, kept_entry_id)?;
        if own.as_ref().is_some_and(|entry| entry.id <= kept.id) {
            report.notes.push(format!("line #{}: target entry is not older than the duplicate; left unchanged", line.id));
            continue;
        }
        if let Err(e) = journal::check_lock(&entities::get_entity(&tx, kept.entity_id)?, kept.date) {
            report.notes.push(format!("line #{}: {e}", line.id));
            continue;
        }
        let evidence_ids: Vec<i64> = {
            let mut stmt = tx.prepare("SELECT id FROM statement_lines WHERE posting_id=?1 ORDER BY id")?;
            let rows = stmt.query_map([posting_id], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
            rows
        };
        let evidence = evidence_ids.into_iter().map(|id| imports::get_line(&tx, id)).collect::<Result<Vec<_>>>()?;
        evidence_snapshot.push(json!({"own":own,"kept":kept,"evidence":evidence}));
        report.matches.push(Match {
            line_id: line.id,
            date: date.to_string(),
            amount: amount.major().to_string(),
            currency: account.commodity,
            description: line.description.clone(),
            posting_id,
            kept_entry_id,
            kept_payee: kept.payee,
            kept_date: kept.date.to_string(),
            days_apart: (kept.date - date).num_days().abs(),
            removed_draft_id: own.as_ref().filter(|e| e.status == EntryStatus::Draft).map(|e| e.id),
            voided_entry_id: own.as_ref().filter(|e| e.status == EntryStatus::Posted).map(|e| e.id),
        });
    }
    let mut claims = HashMap::new();
    for m in &report.matches {
        *claims.entry(m.posting_id).or_insert(0) += 1;
    }
    report.matches.retain(|m| {
        if claims[&m.posting_id] != 1 {
            report.notes.push(format!("line #{}: several incoming lines claim posting #{}; left unchanged", m.line_id, m.posting_id));
            false
        } else {
            true
        }
    });
    report.token = imports::checksum(&serde_json::to_vec(&json!({"import":import,"lines":lines,"matches":report.matches,"notes":report.notes,"evidence":evidence_snapshot}))?);
    if let Some(token) = confirmation {
        if token != report.token {
            return Err(Error::Conflict("the cross-source preview is stale or its token is incorrect; preview again".into()));
        }
        for m in &report.matches {
            let before = imports::get_line(&tx, m.line_id)?;
            if let Some(id) = m.removed_draft_id {
                let own = journal::get_entry(&tx, id)?;
                tx.execute("UPDATE statement_lines SET posting_id=NULL, journal_entry_id=NULL WHERE id=?1", [m.line_id])?;
                tx.execute("DELETE FROM journal_entries WHERE id=?1", [id])?;
                audit::log(&tx, "journal_entries", id, "delete", Some(serde_json::to_value(&own)?), Some(json!({"cross_source_duplicate_of":m.kept_entry_id,"line":m.line_id})))?;
            }
            if let Some(id) = m.voided_entry_id {
                journal::void_entry_in_transaction(&tx, id, None, &format!("Confirmed cross-source duplicate of #{} (line #{})", m.kept_entry_id, m.line_id))?;
            }
            // The target posting, valuation, categories, review state and all prior evidence stay intact.
            tx.execute(
                "UPDATE statement_lines SET posting_id=?2, journal_entry_id=?3, status='matched', note='confirmed cross-source match' WHERE id=?1",
                params![m.line_id, m.posting_id, m.kept_entry_id],
            )?;
            audit::log(&tx, "statement_lines", m.line_id, "cross_source_match", Some(serde_json::to_value(before)?), Some(serde_json::to_value(imports::get_line(&tx, m.line_id)?)?))?;
        }
        tx.execute(
            "UPDATE imports SET
            created_count=(SELECT COUNT(*) FROM statement_lines WHERE import_id=?1 AND status='created'),
            matched_count=(SELECT COUNT(*) FROM statement_lines WHERE import_id=?1 AND status='matched'),
            unmatched_count=(SELECT COUNT(*) FROM statement_lines WHERE import_id=?1 AND status='unmatched'),
            error_count=(SELECT COUNT(*) FROM statement_lines WHERE import_id=?1 AND status='error') WHERE id=?1",
            [import_id],
        )?;
        report.applied = true;
        audit::log(&tx, "imports", import_id, "cross_source_rematch", None, Some(serde_json::to_value(&report)?))?;
        tx.commit()?;
    }
    Ok(report)
}
