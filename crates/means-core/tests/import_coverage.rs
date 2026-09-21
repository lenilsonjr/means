use means_core::{accounts, entities, imports, journal, AccountType, Db, EntryInput, PostingInput};
use rust_decimal::Decimal;

fn setup(amounts: &[i64]) -> (Db, i64) {
    let db = Db::open_memory().unwrap();
    let bank = {
        let mut c = db.conn();
        let entity = entities::create_entity(&mut c, "Example company", "company", "EE", "EUR").unwrap();
        let bank = accounts::ensure_account(&c, entity.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
        let expense = accounts::ensure_account(&c, entity.id, AccountType::Expense, &["Services"], "expense", "EUR").unwrap();
        for amount in amounts {
            let mut entry = EntryInput::new(entity.id, "2026-09-10".parse().unwrap());
            entry.postings = vec![PostingInput::new(bank.id, Decimal::from(-amount)), PostingInput::balancing(expense.id)];
            journal::create_entry(&mut c, entry).unwrap();
        }
        bank.id
    };
    (db, bank)
}

fn file(amounts: &[i64], date: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "channel":"enable_banking", "version":1,
        "account":{"identification_hash":"synthetic-bank","uid":"session","currency":"EUR"},
        "transactions": amounts.iter().enumerate().map(|(i,a)| serde_json::json!({
            "entry_reference":format!("ref-{i}"), "status":"BOOK", "booking_date":date,
            "credit_debit_indicator":"DBIT", "transaction_amount":{"amount":a.to_string(),"currency":"EUR"},
            "remittance_information":["Synthetic service payment"]
        })).collect::<Vec<_>>()
    }))
    .unwrap()
}

#[test]
fn split_and_aggregate_overlap_warn_without_merging_or_changing_old_entries() {
    for (old, new) in [(vec![1500, 3000], vec![4500]), (vec![4500], vec![1500, 3000])] {
        let (db, bank) = setup(&old);
        let mut c = db.conn();
        let bytes = file(&new, "2026-09-11");
        let preview = imports::run_import(&mut c, imports::ImportRequest::new("auto", Some(bank), "preview.json", &bytes).preview(true)).unwrap();
        assert!(preview.import.options["coverage_warning"].as_str().unwrap().contains("incoming lines"));
        assert_eq!(c.query_row::<i64, _, _>("SELECT COUNT(*) FROM imports", [], |r| r.get(0)).unwrap(), 0);
        let result = imports::run_import(&mut c, imports::ImportRequest::new("auto", Some(bank), "bank.json", &bytes)).unwrap();
        assert_eq!(result.import.created_count, new.len() as i32);
        assert_eq!(result.import.matched_count, 0);
        let warning = result.import.options["coverage_warning"].as_str().unwrap();
        assert!(warning.contains(&format!("{} newly created entries", new.len())));
        assert!(result.parse.warnings.iter().any(|w| w == warning));
        assert_eq!(imports::get_import(&c, result.import.id).unwrap().0.options["coverage_warning"], warning);
        let old_total: i64 =
            c.query_row("SELECT SUM(p.quantity) FROM postings p JOIN journal_entries e ON e.id=p.journal_entry_id WHERE p.account_id=?1 AND e.status='posted'", [bank], |r| r.get(0)).unwrap();
        assert_eq!(old_total, -450000);
        // A new envelope repeats the same provider records: all are duplicates, so no warning.
        let mut repeat: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        repeat["pulled_at"] = serde_json::json!("later");
        let repeated = imports::run_import(&mut c, imports::ImportRequest::new("auto", Some(bank), "again.json", &serde_json::to_vec(&repeat).unwrap())).unwrap();
        assert_eq!(repeated.import.created_count, 0);
        assert!(repeated.import.options.get("coverage_warning").is_none());
    }
}

#[test]
fn new_history_outside_window_and_exact_matches_do_not_warn() {
    for (old, new, date) in [(vec![], vec![1500, 3000], "2026-09-10"), (vec![4500], vec![1500], "2026-09-16"), (vec![4500], vec![4500], "2026-09-10")] {
        let (db, bank) = setup(&old);
        let out = imports::run_import(&mut db.conn(), imports::ImportRequest::new("auto", Some(bank), "bank.json", &file(&new, date))).unwrap();
        assert!(out.import.options.get("coverage_warning").is_none(), "{:?}", out.import.options);
    }
}

#[test]
fn other_accounts_do_not_supply_coverage_and_five_day_boundary_is_inclusive() {
    let (db, bank) = setup(&[4500]);
    let mut c = db.conn();
    let other = accounts::ensure_account(&c, 1, AccountType::Asset, &["Other bank"], "bank", "EUR").unwrap();
    let out = imports::run_import(&mut c, imports::ImportRequest::new("auto", Some(other.id), "other.json", &file(&[1500], "2026-09-10"))).unwrap();
    assert!(out.import.options.get("coverage_warning").is_none());
    for date in ["2026-09-05", "2026-09-15"] {
        let out = imports::run_import(&mut c, imports::ImportRequest::new("auto", Some(bank), "preview.json", &file(&[1500], date)).preview(true)).unwrap();
        assert!(out.import.options.get("coverage_warning").is_some());
    }
}

#[test]
fn rule_posted_entries_keep_the_warning_and_retry_does_not_use_its_own_entries() {
    let (db, bank) = setup(&[4500]);
    let mut c = db.conn();
    let expense = accounts::ensure_account(&c, 1, AccountType::Expense, &["Services"], "expense", "EUR").unwrap();
    means_core::rules::save_rule(
        &c,
        &means_core::Rule {
            id: 0,
            entity_id: 1,
            name: "Synthetic services".into(),
            position: 0,
            enabled: true,
            conditions: vec![means_core::RuleCondition { field: "description".into(), op: "contains".into(), value: "Synthetic".into() }],
            account_id: Some(expense.id),
            template_id: None,
            payee: String::new(),
            hits_count: 0,
            created_at: String::new(),
            tags: String::new(),
        },
    )
    .unwrap();
    let result = imports::run_import(&mut c, imports::ImportRequest::new("auto", Some(bank), "bank.json", &file(&[1500, 3000], "2026-09-10"))).unwrap();
    assert!(result.import.options.get("coverage_warning").is_some());
    for line in &result.lines {
        assert_eq!(journal::get_entry(&c, line.journal_entry_id.unwrap()).unwrap().status, means_core::EntryStatus::Posted);
    }
    assert!(imports::retry_import(&mut c, result.import.id).unwrap().options.get("coverage_warning").is_some());
    let (empty, bank) = setup(&[]);
    let mut c = empty.conn();
    let result = imports::run_import(&mut c, imports::ImportRequest::new("auto", Some(bank), "bank.json", &file(&[1500, 3000], "2026-09-10"))).unwrap();
    for line in &result.lines {
        journal::post_entry(&mut c, line.journal_entry_id.unwrap()).unwrap();
    }
    assert!(imports::retry_import(&mut c, result.import.id).unwrap().options.get("coverage_warning").is_none());
}
