use means_core::rusqlite::Connection;
use means_core::{accounts, db, entities, hashchain, journal, model::*, reports, tags};

fn fixture() -> (Connection, i64, Vec<i64>) {
    let mut c = Connection::open_in_memory().unwrap();
    db::migrate_to(&c, 11).unwrap();
    // Seed legacy tag contents using current journal helpers. Payee metadata is
    // independent of the tag repairs under test; do not advance their migration version.
    c.execute_batch(include_str!("../migrations/0016_payees.sql")).unwrap();
    let e = entities::create_entity(&mut c, "Personal", "person", "PT", "EUR").unwrap();
    let bank = accounts::ensure_account(&c, e.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
    let expense = accounts::ensure_account(&c, e.id, AccountType::Expense, &["Food"], "expense", "EUR").unwrap();
    let mut input = EntryInput::new(e.id, means_core::parse_date("2026-09-01").unwrap());
    input.postings = vec![PostingInput::new(bank.id, "-5".parse().unwrap()), PostingInput::new(expense.id, "5".parse().unwrap())];
    let original = journal::create_entry(&mut c, input).unwrap();
    tags::set_tags(&mut c, original.id, &[("trip".into(), "porto".into())]).unwrap();
    let mut ids = vec![original.id];
    for _ in 0..4 {
        let parent = *ids.last().unwrap();
        journal::void_entry(&mut c, parent, None, "test").unwrap();
        let id: i64 = c.query_row("SELECT id FROM journal_entries WHERE reverses_id=?1", [parent], |r| r.get(0)).unwrap();
        // Reproduce pre-fix voids, without recording a user tag edit.
        c.execute("DELETE FROM entry_tags WHERE entry_id=?1", [id]).unwrap();
        ids.push(id);
    }
    (c, e.id, ids)
}

#[test]
fn migration_13_repairs_already_migrated_chains_and_preserves_hashes() {
    let (c, entity, ids) = fixture();
    db::migrate_to(&c, 12).unwrap();
    assert!(tags::strings_for(&c, ids[2]).unwrap().is_empty());
    let before: Vec<String> = c.prepare("SELECT hash FROM journal_entries ORDER BY id").unwrap().query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
    db::migrate_to(&c, 13).unwrap();
    for id in &ids {
        assert_eq!(tags::strings_for(&c, *id).unwrap(), ["trip:porto"]);
    }
    let total = reports::expenses_by_class(&c, entity, None, None, None).unwrap().total;
    assert_eq!(total.major().to_string(), "5");
    assert_eq!(reports::expenses_by_class(&c, entity, None, None, Some("trip:porto")).unwrap().total, total);
    let after: Vec<String> = c.prepare("SELECT hash FROM journal_entries ORDER BY id").unwrap().query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
    assert_eq!(before, after);
    assert!(hashchain::verify(&c, entity).unwrap().first_bad_seq.is_none());
    db::migrate_to(&c, 13).unwrap();
    for id in &ids[2..] {
        let history = means_core::audit::history(&c, "journal_entries", *id).unwrap();
        let changes: Vec<_> = history.iter().filter(|a| a.action == "repair_reversal_tags").collect();
        assert_eq!(changes.len(), 1);
        assert!(changes[0].after.as_ref().unwrap().contains("\"migration\":13"));
    }
}

#[test]
fn manual_clear_stops_propagation_and_audit_failure_rolls_back_whole_chain() {
    let (mut c, _, ids) = fixture();
    tags::set_tags(&mut c, ids[2], &[("manual".into(), String::new())]).unwrap();
    tags::set_tags(&mut c, ids[2], &[]).unwrap();
    db::migrate_to(&c, 13).unwrap();
    assert_eq!(tags::strings_for(&c, ids[1]).unwrap(), ["trip:porto"]);
    for id in &ids[2..] {
        assert!(tags::strings_for(&c, *id).unwrap().is_empty());
    }

    let (c, _, ids) = fixture();
    db::migrate_to(&c, 12).unwrap();
    c.execute_batch(&format!("CREATE TRIGGER fail_chain BEFORE INSERT ON audit_log WHEN NEW.action='repair_reversal_tags' AND NEW.row_id={} BEGIN SELECT RAISE(ABORT, 'test failure'); END;", ids[3]))
        .unwrap();
    assert!(db::migrate_to(&c, 13).is_err());
    assert_eq!(c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).unwrap(), 12);
    for id in &ids[2..] {
        assert!(tags::strings_for(&c, *id).unwrap().is_empty());
    }
    c.execute_batch("DROP TRIGGER fail_chain").unwrap();
    db::migrate_to(&c, 13).unwrap();
    for id in &ids {
        assert_eq!(tags::strings_for(&c, *id).unwrap(), ["trip:porto"]);
    }
}
