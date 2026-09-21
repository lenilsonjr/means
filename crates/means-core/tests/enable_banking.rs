use means_core::imports::{self, enable_banking};
use means_core::model::AccountType;
use means_core::{accounts, entities, Db};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::str::FromStr;

fn transaction(reference: &str, flow: &str, amount: &str) -> Value {
    json!({"entry_reference":reference,"transaction_id":"unstable-session-id","status":"BOOK","booking_date":"2026-09-18","credit_debit_indicator":flow,"transaction_amount":{"amount":amount,"currency":"EUR"},"creditor":{"name":"Shop"},"debtor":{"name":"Employer"},"remittance_information":["Invoice 42"]})
}
fn file(transactions: Vec<Value>) -> Vec<u8> {
    serde_json::to_vec(&json!({"channel":"enable_banking","version":1,"account":{"identification_hash":"stable-account-hash","uid":"session-account-id","currency":"EUR"},"transactions":transactions}))
        .unwrap()
}

#[test]
fn preserves_decimal_precision_signs_and_stable_references() {
    let mut debit = transaction("bank-reference", "DBIT", "90071992547409.91");
    debit["balance_after_transaction"] = json!({"amount":"-10.13","currency":"EUR"});
    let bytes = file(vec![debit.clone()]);
    assert_eq!(imports::detect_source("bank.json", &bytes), enable_banking::SOURCE);
    let parsed = imports::parse(enable_banking::SOURCE, &bytes, None).unwrap();
    assert_eq!(parsed.account_ref, "stable-account-hash:EUR");
    assert_eq!(parsed.lines[0].amount, Some(Decimal::from_str("-90071992547409.91").unwrap()));
    assert_eq!(parsed.lines[0].reference, "bank-reference");
    assert_eq!(parsed.lines[0].description, "Shop Invoice 42");
    assert_eq!(parsed.lines[0].raw, debit);
    assert_eq!(parsed.closing_balance, Some(Decimal::from_str("-10.13").unwrap()));
    let credit = imports::parse(enable_banking::SOURCE, &file(vec![transaction("credit", "CRDT", "50.25")]), None).unwrap();
    assert_eq!(credit.lines[0].amount, Some(Decimal::from_str("50.25").unwrap()));
    assert_eq!(credit.lines[0].description, "Employer Invoice 42");
}

#[test]
fn pending_and_malformed_evidence_cannot_become_postings() {
    let mut pending = transaction("pending", "DBIT", "12");
    pending["status"] = json!("PDNG");
    let mut rows = vec![pending];
    for (field, value) in [
        ("status", json!("INFO")),
        ("booking_date", json!("bad-date")),
        ("credit_debit_indicator", json!("wrong")),
        ("transaction_amount", json!({"amount":1.25,"currency":"EUR"})),
        ("transaction_amount", json!({"amount":"1.25","currency":"USD"})),
    ] {
        let mut row = transaction("malformed", "DBIT", "1.25");
        row[field] = value;
        rows.push(row);
    }
    let parsed = enable_banking::parse(&file(rows)).unwrap();
    assert_eq!(parsed.skipped_records, 1);
    assert_eq!(parsed.lines.len(), 5);
    assert!(parsed.lines.iter().all(|l| l.skip.is_some()));
    assert!(parsed.period_from.is_none());
    assert!(parsed.closing_balance.is_none());
}

#[test]
fn session_transaction_id_is_never_used_as_a_deduplication_reference() {
    let mut tx = transaction("", "CRDT", "10");
    tx.as_object_mut().unwrap().remove("entry_reference");
    let parsed = enable_banking::parse(&file(vec![tx])).unwrap();
    assert!(parsed.lines[0].reference.is_empty());
    assert!(parsed.lines[0].skip.is_none());
    assert!(parsed.warnings[0].contains("no entry_reference"));
}

#[test]
fn ambiguous_same_day_balances_are_not_reported_as_closing_balances() {
    let mut a = transaction("a", "DBIT", "1");
    a["balance_after_transaction"] = json!({"amount":"99","currency":"EUR"});
    let mut b = transaction("b", "DBIT", "1");
    b["balance_after_transaction"] = json!({"amount":"98","currency":"EUR"});
    for rows in [vec![a.clone(), b.clone()], vec![b, a]] {
        assert!(enable_banking::parse(&file(rows)).unwrap().closing_balance.is_none());
    }
}

#[test]
fn rejects_missing_envelope_fields_and_unknown_currency() {
    let original: Value = serde_json::from_slice(&file(vec![])).unwrap();
    for field in ["channel", "version", "transactions", "account"] {
        let mut broken = original.clone();
        broken.as_object_mut().unwrap().remove(field);
        assert!(enable_banking::parse(&serde_json::to_vec(&broken).unwrap()).is_err());
    }
    let mut unknown = original;
    unknown["account"]["currency"] = json!("XXX");
    assert!(enable_banking::parse(&serde_json::to_vec(&unknown).unwrap()).is_err());
}

#[test]
fn repeat_fetch_after_reauthorization_is_recorded_as_duplicate_evidence() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let entity = entities::create_entity(&mut conn, "Personal", "person", "PT", "EUR").unwrap();
    let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
    let first = file(vec![transaction("stable-reference", "DBIT", "3.20")]);
    let mut renewed: Value = serde_json::from_slice(&first).unwrap();
    renewed["account"]["uid"] = json!("renewed-account-id");
    renewed["transactions"][0]["transaction_id"] = json!("renewed-transaction-id");
    renewed["transactions"][0]["remittance_information"] = json!(["Updated bank description"]);
    for (name, bytes) in [("first.json", first), ("renewed.json", serde_json::to_vec(&renewed).unwrap())] {
        imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(bank.id), name, &bytes)).unwrap();
    }
    let evidence: i64 = conn.query_row("SELECT COUNT(*) FROM statement_lines", [], |r| r.get(0)).unwrap();
    assert_eq!(evidence, 2, "both provider responses remain auditable");
    let duplicate: i64 = conn.query_row("SELECT COUNT(*) FROM statement_lines WHERE status = 'duplicate' AND duplicate_of_id IS NOT NULL", [], |r| r.get(0)).unwrap();
    assert_eq!(duplicate, 1);
    let entries: i64 = conn.query_row("SELECT COUNT(*) FROM journal_entries", [], |r| r.get(0)).unwrap();
    assert_eq!(entries, 1, "renewal must not draft a second movement");
}

#[test]
fn inbox_routes_each_account_by_its_envelope_and_never_a_source_default() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let entity = entities::create_entity(&mut conn, "Personal", "person", "PT", "EUR").unwrap();
    let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
    let dir = std::env::temp_dir().join(format!("means-enable-inbox-{}", means_core::new_uid()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("first.json"), file(vec![transaction("first", "DBIT", "3.20")])).unwrap();
    let scanned = imports::inbox::scan(&mut conn, &dir).unwrap();
    assert_eq!(scanned[0].action, "pending");
    imports::inbox::complete_import(&mut conn, scanned[0].import_id.unwrap(), bank.id).unwrap();
    assert!(imports::inbox::list_profiles(&conn).unwrap().is_empty());
    std::fs::write(dir.join("totally-different-name.json"), file(vec![transaction("second", "DBIT", "5.20")])).unwrap();
    let scanned = imports::inbox::scan(&mut conn, &dir).unwrap();
    assert_eq!(scanned[0].action, "imported");
    let mut other: Value = serde_json::from_slice(&file(vec![transaction("third", "DBIT", "7.20")])).unwrap();
    other["account"]["identification_hash"] = json!("another-bank-account");
    std::fs::write(dir.join("other.json"), serde_json::to_vec(&other).unwrap()).unwrap();
    let scanned = imports::inbox::scan(&mut conn, &dir).unwrap();
    assert_eq!(scanned[0].action, "pending", "a second bank must not inherit the first bank's route");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn session_metadata_is_saved_only_for_valid_nonempty_consents() {
    let db = Db::open_memory().unwrap();
    let conn = db.conn();
    let response = json!({"session_id":"64ee0479-2817-4bfe-a1bb-9e4bcb566e75","access":{"valid_until":"2030-01-01T00:00:00Z"},"accounts":[{"uid":"account"}]});
    for (field, value) in [("session_id", json!("invalid")), ("access", json!({"valid_until":"invalid"})), ("access", json!({"valid_until":"2000-01-01T00:00:00Z"})), ("accounts", json!([]))] {
        let mut invalid = response.clone();
        invalid[field] = value;
        assert!(enable_banking::save_session(&conn, "Bank", "PT", &invalid).is_err());
        assert!(enable_banking::sessions(&conn).unwrap().is_empty());
    }
    let saved = enable_banking::save_session(&conn, "Bank", "PT", &response).unwrap();
    assert_eq!(saved.valid_until, "2030-01-01T00:00:00Z");
    assert_eq!(enable_banking::sessions(&conn).unwrap().len(), 1);
}

#[test]
fn publication_preserves_renewed_account_routes_and_separates_currencies() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let entity = entities::create_entity(&mut conn, "Personal", "person", "PT", "EUR").unwrap();
    let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
    // Existing manual evidence has already established the account route.
    imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(bank.id), "manual.json", &file(vec![transaction("existing", "DBIT", "3.20")]))).unwrap();
    let dir = std::env::temp_dir().join(format!("means-enable-publish-{}", means_core::new_uid()));
    let account = json!({"uid":"new-session-account","name":"Personal","currency":"XXX"});
    let mut usd = transaction("usd", "CRDT", "7.31");
    usd["transaction_amount"]["currency"] = json!("USD");
    let hashes = vec!["stable-account-hash".into(), "secondary-hash".into()];
    let files = enable_banking::publish_account(&mut conn, &dir, "session-2", &account, &hashes, vec![transaction("existing", "DBIT", "3.20"), usd], false).unwrap();
    assert_eq!(files.len(), 2);
    let eur = files.iter().find(|f| f.currency == "EUR").unwrap();
    assert_eq!(eur.account_ref, "stable-account-hash:EUR");
    let bytes = std::fs::read(eur.path.as_ref().unwrap()).unwrap();
    assert_eq!(enable_banking::account_for_file(&conn, &bytes).unwrap(), Some(bank.id));
    let raw: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(raw["provider_account"], account);
    assert_eq!(raw["transactions"][0]["transaction_amount"]["amount"], "3.20");
    let usd = files.iter().find(|f| f.currency == "USD").unwrap();
    assert_eq!(enable_banking::account_for_file(&conn, &std::fs::read(usd.path.as_ref().unwrap()).unwrap()).unwrap(), None);
    // A later session keeps the route through the same primary hash.
    let next = enable_banking::publish_account(&mut conn, &dir, "session-3", &json!({"currency":"EUR"}), &["stable-account-hash".into()], vec![], false).unwrap();
    assert_eq!(next[0].account_ref, "stable-account-hash:EUR");
    let cursor: String = conn.query_row("SELECT cursor FROM channel_connections WHERE provider_account_id = 'stable-account-hash:EUR'", [], |r| r.get(0)).unwrap();
    assert!(cursor.is_empty(), "full-history pulls have no incremental cursor");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn dry_run_and_failed_publication_leave_no_connection_state() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let dir = std::env::temp_dir().join(format!("means-enable-dry-{}", means_core::new_uid()));
    let account = json!({"currency":"EUR"});
    let files = enable_banking::publish_account(&mut conn, &dir, "session", &account, &["hash".into()], vec![transaction("id", "DBIT", "2")], true).unwrap();
    assert!(files[0].path.is_none());
    assert!(!dir.exists());
    let count = || conn.query_row::<i64, _, _>("SELECT COUNT(*) FROM channel_connections", [], |r| r.get(0)).unwrap();
    assert_eq!(count(), 0);
    // A file at the requested directory path makes publication impossible.
    std::fs::write(&dir, b"occupied").unwrap();
    assert!(enable_banking::publish_account(&mut conn, &dir, "session", &account, &["hash".into()], vec![transaction("id", "DBIT", "2")], false).is_err());
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM channel_connections", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 0);
    std::fs::remove_file(dir).unwrap();
}

#[test]
fn shared_secondary_hashes_do_not_merge_account_routes() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let entity = entities::create_entity(&mut conn, "Personal", "person", "PT", "EUR").unwrap();
    let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
    let dir = std::env::temp_dir().join(format!("means-enable-distinct-{}", means_core::new_uid()));
    let account = json!({"currency":"EUR"});
    let first = enable_banking::publish_account(&mut conn, &dir, "session", &account, &["first-account".into(), "shared".into()], vec![], false).unwrap();
    let first_content = std::fs::read(first[0].path.as_ref().unwrap()).unwrap();
    imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(bank.id), "first.json", &first_content)).unwrap();
    // Older versions persisted secondary hashes as routing aliases.
    conn.execute("INSERT INTO enable_banking_account_aliases (hash, currency, connection_id) SELECT 'shared', 'EUR', id FROM channel_connections WHERE provider_account_id = 'first-account:EUR'", [])
        .unwrap();
    conn.execute(
        "INSERT INTO enable_banking_account_aliases (hash, currency, connection_id) SELECT 'second-account', 'EUR', id FROM channel_connections WHERE provider_account_id = 'first-account:EUR'",
        [],
    )
    .unwrap();
    let second = enable_banking::publish_account(&mut conn, &dir, "session", &account, &["second-account".into(), "shared".into(), "first-account".into()], vec![], false).unwrap();
    assert_eq!(first[0].account_ref, "first-account:EUR");
    assert_eq!(second[0].account_ref, "second-account:EUR");
    let bytes = std::fs::read(second[0].path.as_ref().unwrap()).unwrap();
    assert_eq!(enable_banking::account_for_file(&conn, &bytes).unwrap(), None);
    assert_eq!(enable_banking::account_for_file(&conn, &first_content).unwrap(), Some(bank.id));
    let raw: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(raw["identification_hashes"], json!(["second-account", "shared", "first-account"]));
    let again = enable_banking::publish_account(&mut conn, &dir, "renewed", &account, &["second-account".into()], vec![], true).unwrap();
    assert_eq!(again[0].account_ref, "second-account:EUR");
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM channel_connections", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 2);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn publication_requires_a_primary_hash_before_writing() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let dir = std::env::temp_dir().join(format!("means-enable-no-primary-{}", means_core::new_uid()));
    for hashes in [vec![], vec![String::new(), "secondary".into()], vec![" ".into(), "secondary".into()]] {
        assert!(enable_banking::publish_account(&mut conn, &dir, "session", &json!({"currency":"EUR"}), &hashes, vec![], false).is_err());
    }
    assert!(!dir.exists());
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM channel_connections", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 0);
}

#[cfg(unix)]
#[test]
fn published_evidence_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let dir = std::env::temp_dir().join(format!("means-enable-private-{}", means_core::new_uid()));
    let files = enable_banking::publish_account(&mut conn, &dir, "session", &json!({"currency":"EUR"}), &["account".into()], vec![], false).unwrap();
    let mode = std::fs::metadata(files[0].path.as_ref().unwrap()).unwrap().permissions().mode();
    assert_eq!(mode & 0o077, 0, "group and other users must not have access");
    assert_ne!(mode & 0o600, 0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn distinct_references_survive_partial_pulls_with_identical_fingerprints() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let entity = entities::create_entity(&mut conn, "Demo", "person", "PT", "EUR").unwrap();
    let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
    let charge = transaction("charge-a", "DBIT", "5.49");
    let reversal = transaction("reversal-a", "CRDT", "5.49");
    let recharge = transaction("charge-b", "DBIT", "5.49");
    let first = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(bank.id), "first.json", &file(vec![charge.clone(), reversal.clone()]))).unwrap();
    assert_eq!(first.import.created_count, 2);
    // A later partial pull restarts the fingerprint occurrence counter at zero.
    let next = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(bank.id), "next.json", &file(vec![recharge.clone()]))).unwrap();
    assert_eq!(next.lines[0].fingerprint, first.lines[0].fingerprint);
    assert_eq!(next.import.created_count, 1);
    assert_eq!(next.import.duplicate_count, 0);
    assert_ne!(next.lines[0].journal_entry_id, first.lines[0].journal_entry_id);
    // The same movements in a different order remain idempotent by reference.
    let again = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(bank.id), "again.json", &file(vec![recharge.clone(), reversal, charge]))).unwrap();
    assert_eq!(again.import.created_count, 0);
    assert_eq!(again.import.duplicate_count, 3);
    let total: i64 = conn.query_row("SELECT SUM(quantity) FROM postings WHERE account_id = ?1", [bank.id], |r| r.get(0)).unwrap();
    assert_eq!(total, -549);
    let other = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Other"], "bank", "EUR").unwrap();
    let separate = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(other.id), "other.json", &file(vec![recharge]))).unwrap();
    assert_eq!(separate.import.created_count, 1, "references are scoped to the ledger account");
}

#[test]
fn missing_references_keep_fingerprint_deduplication() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let entity = entities::create_entity(&mut conn, "Demo", "person", "PT", "EUR").unwrap();
    let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
    let mut row = transaction("", "DBIT", "5.49");
    let first = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(bank.id), "first.json", &file(vec![row.clone()]))).unwrap();
    assert_eq!(first.import.created_count, 1);
    row["transaction_id"] = json!("different-session-id");
    let repeat = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(bank.id), "repeat.json", &file(vec![row.clone()]))).unwrap();
    assert_eq!(repeat.import.duplicate_count, 1);
    row["entry_reference"] = json!("new-stable-reference");
    let identified = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(bank.id), "identified.json", &file(vec![row]))).unwrap();
    assert_eq!(identified.import.created_count, 1, "a new reference must not be suppressed by reference-free evidence");
}

#[test]
fn repeated_reference_free_groups_preserve_each_occurrence_without_errors() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let entity = entities::create_entity(&mut conn, "Demo", "person", "PT", "EUR").unwrap();
    let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
    // Same shape as the report: 52 repeated groups, 111 movements, no bank IDs.
    let mut rows = Vec::new();
    for group in 0..52 {
        let mut row = transaction("", "DBIT", &format!("{}.49", group + 1));
        row.as_object_mut().unwrap().remove("entry_reference");
        for _ in 0..if group < 7 { 3 } else { 2 } {
            rows.push(row.clone());
        }
    }
    let mut content = file(rows);
    let first = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(bank.id), "groups.json", &content)).unwrap();
    assert_eq!(first.import.created_count, 111);
    assert_eq!(first.import.error_count, 0);
    assert_eq!(first.import.duplicate_count, 0);
    let fingerprints: std::collections::HashSet<_> = first.lines.iter().map(|line| &line.fingerprint).collect();
    assert_eq!(fingerprints.len(), 111, "occurrence numbers distinguish identical rows within a file");
    content.push(b'\n'); // Different file checksum; same bank movements.
    let again = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(bank.id), "again.json", &content)).unwrap();
    assert_eq!(again.import.duplicate_count, 111);
    assert_eq!(again.import.created_count, 0);
    assert_eq!(again.import.error_count, 0);
}

#[test]
fn voided_reference_falls_through_to_live_match_or_new_draft() {
    use means_core::{journal, EntryInput, EntryStatus, PostingInput};
    for with_survivor in [false, true] {
        let db = Db::open_memory().unwrap();
        let mut conn = db.conn();
        let entity = entities::create_entity(&mut conn, "Demo", "person", "PT", "EUR").unwrap();
        let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
        let expense = accounts::ensure_account(&conn, entity.id, AccountType::Expense, &["Food"], "expense", "EUR").unwrap();
        let mut content = file(vec![transaction("void-ref", "DBIT", "5.49")]);
        let first = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(bank.id), "first.json", &content)).unwrap();
        let old_id = first.lines[0].journal_entry_id.unwrap();
        journal::post_entry(&mut conn, old_id).unwrap();
        let voided = journal::void_entry(&mut conn, old_id, None, "duplicate").unwrap();
        let old_snapshot = serde_json::to_value(&voided).unwrap();
        let survivor = if with_survivor {
            let mut input = EntryInput::new(entity.id, "2026-09-18".parse().unwrap());
            input.postings = vec![PostingInput::new(bank.id, Decimal::new(-549, 2)), PostingInput::balancing(expense.id)];
            Some(journal::create_entry(&mut conn, input).unwrap().id)
        } else {
            None
        };
        content.push(b'\n');
        let next = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(bank.id), "next.json", &content)).unwrap();
        assert_eq!(next.import.error_count, 0, "{:?}", next.lines);
        let linked = next.lines[0].journal_entry_id.unwrap();
        assert_ne!(linked, old_id);
        assert_eq!(next.lines[0].reference, "void-ref");
        if let Some(survivor) = survivor {
            assert_eq!(linked, survivor);
            assert_eq!(next.import.matched_count, 1);
        } else {
            assert_eq!(next.import.created_count, 1);
            assert_eq!(journal::get_entry(&conn, linked).unwrap().status, EntryStatus::Draft);
        }
        assert_eq!(serde_json::to_value(journal::get_entry(&conn, old_id).unwrap()).unwrap(), old_snapshot);
        let retry = imports::retry_import(&mut conn, first.import.id).unwrap();
        assert_eq!(retry.error_count, 0);
        assert_eq!(retry.duplicate_count, 1);
        // An actual bank credit must not reconcile to the synthetic void reversal.
        let credit = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(bank.id), "credit.json", &file(vec![transaction("real-credit", "CRDT", "5.49")]))).unwrap();
        assert_eq!(credit.import.created_count, 1);
        assert_ne!(credit.lines[0].journal_entry_id, voided.reversed_by_id);
    }
}

#[test]
fn failed_void_reference_can_be_retried_or_reviewed_without_editing_history() {
    use means_core::{journal, EntryStatus};
    for manual in [false, true] {
        let db = Db::open_memory().unwrap();
        let mut conn = db.conn();
        let entity = entities::create_entity(&mut conn, "Demo", "person", "PT", "EUR").unwrap();
        let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
        let expense = accounts::ensure_account(&conn, entity.id, AccountType::Expense, &["Food"], "expense", "EUR").unwrap();
        let first = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(bank.id), "first.json", &file(vec![transaction("held-ref", "DBIT", "5.49")]))).unwrap();
        let id = first.lines[0].journal_entry_id.unwrap();
        journal::post_entry(&mut conn, id).unwrap();
        let voided = journal::void_entry(&mut conn, id, None, "duplicate").unwrap();
        // Persist the state left by the older binary after its failed attachment.
        conn.execute("UPDATE statement_lines SET status='error', note='invalid: a void entry cannot receive statement evidence' WHERE id=?1", [first.lines[0].id]).unwrap();
        if manual {
            means_core::rules::create_entry_from_line(&mut conn, first.lines[0].id, Some(expense.id), "", None, false, &[]).unwrap();
        } else {
            let retry = imports::retry_import(&mut conn, first.import.id).unwrap();
            assert_eq!(retry.created_count, 1);
            assert_eq!(retry.error_count, 0);
        }
        let line = imports::get_line(&conn, first.lines[0].id).unwrap();
        assert_eq!(line.status, "created");
        assert_eq!(line.reference, "held-ref");
        let new = journal::get_entry(&conn, line.journal_entry_id.unwrap()).unwrap();
        assert_eq!(new.status, EntryStatus::Draft);
        assert_ne!(new.id, id);
        let bank_posting = new.postings.iter().find(|p| p.account_id == bank.id).unwrap();
        assert!(bank_posting.external_id.is_none(), "the unique reference remains on the voided history");
        assert_eq!(bank_posting.id, line.posting_id.unwrap());
        assert_eq!(serde_json::to_value(journal::get_entry(&conn, id).unwrap()).unwrap(), serde_json::to_value(voided).unwrap());
    }
}
