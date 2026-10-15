//! The counterparty of an account, and the close check of a related party outside the group.

use chrono::NaiveDate;
use means_core::accounts::{self, AccountUpdate};
use means_core::close::{self, Severity};
use means_core::parties::{self, PartyFactsPatch};
use means_core::snapshot::{self, SnapshotOptions};
use means_core::{entities, journal, payees, AccountType, Db, EntryInput, Error, PostingInput};
use rusqlite::Connection;
use rust_decimal::Decimal;

fn day(s: &str) -> NaiveDate {
    s.parse().unwrap()
}

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

fn payee(c: &mut Connection, entity: i64, name: &str, kind: &str) -> i64 {
    let change = payees::Change { id: 0, entity_id: entity, name: name.into(), active: true, aliases: vec![] };
    let preview = payees::save(c, change.clone(), None).unwrap();
    let id = payees::save(c, change, Some(&preview.token)).unwrap().payee.unwrap().id;
    parties::set_facts(c, id, PartyFactsPatch { kind: Some(kind.into()), ..Default::default() }).unwrap();
    id
}

struct Books {
    db: Db,
    entity: i64,
    due: i64,
}

/// A company with a shareholder loan account that moved in January.
fn books() -> Books {
    let db = Db::open_memory().unwrap();
    let (entity, due) = {
        let mut c = db.conn();
        let e = entities::create_entity(&mut c, "Blue Harbor Pte. Ltd.", "company", "SG", "SGD").unwrap();
        let bank = accounts::ensure_account(&c, e.id, AccountType::Asset, &["Bank", "Operating"], "bank", "SGD").unwrap().id;
        let due = accounts::ensure_account(&c, e.id, AccountType::Liability, &["Amount due to related parties"], "payable", "SGD").unwrap().id;
        accounts::update_account(&c, due, AccountUpdate { intercompany: Some(true), ..Default::default() }).unwrap();
        let mut input = EntryInput::new(e.id, day("2026-01-15"));
        input.postings = vec![PostingInput::new(bank, d("250")), PostingInput::balancing(due)];
        let entry = journal::create_entry(&mut c, input).unwrap();
        journal::mark_reviewed(&c, &[entry.id]).unwrap();
        (e.id, due)
    };
    Books { db, entity, due }
}

fn codes(b: &Books) -> Vec<(String, Severity)> {
    let report = close::close_check(&b.db.conn(), b.entity, day("2026-01-01"), day("2026-01-31")).unwrap();
    report.findings.iter().filter(|f| f.account_uid.is_some()).map(|f| (f.code.clone(), f.severity)).collect()
}

#[test]
fn an_account_without_a_counterparty_keeps_the_intercompany_warning() {
    let b = books();
    assert_eq!(codes(&b), [("intercompany_out_of_balance".to_string(), Severity::Warning)]);
}

#[test]
fn a_person_counterparty_is_a_related_party_balance() {
    let b = books();
    let shareholder = payee(&mut b.db.conn(), b.entity, "Sample Shareholder", "person");
    accounts::update_account(&b.db.conn(), b.due, AccountUpdate { counterparty_payee_id: Some(Some(shareholder)), ..Default::default() }).unwrap();
    assert_eq!(codes(&b), [("related_party_balance".to_string(), Severity::Info)]);
    let report = close::close_check(&b.db.conn(), b.entity, day("2026-01-01"), day("2026-01-31")).unwrap();
    let f = report.findings.iter().find(|f| f.code == "related_party_balance").unwrap();
    assert_eq!(f.amount.as_ref().unwrap().minor(), -25000);
    assert!(f.message.contains("Sample Shareholder (person)"), "{}", f.message);
    assert_eq!(report.warning_count, 0);

    // An empty counterparty brings the warning back.
    accounts::update_account(&b.db.conn(), b.due, AccountUpdate { counterparty_payee_id: Some(None), ..Default::default() }).unwrap();
    assert_eq!(codes(&b)[0].0, "intercompany_out_of_balance");
}

#[test]
fn a_company_counterparty_is_outside_the_group_unless_it_names_another_entity() {
    let b = books();
    let sister = payee(&mut b.db.conn(), b.entity, "Contoso Studio Lda", "company");
    accounts::update_account(&b.db.conn(), b.due, AccountUpdate { counterparty_payee_id: Some(Some(sister)), ..Default::default() }).unwrap();
    assert_eq!(codes(&b)[0].0, "related_party_balance");
    // Once the company has books in the ledger, the movement needs an inter-vault link again.
    entities::create_entity(&mut b.db.conn(), "Contoso Studio Lda", "company", "PT", "EUR").unwrap();
    assert_eq!(codes(&b)[0].0, "intercompany_out_of_balance");
}

#[test]
fn the_counterparty_must_be_a_payee_of_the_same_entity_and_goes_to_the_snapshot() {
    let b = books();
    let other = entities::create_entity(&mut b.db.conn(), "Fabrikam GmbH", "company", "DE", "EUR").unwrap();
    let foreign = payee(&mut b.db.conn(), other.id, "Sample Person", "person");
    let refused = accounts::update_account(&b.db.conn(), b.due, AccountUpdate { counterparty_payee_id: Some(Some(foreign)), ..Default::default() });
    assert!(matches!(refused, Err(Error::Invalid(_))), "{refused:?}");
    let own = payee(&mut b.db.conn(), b.entity, "Sample Shareholder", "person");
    let after = accounts::update_account(&b.db.conn(), b.due, AccountUpdate { counterparty_payee_id: Some(Some(own)), ..Default::default() }).unwrap();
    assert_eq!(after.counterparty_payee_id, Some(own));

    let out = std::env::temp_dir().join(format!("means-counterparty-snapshot-{}", means_core::new_uid()));
    snapshot::export(&mut b.db.conn(), &out, &SnapshotOptions::default()).unwrap();
    let accounts_json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(out.join("accounts.json")).unwrap()).unwrap();
    let parties_json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(out.join("parties.json")).unwrap()).unwrap();
    assert!(snapshot::verify(&out).unwrap().ok);
    let _ = std::fs::remove_dir_all(&out);
    let due = accounts_json.as_array().unwrap().iter().find(|a| a["path"] == "Liabilities:Amount due to related parties").unwrap();
    let shareholder_uid = parties_json.as_array().unwrap().iter().find(|p| p["name"] == "Sample Shareholder").unwrap()["uid"].clone();
    assert_eq!(due["counterparty_uid"], shareholder_uid);
    let bank = accounts_json.as_array().unwrap().iter().find(|a| a["path"] == "Assets:Bank:Operating").unwrap();
    assert!(bank["counterparty_uid"].is_null());
}
