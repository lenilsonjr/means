//! Canonical display names are audited metadata; booked text and source evidence stay intact.
use crate::{audit, entities, journal, rules, Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Payee {
    pub id: i64,
    pub uid: String,
    pub entity_id: i64,
    pub name: String,
    pub active: bool,
    pub aliases: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Change {
    pub id: i64,
    pub entity_id: i64,
    pub name: String,
    pub active: bool,
    pub aliases: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Impact {
    pub entry_id: i64,
    pub line_id: i64,
    pub booked: String,
    pub evidence: String,
    pub candidates: Vec<i64>,
    pub before_rule: Option<i64>,
    pub after_rule: Option<i64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preview {
    pub token: String,
    pub applied: bool,
    pub payee: Option<Payee>,
    pub impacts: Vec<Impact>,
    pub warnings: Vec<String>,
    pub backup: Option<String>,
}
pub fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}
pub fn list(conn: &Connection, entity_id: i64) -> Result<Vec<Payee>> {
    entities::get_entity(conn, entity_id)?;
    let mut q = conn.prepare("SELECT id,uid,entity_id,name,active FROM payees WHERE entity_id=?1 ORDER BY id")?;
    let mut rows = q
        .query_map([entity_id], |r| Ok(Payee { id: r.get(0)?, uid: r.get(1)?, entity_id: r.get(2)?, name: r.get(3)?, active: r.get(4)?, aliases: vec![] }))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut aliases = std::collections::HashMap::<i64, Vec<String>>::new();
    let mut q = conn.prepare("SELECT a.payee_id,a.text FROM payee_aliases a JOIN payees p ON p.id=a.payee_id WHERE p.entity_id=?1 ORDER BY a.normalized")?;
    for row in q.query_map([entity_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
        let (id, text) = row?;
        aliases.entry(id).or_default().push(text);
    }
    for p in &mut rows {
        p.aliases = aliases.remove(&p.id).unwrap_or_default();
    }
    Ok(rows)
}
pub fn get(conn: &Connection, id: i64) -> Result<Payee> {
    let entity: Option<i64> = conn.query_row("SELECT entity_id FROM payees WHERE id=?1", [id], |r| r.get(0)).optional()?;
    list(conn, entity.ok_or_else(|| Error::NotFound(format!("payee {id}")))?)?.into_iter().find(|p| p.id == id).ok_or_else(|| Error::NotFound(format!("payee {id}")))
}
pub fn candidates<'a>(payees: &'a [Payee], text: &str) -> Vec<&'a Payee> {
    let text = normalize(text);
    payees.iter().filter(|p| p.active && p.aliases.iter().any(|a| text.contains(&normalize(a)))).collect()
}
pub fn resolve(conn: &Connection, entity: i64, text: &str) -> Result<Option<Payee>> {
    let all = list(conn, entity)?;
    let matches = candidates(&all, text);
    Ok(if matches.len() == 1 { Some(matches[0].clone()) } else { None })
}
/// Explicit text choices resolve only an exact canonical name, never a competing inferred alias.
pub fn explicit(conn: &Connection, entity: i64, text: &str) -> Result<Option<Payee>> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    let all = list(conn, entity)?;
    let matches: Vec<_> = all.into_iter().filter(|p| p.active && normalize(&p.name) == normalize(text)).collect();
    Ok(if matches.len() == 1 { matches.into_iter().next() } else { None })
}
pub(crate) fn link(conn: &Connection, entry: i64, payee: Option<i64>) -> Result<()> {
    let before: Option<i64> = conn.query_row("SELECT payee_id FROM journal_entries WHERE id=?1", [entry], |r| r.get(0))?;
    if before == payee {
        return Ok(());
    }
    conn.execute("UPDATE journal_entries SET payee_id=?2 WHERE id=?1", params![entry, payee])?;
    audit::log(conn, "entry_payees", entry, "link", Some(json!(before)), Some(json!(payee)))
}
fn token(value: serde_json::Value) -> String {
    hex::encode(Sha256::digest(value.to_string().as_bytes()))
}
fn confirm(expected: &str, provided: Option<&str>) -> Result<bool> {
    match provided {
        None => Ok(false),
        Some(s) if s == expected => Ok(true),
        _ => Err(Error::Conflict("payee preview is stale; preview again and confirm its new token".into())),
    }
}
fn impact(conn: &Connection, entity: i64, before: &[Payee], after: &[Payee]) -> Result<Vec<Impact>> {
    let rs = rules::list_rules(conn, Some(entity))?;
    let mut q = conn.prepare("SELECT s.id FROM statement_lines s JOIN accounts a ON a.id=s.account_id WHERE a.entity_id=?1 ORDER BY s.id")?;
    let ids = q.query_map([entity], |r| r.get::<_, i64>(0))?.collect::<std::result::Result<Vec<_>, _>>()?;
    let mut out = vec![];
    for id in ids {
        let sl = crate::imports::get_line(conn, id)?;
        let b = candidates(before, &sl.description);
        let a = candidates(after, &sl.description);
        let bname = if b.len() == 1 { &b[0].name } else { &sl.description };
        let aname = if a.len() == 1 { &a[0].name } else { &sl.description };
        let br = rules::first_match_with_payee(&rs, &sl, bname).map(|r| r.id);
        let ar = rules::first_match_with_payee(&rs, &sl, aname).map(|r| r.id);
        if bname != aname || br != ar || a.len() > 1 {
            out.push(Impact {
                entry_id: sl.journal_entry_id.unwrap_or(0),
                line_id: id,
                booked: sl.journal_entry_id.map(|id| journal::get_entry(conn, id).map(|e| e.payee)).transpose()?.unwrap_or_default(),
                evidence: sl.description,
                candidates: a.iter().map(|p| p.id).collect(),
                before_rule: br,
                after_rule: ar,
            });
        }
    }
    Ok(out)
}
/// Every change is previewed against existing evidence and rules before becoming active.
pub fn save(conn: &mut Connection, change: Change, confirmation: Option<&str>) -> Result<Preview> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let before = list(&tx, change.entity_id)?;
    let old = if change.id != 0 { Some(get(&tx, change.id)?) } else { None };
    if old.as_ref().is_some_and(|p| p.entity_id != change.entity_id) {
        return Err(Error::Invalid("payee cannot move between entities".into()));
    }
    if change.name.trim().is_empty() {
        return Err(Error::Invalid("payee name is required".into()));
    }
    if before.iter().any(|p| p.id != change.id && normalize(&p.name) == normalize(&change.name)) {
        return Err(Error::Conflict("payee name already exists in this entity".into()));
    }
    let mut aliases = std::collections::BTreeMap::new();
    for a in &change.aliases {
        let n = normalize(a);
        if n.is_empty() {
            return Err(Error::Invalid("empty payee alias".into()));
        }
        aliases.insert(n, a.trim().to_string());
    }
    let proposed = Payee {
        id: change.id,
        uid: old.as_ref().map(|p| p.uid.clone()).unwrap_or_default(),
        entity_id: change.entity_id,
        name: change.name.trim().into(),
        active: change.active,
        aliases: aliases.values().cloned().collect(),
    };
    let mut after: Vec<_> = before.iter().filter(|p| p.id != change.id).cloned().collect();
    after.push(proposed.clone());
    let impacts = impact(&tx, change.entity_id, &before, &after)?;
    let mut warnings = vec![];
    for p in &before {
        if p.id != change.id && p.active && proposed.active {
            for a in &p.aliases {
                for b in &proposed.aliases {
                    if normalize(a).contains(&normalize(b)) || normalize(b).contains(&normalize(a)) {
                        warnings.push(format!("Alias overlap with #{} {}: {:?} / {:?}", p.id, p.name, a, b));
                    }
                }
            }
        }
    }
    let linked: i64 = if change.id == 0 { 0 } else { tx.query_row("SELECT count(*) FROM journal_entries WHERE payee_id=?1", [change.id], |r| r.get(0))? };
    warnings.push(format!("{linked} linked entries retain booked text; renames change their display. Other disjoint aliases can still co-occur and will stay ambiguous."));
    let rs = rules::list_rules(&tx, Some(change.entity_id))?;
    let t = token(json!(["save", before, proposed, impacts, warnings, rs]));
    let applied = confirm(&t, confirmation)?;
    let mut result = Preview { token: t, applied, payee: Some(proposed.clone()), impacts, warnings, backup: None };
    if applied && old.as_ref() != Some(&proposed) {
        let id = if change.id == 0 {
            tx.execute("INSERT INTO payees(uid,entity_id,name,active) VALUES(?1,?2,?3,?4)", params![crate::new_uid(), change.entity_id, proposed.name, proposed.active])?;
            tx.last_insert_rowid()
        } else {
            tx.execute("UPDATE payees SET name=?2,active=?3 WHERE id=?1", params![change.id, proposed.name, proposed.active])?;
            change.id
        };
        tx.execute("DELETE FROM payee_aliases WHERE payee_id=?1", [id])?;
        for (n, a) in aliases {
            tx.execute("INSERT INTO payee_aliases VALUES(?1,?2,?3)", params![id, a, n])?;
        }
        let saved = get(&tx, id)?;
        audit::log(&tx, "payees", id, "save", old.map(serde_json::to_value).transpose()?, Some(serde_json::to_value(&saved)?))?;
        result.payee = Some(saved);
        tx.commit()?;
    }
    Ok(result)
}

/// Historical inference uses actual statement evidence, never old category-like booked labels.
/// Caller confirms chart/rule review. A SQLite snapshot is made before the transaction commits.
pub fn backfill(conn: &mut Connection, entity: i64, confirmation: Option<&str>, chart_reviewed: bool) -> Result<Preview> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let all = list(&tx, entity)?;
    let verification = crate::hashchain::verify(&tx, entity)?;
    if verification.first_bad_entry_id.is_some() {
        return Err(Error::Conflict("repair the ledger hash chain before linking payees".into()));
    }
    let mut q = tx.prepare("SELECT id FROM journal_entries WHERE entity_id=?1 AND payee_id IS NULL ORDER BY id")?;
    let ids = q.query_map([entity], |r| r.get::<_, i64>(0))?.collect::<std::result::Result<Vec<_>, _>>()?;
    drop(q);
    let mut impacts = vec![];
    let mut snapshot = vec![];
    for id in ids {
        let e = journal::get_entry(&tx, id)?;
        snapshot.push(serde_json::to_value(&e)?);
        let mut evidence = vec![];
        let mut cs = std::collections::BTreeSet::new();
        let mut source = e.clone();
        let mut visited = std::collections::BTreeSet::new();
        while let Some(original) = source.reverses_id.or(source.refund_of_id) {
            if !visited.insert(source.id) {
                return Err(Error::Conflict("cyclic reversal/refund history".into()));
            }
            source = journal::get_entry(&tx, original)?;
            snapshot.push(serde_json::to_value(&source)?);
            if source.entity_id != entity {
                return Err(Error::Conflict("reversal/refund belongs to another entity".into()));
            }
        }
        if let Some(payee) = source.payee_id.filter(|_| source.id != id) {
            cs.insert(payee);
            evidence.push((0, format!("Original entry #{} canonical payee", source.id)));
        } else {
            let mut q = tx.prepare("SELECT id,description FROM statement_lines WHERE journal_entry_id=?1 AND status IN ('created','matched') ORDER BY id")?;
            evidence = q.query_map([source.id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?.collect::<std::result::Result<Vec<_>, _>>()?;
            for (_, text) in &evidence {
                for p in candidates(&all, text) {
                    cs.insert(p.id);
                }
            }
            if source.id != id {
                evidence.insert(0, (0, format!("Original entry #{}", source.id)));
            }
        }
        impacts.push(Impact {
            entry_id: id,
            line_id: evidence.first().map(|r| r.0).unwrap_or(0),
            booked: e.payee,
            evidence: evidence.iter().map(|r| r.1.as_str()).collect::<Vec<_>>().join(" | "),
            candidates: cs.into_iter().collect(),
            before_rule: None,
            after_rule: None,
        });
    }
    let t = token(json!(["backfill", entity, all, impacts, snapshot, verification, rules::list_rules(&tx, Some(entity))?]));
    let applied = confirm(&t, confirmation)?;
    let mut report = Preview {
        token: t,
        applied,
        payee: None,
        impacts,
        warnings: vec![
            "Confirm chart/classes, per-payee splits and ongoing rules are reviewed. Only unambiguous statement-backed links are applied; unmatched/ambiguous entries remain unresolved.".into()
        ],
        backup: None,
    };
    if applied {
        if !chart_reviewed {
            return Err(Error::Invalid("historical linking requires explicit chart and rule review confirmation".into()));
        }
        // The read-only connection sees the committed pre-apply ledger. The immediate writer
        // transaction prevents another writer racing the preview and snapshot.
        let path: String = tx.query_row("SELECT file FROM pragma_database_list WHERE name='main'", [], |r| r.get(0))?;
        if !path.is_empty() {
            let backup = format!("{path}.payees-{}.sqlite", crate::new_uid());
            let source = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            drop(options.open(&backup).map_err(anyhow::Error::from)?);
            if let Err(error) = source.execute("VACUUM INTO ?1", [&backup]) {
                let _ = std::fs::remove_file(&backup);
                return Err(error.into());
            }
            report.backup = Some(backup);
        }
        for row in &report.impacts {
            if row.candidates.len() == 1 {
                link(&tx, row.entry_id, Some(row.candidates[0]))?;
            }
        }
        tx.commit()?;
    }
    Ok(report)
}

#[derive(Debug, Serialize)]
pub struct ExpenseRow {
    pub payee_id: Option<i64>,
    pub name: String,
    pub amount: crate::money::Money,
}
#[derive(Debug, Serialize)]
pub struct ExpenseReport {
    pub rows: Vec<ExpenseRow>,
    pub total: crate::money::Money,
}
pub fn expenses(conn: &Connection, entity: i64, from: Option<chrono::NaiveDate>, to: Option<chrono::NaiveDate>, tag: Option<&str>) -> Result<ExpenseReport> {
    let base = crate::reports::expenses_by_class(conn, entity, from, to, tag)?;
    let filter = base.tag.as_deref().map(|s| s.split_once(':').unwrap_or((s, "")));
    let mut q=conn.prepare("SELECT e.payee_id, COALESCE(c.name,e.payee), SUM(p.amount) FROM postings p JOIN journal_entries e ON e.id=p.journal_entry_id JOIN accounts a ON a.id=p.account_id LEFT JOIN payees c ON c.id=e.payee_id WHERE e.entity_id=?1 AND a.entity_id=?1 AND a.type='expense' AND e.status IN ('posted','void') AND (?2 IS NULL OR e.date>=?2) AND (?3 IS NULL OR e.date<=?3) AND (?4 IS NULL OR EXISTS(SELECT 1 FROM entry_tags t WHERE t.entry_id=e.id AND t.key=?4 AND t.value=?5)) GROUP BY e.payee_id,COALESCE(c.name,e.payee) ORDER BY 2,1")?;
    let values = q
        .query_map(params![entity, from.map(|d| d.to_string()), to.map(|d| d.to_string()), filter.map(|f| f.0), filter.map(|f| f.1)], |r| {
            Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let rows = values
        .into_iter()
        .map(|(payee_id, name, minor)| Ok(ExpenseRow { payee_id, name, amount: crate::money::Money::from_minor(minor, base.total.commodity(), base.total.precision())? }))
        .collect::<Result<Vec<_>>>()?;
    Ok(ExpenseReport { rows, total: base.total })
}

/// Explicit reassignment/merge is previewed and audited without rewriting booked fields.
pub fn reassign(conn: &mut Connection, source: i64, target: i64, entry_id: Option<i64>, confirmation: Option<&str>) -> Result<Preview> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let to = get(&tx, target)?;
    if !to.active {
        return Err(Error::Invalid("choose an active payee".into()));
    }
    let (ids, old) = if let Some(entry) = entry_id {
        let e = journal::get_entry(&tx, entry)?;
        if e.entity_id != to.entity_id {
            return Err(Error::Invalid("payee belongs to another entity".into()));
        }
        (vec![entry], None)
    } else {
        let old = get(&tx, source)?;
        if old.entity_id != to.entity_id || old.id == target {
            return Err(Error::Invalid("merge requires two payees in the same entity".into()));
        }
        let mut q = tx.prepare("SELECT id FROM journal_entries WHERE payee_id=?1 ORDER BY id")?;
        let ids = q.query_map([source], |r| r.get::<_, i64>(0))?.collect::<std::result::Result<Vec<_>, _>>()?;
        (ids, Some(old))
    };
    let entries = ids.iter().map(|id| journal::get_entry(&tx, *id)).collect::<Result<Vec<_>>>()?;
    let mut impacts = entries
        .iter()
        .map(|e| Impact { entry_id: e.id, line_id: 0, booked: e.payee.clone(), evidence: e.description.clone(), candidates: vec![target], before_rule: None, after_rule: None })
        .collect::<Vec<_>>();
    let before = list(&tx, to.entity_id)?;
    let mut after = before.clone();
    let mut merged = to.clone();
    let mut warnings = vec!["Explicit canonical reassignment preserves booked text and amounts.".into()];
    if let Some(source) = &old {
        let mut aliases = std::collections::BTreeMap::new();
        for a in source.aliases.iter().chain(to.aliases.iter()) {
            aliases.insert(normalize(a), a.clone());
        }
        merged.aliases = aliases.into_values().collect();
        for p in &mut after {
            if p.id == source.id {
                p.active = false;
            }
            if p.id == target {
                *p = merged.clone();
            }
        }
        impacts.extend(impact(&tx, to.entity_id, &before, &after)?);
        warnings.push(format!("Archive source #{} and transfer {} aliases to #{} ({} distinct aliases after merging).", source.id, source.aliases.len(), target, merged.aliases.len()));
        for p in after.iter().filter(|p| p.active && p.id != target) {
            for a in &p.aliases {
                for b in &merged.aliases {
                    if normalize(a).contains(&normalize(b)) || normalize(b).contains(&normalize(a)) {
                        warnings.push(format!("Alias overlap with #{} {}: {:?} / {:?}", p.id, p.name, a, b));
                    }
                }
            }
        }
        warnings
            .push("Disjoint aliases can also co-occur. Conflicting matches remain unresolved. Explicit rule payee actions keep their booked text; review actions naming the archived source.".into());
    }
    let t = token(json!(["reassign", old, to, entries, before, impacts, warnings, rules::list_rules(&tx, Some(to.entity_id))?]));
    let applied = confirm(&t, confirmation)?;
    let report = Preview { token: t, applied, payee: Some(merged.clone()), impacts, warnings, backup: None };
    if applied {
        for id in ids {
            link(&tx, id, Some(target))?;
        }
        if let Some(old) = old {
            for alias in &merged.aliases {
                tx.execute("INSERT INTO payee_aliases(payee_id,text,normalized) VALUES(?1,?2,?3) ON CONFLICT(payee_id,normalized) DO NOTHING", params![target, alias, normalize(alias)])?;
            }
            audit::log(&tx, "payees", target, "merge_aliases", Some(serde_json::to_value(&to)?), Some(serde_json::to_value(&merged)?))?;
            tx.execute("UPDATE payees SET active=0 WHERE id=?1", [old.id])?;
            audit::log(&tx, "payees", old.id, "merge", Some(serde_json::to_value(&old)?), Some(json!({"target":target,"active":false})))?;
        }
        tx.commit()?;
    }
    Ok(report)
}
