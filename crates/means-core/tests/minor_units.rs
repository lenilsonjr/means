//! D14: a quantity and an amount are integers of their commodity's minor unit. The tests read
//! the raw columns, because that is what the decision is about.

use chrono::NaiveDate;
use means_core::model::*;
use means_core::{accounts, entities, imports, journal, rates, reports, Db, Error};
use rusqlite::Connection;
use rust_decimal::prelude::FromStr;
use rust_decimal::Decimal;

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

fn raw_postings(conn: &Connection, entry_id: i64) -> Vec<(i64, i64, i64)> {
    let mut stmt = conn.prepare("SELECT account_id, quantity, amount FROM postings WHERE journal_entry_id = ?1 ORDER BY position").unwrap();
    let rows = stmt.query_map([entry_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
    rows.collect::<std::result::Result<Vec<_>, _>>().unwrap()
}

fn raw_posting(conn: &Connection, entry_id: i64, account_id: i64) -> (i64, i64) {
    let (_, q, a) = raw_postings(conn, entry_id).into_iter().find(|(id, _, _)| *id == account_id).expect("posting on the account");
    (q, a)
}

struct Books {
    db: Db,
    entity: Entity,
    n26: Account,
    btc: Account,
    jpy: Account,
    food: Account,
}

/// An entity in EUR with a EUR bank account, a BTC holding and a JPY account.
fn books() -> Books {
    let db = Db::open_memory().unwrap();
    let (entity, n26, btc, jpy, food) = {
        let mut conn = db.conn();
        let entity = entities::create_entity(&mut conn, "Personal", "person", "PT", "EUR").unwrap();
        let n26 = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank", "N26"], "bank", "EUR").unwrap();
        let btc = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Coinbase", "BTC"], "holding", "BTC").unwrap();
        let jpy = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank", "Japan Post"], "bank", "JPY").unwrap();
        let food = accounts::ensure_account(&conn, entity.id, AccountType::Expense, &["Food"], "expense", "EUR").unwrap();
        rates::set_price(&conn, "BTC", "EUR", date("2026-02-01"), d("65000"), "manual").unwrap();
        rates::set_price(&conn, "JPY", "EUR", date("2026-02-01"), d("0.006"), "manual").unwrap();
        (entity, n26, btc, jpy, food)
    };
    Books { db, entity, n26, btc, jpy, food }
}

#[test]
fn the_four_columns_hold_minor_units_of_their_commodity() {
    let b = books();
    let mut conn = b.db.conn();
    assert_eq!((b.n26.precision, b.btc.precision, b.jpy.precision), (2, 8, 0));

    // A coffee: cents on both legs, because the account and the functional currency are EUR.
    let mut coffee = EntryInput::new(b.entity.id, date("2026-02-01"));
    coffee.payee = "Cafe Central".into();
    coffee.postings = vec![PostingInput::new(b.food.id, d("3.20")), PostingInput::new(b.n26.id, d("-3.20"))];
    let coffee = journal::create_entry(&mut conn, coffee).unwrap();
    assert_eq!(raw_posting(&conn, coffee.id, b.food.id), (320, 320));
    assert_eq!(raw_posting(&conn, coffee.id, b.n26.id), (-320, -320));

    // 0.01 BTC at 65,000 EUR: satoshis in the quantity, cents in the amount.
    let mut buy = EntryInput::new(b.entity.id, date("2026-02-01"));
    buy.postings = vec![PostingInput::new(b.btc.id, d("0.01")), PostingInput::new(b.n26.id, d("-650.00"))];
    let buy = journal::create_entry(&mut conn, buy).unwrap();
    assert_eq!(raw_posting(&conn, buy.id, b.btc.id), (1_000_000, 65_000));
    assert_eq!(raw_posting(&conn, buy.id, b.n26.id), (-65_000, -65_000));

    // 500 JPY at 0.006 EUR: whole yen in the quantity, 3.00 EUR in the amount.
    let mut yen = EntryInput::new(b.entity.id, date("2026-02-01"));
    yen.postings = vec![PostingInput::new(b.jpy.id, d("500")), PostingInput::new(b.n26.id, d("-3.00"))];
    let yen = journal::create_entry(&mut conn, yen).unwrap();
    assert_eq!(raw_posting(&conn, yen.id, b.jpy.id), (500, 300));
    assert_eq!(raw_posting(&conn, yen.id, b.n26.id), (-300, -300));

    // A statement line is in its account's commodity.
    let mapping = imports::CsvMapping { date_column: "Date".into(), description_column: "Description".into(), amount_column: "Amount".into(), ..Default::default() };
    let csv = "Date,Description,Amount\n2026-02-03,Pingo Doce,-12.50\n";
    let out = imports::run_import(&mut conn, imports::ImportRequest::new("generic_csv", Some(b.n26.id), "n26-feb.csv", csv.as_bytes()).mapping(Some(&mapping))).unwrap();
    let line_id = out.lines[0].id;
    let raw_amount: i64 = conn.query_row("SELECT amount FROM statement_lines WHERE id = ?1", [line_id], |r| r.get(0)).unwrap();
    assert_eq!(raw_amount, -1250);
    assert_eq!(imports::get_line(&conn, line_id).unwrap().amount.map(|a| a.major()), Some(d("-12.50")));

    let yen_csv = "Date,Description,Amount\n2026-02-04,Konbini,-500\n";
    let out = imports::run_import(&mut conn, imports::ImportRequest::new("generic_csv", Some(b.jpy.id), "jp-feb.csv", yen_csv.as_bytes()).mapping(Some(&mapping))).unwrap();
    let raw_amount: i64 = conn.query_row("SELECT amount FROM statement_lines WHERE id = ?1", [out.lines[0].id], |r| r.get(0)).unwrap();
    assert_eq!(raw_amount, -500);
    assert_eq!(imports::get_line(&conn, out.lines[0].id).unwrap().amount.map(|a| a.major()), Some(d("-500")));

    // What the reports read back is what was written.
    let ledger = reports::general_ledger(&conn, b.btc.id, None, None, 100, false).unwrap();
    assert_eq!(ledger.closing_balance.major(), d("0.01"));
    let balances = accounts::list_accounts_with_balances(&conn, Some(b.entity.id), false, None).unwrap();
    let jpy_balance = balances.iter().find(|a| a.id == b.jpy.id).unwrap();
    assert_eq!(jpy_balance.balance, d("500"));
    assert_eq!(jpy_balance.balance_functional, d("3.00"));
    let tb = reports::trial_balance(&conn, b.entity.id, None).unwrap();
    assert_eq!(tb.total_debit, tb.total_credit);
}

#[test]
fn an_entry_off_by_one_minor_unit_is_unbalanced() {
    let b = books();
    let mut conn = b.db.conn();
    let mut off = EntryInput::new(b.entity.id, date("2026-02-01"));
    // Both legs are in EUR and both amounts are given, so nothing balances or absorbs the cent.
    off.postings = vec![
        PostingInput { account_id: b.food.id, quantity: d("3.20"), amount: Some(d("3.20")), ..Default::default() },
        PostingInput { account_id: b.n26.id, quantity: d("-3.19"), amount: Some(d("-3.19")), ..Default::default() },
    ];
    let err = journal::create_entry(&mut conn, off).unwrap_err();
    assert!(matches!(err, Error::Unbalanced(_)), "{err:?}");

    // The same entry to the cent is accepted.
    let mut exact = EntryInput::new(b.entity.id, date("2026-02-01"));
    exact.postings = vec![
        PostingInput { account_id: b.food.id, quantity: d("3.20"), amount: Some(d("3.20")), ..Default::default() },
        PostingInput { account_id: b.n26.id, quantity: d("-3.20"), amount: Some(d("-3.20")), ..Default::default() },
    ];
    journal::create_entry(&mut conn, exact).unwrap();
}

#[test]
fn a_commodity_precision_is_zero_to_eight() {
    let db = Db::open_memory().unwrap();
    let conn = db.conn();
    assert_eq!(entities::create_commodity(&conn, "BTC", "crypto", "Bitcoin", Some(8), "").unwrap().precision, 8);
    assert_eq!(entities::create_commodity(&conn, "JPY", "currency", "Japanese yen", Some(0), "").unwrap().precision, 0);
    assert_eq!(entities::create_commodity(&conn, "VWCE", "security", "Vanguard FTSE All-World", None, "").unwrap().precision, 4);
    let err = entities::create_commodity(&conn, "WEI", "crypto", "Wei", Some(9), "").unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err:?}");
    assert!(entities::get_commodity_by_code(&conn, "WEI").is_err(), "a refused commodity is not created");
}

#[test]
fn a_commodity_precision_cannot_change_after_creation() {
    let db = Db::open_memory().unwrap();
    let conn = db.conn();
    let btc = entities::create_commodity(&conn, "BTC", "crypto", "Bitcoin", Some(8), "").unwrap();

    // Asking for another precision is refused: it would reinterpret every BTC row by 100.
    let err = entities::create_commodity(&conn, "BTC", "crypto", "Bitcoin", Some(6), "").unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err:?}");
    assert_eq!(entities::get_commodity_by_code(&conn, "BTC").unwrap().precision, 8);

    // Asking for no precision keeps the stored one, and the rest of the row still updates.
    let again = entities::create_commodity(&conn, "BTC", "crypto", "Bitcoin XBT", None, "").unwrap();
    assert_eq!((again.id, again.precision, again.name.as_str()), (btc.id, 8, "Bitcoin XBT"));

    // The stored precision wins over the default for the kind, too.
    let fund = entities::create_commodity(&conn, "VWCE", "security", "All-World", Some(3), "").unwrap();
    assert_eq!(fund.precision, 3, "a security defaults to 4 decimals");
    assert_eq!(entities::create_commodity(&conn, "VWCE", "security", "", None, "IE00BK5BQT80").unwrap().precision, 3);
    assert_eq!(entities::get_commodity_by_code(&conn, "VWCE").unwrap().isin, "IE00BK5BQT80");
}

#[test]
fn explicit_valuations_preserve_quantity_and_amount_invariants() {
    let b = books();
    let mut conn = b.db.conn();
    let mut input = EntryInput::new(b.entity.id, date("2026-02-01"));
    input.status = EntryStatus::Posted;
    input.postings = vec![PostingInput::new(b.n26.id, d("-100")), PostingInput::balancing(b.food.id)];
    let original = journal::create_entry(&mut conn, input.clone()).unwrap();
    let head = means_core::hashchain::verify(&conn, b.entity.id).unwrap().head;
    let audit_count: i64 = conn.query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0)).unwrap();
    for (account, quantity, amount) in [(b.n26.id, "-100", "-1"), (b.n26.id, "100", "99.99"), (b.btc.id, "1", "-2"), (b.btc.id, "-1", "2")] {
        input.postings[0] = PostingInput { amount: Some(d(amount)), ..PostingInput::new(account, d(quantity)) };
        assert!(matches!(journal::create_entry(&mut conn, input.clone()), Err(Error::Invalid(_))));
        assert!(matches!(journal::update_entry(&mut conn, original.id, input.clone()), Err(Error::Invalid(_))));
        assert_eq!(serde_json::to_value(journal::get_entry(&conn, original.id).unwrap()).unwrap(), serde_json::to_value(&original).unwrap());
        assert_eq!(means_core::hashchain::verify(&conn, b.entity.id).unwrap().head, head);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get::<_, i64>(0)).unwrap(), audit_count);
        assert_eq!(journal::count_by_status(&conn, "posted").unwrap(), 1);
    }
    // Equality is evaluated at stored precision; sub-cent differences are harmless.
    for (account, quantity, amount) in [(b.n26.id, "-1.004", "-1.001"), (b.btc.id, "-0.01", "-650"), (b.btc.id, "0.00000001", "0")] {
        input.postings[0] = PostingInput { amount: Some(d(amount)), ..PostingInput::new(account, d(quantity)) };
        journal::create_entry(&mut conn, input.clone()).unwrap();
        journal::update_entry(&mut conn, original.id, input.clone()).unwrap();
    }
    assert!(means_core::hashchain::verify(&conn, b.entity.id).unwrap().first_bad_seq.is_none());
}

#[test]
fn statement_money_keeps_its_unit_and_matching_refuses_another_currency() {
    use means_core::{matcher, money::Money};
    let b = books();
    let mut conn = b.db.conn();
    let csv = "Date,Description,Amount\n2026-02-01,Coffee,-12.345\n";
    let mapping = imports::CsvMapping { date_column: "Date".into(), description_column: "Description".into(), amount_column: "Amount".into(), decimal_separator: ".".into(), ..Default::default() };
    let preview = imports::run_import(&mut conn, imports::ImportRequest::new("generic_csv", None, "preview.csv", csv.as_bytes()).mapping(Some(&mapping)).preview(true)).unwrap();
    assert_eq!(preview.parse.lines[0].amount, Some(d("-12.345")));
    assert!(preview.lines[0].amount.is_none(), "an unplaced preview cannot assign an accounting commodity");
    let imported = imports::run_import(&mut conn, imports::ImportRequest::new("generic_csv", Some(b.n26.id), "bank.csv", csv.as_bytes()).mapping(Some(&mapping))).unwrap();
    let stored = imports::get_line(&conn, imported.lines[0].id).unwrap();
    assert_eq!(stored.amount.unwrap(), Money::from_minor(-1234, "EUR", 2).unwrap());
    assert_eq!(stored.amount, imported.lines[0].amount);
    let line_json = serde_json::to_value(&stored).unwrap();
    assert_eq!(line_json["amount"], serde_json::json!({"minor":"-1234", "commodity":"EUR", "precision":2}));
    for wrong in [Money::from_minor(-1234, "USD", 2).unwrap(), Money::from_minor(-1234, "EUR", 3).unwrap()] {
        assert!(matcher::candidates(&conn, b.n26.id, wrong, date("2026-02-01"), 7).is_err());
    }
}
