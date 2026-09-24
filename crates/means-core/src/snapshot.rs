//! A complete, hashed and atomic read of the books for external tools.
//!
//! `export` reads every selected record in one read transaction. It writes the files into a
//! temporary directory next to the destination, syncs them, writes the manifest last and then
//! renames the directory into place. `verify` hashes every file again and checks the manifest.
//! See docs/snapshot-format.md for the contract.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::money::Money;
use crate::{accounts, documents, export, new_uid, now_ts, policy, Error, Result};

/// The version of the snapshot format. A reader refuses other versions.
pub const SCHEMA_VERSION: i64 = 1;

/// The name of the manifest file. The manifest does not list itself.
pub const MANIFEST: &str = "manifest.json";

/// The names of the counts in the manifest, in contract order.
pub const COUNT_KEYS: &[&str] =
    &["entities", "accounts", "entries", "postings", "drafts", "parties", "documents", "statement_lines", "tax_codes", "tax_events", "invoices", "settlements", "filings", "intervault_links"];

/// What to export.
#[derive(Debug, Clone, Default)]
pub struct SnapshotOptions {
    /// Entity uids. An empty list selects every entity.
    pub entity_uids: Vec<String>,
    /// The last day of the export. `None` exports every date.
    pub as_of: Option<NaiveDate>,
}

/// A file of the snapshot and its content.
struct Payload {
    name: String,
    bytes: Vec<u8>,
}

/// A stored document file to copy into the snapshot.
struct DocumentFile {
    /// The path in the snapshot, for example `documents/ab/ab12...`.
    name: String,
    source: PathBuf,
    sha256: String,
}

/// Everything that the read transaction gives, before any file is written.
struct Collected {
    payloads: Vec<Payload>,
    documents: Vec<DocumentFile>,
    entity_uids: Vec<String>,
    posting_policy: String,
    counts: BTreeMap<String, i64>,
    chain_heads: Map<String, Value>,
    control_totals: Map<String, Value>,
}

fn money(minor: i64, commodity: &str, precision: u32) -> Result<Value> {
    Ok(serde_json::to_value(Money::from_minor(minor, commodity, precision)?)?)
}

fn json_text(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| json!({}))
}

fn uid_of(map: &HashMap<i64, String>, id: Option<i64>) -> Value {
    id.and_then(|id| map.get(&id)).map(|uid| Value::String(uid.clone())).unwrap_or(Value::Null)
}

fn uid_map(tx: &Connection, table: &str) -> Result<HashMap<i64, String>> {
    let mut stmt = tx.prepare(&format!("SELECT id, uid FROM {table}"))?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn table_exists(tx: &Connection, name: &str) -> Result<bool> {
    Ok(tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)", [name], |r| r.get(0))?)
}

fn precision_of(precisions: &HashMap<String, u32>, code: &str) -> Result<u32> {
    precisions.get(code).copied().ok_or_else(|| Error::Invalid(format!("unknown commodity {code:?}")))
}

fn array_file(name: &str, rows: &[Value]) -> Result<Payload> {
    let mut bytes = serde_json::to_vec_pretty(rows)?;
    bytes.push(b'\n');
    Ok(Payload { name: name.into(), bytes })
}

fn lines_file(name: &str, rows: &[Value]) -> Result<Payload> {
    let mut bytes = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut bytes, row)?;
        bytes.push(b'\n');
    }
    Ok(Payload { name: name.into(), bytes })
}

/// The latest non-empty actor of each `confirm` audit event of an entry.
fn reviewers(tx: &Connection) -> Result<HashMap<i64, String>> {
    let has_actor: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('audit_log') WHERE name = 'actor')", [], |r| r.get(0))?;
    let mut out = HashMap::new();
    if !has_actor {
        return Ok(out);
    }
    let mut stmt = tx.prepare("SELECT row_id, actor FROM audit_log WHERE table_name = 'journal_entries' AND action = 'confirm' AND actor <> '' ORDER BY id")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (id, actor) = row?;
        out.insert(id, actor);
    }
    Ok(out)
}

#[derive(Clone)]
struct EntryRow {
    id: i64,
    entity_id: i64,
    status: String,
    value: Map<String, Value>,
}

fn collect(tx: &Connection, options: &SnapshotOptions) -> Result<Collected> {
    let as_of = options.as_of.map(|d| d.to_string());
    let before_or_on = |date: Option<&str>| match (&as_of, date) {
        (None, _) => true,
        (Some(limit), Some(date)) => date.get(..10).unwrap_or(date) <= limit.as_str(),
        (Some(_), None) => false,
    };
    let scope_date = options.as_of.unwrap_or_else(crate::today).to_string();

    let precisions: HashMap<String, u32> = {
        let mut stmt = tx.prepare("SELECT code, precision FROM commodities")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?)))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let entity_uids = uid_map(tx, "entities")?;
    let account_uids = uid_map(tx, "accounts")?;
    let entry_uids = uid_map(tx, "journal_entries")?;
    let posting_uids = uid_map(tx, "postings")?;
    let payee_uids = uid_map(tx, "payees")?;
    let document_uids = uid_map(tx, "documents")?;
    let tax_code_uids = uid_map(tx, "tax_codes")?;
    let tax_event_uids = uid_map(tx, "tax_events")?;
    let invoice_uids = uid_map(tx, "invoices")?;
    let filing_uids = uid_map(tx, "filings")?;
    let template_uids = uid_map(tx, "entry_templates")?;

    // Entities and the scope.
    struct EntityRow {
        id: i64,
        uid: String,
        currency: String,
        value: Map<String, Value>,
    }
    let mut entity_rows: Vec<EntityRow> = {
        let mut stmt = tx.prepare("SELECT id, uid, name, kind, legal_form, country, currency, fiscal_year_end, lock_date, incorporated_on, registrations, archived_at FROM entities ORDER BY id")?;
        let rows = stmt.query_map([], |r| {
            let uid: String = r.get(1)?;
            let currency: String = r.get(6)?;
            let mut v = Map::new();
            v.insert("uid".into(), json!(uid));
            v.insert("name".into(), json!(r.get::<_, String>(2)?));
            v.insert("kind".into(), json!(r.get::<_, String>(3)?));
            v.insert("legal_form".into(), json!(r.get::<_, String>(4)?));
            v.insert("country".into(), json!(r.get::<_, String>(5)?));
            v.insert("currency".into(), json!(currency));
            v.insert("fiscal_year_end".into(), json!(r.get::<_, String>(7)?));
            v.insert("lock_date".into(), json!(r.get::<_, Option<String>>(8)?));
            v.insert("incorporated_on".into(), json!(r.get::<_, Option<String>>(9)?));
            v.insert("registrations".into(), json_text(&r.get::<_, String>(10)?));
            v.insert("archived_at".into(), json!(r.get::<_, Option<String>>(11)?));
            Ok(EntityRow { id: r.get(0)?, uid, currency, value: v })
        })?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    if !options.entity_uids.is_empty() {
        let wanted: BTreeSet<&str> = options.entity_uids.iter().map(String::as_str).collect();
        for uid in &wanted {
            if !entity_rows.iter().any(|e| e.uid == *uid) {
                return Err(Error::NotFound(format!("entity {uid}")));
            }
        }
        entity_rows.retain(|e| wanted.contains(e.uid.as_str()));
    }
    let scope: HashSet<i64> = entity_rows.iter().map(|e| e.id).collect();
    let currency_of: HashMap<i64, String> = entity_rows.iter().map(|e| (e.id, e.currency.clone())).collect();

    let mut classifications: HashMap<i64, Vec<Value>> = HashMap::new();
    {
        let mut stmt =
            tx.prepare("SELECT entity_id, jurisdiction, classification, effective_from, effective_to, note FROM entity_classifications ORDER BY entity_id, jurisdiction, effective_from, id")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                json!({
                    "jurisdiction": r.get::<_, String>(1)?,
                    "classification": r.get::<_, String>(2)?,
                    "effective_from": r.get::<_, String>(3)?,
                    "effective_to": r.get::<_, Option<String>>(4)?,
                    "note": r.get::<_, String>(5)?,
                }),
            ))
        })?;
        for row in rows {
            let (id, v) = row?;
            classifications.entry(id).or_default().push(v);
        }
    }
    let mut groups: HashMap<i64, Value> = HashMap::new();
    {
        let mut stmt = tx.prepare(
            "SELECT m.entity_id, g.uid, g.name, m.parent_entity_id, m.ownership_bp, m.control, m.effective_from, m.effective_to
             FROM entity_group_members m JOIN entity_groups g ON g.id = m.group_id
             WHERE m.effective_from <= ?1 AND (m.effective_to IS NULL OR m.effective_to > ?1) ORDER BY m.id",
        )?;
        let rows = stmt.query_map([&scope_date], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, bool>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Option<String>>(7)?,
            ))
        })?;
        for row in rows {
            let (entity, group_uid, group_name, parent, ownership_bp, control, from, to) = row?;
            groups.insert(
                entity,
                json!({
                    "group_uid": group_uid,
                    "group_name": group_name,
                    "parent_entity_uid": uid_of(&entity_uids, parent),
                    "ownership_bp": ownership_bp,
                    "control": control,
                    "effective_from": from,
                    "effective_to": to,
                }),
            );
        }
    }
    let entities_json: Vec<Value> = entity_rows
        .iter()
        .map(|e| {
            let mut v = e.value.clone();
            v.insert("classifications".into(), Value::Array(classifications.remove(&e.id).unwrap_or_default()));
            v.insert("group".into(), groups.remove(&e.id).unwrap_or(Value::Null));
            Value::Object(v)
        })
        .collect();

    // Accounts, including closed accounts.
    let accounts_json: Vec<Value> = accounts::list_accounts(tx, None, true)?
        .into_iter()
        .filter(|a| scope.contains(&a.entity_id))
        .map(|a| {
            json!({
                "uid": a.uid,
                "entity_uid": uid_of(&entity_uids, Some(a.entity_id)),
                "parent_uid": uid_of(&account_uids, a.parent_id),
                "path": a.path,
                "name": a.name,
                "type": a.r#type.as_str(),
                "subtype": a.subtype,
                "class": a.class,
                "commodity": a.commodity,
                "precision": a.precision,
                "placeholder": a.placeholder,
                "closed_at": a.closed_at,
                "statutory_code": a.statutory_code,
                "tax_treatment": a.tax_treatment,
                "intercompany": a.intercompany,
                "external_ids": a.external_ids,
            })
        })
        .collect();

    // Entries and drafts: the rows first, so that other records can check their links.
    let reviewed_by = reviewers(tx)?;
    let mut tags: HashMap<i64, Vec<String>> = HashMap::new();
    {
        let mut stmt = tx.prepare("SELECT entry_id, key, value FROM entry_tags ORDER BY entry_id, key, value")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
        for row in rows {
            let (id, key, value) = row?;
            tags.entry(id).or_default().push(if value.is_empty() { key } else { format!("{key}:{value}") });
        }
    }
    let entry_rows: Vec<EntryRow> = {
        let mut stmt = tx.prepare(
            "SELECT id, uid, entity_id, date, payee, payee_id, description, notes, status, origin, reverses_id, counterpart_id, reviewed_at, seq, prev_hash, hash,
                    refund_of_id, template_id, template_version, posted_at, created_at
             FROM journal_entries ORDER BY date, id",
        )?;
        let rows = stmt.query_map([], |r| {
            let id: i64 = r.get(0)?;
            let date: String = r.get(3)?;
            let mut v = Map::new();
            v.insert("uid".into(), json!(r.get::<_, String>(1)?));
            v.insert("entity_uid".into(), uid_of(&entity_uids, Some(r.get(2)?)));
            v.insert("date".into(), json!(date));
            v.insert("payee".into(), json!(r.get::<_, String>(4)?));
            v.insert("payee_uid".into(), uid_of(&payee_uids, r.get(5)?));
            v.insert("description".into(), json!(r.get::<_, String>(6)?));
            v.insert("notes".into(), json!(r.get::<_, String>(7)?));
            v.insert("status".into(), json!(r.get::<_, String>(8)?));
            v.insert("origin".into(), json!(r.get::<_, String>(9)?));
            v.insert("reverses_uid".into(), uid_of(&entry_uids, r.get(10)?));
            v.insert("counterpart_uid".into(), uid_of(&entry_uids, r.get(11)?));
            v.insert("reviewed_at".into(), json!(r.get::<_, Option<String>>(12)?));
            v.insert("reviewed_by".into(), json!(reviewed_by.get(&id)));
            v.insert("seq".into(), json!(r.get::<_, Option<i64>>(13)?));
            v.insert("prev_hash".into(), json!(r.get::<_, Option<String>>(14)?));
            v.insert("hash".into(), json!(r.get::<_, Option<String>>(15)?));
            v.insert("tags".into(), json!(tags.get(&id).cloned().unwrap_or_default()));
            let mut metadata = Map::new();
            if let Some(uid) = r.get::<_, Option<i64>>(16)?.and_then(|i| entry_uids.get(&i)) {
                metadata.insert("refund_of_uid".into(), json!(uid));
            }
            if let Some(uid) = r.get::<_, Option<i64>>(17)?.and_then(|i| template_uids.get(&i)) {
                metadata.insert("template_uid".into(), json!(uid));
            }
            if let Some(version) = r.get::<_, Option<i64>>(18)? {
                metadata.insert("template_version".into(), json!(version));
            }
            if let Some(at) = r.get::<_, Option<String>>(19)? {
                metadata.insert("posted_at".into(), json!(at));
            }
            metadata.insert("created_at".into(), json!(r.get::<_, String>(20)?));
            v.insert("metadata".into(), Value::Object(metadata));
            Ok(EntryRow { id, entity_id: r.get(2)?, status: r.get(8)?, value: v })
        })?;
        let all: Vec<EntryRow> = rows.collect::<std::result::Result<_, _>>()?;
        all.into_iter().filter(|e| scope.contains(&e.entity_id) && before_or_on(e.value["date"].as_str())).collect()
    };
    let exported_entries: HashSet<i64> = entry_rows.iter().map(|e| e.id).collect();
    let posting_entry: HashMap<i64, i64> = {
        let mut stmt = tx.prepare("SELECT id, journal_entry_id FROM postings")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let posting_exported = |id: i64| posting_entry.get(&id).is_some_and(|e| exported_entries.contains(e));

    // Statement lines of accounts in scope.
    let mut line_ids_by_posting: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut exported_lines: HashSet<i64> = HashSet::new();
    let mut lines_json = Vec::new();
    {
        let mut stmt = tx.prepare(
            "SELECT s.id, i.uid, a.uid, a.entity_id, s.date, s.amount, ac.code, ac.precision, s.description, s.raw, s.reference, s.fingerprint, s.posting_id
             FROM statement_lines s JOIN imports i ON i.id = s.import_id JOIN accounts a ON a.id = s.account_id JOIN commodities ac ON ac.id = a.commodity_id
             ORDER BY s.id",
        )?;
        #[allow(clippy::type_complexity)]
        let rows: Vec<(i64, String, String, i64, Option<String>, Option<i64>, String, u32, String, String, String, String, Option<i64>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?, r.get(11)?, r.get(12)?)))?
            .collect::<std::result::Result<_, _>>()?;
        for (id, import_uid, account_uid, entity_id, date, amount, commodity, precision, description, raw, reference, fingerprint, posting_id) in rows {
            if !scope.contains(&entity_id) || !before_or_on(date.as_deref()) {
                continue;
            }
            let raw = json_text(&raw);
            let counterparty = raw.get("counterparty").and_then(Value::as_str).unwrap_or_default().to_string();
            let posting = posting_id.filter(|p| posting_exported(*p));
            if let Some(p) = posting {
                line_ids_by_posting.entry(p).or_default().push(id);
            }
            exported_lines.insert(id);
            lines_json.push(json!({
                "id": id,
                "import_uid": import_uid,
                "account_uid": account_uid,
                "entity_uid": uid_of(&entity_uids, Some(entity_id)),
                "date": date,
                "amount": amount.map(|m| money(m, &commodity, precision)).transpose()?,
                "description": description,
                "counterparty": counterparty,
                "external_id": reference,
                "fingerprint": fingerprint,
                "posting_uids": posting.and_then(|p| posting_uids.get(&p)).into_iter().collect::<Vec<_>>(),
            }));
        }
    }

    // Tax events of exported entries and drafts.
    let mut tax_by_posting: HashMap<i64, Vec<String>> = HashMap::new();
    let mut tax_events_json = Vec::new();
    {
        let mut stmt = tx.prepare(
            "SELECT t.id, t.uid, t.entity_id, t.entry_id, t.posting_id, t.tax_code_id, c.code, c.kind, t.tax_point, t.base_amount, t.tax_amount, t.recoverable_amount,
                    t.withheld_amount, t.commodity, t.native_base, t.native_tax, t.native_commodity, t.party_id, t.document_ref, t.rule_version, t.corrects_event_id, t.metadata
             FROM tax_events t JOIN tax_codes c ON c.id = t.tax_code_id ORDER BY t.id",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let entity_id: i64 = r.get(2)?;
            let entry_id: i64 = r.get(3)?;
            let tax_point: String = r.get(8)?;
            if !scope.contains(&entity_id) || !exported_entries.contains(&entry_id) || !before_or_on(Some(&tax_point)) {
                continue;
            }
            let uid: String = r.get(1)?;
            let posting_id: Option<i64> = r.get(4)?;
            if let Some(p) = posting_id {
                tax_by_posting.entry(p).or_default().push(uid.clone());
            }
            let commodity: String = r.get(13)?;
            let precision = precision_of(&precisions, &commodity)?;
            let native_commodity: Option<String> = r.get(16)?;
            let native = |minor: Option<i64>| -> Result<Value> {
                match (minor, &native_commodity) {
                    (Some(m), Some(code)) => money(m, code, precision_of(&precisions, code)?),
                    _ => Ok(Value::Null),
                }
            };
            tax_events_json.push(json!({
                "uid": uid,
                "entity_uid": uid_of(&entity_uids, Some(entity_id)),
                "entry_uid": uid_of(&entry_uids, Some(entry_id)),
                "posting_uid": uid_of(&posting_uids, posting_id),
                "tax_code_uid": uid_of(&tax_code_uids, r.get(5)?),
                "code": r.get::<_, String>(6)?,
                "kind": r.get::<_, String>(7)?,
                "tax_point": tax_point,
                "base": money(r.get(9)?, &commodity, precision)?,
                "tax": money(r.get(10)?, &commodity, precision)?,
                "recoverable": money(r.get(11)?, &commodity, precision)?,
                "withheld": money(r.get(12)?, &commodity, precision)?,
                "native_base": native(r.get(14)?)?,
                "native_tax": native(r.get(15)?)?,
                "native_commodity": native_commodity,
                "party_uid": uid_of(&payee_uids, r.get(17)?),
                "document_ref": r.get::<_, String>(18)?,
                "rule_version": r.get::<_, String>(19)?,
                "corrects_event_uid": uid_of(&tax_event_uids, r.get(20)?),
                "metadata": json_text(&r.get::<_, String>(21)?),
            }));
        }
    }

    // Documents and their links.
    let mut docs_by_entry: HashMap<i64, Vec<String>> = HashMap::new();
    let mut docs_by_posting: HashMap<i64, Vec<String>> = HashMap::new();
    let mut links_by_document: HashMap<i64, Vec<Value>> = HashMap::new();
    {
        let mut stmt = tx.prepare("SELECT document_id, entry_id, posting_id, statement_line_id, role FROM document_links ORDER BY id")?;
        type LinkRow = (i64, Option<i64>, Option<i64>, Option<i64>, String);
        let rows: Vec<LinkRow> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<std::result::Result<_, _>>()?;
        for (document_id, entry_id, posting_id, line_id, role) in rows {
            // A link that points outside the snapshot would break, so it stays out.
            let entry_ok = entry_id.is_none_or(|e| exported_entries.contains(&e));
            let posting_ok = posting_id.is_none_or(&posting_exported);
            let line_ok = line_id.is_none_or(|l| exported_lines.contains(&l));
            if !(entry_ok && posting_ok && line_ok) {
                continue;
            }
            let Some(doc_uid) = document_uids.get(&document_id) else { continue };
            if let Some(e) = entry_id {
                docs_by_entry.entry(e).or_default().push(doc_uid.clone());
            }
            if let Some(p) = posting_id {
                docs_by_posting.entry(p).or_default().push(doc_uid.clone());
            }
            links_by_document.entry(document_id).or_default().push(json!({
                "entry_uid": uid_of(&entry_uids, entry_id),
                "posting_uid": uid_of(&posting_uids, posting_id),
                "statement_line_id": line_id,
                "role": role,
            }));
        }
    }
    let mut documents_json = Vec::new();
    let mut document_files: Vec<DocumentFile> = Vec::new();
    {
        let mut stmt = tx.prepare("SELECT id, uid, entity_id, sha256, filename, mime, size, stored_path, source, received_at, metadata FROM documents ORDER BY id")?;
        #[allow(clippy::type_complexity)]
        let rows: Vec<(i64, String, i64, String, String, String, i64, String, String, String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?)))?
            .collect::<std::result::Result<_, _>>()?;
        let mut seen = HashSet::new();
        for (id, uid, entity_id, sha256, filename, mime, size, stored_path, source, received_at, metadata) in rows {
            if !scope.contains(&entity_id) || !before_or_on(Some(&received_at)) {
                continue;
            }
            if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(Error::Invalid(format!("document {uid}: invalid sha256")));
            }
            let path = documents::relative_store_path(&sha256);
            if seen.insert(path.clone()) {
                document_files.push(DocumentFile { name: path.clone(), source: documents::ledger_dir(tx)?.join(&stored_path), sha256: sha256.clone() });
            }
            documents_json.push(json!({
                "uid": uid,
                "entity_uid": uid_of(&entity_uids, Some(entity_id)),
                "sha256": sha256,
                "filename": filename,
                "mime": mime,
                "size": size,
                "source": source,
                "received_at": received_at,
                "metadata": json_text(&metadata),
                "path": path,
                "links": links_by_document.remove(&id).unwrap_or_default(),
            }));
        }
    }

    // Postings, with the control totals of booked entries.
    let mut postings_by_entry: HashMap<i64, Vec<Value>> = HashMap::new();
    let mut debits: HashMap<i64, i128> = HashMap::new();
    let mut credits: HashMap<i64, i128> = HashMap::new();
    let booked: HashMap<i64, (i64, bool)> = entry_rows.iter().map(|e| (e.id, (e.entity_id, e.status != "draft"))).collect();
    let mut booked_postings = 0i64;
    {
        let mut stmt = tx.prepare(
            "SELECT p.id, p.uid, p.journal_entry_id, a.uid, p.quantity, ac.code, ac.precision, p.amount, fc.code, fc.precision, p.rate, p.rate_source, p.memo, p.metadata,
                    p.external_id, p.fingerprint, p.reconciled_at
             FROM postings p JOIN accounts a ON a.id = p.account_id JOIN commodities ac ON ac.id = a.commodity_id
                  JOIN journal_entries e ON e.id = p.journal_entry_id JOIN entities en ON en.id = e.entity_id JOIN commodities fc ON fc.code = en.currency
             ORDER BY p.journal_entry_id, p.position, p.id",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            let entry_id: i64 = r.get(2)?;
            let Some(&(entity_id, is_booked)) = booked.get(&entry_id) else { continue };
            let amount: i64 = r.get(7)?;
            if is_booked {
                booked_postings += 1;
                if amount > 0 {
                    *debits.entry(entity_id).or_default() += amount as i128;
                } else {
                    *credits.entry(entity_id).or_default() -= amount as i128;
                }
            }
            postings_by_entry.entry(entry_id).or_default().push(json!({
                "uid": r.get::<_, String>(1)?,
                "account_uid": r.get::<_, String>(3)?,
                "quantity": money(r.get(4)?, &r.get::<_, String>(5)?, r.get(6)?)?,
                "amount": money(amount, &r.get::<_, String>(8)?, r.get(9)?)?,
                "rate": r.get::<_, Option<String>>(10)?,
                "rate_source": r.get::<_, String>(11)?,
                "memo": r.get::<_, String>(12)?,
                "metadata": json_text(&r.get::<_, String>(13)?),
                "external_id": r.get::<_, Option<String>>(14)?,
                "fingerprint": r.get::<_, Option<String>>(15)?,
                "reconciled_at": r.get::<_, Option<String>>(16)?,
                "statement_line_ids": line_ids_by_posting.remove(&id).unwrap_or_default(),
                "document_uids": docs_by_posting.remove(&id).unwrap_or_default(),
                "tax_event_uids": tax_by_posting.remove(&id).unwrap_or_default(),
            }));
        }
    }
    let mut entries_json = Vec::new();
    let mut drafts_json = Vec::new();
    for mut e in entry_rows {
        e.value.insert("postings".into(), Value::Array(postings_by_entry.remove(&e.id).unwrap_or_default()));
        e.value.insert("document_uids".into(), json!(docs_by_entry.remove(&e.id).unwrap_or_default()));
        if e.status == "draft" {
            drafts_json.push(Value::Object(e.value));
        } else {
            entries_json.push(Value::Object(e.value));
        }
    }

    // Parties: the payees of the entities in scope.
    let mut aliases: HashMap<i64, Vec<String>> = HashMap::new();
    {
        let mut stmt = tx.prepare("SELECT payee_id, text FROM payee_aliases ORDER BY payee_id, text")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (id, text) = row?;
            aliases.entry(id).or_default().push(text);
        }
    }
    let mut relationships: HashMap<i64, Vec<Value>> = HashMap::new();
    {
        let mut stmt = tx.prepare("SELECT payee_id, entity_id, kind, ownership_bp, effective_from, effective_to, basis FROM party_relationships ORDER BY id")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                json!({
                    "entity_uid": uid_of(&entity_uids, Some(r.get(1)?)),
                    "kind": r.get::<_, String>(2)?,
                    "ownership_bp": r.get::<_, Option<i64>>(3)?,
                    "effective_from": r.get::<_, String>(4)?,
                    "effective_to": r.get::<_, Option<String>>(5)?,
                    "basis": r.get::<_, String>(6)?,
                }),
            ))
        })?;
        for row in rows {
            let (id, v) = row?;
            relationships.entry(id).or_default().push(v);
        }
    }
    let mut parties_json = Vec::new();
    {
        let mut stmt = tx.prepare("SELECT id, uid, entity_id, name, kind, country, tax_ids FROM payees ORDER BY id")?;
        let rows: Vec<(i64, String, i64, String, String, String, String)> =
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?.collect::<std::result::Result<_, _>>()?;
        for (id, uid, entity_id, name, kind, country, tax_ids) in rows {
            if !scope.contains(&entity_id) {
                continue;
            }
            parties_json.push(json!({
                "uid": uid,
                "name": name,
                "kind": kind,
                "country": country,
                "tax_ids": json_text(&tax_ids),
                "aliases": aliases.remove(&id).unwrap_or_default(),
                "relationships": relationships.remove(&id).unwrap_or_default(),
            }));
        }
    }

    // Rates from the prices table.
    let rates_json: Vec<Value> = {
        let mut stmt = tx.prepare(
            "SELECT p.on_date, c.code, cur.code, p.price, p.source FROM prices p JOIN commodities c ON c.id = p.commodity_id JOIN commodities cur ON cur.id = p.currency_id
             ORDER BY p.on_date, c.code, cur.code",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(json!({
                "date": r.get::<_, String>(0)?,
                "base": r.get::<_, String>(1)?,
                "quote": r.get::<_, String>(2)?,
                "rate": r.get::<_, String>(3)?,
                "source": r.get::<_, String>(4)?,
            }))
        })?;
        rows.collect::<std::result::Result<_, _>>()?
    };

    // Tax codes.
    let tax_codes_json: Vec<Value> = {
        let mut stmt = tx.prepare(
            "SELECT uid, entity_id, code, name, kind, rate_bp, tax_account_id, recoverable_bp, jurisdiction, effective_from, effective_to, active FROM tax_codes
             ORDER BY entity_id, code, effective_from, id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(1)?,
                json!({
                    "uid": r.get::<_, String>(0)?,
                    "entity_uid": uid_of(&entity_uids, Some(r.get(1)?)),
                    "code": r.get::<_, String>(2)?,
                    "name": r.get::<_, String>(3)?,
                    "kind": r.get::<_, String>(4)?,
                    "rate_bp": r.get::<_, i64>(5)?,
                    "tax_account_uid": uid_of(&account_uids, r.get(6)?),
                    "recoverable_bp": r.get::<_, i64>(7)?,
                    "jurisdiction": r.get::<_, String>(8)?,
                    "effective_from": r.get::<_, String>(9)?,
                    "effective_to": r.get::<_, Option<String>>(10)?,
                    "active": r.get::<_, bool>(11)?,
                }),
            ))
        })?;
        let all: Vec<(i64, Value)> = rows.collect::<std::result::Result<_, _>>()?;
        all.into_iter().filter(|(e, _)| scope.contains(e)).map(|(_, v)| v).collect()
    };

    // Invoices with their lines and settlements.
    let invoice_commodity: HashMap<i64, (String, u32)> = {
        let mut stmt = tx.prepare("SELECT id, commodity FROM invoices")?;
        let rows: Vec<(i64, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<std::result::Result<_, _>>()?;
        let mut out = HashMap::new();
        for (id, code) in rows {
            let precision = precision_of(&precisions, &code)?;
            out.insert(id, (code, precision));
        }
        out
    };
    let mut invoice_lines: HashMap<i64, Vec<Value>> = HashMap::new();
    {
        let mut stmt = tx.prepare("SELECT invoice_id, position, description, quantity, unit_price, net, tax_code, tax, account_id FROM invoice_lines ORDER BY invoice_id, position")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let invoice_id: i64 = r.get(0)?;
            let Some((code, precision)) = invoice_commodity.get(&invoice_id) else { continue };
            invoice_lines.entry(invoice_id).or_default().push(json!({
                "position": r.get::<_, i64>(1)?,
                "description": r.get::<_, String>(2)?,
                "quantity": r.get::<_, String>(3)?,
                "unit_price": money(r.get(4)?, code, *precision)?,
                "net": money(r.get(5)?, code, *precision)?,
                "tax_code": r.get::<_, String>(6)?,
                "tax": money(r.get(7)?, code, *precision)?,
                "account_uid": uid_of(&account_uids, r.get(8)?),
            }));
        }
    }
    let mut settlements: HashMap<i64, Vec<Value>> = HashMap::new();
    {
        let mut stmt = tx.prepare("SELECT uid, invoice_id, entry_id, posting_id, date, amount, kind, fx_difference_entry_id, credit_note_id FROM settlements ORDER BY invoice_id, date, id")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let invoice_id: i64 = r.get(1)?;
            let Some((code, precision)) = invoice_commodity.get(&invoice_id) else { continue };
            settlements.entry(invoice_id).or_default().push(json!({
                "uid": r.get::<_, String>(0)?,
                "entry_uid": uid_of(&entry_uids, r.get(2)?),
                "posting_uid": uid_of(&posting_uids, r.get(3)?),
                "date": r.get::<_, String>(4)?,
                "amount": money(r.get(5)?, code, *precision)?,
                "kind": r.get::<_, String>(6)?,
                "fx_difference_entry_uid": uid_of(&entry_uids, r.get(7)?),
                "credit_note_uid": uid_of(&invoice_uids, r.get(8)?),
            }));
        }
    }
    let mut invoices_json = Vec::new();
    let mut settlement_count = 0i64;
    {
        let mut stmt = tx.prepare(
            "SELECT id, uid, entity_id, kind, direction, party_id, series, number, issue_date, tax_point, due_date, commodity, net, tax, gross, withheld, status,
                    credits_invoice_id, recognition_entry_id, document_id, external_ref, metadata
             FROM invoices ORDER BY id",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            let entity_id: i64 = r.get(2)?;
            if !scope.contains(&entity_id) {
                continue;
            }
            let (code, precision) = &invoice_commodity[&id];
            let invoice_settlements = settlements.remove(&id).unwrap_or_default();
            settlement_count += invoice_settlements.len() as i64;
            invoices_json.push(json!({
                "uid": r.get::<_, String>(1)?,
                "entity_uid": uid_of(&entity_uids, Some(entity_id)),
                "kind": r.get::<_, String>(3)?,
                "direction": r.get::<_, String>(4)?,
                "party_uid": uid_of(&payee_uids, r.get(5)?),
                "series": r.get::<_, String>(6)?,
                "number": r.get::<_, String>(7)?,
                "issue_date": r.get::<_, String>(8)?,
                "tax_point": r.get::<_, String>(9)?,
                "due_date": r.get::<_, Option<String>>(10)?,
                "commodity": code,
                "net": money(r.get(12)?, code, *precision)?,
                "tax": money(r.get(13)?, code, *precision)?,
                "gross": money(r.get(14)?, code, *precision)?,
                "withheld": money(r.get(15)?, code, *precision)?,
                "status": r.get::<_, String>(16)?,
                "credits_invoice_uid": uid_of(&invoice_uids, r.get(17)?),
                "recognition_entry_uid": uid_of(&entry_uids, r.get(18)?),
                "document_uid": uid_of(&document_uids, r.get(19)?),
                "external_ref": r.get::<_, String>(20)?,
                "metadata": json_text(&r.get::<_, String>(21)?),
                "lines": invoice_lines.remove(&id).unwrap_or_default(),
                "settlements": invoice_settlements,
            }));
        }
    }

    // Filings.
    let filings_json: Vec<Value> = {
        let mut stmt = tx.prepare(
            "SELECT uid, entity_id, jurisdiction, filing_code, period_start, period_end, status, snapshot_digest, chain_head_seq, chain_head_hash, artifact_digest,
                    receipt_ref, submitted_at, notes, amends_id, created_at, updated_at
             FROM filings ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(1)?,
                json!({
                    "uid": r.get::<_, String>(0)?,
                    "entity_uid": uid_of(&entity_uids, Some(r.get(1)?)),
                    "jurisdiction": r.get::<_, String>(2)?,
                    "filing_code": r.get::<_, String>(3)?,
                    "period_start": r.get::<_, String>(4)?,
                    "period_end": r.get::<_, String>(5)?,
                    "status": r.get::<_, String>(6)?,
                    "snapshot_digest": r.get::<_, String>(7)?,
                    "chain_head_seq": r.get::<_, Option<i64>>(8)?,
                    "chain_head_hash": r.get::<_, Option<String>>(9)?,
                    "artifact_digest": r.get::<_, String>(10)?,
                    "receipt_ref": r.get::<_, String>(11)?,
                    "submitted_at": r.get::<_, Option<String>>(12)?,
                    "notes": r.get::<_, String>(13)?,
                    "amends_uid": uid_of(&filing_uids, r.get(14)?),
                    "created_at": r.get::<_, String>(15)?,
                    "updated_at": r.get::<_, String>(16)?,
                }),
            ))
        })?;
        let all: Vec<(i64, Value)> = rows.collect::<std::result::Result<_, _>>()?;
        all.into_iter().filter(|(e, _)| scope.contains(e)).map(|(_, v)| v).collect()
    };

    // Links between vaults, only when the ledger has the table.
    let intervault_json: Option<Vec<Value>> = if table_exists(tx, "intervault_links")? {
        let entry_entity: HashMap<i64, i64> = {
            let mut stmt = tx.prepare("SELECT id, entity_id FROM journal_entries")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let mut stmt = tx.prepare("SELECT uid, entry_a_id, entry_b_id, kind, amount_a, commodity_a, amount_b, commodity_b, rate, note, status FROM intervault_links ORDER BY id")?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            let a: i64 = r.get(1)?;
            let b: i64 = r.get(2)?;
            let touches = [a, b].iter().any(|e| entry_entity.get(e).is_some_and(|entity| scope.contains(entity)));
            if !touches {
                continue;
            }
            let (code_a, code_b): (String, String) = (r.get(5)?, r.get(7)?);
            out.push(json!({
                "uid": r.get::<_, String>(0)?,
                "entry_a_uid": uid_of(&entry_uids, Some(a)),
                "entry_b_uid": uid_of(&entry_uids, Some(b)),
                "kind": r.get::<_, String>(3)?,
                "amount_a": money(r.get(4)?, &code_a, precision_of(&precisions, &code_a)?)?,
                "amount_b": money(r.get(6)?, &code_b, precision_of(&precisions, &code_b)?)?,
                "rate": r.get::<_, String>(8)?,
                "note": r.get::<_, String>(9)?,
                "status": r.get::<_, String>(10)?,
            }));
        }
        Some(out)
    } else {
        None
    };

    // Chain heads and control totals per entity.
    let mut chain_heads = Map::new();
    let mut control_totals = Map::new();
    for e in &entity_rows {
        let head: Option<(i64, String)> = tx
            .query_row("SELECT seq, hash FROM journal_entries WHERE entity_id = ?1 AND seq IS NOT NULL ORDER BY seq DESC LIMIT 1", [e.id], |r| {
                Ok((r.get(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default()))
            })
            .optional()?;
        if let Some((seq, hash)) = head {
            chain_heads.insert(e.uid.clone(), json!({"seq": seq, "hash": hash}));
        }
        let currency = &currency_of[&e.id];
        let precision = precision_of(&precisions, currency)?;
        let total = |map: &HashMap<i64, i128>| -> Result<Value> {
            let sum = map.get(&e.id).copied().unwrap_or_default();
            let minor = i64::try_from(sum).map_err(|_| Error::Invalid("control total overflow".into()))?;
            money(minor, currency, precision)
        };
        control_totals.insert(e.uid.clone(), json!({"debits": total(&debits)?, "credits": total(&credits)?}));
    }

    let beancount = export::beancount_scoped(tx, Some(&scope), options.as_of)?;

    let mut counts = BTreeMap::new();
    counts.insert("entities".to_string(), entities_json.len() as i64);
    counts.insert("accounts".to_string(), accounts_json.len() as i64);
    counts.insert("entries".to_string(), entries_json.len() as i64);
    counts.insert("postings".to_string(), booked_postings);
    counts.insert("drafts".to_string(), drafts_json.len() as i64);
    counts.insert("parties".to_string(), parties_json.len() as i64);
    counts.insert("documents".to_string(), documents_json.len() as i64);
    counts.insert("statement_lines".to_string(), lines_json.len() as i64);
    counts.insert("tax_codes".to_string(), tax_codes_json.len() as i64);
    counts.insert("tax_events".to_string(), tax_events_json.len() as i64);
    counts.insert("invoices".to_string(), invoices_json.len() as i64);
    counts.insert("settlements".to_string(), settlement_count);
    counts.insert("filings".to_string(), filings_json.len() as i64);
    counts.insert("intervault_links".to_string(), intervault_json.as_ref().map_or(0, |v| v.len() as i64));

    let mut payloads = vec![
        array_file("entities.json", &entities_json)?,
        array_file("accounts.json", &accounts_json)?,
        lines_file("entries.jsonl", &entries_json)?,
        lines_file("drafts.jsonl", &drafts_json)?,
        array_file("parties.json", &parties_json)?,
        array_file("documents.json", &documents_json)?,
        array_file("statement_lines.json", &lines_json)?,
        array_file("rates.json", &rates_json)?,
        array_file("tax_codes.json", &tax_codes_json)?,
        lines_file("tax_events.jsonl", &tax_events_json)?,
        array_file("invoices.json", &invoices_json)?,
        array_file("filings.json", &filings_json)?,
        Payload { name: "books.beancount".into(), bytes: beancount.into_bytes() },
    ];
    if let Some(links) = &intervault_json {
        payloads.push(array_file("intervault_links.json", links)?);
    }

    Ok(Collected {
        payloads,
        documents: document_files,
        entity_uids: entity_rows.iter().map(|e| e.uid.clone()).collect(),
        posting_policy: policy::current(tx)?.as_str().to_string(),
        counts,
        chain_heads,
        control_totals,
    })
}

fn io(context: &str, path: &Path, e: std::io::Error) -> Error {
    Error::Other(anyhow::anyhow!("{context} {}: {e}", path.display()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Write a new file, flush it and sync it to disk.
fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| io("create", dir, e))?;
    }
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(|e| io("create", path, e))?;
    file.write_all(bytes).map_err(|e| io("write", path, e))?;
    file.flush().map_err(|e| io("flush", path, e))?;
    file.sync_all().map_err(|e| io("sync", path, e))?;
    Ok(())
}

fn sync_dir(dir: &Path) {
    // Some platforms refuse to sync a directory. The rename still happens.
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

/// The manifest of a snapshot, as `export` writes it.
#[derive(Debug, Clone, Serialize)]
pub struct ExportOutcome {
    pub dir: PathBuf,
    pub manifest: Value,
}

/// Export the books into the new directory `dir`. Refuses an existing `dir`.
///
/// One read transaction gives every record. The files go into `DIR.tmp-<random>` first. The
/// manifest goes in last, and a rename then publishes the complete directory.
pub fn export(conn: &mut Connection, dir: &Path, options: &SnapshotOptions) -> Result<ExportOutcome> {
    if dir.symlink_metadata().is_ok() {
        return Err(Error::Conflict(format!("{} exists; a snapshot needs a new directory", dir.display())));
    }
    let name = dir.file_name().and_then(|n| n.to_str()).ok_or_else(|| Error::Invalid(format!("{} has no directory name", dir.display())))?;
    let collected = {
        let tx = conn.transaction()?;
        let collected = collect(&tx, options)?;
        tx.commit()?;
        collected
    };
    let exported_at = now_ts();
    let tmp = dir.with_file_name(format!("{name}.tmp-{}", new_uid()));
    let result = (|| -> Result<Value> {
        std::fs::create_dir_all(&tmp).map_err(|e| io("create", &tmp, e))?;
        let mut files = Map::new();
        for payload in &collected.payloads {
            write_synced(&tmp.join(&payload.name), &payload.bytes)?;
            files.insert(payload.name.clone(), json!({"sha256": sha256_hex(&payload.bytes), "bytes": payload.bytes.len()}));
        }
        for doc in &collected.documents {
            let bytes = std::fs::read(&doc.source).map_err(|e| io("read document", &doc.source, e))?;
            let actual = sha256_hex(&bytes);
            if actual != doc.sha256 {
                return Err(Error::Conflict(format!("hash mismatch: stored document {} has hash {actual}", doc.sha256)));
            }
            write_synced(&tmp.join(&doc.name), &bytes)?;
            files.insert(doc.name.clone(), json!({"sha256": actual, "bytes": bytes.len()}));
        }
        let manifest = json!({
            "schema_version": SCHEMA_VERSION,
            "generator": {
                "name": "means",
                "version": env!("CARGO_PKG_VERSION"),
                "commit": option_env!("MEANS_GIT_COMMIT").unwrap_or("unknown"),
            },
            "exported_at": exported_at,
            "scope": {"entity_uids": collected.entity_uids, "as_of": options.as_of.map(|d| d.to_string())},
            "posting_policy": collected.posting_policy,
            "files": files,
            "counts": collected.counts,
            "chain_heads": collected.chain_heads,
            "control_totals": collected.control_totals,
        });
        let mut bytes = serde_json::to_vec_pretty(&manifest)?;
        bytes.push(b'\n');
        write_synced(&tmp.join(MANIFEST), &bytes)?;
        for sub in dirs_under(&tmp) {
            sync_dir(&sub);
        }
        sync_dir(&tmp);
        if dir.symlink_metadata().is_ok() {
            return Err(Error::Conflict(format!("{} exists; a snapshot needs a new directory", dir.display())));
        }
        std::fs::rename(&tmp, dir).map_err(|e| io("rename", dir, e))?;
        if let Some(parent) = dir.parent() {
            sync_dir(if parent.as_os_str().is_empty() { Path::new(".") } else { parent });
        }
        Ok(manifest)
    })();
    match result {
        Ok(manifest) => Ok(ExportOutcome { dir: dir.to_path_buf(), manifest }),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            Err(e)
        }
    }
}

fn dirs_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(read) = std::fs::read_dir(dir) {
        for item in read.flatten() {
            let path = item.path();
            if path.is_dir() {
                out.extend(dirs_under(&path));
                out.push(path);
            }
        }
    }
    out
}

/// Every regular file under `dir`, as paths relative to it with `/` separators.
fn files_under(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for item in read.flatten() {
        let name = item.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        let path = item.path();
        if path.is_dir() {
            files_under(&path, &rel, out);
        } else {
            out.push(rel);
        }
    }
}

/// The result of `verify`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct VerifyReport {
    pub ok: bool,
    pub files_checked: usize,
    pub problems: Vec<String>,
}

fn read_array(dir: &Path, name: &str, problems: &mut Vec<String>) -> Vec<Value> {
    match std::fs::read(dir.join(name)).map_err(|e| e.to_string()).and_then(|b| serde_json::from_slice::<Vec<Value>>(&b).map_err(|e| e.to_string())) {
        Ok(rows) => rows,
        Err(e) => {
            problems.push(format!("{name}: {e}"));
            Vec::new()
        }
    }
}

fn read_lines(dir: &Path, name: &str, problems: &mut Vec<String>) -> Vec<Value> {
    let text = match std::fs::read_to_string(dir.join(name)) {
        Ok(text) => text,
        Err(e) => {
            problems.push(format!("{name}: {e}"));
            return Vec::new();
        }
    };
    let mut rows = Vec::new();
    for (n, line) in text.lines().enumerate() {
        match serde_json::from_str(line) {
            Ok(v) => rows.push(v),
            Err(e) => problems.push(format!("{name} line {}: {e}", n + 1)),
        }
    }
    rows
}

fn minor_of(v: &Value) -> Option<(i128, String)> {
    let minor = v.get("minor")?.as_str()?.parse::<i64>().ok()?;
    Some((minor as i128, v.get("commodity")?.as_str()?.to_string()))
}

/// Hash every file of a snapshot again and check the manifest, the counts and the control totals.
/// A problem goes into the report; only a missing directory is an error.
pub fn verify(dir: &Path) -> Result<VerifyReport> {
    if !dir.is_dir() {
        return Err(Error::NotFound(format!("snapshot directory {}", dir.display())));
    }
    let fail = |problem: String| Ok(VerifyReport { ok: false, files_checked: 0, problems: vec![problem] });
    let manifest: Value = match std::fs::read(dir.join(MANIFEST)).map_err(|e| e.to_string()).and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string())) {
        Ok(v) => v,
        Err(e) => return fail(format!("{MANIFEST}: {e}")),
    };
    if manifest.get("schema_version").and_then(Value::as_i64) != Some(SCHEMA_VERSION) {
        return fail(format!("unsupported schema_version {}", manifest.get("schema_version").unwrap_or(&Value::Null)));
    }
    let mut files_checked = 0;
    let mut problem_list = Vec::new();
    let problems = &mut problem_list;

    // Files: every listed file has its hash and size, and no other file is present.
    let listed = manifest.get("files").and_then(Value::as_object).cloned().unwrap_or_default();
    for (name, meta) in &listed {
        if name.split('/').any(|part| part.is_empty() || part == "." || part == "..") || name.contains('\\') {
            problems.push(format!("{name}: invalid file name"));
            continue;
        }
        match std::fs::read(dir.join(name)) {
            Ok(bytes) => {
                files_checked += 1;
                if meta.get("sha256").and_then(Value::as_str) != Some(sha256_hex(&bytes).as_str()) {
                    problems.push(format!("{name}: sha256 does not match the manifest"));
                }
                if meta.get("bytes").and_then(Value::as_u64) != Some(bytes.len() as u64) {
                    problems.push(format!("{name}: size does not match the manifest"));
                }
            }
            Err(e) => problems.push(format!("{name}: {e}")),
        }
    }
    let mut present = Vec::new();
    files_under(dir, "", &mut present);
    for name in present {
        if name != MANIFEST && !listed.contains_key(&name) {
            problems.push(format!("{name}: file is not in the manifest"));
        }
    }

    // Counts.
    let entities = read_array(dir, "entities.json", problems);
    let accounts = read_array(dir, "accounts.json", problems);
    let entries = read_lines(dir, "entries.jsonl", problems);
    let drafts = read_lines(dir, "drafts.jsonl", problems);
    let parties = read_array(dir, "parties.json", problems);
    let documents = read_array(dir, "documents.json", problems);
    let lines = read_array(dir, "statement_lines.json", problems);
    let tax_codes = read_array(dir, "tax_codes.json", problems);
    let tax_events = read_lines(dir, "tax_events.jsonl", problems);
    let invoices = read_array(dir, "invoices.json", problems);
    let filings = read_array(dir, "filings.json", problems);
    let intervault = if listed.contains_key("intervault_links.json") { read_array(dir, "intervault_links.json", problems) } else { Vec::new() };
    let postings = |rows: &[Value]| rows.iter().map(|e| e.get("postings").and_then(Value::as_array).map_or(0, Vec::len)).sum::<usize>();
    let settlements = invoices.iter().map(|i| i.get("settlements").and_then(Value::as_array).map_or(0, Vec::len)).sum::<usize>();
    let actual: BTreeMap<&str, usize> = [
        ("entities", entities.len()),
        ("accounts", accounts.len()),
        ("entries", entries.len()),
        ("postings", postings(&entries)),
        ("drafts", drafts.len()),
        ("parties", parties.len()),
        ("documents", documents.len()),
        ("statement_lines", lines.len()),
        ("tax_codes", tax_codes.len()),
        ("tax_events", tax_events.len()),
        ("invoices", invoices.len()),
        ("settlements", settlements),
        ("filings", filings.len()),
        ("intervault_links", intervault.len()),
    ]
    .into_iter()
    .collect();
    let counts = manifest.get("counts").cloned().unwrap_or(Value::Null);
    for key in COUNT_KEYS {
        let expected = counts.get(*key).and_then(Value::as_u64);
        if expected != Some(actual[key] as u64) {
            problems.push(format!("count {key}: manifest has {}, files have {}", expected.map_or("none".into(), |n| n.to_string()), actual[key]));
        }
    }

    // Statuses and document files.
    for (name, rows, allowed) in [("entries.jsonl", &entries, &["posted", "void"][..]), ("drafts.jsonl", &drafts, &["draft"][..])] {
        for row in rows.iter() {
            let status = row.get("status").and_then(Value::as_str).unwrap_or_default();
            if !allowed.contains(&status) {
                problems.push(format!("{name}: entry {} has status {status:?}", row.get("uid").and_then(Value::as_str).unwrap_or("?")));
            }
        }
    }
    for doc in &documents {
        let path = doc.get("path").and_then(Value::as_str).unwrap_or_default();
        let sha = doc.get("sha256").and_then(Value::as_str).unwrap_or_default();
        if !listed.contains_key(path) || !path.ends_with(sha) || sha.is_empty() {
            problems.push(format!("documents.json: document {} has no file in the snapshot", doc.get("uid").and_then(Value::as_str).unwrap_or("?")));
        }
    }

    // Control totals: the manifest balances, and entries.jsonl gives the same sums.
    let mut sums: HashMap<String, (i128, i128)> = HashMap::new();
    for entry in &entries {
        let entity = entry.get("entity_uid").and_then(Value::as_str).unwrap_or_default().to_string();
        let mut balance = 0i128;
        for posting in entry.get("postings").and_then(Value::as_array).into_iter().flatten() {
            let Some((minor, _)) = posting.get("amount").and_then(minor_of) else {
                problems.push(format!("entries.jsonl: entry {} has a posting without an amount", entry.get("uid").and_then(Value::as_str).unwrap_or("?")));
                continue;
            };
            balance += minor;
            let sum = sums.entry(entity.clone()).or_default();
            if minor > 0 {
                sum.0 += minor;
            } else {
                sum.1 -= minor;
            }
        }
        if balance != 0 {
            problems.push(format!("entries.jsonl: entry {} does not balance", entry.get("uid").and_then(Value::as_str).unwrap_or("?")));
        }
    }
    let totals = manifest.get("control_totals").and_then(Value::as_object).cloned().unwrap_or_default();
    for (entity, total) in &totals {
        let debits = total.get("debits").and_then(minor_of);
        let credits = total.get("credits").and_then(minor_of);
        match (debits, credits) {
            (Some(d), Some(c)) => {
                if d != c {
                    problems.push(format!("control totals of {entity}: debits {} and credits {} differ", d.0, c.0));
                }
                let (sd, sc) = sums.get(entity).copied().unwrap_or_default();
                if (sd, sc) != (d.0, c.0) {
                    problems.push(format!("control totals of {entity}: entries.jsonl gives debits {sd} and credits {sc}"));
                }
            }
            _ => problems.push(format!("control totals of {entity}: invalid money")),
        }
    }
    for entity in sums.keys() {
        if !totals.contains_key(entity) {
            problems.push(format!("control totals: entity {entity} is missing"));
        }
    }
    Ok(VerifyReport { ok: problem_list.is_empty(), files_checked, problems: problem_list })
}
