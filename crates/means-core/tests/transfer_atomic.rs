use means_core::rusqlite::Connection;
use means_core::{accounts, entities, hashchain, journal, model::*, reports, Db};

fn fixture() -> (Db, journal::Transfer, i64, i64) {
    let db = Db::open_memory().unwrap();
    let (transfer, from, to) = {
        let mut conn = db.conn();
        let from = entities::create_entity(&mut conn, "Cedar", "company", "", "USD").unwrap();
        let to = entities::create_entity(&mut conn, "Maple", "company", "", "USD").unwrap();
        let bank_a = accounts::ensure_account(&conn, from.id, AccountType::Asset, &["Bank"], "bank", "USD").unwrap();
        let bank_b = accounts::ensure_account(&conn, to.id, AccountType::Asset, &["Bank"], "bank", "USD").unwrap();
        let transfer = journal::Transfer {
            date: means_core::parse_date("2026-01-10").unwrap(),
            from_account_id: bank_a.id,
            to_account_id: bank_b.id,
            from_quantity: "100".parse().unwrap(),
            to_quantity: None,
            fee: None,
            fee_account_id: None,
            payee: String::new(),
            notes: String::new(),
            from_contra_account_id: None,
            to_contra_account_id: None,
            status: EntryStatus::Posted,
            origin: String::new(),
            external: None,
        };
        (transfer, from.id, to.id)
    };
    (db, transfer, from, to)
}

fn state(conn: &Connection) -> serde_json::Value {
    let (entries, _) = journal::list_entries(conn, &journal::EntryFilter { limit: 100, ..Default::default() }).unwrap();
    let audits: i64 = conn.query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0)).unwrap();
    serde_json::json!({"entries": entries, "audits": audits})
}

#[test]
fn transfer_failure_preserves_both_books_and_audit_records() {
    for trigger in [
        "CREATE TRIGGER fail_second BEFORE INSERT ON journal_entries WHEN NEW.counterpart_id IS NOT NULL BEGIN SELECT RAISE(ABORT, 'second entry failed'); END;",
        "CREATE TRIGGER fail_link BEFORE UPDATE OF counterpart_id ON journal_entries BEGIN SELECT RAISE(ABORT, 'link failed'); END;",
        "CREATE TRIGGER fail_audit BEFORE INSERT ON audit_log WHEN NEW.table_name='journal_entries' AND (SELECT COUNT(*) FROM journal_entries)=2 BEGIN SELECT RAISE(ABORT, 'audit failed'); END;",
    ] {
        let (db, transfer, from, to) = fixture();
        let mut conn = db.conn();
        conn.execute_batch(trigger).unwrap();
        let before = state(&conn);
        assert!(journal::create_transfer(&mut conn, transfer).is_err());
        assert_eq!(state(&conn), before);
        for entity in [from, to] {
            assert_eq!(hashchain::verify(&conn, entity).unwrap().checked, 0);
            assert!(reports::trial_balance(&conn, entity, None).unwrap().rows.is_empty());
        }
    }
}

#[test]
fn locked_destination_preserves_source_and_audit_records() {
    let (db, transfer, _, to) = fixture();
    let mut conn = db.conn();
    entities::update_entity(&mut conn, to, "", "", "", Some(transfer.date), false).unwrap();
    let before = state(&conn);
    assert!(journal::create_transfer(&mut conn, transfer).is_err());
    assert_eq!(state(&conn), before);
}

#[test]
fn transfer_commits_reciprocal_links_and_balanced_books() {
    for status in [EntryStatus::Draft, EntryStatus::Posted] {
        let (db, mut transfer, from, to) = fixture();
        let mut conn = db.conn();
        transfer.status = status;
        let (a, b) = journal::create_transfer(&mut conn, transfer).unwrap();
        let b = b.unwrap();
        assert_eq!((a.counterpart_id, b.counterpart_id), (Some(b.id), Some(a.id)));
        assert_eq!((a.status, b.status), (status, status));
        assert_eq!(a.postings[0].amount.minor(), -10_000);
        assert_eq!(b.postings[0].amount.minor(), 10_000);
        for entity in [from, to] {
            let chain = hashchain::verify(&conn, entity).unwrap();
            assert!(chain.first_bad_seq.is_none());
            assert_eq!(chain.checked, usize::from(status == EntryStatus::Posted));
            assert!(reports::trial_balance(&conn, entity, None).unwrap().net.is_zero());
        }
    }
}
