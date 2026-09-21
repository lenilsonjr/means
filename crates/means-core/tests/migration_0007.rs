//! Migration 0007 rescales a ledger written at the one scale of 10^8 (D3) into minor units of
//! each commodity (D14), or refuses and leaves the ledger where it was.

use chrono::NaiveDate;
use means_core::model::*;
use means_core::{db, hashchain, reports, Error};
use rusqlite::{params, Connection};
use rust_decimal::prelude::{FromStr, ToPrimitive};
use rust_decimal::Decimal;

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

/// What the money columns held before D14: the value times 10^8, whatever the commodity.
fn old(v: Decimal) -> i64 {
    (v * Decimal::from(100_000_000i64)).to_i64().unwrap()
}

fn posting(account_id: i64, commodity: &str, quantity: Decimal, amount: Decimal) -> Posting {
    Posting {
        id: 0,
        uid: String::new(),
        journal_entry_id: 0,
        account_id,
        account_path: String::new(),
        account_type: AccountType::Asset,
        quantity: means_core::money::Money::from_major(quantity, commodity, means_core::money::default_precision(commodity)).unwrap(),
        amount: means_core::money::Money::from_major(amount, "EUR", 2).unwrap(),
        rate: None,
        rate_source: String::new(),
        memo: String::new(),
        metadata: serde_json::json!({}),
        external_id: None,
        fingerprint: None,
        reconciled_at: None,
        position: 0,
    }
}

fn entry(id: i64, uid: &str, payee: &str, on: &str, postings: Vec<Posting>) -> JournalEntry {
    JournalEntry {
        id,
        uid: uid.to_string(),
        entity_id: 1,
        date: date(on),
        payee: payee.to_string(),
        payee_id: None,
        display_payee: String::new(),
        description: String::new(),
        notes: String::new(),
        status: EntryStatus::Posted,
        reverses_id: None,
        reversed_by_id: None,
        refund_of_id: None,
        counterpart_id: None,
        template_id: None,
        template_version: None,
        origin: "capture".into(),
        posted_at: Some("2026-02-01T00:00:00.000Z".into()),
        created_at: "2026-02-01T00:00:00.000Z".into(),
        seq: None,
        hash: None,
        postings,
        kind: String::new(),
        amount_functional: Decimal::ZERO,
        statement_line_id: None,
        tags: vec![],
        reviewed_at: None,
    }
}

/// Account ids: 1 N26 (EUR), 2 Coinbase BTC (BTC), 3 Japan Post (JPY), 4 Food (EUR).
/// Entity 1 keeps its books in EUR.
fn ledger_at_version_6() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    db::migrate_to(&conn, 6).unwrap();
    assert_eq!(user_version(&conn), 6);
    conn.execute_batch(
        "INSERT INTO entities (id, uid, name, kind, country, currency, created_at, updated_at)
           VALUES (1, 'e-1', 'Personal', 'person', 'PT', 'EUR', '2026-01-01', '2026-01-01');
         INSERT INTO commodities (id, code, kind, name, precision) VALUES
           (1, 'EUR', 'currency', 'Euro', 2), (2, 'BTC', 'crypto', 'Bitcoin', 8), (3, 'JPY', 'currency', 'Japanese yen', 0);
         INSERT INTO accounts (id, uid, entity_id, name, type, subtype, commodity_id, created_at, updated_at) VALUES
           (1, 'a-n26', 1, 'N26', 'asset', 'bank', 1, '2026-01-01', '2026-01-01'),
           (2, 'a-btc', 1, 'BTC', 'asset', 'holding', 2, '2026-01-01', '2026-01-01'),
           (3, 'a-jpy', 1, 'Japan Post', 'asset', 'bank', 3, '2026-01-01', '2026-01-01'),
           (4, 'a-food', 1, 'Food', 'expense', 'expense', 1, '2026-01-01', '2026-01-01');
         INSERT INTO imports (id, uid, source, account_id, filename, checksum, created_at)
           VALUES (1, 'i-1', 'generic_csv', 1, 'feb.csv', 'sum-1', '2026-02-05');",
    )
    .unwrap();

    let entries = [
        entry(1, "je-1", "Cafe Central", "2026-02-01", vec![posting(4, "EUR", d("3.20"), d("3.20")), posting(1, "EUR", d("-3.20"), d("-3.20"))]),
        entry(2, "je-2", "Coinbase", "2026-02-02", vec![posting(2, "BTC", d("0.01"), d("650")), posting(1, "EUR", d("-650"), d("-650"))]),
        entry(3, "je-3", "Japan Post", "2026-02-03", vec![posting(3, "JPY", d("500"), d("3")), posting(1, "EUR", d("-3"), d("-3"))]),
    ];
    let uid_of = |id: i64| match id {
        1 => "a-n26".to_string(),
        2 => "a-btc".to_string(),
        3 => "a-jpy".to_string(),
        _ => "a-food".to_string(),
    };
    // Frozen legacy canonical hashes: structured Money JSON must not change the chain.
    let legacy_hashes = [
        "fc495dc948f7d11d143e6f13f78a18f0062743301cabed59d09b577edde2faaf",
        "4d2ec870c88138813f10ce3620f4536be81d9c00bea1240ed853a805c368fdfb",
        "8087b98c70546e34ba91e8a0659b9498cb55696d0eaccc65464b428dd36c22ed",
    ];
    let mut prev = String::new();
    for (i, e) in entries.iter().enumerate() {
        let seq = i as i64 + 1;
        let hash = hashchain::digest(&prev, &hashchain::canonical(e, &uid_of));
        assert_eq!(hash, legacy_hashes[i]);
        conn.execute(
            "INSERT INTO journal_entries (id, uid, entity_id, date, payee, description, notes, status, origin, posted_at, seq, prev_hash, hash, created_at, updated_at)
             VALUES (?1, ?2, 1, ?3, ?4, '', '', 'posted', 'capture', ?5, ?6, ?7, ?8, ?5, ?5)",
            params![e.id, e.uid, e.date.to_string(), e.payee, e.created_at, seq, prev, hash],
        )
        .unwrap();
        for (position, p) in e.postings.iter().enumerate() {
            conn.execute(
                "INSERT INTO postings (uid, journal_entry_id, account_id, quantity, amount, position)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![format!("p-{}-{position}", e.id), e.id, p.account_id, old(p.quantity.major()), old(p.amount.major()), position as i64],
            )
            .unwrap();
        }
        prev = hash;
    }
    // Two statement lines: one on the EUR account, one with no account that names its own currency.
    conn.execute(
        "INSERT INTO statement_lines (id, import_id, account_id, position, date, amount, currency, description, balance_after, status)
         VALUES (1, 1, 1, 1, '2026-02-01', ?1, 'EUR', 'Cafe Central', ?2, 'matched'),
                (2, 1, NULL, 2, '2026-02-03', ?3, 'JPY', 'Konbini', NULL, 'unmatched')",
        params![old(d("-3.20")), old(d("1200.50")), old(d("-500"))],
    )
    .unwrap();
    conn
}

fn user_version(conn: &Connection) -> i64 {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap()
}

fn heads(conn: &Connection) -> Vec<String> {
    let mut stmt = conn.prepare("SELECT hash FROM journal_entries ORDER BY seq").unwrap();
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
    rows.collect::<std::result::Result<Vec<_>, _>>().unwrap()
}

#[test]
fn it_rescales_a_version_6_ledger_and_leaves_the_chain_alone() {
    let conn = ledger_at_version_6();
    let before = heads(&conn);

    db::migrate_to(&conn, 7).unwrap();
    assert_eq!(user_version(&conn), 7);

    let raw = |entry_id: i64, account_id: i64| -> (i64, i64) {
        conn.query_row("SELECT quantity, amount FROM postings WHERE journal_entry_id = ?1 AND account_id = ?2", params![entry_id, account_id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
    };
    assert_eq!(raw(1, 4), (320, 320), "3.20 EUR is 320 cents");
    assert_eq!(raw(1, 1), (-320, -320));
    assert_eq!(raw(2, 2), (1_000_000, 65_000), "0.01 BTC is 1000000 satoshis, valued at 650.00 EUR");
    assert_eq!(raw(3, 3), (500, 300), "500 JPY is 500 whole yen, valued at 3.00 EUR");

    let line: (i64, Option<i64>) = conn.query_row("SELECT amount, balance_after FROM statement_lines WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(line, (-320, Some(120_050)));
    let orphan: i64 = conn.query_row("SELECT amount FROM statement_lines WHERE id = 2", [], |r| r.get(0)).unwrap();
    assert_eq!(orphan, -500, "no account: the line's own currency says what a minor unit is");

    assert_eq!(heads(&conn), before, "migration 0007 preserves the stored legacy hashes");
    // Current readers require the current schema; verify the frozen chain again
    // after the later, non-accounting migrations have been applied.
    db::migrate_to(&conn, i64::MAX).unwrap();
    let report = hashchain::verify(&conn, 1).unwrap();
    assert_eq!(report.first_bad_seq, None, "the chain is unchanged: canonical renders decimals");
    assert_eq!(report.checked, 3);
    assert_eq!(report.head, before.last().cloned());
    assert_eq!(heads(&conn), before);

    let tb = reports::trial_balance(&conn, 1, None).unwrap();
    assert_eq!(tb.total_debit, tb.total_credit);
    assert_eq!(tb.rows.iter().map(|r| r.amount.major()).sum::<Decimal>(), Decimal::ZERO);
}

#[test]
fn it_refuses_a_value_that_is_not_a_whole_number_of_minor_units() {
    let conn = ledger_at_version_6();
    conn.execute("UPDATE postings SET quantity = 123456789 WHERE journal_entry_id = 1 AND account_id = 4", []).unwrap();

    let err = db::migrate_to(&conn, 7).unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err:?}");
    assert!(err.to_string().contains("EUR minor units"), "{err}");
    assert_eq!(user_version(&conn), 6, "the refusal rolls the migration back whole");
    let quantity: i64 = conn.query_row("SELECT quantity FROM postings WHERE journal_entry_id = 1 AND account_id = 4", [], |r| r.get(0)).unwrap();
    assert_eq!(quantity, 123456789, "nothing was rescaled");
}

/// This line is refused after every posting has already been rewritten, so the postings prove
/// the migration runs in one transaction: without it they would keep their new values.
#[test]
fn it_refuses_a_statement_line_with_a_value_and_no_commodity() {
    let conn = ledger_at_version_6();
    conn.execute("UPDATE statement_lines SET currency = 'XYZ' WHERE id = 2", []).unwrap();

    let err = db::migrate_to(&conn, 7).unwrap_err();
    assert!(err.to_string().contains("statement line 2"), "{err}");
    assert_eq!(user_version(&conn), 6);
    let mut stmt = conn.prepare("SELECT quantity, amount FROM postings ORDER BY id").unwrap();
    let rows: Vec<(i64, i64)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().collect::<std::result::Result<_, _>>().unwrap();
    assert_eq!(
        rows,
        vec![
            (old(d("3.20")), old(d("3.20"))),
            (old(d("-3.20")), old(d("-3.20"))),
            (old(d("0.01")), old(d("650"))),
            (old(d("-650")), old(d("-650"))),
            (old(d("500")), old(d("3"))),
            (old(d("-3")), old(d("-3"))),
        ],
        "the postings were rescaled before the line failed, and the rollback put them back"
    );
    let line: i64 = conn.query_row("SELECT amount FROM statement_lines WHERE id = 1", [], |r| r.get(0)).unwrap();
    assert_eq!(line, old(d("-3.20")));
}
#[test]
fn it_refuses_a_commodity_with_more_than_eight_decimals() {
    let conn = ledger_at_version_6();
    conn.execute("UPDATE commodities SET precision = 18 WHERE code = 'BTC'", []).unwrap();

    let err = db::migrate_to(&conn, 7).unwrap_err();
    assert!(err.to_string().contains("BTC"), "{err}");
    assert_eq!(user_version(&conn), 6);
}
