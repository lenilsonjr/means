//! The filings register: record, status changes, the freeze of a filed period and amendments.

use chrono::NaiveDate;
use means_core::filings::{self, Filing, FilingStatus, NewFiling, StatusChange};
use means_core::proposal::AgentError;
use means_core::{accounts, audit, close, entities, journal, AccountType, Db, EntryInput, PostingInput};

const SNAPSHOT: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";

struct Books {
    db: Db,
    entity: i64,
}

fn day(s: &str) -> NaiveDate {
    s.parse().unwrap()
}

fn books() -> Books {
    let db = Db::open_memory().unwrap();
    let entity = {
        let mut c = db.conn();
        let e = entities::create_entity(&mut c, "Cedar Lane Studio Ltd", "company", "GB", "GBP").unwrap();
        let bank = accounts::ensure_account(&c, e.id, AccountType::Asset, &["Bank", "Current"], "bank", "GBP").unwrap().id;
        let rent = accounts::ensure_account(&c, e.id, AccountType::Expense, &["Rent"], "expense", "GBP").unwrap().id;
        let mut input = EntryInput::new(e.id, day("2026-02-03"));
        input.postings = vec![PostingInput::new(bank, "-900.00".parse().unwrap()), PostingInput::balancing(rent)];
        journal::create_entry(&mut c, input).unwrap();
        e.id
    };
    Books { db, entity }
}

impl Books {
    fn record(&self, code: &str, from: &str, to: &str, status: FilingStatus) -> Filing {
        let input = NewFiling {
            entity_id: self.entity,
            jurisdiction: "gb".into(),
            filing_code: code.into(),
            period_start: day(from),
            period_end: day(to),
            status,
            snapshot_digest: SNAPSHOT.into(),
            artifact_digest: String::new(),
            notes: "Sample quarterly return".into(),
        };
        filings::record_filing(&mut self.db.conn(), input, "Morgan Hale", "Prepared from the Q1 snapshot").unwrap()
    }

    fn submit(&self, filing: &Filing) -> Filing {
        let change =
            StatusChange { status: FilingStatus::Submitted, receipt_ref: Some("RCPT-0001".into()), submitted_at: Some("2026-04-20".into()), actor: "Morgan Hale".into(), reason: String::new() };
        filings::update_filing_status(&mut self.db.conn(), filing.id, change).unwrap()
    }

    fn lock_q1(&self) {
        close::close_lock(&mut self.db.conn(), self.entity, day("2026-03-31"), "Morgan Hale", "Q1 close", false).unwrap();
    }
}

fn code_of<T: std::fmt::Debug>(result: Result<T, AgentError>) -> &'static str {
    result.expect_err("expected a refusal").code
}

#[test]
fn record_and_list_filings() {
    let b = books();
    let vat = b.record("VAT100", "2026-01-01", "2026-03-31", FilingStatus::Prepared);
    b.record("CT600", "2026-01-01", "2026-12-31", FilingStatus::Prepared);
    assert_eq!(vat.jurisdiction, "GB");
    assert_eq!(vat.status, FilingStatus::Prepared);
    assert_eq!(vat.snapshot_digest, SNAPSHOT);
    assert_eq!(vat.chain_head_seq, Some(1), "the row keeps the chain head");
    assert!(vat.chain_head_hash.is_some());
    assert!(vat.submitted_at.is_none());
    let c = b.db.conn();
    assert_eq!(filings::list_filings(&c, b.entity, None, None).unwrap().len(), 2);
    assert_eq!(filings::list_filings(&c, b.entity, Some("GB"), Some("VAT100")).unwrap(), vec![vat.clone()]);
    assert!(filings::list_filings(&c, b.entity, Some("US"), None).unwrap().is_empty());
    let history = audit::history(&c, "filings", vat.id).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!((history[0].action.as_str(), history[0].actor.as_str(), history[0].reason.as_str()), ("record", "Morgan Hale", "Prepared from the Q1 snapshot"));
}

#[test]
fn record_refuses_bad_input() {
    let b = books();
    let base = || NewFiling {
        entity_id: b.entity,
        jurisdiction: "GB".into(),
        filing_code: "VAT100".into(),
        period_start: day("2026-01-01"),
        period_end: day("2026-03-31"),
        status: FilingStatus::Prepared,
        snapshot_digest: String::new(),
        artifact_digest: String::new(),
        notes: String::new(),
    };
    let mut c = b.db.conn();
    assert_eq!(code_of(filings::record_filing(&mut c, NewFiling { period_end: day("2025-12-31"), ..base() }, "Morgan Hale", "")), "invalid_input");
    assert_eq!(code_of(filings::record_filing(&mut c, NewFiling { filing_code: " ".into(), ..base() }, "Morgan Hale", "")), "invalid_input");
    assert_eq!(code_of(filings::record_filing(&mut c, NewFiling { snapshot_digest: "not-a-digest".into(), ..base() }, "Morgan Hale", "")), "invalid_input");
    assert_eq!(code_of(filings::record_filing(&mut c, NewFiling { status: FilingStatus::Amended, ..base() }, "Morgan Hale", "")), "invalid_input");
    assert_eq!(code_of(filings::record_filing(&mut c, NewFiling { entity_id: 999, ..base() }, "Morgan Hale", "")), "not_found");
}

#[test]
fn status_changes_follow_the_lifecycle_and_are_audited() {
    let b = books();
    let vat = b.record("VAT100", "2026-01-01", "2026-03-31", FilingStatus::Prepared);
    let no_receipt = StatusChange { status: FilingStatus::Submitted, receipt_ref: None, submitted_at: None, actor: "Morgan Hale".into(), reason: String::new() };
    assert_eq!(code_of(filings::update_filing_status(&mut b.db.conn(), vat.id, no_receipt)), "invalid_input");
    let early = StatusChange { status: FilingStatus::Accepted, receipt_ref: None, submitted_at: None, actor: "Morgan Hale".into(), reason: String::new() };
    assert_eq!(code_of(filings::update_filing_status(&mut b.db.conn(), vat.id, early)), "invalid_transition");

    let submitted = b.submit(&vat);
    assert_eq!(submitted.status, FilingStatus::Submitted);
    assert_eq!(submitted.receipt_ref, "RCPT-0001");
    assert_eq!(submitted.submitted_at.as_deref(), Some("2026-04-20"));
    let accepted = StatusChange { status: FilingStatus::Accepted, receipt_ref: None, submitted_at: None, actor: "Robin Vale".into(), reason: "Authority confirmed".into() };
    let accepted = filings::update_filing_status(&mut b.db.conn(), vat.id, accepted).unwrap();
    assert_eq!(accepted.receipt_ref, "RCPT-0001", "the receipt stays");
    let history = audit::history(&b.db.conn(), "filings", vat.id).unwrap();
    assert_eq!(history.iter().map(|e| e.action.as_str()).collect::<Vec<_>>(), vec!["record", "status", "status"]);
    assert_eq!((history[2].actor.as_str(), history[2].reason.as_str()), ("Robin Vale", "Authority confirmed"));
    assert!(history[2].before.as_deref().unwrap().contains("\"submitted\""));
}

#[test]
fn a_submitted_filing_blocks_unlock_without_amend() {
    let b = books();
    b.lock_q1();
    let vat = b.record("VAT100", "2026-01-01", "2026-03-31", FilingStatus::Prepared);
    // A prepared filing does not freeze its period.
    close::unlock(&mut b.db.conn(), b.entity, day("2026-01-31"), "Morgan Hale", "Fix a January entry", false).unwrap();
    b.lock_q1();
    b.submit(&vat);
    let refused = close::unlock(&mut b.db.conn(), b.entity, day("2026-01-31"), "Morgan Hale", "Fix a January entry", false);
    assert_eq!(code_of(refused), "filed_period");
    assert_eq!(entities::get_entity(&b.db.conn(), b.entity).unwrap().lock_date, Some(day("2026-03-31")));
    assert_eq!(filings::list_filings(&b.db.conn(), b.entity, None, None).unwrap().len(), 1, "a refusal writes nothing");
}

#[test]
fn a_filing_outside_the_reopened_days_does_not_block() {
    let b = books();
    close::close_lock(&mut b.db.conn(), b.entity, day("2026-06-30"), "Morgan Hale", "H1 close", false).unwrap();
    let q1 = b.record("VAT100", "2026-01-01", "2026-03-31", FilingStatus::Prepared);
    b.submit(&q1);
    close::unlock(&mut b.db.conn(), b.entity, day("2026-03-31"), "Morgan Hale", "Reopen Q2", false).unwrap();
}

#[test]
fn amend_creates_a_linked_row_and_unlocks() {
    let b = books();
    b.lock_q1();
    let vat = b.record("VAT100", "2026-01-01", "2026-03-31", FilingStatus::Prepared);
    b.submit(&vat);
    let out = close::unlock(&mut b.db.conn(), b.entity, day("2026-01-31"), "Morgan Hale", "Correct a February invoice", true).unwrap();
    assert_eq!(out.lock_date, day("2026-01-31"));
    assert_eq!(out.amendments.len(), 1);
    let amendment = &out.amendments[0];
    assert_eq!(amendment.amends_id, Some(vat.id));
    assert_eq!(amendment.amends_uid.as_deref(), Some(vat.uid.as_str()));
    assert_eq!(amendment.status, FilingStatus::Prepared);
    assert_eq!((amendment.jurisdiction.as_str(), amendment.filing_code.as_str()), ("GB", "VAT100"));
    assert_eq!((amendment.period_start, amendment.period_end), (vat.period_start, vat.period_end));
    let c = b.db.conn();
    assert_eq!(filings::get_filing(&c, vat.id).unwrap().status, FilingStatus::Submitted, "the original filing keeps its status");
    let history = audit::history(&c, "filings", amendment.id).unwrap();
    assert_eq!((history[0].action.as_str(), history[0].actor.as_str(), history[0].reason.as_str()), ("amend", "Morgan Hale", "Correct a February invoice"));
    let event = audit::get(&c, out.audit_event_id).unwrap();
    assert!(event.after.as_deref().unwrap().contains(&amendment.uid));
    drop(c);

    // A second reopening reuses the open amendment.
    b.lock_q1();
    let again = close::unlock(&mut b.db.conn(), b.entity, day("2026-01-31"), "Morgan Hale", "Second correction", true).unwrap();
    assert_eq!(again.amendments[0].id, amendment.id);
    assert_eq!(filings::list_filings(&b.db.conn(), b.entity, None, Some("VAT100")).unwrap().len(), 2);
}
