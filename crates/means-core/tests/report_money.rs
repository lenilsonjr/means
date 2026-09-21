use chrono::NaiveDate;
use means_core::model::*;
use means_core::money::Money;
use means_core::{accounts, entities, journal, rates, reports, Db};
use rust_decimal::Decimal;
use std::str::FromStr;

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}
fn day(n: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, n).unwrap()
}
fn eur(s: &str) -> Money {
    Money::from_major(d(s), "EUR", 2).unwrap()
}

#[test]
fn converted_accounts_round_before_parent_and_consolidated_totals() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    rates::set_price(&conn, "USD", "EUR", day(1), d("0.5"), "manual").unwrap();
    for name in ["Personal", "Business"] {
        let entity = entities::create_entity(&mut conn, name, "person", "US", "USD").unwrap();
        for leaf in ["A", "B"] {
            let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Banks", leaf], "bank", "USD").unwrap();
            let income = accounts::ensure_account(&conn, entity.id, AccountType::Income, &["Sales", leaf], "income", "USD").unwrap();
            let mut entry = EntryInput::new(entity.id, day(1));
            entry.postings = vec![PostingInput::new(bank.id, d("0.01")), PostingInput::new(income.id, d("-0.01"))];
            journal::create_entry(&mut conn, entry).unwrap();
        }
    }
    let report = reports::balance_sheet(&conn, None, Some(day(2)), "EUR").unwrap();
    // Every 0.005 EUR account rounds to zero (ties to even), even though
    // converting the aggregate would produce 0.02 EUR across the two entities.
    assert_eq!(report.net, eur("0"));
    assert_eq!(report.total_debit, eur("0"));
    assert_eq!(report.summary.len(), 2);
    for row in report.rows.iter().chain(&report.summary) {
        assert_eq!(row.amount, eur("0"));
        assert_eq!(row.debit, eur("0"));
        assert_eq!(row.credit, eur("0"));
    }
    assert_eq!(report.rows.iter().filter(|r| r.name == "Retained earnings").count(), 2);
    assert!(report.rows.iter().any(|r| r.quantity.commodity() == "USD" && r.quantity.minor() == 2));
}

#[test]
fn converted_reports_use_registered_target_precision() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let entity = entities::create_entity(&mut conn, "Personal", "person", "PT", "EUR").unwrap();
    let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
    let equity = accounts::ensure_account(&conn, entity.id, AccountType::Equity, &["Opening"], "equity", "EUR").unwrap();
    let mut entry = EntryInput::new(entity.id, day(1));
    entry.postings = vec![PostingInput::new(bank.id, d("1")), PostingInput::new(equity.id, d("-1"))];
    journal::create_entry(&mut conn, entry).unwrap();
    for (code, precision, rate, minor) in [("JPY", 0, "1.5", 2), ("BTC", 8, "0.000000015", 2), ("TEST", 3, "0.1235", 124)] {
        entities::create_commodity(&conn, code, "currency", code, Some(precision), "").unwrap();
        rates::set_price(&conn, "EUR", code, day(1), d(rate), "manual").unwrap();
        let report = reports::balance_sheet(&conn, Some(entity.id), Some(day(2)), code).unwrap();
        let expected = Money::from_minor(minor, code, precision).unwrap();
        assert_eq!(report.net, expected);
        assert_eq!(report.rows.iter().find(|r| r.account_id == bank.id).unwrap().amount, expected);
        assert_eq!(report.summary[0].amount, expected);
    }
}

#[test]
fn market_rollups_include_cash_before_and_after_priced_children() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let entity = entities::create_entity(&mut conn, "Personal", "person", "PT", "EUR").unwrap();
    let cash_a = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Portfolio", "Nested", "A cash"], "bank", "EUR").unwrap();
    let btc = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Portfolio", "Nested", "BTC"], "holding", "BTC").unwrap();
    let cash_z = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Portfolio", "Z cash"], "bank", "EUR").unwrap();
    let equity = accounts::ensure_account(&conn, entity.id, AccountType::Equity, &["Opening"], "equity", "EUR").unwrap();
    rates::set_price(&conn, "BTC", "EUR", day(1), d("100000"), "manual").unwrap();
    let mut entry = EntryInput::new(entity.id, day(1));
    entry.postings = vec![PostingInput::new(cash_a.id, d("20")), PostingInput::new(btc.id, d("0.001")), PostingInput::new(cash_z.id, d("30")), PostingInput::new(equity.id, d("-150"))];
    journal::create_entry(&mut conn, entry).unwrap();
    rates::set_price(&conn, "BTC", "EUR", day(2), d("150000"), "manual").unwrap();
    let report = reports::balance_sheet(&conn, Some(entity.id), Some(day(2)), "EUR").unwrap();
    let parent = report.rows.iter().find(|r| r.name == "Portfolio").unwrap();
    assert_eq!(parent.amount, eur("150"));
    assert_eq!(parent.market_value, Some(eur("200")));
    let nested = report.rows.iter().find(|r| r.name == "Nested").unwrap();
    assert_eq!(nested.amount, eur("120"));
    assert_eq!(nested.market_value, Some(eur("170")));
    assert_eq!(report.net, eur("200"));
    assert_eq!(report.rows.iter().find(|r| r.name == "Unrealised gains").unwrap().amount, eur("50"));
    let holding = report.rows.iter().find(|r| r.account_id == btc.id).unwrap();
    assert_eq!(holding.quantity, Money::from_minor(100_000, "BTC", 8).unwrap());
    assert_eq!(holding.amount, eur("100"));
}

#[test]
fn ledger_keeps_account_units_separate_from_book_value_and_excludes_drafts_from_balance() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let entity = entities::create_entity(&mut conn, "Personal", "person", "PT", "EUR").unwrap();
    let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Japan"], "bank", "JPY").unwrap();
    let equity = accounts::ensure_account(&conn, entity.id, AccountType::Equity, &["Opening"], "equity", "EUR").unwrap();
    rates::set_price(&conn, "JPY", "EUR", day(1), d("0.006"), "manual").unwrap();
    for (on, quantity, amount, status) in [(1, "500", "-3", EntryStatus::Posted), (2, "-100", "0.6", EntryStatus::Posted), (3, "1000", "-6", EntryStatus::Draft)] {
        let mut entry = EntryInput::new(entity.id, day(on));
        entry.status = status;
        entry.postings = vec![PostingInput::new(bank.id, d(quantity)), PostingInput::new(equity.id, d(amount))];
        journal::create_entry(&mut conn, entry).unwrap();
    }
    let report = reports::general_ledger(&conn, bank.id, Some(day(2)), Some(day(3)), 100, true).unwrap();
    assert_eq!(report.opening_balance, Money::from_minor(500, "JPY", 0).unwrap());
    assert_eq!(report.closing_balance, Money::from_minor(400, "JPY", 0).unwrap());
    assert_eq!(report.rows.len(), 2);
    assert_eq!(report.rows[0].credit, Money::from_minor(100, "JPY", 0).unwrap());
    assert_eq!(report.rows[0].amount, eur("-0.6"));
    assert_eq!(report.rows[1].debit, Money::from_minor(1000, "JPY", 0).unwrap());
    assert_eq!(report.rows[1].amount, eur("6"));
    assert_eq!(report.rows[0].running_balance, report.rows[1].running_balance);
}

#[test]
fn aggregation_overflow_returns_an_error() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let entity = entities::create_entity(&mut conn, "Personal", "person", "PT", "EUR").unwrap();
    let large = Money::from_minor(i64::MAX, "EUR", 2).unwrap().major();
    for leaf in ["A", "B"] {
        let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Banks", leaf], "bank", "EUR").unwrap();
        let equity = accounts::ensure_account(&conn, entity.id, AccountType::Equity, &["Opening", leaf], "equity", "EUR").unwrap();
        let mut entry = EntryInput::new(entity.id, day(1));
        entry.postings = vec![PostingInput::new(bank.id, large), PostingInput::new(equity.id, -large)];
        journal::create_entry(&mut conn, entry).unwrap();
    }
    assert!(reports::balance_sheet(&conn, Some(entity.id), Some(day(2)), "EUR").is_err());
    assert!(reports::trial_balance(&conn, entity.id, None).is_err());
}

#[test]
fn class_expenses_use_book_values_exact_tags_and_each_posting_once() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let e = entities::create_entity(&mut conn, "Personal", "person", "PT", "EUR").unwrap();
    let bank = accounts::ensure_account(&conn, e.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
    let fixed = accounts::ensure_account(&conn, e.id, AccountType::Expense, &["Home", "Rent"], "expense", "EUR").unwrap();
    let food = accounts::ensure_account(&conn, e.id, AccountType::Expense, &["Food"], "expense", "JPY").unwrap();
    let other = accounts::ensure_account(&conn, e.id, AccountType::Expense, &["Other"], "expense", "EUR").unwrap();
    accounts::update_account(&conn, fixed.id, accounts::AccountUpdate { class: Some("fixed".into()), ..Default::default() }).unwrap();
    accounts::update_account(&conn, food.id, accounts::AccountUpdate { class: Some("committed".into()), ..Default::default() }).unwrap();
    rates::set_price(&conn, "JPY", "EUR", day(1), d("0.01"), "manual").unwrap();
    let mut input = EntryInput::new(e.id, day(2));
    input.postings = vec![PostingInput::new(bank.id, d("-13")), PostingInput::new(fixed.id, d("10")), PostingInput::new(food.id, d("200")), PostingInput::new(other.id, d("1"))];
    let purchase = journal::create_entry(&mut conn, input).unwrap();
    means_core::tags::set_tags(&mut conn, purchase.id, &means_core::tags::parse("trip:porto with:friends").unwrap()).unwrap();
    let mut input = EntryInput::new(e.id, day(3));
    input.postings = vec![PostingInput::new(bank.id, d("2")), PostingInput::new(food.id, d("-200"))];
    let refund = journal::create_entry(&mut conn, input).unwrap();
    means_core::tags::set_tags(&mut conn, refund.id, &means_core::tags::parse("trip:porto").unwrap()).unwrap();
    let mut input = EntryInput::new(e.id, day(2));
    input.status = EntryStatus::Draft;
    input.postings = vec![PostingInput::new(bank.id, d("-99")), PostingInput::new(other.id, d("99"))];
    journal::create_entry(&mut conn, input).unwrap();
    rates::set_price(&conn, "JPY", "EUR", day(4), d("0.02"), "manual").unwrap();
    let report = reports::expenses_by_class(&conn, e.id, Some(day(2)), Some(day(2)), Some("#TRIP:PORTO")).unwrap();
    assert_eq!(report.total, eur("13"));
    assert_eq!(report.rows.iter().map(|r| (r.class.as_str(), r.amount)).collect::<Vec<_>>(), vec![("fixed", eur("10")), ("committed", eur("2")), ("", eur("1"))]);
    assert_eq!(report.tag.as_deref(), Some("trip:porto"));
    assert_eq!(reports::expenses_by_class(&conn, e.id, None, None, None).unwrap().total, eur("11"));
    assert_eq!(reports::expenses_by_class(&conn, e.id, None, None, Some("trip:port")).unwrap().total, eur("0"));
    assert_eq!(reports::expenses_by_class(&conn, e.id, None, None, Some("trip")).unwrap().total, eur("0"));
    assert!(reports::expenses_by_class(&conn, e.id, Some(day(3)), Some(day(2)), None).is_err());
    assert!(reports::expenses_by_class(&conn, e.id, None, None, Some("trip:porto with:friends")).is_err());
    let other_entity = entities::create_entity(&mut conn, "Business", "company", "PT", "EUR").unwrap();
    assert_eq!(reports::expenses_by_class(&conn, other_entity.id, None, None, None).unwrap().total, eur("0"));
    assert!(reports::expenses_by_class(&conn, i64::MAX, None, None, None).is_err());
    let json = serde_json::to_value(report).unwrap();
    assert_eq!(json["total"], serde_json::json!({"minor":"1300", "commodity":"EUR", "precision":2}));
}

#[test]
fn void_preserves_tagged_totals_and_rolls_back_the_whole_operation_on_failure() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let e = entities::create_entity(&mut conn, "Personal", "person", "PT", "EUR").unwrap();
    let bank = accounts::ensure_account(&conn, e.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
    let expense = accounts::ensure_account(&conn, e.id, AccountType::Expense, &["Food"], "expense", "EUR").unwrap();
    let mut input = EntryInput::new(e.id, day(1));
    input.postings = vec![PostingInput::new(bank.id, d("-5")), PostingInput::new(expense.id, d("5"))];
    let entry = journal::create_entry(&mut conn, input).unwrap();
    means_core::tags::set_tags(&mut conn, entry.id, &[("trip".into(), "porto".into()), ("Exact Case".into(), "with spaces".into())]).unwrap();
    conn.execute_batch("CREATE TRIGGER fail_void BEFORE UPDATE OF status ON journal_entries WHEN NEW.status = 'void' BEGIN SELECT RAISE(ABORT, 'test failure'); END;").unwrap();
    assert!(journal::void_entry(&mut conn, entry.id, Some(day(2)), "test").is_err());
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM journal_entries", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    assert_eq!(journal::get_entry(&conn, entry.id).unwrap().status, EntryStatus::Posted);
    conn.execute_batch("DROP TRIGGER fail_void").unwrap();
    journal::void_entry(&mut conn, entry.id, Some(day(2)), "test").unwrap();
    let reversal: i64 = conn.query_row("SELECT id FROM journal_entries WHERE reverses_id = ?1", [entry.id], |r| r.get(0)).unwrap();
    assert_eq!(means_core::tags::strings_for(&conn, reversal).unwrap(), means_core::tags::strings_for(&conn, entry.id).unwrap());
    assert_eq!(reports::expenses_by_class(&conn, e.id, None, None, Some("trip:porto")).unwrap().total, eur("0"));
    assert_eq!(reports::expenses_by_class(&conn, e.id, Some(day(2)), Some(day(2)), Some("trip:porto")).unwrap().total, eur("-5"));
    assert!(means_core::hashchain::verify(&conn, e.id).unwrap().first_bad_seq.is_none());
}

#[test]
fn legacy_reversal_tag_repair_is_audited_atomic_and_preserves_edits() {
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    means_core::db::migrate_to(&conn, 11).unwrap();
    // Seed legacy tag contents using current journal helpers. Payee metadata is
    // independent of the tag repairs under test; do not advance their migration version.
    conn.execute_batch(include_str!("../migrations/0016_payees.sql")).unwrap();
    let e = entities::create_entity(&mut conn, "Personal", "person", "PT", "EUR").unwrap();
    let bank = accounts::ensure_account(&conn, e.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
    let expense = accounts::ensure_account(&conn, e.id, AccountType::Expense, &["Food"], "expense", "EUR").unwrap();
    let mut reversals = Vec::new();
    for i in 0..4 {
        let mut input = EntryInput::new(e.id, day(1));
        input.postings = vec![PostingInput::new(bank.id, d("-5")), PostingInput::new(expense.id, d("5"))];
        let entry = journal::create_entry(&mut conn, input).unwrap();
        means_core::tags::set_tags(&mut conn, entry.id, &[("trip".into(), "porto".into())]).unwrap();
        journal::void_entry(&mut conn, entry.id, Some(day(2)), "test").unwrap();
        let reversal: i64 = conn.query_row("SELECT id FROM journal_entries WHERE reverses_id = ?1", [entry.id], |r| r.get(0)).unwrap();
        // Emulate old void behavior: no tags were copied and no tag edit occurred.
        conn.execute("DELETE FROM entry_tags WHERE entry_id = ?1", [reversal]).unwrap();
        if i == 1 || i == 2 {
            means_core::tags::set_tags(&mut conn, reversal, &[("manual".into(), "".into())]).unwrap();
        }
        if i == 2 {
            means_core::tags::set_tags(&mut conn, reversal, &[]).unwrap();
        }
        if i == 3 {
            conn.execute("INSERT INTO entry_tags VALUES (?1, 'existing', '')", [reversal]).unwrap();
        }
        reversals.push(reversal);
    }
    conn.execute_batch("CREATE TRIGGER fail_repair BEFORE INSERT ON audit_log WHEN NEW.action = 'repair_reversal_tags' BEGIN SELECT RAISE(ABORT, 'test failure'); END;").unwrap();
    assert!(means_core::db::migrate_to(&conn, 12).is_err());
    assert!(means_core::tags::strings_for(&conn, reversals[0]).unwrap().is_empty());
    assert_eq!(conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).unwrap(), 11);
    conn.execute_batch("DROP TRIGGER fail_repair").unwrap();
    means_core::db::migrate_to(&conn, 12).unwrap();
    assert_eq!(means_core::tags::strings_for(&conn, reversals[0]).unwrap(), ["trip:porto"]);
    assert_eq!(means_core::tags::strings_for(&conn, reversals[1]).unwrap(), ["manual"]);
    assert!(means_core::tags::strings_for(&conn, reversals[2]).unwrap().is_empty());
    assert_eq!(means_core::tags::strings_for(&conn, reversals[3]).unwrap(), ["existing"]);
    means_core::db::migrate_to(&conn, 12).unwrap();
    let history = means_core::audit::history(&conn, "journal_entries", reversals[0]).unwrap();
    let repairs: Vec<_> = history.iter().filter(|a| a.action == "repair_reversal_tags").collect();
    assert_eq!(repairs.len(), 1);
    assert_eq!(repairs[0].before.as_deref(), Some("[]"));
    let after: serde_json::Value = serde_json::from_str(repairs[0].after.as_ref().unwrap()).unwrap();
    assert_eq!(after["tags"], serde_json::json!(["trip:porto"]));
    assert!(means_core::hashchain::verify(&conn, e.id).unwrap().first_bad_seq.is_none());
}

#[test]
fn conversion_requires_a_rate_for_nonzero_books_but_not_empty_entities() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let entity = entities::create_entity(&mut conn, "Personal", "person", "PT", "EUR").unwrap();
    let empty = reports::balance_sheet(&conn, Some(entity.id), Some(day(1)), "USD").unwrap();
    assert!(empty.net.is_zero());
    let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
    let equity = accounts::ensure_account(&conn, entity.id, AccountType::Equity, &["Capital"], "equity", "EUR").unwrap();
    let mut input = EntryInput::new(entity.id, day(1));
    input.postings = vec![PostingInput::new(bank.id, d("100")), PostingInput::balancing(equity.id)];
    journal::create_entry(&mut conn, input).unwrap();
    for scope in [Some(entity.id), None] {
        let error = reports::balance_sheet(&conn, scope, Some(day(1)), "USD").err().unwrap().to_string();
        assert!(error.contains("missing exchange rate from EUR to USD on 2026-09-01"), "{error}");
    }
    rates::set_price(&conn, "EUR", "USD", day(1) - chrono::Duration::days(401), d("2"), "manual").unwrap();
    assert!(reports::balance_sheet(&conn, Some(entity.id), Some(day(1)), "USD").is_err());
    rates::set_price(&conn, "EUR", "USD", day(1), d("1.25"), "manual").unwrap();
    let converted = reports::balance_sheet(&conn, Some(entity.id), Some(day(1)), "USD").unwrap();
    assert_eq!(converted.net.major(), d("125"));
    assert_eq!(converted.net.commodity(), "USD");
}

#[test]
fn tag_expenses_overlap_without_inflating_total_and_keep_untagged_separate() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let e = entities::create_entity(&mut c, "Personal", "person", "PT", "EUR").unwrap();
    let bank = accounts::ensure_account(&c, e.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
    let food = accounts::ensure_account(&c, e.id, AccountType::Expense, &["Food"], "expense", "EUR").unwrap();
    let extra = accounts::ensure_account(&c, e.id, AccountType::Expense, &["Extra"], "expense", "EUR").unwrap();
    for (date, amount, tags, status) in [
        (day(1), "10", "trip:porto with:friends", EntryStatus::Posted),
        (day(2), "-2", "trip:porto", EntryStatus::Posted),
        (day(2), "3", "", EntryStatus::Posted),
        (day(2), "7", "untagged", EntryStatus::Posted),
        (day(3), "99", "trip:porto", EntryStatus::Draft),
    ] {
        let mut input = EntryInput::new(e.id, date);
        input.status = status;
        input.postings = vec![PostingInput::new(bank.id, -d(amount)), PostingInput::new(food.id, d(amount) / d("2")), PostingInput::new(extra.id, d(amount) / d("2"))];
        let entry = journal::create_entry(&mut c, input).unwrap();
        means_core::tags::set_tags(&mut c, entry.id, &means_core::tags::parse(tags).unwrap()).unwrap();
    }
    let r = reports::expenses_by_tag(&c, e.id, None, None, None).unwrap();
    assert!(r.overlapping);
    assert_eq!(r.total, eur("18"));
    let rows: Vec<_> = r.rows.iter().map(|r| (r.tag.as_deref(), r.amount)).collect();
    assert_eq!(rows, vec![(None, eur("3")), (Some("trip:porto"), eur("8")), (Some("untagged"), eur("7")), (Some("with:friends"), eur("10"))]);
    let r = reports::expenses_by_tag(&c, e.id, Some(day(1)), Some(day(1)), Some("#TRIP:PORTO")).unwrap();
    assert_eq!(r.total, eur("10"));
    assert_eq!(r.rows.len(), 2);
    assert!(r.rows.iter().all(|r| r.amount == eur("10")));
    assert!(reports::expenses_by_tag(&c, e.id, Some(day(2)), Some(day(1)), None).is_err());
    assert!(reports::expenses_by_tag(&c, e.id, None, None, Some("trip:a trip:b")).is_err());
    assert!(reports::expenses_by_tag(&c, 999, None, None, None).is_err());
    let entry: i64 = c.query_row("SELECT id FROM journal_entries WHERE date='2026-09-01'", [], |r| r.get(0)).unwrap();
    journal::void_entry(&mut c, entry, Some(day(4)), "test").unwrap();
    let r = reports::expenses_by_tag(&c, e.id, Some(day(4)), Some(day(4)), None).unwrap();
    assert_eq!(r.total, eur("-10"));
    assert_eq!(r.rows.len(), 2);
    assert!(r.rows.iter().all(|r| r.amount == eur("-10")));
}

#[test]
fn net_worth_requires_one_vault_and_keeps_native_account_currencies() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let company = entities::create_entity(&mut c, "Company", "company", "PT", "EUR").unwrap();
    let personal = entities::create_entity(&mut c, "Personal", "person", "US", "USD").unwrap();
    rates::set_price(&c, "EUR", "USD", day(1), d("1.2"), "manual").unwrap();
    for (entity, amount) in [(&company, "1000000"), (&personal, "100")] {
        let bank = accounts::ensure_account(&c, entity.id, AccountType::Asset, &["N26"], "bank", "EUR").unwrap();
        let opening = accounts::ensure_account(&c, entity.id, AccountType::Equity, &["Opening"], "equity", &entity.currency).unwrap();
        let mut input = EntryInput::new(entity.id, day(1));
        input.postings = vec![PostingInput::new(bank.id, d(amount)), PostingInput::balancing(opening.id)];
        journal::create_entry(&mut c, input).unwrap();
    }
    let report = reports::net_worth(&c, personal.id, "", Some(day(1))).unwrap();
    assert_eq!(report.currency, "USD");
    assert_eq!(report.total, Money::from_major(d("120"), "USD", 2).unwrap());
    assert_eq!(report.by_entity.len(), 1);
    assert_eq!(report.by_entity[0].name, "Personal");
    assert_eq!(report.by_account[0].quantity, eur("100"));
    assert!(reports::net_worth(&c, 0, "", Some(day(1))).is_err());
    assert!(reports::net_worth(&c, 9999, "", Some(day(1))).is_err());
    let eur_report = reports::net_worth(&c, personal.id, "EUR", Some(day(1))).unwrap();
    assert_eq!(eur_report.total, eur("100"));
    // A company with an unrelated missing FX rate cannot break personal net worth.
    let other = entities::create_entity(&mut c, "Other company", "company", "JP", "JPY").unwrap();
    let bank = accounts::ensure_account(&c, other.id, AccountType::Asset, &["Bank"], "bank", "JPY").unwrap();
    let equity = accounts::ensure_account(&c, other.id, AccountType::Equity, &["Opening"], "equity", "JPY").unwrap();
    let mut input = EntryInput::new(other.id, day(1));
    input.postings = vec![PostingInput::new(bank.id, d("100")), PostingInput::new(equity.id, d("-100"))];
    journal::create_entry(&mut c, input).unwrap();
    assert_eq!(reports::net_worth(&c, personal.id, "", Some(day(1))).unwrap().total, report.total);
}
