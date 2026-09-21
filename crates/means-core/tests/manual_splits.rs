use chrono::NaiveDate;
use means_core::model::*;
use means_core::{accounts, entities, journal, rules, Db};
use rusqlite::params;
use rust_decimal::Decimal;
use std::str::FromStr;

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

#[test]
fn capture_and_review_keep_last_split_rounding_for_expenses_income_and_discounts() {
    for review in [false, true] {
        for kind in ["expense", "income"] {
            for (parts, expected) in [(["5.005", "5.005"], [500, 501]), (["12.005", "-1.995"], [1200, -199])] {
                let db = Db::open_memory().unwrap();
                let mut conn = db.conn();
                let entity = entities::create_entity(&mut conn, "Personal", "person", "PT", "EUR").unwrap();
                let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
                let category_type = if kind == "expense" { AccountType::Expense } else { AccountType::Income };
                let first = accounts::ensure_account(&conn, entity.id, category_type, &["First"], kind, "EUR").unwrap();
                let last = accounts::ensure_account(&conn, entity.id, category_type, &["Last"], kind, "EUR").unwrap();
                let splits = vec![(first.id, d(parts[0]), "first".into()), (last.id, d(parts[1]), "last".into())];
                let sign = if kind == "expense" { 1 } else { -1 };
                let entry = if review {
                    // A persisted unmatched statement line, as handed to manual Review.
                    conn.execute("INSERT INTO imports (uid, source, account_id, checksum, created_at) VALUES ('test-import', 'generic_csv', ?1, 'test-checksum', '2026-09-19')", [bank.id]).unwrap();
                    let import = conn.last_insert_rowid();
                    conn.execute(
                        "INSERT INTO statement_lines (import_id, account_id, position, date, amount, currency, fingerprint) VALUES (?1, ?2, 0, '2026-09-19', ?3, 'EUR', 'test-fingerprint')",
                        params![import, bank.id, -sign * 1001],
                    )
                    .unwrap();
                    let line = conn.last_insert_rowid();
                    rules::create_entry_from_line(&mut conn, line, None, "Payee", None, true, &splits).unwrap()
                } else {
                    journal::create_simple(
                        &mut conn,
                        journal::SimpleEntry {
                            entity_id: entity.id,
                            date: NaiveDate::from_ymd_opt(2026, 9, 19).unwrap(),
                            kind: kind.into(),
                            account_id: bank.id,
                            contra_account_id: None,
                            quantity: d("10.01"),
                            contra_quantity: None,
                            payee: "Payee".into(),
                            notes: String::new(),
                            splits,
                            status: EntryStatus::Posted,
                            fee: None,
                            fee_account_id: None,
                            origin: String::new(),
                        },
                    )
                    .unwrap()
                };
                assert_eq!(entry.postings[0].quantity.minor(), -sign * 1001);
                assert_eq!(entry.postings[1].quantity.minor(), sign * expected[0]);
                assert_eq!(entry.postings[2].quantity.minor(), sign * expected[1]);
                assert_eq!(entry.postings.iter().map(|p| p.amount.minor()).sum::<i64>(), 0);
                assert_eq!(entry.postings[2].metadata["balance"], true);
            }
        }
    }
}
