use means_core::{accounts, entities, hashchain, imports, journal, rates, splits::Leg, tags, AccountType, Db, EntryStatus};
use rust_decimal::Decimal;
use serde_json::json;
fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}
fn leg(id: i64, value: &str) -> Leg {
    Leg { account_id: id, quantity: if value.is_empty() { None } else { Some(d(value)) }, memo: String::new() }
}

#[test]
fn imported_splits_preserve_bank_row_evidence_tags_and_book_values() {
    for (native, functional, amount, first, second) in
        [("USD", "USD", "-90.50", "72.40", "18.10"), ("EUR", "USD", "-90.50", "72.40", "18.10"), ("EUR", "USD", "90.50", "72.40", "18.10"), ("EUR", "EUR", "-10.01", "5.005", "5.005")]
    {
        let db = Db::open_memory().unwrap();
        let mut c = db.conn();
        let entity = entities::create_entity(&mut c, "Personal", "person", "US", functional).unwrap();
        let bank = accounts::ensure_account(&c, entity.id, AccountType::Asset, &["Bank"], "bank", native).unwrap();
        let a = accounts::ensure_account(&c, entity.id, AccountType::Expense, &["Groceries"], "expense", functional).unwrap();
        let b = accounts::ensure_account(&c, entity.id, AccountType::Expense, &["Household"], "expense", native).unwrap();
        let date = "2026-09-19".parse().unwrap();
        if native != functional {
            rates::set_price(&c, native, functional, date, d("1.2"), "test").unwrap();
        }
        let payload = json!({"channel":"wise","version":1,"account":{"id":"1:2","profile_id":1,"balance_id":2,"currency":native},"transactions":[{"referenceNumber":"TEST-1","type":if amount.starts_with('-'){"DEBIT"}else{"CREDIT"},"date":"2026-09-19T00:00:00Z","amount":{"value":amount,"currency":native},"details":{"description":"Shop"}}]});
        imports::run_import(&mut c, imports::ImportRequest::new("wise_json", Some(bank.id), "test.json", &serde_json::to_vec(&payload).unwrap())).unwrap();
        let line = imports::list_lines(&c, Some(bank.id), "created", None, 10).unwrap().remove(0);
        let id = line.journal_entry_id.unwrap();
        tags::set_tags(&mut c, id, &[("trip".into(), "test".into())]).unwrap();
        let before = journal::get_entry(&c, id).unwrap();
        let bank_before = before.postings.iter().find(|p| p.account_id == bank.id).unwrap();
        // A later rate correction must not revalue the bank or the split's booked total.
        if native != functional {
            rates::set_price(&c, native, functional, date, d("1.5"), "later").unwrap();
        }
        let after = journal::post_draft_to(&mut c, id, &[leg(a.id, first), leg(b.id, if amount == "-10.01" { second } else { "" })], None).unwrap();
        assert_eq!(after.status, EntryStatus::Posted);
        assert!(after.reviewed_at.is_some());
        assert_eq!(after.tags, before.tags);
        let bank_after = after.postings.iter().find(|p| p.account_id == bank.id).unwrap();
        assert_eq!(serde_json::to_value(bank_before).unwrap(), serde_json::to_value(bank_after).unwrap());
        assert_eq!(serde_json::to_value(&line).unwrap(), serde_json::to_value(imports::get_line(&c, line.id).unwrap()).unwrap());
        assert_eq!(after.postings.iter().map(|p| p.amount.minor()).sum::<i64>(), 0);
        let final_leg = after.postings.iter().find(|p| p.account_id == b.id).unwrap();
        assert_eq!(final_leg.metadata["balance"], true);
        assert_ne!(after.postings.iter().find(|p| p.account_id == a.id).unwrap().metadata["balance"], true);
        let sign = if amount.starts_with('-') { Decimal::ONE } else { Decimal::NEGATIVE_ONE };
        assert_eq!(
            final_leg.quantity.major(),
            if second == "5.005" {
                d("5.01")
            } else if second == "-1.995" {
                d("-1.99")
            } else {
                d(second) * sign
            }
        );
        assert!(hashchain::verify(&c, entity.id).unwrap().first_bad_seq.is_none());
        assert!(journal::post_draft_to(&mut c, id, &[leg(a.id, "")], None).is_err());
        let proposal = journal::PostingProposal::Categorize { id, legs: vec![leg(a.id, "2"), leg(b.id, "3"), leg(a.id, "")], payee: None, existing: true };
        let audits: i64 = c.query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0)).unwrap();
        let (_, token) = journal::confirm_posting(&mut c, proposal.clone(), None).unwrap();
        assert_eq!(c.query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get::<_, i64>(0)).unwrap(), audits);
        assert_eq!(serde_json::to_value(journal::get_entry(&c, id).unwrap()).unwrap(), serde_json::to_value(&after).unwrap());
        assert!(hashchain::verify(&c, entity.id).unwrap().first_bad_seq.is_none());
        assert!(journal::confirm_posting(&mut c, proposal.clone(), Some("wrong token")).is_err());
        let (resplit, _) = journal::confirm_posting(&mut c, proposal.clone(), Some(&token)).unwrap();
        assert_eq!(resplit.postings.len(), 4);
        assert_eq!(resplit.postings.iter().map(|p| p.amount.minor()).sum::<i64>(), 0);
        assert_eq!(serde_json::to_value(resplit.postings.iter().find(|p| p.account_id == bank.id).unwrap()).unwrap(), serde_json::to_value(bank_before).unwrap());
        assert_eq!(serde_json::to_value(imports::get_line(&c, line.id).unwrap()).unwrap(), serde_json::to_value(&line).unwrap());
        assert_eq!(resplit.tags, before.tags);
        assert!(hashchain::verify(&c, entity.id).unwrap().first_bad_seq.is_none());
        assert!(journal::confirm_posting(&mut c, proposal.clone(), Some(&token)).is_err(), "a consumed preview must not replay");
        // Reconciled category allocations and locked periods are protected.
        c.execute("UPDATE postings SET reconciled_at='test' WHERE id=?1", [resplit.postings.iter().find(|p| p.account_id == a.id).unwrap().id]).unwrap();
        assert!(journal::confirm_posting(&mut c, proposal.clone(), None).is_err());
        c.execute("UPDATE postings SET reconciled_at=NULL WHERE journal_entry_id=?1 AND account_id=?2", [id, a.id]).unwrap();
        entities::update_entity(&mut c, entity.id, "Personal", "person", "US", Some(date), false).unwrap();
        assert!(journal::confirm_posting(&mut c, proposal, None).is_err());
    }
}

#[test]
fn invalid_splits_are_atomic() {
    use means_core::{EntryInput, PostingInput};
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let e = entities::create_entity(&mut c, "Personal", "person", "US", "USD").unwrap();
    let other = entities::create_entity(&mut c, "Other", "company", "US", "USD").unwrap();
    let bank = accounts::ensure_account(&c, e.id, AccountType::Asset, &["Bank"], "bank", "USD").unwrap();
    let category = accounts::ensure_account(&c, e.id, AccountType::Expense, &["Food"], "expense", "USD").unwrap();
    let wrong = accounts::ensure_account(&c, other.id, AccountType::Expense, &["Food"], "expense", "USD").unwrap();
    let suspense = accounts::find_by_role(&c, e.id, "suspense").unwrap();
    let mut input = EntryInput::new(e.id, "2026-09-19".parse().unwrap());
    input.status = EntryStatus::Draft;
    input.postings = vec![PostingInput::new(bank.id, d("-10")), PostingInput::balancing(suspense.id)];
    let entry = journal::create_entry(&mut c, input).unwrap();
    for legs in [
        vec![],
        vec![leg(category.id, "9")],
        vec![leg(category.id, "11")],
        vec![leg(category.id, "0"), leg(category.id, "10")],
        vec![leg(category.id, "-1"), leg(category.id, "10")],
        vec![leg(category.id, ""), leg(category.id, "2")],
        vec![leg(category.id, "10"), leg(category.id, "")],
        vec![leg(category.id, "2"), leg(wrong.id, "")],
        vec![leg(suspense.id, "")],
        vec![leg(bank.id, "")],
    ] {
        assert!(journal::post_draft_to(&mut c, entry.id, &legs, Some("Changed")).is_err());
        assert_eq!(serde_json::to_value(journal::get_entry(&c, entry.id).unwrap()).unwrap(), serde_json::to_value(&entry).unwrap());
    }
}

#[test]
fn capture_preview_is_rolled_back_and_requires_an_unchanged_proposal() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let e = entities::create_entity(&mut c, "Personal", "person", "US", "USD").unwrap();
    let bank = accounts::ensure_account(&c, e.id, AccountType::Asset, &["Bank"], "bank", "USD").unwrap();
    let food = accounts::ensure_account(&c, e.id, AccountType::Expense, &["Food"], "expense", "USD").unwrap();
    let proposal = journal::PostingProposal::Capture {
        entry: journal::SimpleEntry {
            entity_id: e.id,
            date: "2026-09-19".parse().unwrap(),
            kind: "expense".into(),
            account_id: bank.id,
            contra_account_id: Some(food.id),
            quantity: d("10"),
            contra_quantity: None,
            payee: "Shop".into(),
            notes: String::new(),
            splits: vec![],
            status: EntryStatus::Posted,
            fee: None,
            fee_account_id: None,
            origin: String::new(),
        },
        tags: "trip:test".into(),
    };
    let count = |c: &rusqlite::Connection| -> (i64, i64, i64) {
        c.query_row("SELECT (SELECT COUNT(*) FROM journal_entries),(SELECT COUNT(*) FROM audit_log),(SELECT COUNT(*) FROM entry_tags)", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap()
    };
    let before = count(&c);
    let (preview, token) = journal::confirm_posting(&mut c, proposal.clone(), None).unwrap();
    assert_eq!(count(&c), before);
    let mut changed = proposal.clone();
    if let journal::PostingProposal::Capture { entry, .. } = &mut changed {
        entry.quantity = d("11");
    }
    assert!(journal::confirm_posting(&mut c, changed, Some(&token)).is_err());
    assert_eq!(count(&c), before);
    let (posted, _) = journal::confirm_posting(&mut c, proposal.clone(), Some(&token)).unwrap();
    assert_eq!(posted.tags, vec!["trip:test"]);
    assert_eq!(posted.postings.iter().map(|p| p.amount.minor()).collect::<Vec<_>>(), preview.postings.iter().map(|p| p.amount.minor()).collect::<Vec<_>>());
    assert!(journal::confirm_posting(&mut c, proposal, Some(&token)).is_err());
    assert_eq!(count(&c).0, 1);
    assert!(hashchain::verify(&c, e.id).unwrap().first_bad_seq.is_none());
}
