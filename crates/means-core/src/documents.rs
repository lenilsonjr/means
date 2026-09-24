//! Documents as evidence: invoices, receipts, contracts, statements and filed forms.
//!
//! The store is content-addressed. A file lives at
//! `<ledger directory>/documents/<first two hex digits>/<sha256>`, next to the SQLite file.
//! The `documents` row keeps the metadata and the path relative to the ledger directory.
//! One entity keeps one row per hash, so an add of the same bytes returns the existing row.
//!
//! The write order protects the evidence: write a temporary file in the target directory,
//! flush and sync it, hash what is on disk, rename it into place, then insert the row.
//! If the insert fails, the file goes away again unless it existed before.
//! Nothing here deletes a stored file.

use std::io::Write;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::{new_uid, now_ts, Error, Result};

/// Sources that the `documents.source` column accepts.
pub const SOURCES: &[&str] = &["email", "upload", "api", "filing", "import", "other"];

/// The store directory name under the ledger directory.
pub const STORE_DIR: &str = "documents";

/// Message prefixes that let clients classify errors without new error variants.
const PATH_PREFIX: &str = "invalid path: ";
const HASH_PREFIX: &str = "hash mismatch: ";

#[derive(Debug, Clone, Serialize)]
pub struct Document {
    pub id: i64,
    pub uid: String,
    pub entity_id: i64,
    pub sha256: String,
    pub filename: String,
    pub mime: String,
    pub size: i64,
    /// Path relative to the ledger directory.
    pub stored_path: String,
    pub source: String,
    pub received_at: String,
    pub metadata: serde_json::Value,
    pub created_at: String,
    /// True when this call stored a new document; false when the entity already had this hash.
    pub created: bool,
}

/// The record that a link attaches a document to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    Entry(i64),
    Posting(i64),
    StatementLine(i64),
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentLink {
    pub id: i64,
    pub document_id: i64,
    pub document_uid: String,
    pub entry_id: Option<i64>,
    pub entry_uid: Option<String>,
    pub posting_id: Option<i64>,
    pub posting_uid: Option<String>,
    pub statement_line_id: Option<i64>,
    pub role: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifyIssue {
    pub uid: String,
    pub entity_id: i64,
    pub stored_path: String,
    pub expected_sha256: String,
    /// The hash of the file on disk. Empty when the file is missing or unreadable.
    pub actual_sha256: String,
    pub problem: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifyReport {
    pub checked: usize,
    pub passed: usize,
    pub missing: Vec<VerifyIssue>,
    pub mismatched: Vec<VerifyIssue>,
}

impl VerifyReport {
    pub fn is_clean(&self) -> bool {
        self.missing.is_empty() && self.mismatched.is_empty()
    }
}

/// A stable snake_case code for an error from this module.
pub fn error_code(e: &Error) -> &'static str {
    match e {
        Error::NotFound(_) => "not_found",
        Error::Invalid(m) if m.starts_with(PATH_PREFIX) => "invalid_path",
        Error::Conflict(m) if m.starts_with(HASH_PREFIX) => "hash_mismatch",
        Error::Conflict(_) => "duplicate",
        Error::Invalid(_) | Error::Parse(_) | Error::Json(_) => "invalid_input",
        Error::Unbalanced(_) => "unbalanced",
        Error::Locked(_) => "locked_period",
        Error::Db(_) | Error::Other(_) => "io_error",
    }
}

/// The lowercase hex SHA-256 of the content. Imports use the same checksum.
pub fn sha256_hex(bytes: &[u8]) -> String {
    crate::imports::checksum(bytes)
}

/// Reject a file name that could escape the store or name a directory.
/// The store never uses the name as a path; it keeps it as metadata only.
pub fn validate_filename(filename: &str) -> Result<()> {
    let name = filename.trim();
    if name.is_empty() {
        return Err(Error::Invalid(format!("{PATH_PREFIX}file name is empty")));
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        return Err(Error::Invalid(format!("{PATH_PREFIX}file name {filename:?} contains a path separator")));
    }
    if name.contains("..") {
        return Err(Error::Invalid(format!("{PATH_PREFIX}file name {filename:?} contains \"..\"")));
    }
    Ok(())
}

/// The MIME type for a file name, by extension.
pub fn mime_for(filename: &str) -> &'static str {
    let ext = filename.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "csv" => "text/csv",
        "txt" => "text/plain",
        "xml" => "application/xml",
        "json" => "application/json",
        "eml" => "message/rfc822",
        "msg" => "application/vnd.ms-outlook",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

/// The directory that holds the ledger file. In-memory ledgers have none.
pub fn ledger_dir(conn: &Connection) -> Result<PathBuf> {
    let path = conn.path().unwrap_or_default();
    if path.is_empty() || path == ":memory:" {
        return Err(Error::Invalid("documents need a ledger file; an in-memory ledger has no document store".into()));
    }
    let dir = Path::new(path).parent().map(Path::to_path_buf).unwrap_or_default();
    Ok(if dir.as_os_str().is_empty() { PathBuf::from(".") } else { dir })
}

/// The path of a stored file relative to the ledger directory.
pub fn relative_store_path(sha256: &str) -> String {
    format!("{STORE_DIR}/{}/{sha256}", &sha256[..2])
}

/// The absolute path of a stored document.
pub fn absolute_path(conn: &Connection, doc: &Document) -> Result<PathBuf> {
    Ok(ledger_dir(conn)?.join(&doc.stored_path))
}

fn validate_source(source: &str) -> Result<()> {
    if SOURCES.contains(&source) {
        Ok(())
    } else {
        Err(Error::Invalid(format!("source must be one of {}, got {source:?}", SOURCES.join(", "))))
    }
}

fn validate_received(received_at: &str) -> Result<()> {
    let s = received_at.trim();
    if chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok() || chrono::DateTime::parse_from_rfc3339(s).is_ok() {
        Ok(())
    } else {
        Err(Error::Invalid(format!("received date must be YYYY-MM-DD or RFC 3339, got {received_at:?}")))
    }
}

fn io(context: &str, path: &Path, e: std::io::Error) -> Error {
    Error::Other(anyhow::anyhow!("{context} {}: {e}", path.display()))
}

fn hash_file(path: &Path) -> std::io::Result<String> {
    Ok(sha256_hex(&std::fs::read(path)?))
}

/// Write the bytes to `target` through a synced temporary file in the same directory.
/// The caller makes sure that `target` does not exist yet.
fn write_verified(target: &Path, bytes: &[u8], sha256: &str) -> Result<()> {
    let dir = target.parent().ok_or_else(|| Error::Invalid(format!("{PATH_PREFIX}no parent directory")))?;
    std::fs::create_dir_all(dir).map_err(|e| io("create", dir, e))?;
    let tmp = dir.join(format!(".{sha256}.{}.tmp", new_uid()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp).map_err(|e| io("create", &tmp, e))?;
        file.write_all(bytes).map_err(|e| io("write", &tmp, e))?;
        file.flush().map_err(|e| io("flush", &tmp, e))?;
        file.sync_all().map_err(|e| io("sync", &tmp, e))?;
        drop(file);
        let written = hash_file(&tmp).map_err(|e| io("read", &tmp, e))?;
        if written != sha256 {
            return Err(Error::Conflict(format!("{HASH_PREFIX}wrote {written}, expected {sha256}")));
        }
        std::fs::rename(&tmp, target).map_err(|e| io("rename", target, e))?;
        // Sync the directory so that the rename survives a crash. Some platforms refuse this.
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

const DOC_COLUMNS: &str = "id, uid, entity_id, sha256, filename, mime, size, stored_path, source, received_at, metadata, created_at";

fn row_to_document(r: &rusqlite::Row<'_>) -> rusqlite::Result<(Document, String)> {
    let metadata: String = r.get(10)?;
    Ok((
        Document {
            id: r.get(0)?,
            uid: r.get(1)?,
            entity_id: r.get(2)?,
            sha256: r.get(3)?,
            filename: r.get(4)?,
            mime: r.get(5)?,
            size: r.get(6)?,
            stored_path: r.get(7)?,
            source: r.get(8)?,
            received_at: r.get(9)?,
            metadata: serde_json::Value::Null,
            created_at: r.get(11)?,
            created: false,
        },
        metadata,
    ))
}

fn with_metadata((mut doc, metadata): (Document, String)) -> Result<Document> {
    doc.metadata = serde_json::from_str(&metadata)?;
    Ok(doc)
}

fn find_by_hash(conn: &Connection, entity_id: i64, sha256: &str) -> Result<Option<Document>> {
    let row = conn.query_row(&format!("SELECT {DOC_COLUMNS} FROM documents WHERE entity_id = ?1 AND sha256 = ?2"), params![entity_id, sha256], row_to_document).optional()?;
    row.map(with_metadata).transpose()
}

/// Store a document for an entity and record it. Idempotent per entity and hash:
/// a second add of the same bytes returns the existing row with `created: false`.
pub fn add_document(conn: &mut Connection, entity_id: i64, bytes: &[u8], filename: &str, source: &str, received_at: &str, metadata: serde_json::Value) -> Result<Document> {
    validate_filename(filename)?;
    validate_source(source)?;
    validate_received(received_at)?;
    if !metadata.is_object() {
        return Err(Error::Invalid("metadata must be a JSON object".into()));
    }
    crate::entities::get_entity(conn, entity_id)?;
    let sha256 = sha256_hex(bytes);
    let relative = relative_store_path(&sha256);
    let target = ledger_dir(conn)?.join(&relative);

    // A file with this hash can already exist, for another entity or from an earlier add.
    // Never overwrite it. Check that it still holds the expected bytes.
    let existed = target.exists();
    if existed {
        let on_disk = hash_file(&target).map_err(|e| io("read", &target, e))?;
        if on_disk != sha256 {
            return Err(Error::Conflict(format!("{HASH_PREFIX}stored file {relative} has hash {on_disk}; run doc verify")));
        }
    }

    if let Some(doc) = find_by_hash(conn, entity_id, &sha256)? {
        // The row exists. Restore a missing file from the same bytes; the hash proves it is the same evidence.
        if !existed {
            write_verified(&target, bytes, &sha256)?;
        }
        return Ok(doc);
    }

    if !existed {
        write_verified(&target, bytes, &sha256)?;
    }
    match insert_document(conn, entity_id, bytes.len() as i64, &sha256, filename.trim(), &relative, source, received_at.trim(), &metadata) {
        Ok(doc) => Ok(doc),
        Err(e) => {
            if !existed {
                let _ = std::fs::remove_file(&target);
            }
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn insert_document(conn: &mut Connection, entity_id: i64, size: i64, sha256: &str, filename: &str, relative: &str, source: &str, received_at: &str, metadata: &serde_json::Value) -> Result<Document> {
    let tx = conn.transaction()?;
    let uid = new_uid();
    let now = now_ts();
    let mime = mime_for(filename);
    tx.execute(
        "INSERT INTO documents (uid, entity_id, sha256, filename, mime, size, stored_path, source, received_at, metadata, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![uid, entity_id, sha256, filename, mime, size, relative, source, received_at, metadata.to_string(), now],
    )?;
    let id = tx.last_insert_rowid();
    let doc = Document {
        id,
        uid,
        entity_id,
        sha256: sha256.to_string(),
        filename: filename.to_string(),
        mime: mime.to_string(),
        size,
        stored_path: relative.to_string(),
        source: source.to_string(),
        received_at: received_at.to_string(),
        metadata: metadata.clone(),
        created_at: now,
        created: true,
    };
    crate::audit::log(&tx, "documents", id, "create", None, Some(serde_json::to_value(&doc)?))?;
    tx.commit()?;
    Ok(doc)
}

/// The entity that owns the target, or an error when the target does not exist.
fn target_entity(conn: &Connection, target: Link) -> Result<Option<i64>> {
    let (sql, what, id) = match target {
        Link::Entry(id) => ("SELECT entity_id FROM journal_entries WHERE id = ?1", "entry", id),
        Link::Posting(id) => ("SELECT je.entity_id FROM postings p JOIN journal_entries je ON je.id = p.journal_entry_id WHERE p.id = ?1", "posting", id),
        Link::StatementLine(id) => ("SELECT a.entity_id FROM statement_lines sl LEFT JOIN accounts a ON a.id = sl.account_id WHERE sl.id = ?1", "statement line", id),
    };
    conn.query_row(sql, [id], |r| r.get::<_, Option<i64>>(0)).optional()?.ok_or_else(|| Error::NotFound(format!("{what} #{id}")))
}

/// Attach a document to an entry, a posting or a statement line of the same entity.
pub fn link_document(conn: &mut Connection, document_id: i64, target: Link, role: &str) -> Result<DocumentLink> {
    let doc_entity: i64 =
        conn.query_row("SELECT entity_id FROM documents WHERE id = ?1", [document_id], |r| r.get(0)).optional()?.ok_or_else(|| Error::NotFound(format!("document #{document_id}")))?;
    if let Some(entity) = target_entity(conn, target)? {
        if entity != doc_entity {
            return Err(Error::Invalid("the document and the target belong to different entities".into()));
        }
    }
    let (entry, posting, line) = match target {
        Link::Entry(id) => (Some(id), None, None),
        Link::Posting(id) => (None, Some(id), None),
        Link::StatementLine(id) => (None, None, Some(id)),
    };
    let role = role.trim();
    let tx = conn.transaction()?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM document_links WHERE document_id = ?1 AND COALESCE(entry_id, 0) = COALESCE(?2, 0)
           AND COALESCE(posting_id, 0) = COALESCE(?3, 0) AND COALESCE(statement_line_id, 0) = COALESCE(?4, 0) AND role = ?5)",
        params![document_id, entry, posting, line, role],
        |r| r.get(0),
    )?;
    if exists {
        return Err(Error::Conflict("the document already has this link".into()));
    }
    tx.execute(
        "INSERT INTO document_links (document_id, entry_id, posting_id, statement_line_id, role, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![document_id, entry, posting, line, role, now_ts()],
    )?;
    let id = tx.last_insert_rowid();
    let link = get_link(&tx, id)?;
    crate::audit::log(&tx, "document_links", id, "create", None, Some(serde_json::to_value(&link)?))?;
    tx.commit()?;
    Ok(link)
}

const LINK_SELECT: &str = "SELECT l.id, l.document_id, d.uid, l.entry_id, je.uid, l.posting_id, p.uid, l.statement_line_id, l.role, l.created_at
     FROM document_links l
     JOIN documents d ON d.id = l.document_id
     LEFT JOIN journal_entries je ON je.id = l.entry_id
     LEFT JOIN postings p ON p.id = l.posting_id";

fn row_to_link(r: &rusqlite::Row<'_>) -> rusqlite::Result<DocumentLink> {
    Ok(DocumentLink {
        id: r.get(0)?,
        document_id: r.get(1)?,
        document_uid: r.get(2)?,
        entry_id: r.get(3)?,
        entry_uid: r.get(4)?,
        posting_id: r.get(5)?,
        posting_uid: r.get(6)?,
        statement_line_id: r.get(7)?,
        role: r.get(8)?,
        created_at: r.get(9)?,
    })
}

fn get_link(conn: &Connection, id: i64) -> Result<DocumentLink> {
    conn.query_row(&format!("{LINK_SELECT} WHERE l.id = ?1"), [id], row_to_link).optional()?.ok_or_else(|| Error::NotFound(format!("document link #{id}")))
}

/// Every link of one document, oldest first.
pub fn links_for_document(conn: &Connection, document_id: i64) -> Result<Vec<DocumentLink>> {
    let mut stmt = conn.prepare(&format!("{LINK_SELECT} WHERE l.document_id = ?1 ORDER BY l.id"))?;
    let rows = stmt.query_map([document_id], row_to_link)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Links that evidence an entry: links to the entry itself, to one of its postings,
/// or to a statement line that matches the entry or one of its postings.
pub fn document_links_for_entry(conn: &Connection, entry_id: i64) -> Result<Vec<DocumentLink>> {
    let mut stmt = conn.prepare(&format!(
        "{LINK_SELECT}
         WHERE l.entry_id = ?1
            OR l.posting_id IN (SELECT id FROM postings WHERE journal_entry_id = ?1)
            OR l.statement_line_id IN (
                 SELECT id FROM statement_lines
                 WHERE journal_entry_id = ?1 OR posting_id IN (SELECT id FROM postings WHERE journal_entry_id = ?1))
         ORDER BY l.id"
    ))?;
    let rows = stmt.query_map([entry_id], row_to_link)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// A posting link set aside while an entry edit replaces its postings.
pub(crate) struct DetachedLink {
    id: i64,
    document_id: i64,
    posting_id: i64,
    role: String,
    created_at: String,
}

/// Remove the posting links of an entry before its postings are replaced, and return them.
pub(crate) fn detach_posting_links(conn: &Connection, entry_id: i64) -> Result<Vec<DetachedLink>> {
    let mut stmt = conn.prepare(
        "SELECT l.id, l.document_id, l.posting_id, l.role, l.created_at FROM document_links l
         JOIN postings p ON p.id = l.posting_id WHERE p.journal_entry_id = ?1 ORDER BY l.id",
    )?;
    let rows = stmt.query_map([entry_id], |r| Ok(DetachedLink { id: r.get(0)?, document_id: r.get(1)?, posting_id: r.get(2)?, role: r.get(3)?, created_at: r.get(4)? }))?;
    let links = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    for link in &links {
        conn.execute("DELETE FROM document_links WHERE id = ?1", [link.id])?;
    }
    Ok(links)
}

/// Put detached links back. A link follows its posting to the replacement posting.
/// A link of a posting without a replacement moves to the entry.
pub(crate) fn reattach_posting_links(conn: &Connection, entry_id: i64, links: Vec<DetachedLink>, replaced: &std::collections::HashMap<i64, i64>) -> Result<()> {
    for link in links {
        match replaced.get(&link.posting_id) {
            Some(new_id) => conn.execute(
                "INSERT INTO document_links (id, document_id, posting_id, role, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![link.id, link.document_id, new_id, link.role, link.created_at],
            )?,
            None => {
                conn.execute("INSERT OR IGNORE INTO document_links (document_id, entry_id, role, created_at) VALUES (?1, ?2, ?3, ?4)", params![link.document_id, entry_id, link.role, link.created_at])?
            }
        };
    }
    Ok(())
}

/// Documents of an entity, newest received first. `since` keeps documents received on or after it.
pub fn list_documents(conn: &Connection, entity_id: i64, unlinked_only: bool, since: Option<&str>) -> Result<Vec<Document>> {
    let mut sql = format!("SELECT {DOC_COLUMNS} FROM documents d WHERE entity_id = ?1 AND (?2 IS NULL OR received_at >= ?2)");
    if unlinked_only {
        sql.push_str(" AND NOT EXISTS (SELECT 1 FROM document_links l WHERE l.document_id = d.id)");
    }
    sql.push_str(" ORDER BY received_at DESC, id DESC");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![entity_id, since.map(str::trim)], row_to_document)?;
    rows.map(|r| with_metadata(r?)).collect()
}

/// One document by its uid.
pub fn get_document(conn: &Connection, uid: &str) -> Result<Document> {
    let row = conn.query_row(&format!("SELECT {DOC_COLUMNS} FROM documents WHERE uid = ?1"), [uid.trim()], row_to_document).optional()?;
    row.map(with_metadata).transpose()?.ok_or_else(|| Error::NotFound(format!("document {uid}")))
}

/// The id of a journal entry by its uid, or by its numeric id.
pub fn entry_id_for(conn: &Connection, key: &str) -> Result<i64> {
    let key = key.trim();
    if let Some(id) = conn.query_row("SELECT id FROM journal_entries WHERE uid = ?1", [key], |r| r.get(0)).optional()? {
        return Ok(id);
    }
    if let Ok(id) = key.parse::<i64>() {
        if let Some(id) = conn.query_row("SELECT id FROM journal_entries WHERE id = ?1", [id], |r| r.get(0)).optional()? {
            return Ok(id);
        }
    }
    Err(Error::NotFound(format!("entry {key}")))
}

/// Rehash every stored file and report missing files and hash mismatches.
pub fn verify_all(conn: &Connection) -> Result<VerifyReport> {
    let dir = ledger_dir(conn)?;
    let mut stmt = conn.prepare("SELECT uid, entity_id, stored_path, sha256 FROM documents ORDER BY id")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))?;
    let mut report = VerifyReport { checked: 0, passed: 0, missing: Vec::new(), mismatched: Vec::new() };
    for row in rows {
        let (uid, entity_id, stored_path, expected) = row?;
        report.checked += 1;
        let issue = |actual: String, problem: String| VerifyIssue { uid: uid.clone(), entity_id, stored_path: stored_path.clone(), expected_sha256: expected.clone(), actual_sha256: actual, problem };
        match hash_file(&dir.join(&stored_path)) {
            Ok(actual) if actual == expected => report.passed += 1,
            Ok(actual) => report.mismatched.push(issue(actual, "hash mismatch".into())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => report.missing.push(issue(String::new(), "missing file".into())),
            Err(e) => report.missing.push(issue(String::new(), format!("unreadable file: {e}"))),
        }
    }
    Ok(report)
}
