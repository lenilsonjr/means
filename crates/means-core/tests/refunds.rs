use chrono::NaiveDate;
use means_core::{accounts, entities, hashchain, imports, journal, model::*, rates, refunds, rules, tags, Db};
use rusqlite::{params, Connection};
use rust_decimal::Decimal;
use std::str::FromStr;
fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}
fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}
fn setup(conn: &mut Connection, foreign: bool) -> (Entity, Account, Account, Account, JournalEntry) {
    let entity = entities::create_entity(conn, "Personal", "person", "PT", "EUR").unwrap();
    let bank = accounts::ensure_account(conn, entity.id, AccountType::Asset, &["Bank"], "bank", if foreign { "USD" } else { "EUR" }).unwrap();
    let first = accounts::ensure_account(conn, entity.id, AccountType::Expense, &["First"], "expense", "EUR").unwrap();
    let second = accounts::ensure_account(conn, entity.id, AccountType::Expense, &["Second"], "expense", "EUR").unwrap();
    if foreign {
        rates::set_price(conn, "USD", "EUR", date("2026-01-01"), d("0.9"), "manual").unwrap();
        rates::set_price(conn, "USD", "EUR", date("2026-02-01"), d("0.92"), "manual").unwrap();
    }
    let mut input = EntryInput::new(entity.id, date("2026-01-01"));
    input.payee = "Shop".into();
    input.postings = vec![PostingInput::new(bank.id, d("-50")), PostingInput::new(first.id, d("30")), PostingInput::balancing(second.id)];
    let original = journal::create_entry(conn, input).unwrap();
    let mut original_tags = tags::parse("purpose:travel").unwrap();
    original_tags.push(("trip".into(), "Summer break".into()));
    tags::set_tags(conn, original.id, &original_tags).unwrap();
    conn.execute("UPDATE postings SET reconciled_at = '2026-01-02' WHERE journal_entry_id = ?1 AND account_id = ?2", params![original.id, bank.id]).unwrap();
    (entity, bank, first, second, journal::get_entry(conn, original.id).unwrap())
}
fn line(conn: &Connection, bank: &Account, minor: i64, on: &str) -> i64 {
    conn.execute("INSERT INTO imports(uid, source, account_id, checksum, created_at) VALUES (?1, 'test', ?2, ?1, 'now')", params![means_core::new_uid(), bank.id]).unwrap();
    let id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO statement_lines(import_id, account_id, position, date, amount, currency, description, fingerprint) VALUES (?1, ?2, 0, ?3, ?4, ?5, 'Refund from Shop', ?6)",
        params![id, bank.id, on, minor, bank.commodity, format!("line-{id}")],
    )
    .unwrap();
    conn.last_insert_rowid()
}
#[test]
fn full_refund_preserves_locked_purchase_reverses_splits_and_separates_fx() {
    for foreign in [false, true] {
        let db = Db::open_memory().unwrap();
        let mut conn = db.conn();
        let (entity, bank, first, second, original) = setup(&mut conn, foreign);
        conn.execute("UPDATE entities SET lock_date = '2026-01-31' WHERE id = ?1", [entity.id]).unwrap();
        let credit = line(&conn, &bank, 5000, "2026-02-01");
        assert_eq!(refunds::candidates(&conn, credit, 90).unwrap()[0].id, original.id);
        let refund = refunds::link(&mut conn, credit, original.id).unwrap();
        assert_eq!(refund.refund_of_id, Some(original.id));
        assert_eq!(refund.reverses_id, None);
        assert_eq!(refund.tags, original.tags);
        assert_eq!(refund.kind, "refund");
        assert_eq!(serde_json::to_value(journal::get_entry(&conn, original.id).unwrap()).unwrap(), serde_json::to_value(&original).unwrap());
        for account in [first.id, second.id] {
            let total: i64 = conn.query_row("SELECT SUM(amount) FROM postings WHERE account_id = ?1", [account], |r| r.get(0)).unwrap();
            assert_eq!(total, 0, "the category must return to zero, not become income");
        }
        if foreign {
            let fx = accounts::find_by_role(&conn, entity.id, "fx_gain_loss").unwrap();
            assert_eq!(refund.postings.iter().find(|p| p.account_id == fx.id).unwrap().amount.minor(), -100);
        }
        assert!(hashchain::verify(&conn, entity.id).unwrap().first_bad_seq.is_none());
        assert!(refunds::link(&mut conn, credit, original.id).is_err());
        let second_credit = line(&conn, &bank, 5000, "2026-02-02");
        assert!(refunds::candidates(&conn, second_credit, 90).unwrap().is_empty());
        assert!(refunds::link(&mut conn, second_credit, original.id).is_err());
    }
}
#[test]
fn replaces_only_the_credit_suspense_draft_atomically() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let (entity, bank, _, _, original) = setup(&mut conn, false);
    let credit = line(&conn, &bank, 5000, "2026-02-01");
    let mut sl = imports::get_line(&conn, credit).unwrap();
    rules::draft_from_line(&mut conn, &bank, &mut sl, &[]).unwrap();
    let draft = sl.journal_entry_id.unwrap();
    let before = serde_json::to_value(journal::get_entry(&conn, draft).unwrap()).unwrap();
    conn.execute_batch("CREATE TEMP TRIGGER fail_refund BEFORE UPDATE OF status ON statement_lines BEGIN SELECT RAISE(ABORT, 'test failure'); END;").unwrap();
    assert!(refunds::link(&mut conn, credit, original.id).is_err());
    assert_eq!(serde_json::to_value(journal::get_entry(&conn, draft).unwrap()).unwrap(), before);
    assert_eq!(imports::get_line(&conn, credit).unwrap().journal_entry_id, Some(draft));
    assert_eq!(journal::list_entries(&conn, &journal::EntryFilter::default()).unwrap().1, 2);
    conn.execute_batch("DROP TRIGGER fail_refund").unwrap();
    let refund = refunds::link(&mut conn, credit, original.id).unwrap();
    assert!(journal::get_entry(&conn, draft).is_err());
    assert_eq!(imports::get_line(&conn, credit).unwrap().journal_entry_id, Some(refund.id));
    assert!(hashchain::verify(&conn, entity.id).unwrap().first_bad_seq.is_none());
}
#[test]
fn rejects_wrong_amount_account_date_and_locked_refund_without_changes() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let (entity, bank, _, _, original) = setup(&mut conn, false);
    let other_entity = entities::create_entity(&mut conn, "Other person", "person", "PT", "EUR").unwrap();
    let other = accounts::ensure_account(&conn, other_entity.id, AccountType::Asset, &["Other"], "bank", "EUR").unwrap();
    for (account, amount, on) in [(&bank, 4900, "2026-02-01"), (&bank, -5000, "2026-02-01"), (&bank, 5000, "2025-12-31"), (&other, 5000, "2026-02-01")] {
        let credit = line(&conn, account, amount, on);
        assert!(refunds::link(&mut conn, credit, original.id).is_err());
        assert_eq!(imports::get_line(&conn, credit).unwrap().status, "unmatched");
    }
    conn.execute("UPDATE entities SET lock_date = '2026-02-02' WHERE id = ?1", [entity.id]).unwrap();
    let credit = line(&conn, &bank, 5000, "2026-02-01");
    assert!(refunds::link(&mut conn, credit, original.id).is_err());
    assert_eq!(journal::list_entries(&conn, &journal::EntryFilter::default()).unwrap().1, 1);
}

#[test]
fn imports_offer_refunds_before_income_rules_and_retain_duplicate_evidence() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let (entity, bank, _, _, original) = setup(&mut conn, false);
    let income = accounts::ensure_account(&conn, entity.id, AccountType::Income, &["Other income"], "income", "EUR").unwrap();
    conn.execute("INSERT INTO rules(entity_id, name, conditions, account_id, created_at) VALUES (?1, 'Broad credit rule', '[]', ?2, 'now')", params![entity.id, income.id]).unwrap();
    let bytes = b"Date,Amount,Description,Reference\n2026-02-01,50,Refund from Shop,refund-1\n";
    let mapping = imports::CsvMapping {
        date_column: "Date".into(),
        date_format: "%Y-%m-%d".into(),
        amount_column: "Amount".into(),
        description_column: "Description".into(),
        reference_column: "Reference".into(),
        currency: "EUR".into(),
        ..Default::default()
    };
    let mut request = imports::ImportRequest::new("generic_csv", Some(bank.id), "refund.csv", bytes);
    request.mapping = Some(&mapping);
    let imported = imports::run_import(&mut conn, request).unwrap();
    let sl = &imported.lines[0];
    let draft = journal::get_entry(&conn, sl.journal_entry_id.unwrap()).unwrap();
    assert_eq!(draft.status, EntryStatus::Draft);
    assert!(draft.postings.iter().all(|p| p.account_id != income.id));
    let refund = refunds::link(&mut conn, sl.id, original.id).unwrap();
    let again = b"Date,Amount,Description,Reference\n2026-02-01,50,Changed bank description,refund-1\n";
    let mut request = imports::ImportRequest::new("generic_csv", Some(bank.id), "again.csv", again);
    request.mapping = Some(&mapping);
    let duplicate = imports::run_import(&mut conn, request).unwrap();
    assert_eq!(duplicate.lines[0].status, "duplicate");
    assert_eq!(journal::list_entries(&conn, &journal::EntryFilter::default()).unwrap().1, 2);
    let export = means_core::export::beancount(&mut conn).unwrap();
    assert!(export.contains(&format!("means-refund-of-id: {}", original.id)));
    assert_eq!(journal::get_entry(&conn, refund.id).unwrap().refund_of_id, Some(original.id));
}

#[test]
fn full_refund_can_arrive_in_another_same_currency_account_of_the_entity() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let (entity, original_bank, _, _, original) = setup(&mut conn, false);
    let destination = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["New bank"], "bank", "EUR").unwrap();
    let credit = line(&conn, &destination, 5000, "2026-02-01");
    assert_eq!(refunds::candidates(&conn, credit, 90).unwrap()[0].id, original.id);
    let refund = refunds::link(&mut conn, credit, original.id).unwrap();
    assert!(refund.postings.iter().any(|p| p.account_id == destination.id && p.quantity.minor() == 5000));
    assert!(refund.postings.iter().all(|p| p.account_id != original_bank.id));
}

#[test]
fn missing_refund_rate_is_an_error_instead_of_a_guessed_fx_gain() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let (_, bank, _, _, original) = setup(&mut conn, true);
    conn.execute("DELETE FROM prices", []).unwrap();
    let credit = line(&conn, &bank, 5000, "2026-02-01");
    let error = refunds::link(&mut conn, credit, original.id).unwrap_err();
    assert!(error.to_string().contains("exchange rate"));
    assert_eq!(imports::get_line(&conn, credit).unwrap().status, "unmatched");
    assert_eq!(journal::list_entries(&conn, &journal::EntryFilter::default()).unwrap().1, 1);
}

#[test]
fn signed_discount_splits_reverse_exactly_and_remain_a_refund() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let (entity, bank, first, second, _) = setup(&mut conn, false);
    let mut input = EntryInput::new(entity.id, date("2026-01-02"));
    input.postings = vec![PostingInput::new(bank.id, d("-50")), PostingInput::new(first.id, d("60")), PostingInput::new(second.id, d("-10"))];
    let original = journal::create_entry(&mut conn, input).unwrap();
    let credit = line(&conn, &bank, 5000, "2026-02-01");
    let refund = refunds::link(&mut conn, credit, original.id).unwrap();
    assert_eq!(refund.kind, "refund");
    assert_eq!(refund.postings.iter().find(|p| p.account_id == first.id).unwrap().amount.minor(), -6000);
    assert_eq!(refund.postings.iter().find(|p| p.account_id == second.id).unwrap().amount.minor(), 1000);
}

#[test]
fn rounded_splits_keep_their_quantities_and_values_on_void_and_refund() {
    for (unit, quantity, amount) in [("EUR", "0", "0"), ("USD", "0.01", "0"), ("JPY", "0", "0.01")] {
        let db = Db::open_memory().unwrap();
        let mut conn = db.conn();
        let entity = entities::create_entity(&mut conn, "Personal", "person", "PT", "EUR").unwrap();
        let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
        let split = accounts::ensure_account(&conn, entity.id, AccountType::Expense, &["Split"], "expense", unit).unwrap();
        let rest = accounts::ensure_account(&conn, entity.id, AccountType::Expense, &["Rest"], "expense", "EUR").unwrap();
        if unit == "JPY" {
            rates::set_price(&conn, "EUR", "JPY", date("2026-01-01"), d("1"), "manual").unwrap();
        }
        let mut input = EntryInput::new(entity.id, date("2026-01-01"));
        let split_input = if unit == "USD" {
            let mut posting = PostingInput::new(split.id, d(quantity));
            posting.amount = Some(d(amount));
            posting
        } else {
            PostingInput::valued(split.id, d(amount), "EUR")
        };
        input.postings = vec![PostingInput::new(bank.id, d("-0.02")), split_input, PostingInput::balancing(rest.id)];
        let original = journal::create_entry(&mut conn, input.clone()).unwrap();
        let to_void = journal::create_entry(&mut conn, input).unwrap();
        let voided = journal::void_entry(&mut conn, to_void.id, None, "correct purchase").unwrap();
        let reversal = journal::get_entry(&conn, voided.reversed_by_id.unwrap()).unwrap();
        let credit = line(&conn, &bank, 2, "2026-02-01");
        let refund = refunds::link(&mut conn, credit, original.id).unwrap();
        for entry in [reversal, refund] {
            let posting = entry.postings.iter().find(|p| p.account_id == split.id).unwrap();
            assert_eq!(posting.quantity.major(), -d(quantity));
            assert_eq!(posting.amount.major(), -d(amount));
        }
        assert!(hashchain::verify(&conn, entity.id).unwrap().first_bad_seq.is_none());
    }
}
