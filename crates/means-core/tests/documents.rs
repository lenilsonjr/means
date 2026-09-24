use chrono::NaiveDate;
use means_core::documents::{self, Link};
use means_core::{accounts, entities, journal, model::*, Db};
use rusqlite::{params, Connection};
use rust_decimal::Decimal;
use std::path::PathBuf;

fn ledger(name: &str) -> (PathBuf, Db) {
    let dir = std::env::temp_dir().join(format!("means-documents-{name}-{}", means_core::new_uid()));
    let db = Db::open(dir.join("ledger.db")).unwrap();
    (dir, db)
}

fn entity(conn: &mut Connection) -> Entity {
    entities::create_entity(conn, "Sample Studio", "company", "PT", "EUR").unwrap()
}

fn entry(conn: &mut Connection, entity: &Entity) -> JournalEntry {
    let bank = accounts::ensure_account(conn, entity.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
    let office = accounts::ensure_account(conn, entity.id, AccountType::Expense, &["Office"], "expense", "EUR").unwrap();
    let mut input = EntryInput::new(entity.id, NaiveDate::from_ymd_opt(2026, 3, 4).unwrap());
    input.payee = "Example Paper Supplies".into();
    input.postings = vec![PostingInput::new(bank.id, Decimal::new(-4250, 2)), PostingInput::balancing(office.id)];
    journal::create_entry(conn, input).unwrap()
}

fn files_under(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(read) = std::fs::read_dir(dir) else { return out };
    for item in read.flatten() {
        let path = item.path();
        if path.is_dir() {
            out.extend(files_under(&path));
        } else {
            out.push(path);
        }
    }
    out
}

const INVOICE: &[u8] = b"%PDF-1.4 fictional invoice 0001 for 42.50 EUR";

#[test]
fn add_stores_content_addressed_file_and_dedupes() {
    let (dir, db) = ledger("dedupe");
    let mut conn = db.conn();
    let e = entity(&mut conn);
    let doc = documents::add_document(&mut conn, e.id, INVOICE, "invoice-0001.pdf", "upload", "2026-03-05", serde_json::json!({"vendor": "Example Paper Supplies"})).unwrap();
    assert!(doc.created);
    assert_eq!(doc.mime, "application/pdf");
    assert_eq!(doc.size, INVOICE.len() as i64);
    assert_eq!(doc.sha256, documents::sha256_hex(INVOICE));
    assert_eq!(doc.stored_path, format!("documents/{}/{}", &doc.sha256[..2], doc.sha256));
    let stored = dir.join(&doc.stored_path);
    assert_eq!(std::fs::read(&stored).unwrap(), INVOICE);

    let again = documents::add_document(&mut conn, e.id, INVOICE, "copy.pdf", "email", "2026-03-06", serde_json::json!({})).unwrap();
    assert!(!again.created);
    assert_eq!(again.uid, doc.uid);
    assert_eq!(again.filename, "invoice-0001.pdf", "the first add keeps its metadata");
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM documents", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 1);
    assert_eq!(files_under(&dir.join("documents")).len(), 1, "no temporary file stays behind");
    let audits: i64 = conn.query_row("SELECT COUNT(*) FROM audit_log WHERE table_name = 'documents' AND row_id = ?1", [doc.id], |r| r.get(0)).unwrap();
    assert_eq!(audits, 1);
    assert_eq!(documents::get_document(&conn, &doc.uid).unwrap().metadata["vendor"], "Example Paper Supplies");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn link_to_entry_and_list_unlinked() {
    let (dir, db) = ledger("link");
    let mut conn = db.conn();
    let e = entity(&mut conn);
    let je = entry(&mut conn, &e);
    let linked = documents::add_document(&mut conn, e.id, INVOICE, "invoice-0001.pdf", "upload", "2026-03-05", serde_json::json!({})).unwrap();
    let loose = documents::add_document(&mut conn, e.id, b"fictional receipt 77", "receipt-77.png", "email", "2026-03-10", serde_json::json!({})).unwrap();
    assert_eq!(loose.mime, "image/png");

    let link = documents::link_document(&mut conn, linked.id, Link::Entry(je.id), "invoice").unwrap();
    assert_eq!(link.entry_uid.as_deref(), Some(je.uid.as_str()));
    assert_eq!(link.document_uid, linked.uid);
    let err = documents::link_document(&mut conn, linked.id, Link::Entry(je.id), "invoice").unwrap_err();
    assert_eq!(documents::error_code(&err), "duplicate");
    let missing = documents::link_document(&mut conn, linked.id, Link::Entry(9999), "").unwrap_err();
    assert_eq!(documents::error_code(&missing), "not_found");

    let posting = je.postings[0].id;
    documents::link_document(&mut conn, loose.id, Link::Posting(posting), "receipt").unwrap();
    let for_entry = documents::document_links_for_entry(&conn, je.id).unwrap();
    assert_eq!(for_entry.len(), 2, "direct and posting links both evidence the entry");

    let third = documents::add_document(&mut conn, e.id, b"fictional contract", "contract.txt", "filing", "2026-02-01", serde_json::json!({})).unwrap();
    let unlinked = documents::list_documents(&conn, e.id, true, None).unwrap();
    assert_eq!(unlinked.iter().map(|d| d.uid.as_str()).collect::<Vec<_>>(), vec![third.uid.as_str()]);
    let recent = documents::list_documents(&conn, e.id, false, Some("2026-03-01")).unwrap();
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].uid, loose.uid, "newest received first");
    let audits: i64 = conn.query_row("SELECT COUNT(*) FROM audit_log WHERE table_name = 'document_links'", [], |r| r.get(0)).unwrap();
    assert_eq!(audits, 2);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn verify_detects_tampered_and_missing_files() {
    let (dir, db) = ledger("verify");
    let mut conn = db.conn();
    let e = entity(&mut conn);
    let a = documents::add_document(&mut conn, e.id, INVOICE, "invoice-0001.pdf", "upload", "2026-03-05", serde_json::json!({})).unwrap();
    let b = documents::add_document(&mut conn, e.id, b"fictional statement", "statement.csv", "import", "2026-03-31", serde_json::json!({})).unwrap();
    let clean = documents::verify_all(&conn).unwrap();
    assert!(clean.is_clean());
    assert_eq!((clean.checked, clean.passed), (2, 2));

    std::fs::write(dir.join(&a.stored_path), b"altered bytes").unwrap();
    std::fs::remove_file(dir.join(&b.stored_path)).unwrap();
    let report = documents::verify_all(&conn).unwrap();
    assert_eq!(report.passed, 0);
    assert_eq!(report.mismatched.len(), 1);
    assert_eq!(report.mismatched[0].uid, a.uid);
    assert_eq!(report.mismatched[0].actual_sha256, documents::sha256_hex(b"altered bytes"));
    assert_eq!(report.missing.len(), 1);
    assert_eq!(report.missing[0].uid, b.uid);

    let err = documents::add_document(&mut conn, e.id, INVOICE, "invoice-0001.pdf", "upload", "2026-03-05", serde_json::json!({})).unwrap_err();
    assert_eq!(documents::error_code(&err), "hash_mismatch", "a tampered file is never overwritten");
    assert_eq!(std::fs::read(dir.join(&a.stored_path)).unwrap(), b"altered bytes");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn failed_insert_leaves_no_orphan_file() {
    let (dir, db) = ledger("orphan");
    let mut conn = db.conn();
    let e = entity(&mut conn);
    conn.execute_batch("CREATE TEMP TRIGGER refuse_documents BEFORE INSERT ON documents BEGIN SELECT RAISE(ABORT, 'simulated failure'); END;").unwrap();
    let err = documents::add_document(&mut conn, e.id, INVOICE, "invoice-0001.pdf", "upload", "2026-03-05", serde_json::json!({})).unwrap_err();
    assert_eq!(documents::error_code(&err), "io_error");
    assert!(files_under(&dir.join("documents")).is_empty(), "the stored file is removed with the failed row");
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM documents", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 0);

    // A file that existed before the failed insert stays: another entity owns it.
    conn.execute_batch("DROP TRIGGER refuse_documents;").unwrap();
    let other = entities::create_entity(&mut conn, "Sample Household", "person", "PT", "EUR").unwrap();
    let kept = documents::add_document(&mut conn, other.id, INVOICE, "invoice-0001.pdf", "upload", "2026-03-05", serde_json::json!({})).unwrap();
    conn.execute_batch("CREATE TEMP TRIGGER refuse_documents BEFORE INSERT ON documents BEGIN SELECT RAISE(ABORT, 'simulated failure'); END;").unwrap();
    documents::add_document(&mut conn, e.id, INVOICE, "invoice-0001.pdf", "upload", "2026-03-05", serde_json::json!({})).unwrap_err();
    assert_eq!(std::fs::read(dir.join(&kept.stored_path)).unwrap(), INVOICE);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn rejects_path_traversal_in_filename() {
    let (dir, db) = ledger("path");
    let mut conn = db.conn();
    let e = entity(&mut conn);
    for name in ["../invoice.pdf", "..", "sub/invoice.pdf", "sub\\invoice.pdf", ""] {
        let err = documents::add_document(&mut conn, e.id, INVOICE, name, "upload", "2026-03-05", serde_json::json!({})).unwrap_err();
        assert_eq!(documents::error_code(&err), "invalid_path", "{name:?}");
    }
    assert!(!dir.join("documents").exists(), "a rejected name writes nothing");
    let err = documents::add_document(&mut conn, e.id, INVOICE, "a.pdf", "fax", "2026-03-05", serde_json::json!({})).unwrap_err();
    assert_eq!(documents::error_code(&err), "invalid_input");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn link_to_statement_line_counts_for_matched_entry() {
    let (dir, db) = ledger("line");
    let mut conn = db.conn();
    let e = entity(&mut conn);
    let je = entry(&mut conn, &e);
    let bank = je.postings[0].account_id;
    conn.execute("INSERT INTO imports(uid, source, account_id, checksum, created_at) VALUES (?1, 'test', ?2, ?1, 'now')", params![means_core::new_uid(), bank]).unwrap();
    let import = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO statement_lines(import_id, account_id, position, date, amount, currency, description, fingerprint, journal_entry_id, status)
         VALUES (?1, ?2, 0, '2026-03-04', -4250, 'EUR', 'Example Paper Supplies', 'fp-1', ?3, 'matched')",
        params![import, bank, je.id],
    )
    .unwrap();
    let line = conn.last_insert_rowid();
    let doc = documents::add_document(&mut conn, e.id, INVOICE, "invoice-0001.pdf", "email", "2026-03-05", serde_json::json!({})).unwrap();
    documents::link_document(&mut conn, doc.id, Link::StatementLine(line), "invoice").unwrap();
    let links = documents::document_links_for_entry(&conn, je.id).unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].statement_line_id, Some(line));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn memory_ledger_has_no_document_store() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let e = entity(&mut conn);
    let err = documents::add_document(&mut conn, e.id, INVOICE, "invoice.pdf", "upload", "2026-03-05", serde_json::json!({})).unwrap_err();
    assert_eq!(documents::error_code(&err), "invalid_input");
}

fn edit_input(entity: &Entity, entry: &JournalEntry, bank_minor: i64) -> EntryInput {
    let mut input = EntryInput::new(entity.id, entry.date);
    input.payee = "Example Paper Supplies Ltd".into();
    input.postings = vec![PostingInput::new(entry.postings[0].account_id, Decimal::new(bank_minor, 2)), PostingInput::balancing(entry.postings[1].account_id)];
    input
}

#[test]
fn posting_links_follow_entry_edits_and_entry_deletes() {
    let (dir, db) = ledger("lifecycle");
    let mut conn = db.conn();
    let e = entity(&mut conn);
    let je = entry(&mut conn, &e);
    let doc = documents::add_document(&mut conn, e.id, INVOICE, "invoice-0001.pdf", "upload", "2026-03-05", serde_json::json!({})).unwrap();
    let link = documents::link_document(&mut conn, doc.id, Link::Posting(je.postings[0].id), "receipt").unwrap();

    // An edit that keeps the bank leg moves the link to the replacement posting.
    let edited = journal::update_entry(&mut conn, je.id, edit_input(&e, &je, -4250)).unwrap();
    let links = documents::links_for_document(&conn, doc.id).unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].id, link.id);
    assert_eq!(links[0].posting_id, Some(edited.postings[0].id));

    // An edit that changes the bank leg keeps the evidence on the entry.
    journal::update_entry(&mut conn, je.id, edit_input(&e, &je, -5000)).unwrap();
    let links = documents::links_for_document(&conn, doc.id).unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!((links[0].entry_id, links[0].posting_id), (Some(je.id), None));

    // A deleted draft takes its links. The document returns to the unlinked list.
    let mut draft = EntryInput::new(e.id, NaiveDate::from_ymd_opt(2026, 3, 9).unwrap());
    draft.status = EntryStatus::Draft;
    draft.postings = vec![PostingInput::new(je.postings[0].account_id, Decimal::new(-100, 2)), PostingInput::balancing(je.postings[1].account_id)];
    let draft = journal::create_entry(&mut conn, draft).unwrap();
    let receipt = documents::add_document(&mut conn, e.id, b"fictional draft receipt", "draft.jpg", "upload", "2026-03-09", serde_json::json!({})).unwrap();
    documents::link_document(&mut conn, receipt.id, Link::Entry(draft.id), "").unwrap();
    documents::link_document(&mut conn, receipt.id, Link::Posting(draft.postings[1].id), "").unwrap();
    journal::delete_entry(&mut conn, draft.id).unwrap();
    let unlinked = documents::list_documents(&conn, e.id, true, None).unwrap();
    assert_eq!(unlinked.iter().map(|d| d.uid.as_str()).collect::<Vec<_>>(), vec![receipt.uid.as_str()]);
    assert!(dir.join(&receipt.stored_path).is_file(), "the document outlives the entry");
    let _ = std::fs::remove_dir_all(dir);
}
