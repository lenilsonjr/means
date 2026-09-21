use means_core::rusqlite::Connection;
use means_core::{accounts, entities, hashchain, imports, journal, model::*, Db};

struct Fixture {
    db: Db,
    entity: i64,
    bank: i64,
    food: i64,
    import: i64,
    line: i64,
    source: i64,
    kept: i64,
}
fn fixture() -> Fixture {
    fixture_with_status(EntryStatus::Posted)
}
fn fixture_with_status(source_status: EntryStatus) -> Fixture {
    let db = Db::open_memory().unwrap();
    let (entity, bank, food, import, line, source, kept) = {
        let mut c = db.conn();
        let e = entities::create_entity(&mut c, "Personal", "person", "PT", "EUR").unwrap();
        let bank = accounts::ensure_account(&c, e.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
        let food = accounts::ensure_account(&c, e.id, AccountType::Expense, &["Food"], "expense", "EUR").unwrap();
        let other = accounts::ensure_account(&c, e.id, AccountType::Expense, &["Other"], "expense", "EUR").unwrap();
        let csv = b"Booking Date,Value Date,Partner Name,Amount (EUR)\n2026-09-01,2026-09-01,Cafe,-10.00\n";
        let imported = imports::run_import(&mut c, imports::ImportRequest::new("n26_csv", Some(bank.id), "bank.csv", csv)).unwrap();
        let source = imported.lines[0].journal_entry_id.unwrap();
        let date = means_core::parse_date("2026-09-01").unwrap();
        let mut categorized = EntryInput::new(e.id, date);
        categorized.status = source_status;
        categorized.postings = vec![PostingInput::new(bank.id, "-10".parse().unwrap()), PostingInput::balancing(food.id)];
        journal::update_entry(&mut c, source, categorized).unwrap();
        let mut input = EntryInput::new(e.id, date);
        input.status = EntryStatus::Draft;
        input.postings = vec![PostingInput::new(bank.id, "-10".parse().unwrap()), PostingInput::balancing(other.id)];
        let kept = journal::create_entry(&mut c, input).unwrap();
        // Ensure replacement postings cannot receive their old SQLite IDs.
        let mut later = EntryInput::new(e.id, date);
        later.postings = vec![PostingInput::new(bank.id, "-99".parse().unwrap()), PostingInput::balancing(other.id)];
        journal::create_entry(&mut c, later).unwrap();
        (e.id, bank.id, food.id, imported.import.id, imported.lines[0].id, source, kept.id)
    };
    Fixture { db, entity, bank, food, import, line, source, kept }
}
fn state(c: &Connection, f: &Fixture) -> serde_json::Value {
    serde_json::json!({
        "source": journal::get_entry(c, f.source).unwrap(),
        "kept": journal::get_entry(c, f.kept).unwrap(),
        "import": imports::get_import(c, f.import).unwrap(),
        "audit": c.query_row("SELECT COUNT(*) FROM audit_log", [], |r|r.get::<_,i64>(0)).unwrap(),
    })
}

#[test]
fn category_carry_uses_replacement_posting_and_commits_evidence_and_counters() {
    let f = fixture();
    let mut c = f.db.conn();
    let old_bank = journal::get_entry(&c, f.kept).unwrap().postings.iter().find(|p| p.account_id == f.bank).unwrap().id;
    let result = imports::rematch_import(&mut c, f.import).unwrap();
    assert_eq!((result.rematched, result.category_kept), (1, 1), "{:?}", result.notes);
    assert!(journal::get_entry(&c, f.source).is_err());
    let kept = journal::get_entry(&c, f.kept).unwrap();
    assert_eq!(kept.status, EntryStatus::Posted);
    assert!(kept.postings.iter().any(|p| p.account_id == f.food));
    let bank = kept.postings.iter().find(|p| p.account_id == f.bank).unwrap();
    assert_ne!(bank.id, old_bank);
    let line = imports::get_line(&c, f.line).unwrap();
    assert_eq!((line.status.as_str(), line.posting_id, line.journal_entry_id), ("matched", Some(bank.id), Some(f.kept)));
    assert_eq!(line.amount, Some(bank.quantity));
    let import = imports::get_import(&c, f.import).unwrap().0;
    assert_eq!((import.created_count, import.matched_count), (0, 1));
    assert!(hashchain::verify(&c, f.entity).unwrap().first_bad_seq.is_none());
    assert_eq!(imports::rematch_import(&mut c, f.import).unwrap().rematched, 0);
}

#[test]
fn failures_after_category_update_or_source_deletion_leave_everything_unchanged() {
    for trigger in [
        "CREATE TRIGGER fail_attach BEFORE UPDATE OF status ON statement_lines WHEN NEW.status='matched' BEGIN SELECT RAISE(ABORT, 'attachment failure'); END;",
        "CREATE TRIGGER fail_delete BEFORE INSERT ON audit_log WHEN NEW.action='delete' BEGIN SELECT RAISE(ABORT, 'audit failure'); END;",
        "CREATE TRIGGER fail_counts BEFORE UPDATE OF matched_count ON imports BEGIN SELECT RAISE(ABORT, 'counter failure'); END;",
        "CREATE TRIGGER fail_summary BEFORE INSERT ON audit_log WHEN NEW.action='rematch' BEGIN SELECT RAISE(ABORT, 'summary failure'); END;",
    ] {
        let f = fixture();
        let mut c = f.db.conn();
        let before = state(&c, &f);
        c.execute_batch(trigger).unwrap();
        let result = imports::rematch_import(&mut c, f.import);
        if let Ok(report) = result {
            assert_eq!((report.rematched, report.category_kept, report.untouched), (0, 0, 1));
            assert!(!report.notes.is_empty());
            // The pass records its failed attempt, but no entry audit changes survive.
            c.execute("DELETE FROM audit_log WHERE action='rematch'", []).unwrap();
        }
        assert_eq!(state(&c, &f), before);
        assert!(hashchain::verify(&c, f.entity).unwrap().first_bad_seq.is_none());
    }
}

#[test]
fn rematch_preserves_source_with_other_statement_evidence() {
    let f = fixture();
    let mut c = f.db.conn();
    c.execute("INSERT INTO statement_lines (import_id, account_id, position, journal_entry_id, status) VALUES (?1, ?2, 2, ?3, 'matched')", [f.import, f.bank, f.source]).unwrap();
    let before = state(&c, &f);
    let report = imports::rematch_import(&mut c, f.import).unwrap();
    assert_eq!(report.rematched, 0);
    assert!(report.notes[0].contains("other statement evidence"));
    c.execute("DELETE FROM audit_log WHERE action='rematch'", []).unwrap();
    assert_eq!(state(&c, &f), before);
}

#[test]
fn rematch_does_not_flatten_a_categorized_split() {
    for status in [EntryStatus::Draft, EntryStatus::Posted] {
        let f = fixture_with_status(status);
        let mut c = f.db.conn();
        let other = accounts::ensure_account(&c, f.entity, AccountType::Expense, &["Split"], "expense", "EUR").unwrap();
        let entry = journal::get_entry(&c, f.source).unwrap();
        let mut input = EntryInput::new(f.entity, entry.date);
        input.status = status;
        input.postings = vec![PostingInput::new(f.bank, "-10".parse().unwrap()), PostingInput::new(f.food, "6".parse().unwrap()), PostingInput::new(other.id, "4".parse().unwrap())];
        journal::update_entry(&mut c, f.source, input).unwrap();
        let before = state(&c, &f);
        let report = imports::rematch_import(&mut c, f.import).unwrap();
        assert_eq!(report.rematched, 0);
        assert!(report.notes[0].contains("manual review"));
        c.execute("DELETE FROM audit_log WHERE action='rematch'", []).unwrap();
        assert_eq!(state(&c, &f), before);
    }
}
