//! End-to-end tests of the import pipeline and the Account Tracker migration.

use chrono::NaiveDate;
use means_core::model::*;
use means_core::{accounts, entities, imports, journal, rates, reports, rules, Db, Error};
use rust_decimal::prelude::FromStr;
use rust_decimal::Decimal;

const N26_CSV: &str = r#""Booking Date","Value Date","Partner Name","Partner Iban","Type","Payment Reference","Account Name","Amount (EUR)","Original Amount","Original Currency","Exchange Rate"
"2026-02-01","2026-02-01","Cafe Central","","MasterCard Payment","","Main Account","-3.20","","",""
"2026-02-02","2026-02-02","LIDL SAGT DANKE","","MasterCard Payment","","Main Account","-41.00","","",""
"2026-02-03","2026-02-03","Example Client","","Credit Transfer","Invoice 42","Main Account","7000.00","","",""
"#;

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}
fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

struct World {
    db: Db,
    entity: Entity,
    n26: Account,
    food: Account,
}

fn world() -> World {
    let db = Db::open_memory().unwrap();
    let (entity, n26, food) = {
        let mut conn = db.conn();
        let entity = entities::create_entity(&mut conn, "Personal", "person", "PT", "EUR").unwrap();
        let n26 = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank", "N26"], "bank", "EUR").unwrap();
        let food = accounts::ensure_account(&conn, entity.id, AccountType::Expense, &["Food"], "expense", "EUR").unwrap();
        rates::set_price(&conn, "USD", "EUR", date("2026-01-01"), d("0.88"), "manual").unwrap();
        (entity, n26, food)
    };
    World { db, entity, n26, food }
}

#[test]
fn n26_import_drafts_dedupes_and_rejects_repeats() {
    let w = world();
    let mut conn = w.db.conn();
    let out = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(w.n26.id), "n26.csv", N26_CSV.as_bytes())).unwrap();
    assert_eq!(out.import.source, "n26_csv");
    assert_eq!(out.import.lines_count, 3);
    assert_eq!(out.import.created_count, 3);
    assert_eq!(out.import.matched_count, 0);
    // Every line became a draft against Suspense with the bank posting reconciled.
    for l in &out.lines {
        assert_eq!(l.status, "created");
        let e = journal::get_entry(&conn, l.journal_entry_id.unwrap()).unwrap();
        assert_eq!(e.status, EntryStatus::Draft);
        assert_eq!(e.postings[1].account_path, "Expenses:Uncategorized");
        assert!(e.postings[0].reconciled_at.is_some());
    }
    // Same file again: refused.
    let again = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(w.n26.id), "n26.csv", N26_CSV.as_bytes()));
    assert!(matches!(again, Err(Error::Conflict(_))));
    // Overlapping statement with one new line: duplicates detected, one created.
    let overlap = format!("{N26_CSV}\"2026-02-04\",\"2026-02-04\",\"Pingo Doce\",\"\",\"MasterCard Payment\",\"\",\"Main Account\",\"-12.50\",\"\",\"\",\"\"\n");
    let out2 = imports::run_import(&mut conn, imports::ImportRequest::new("n26_csv", Some(w.n26.id), "n26-feb.csv", overlap.as_bytes())).unwrap();
    assert_eq!(out2.import.duplicate_count, 3);
    assert_eq!(out2.import.created_count, 1);
    // Rolling back the second import removes only its entry.
    let before = journal::count_by_status(&conn, "draft").unwrap();
    imports::delete_import(&mut conn, out2.import.id, false).unwrap();
    assert_eq!(journal::count_by_status(&conn, "draft").unwrap(), before - 1);
}

#[test]
fn capture_entry_is_matched_by_the_bank_line_and_rules_post() {
    let w = world();
    let mut conn = w.db.conn();
    // A coffee captured by hand before the statement arrives.
    let e = journal::create_simple(
        &mut conn,
        journal::SimpleEntry {
            entity_id: w.entity.id,
            date: date("2026-02-01"),
            kind: "expense".into(),
            account_id: w.n26.id,
            contra_account_id: Some(w.food.id),
            quantity: d("3.20"),
            contra_quantity: None,
            payee: "Cafe".into(),
            notes: String::new(),
            splits: vec![],
            status: EntryStatus::Posted,
            fee: None,
            fee_account_id: None,
            origin: String::new(),
        },
    )
    .unwrap();
    // A rule for the supermarket.
    rules::save_rule(
        &conn,
        &Rule {
            id: 0,
            entity_id: w.entity.id,
            name: "Lidl".into(),
            position: 0,
            enabled: true,
            conditions: vec![RuleCondition { field: "description".into(), op: "contains".into(), value: "lidl".into() }],
            account_id: Some(w.food.id),
            template_id: None,
            payee: "Lidl".into(),
            hits_count: 0,
            created_at: String::new(),
            tags: String::new(),
        },
    )
    .unwrap();
    let out = imports::run_import(&mut conn, imports::ImportRequest::new("n26_csv", Some(w.n26.id), "n26.csv", N26_CSV.as_bytes())).unwrap();
    assert_eq!(out.import.matched_count, 1);
    assert_eq!(out.import.created_count, 2);
    let coffee = out.lines.iter().find(|l| l.description.contains("Cafe")).unwrap();
    assert_eq!(coffee.status, "matched");
    assert_eq!(coffee.journal_entry_id, Some(e.id));
    let e = journal::get_entry(&conn, e.id).unwrap();
    assert!(e.postings[0].reconciled_at.is_some());
    let lidl = out.lines.iter().find(|l| l.description.contains("LIDL")).unwrap();
    let le = journal::get_entry(&conn, lidl.journal_entry_id.unwrap()).unwrap();
    assert_eq!(le.status, EntryStatus::Posted);
    assert_eq!(le.origin, "rule");
    assert_eq!(le.payee, "Lidl");
    assert_eq!(le.postings[1].account_id, w.food.id);
    let income = out.lines.iter().find(|l| l.description.contains("Example Client")).unwrap();
    let ie = journal::get_entry(&conn, income.journal_entry_id.unwrap()).unwrap();
    assert_eq!(ie.status, EntryStatus::Draft);
    assert_eq!(ie.postings[0].quantity.major(), d("7000"));
    // Review: re-account the income draft by hand.
    let contracting = accounts::ensure_account(&conn, w.entity.id, AccountType::Income, &["Contracting"], "income", "EUR").unwrap();
    let mut input = EntryInput::new(w.entity.id, ie.date);
    input.payee = "Example Client".into();
    input.status = EntryStatus::Posted;
    input.postings = vec![PostingInput::new(w.n26.id, d("7000")), PostingInput::balancing(contracting.id)];
    let posted = journal::update_entry(&mut conn, ie.id, input).unwrap();
    assert_eq!(posted.status, EntryStatus::Posted);
    assert_eq!(posted.postings[1].account_path, "Income:Contracting");
    assert!(posted.postings[0].reconciled_at.is_some(), "the reconciled bank posting survives the edit");
    let tb = reports::trial_balance(&conn, w.entity.id, None).unwrap();
    assert_eq!(tb.total_debit, tb.total_credit);
    let is = reports::income_statement(&conn, w.entity.id, None, None).unwrap();
    assert_eq!(is.total_credit.major(), d("7000"));
    assert_eq!(is.total_debit.major(), d("44.20"));
    let rec = reports::reconciliation(&conn, w.n26.id).unwrap();
    assert_eq!(rec.ledger_balance, d("6955.80"));
    assert_eq!(rec.unreconciled.len(), 0);
}

#[test]
fn generic_csv_with_mapping_and_ofx() {
    let w = world();
    let mut conn = w.db.conn();
    let csv = "Data;Descricao;Debito;Credito\n02/02/2026;Padaria;34,04;\n03/02/2026;Cliente;;1.000,00\n";
    let mapping = imports::CsvMapping {
        date_column: "Data".into(),
        description_column: "Descricao".into(),
        debit_column: "Debito".into(),
        credit_column: "Credito".into(),
        currency: "EUR".into(),
        ..Default::default()
    };
    let out = imports::run_import(&mut conn, imports::ImportRequest::new("generic_csv", Some(w.n26.id), "x.csv", csv.as_bytes()).mapping(Some(&mapping)).preview(true)).unwrap();
    assert_eq!(out.import.status, "preview");
    assert_eq!(out.lines.len(), 2);
    assert_eq!(out.lines[0].amount.map(|a| a.major()), Some(d("-34.04")));
    assert_eq!(out.lines[1].amount.map(|a| a.major()), Some(d("1000")));
    assert_eq!(out.lines[0].date, Some(date("2026-02-02")));
    let ofx = "OFXHEADER:100\n<OFX><STMTRS><CURDEF>EUR<BANKTRANLIST><STMTTRN><TRNTYPE>DEBIT<DTPOSTED>20260205<TRNAMT>-9.90<FITID>abc1<MEMO>Spotify</STMTTRN></BANKTRANLIST><LEDGERBAL><BALAMT>990.10<DTASOF>20260205</LEDGERBAL></STMTRS></OFX>";
    let out = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(w.n26.id), "inter.ofx", ofx.as_bytes())).unwrap();
    assert_eq!(out.import.source, "inter_ofx");
    assert_eq!(out.import.created_count, 1);
    assert_eq!(out.import.closing_balance, Some(d("990.10")));
    let rec = reports::reconciliation(&conn, w.n26.id).unwrap();
    assert_eq!(rec.statement_balance, Some(d("990.10")));
    assert_eq!(rec.difference, Some(d("990.10")), "the draft does not count yet; the statement balance stands alone");
}

/// Opt-in migration check for an explicitly supplied Account Tracker backup.
#[test]
#[ignore = "requires an explicitly supplied MEANS_ATB backup; may print private data"]
fn account_tracker_real_backup() {
    let path = std::env::var_os("MEANS_ATB").expect("set MEANS_ATB to an explicit backup path");
    let content = std::fs::read(path).expect("read explicitly selected backup");
    let insp = imports::account_tracker::inspect(&content).unwrap();
    eprintln!(
        "inspect: {} accounts, {} transactions, {} recurring, base {}, exported {}, pence side {}, {} balance mismatches",
        insp.accounts.len(),
        insp.transactions,
        insp.recurring,
        insp.base_currency,
        insp.exported_on,
        insp.pence_side,
        insp.balance_mismatches
    );
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let personal = entities::create_entity(&mut conn, "Import check", "person", "", &insp.base_currency).unwrap();
    let mappings: Vec<_> = insp
        .accounts
        .iter()
        .map(|a| imports::account_tracker::Mapping { external_id: a.external_id.clone(), entity_id: personal.id, r#type: a.suggested_type.clone(), subtype: a.suggested_subtype.clone(), skip: false })
        .collect();
    let started = std::time::Instant::now();
    let r = imports::account_tracker::import(&mut conn, &content, "backup.atb", &mappings, personal.id, true, true).unwrap();
    eprintln!("import: {} created, {} existing, {} warnings in {:.1}s", r.created, r.skipped_existing, r.warnings.len(), started.elapsed().as_secs_f64());
    for w in r.warnings.iter().take(15) {
        eprintln!("  warning: {w}");
    }
    let mut bad = 0;
    for c in &r.checks {
        if !c.ok {
            bad += 1;
        }
        eprintln!("  {} {:<22} expected {:>12} actual {:>12}", if c.ok { "ok  " } else { "DIFF" }, c.name, c.expected, c.actual);
    }
    eprintln!("checks: {} of {} match", r.checks.len() - bad, r.checks.len());
    {
        let e = &personal;
        let tb = reports::trial_balance(&conn, e.id, None).unwrap();
        assert_eq!(tb.total_debit, tb.total_credit, "trial balance of {} balances", e.name);
        let v = means_core::hashchain::verify(&conn, e.id).unwrap();
        assert!(v.first_bad_seq.is_none(), "hash chain of {} verifies", e.name);
        eprintln!("{}: trial balance {} = {}, chain {} entries", e.name, tb.total_debit.major(), tb.total_credit.major(), v.checked);
    }
    assert!(r.created > 0);
    assert!(r.checks.iter().all(|check| check.ok));
}

#[test]
fn merge_accounts_and_apply_chart() {
    let w = world();
    let mut conn = w.db.conn();
    let groceries = accounts::ensure_account(&conn, w.entity.id, AccountType::Expense, &["Z [DEPRECATED] Groceries"], "expense", "EUR").unwrap();
    for (acc, q, d) in [(w.food.id, "3.20", "2026-02-01"), (groceries.id, "41.00", "2026-02-02"), (groceries.id, "12.50", "2026-02-03")] {
        journal::create_simple(
            &mut conn,
            journal::SimpleEntry {
                entity_id: w.entity.id,
                date: date(d),
                kind: "expense".into(),
                account_id: w.n26.id,
                contra_account_id: Some(acc),
                quantity: Decimal::from_str(q).unwrap(),
                contra_quantity: None,
                payee: "x".into(),
                notes: String::new(),
                splits: vec![],
                status: EntryStatus::Posted,
                fee: None,
                fee_account_id: None,
                origin: String::new(),
            },
        )
        .unwrap();
    }
    rules::save_rule(
        &conn,
        &Rule {
            id: 0,
            entity_id: w.entity.id,
            name: "lidl".into(),
            position: 0,
            enabled: true,
            conditions: vec![RuleCondition { field: "description".into(), op: "contains".into(), value: "lidl".into() }],
            account_id: Some(groceries.id),
            template_id: None,
            payee: String::new(),
            hits_count: 0,
            created_at: String::new(),
            tags: String::new(),
        },
    )
    .unwrap();
    // A chart with codes; the existing Food account is kept and coded.
    let nodes = vec![
        accounts::ChartNode { code: "6000".into(), path: "Expenses".into(), r#type: "expense".into(), placeholder: true, description: String::new() },
        accounts::ChartNode { code: "6200".into(), path: "Expenses:Food".into(), r#type: "expense".into(), placeholder: false, description: "meals".into() },
        accounts::ChartNode { code: "6210".into(), path: "Expenses:Groceries".into(), r#type: "expense".into(), placeholder: false, description: String::new() },
    ];
    let (created, existing) = accounts::apply_chart(&mut conn, w.entity.id, &nodes).unwrap();
    assert_eq!((created, existing), (1, 1));
    let food = accounts::get_account(&conn, w.food.id).unwrap();
    assert_eq!(food.code, "6200");
    // A differently-cased path finds the existing account instead of growing a twin.
    let same = accounts::ensure_account(&conn, w.entity.id, AccountType::Expense, &["food"], "expense", "EUR").unwrap();
    assert_eq!(same.id, w.food.id);
    let target = accounts::find_by_path(&conn, w.entity.id, "Groceries").unwrap().unwrap();
    assert_eq!(target.code, "6210");
    let m = accounts::merge_accounts(&mut conn, groceries.id, target.id).unwrap();
    assert_eq!(m.moved_postings, 2);
    assert_eq!(m.moved_rules, 1);
    assert!(m.source_deleted);
    assert!(accounts::get_account(&conn, groceries.id).is_err());
    let balances = accounts::list_accounts_with_balances(&conn, Some(w.entity.id), false, None).unwrap();
    assert_eq!(balances.iter().find(|a| a.id == target.id).unwrap().balance, d("53.50"));
    assert_eq!(rules::list_rules(&conn, Some(w.entity.id)).unwrap()[0].account_id, Some(target.id));
    let tb = reports::trial_balance(&conn, w.entity.id, None).unwrap();
    assert_eq!(tb.total_debit, tb.total_credit);
    assert!(means_core::hashchain::verify(&conn, w.entity.id).unwrap().first_bad_seq.is_none());
    // Categories may merge across income/expense; a category cannot merge into a bank account.
    assert!(matches!(accounts::merge_accounts(&mut conn, w.food.id, w.n26.id), Err(Error::Invalid(_))));
}

#[test]
fn remap_is_atomic_and_rechains_once() {
    let w = world();
    let mut conn = w.db.conn();
    let old_a = accounts::ensure_account(&conn, w.entity.id, AccountType::Expense, &["Z Transportation"], "expense", "EUR").unwrap();
    let old_b = accounts::ensure_account(&conn, w.entity.id, AccountType::Expense, &["Z Holiday"], "expense", "EUR").unwrap();
    for (acc, q, d) in [(old_a.id, "9.00", "2026-03-01"), (old_a.id, "11.00", "2026-03-02"), (old_b.id, "300.00", "2026-03-03")] {
        journal::create_simple(
            &mut conn,
            journal::SimpleEntry {
                entity_id: w.entity.id,
                date: date(d),
                kind: "expense".into(),
                account_id: w.n26.id,
                contra_account_id: Some(acc),
                quantity: Decimal::from_str(q).unwrap(),
                contra_quantity: None,
                payee: "x".into(),
                notes: String::new(),
                splits: vec![],
                status: EntryStatus::Posted,
                fee: None,
                fee_account_id: None,
                origin: String::new(),
            },
        )
        .unwrap();
    }
    let node = |code: &str, path: &str, ph: bool| accounts::ChartNode { code: code.into(), path: path.into(), r#type: "expense".into(), placeholder: ph, description: String::new() };
    let chart = vec![
        node("6000", "Expenses", true),
        node("6300", "Expenses:Transport", true),
        node("6310", "Expenses:Transport:Local transport", false),
        node("6600", "Expenses:Travel", true),
        node("6630", "Expenses:Travel:Trips", false),
    ];
    let moves = vec![
        (old_a.id, "Expenses:Transport:Local transport".to_string()),
        (old_b.id, "Expenses:Travel".to_string()), // a placeholder: reported, not applied
        (999_999, "Expenses:Travel:Trips".to_string()),
    ];
    let r = accounts::remap_accounts(&mut conn, w.entity.id, &chart, &moves).unwrap();
    assert_eq!((r.created, r.merged, r.moved_postings), (4, 1, 2));
    assert_eq!(r.skipped.len(), 2, "{:?}", r.skipped);
    let local = accounts::find_by_path(&conn, w.entity.id, "Transport:Local transport").unwrap().unwrap();
    assert_eq!(local.code, "6310");
    let balances = accounts::list_accounts_with_balances(&conn, Some(w.entity.id), false, None).unwrap();
    assert_eq!(balances.iter().find(|a| a.id == local.id).unwrap().balance, d("20.00"));
    assert!(balances.iter().any(|a| a.id == old_b.id), "the skipped account stays");
    assert!(accounts::get_account(&conn, old_a.id).is_err());
    assert!(means_core::hashchain::verify(&conn, w.entity.id).unwrap().first_bad_seq.is_none());
    let tb = reports::trial_balance(&conn, w.entity.id, None).unwrap();
    assert_eq!(tb.total_debit, tb.total_credit);
    // A busy account can become the parent of its own postings: Z Holiday -> Z Holiday:Trips.
    let trips = accounts::ensure_account(&conn, w.entity.id, AccountType::Expense, &["Z Holiday", "Trips"], "expense", "EUR").unwrap();
    let m = accounts::merge_accounts(&mut conn, old_b.id, trips.id).unwrap();
    assert_eq!((m.moved_postings, m.source_deleted), (1, false));
    let parent = accounts::get_account(&conn, old_b.id).unwrap();
    assert!(parent.placeholder);
    let balances = accounts::list_accounts_with_balances(&conn, Some(w.entity.id), false, None).unwrap();
    assert_eq!(balances.iter().find(|a| a.id == trips.id).unwrap().balance, d("300.00"));
    assert!(means_core::hashchain::verify(&conn, w.entity.id).unwrap().first_bad_seq.is_none());
}

#[test]
fn inbox_learns_and_lands_files() {
    let w = world();
    let mut conn = w.db.conn();
    let dir = std::env::temp_dir().join(format!("means-inbox-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Nothing learned yet: the file is kept as a pending import and moved to done/.
    std::fs::write(dir.join("n26-csv-transactions-2026-07.csv"), N26_CSV).unwrap();
    let out = imports::inbox::scan(&mut conn, &dir).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].action, "pending", "{}", out[0].detail);
    let pending_id = out[0].import_id.unwrap();
    assert!(dir.join("done/n26-csv-transactions-2026-07.csv").exists());
    assert!(!dir.join("n26-csv-transactions-2026-07.csv").exists());
    // One keystroke later the stored file lands, and the profile is learned from it.
    let done = imports::inbox::complete_import(&mut conn, pending_id, w.n26.id).unwrap();
    assert_eq!(done.import.created_count, 3, "{:?}", done.import);
    assert!(imports::get_import(&conn, pending_id).is_err(), "the pending row is gone");
    // The next month's file needs nobody.
    let march = N26_CSV.replace("2026-02", "2026-03");
    std::fs::write(dir.join("n26-csv-transactions-2026-08.csv"), &march).unwrap();
    let out = imports::inbox::scan(&mut conn, &dir).unwrap();
    assert_eq!(out[0].action, "imported", "{}", out[0].detail);
    // A re-dropped copy is recognized by content and filed away.
    std::fs::write(dir.join("again.csv"), &march).unwrap();
    let out = imports::inbox::scan(&mut conn, &dir).unwrap();
    assert_eq!(out[0].action, "duplicate");
    assert!(dir.join("done/again.csv").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tags_stay_outside_the_chain() {
    let w = world();
    let mut conn = w.db.conn();
    let e = journal::create_simple(
        &mut conn,
        journal::SimpleEntry {
            entity_id: w.entity.id,
            date: date("2026-05-02"),
            kind: "expense".into(),
            account_id: w.n26.id,
            contra_account_id: Some(w.food.id),
            quantity: Decimal::from_str("18.40").unwrap(),
            contra_quantity: None,
            payee: "Tasca do Chico".into(),
            notes: String::new(),
            splits: vec![],
            status: EntryStatus::Posted,
            fee: None,
            fee_account_id: None,
            origin: String::new(),
        },
    )
    .unwrap();
    let head_before = means_core::hashchain::verify(&conn, w.entity.id).unwrap().head;
    // Tagging is a lens: the chain head must not move.
    let set = means_core::tags::set_tags(&mut conn, e.id, &means_core::tags::parse("City:Lisbon trip:Alentejo-2026 #reviewed").unwrap()).unwrap();
    assert_eq!(set, vec!["city:lisbon".to_string(), "reviewed".to_string(), "trip:alentejo-2026".to_string()]);
    assert_eq!(head_before, means_core::hashchain::verify(&conn, w.entity.id).unwrap().head, "tagging must not rewrite the chain");
    // The journal filter finds tags (key:value and bare keys), and entries carry them.
    let by_tag = journal::list_entries(&conn, &journal::EntryFilter { entity_id: Some(w.entity.id), query: "trip:alentejo".into(), limit: 50, ..Default::default() }).unwrap().0;
    assert!(by_tag.iter().any(|x| x.id == e.id));
    assert!(by_tag.iter().find(|x| x.id == e.id).unwrap().tags.contains(&"city:lisbon".to_string()));
    let bare = journal::list_entries(&conn, &journal::EntryFilter { entity_id: Some(w.entity.id), query: "reviewed".into(), limit: 50, ..Default::default() }).unwrap().0;
    assert!(bare.iter().any(|x| x.id == e.id));
    // Retagging replaces.
    let set2 = means_core::tags::set_tags(&mut conn, e.id, &means_core::tags::parse("city:porto").unwrap()).unwrap();
    assert_eq!(set2, vec!["city:porto".to_string()]);
    // A rule's tags land on the entries it posts.
    rules::save_rule(
        &conn,
        &Rule {
            id: 0,
            entity_id: w.entity.id,
            name: "lidl".into(),
            position: 0,
            enabled: true,
            conditions: vec![RuleCondition { field: "description".into(), op: "contains".into(), value: "lidl".into() }],
            account_id: Some(w.food.id),
            template_id: None,
            payee: String::new(),
            hits_count: 0,
            created_at: String::new(),
            tags: "source:bank city:berlin".into(),
        },
    )
    .unwrap();
    let csv = N26_CSV.replace("2026-02", "2026-05");
    let out = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(w.n26.id), "n26-may.csv", csv.as_bytes())).unwrap();
    let lidl = out.lines.iter().find(|l| l.description.contains("LIDL")).unwrap();
    assert_eq!(means_core::tags::strings_for(&conn, lidl.journal_entry_id.unwrap()).unwrap(), vec!["city:berlin".to_string(), "source:bank".to_string()]);
    assert!(means_core::hashchain::verify(&conn, w.entity.id).unwrap().first_bad_seq.is_none());
}

#[test]
fn recategorize_splits_by_payee_with_tags() {
    let w = world();
    let mut conn = w.db.conn();
    let transport = accounts::ensure_account(&conn, w.entity.id, AccountType::Expense, &["Local transport"], "expense", "EUR").unwrap();
    let rides = accounts::ensure_account(&conn, w.entity.id, AccountType::Expense, &["Local transport", "Ridesharing"], "expense", "EUR").unwrap();
    let transit = accounts::ensure_account(&conn, w.entity.id, AccountType::Expense, &["Local transport", "Public transport"], "expense", "EUR").unwrap();
    for (payee, q) in [("Ridesharing", "7.20"), ("Ridesharing", "9.10"), ("Public Transport", "1.80"), ("", "5.00")] {
        journal::create_simple(
            &mut conn,
            journal::SimpleEntry {
                entity_id: w.entity.id,
                date: date("2026-06-01"),
                kind: "expense".into(),
                account_id: w.n26.id,
                contra_account_id: Some(transport.id),
                quantity: Decimal::from_str(q).unwrap(),
                contra_quantity: None,
                payee: payee.into(),
                notes: String::new(),
                splits: vec![],
                status: EntryStatus::Posted,
                fee: None,
                fee_account_id: None,
                origin: String::new(),
            },
        )
        .unwrap();
    }
    // Exact payee, blank-only, then the rest; classes on the new leaves; one rechain each.
    let r1 = accounts::recategorize(&mut conn, transport.id, rides.id, Some("ridesharing"), "").unwrap();
    assert_eq!((r1.entries, r1.postings), (2, 2));
    let r2 = accounts::recategorize(&mut conn, transport.id, transit.id, Some(""), "commute").unwrap();
    assert_eq!(r2.postings, 1);
    let r3 = accounts::recategorize(&mut conn, transport.id, transit.id, None, "").unwrap();
    assert_eq!(r3.postings, 1, "the remaining labeled posting moves");
    accounts::update_account(&conn, transport.id, accounts::AccountUpdate { placeholder: Some(true), ..Default::default() }).unwrap();
    accounts::update_account(&conn, rides.id, accounts::AccountUpdate { class: Some("discretionary".into()), ..Default::default() }).unwrap();
    assert!(accounts::update_account(&conn, w.n26.id, accounts::AccountUpdate { class: Some("fixed".into()), ..Default::default() }).is_err(), "class is for expense accounts");
    let balances = accounts::list_accounts_with_balances(&conn, Some(w.entity.id), false, None).unwrap();
    assert_eq!(balances.iter().find(|a| a.id == rides.id).unwrap().balance, d("16.30"));
    assert_eq!(balances.iter().find(|a| a.id == rides.id).unwrap().class, "discretionary");
    assert_eq!(balances.iter().find(|a| a.id == transit.id).unwrap().balance, d("6.80"));
    let tagged = journal::list_entries(&conn, &journal::EntryFilter { entity_id: Some(w.entity.id), query: "commute".into(), limit: 20, ..Default::default() }).unwrap().0;
    assert_eq!(tagged.len(), 1);
    assert!(means_core::hashchain::verify(&conn, w.entity.id).unwrap().first_bad_seq.is_none());
    let tb = reports::trial_balance(&conn, w.entity.id, None).unwrap();
    assert_eq!(tb.total_debit, tb.total_credit);
    let n = accounts::tag_account(&mut conn, transit.id, "transit").unwrap();
    assert_eq!(n, 2);
}

#[test]
fn machine_entries_wait_for_review() {
    let w = world();
    let mut conn = w.db.conn();
    // A human capture is reviewed by making it.
    let mine = journal::create_simple(
        &mut conn,
        journal::SimpleEntry {
            entity_id: w.entity.id,
            date: date("2026-07-01"),
            kind: "expense".into(),
            account_id: w.n26.id,
            contra_account_id: Some(w.food.id),
            quantity: Decimal::from_str("4.00").unwrap(),
            contra_quantity: None,
            payee: "Cafe".into(),
            notes: String::new(),
            splits: vec![],
            status: EntryStatus::Posted,
            fee: None,
            fee_account_id: None,
            origin: String::new(),
        },
    )
    .unwrap();
    assert!(mine.reviewed_at.is_some());
    // A rule-posted entry waits.
    rules::save_rule(
        &conn,
        &Rule {
            id: 0,
            entity_id: w.entity.id,
            name: "lidl2".into(),
            position: 0,
            enabled: true,
            conditions: vec![RuleCondition { field: "description".into(), op: "contains".into(), value: "lidl".into() }],
            account_id: Some(w.food.id),
            template_id: None,
            payee: String::new(),
            hits_count: 0,
            created_at: String::new(),
            tags: String::new(),
        },
    )
    .unwrap();
    let csv = N26_CSV.replace("2026-02", "2026-07");
    let out = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(w.n26.id), "n26-jul.csv", csv.as_bytes())).unwrap();
    let lidl = out.lines.iter().find(|l| l.description.contains("LIDL")).unwrap();
    let entry = journal::get_entry(&conn, lidl.journal_entry_id.unwrap()).unwrap();
    assert_eq!(entry.status, EntryStatus::Posted);
    assert!(entry.reviewed_at.is_none(), "a rule's decision waits for a human");
    // The unreviewed filter finds it; confirming stamps it.
    let unreviewed = journal::list_entries(&conn, &journal::EntryFilter { entity_id: Some(w.entity.id), only_unreviewed: true, limit: 50, ..Default::default() }).unwrap().0;
    assert!(unreviewed.iter().any(|e| e.id == entry.id));
    assert_eq!(journal::mark_reviewed(&conn, &[entry.id]).unwrap(), 1);
    assert_eq!(journal::mark_reviewed(&conn, &[entry.id]).unwrap(), 0, "already reviewed");
    let after = journal::list_entries(&conn, &journal::EntryFilter { entity_id: Some(w.entity.id), only_unreviewed: true, limit: 50, ..Default::default() }).unwrap().0;
    assert!(!after.iter().any(|e| e.id == entry.id));
    assert!(means_core::hashchain::verify(&conn, w.entity.id).unwrap().first_bad_seq.is_none());
}

fn lock_world(conn: &mut rusqlite::Connection, w: &World, through: &str) {
    entities::update_entity(conn, w.entity.id, "Personal", "person", "PT", Some(date(through)), false).unwrap();
}

fn ledger_snapshot(conn: &rusqlite::Connection) -> Vec<String> {
    // Include evidence, audit records and chain fields, not just account totals.
    ["journal_entries", "postings", "statement_lines", "imports", "audit_log"]
        .iter()
        .map(|table| {
            let mut stmt = conn.prepare(&format!("SELECT * FROM {table} ORDER BY id")).unwrap();
            let columns = stmt.column_count();
            stmt.query_map([], |row| Ok((0..columns).map(|i| format!("{:?}", row.get_ref(i).unwrap())).collect::<Vec<_>>().join("|"))).unwrap().collect::<Result<Vec<_>, _>>().unwrap().join("\n")
        })
        .collect()
}

fn coffee(conn: &mut rusqlite::Connection, w: &World, day: &str, status: EntryStatus) -> JournalEntry {
    let mut input = EntryInput::new(w.entity.id, date(day));
    input.status = status;
    input.postings = vec![PostingInput::new(w.n26.id, d("-3.20")), PostingInput::balancing(w.food.id)];
    journal::create_entry(conn, input).unwrap()
}

#[test]
fn rollback_cannot_remove_locked_entries_or_reconciliation_even_with_force() {
    for matched in [false, true] {
        let w = world();
        let mut conn = w.db.conn();
        if matched {
            coffee(&mut conn, &w, "2026-02-01", EntryStatus::Posted);
        }
        let out = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(w.n26.id), "n26.csv", N26_CSV.as_bytes())).unwrap();
        // Leave the earlier lines unlocked drafts so rollback must undo their deletion
        // when it reaches the locked last line.
        if !matched {
            journal::post_entry(&mut conn, out.lines[2].journal_entry_id.unwrap()).unwrap();
        }
        lock_world(&mut conn, &w, "2026-02-03");
        let before = ledger_snapshot(&conn);
        for force in [false, true] {
            assert!(matches!(imports::delete_import(&mut conn, out.import.id, force), Err(Error::Locked(_))));
            assert_eq!(ledger_snapshot(&conn), before);
        }
        entities::update_entity(&mut conn, w.entity.id, "Personal", "person", "PT", None, false).unwrap();
        imports::delete_import(&mut conn, out.import.id, false).unwrap();
    }
}

#[test]
fn rematch_preserves_both_sides_when_either_period_is_locked() {
    for (own_day, kept_day, kept_status) in [("2026-02-01", "2026-02-02", EntryStatus::Posted), ("2026-02-02", "2026-02-01", EntryStatus::Posted), ("2026-02-02", "2026-02-01", EntryStatus::Draft)] {
        let w = world();
        let mut conn = w.db.conn();
        let csv = N26_CSV.lines().take(2).collect::<Vec<_>>().join("\n").replace("2026-02-01", own_day);
        let out = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(w.n26.id), "n26.csv", csv.as_bytes())).unwrap();
        journal::post_entry(&mut conn, out.lines[0].journal_entry_id.unwrap()).unwrap();
        coffee(&mut conn, &w, kept_day, kept_status);
        lock_world(&mut conn, &w, "2026-02-01");
        let before = ledger_snapshot(&conn);
        let report = imports::rematch_import(&mut conn, out.import.id).unwrap();
        assert_eq!(report.rematched, 0);
        assert_eq!(report.untouched, 1);
        assert!(report.notes[0].contains("lock date"));
        // The pass itself is audited, but no accounting or evidence rows change.
        assert_eq!(ledger_snapshot(&conn)[..4], before[..4]);
        entities::update_entity(&mut conn, w.entity.id, "Personal", "person", "PT", None, false).unwrap();
        assert_eq!(imports::rematch_import(&mut conn, out.import.id).unwrap().rematched, 1);
    }
}

#[test]
fn revaluation_reports_locked_entries_and_still_fixes_open_entries_and_drafts() {
    let w = world();
    let mut conn = w.db.conn();
    let bank = accounts::ensure_account(&conn, w.entity.id, AccountType::Asset, &["Foreign"], "bank", "GBP").unwrap();
    let mut ids = Vec::new();
    for (day, status) in [("2026-02-01", EntryStatus::Posted), ("2026-02-02", EntryStatus::Posted), ("2026-02-01", EntryStatus::Draft)] {
        let mut input = EntryInput::new(w.entity.id, date(day));
        input.status = status;
        input.postings = vec![PostingInput::new(bank.id, d("-10")), PostingInput::balancing(w.food.id)];
        ids.push(journal::create_entry(&mut conn, input).unwrap().id);
    }
    lock_world(&mut conn, &w, "2026-02-01");
    rates::set_price(&conn, "GBP", "EUR", date("2026-02-01"), d("1.2"), "manual").unwrap();
    let before = ledger_snapshot(&conn);
    assert!(matches!(journal::revalue_entry(&mut conn, ids[0], true), Err(Error::Locked(_))));
    assert_eq!(ledger_snapshot(&conn), before);
    let locked = serde_json::to_value(journal::get_entry(&conn, ids[0]).unwrap()).unwrap();
    let report = rates::revalue_missing(&mut conn).unwrap();
    assert_eq!(report.fixed, 2);
    assert_eq!(report.failed, 1);
    assert!(report.first_error.unwrap().contains("lock date"));
    assert_eq!(serde_json::to_value(journal::get_entry(&conn, ids[0]).unwrap()).unwrap(), locked);
    for id in &ids[1..] {
        assert_eq!(journal::get_entry(&conn, *id).unwrap().postings[0].amount.major(), d("-12"));
    }
    assert!(means_core::hashchain::verify(&conn, w.entity.id).unwrap().first_bad_seq.is_none());
    let before = ledger_snapshot(&conn);
    assert_eq!(rates::revalue_missing(&mut conn).unwrap().fixed, 0);
    assert_eq!(ledger_snapshot(&conn), before);
}

fn unmatched_coffee(conn: &mut rusqlite::Connection, w: &World) -> i64 {
    let csv = N26_CSV.lines().take(2).collect::<Vec<_>>().join("\n");
    let out = imports::run_import(conn, imports::ImportRequest::new("auto", Some(w.n26.id), "n26.csv", csv.as_bytes())).unwrap();
    journal::delete_entry(conn, out.lines[0].journal_entry_id.unwrap()).unwrap();
    conn.execute("UPDATE statement_lines SET status = 'unmatched' WHERE id = ?1", [out.lines[0].id]).unwrap();
    out.lines[0].id
}

#[test]
fn manual_matching_rejects_invalid_evidence_without_mutation() {
    for case in ["amount", "sign", "missing", "void"] {
        let w = world();
        let mut conn = w.db.conn();
        let line = unmatched_coffee(&mut conn, &w);
        let entry = coffee(&mut conn, &w, "2026-02-01", EntryStatus::Draft);
        match case {
            "amount" => {
                conn.execute("UPDATE statement_lines SET amount = -10000 WHERE id = ?1", [line]).unwrap();
            }
            "sign" => {
                conn.execute("UPDATE statement_lines SET amount = 320 WHERE id = ?1", [line]).unwrap();
            }
            "missing" => {
                conn.execute("UPDATE statement_lines SET amount = NULL WHERE id = ?1", [line]).unwrap();
            }
            "void" => {
                journal::post_entry(&mut conn, entry.id).unwrap();
                journal::void_entry(&mut conn, entry.id, None, "test").unwrap();
            }
            _ => unreachable!(),
        }
        let before = ledger_snapshot(&conn);
        assert!(matches!(means_core::matcher::match_line(&mut conn, line, entry.postings[0].id), Err(Error::Invalid(_))), "{case}");
        assert_eq!(ledger_snapshot(&conn), before, "{case}");
    }
}

#[test]
fn matching_posts_and_links_atomically() {
    for status in [EntryStatus::Draft, EntryStatus::Posted] {
        let w = world();
        let mut conn = w.db.conn();
        let line = unmatched_coffee(&mut conn, &w);
        let entry = coffee(&mut conn, &w, "2026-02-01", status);
        let before = ledger_snapshot(&conn);
        conn.execute_batch("CREATE TEMP TRIGGER reject_match BEFORE UPDATE OF posting_id ON statement_lines BEGIN SELECT RAISE(ABORT, 'injected link failure'); END;").unwrap();
        assert!(means_core::matcher::match_line(&mut conn, line, entry.postings[0].id).is_err());
        assert_eq!(ledger_snapshot(&conn), before);
        conn.execute_batch("DROP TRIGGER reject_match;").unwrap();
        let matched = means_core::matcher::match_line(&mut conn, line, entry.postings[0].id).unwrap();
        assert_eq!(matched.status, "matched");
        assert_eq!(matched.posting_id, Some(entry.postings[0].id));
        assert_eq!(matched.journal_entry_id, Some(entry.id));
        let posted = journal::get_entry(&conn, entry.id).unwrap();
        assert_eq!(posted.status, EntryStatus::Posted);
        assert!(posted.postings[0].reconciled_at.is_some());
        assert!(means_core::hashchain::verify(&conn, w.entity.id).unwrap().first_bad_seq.is_none());
    }
}

#[test]
fn failed_import_lines_roll_back_entries_tags_evidence_and_rule_hits_then_retry_once() {
    for mode in ["draft", "rule", "template"] {
        let w = world();
        let mut conn = w.db.conn();
        if mode != "draft" {
            let template_id = if mode == "template" {
                Some(
                    means_core::templates::save_template(
                        &conn,
                        &EntryTemplate {
                            id: 0,
                            uid: String::new(),
                            entity_id: w.entity.id,
                            name: "Food".into(),
                            payee: String::new(),
                            description: String::new(),
                            lines: vec![
                                TemplateLine { account_id: w.n26.id, method: "input".into(), value: None, of_line: None, memo: String::new(), label: "Bank".into() },
                                TemplateLine { account_id: w.food.id, method: "balance".into(), value: None, of_line: None, memo: String::new(), label: String::new() },
                            ],
                            rrule: String::new(),
                            starts_on: None,
                            next_on: None,
                            ends_on: None,
                            auto_post: false,
                            lead_days: 0,
                            version: 1,
                            active: true,
                            created_at: String::new(),
                        },
                    )
                    .unwrap()
                    .id,
                )
            } else {
                None
            };
            rules::save_rule(
                &conn,
                &Rule {
                    id: 0,
                    entity_id: w.entity.id,
                    name: "All".into(),
                    position: 0,
                    enabled: true,
                    conditions: vec![RuleCondition { field: "description".into(), op: "regex".into(), value: ".*".into() }],
                    account_id: if template_id.is_none() { Some(w.food.id) } else { None },
                    template_id,
                    payee: String::new(),
                    hits_count: 0,
                    created_at: String::new(),
                    tags: "trip:atomic".into(),
                },
            )
            .unwrap();
        }
        // The second line fails after creation and tagging; other lines still succeed.
        conn.execute_batch("CREATE TEMP TRIGGER fail_link BEFORE UPDATE OF journal_entry_id ON statement_lines WHEN NEW.position = 2 AND NEW.status = 'created' BEGIN SELECT RAISE(ABORT, 'injected link failure'); END;").unwrap();
        let out = imports::run_import(&mut conn, imports::ImportRequest::new("n26_csv", Some(w.n26.id), "n26.csv", N26_CSV.as_bytes())).unwrap();
        assert_eq!((out.import.created_count, out.import.error_count), (2, 1), "{mode}");
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM journal_entries", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
        assert!(out.lines[1].journal_entry_id.is_none());
        let untouched = [out.lines[0].journal_entry_id.unwrap(), out.lines[2].journal_entry_id.unwrap()].map(|id| serde_json::to_value(journal::get_entry(&conn, id).unwrap()).unwrap());
        if mode != "draft" {
            assert_eq!(rules::list_rules(&conn, Some(w.entity.id)).unwrap()[0].hits_count, 2);
            assert_eq!(conn.query_row("SELECT COUNT(*) FROM entry_tags", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
        }
        assert_eq!(imports::retry_import(&mut conn, out.import.id).unwrap().error_count, 1);
        conn.execute_batch("DROP TRIGGER fail_link;").unwrap();
        // Also inject a failure after evidence linking, when the rule hit is counted.
        if mode != "draft" {
            conn.execute_batch("CREATE TEMP TRIGGER fail_hit BEFORE UPDATE OF hits_count ON rules BEGIN SELECT RAISE(ABORT, 'injected hit failure'); END;").unwrap();
            assert_eq!(imports::retry_import(&mut conn, out.import.id).unwrap().error_count, 1);
            assert_eq!(conn.query_row("SELECT COUNT(*) FROM journal_entries", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
            conn.execute_batch("DROP TRIGGER fail_hit;").unwrap();
        }
        if mode == "draft" {
            // A stopped process can leave a persisted line unprocessed and counters stale.
            conn.execute("UPDATE statement_lines SET status = 'unmatched', note = '' WHERE id = ?1", [out.lines[1].id]).unwrap();
            conn.execute("UPDATE imports SET created_count = 0, error_count = 0 WHERE id = ?1", [out.import.id]).unwrap();
        }
        let done = imports::retry_import(&mut conn, out.import.id).unwrap();
        assert_eq!((done.created_count, done.error_count), (3, 0));
        let (_, lines) = imports::get_import(&conn, out.import.id).unwrap();
        let entries = lines.iter().map(|l| serde_json::to_value(journal::get_entry(&conn, l.journal_entry_id.unwrap()).unwrap()).unwrap()).collect::<Vec<_>>();
        assert_eq!(entries[0], untouched[0]);
        assert_eq!(entries[2], untouched[1]);
        assert_eq!(imports::retry_import(&mut conn, out.import.id).unwrap().created_count, 3);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM journal_entries", [], |r| r.get::<_, i64>(0)).unwrap(), 3);
        if mode != "draft" {
            assert_eq!(rules::list_rules(&conn, Some(w.entity.id)).unwrap()[0].hits_count, 3);
        }
        assert!(means_core::hashchain::verify(&conn, w.entity.id).unwrap().first_bad_seq.is_none());
    }
}

#[test]
fn category_split_filters_all_matching_entries_atomically_and_rechains_once() {
    let w = world();
    let mut conn = w.db.conn();
    let target = accounts::ensure_account(&conn, w.entity.id, AccountType::Expense, &["Delivery"], "expense", "EUR").unwrap();
    let mut ids = Vec::new();
    for (payee, description, memo) in [("Deliveroo", "", ""), ("Lidl", "", ""), ("Cafe", "DELIVERY dinner", ""), ("Shop", "", "delivery fee")] {
        let mut input = EntryInput::new(w.entity.id, date("2026-02-01"));
        input.status = EntryStatus::Posted;
        input.payee = payee.into();
        input.description = description.into();
        input.postings = vec![PostingInput::new(w.n26.id, d("-10")), PostingInput::new(w.food.id, d("10")).memo(memo)];
        let entry = journal::create_entry(&mut conn, input).unwrap();
        conn.execute("UPDATE postings SET reconciled_at = 'test' WHERE id = ?1", [entry.postings[0].id]).unwrap();
        ids.push(entry.id);
    }
    let before = ledger_snapshot(&conn);
    let balance = reports::trial_balance(&conn, w.entity.id, None).unwrap();
    let preview = accounts::split_category(&mut conn, w.food.id, target.id, "deliver", true).unwrap();
    assert_eq!((preview.entries, preview.postings), (3, 3));
    assert_eq!(ledger_snapshot(&conn), before);
    conn.execute_batch(
        "CREATE TEMP TABLE chain_writes (entry_id INTEGER); CREATE TEMP TRIGGER count_rechain AFTER UPDATE OF hash ON journal_entries BEGIN INSERT INTO chain_writes VALUES (NEW.id); END;",
    )
    .unwrap();
    let result = accounts::split_category(&mut conn, w.food.id, target.id, "deliver", false).unwrap();
    assert_eq!((result.entries, result.postings), (3, 3));
    for (index, id) in ids.iter().enumerate() {
        let entry = journal::get_entry(&conn, *id).unwrap();
        assert_eq!(entry.postings[1].account_id, if index == 1 { w.food.id } else { target.id });
        assert_eq!(entry.postings[0].reconciled_at.as_deref(), Some("test"));
    }
    let after = reports::trial_balance(&conn, w.entity.id, None).unwrap();
    assert_eq!((after.total_debit, after.total_credit), (balance.total_debit, balance.total_credit));
    assert_eq!(conn.query_row("SELECT MAX(n) FROM (SELECT COUNT(*) n FROM chain_writes GROUP BY entry_id)", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    assert!(means_core::hashchain::verify(&conn, w.entity.id).unwrap().first_bad_seq.is_none());
    assert_eq!(accounts::split_category(&mut conn, w.food.id, target.id, "%", false).unwrap().entries, 0, "filter is literal, not SQL wildcard");
}

#[test]
fn category_split_refuses_locked_reconciled_and_incompatible_moves_without_changes() {
    for mode in ["locked", "reconciled", "commodity", "failure"] {
        let w = world();
        let mut conn = w.db.conn();
        let target = accounts::ensure_account(&conn, w.entity.id, AccountType::Expense, &["Delivery"], "expense", if mode == "commodity" { "JPY" } else { "EUR" }).unwrap();
        for day in ["2026-02-02", "2026-02-01"] {
            let entry = coffee(&mut conn, &w, day, EntryStatus::Posted);
            if mode == "reconciled" && day.ends_with("01") {
                conn.execute("UPDATE postings SET reconciled_at = 'test' WHERE id = ?1", [entry.postings[1].id]).unwrap();
            }
            conn.execute("UPDATE journal_entries SET description = 'split' WHERE id = ?1", [entry.id]).unwrap();
        }
        means_core::hashchain::rechain(&conn, w.entity.id, 1).unwrap();
        if mode == "locked" {
            lock_world(&mut conn, &w, "2026-02-01");
        }
        if mode == "failure" {
            conn.execute_batch("CREATE TEMP TRIGGER fail_split BEFORE UPDATE OF account_id ON postings WHEN NEW.account_id <> OLD.account_id BEGIN SELECT RAISE(ABORT, 'injected failure'); END;")
                .unwrap();
        }
        let before = ledger_snapshot(&conn);
        assert!(accounts::split_category(&mut conn, w.food.id, target.id, "split", false).is_err(), "{mode}");
        assert_eq!(ledger_snapshot(&conn), before, "{mode}");
    }
}

#[test]
fn email_attachments_use_original_names_for_inbox_routes() {
    let w = world();
    let mut conn = w.db.conn();
    let dir = std::env::temp_dir().join(format!("means-email-route-{}", means_core::new_uid()));
    conn.execute("INSERT INTO import_profiles (source, filename_glob, account_id, hits_count, updated_at) VALUES ('n26_csv', 'Bank Statement *.csv', ?1, 1, '2026-09-19')", [w.n26.id]).unwrap();
    let raw = format!("MIME-Version: 1.0\r\nContent-Type: text/csv\r\nContent-Disposition: attachment; filename=\"Bank Statement 2026.csv\"\r\n\r\n{N26_CSV}");
    let delivery = imports::email::deliver(&dir, raw.as_bytes(), false).unwrap();
    let out = imports::inbox::scan(&mut conn, &dir).unwrap();
    assert_eq!(out[0].action, "imported", "{}", out[0].detail);
    let imported = imports::get_import(&conn, out[0].import_id.unwrap()).unwrap().0;
    assert_eq!(imported.filename, "Bank Statement 2026.csv");
    assert_eq!(imported.account_id, Some(w.n26.id));
    assert!(dir.join("done").join(&delivery.filenames[0]).exists());
    let raw = raw.replace("Bank Statement", "Other Bank").replace("2026-02", "2026-03");
    imports::email::deliver(&dir, raw.as_bytes(), false).unwrap();
    // The previous import also learned a source default. Remove that fallback.
    conn.execute("DELETE FROM import_profiles WHERE filename_glob = ''", []).unwrap();
    let out = imports::inbox::scan(&mut conn, &dir).unwrap();
    assert_eq!(out[0].action, "pending");
    assert_eq!(imports::get_import(&conn, out[0].import_id.unwrap()).unwrap().0.filename, "Other Bank 2026.csv");
    std::fs::remove_dir_all(dir).unwrap();
}
