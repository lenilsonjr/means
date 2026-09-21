use means_core::imports::{self, mercury};
use means_core::{accounts, entities, model::AccountType, Db};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::str::FromStr;

const ACCOUNT: &str = "11111111-1111-4111-8111-111111111111";
const TRANSACTION: &str = "22222222-2222-4222-8222-222222222222";
fn transaction() -> Value {
    json!({"id":TRANSACTION,"accountId":ACCOUNT,"amount":-3.21,"status":"sent","postedAt":"2020-01-02T00:00:00Z","createdAt":"2019-12-20T00:00:00Z","counterpartyName":"Shop","bankDescription":"Purchase","externalMemo":"Invoice 42"})
}
fn file(rows: Vec<Value>) -> Vec<u8> {
    serde_json::to_vec(&json!({"channel":"mercury","version":1,"currency":"USD","account":{"id":ACCOUNT,"currentBalance":1234},"transactions":rows})).unwrap()
}

#[test]
fn uses_exact_signed_numbers_and_posted_date_without_inventing_balances() {
    let mut tx = transaction();
    tx["amount"] = serde_json::from_str("-90071992547409.91").unwrap();
    let content = file(vec![tx.clone()]);
    assert_eq!(imports::detect_source("bank.json", &content), mercury::SOURCE);
    let parsed = imports::parse(mercury::SOURCE, &content, None).unwrap();
    assert_eq!(parsed.lines[0].amount, Some(Decimal::from_str("-90071992547409.91").unwrap()));
    assert_eq!(parsed.lines[0].reference, TRANSACTION);
    assert_eq!(parsed.lines[0].description, "Shop Purchase Invoice 42");
    assert_eq!(parsed.lines[0].raw, tx);
    assert_eq!(parsed.period_from.unwrap().to_string(), "2020-01-02");
    assert!(parsed.closing_balance.is_none());
    assert!(parsed.lines[0].balance_after.is_none());
}

#[test]
fn nonbooked_and_malformed_records_cannot_post() {
    let mut rows = Vec::new();
    for (key, value) in [
        ("status", json!("pending")),
        ("status", json!("reversed")),
        ("status", json!("failed")),
        ("status", json!("cancelled")),
        ("status", json!("blocked")),
        ("status", json!("unexpected")),
        ("postedAt", Value::Null),
        ("postedAt", json!("2020-01-02")),
        ("id", json!("")),
        ("amount", json!("1.25")),
        ("amount", json!(1.001)),
        ("amount", json!(0)),
    ] {
        let mut tx = transaction();
        tx[key] = value;
        rows.push(tx);
    }
    let out = mercury::parse(&file(rows)).unwrap();
    assert_eq!(out.skipped_records, 1);
    assert_eq!(out.lines.len(), 11);
    assert!(out.lines.iter().all(|l| l.skip.is_some()));
    assert!(out.period_from.is_none());
    let mut wrong_account = transaction();
    wrong_account["accountId"] = json!("different");
    assert!(mercury::parse(&file(vec![wrong_account])).is_err());
}

#[test]
fn pending_then_booked_then_refetched_creates_one_movement() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let entity = entities::create_entity(&mut conn, "Company", "company", "US", "USD").unwrap();
    let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank"], "bank", "USD").unwrap();
    let mut pending = transaction();
    pending["status"] = json!("pending");
    pending["postedAt"] = Value::Null;
    let mut updated = transaction();
    updated["bankDescription"] = json!("Corrected description");
    for (name, row) in [("pending.json", pending), ("booked.json", transaction()), ("again.json", updated)] {
        imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(bank.id), name, &file(vec![row]))).unwrap();
    }
    let entries: i64 = conn.query_row("SELECT COUNT(*) FROM journal_entries", [], |r| r.get(0)).unwrap();
    assert_eq!(entries, 1);
    let duplicate: i64 = conn.query_row("SELECT COUNT(*) FROM statement_lines WHERE status = 'duplicate'", [], |r| r.get(0)).unwrap();
    assert_eq!(duplicate, 1);
}

#[test]
fn inbox_routes_from_account_identity_not_filename_or_source_default() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let entity = entities::create_entity(&mut conn, "Company", "company", "US", "USD").unwrap();
    let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank"], "bank", "USD").unwrap();
    let dir = std::env::temp_dir().join(format!("means-mercury-inbox-{}", means_core::new_uid()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("first.json"), file(vec![transaction()])).unwrap();
    let scanned = imports::inbox::scan(&mut conn, &dir).unwrap();
    assert_eq!(scanned[0].action, "pending");
    imports::inbox::complete_import(&mut conn, scanned[0].import_id.unwrap(), bank.id).unwrap();
    assert!(imports::inbox::list_profiles(&conn).unwrap().is_empty());
    let mut changed = transaction();
    changed["id"] = json!("33333333-3333-4333-8333-333333333333");
    std::fs::write(dir.join("renamed.json"), file(vec![changed])).unwrap();
    assert_eq!(imports::inbox::scan(&mut conn, &dir).unwrap()[0].action, "imported");
    let mut other: Value = serde_json::from_slice(&file(vec![])).unwrap();
    other["account"]["id"] = json!("44444444-4444-4444-8444-444444444444");
    std::fs::write(dir.join("other.json"), serde_json::to_vec(&other).unwrap()).unwrap();
    assert_eq!(imports::inbox::scan(&mut conn, &dir).unwrap()[0].action, "pending");
    let euro = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Euro"], "bank", "EUR").unwrap();
    assert!(imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(euro.id), "wrong.json", &file(vec![transaction()]))).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn publication_is_validated_atomic_and_preserves_learned_routes() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let entity = entities::create_entity(&mut conn, "Company", "company", "US", "USD").unwrap();
    let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank"], "bank", "USD").unwrap();
    imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(bank.id), "learn.json", &file(vec![transaction()]))).unwrap();
    let dir = std::env::temp_dir().join(format!("means-mercury-publish-{}", means_core::new_uid()));
    let account = json!({"id":ACCOUNT,"kind":"checking","name":"Operating","currentBalance":1234});
    let dry = mercury::publish_account(&mut conn, &dir, &account, vec![transaction()], true).unwrap();
    assert!(dry.path.is_none());
    assert!(!dir.exists());
    let count = || conn.query_row("SELECT COUNT(*) FROM channel_connections WHERE last_pull_at <> ''", [], |r| r.get::<_, i64>(0)).unwrap();
    assert_eq!(count(), 0);
    let mut wrong = transaction();
    wrong["accountId"] = json!("bad");
    assert!(mercury::publish_account(&mut conn, &dir, &account, vec![wrong], false).is_err());
    assert!(!dir.exists());
    std::fs::write(&dir, b"not a directory").unwrap();
    assert!(mercury::publish_account(&mut conn, &dir, &account, vec![transaction()], false).is_err());
    let last_pull: String = conn.query_row("SELECT last_pull_at FROM channel_connections", [], |r| r.get(0)).unwrap();
    assert!(last_pull.is_empty());
    std::fs::remove_file(&dir).unwrap();
    let published = mercury::publish_account(&mut conn, &dir, &account, vec![transaction()], false).unwrap();
    let content = std::fs::read(published.path.unwrap()).unwrap();
    let envelope: Value = serde_json::from_slice(&content).unwrap();
    assert_eq!(envelope["account"], account);
    assert_eq!(envelope["transactions"][0], transaction());
    assert_eq!(mercury::account_for_file(&conn, &content).unwrap(), Some(bank.id));
    let state: (String, String) = conn.query_row("SELECT cursor, last_pull_at FROM channel_connections", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert!(state.0.is_empty());
    assert!(!state.1.is_empty());
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    std::fs::remove_dir_all(dir).unwrap();
}

fn credit_file(rows: Vec<Value>) -> Vec<u8> {
    let mut value: Value = serde_json::from_slice(&file(rows)).unwrap();
    value["account_type"] = json!("credit");
    serde_json::to_vec(&value).unwrap()
}

#[test]
fn credit_movements_keep_ledger_signs_and_require_a_usd_liability() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let entity = entities::create_entity(&mut conn, "Company", "company", "US", "USD").unwrap();
    let card = accounts::ensure_account(&conn, entity.id, AccountType::Liability, &["IO"], "credit_card", "USD").unwrap();
    let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank"], "bank", "USD").unwrap();
    let euro = accounts::ensure_account(&conn, entity.id, AccountType::Liability, &["EUR card"], "credit_card", "EUR").unwrap();
    let mut rows = Vec::new();
    for (n, kind, amount) in [(1, "creditCardTransaction", -100), (2, "creditCardCredit", 20), (3, "internalTransfer", 80), (4, "cardInternationalTransactionFee", -1)] {
        let mut row = transaction();
        row["id"] = json!(format!("00000000-0000-4000-8000-{n:012}"));
        row["kind"] = json!(kind);
        row["amount"] = json!(amount);
        rows.push(row);
    }
    let content = credit_file(rows.clone());
    for account in [bank.id, euro.id] {
        assert!(imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(account), "io.json", &content)).is_err());
    }
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM imports", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    let result = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(card.id), "io.json", &content)).unwrap();
    assert_eq!(result.import.created_count, 4);
    for (line, original) in result.lines.iter().zip(&rows) {
        assert_eq!(line.amount.unwrap().major(), Decimal::from_str(&original["amount"].to_string()).unwrap());
        assert_eq!(line.raw, *original);
    }
    let quantity: i64 = conn.query_row("SELECT SUM(quantity) FROM postings WHERE account_id = ?1", [card.id], |r| r.get(0)).unwrap();
    assert_eq!(quantity, -100, "USD 1 owed after charges, refund, payment and fee");
    let mut replay: Value = serde_json::from_slice(&content).unwrap();
    replay["pulled_at"] = json!("2026-09-19T00:00:00Z");
    let replay = serde_json::to_vec(&replay).unwrap();
    let result = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(card.id), "again.json", &replay)).unwrap();
    assert_eq!(result.import.duplicate_count, 4);
    assert_eq!(result.import.created_count, 0);
}

#[test]
fn credit_pending_settlement_and_payment_match_do_not_double_book() {
    use means_core::{
        journal,
        model::{EntryInput, PostingInput},
    };
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let e = entities::create_entity(&mut conn, "Company", "company", "US", "USD").unwrap();
    let card = accounts::ensure_account(&conn, e.id, AccountType::Liability, &["IO"], "credit_card", "USD").unwrap();
    let bank = accounts::ensure_account(&conn, e.id, AccountType::Asset, &["Bank"], "bank", "USD").unwrap();
    let mut pending = transaction();
    pending["status"] = json!("pending");
    pending["postedAt"] = Value::Null;
    let first = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(card.id), "pending.json", &credit_file(vec![pending]))).unwrap();
    assert_eq!(first.import.created_count, 0);
    let settled = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(card.id), "settled.json", &credit_file(vec![transaction()]))).unwrap();
    assert_eq!(settled.import.created_count, 1);
    let mut input = EntryInput::new(e.id, "2020-01-02".parse().unwrap());
    input.postings = vec![PostingInput::new(bank.id, Decimal::from(-50)), PostingInput::new(card.id, Decimal::from(50))];
    let payment = journal::create_entry(&mut conn, input).unwrap();
    let mut row = transaction();
    row["id"] = json!("33333333-3333-4333-8333-333333333333");
    row["amount"] = json!(50);
    row["kind"] = json!("internalTransfer");
    let result = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(card.id), "payment.json", &credit_file(vec![row]))).unwrap();
    assert_eq!(result.import.matched_count, 1);
    assert_eq!(result.import.created_count, 0);
    assert_eq!(result.lines[0].journal_entry_id, Some(payment.id));
}

#[test]
fn credit_publication_marks_family_and_rejects_an_asset_route() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let e = entities::create_entity(&mut conn, "Company", "company", "US", "USD").unwrap();
    let bank = accounts::ensure_account(&conn, e.id, AccountType::Asset, &["Bank"], "bank", "USD").unwrap();
    let card = accounts::ensure_account(&conn, e.id, AccountType::Liability, &["IO"], "credit_card", "USD").unwrap();
    imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(bank.id), "old.json", &file(vec![]))).unwrap();
    let dir = std::env::temp_dir().join(format!("means-io-{}", means_core::new_uid()));
    let account = json!({"id":ACCOUNT,"status":"active","createdAt":"2020-01-01T00:00:00Z","availableBalance":-3.21,"currentBalance":-3.21});
    mercury::publish_credit_account(&mut conn, &dir, &account, vec![transaction()], true).unwrap();
    assert!(!dir.exists());
    let published = mercury::publish_credit_account(&mut conn, &dir, &account, vec![transaction()], false).unwrap();
    let content = std::fs::read(published.path.unwrap()).unwrap();
    let value: Value = serde_json::from_slice(&content).unwrap();
    assert_eq!(value["account"], account);
    assert_eq!(value["account_type"], "credit");
    assert_eq!(mercury::account_for_file(&conn, &content).unwrap(), None);
    let scan = imports::inbox::scan(&mut conn, &dir).unwrap();
    assert_eq!(scan[0].action, "pending");
    imports::inbox::complete_import(&mut conn, scan[0].import_id.unwrap(), card.id).unwrap();
    assert_eq!(mercury::account_for_file(&conn, &content).unwrap(), Some(card.id));
    assert_eq!(conn.query_row("SELECT provider_type FROM channel_connections WHERE provider_account_id = ?1", [ACCOUNT], |r| r.get::<_, String>(0)).unwrap(), "credit");
    let mut malformed = value;
    malformed["account_type"] = json!("investment");
    assert!(mercury::parse(&serde_json::to_vec(&malformed).unwrap()).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}
