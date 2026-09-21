//! A per-entity hash chain over posted journal entries.
//!
//! Every posted entry gets a sequence number and `hash = sha256(prev_hash || canonical(entry))`.
//! `verify` checks internal consistency. Comparing against a separately trusted head can
//! detect changes to the accounting content committed there; a head supplied by the same
//! untrusted database cannot independently establish that history was preserved. The chain
//! does not cover all metadata. See docs/vault-sharing.md for the sharing trust boundary.

use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::model::JournalEntry;
use crate::Result;

/// The canonical text of an entry: everything that is accounting content, nothing that is workflow.
pub fn canonical(entry: &JournalEntry, account_uids: &dyn Fn(i64) -> String) -> String {
    let mut postings: Vec<serde_json::Value> = entry
        .postings
        .iter()
        .map(|p| {
            serde_json::json!({
                "account": account_uids(p.account_id),
                "quantity": p.quantity.major().normalize().to_string(),
                "amount": p.amount.major().normalize().to_string(),
                "commodity": p.quantity.commodity(),
            })
        })
        .collect();
    postings.sort_by_key(|v| v.to_string());
    serde_json::json!({
        "uid": entry.uid,
        "date": entry.date.to_string(),
        "payee": entry.payee,
        "description": entry.description,
        "postings": postings,
    })
    .to_string()
}

pub fn digest(prev: &str, canonical: &str) -> String {
    let mut h = Sha256::new();
    h.update(prev.as_bytes());
    h.update(b"\n");
    h.update(canonical.as_bytes());
    hex::encode(h.finalize())
}

fn account_uid_lookup(conn: &Connection) -> impl Fn(i64) -> String + '_ {
    move |id| conn.query_row("SELECT uid FROM accounts WHERE id = ?1", [id], |r| r.get::<_, String>(0)).unwrap_or_default()
}

/// Append a freshly posted entry to its entity's chain. Returns (seq, hash).
pub fn append(conn: &Connection, entry: &JournalEntry) -> Result<(i64, String)> {
    let last: Option<(i64, String)> =
        conn.query_row("SELECT seq, hash FROM journal_entries WHERE entity_id = ?1 AND seq IS NOT NULL ORDER BY seq DESC LIMIT 1", [entry.entity_id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
    let (seq, prev) = match last {
        Some((s, h)) => (s + 1, h),
        None => (1, String::new()),
    };
    let text = canonical(entry, &account_uid_lookup(conn));
    let hash = digest(&prev, &text);
    conn.execute("UPDATE journal_entries SET seq = ?2, prev_hash = ?3, hash = ?4 WHERE id = ?1", params![entry.id, seq, prev, hash])?;
    Ok((seq, hash))
}

/// Recompute hashes from `from_seq` onward (after an allowed edit of a posted entry).
pub fn rechain(conn: &Connection, entity_id: i64, from_seq: i64) -> Result<usize> {
    let ids: Vec<(i64, i64)> = {
        let mut stmt = conn.prepare("SELECT id, seq FROM journal_entries WHERE entity_id = ?1 AND seq IS NOT NULL AND seq >= ?2 ORDER BY seq")?;
        let v: Vec<(i64, i64)> = stmt.query_map(params![entity_id, from_seq], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<std::result::Result<_, _>>()?;
        v
    };
    let mut prev: String = conn
        .query_row("SELECT hash FROM journal_entries WHERE entity_id = ?1 AND seq IS NOT NULL AND seq < ?2 ORDER BY seq DESC LIMIT 1", params![entity_id, from_seq], |r| r.get(0))
        .optional()?
        .unwrap_or_default();
    let lookup = account_uid_lookup(conn);
    let mut n = 0;
    for (id, _seq) in ids {
        let entry = crate::journal::get_entry(conn, id)?;
        let hash = digest(&prev, &canonical(&entry, &lookup));
        conn.execute("UPDATE journal_entries SET prev_hash = ?2, hash = ?3 WHERE id = ?1", params![id, prev, hash])?;
        prev = hash;
        n += 1;
    }
    Ok(n)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct VerifyReport {
    pub entity_id: i64,
    pub checked: usize,
    pub head: Option<String>,
    pub first_bad_seq: Option<i64>,
    pub first_bad_entry_id: Option<i64>,
}

pub fn verify(conn: &Connection, entity_id: i64) -> Result<VerifyReport> {
    let ids: Vec<(i64, i64, String, String)> = {
        let mut stmt = conn.prepare("SELECT id, seq, prev_hash, hash FROM journal_entries WHERE entity_id = ?1 AND seq IS NOT NULL ORDER BY seq")?;
        let v: Vec<(i64, i64, String, String)> = stmt
            .query_map([entity_id], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, Option<String>>(2)?.unwrap_or_default(), r.get::<_, Option<String>>(3)?.unwrap_or_default())))?
            .collect::<std::result::Result<_, _>>()?;
        v
    };
    let lookup = account_uid_lookup(conn);
    let mut prev = String::new();
    let mut report = VerifyReport { entity_id, checked: 0, head: None, first_bad_seq: None, first_bad_entry_id: None };
    for (id, seq, stored_prev, stored_hash) in ids {
        let entry = crate::journal::get_entry(conn, id)?;
        let expected = digest(&prev, &canonical(&entry, &lookup));
        if stored_prev != prev || stored_hash != expected {
            report.first_bad_seq = Some(seq);
            report.first_bad_entry_id = Some(id);
            break;
        }
        prev = expected;
        report.checked += 1;
        report.head = Some(prev.clone());
    }
    Ok(report)
}
