//! The machine write interface: an agent drafts, a proposal previews, a human confirms.

use means_core::proposal::{self, DraftDocument};
use means_core::{accounts, entities, journal, rates, AccountType, Db, EntryStatus};
use serde_json::json;

struct Books {
    db: Db,
    entity: i64,
}

fn books() -> Books {
    let db = Db::open_memory().unwrap();
    let entity = {
        let mut conn = db.conn();
        let e = entities::create_entity(&mut conn, "Blue Harbor LLC", "company", "US", "USD").unwrap();
        accounts::ensure_account(&conn, e.id, AccountType::Asset, &["Bank", "Checking"], "bank", "USD").unwrap();
        accounts::ensure_account(&conn, e.id, AccountType::Asset, &["Bank", "Euro"], "bank", "EUR").unwrap();
        accounts::ensure_account(&conn, e.id, AccountType::Asset, &["Bank", "Pesos"], "bank", "MXN").unwrap();
        accounts::ensure_account(&conn, e.id, AccountType::Expense, &["Software"], "expense", "USD").unwrap();
        rates::set_price(&conn, "EUR", "USD", "2026-03-01".parse().unwrap(), "1.10".parse().unwrap(), "manual").unwrap();
        e.id
    };
    Books { db, entity }
}

fn doc(value: serde_json::Value) -> DraftDocument {
    serde_json::from_value(value).unwrap()
}

fn software_purchase() -> DraftDocument {
    doc(json!({
        "date": "2026-03-10",
        "payee": "Sample Vendor",
        "description": "Annual licence",
        "tags": ["project=alpha"],
        "postings": [
            {"account": "Bank:Euro", "quantity": "-100.00", "commodity": "EUR", "memo": "card"},
            {"account": "Expenses:Software"}
        ],
        "metadata": {"source": "agent-test"}
    }))
}

fn code_of(result: Result<impl std::fmt::Debug, proposal::AgentError>) -> &'static str {
    result.expect_err("expected a refusal").code
}

#[test]
fn draft_propose_confirm_posts_only_the_reviewed_entry() {
    let b = books();
    let mut c = b.db.conn();
    let drafted = proposal::create_draft(&mut c, b.entity, &software_purchase()).unwrap();
    assert_eq!(drafted.entry.status, EntryStatus::Draft);
    assert_eq!(drafted.entry.origin, "agent");
    assert!(drafted.entry.hash.is_none());
    assert!(drafted.entry.reviewed_at.is_none());
    assert_eq!(drafted.entry.tags, vec!["project:alpha".to_string()]);
    let event: String = c.query_row("SELECT action FROM audit_log WHERE id = ?1", [drafted.audit_event_id], |r| r.get(0)).unwrap();
    assert_eq!(event, "agent_draft");

    let id = proposal::entry_id_by_uid(&c, &drafted.entry.uid).unwrap();
    let first = proposal::propose(&c, id).unwrap();
    let again = proposal::propose(&c, id).unwrap();
    assert_eq!(first.proposal_digest, again.proposal_digest);
    assert_eq!(first.proposal_digest.len(), 64);
    assert_eq!(first.postings[0].account_path, "Assets:Bank:Euro");
    assert_eq!(first.postings[0].amount.minor(), -11000);
    assert_eq!(first.postings[0].rate.as_deref(), Some("1.1"));
    assert_eq!(first.postings[0].rate_source, "nearest");
    assert_eq!(first.totals.debit.minor(), 11000);
    assert_eq!(first.totals.credit.minor(), 11000);
    assert!(first.totals.balance.is_zero());
    assert!(first.warnings.iter().any(|w| w.contains("nearest")));
    assert_eq!(journal::get_entry(&c, id).unwrap().status, EntryStatus::Draft, "propose writes nothing");

    assert_eq!(code_of(proposal::confirm(&mut c, id, "0000", "Sample Person")), "stale_proposal");
    assert_eq!(journal::get_entry(&c, id).unwrap().status, EntryStatus::Draft);

    let confirmed = proposal::confirm(&mut c, id, &first.proposal_digest, "Sample Person").unwrap();
    assert_eq!(confirmed.entry.status, EntryStatus::Posted);
    assert!(confirmed.entry.hash.is_some());
    assert!(confirmed.entry.reviewed_at.is_some());
    let after: String = c.query_row("SELECT after FROM audit_log WHERE id = ?1", [confirmed.audit_event_id], |r| r.get(0)).unwrap();
    let after: serde_json::Value = serde_json::from_str(&after).unwrap();
    assert_eq!(after["actor"], "Sample Person");
    assert_eq!(after["action"], "confirm");
    assert_eq!(after["proposal_digest"].as_str(), Some(first.proposal_digest.as_str()));
    assert_eq!(code_of(proposal::propose(&c, id)), "invalid_input", "a posted entry has no proposal");
}

#[test]
fn an_edit_after_the_proposal_makes_the_digest_stale() {
    let b = books();
    let mut c = b.db.conn();
    let drafted = proposal::create_draft(&mut c, b.entity, &software_purchase()).unwrap();
    let id = drafted.entry.id;
    let preview = proposal::propose(&c, id).unwrap();
    c.execute("UPDATE journal_entries SET payee = 'Changed Vendor' WHERE id = ?1", [id]).unwrap();
    assert_eq!(code_of(proposal::confirm(&mut c, id, &preview.proposal_digest, "Sample Person")), "stale_proposal");
    assert_eq!(journal::get_entry(&c, id).unwrap().status, EntryStatus::Draft);
}

#[test]
fn draft_refusals_carry_stable_codes() {
    let b = books();
    let mut c = b.db.conn();
    let unknown = doc(json!({"date": "2026-03-10", "postings": [{"account": "Bank:Nowhere", "quantity": "-1"}, {"account": "Expenses:Software"}]}));
    assert_eq!(code_of(proposal::create_draft(&mut c, b.entity, &unknown)), "unknown_account");
    let group = doc(json!({"date": "2026-03-10", "postings": [{"account": "Assets:Bank", "quantity": "-1"}, {"account": "Expenses:Software"}]}));
    assert_eq!(code_of(proposal::create_draft(&mut c, b.entity, &group)), "not_leaf_account");
    let unbalanced = doc(json!({"date": "2026-03-10", "postings": [{"account": "Bank:Checking", "quantity": "-1"}, {"account": "Expenses:Software", "quantity": "2"}]}));
    assert_eq!(code_of(proposal::create_draft(&mut c, b.entity, &unbalanced)), "unbalanced");
    let two_balancing = doc(json!({"date": "2026-03-10", "postings": [{"account": "Bank:Checking"}, {"account": "Expenses:Software"}]}));
    assert_eq!(code_of(proposal::create_draft(&mut c, b.entity, &two_balancing)), "invalid_input");
    let bad_date = doc(json!({"date": "10/03/2026", "postings": [{"account": "Bank:Checking", "quantity": "-1"}, {"account": "Expenses:Software"}]}));
    assert_eq!(code_of(proposal::create_draft(&mut c, b.entity, &bad_date)), "invalid_input");
    let wrong_rate = doc(json!({"date": "2026-03-10", "postings": [{"account": "Bank:Euro", "quantity": "-10", "amount": "-12.00", "rate": "1.10"}, {"account": "Expenses:Software"}]}));
    assert_eq!(code_of(proposal::create_draft(&mut c, b.entity, &wrong_rate)), "invalid_input");
    assert!(serde_json::from_value::<DraftDocument>(json!({"date": "2026-03-10", "postings": [], "status": "posted"})).is_err(), "unknown fields are refused");
    let count: i64 = c.query_row("SELECT COUNT(*) FROM journal_entries", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 0);
}

#[test]
fn a_given_rate_values_the_posting() {
    let b = books();
    let mut c = b.db.conn();
    let d = doc(json!({"date": "2026-03-10", "postings": [{"account": "Bank:Euro", "quantity": "-10", "rate": "1.2"}, {"account": "Expenses:Software"}]}));
    let drafted = proposal::create_draft(&mut c, b.entity, &d).unwrap();
    assert_eq!(drafted.entry.postings[0].amount.minor(), -1200);
    assert_eq!(drafted.entry.postings[0].rate_source, "input");
}

#[test]
fn propose_refuses_missing_rates_and_locked_periods() {
    let b = books();
    let mut c = b.db.conn();
    let pesos = doc(json!({"date": "2026-03-10", "postings": [{"account": "Bank:Pesos", "quantity": "-100"}, {"account": "Expenses:Software"}]}));
    let drafted = proposal::create_draft(&mut c, b.entity, &pesos).unwrap();
    assert_eq!(code_of(proposal::propose(&c, drafted.entry.id)), "missing_rate");

    let drafted = proposal::create_draft(&mut c, b.entity, &software_purchase()).unwrap();
    entities::update_entity(&mut c, b.entity, "", "", "", Some("2026-03-31".parse().unwrap()), false).unwrap();
    assert_eq!(code_of(proposal::propose(&c, drafted.entry.id)), "locked_period");
    assert_eq!(code_of(proposal::confirm(&mut c, drafted.entry.id, "x", "Sample Person")), "locked_period");
    assert_eq!(code_of(proposal::entry_id_by_uid(&c, "no-such-uid")), "not_found");
}
