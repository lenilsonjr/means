//! Tags: key:value lenses on journal entries — city:lisbon, trip:alentejo-2026, with:friends.
//!
//! Tags are context, not accounting facts: they live outside the hash chain on purpose, so
//! retagging history never rewrites it. Amounts, dates and accounts stay tamper-evident.

use std::collections::HashMap;

use rusqlite::{params, Connection};

use crate::{Error, Result};

/// "city:lisbon trip:alentejo-2026 reviewed" → normalized (key, value) pairs. A bare word is a
/// key with an empty value; a leading # is dropped. Lowercase, deduped, order kept.
pub fn parse(spec: &str) -> Result<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> = Vec::new();
    for token in spec.split_whitespace() {
        let token = token.trim_start_matches('#').to_lowercase();
        if token.is_empty() {
            continue;
        }
        let (k, v) = match token.split_once(':') {
            Some((k, v)) => (k.trim().to_string(), v.trim().to_string()),
            None => (token.clone(), String::new()),
        };
        if k.is_empty() {
            return Err(Error::Invalid(format!("tag {token:?} has no key")));
        }
        if !out.iter().any(|(ok, ov)| *ok == k && *ov == v) {
            out.push((k, v));
        }
    }
    Ok(out)
}

fn joined(k: &str, v: &str) -> String {
    if v.is_empty() {
        k.to_string()
    } else {
        format!("{k}:{v}")
    }
}

/// Replace the entry's tags. No rechain — only the audit log records the change.
pub fn set_tags(conn: &mut Connection, entry_id: i64, tags: &[(String, String)]) -> Result<Vec<String>> {
    let tx = conn.transaction()?;
    let result = set_tags_in_transaction(&tx, entry_id, tags)?;
    tx.commit()?;
    Ok(result)
}

pub(crate) fn set_tags_in_transaction(conn: &rusqlite::Transaction<'_>, entry_id: i64, tags: &[(String, String)]) -> Result<Vec<String>> {
    let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM journal_entries WHERE id = ?1)", [entry_id], |r| r.get(0))?;
    if !exists {
        return Err(Error::NotFound(format!("entry {entry_id}")));
    }
    let before = strings_for(conn, entry_id)?;
    let after: Vec<String> = tags.iter().map(|(k, v)| joined(k, v)).collect();
    conn.execute("DELETE FROM entry_tags WHERE entry_id = ?1", [entry_id])?;
    for (k, v) in tags {
        conn.execute("INSERT OR IGNORE INTO entry_tags (entry_id, key, value) VALUES (?1, ?2, ?3)", params![entry_id, k, v])?;
    }
    if before != after {
        crate::audit::log(conn, "journal_entries", entry_id, "tags", Some(serde_json::json!(before)), Some(serde_json::json!(after)))?;
    }
    strings_for(conn, entry_id)
}

/// The entry's tags as "key:value" strings (a bare key when the value is empty).
pub fn strings_for(conn: &Connection, entry_id: i64) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT key, value FROM entry_tags WHERE entry_id = ?1 ORDER BY key, value")?;
    let rows = stmt.query_map([entry_id], |r| Ok(joined(&r.get::<_, String>(0)?, &r.get::<_, String>(1)?)))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Repair legacy void reversals during migration, without changing accounting hashes.
pub(crate) fn repair_reversal_tags(conn: &Connection) -> Result<()> {
    // Called inside the migration transaction. Never overwrite a user's tag edit,
    // including a deliberate removal that leaves the reversal untagged.
    let mut stmt = conn.prepare(
        "SELECT r.id, r.reverses_id FROM journal_entries r
         JOIN journal_entries original ON original.id = r.reverses_id
         WHERE r.origin = 'system' AND original.status = 'void'
           AND NOT EXISTS (SELECT 1 FROM entry_tags t WHERE t.entry_id = r.id)
           AND EXISTS (SELECT 1 FROM entry_tags t WHERE t.entry_id = original.id)
           AND NOT EXISTS (SELECT 1 FROM audit_log a
                           WHERE a.table_name = 'journal_entries' AND a.row_id = r.id AND a.action = 'tags')",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?.collect::<std::result::Result<Vec<_>, _>>()?;
    for (reversal, original) in rows {
        conn.execute("INSERT INTO entry_tags (entry_id, key, value) SELECT ?1, key, value FROM entry_tags WHERE entry_id = ?2", params![reversal, original])?;
        crate::audit::log(
            conn,
            "journal_entries",
            reversal,
            "repair_reversal_tags",
            Some(serde_json::json!([])),
            Some(serde_json::json!({"original_entry_id": original, "tags": strings_for(conn, reversal)?, "migration": 12})),
        )?;
    }
    Ok(())
}

/// Finish legacy reversal chains, including databases that already ran migration 12.
pub(crate) fn repair_reversal_chains(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT r.id, r.reverses_id FROM journal_entries r
         JOIN journal_entries original ON original.id = r.reverses_id
         WHERE r.origin = 'system' AND original.status = 'void'
           AND NOT EXISTS (SELECT 1 FROM entry_tags t WHERE t.entry_id = r.id)
           AND NOT EXISTS (SELECT 1 FROM audit_log a
                           WHERE a.table_name = 'journal_entries' AND a.row_id = r.id AND a.action = 'tags')",
    )?;
    let mut children = std::collections::BTreeMap::<i64, Vec<i64>>::new();
    for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
        let (reversal, original) = row?;
        children.entry(original).or_default().push(reversal);
    }
    let mut ready = std::collections::VecDeque::new();
    for original in children.keys() {
        if !strings_for(conn, *original)?.is_empty() {
            ready.push_back(*original);
        }
    }
    while let Some(original) = ready.pop_front() {
        for reversal in children.remove(&original).unwrap_or_default() {
            conn.execute("INSERT INTO entry_tags (entry_id, key, value) SELECT ?1, key, value FROM entry_tags WHERE entry_id = ?2", params![reversal, original])?;
            crate::audit::log(
                conn,
                "journal_entries",
                reversal,
                "repair_reversal_tags",
                Some(serde_json::json!([])),
                Some(serde_json::json!({"original_entry_id": original, "tags": strings_for(conn, reversal)?, "migration": 13})),
            )?;
            ready.push_back(reversal);
        }
    }
    Ok(())
}

/// Tags of many entries at once, for lists.
pub fn for_entries(conn: &Connection, ids: &[i64]) -> Result<HashMap<i64, Vec<String>>> {
    let mut map: HashMap<i64, Vec<String>> = HashMap::new();
    if ids.is_empty() {
        return Ok(map);
    }
    let placeholders = vec!["?"; ids.len()].join(",");
    let sql = format!("SELECT entry_id, key, value FROM entry_tags WHERE entry_id IN ({placeholders}) ORDER BY key, value");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
    for row in rows {
        let (id, k, v) = row?;
        map.entry(id).or_default().push(joined(&k, &v));
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_normalizes() {
        let t = parse("City:Lisbon  trip:Alentejo-2026 #reviewed city:lisbon").unwrap();
        assert_eq!(t, vec![("city".into(), "lisbon".into()), ("trip".into(), "alentejo-2026".into()), ("reviewed".into(), String::new())]);
        assert!(parse(":oops").is_err());
    }
}
