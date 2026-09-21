use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};

use means_core::{accounts, entities, export, journal, model::*, reports, Db};
use rust_decimal::Decimal;

fn fixture() -> (Db, BTreeMap<i64, Decimal>) {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let mut expected = BTreeMap::new();
    for (name, unit, qty, amount) in [("Personal \"quoted\"\nLisboa", "EUR", 12, 10), ("Company", "BRL", 20, 100), ("Odd currency", "1", 2, 3), ("Reserved prefix", "MEANSX31", 4, 5)] {
        let entity = entities::create_entity(&mut conn, name, "person", "PT", unit).unwrap();
        let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank", "A / B"], "bank", "USD").unwrap();
        let expense = accounts::ensure_account(&conn, entity.id, AccountType::Expense, &["Food"], "expense", unit).unwrap();
        let mut input = EntryInput::new(entity.id, means_core::parse_date("2026-01-02").unwrap());
        input.payee = "José \"quoted\" \\ shop\nnext line".into();
        input.notes = "a\tb".into();
        let mut debit = PostingInput::new(bank.id, Decimal::from(-qty));
        debit.amount = Some(Decimal::from(-amount));
        input.postings = vec![debit, PostingInput::new(expense.id, Decimal::from(amount))];
        journal::create_entry(&mut conn, input.clone()).unwrap();
        let reversed = journal::create_entry(&mut conn, input.clone()).unwrap();
        journal::void_entry(&mut conn, reversed.id, None, "returned").unwrap();
        input.status = EntryStatus::Draft;
        journal::create_entry(&mut conn, input).unwrap();
        conn.execute("UPDATE accounts SET closed_at = '2026-02-01' WHERE id = ?1", [bank.id]).unwrap();
        conn.execute("UPDATE entities SET archived_at = '2026-02-01' WHERE id = ?1", [entity.id]).unwrap();
        expected.insert(bank.id, Decimal::from(-amount));
        expected.insert(expense.id, Decimal::from(amount));
        let trial = reports::trial_balance(&conn, entity.id, None).unwrap();
        let actual: BTreeMap<_, _> = trial.rows.iter().map(|row| (row.account_id, row.debit.checked_sub(row.credit).unwrap().major())).collect();
        assert_eq!(actual, BTreeMap::from([(bank.id, Decimal::from(-amount)), (expense.id, Decimal::from(amount))]));
    }
    drop(conn);
    (db, expected)
}

#[test]
fn export_matches_golden_book_balances_and_keeps_native_evidence() {
    let (db, expected) = fixture();
    let text = export::beancount(&mut db.conn()).unwrap();
    let mut actual = BTreeMap::<i64, Decimal>::new();
    for line in text.lines().filter(|l| l.starts_with("  ") && !l.starts_with("    ")) {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() == 3 && fields[0].contains(":E") {
            let id: i64 = fields[0].split(':').nth(2).unwrap().strip_prefix('A').unwrap().split('-').next().unwrap().parse().unwrap();
            *actual.entry(id).or_default() += fields[1].parse::<Decimal>().unwrap();
        }
    }
    assert_eq!(actual, expected);
    assert_eq!(text.matches("means-entry-id:").count(), 12);
    assert!(text.contains("means-quantity: -12\n    means-commodity: \"USD\""));
    assert!(text.contains("means-status: \"void\""));
    assert!(!text.contains("means-status: \"draft\""));
    assert!(text.contains("José \\\"quoted\\\" \\\\ shop\\nnext line"));
    assert_eq!(text, export::beancount(&mut db.conn()).unwrap());
}

#[test]
fn export_is_not_capped_at_the_journal_ui_limit() {
    let (db, _) = fixture();
    let mut conn = db.conn();
    // A large balanced journal without spending this test on hash-chain generation.
    conn.execute_batch(
        "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<2001)
      INSERT INTO journal_entries(uid,entity_id,date,payee,description,notes,status,origin,created_at,updated_at)
      SELECT 'export-test-'||x, 1, '2026-01-03', '', '', '', 'posted', 'manual', '', '' FROM n;
      INSERT INTO postings(uid,journal_entry_id,account_id,quantity,amount,rate_source,memo,metadata,position)
      SELECT 'export-posting-'||e.id||'-'||p.id,e.id,p.account_id,p.quantity,p.amount,'','','{}',p.position
      FROM journal_entries e JOIN postings p ON p.journal_entry_id=1 WHERE e.uid LIKE 'export-test-%';",
    )
    .unwrap();
    let text = export::beancount(&mut conn).unwrap();
    assert_eq!(text.matches("means-entry-id:").count(), 2013);
}

/// Run explicitly with Beancount installed; this uses its real parser and balance
/// inventory, independently of the Rust golden assertion above.
#[test]
#[ignore = "requires Beancount; set MEANS_BEAN_PYTHON to its Python interpreter"]
fn beancount_parser_balances_match_the_core() {
    let (db, expected) = fixture();
    let text = export::beancount(&mut db.conn()).unwrap();
    let python = std::env::var("MEANS_BEAN_PYTHON").unwrap_or_else(|_| "python3".into());
    let mut child = Command::new(python)
        .args([
            "-c",
            r#"
import json, sys
from decimal import Decimal
from beancount import loader
from beancount.core import data
payload = json.load(sys.stdin)
entries, errors, _ = loader.load_string(payload['document'])
assert not errors, errors
accounts = {e.account: int(e.meta['means-account-id']) for e in entries if isinstance(e, data.Open)}
actual = {}
for entry in entries:
    if isinstance(entry, data.Transaction):
        for p in entry.postings:
            key = str(accounts[p.account])
            actual[key] = actual.get(key, Decimal(0)) + p.units.number
expected = {k: Decimal(v) for k, v in payload['balances'].items()}
assert actual == expected, (actual, expected)
"#,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let payload = serde_json::json!({"document": text, "balances": expected});
    child.stdin.take().unwrap().write_all(payload.to_string().as_bytes()).unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
}
