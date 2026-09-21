//! Editing a journal must preserve the identity of its evidence and its booked valuation.
use chrono::NaiveDate;
use means_core::model::*;
use means_core::{accounts, entities, hashchain, imports, journal, rates, Db};
use rust_decimal::Decimal;

fn date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 2, 1).unwrap()
}
fn d(v: &str) -> Decimal {
    v.parse().unwrap()
}
fn roundtrip(e: &JournalEntry) -> EntryInput {
    let mut i = EntryInput::new(e.entity_id, e.date);
    i.status = e.status;
    i.payee = e.payee.clone();
    i.postings = e
        .postings
        .iter()
        .map(|p| PostingInput {
            account_id: p.account_id,
            quantity: p.quantity.major(),
            amount: Some(p.amount.major()),
            memo: p.memo.clone(),
            metadata: p.metadata.clone(),
            external_id: p.external_id.clone(),
            fingerprint: p.fingerprint.clone(),
            ..Default::default()
        })
        .collect();
    i
}

#[test]
fn edit_keeps_each_statement_on_its_own_posting_including_equal_amounts() {
    for amounts in [["-10", "-20"], ["-10", "-10"]] {
        let db = Db::open_memory().unwrap();
        let mut c = db.conn();
        let e = entities::create_entity(&mut c, "Audit", "person", "PT", "EUR").unwrap();
        let bank = accounts::ensure_account(&c, e.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
        let food = accounts::ensure_account(&c, e.id, AccountType::Expense, &["Food"], "expense", "EUR").unwrap();
        let mut i = EntryInput::new(e.id, date());
        i.postings = vec![PostingInput::new(bank.id, d(amounts[0])), PostingInput::new(bank.id, d(amounts[1])), PostingInput::balancing(food.id)];
        let entry = journal::create_entry(&mut c, i).unwrap();
        let csv = format!("\"Booking Date\",\"Partner Name\",\"Amount (EUR)\"\n\"2026-02-01\",\"First\",\"{}\"\n\"2026-02-01\",\"Second\",\"{}\"\n", amounts[0], amounts[1]);
        let out = imports::run_import(&mut c, imports::ImportRequest::new("n26_csv", Some(bank.id), "audit.csv", csv.as_bytes())).unwrap();
        assert_eq!(out.import.matched_count, 2);
        let before = journal::get_entry(&c, entry.id).unwrap();
        let positions: Vec<_> = out.lines.iter().map(|l| before.postings.iter().position(|p| Some(p.id) == l.posting_id).unwrap()).collect();
        let mut i = roundtrip(&before);
        i.payee = "Edited payee".into();
        let after = journal::update_entry(&mut c, before.id, i).unwrap();
        for (line, position) in out.lines.iter().zip(positions) {
            let linked = imports::get_line(&c, line.id).unwrap();
            assert_eq!(linked.posting_id, Some(after.postings[position].id));
            assert_eq!(linked.amount.as_ref(), Some(&after.postings[position].quantity));
            assert_eq!(after.postings[position].reconciled_at, before.postings[position].reconciled_at);
        }
        // One replacement cannot satisfy two reconciled postings, even when their values coincide.
        let snapshot = serde_json::to_value(&after).unwrap();
        let mut i = roundtrip(&after);
        i.postings.remove(1);
        i.postings[1] = PostingInput::balancing(food.id);
        assert!(journal::update_entry(&mut c, after.id, i).is_err());
        assert_eq!(serde_json::to_value(journal::get_entry(&c, after.id).unwrap()).unwrap(), snapshot);
        assert!(hashchain::verify(&c, e.id).unwrap().first_bad_seq.is_none());
    }
}

#[test]
fn explicit_roundtrip_keeps_missing_rate_until_rates_arrive() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let e = entities::create_entity(&mut c, "Audit", "person", "PT", "EUR").unwrap();
    let bank = accounts::ensure_account(&c, e.id, AccountType::Asset, &["USD"], "bank", "USD").unwrap();
    let food = accounts::ensure_account(&c, e.id, AccountType::Expense, &["Food"], "expense", "EUR").unwrap();
    let mut i = EntryInput::new(e.id, date());
    i.status = EntryStatus::Draft;
    i.postings = vec![PostingInput::new(bank.id, d("-10")), PostingInput::balancing(food.id)];
    let before = journal::create_entry(&mut c, i).unwrap();
    let mut i = roundtrip(&before);
    i.status = EntryStatus::Posted;
    let after = journal::update_entry(&mut c, before.id, i).unwrap();
    assert_eq!(after.postings[0].rate_source, "missing");
    rates::set_price(&c, "USD", "EUR", date(), d("0.9"), "manual").unwrap();
    assert_eq!(rates::revalue_missing(&mut c).unwrap().fixed, 1);
    let valued = journal::get_entry(&c, before.id).unwrap();
    assert_eq!(valued.postings[0].amount.major(), d("-9"));
    assert_eq!(valued.postings[1].amount.major(), d("9"));
    assert!(hashchain::verify(&c, e.id).unwrap().first_bad_seq.is_none());
}

#[test]
fn descriptive_roundtrip_preserves_explicit_fx_and_metadata_but_accepts_new_value() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let e = entities::create_entity(&mut c, "Audit", "person", "PT", "EUR").unwrap();
    let bank = accounts::ensure_account(&c, e.id, AccountType::Asset, &["USD"], "bank", "USD").unwrap();
    let food = accounts::ensure_account(&c, e.id, AccountType::Expense, &["Food"], "expense", "EUR").unwrap();
    let mut i = EntryInput::new(e.id, date());
    i.postings = vec![
        PostingInput { amount: Some(d("-9")), metadata: serde_json::json!({"original": {"amount":"-10","currency":"USD"},"booked_on":"2026-02-02"}), ..PostingInput::new(bank.id, d("-10")) },
        PostingInput::balancing(food.id),
    ];
    let before = journal::create_entry(&mut c, i).unwrap();
    rates::set_price(&c, "USD", "EUR", date(), d("0.8"), "manual").unwrap();
    let mut i = roundtrip(&before);
    i.description = "Description changed".into();
    let after = journal::update_entry(&mut c, before.id, i).unwrap();
    assert_eq!(after.postings[0].amount, before.postings[0].amount);
    assert_eq!(after.postings[0].metadata, before.postings[0].metadata);
    assert_eq!(after.postings[0].rate, before.postings[0].rate);
    let mut i = roundtrip(&after);
    i.postings[0].amount = Some(d("-7"));
    i.postings[1] = PostingInput::balancing(food.id);
    let changed = journal::update_entry(&mut c, after.id, i).unwrap();
    assert_eq!(changed.postings[0].rate, Some(d("0.7")));
}

#[test]
fn revaluation_keeps_repeated_account_statement_links_distinct() {
    for amounts in [["-10", "-20"], ["-10", "-10"]] {
        let db = Db::open_memory().unwrap();
        let mut c = db.conn();
        let e = entities::create_entity(&mut c, "Audit", "person", "PT", "EUR").unwrap();
        let bank = accounts::ensure_account(&c, e.id, AccountType::Asset, &["USD"], "bank", "USD").unwrap();
        let food = accounts::ensure_account(&c, e.id, AccountType::Expense, &["Food"], "expense", "EUR").unwrap();
        let mut i = EntryInput::new(e.id, date());
        i.postings = vec![PostingInput::new(bank.id, d(amounts[0])), PostingInput::new(bank.id, d(amounts[1])), PostingInput::balancing(food.id)];
        let entry = journal::create_entry(&mut c, i).unwrap();
        let csv = format!(
            "\"Date (UTC)\",\"Description\",\"Amount\",\"Status\",\"Transaction ID\"\n\"02-01-2026\",\"First\",\"{}\",\"Sent\",\"bank-1\"\n\"02-01-2026\",\"Second\",\"{}\",\"Sent\",\"bank-2\"\n",
            amounts[0], amounts[1]
        );
        let out = imports::run_import(&mut c, imports::ImportRequest::new("mercury_csv", Some(bank.id), "audit.csv", csv.as_bytes())).unwrap();
        assert_eq!(out.import.matched_count, 2);
        rates::set_price(&c, "USD", "EUR", date(), d("0.9"), "manual").unwrap();
        assert_eq!(rates::revalue_missing(&mut c).unwrap().fixed, 1);
        let after = journal::get_entry(&c, entry.id).unwrap();
        for line in &out.lines {
            let linked = imports::get_line(&c, line.id).unwrap();
            let posting = after.postings.iter().find(|p| Some(p.id) == linked.posting_id).unwrap();
            assert_eq!(posting.external_id.as_deref(), Some(line.reference.as_str()));
            assert_eq!(Some(&posting.quantity), linked.amount.as_ref());
        }
        // Reordering equal-valued postings also keeps their distinct bank references attached.
        let mut input = roundtrip(&after);
        input.payee = "Reordered".into();
        input.postings.swap(0, 1);
        let reordered = journal::update_entry(&mut c, entry.id, input).unwrap();
        for line in &out.lines {
            let linked = imports::get_line(&c, line.id).unwrap();
            let posting = reordered.postings.iter().find(|p| Some(p.id) == linked.posting_id).unwrap();
            assert_eq!(posting.external_id.as_deref(), Some(line.reference.as_str()));
        }
        assert!(hashchain::verify(&c, e.id).unwrap().first_bad_seq.is_none());
    }
}

#[test]
fn date_correction_keeps_missing_rate_and_real_one_to_one_rate_clears_it() {
    for rate in ["0.9", "1"] {
        let db = Db::open_memory().unwrap();
        let mut c = db.conn();
        let entity = entities::create_entity(&mut c, "Audit", "person", "PT", "EUR").unwrap();
        let bank = accounts::ensure_account(&c, entity.id, AccountType::Asset, &["USD"], "bank", "USD").unwrap();
        let food = accounts::ensure_account(&c, entity.id, AccountType::Expense, &["Food"], "expense", "EUR").unwrap();
        let mut input = EntryInput::new(entity.id, date());
        input.postings = vec![PostingInput::new(bank.id, d("-10")), PostingInput::balancing(food.id)];
        let before = journal::create_entry(&mut c, input).unwrap();
        let mut edit = roundtrip(&before);
        edit.date = date().succ_opt().unwrap();
        let after = journal::update_entry(&mut c, before.id, edit).unwrap();
        assert_eq!(after.postings[0].rate_source, "missing", "date-only edits must not invent a manual rate");
        assert_eq!(after.postings[0].amount, before.postings[0].amount);
        rates::set_price(&c, "USD", "EUR", after.date, d(rate), "manual").unwrap();
        assert_eq!(rates::revalue_missing(&mut c).unwrap().fixed, 1);
        let valued = journal::get_entry(&c, before.id).unwrap();
        assert_eq!(valued.postings[0].amount.major(), d("-10") * d(rate));
        assert_ne!(valued.postings[0].rate_source, "missing", "a real 1:1 rate must clear the missing marker too");
        assert_eq!(rates::revalue_missing(&mut c).unwrap().fixed, 0);
        assert!(hashchain::verify(&c, entity.id).unwrap().first_bad_seq.is_none());
    }
}
