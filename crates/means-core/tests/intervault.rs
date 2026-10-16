//! Inter-vault links: the pair lifecycle, transfers, lists, reconciliation and exceptions.

use chrono::NaiveDate;
use means_core::intervault::{self, Link};
use means_core::proposal::{self, DraftDocument};
use means_core::{accounts, audit, entities, groups, journal, AccountType, Db, EntryInput, EntryStatus, Error, JournalEntry, PostingInput};
use rust_decimal::Decimal;
use serde_json::json;

struct Books {
    db: Db,
    parent: i64,
    subsidiary: i64,
    parent_bank: i64,
    parent_loan: i64,
    parent_office: i64,
    sub_bank: i64,
    sub_loan: i64,
}

fn day(s: &str) -> NaiveDate {
    s.parse().unwrap()
}

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

/// A parent in SGD that lends to its subsidiary in USD.
fn books() -> Books {
    let db = Db::open_memory().unwrap();
    let b = {
        let mut c = db.conn();
        let parent = entities::create_entity(&mut c, "Northwind Holdings Pte. Ltd.", "company", "SG", "SGD").unwrap();
        let subsidiary = entities::create_entity(&mut c, "Blue Harbor LLC", "company", "US", "USD").unwrap();
        let parent_bank = accounts::ensure_account(&c, parent.id, AccountType::Asset, &["Bank", "Operating"], "bank", "SGD").unwrap().id;
        let parent_loan = accounts::ensure_account(&c, parent.id, AccountType::Asset, &["Loan to Blue Harbor"], "receivable", "SGD").unwrap().id;
        let parent_office = accounts::ensure_account(&c, parent.id, AccountType::Expense, &["Office"], "expense", "SGD").unwrap().id;
        let sub_bank = accounts::ensure_account(&c, subsidiary.id, AccountType::Asset, &["Bank", "Checking"], "bank", "USD").unwrap().id;
        let sub_loan = accounts::ensure_account(&c, subsidiary.id, AccountType::Liability, &["Loan from Northwind"], "loan", "USD").unwrap().id;
        for id in [parent_loan, sub_loan] {
            c.execute("UPDATE accounts SET intercompany = 1 WHERE id = ?1", [id]).unwrap();
        }
        (parent.id, subsidiary.id, parent_bank, parent_loan, parent_office, sub_bank, sub_loan)
    };
    Books { db, parent: b.0, subsidiary: b.1, parent_bank: b.2, parent_loan: b.3, parent_office: b.4, sub_bank: b.5, sub_loan: b.6 }
}

impl Books {
    fn entry(&self, entity: i64, date: &str, debit: i64, amount: &str, credit: i64, status: EntryStatus) -> JournalEntry {
        let mut c = self.db.conn();
        let mut input = EntryInput::new(entity, day(date));
        input.status = status;
        input.payee = "Intercompany loan".into();
        input.postings = vec![PostingInput::new(debit, d(amount)), PostingInput::balancing(credit)];
        journal::create_entry(&mut c, input).unwrap()
    }

    /// The loan in the books of the parent: 1350.00 SGD out of the bank.
    fn parent_loan_entry(&self, date: &str, status: EntryStatus) -> JournalEntry {
        self.entry(self.parent, date, self.parent_loan, "1350.00", self.parent_bank, status)
    }

    /// The loan in the books of the subsidiary: 1000.00 USD into the bank.
    fn sub_loan_entry(&self, date: &str, status: EntryStatus) -> JournalEntry {
        self.entry(self.subsidiary, date, self.sub_bank, "1000.00", self.sub_loan, status)
    }

    fn link(&self, a: &JournalEntry, b: &JournalEntry, kind: &str) -> means_core::Result<Link> {
        intervault::link(&mut self.db.conn(), &a.uid, &b.uid, kind, "")
    }

    fn get(&self, entry: &JournalEntry) -> JournalEntry {
        journal::get_entry(&self.db.conn(), entry.id).unwrap()
    }

    fn status_of(&self, link: &Link) -> String {
        intervault::get_link_by_uid(&self.db.conn(), &link.uid).unwrap().status
    }

    fn lock(&self, entity: i64, through: &str) {
        entities::update_entity(&mut self.db.conn(), entity, "", "", "", Some(day(through)), false).unwrap();
    }
}

#[test]
fn a_draft_pair_becomes_posted_when_both_entries_post() {
    let b = books();
    let a = b.parent_loan_entry("2026-03-02", EntryStatus::Draft);
    let s = b.sub_loan_entry("2026-03-03", EntryStatus::Draft);
    let link = b.link(&a, &s, "loan").unwrap();
    assert_eq!((link.status.as_str(), link.kind.as_str()), ("draft", "loan"));
    assert_eq!((link.amount_a.minor(), link.amount_a.commodity()), (135000, "SGD"));
    assert_eq!((link.amount_b.minor(), link.amount_b.commodity()), (-100000, "USD"));
    assert_eq!(link.rate, "0.740740740741");
    assert!(link.warnings.is_empty(), "{:?}", link.warnings);

    journal::post_entry(&mut b.db.conn(), a.id).unwrap();
    assert_eq!(b.status_of(&link), "draft");
    journal::post_entry(&mut b.db.conn(), s.id).unwrap();
    assert_eq!(b.status_of(&link), "posted");

    let history = audit::history(&b.db.conn(), "intervault_links", link.id).unwrap();
    let actions: Vec<_> = history.iter().map(|h| h.action.as_str()).collect();
    assert_eq!(actions, ["link", "post"]);
}

#[test]
fn a_link_of_posted_entries_is_posted_and_warns_without_intercompany_postings() {
    let b = books();
    let a = b.entry(b.parent, "2026-03-02", b.parent_office, "25.00", b.parent_bank, EntryStatus::Posted);
    let s = b.sub_loan_entry("2026-03-02", EntryStatus::Posted);
    let link = b.link(&a, &s, "service").unwrap();
    assert_eq!(link.status, "posted");
    assert_eq!(link.warnings.len(), 1);
    assert!(link.warnings[0].contains("no intercompany posting"), "{:?}", link.warnings);
    // The largest posting of the office entry is its first posting.
    assert_eq!(link.amount_a.minor(), 2500);
}

/// A void with cascade: the entry and every linked entry of other entities.
fn void_cascade(b: &Books, entry: &JournalEntry, date: Option<NaiveDate>, reason: &str) -> means_core::Result<journal::VoidReview> {
    journal::review_void(&mut b.db.conn(), entry.id, date, reason, true, true)
}

#[test]
fn a_void_preview_lists_the_linked_entry_of_the_other_entity() {
    let b = books();
    let a = b.parent_loan_entry("2026-03-02", EntryStatus::Posted);
    let s = b.sub_loan_entry("2026-03-03", EntryStatus::Posted);
    let link = b.link(&a, &s, "loan").unwrap();

    let preview = journal::review_void(&mut b.db.conn(), a.id, None, "wrong lender", false, false).unwrap();
    assert!(!preview.applied);
    assert!(preview.needs_cascade);
    assert_eq!(preview.linked_entries.len(), 1);
    let other = &preview.linked_entries[0];
    assert_eq!((other.entity_name.as_str(), other.entry_id, other.entry_uid.as_str()), ("Blue Harbor LLC", s.id, s.uid.as_str()));
    assert_eq!(other.date, day("2026-03-03"));
    assert_eq!((other.amount.minor(), other.amount.commodity()), (-100000, "USD"));
    assert_eq!((other.link_uid.as_str(), other.cascade_action.as_str(), other.locked, other.voided), (link.uid.as_str(), "void", false, false));
    // The preview reverses entry A only and saves nothing.
    assert_eq!(preview.reversal.entity_id, b.parent);
    assert_eq!(b.get(&a).status, EntryStatus::Posted);
    assert_eq!(b.get(&s).status, EntryStatus::Posted);
    assert_eq!(b.status_of(&link), "posted");
}

#[test]
fn a_void_without_cascade_is_refused_when_a_linked_entry_belongs_to_another_entity() {
    let b = books();
    let a = b.parent_loan_entry("2026-03-02", EntryStatus::Posted);
    let s = b.sub_loan_entry("2026-03-03", EntryStatus::Posted);
    let link = b.link(&a, &s, "loan").unwrap();
    let audit_before: i64 = b.db.conn().query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0)).unwrap();

    let err = journal::void_entry(&mut b.db.conn(), a.id, None, "wrong lender").unwrap_err();
    match &err {
        Error::Conflict(m) => {
            assert!(m.contains("Blue Harbor LLC") && m.contains(&s.uid) && m.contains("2026-03-03") && m.contains("-1000 USD"), "{m}");
            assert!(m.contains("cascade"), "{m}");
        }
        other => panic!("expected a conflict, got {other:?}"),
    }
    assert_eq!(proposal::AgentError::from(err).code, "invalid_input");
    // The refusal writes nothing.
    assert_eq!(b.get(&a).status, EntryStatus::Posted);
    assert_eq!(b.get(&s).status, EntryStatus::Posted);
    assert_eq!(b.status_of(&link), "posted");
    let audit_after: i64 = b.db.conn().query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0)).unwrap();
    assert_eq!(audit_before, audit_after);
}

#[test]
fn a_void_with_cascade_voids_the_other_side_and_records_it() {
    let b = books();
    let a = b.parent_loan_entry("2026-03-02", EntryStatus::Posted);
    let s = b.sub_loan_entry("2026-03-03", EntryStatus::Posted);
    let link = b.link(&a, &s, "loan").unwrap();
    let review = void_cascade(&b, &a, None, "wrong lender").unwrap();
    assert!(review.applied && review.cascade && !review.needs_cascade);
    assert_eq!(review.linked_entries.len(), 1);
    assert!(review.linked_entries[0].voided);

    let s = b.get(&s);
    assert_eq!(s.status, EntryStatus::Void);
    let reversal = journal::get_entry(&b.db.conn(), s.reversed_by_id.unwrap()).unwrap();
    assert_eq!(reversal.date, day("2026-03-03"));
    assert!(reversal.description.contains("wrong lender"), "{}", reversal.description);
    assert_eq!(b.get(&a).status, EntryStatus::Void);
    assert_eq!(b.status_of(&link), "void");

    // The audit events say that the void was a cascade, and from which entry.
    let event = |id: i64| -> serde_json::Value {
        let history = audit::history(&b.db.conn(), "journal_entries", id).unwrap();
        serde_json::from_str(history.iter().rev().find(|h| h.action == "void").unwrap().after.as_deref().unwrap()).unwrap()
    };
    let first = event(a.id);
    assert_eq!(first["cascade"], true);
    assert_eq!(first["linked_entry_uids"], json!([s.uid]));
    assert_eq!(event(s.id)["cascaded_from"], a.uid.as_str());
    let link_event = audit::history(&b.db.conn(), "intervault_links", link.id).unwrap().pop().unwrap();
    let after: serde_json::Value = serde_json::from_str(link_event.after.as_deref().unwrap()).unwrap();
    assert_eq!((after["status"].as_str(), after["cascade"].as_bool(), after["voided_from"].as_str()), (Some("void"), Some(true), Some(a.uid.as_str())));
}

#[test]
fn a_cascade_void_is_refused_when_the_other_period_is_locked() {
    let b = books();
    let a = b.parent_loan_entry("2026-03-02", EntryStatus::Posted);
    let s = b.sub_loan_entry("2026-03-03", EntryStatus::Posted);
    let link = b.link(&a, &s, "loan").unwrap();
    b.lock(b.subsidiary, "2026-03-31");

    let preview = journal::review_void(&mut b.db.conn(), a.id, None, "wrong lender", false, false).unwrap();
    assert!(preview.linked_entries[0].locked);
    let err = void_cascade(&b, &a, None, "wrong lender").unwrap_err();
    match &err {
        Error::Locked(m) => assert!(m.contains("Blue Harbor LLC"), "{m}"),
        other => panic!("expected a locked error, got {other:?}"),
    }
    assert_eq!(proposal::AgentError::from(err).code, "locked_period");
    // The refusal writes nothing.
    assert_eq!(b.get(&a).status, EntryStatus::Posted);
    assert_eq!(b.get(&s).status, EntryStatus::Posted);
    assert_eq!(b.status_of(&link), "posted");

    // A void date after the lock date of the other entity is accepted.
    void_cascade(&b, &a, Some(day("2026-04-01")), "wrong lender").unwrap();
    assert_eq!(b.get(&s).status, EntryStatus::Void);
}

#[test]
fn unlink_needs_two_drafts_and_a_reason() {
    let b = books();
    let a = b.parent_loan_entry("2026-03-02", EntryStatus::Draft);
    let s = b.sub_loan_entry("2026-03-03", EntryStatus::Draft);
    let link = b.link(&a, &s, "loan").unwrap();
    assert!(matches!(intervault::unlink(&mut b.db.conn(), &link.uid, "", " ", false), Err(Error::Invalid(_))));
    intervault::unlink(&mut b.db.conn(), &link.uid, "", "linked by mistake", false).unwrap();
    assert!(intervault::links_for_entry(&b.db.conn(), a.id).unwrap().is_empty());
    let history = audit::history(&b.db.conn(), "intervault_links", link.id).unwrap();
    assert_eq!(history.last().unwrap().reason, "linked by mistake");

    // A posted entry makes the removal a human action: it needs the actor.
    let link = b.link(&a, &s, "loan").unwrap();
    journal::post_entry(&mut b.db.conn(), a.id).unwrap();
    assert!(matches!(intervault::unlink(&mut b.db.conn(), &link.uid, "", "late change", false), Err(Error::Invalid(m)) if m.contains("--actor")));
    assert_eq!(b.status_of(&link), "draft");
}

#[test]
fn unlink_of_posted_entries_is_audited_and_reopens_the_close_warning() {
    let b = books();
    let a = b.parent_loan_entry("2026-03-02", EntryStatus::Posted);
    let s = b.sub_loan_entry("2026-03-02", EntryStatus::Posted);
    let link = b.link(&a, &s, "loan").unwrap();
    assert!(finding(&parent_close(&b), "intercompany_out_of_balance").is_none());

    let out = intervault::unlink(&mut b.db.conn(), &link.uid, "Sample Reviewer", "booked against the wrong lender", false).unwrap();
    assert_eq!((out.actor.as_str(), out.force_locked), ("Sample Reviewer", false));
    assert!(intervault::get_link_by_uid(&b.db.conn(), &link.uid).is_err());

    // The audit event has the actor, the reason and both entry uids.
    let event = audit::get(&b.db.conn(), out.audit_event_id).unwrap();
    assert_eq!((event.table_name.as_str(), event.row_id, event.action.as_str()), ("intervault_links", link.id, "unlink"));
    assert_eq!((event.actor.as_str(), event.reason.as_str()), ("Sample Reviewer", "booked against the wrong lender"));
    let after: serde_json::Value = serde_json::from_str(event.after.as_deref().unwrap()).unwrap();
    assert_eq!(after["entry_a_uid"], a.uid.as_str());
    assert_eq!(after["entry_b_uid"], s.uid.as_str());
    assert_eq!(after["intercompany_reconciled"], false);
    assert_eq!(after["force_locked"], false);

    // Both entries keep their postings and are not reconciled for intercompany purposes now.
    assert_eq!(b.get(&a).status, EntryStatus::Posted);
    assert_eq!(b.get(&s).status, EntryStatus::Posted);
    let report = parent_close(&b);
    let warning = finding(&report, "intercompany_out_of_balance").unwrap();
    assert_eq!(warning.amount.unwrap().minor(), 135000);
    assert!(finding(&report, "intercompany_linked").is_none());
    let sub_report = means_core::close::close_check(&b.db.conn(), b.subsidiary, day("2026-03-01"), day("2026-03-31")).unwrap();
    assert_eq!(finding(&sub_report, "intercompany_out_of_balance").unwrap().amount.unwrap().minor(), -100000);
}

#[test]
fn a_leg_only_void_after_unlink_ignores_the_lock_of_the_other_entity() {
    let b = books();
    let a = b.parent_loan_entry("2026-03-02", EntryStatus::Posted);
    let s = b.sub_loan_entry("2026-03-03", EntryStatus::Posted);
    let link = b.link(&a, &s, "loan").unwrap();
    b.lock(b.subsidiary, "2026-03-31");

    // The entry of the subsidiary is in a locked period: the removal needs force_locked.
    let err = intervault::unlink(&mut b.db.conn(), &link.uid, "Sample Reviewer", "wrong lender", false).unwrap_err();
    assert!(matches!(&err, Error::Locked(m) if m.contains("Blue Harbor LLC") && m.contains("--force-locked")), "{err:?}");
    assert_eq!(b.status_of(&link), "posted");
    let out = intervault::unlink(&mut b.db.conn(), &link.uid, "Sample Reviewer", "wrong lender", true).unwrap();
    assert!(out.force_locked);
    assert_eq!(out.locked_entry_uids, vec![s.uid.clone()]);
    let after: serde_json::Value = serde_json::from_str(audit::get(&b.db.conn(), out.audit_event_id).unwrap().after.as_deref().unwrap()).unwrap();
    assert_eq!(after["force_locked"], true);
    assert_eq!(after["locked_entry_uids"], json!([s.uid]));

    // Without the link, the void of A touches A only, and the lock of the subsidiary does not block.
    let review = journal::review_void(&mut b.db.conn(), a.id, None, "wrong lender", true, false).unwrap();
    assert!(review.linked_entries.is_empty() && !review.needs_cascade);
    assert_eq!(b.get(&a).status, EntryStatus::Void);
    assert_eq!(b.get(&s).status, EntryStatus::Posted);
}

#[test]
fn a_deleted_draft_deletes_its_link() {
    let b = books();
    let a = b.parent_loan_entry("2026-03-02", EntryStatus::Draft);
    let s = b.sub_loan_entry("2026-03-03", EntryStatus::Posted);
    let link = b.link(&a, &s, "loan").unwrap();
    journal::delete_entry(&mut b.db.conn(), a.id).unwrap();
    assert!(intervault::get_link_by_uid(&b.db.conn(), &link.uid).is_err());
    assert!(intervault::links_for_entry(&b.db.conn(), s.id).unwrap().is_empty());
}

#[test]
fn a_link_refuses_one_entity_a_void_entry_and_a_second_link() {
    let b = books();
    let a = b.parent_loan_entry("2026-03-02", EntryStatus::Posted);
    let other = b.entry(b.parent, "2026-03-04", b.parent_office, "10.00", b.parent_bank, EntryStatus::Posted);
    let err = b.link(&a, &other, "loan").unwrap_err();
    assert!(matches!(&err, Error::Invalid(m) if m.contains("Northwind Holdings Pte. Ltd.")), "{err:?}");

    let s = b.sub_loan_entry("2026-03-03", EntryStatus::Posted);
    assert!(matches!(b.link(&a, &s, "gift"), Err(Error::Invalid(_))));
    b.link(&a, &s, "loan").unwrap();
    assert!(matches!(b.link(&s, &a, "loan"), Err(Error::Conflict(_))));

    let voided = b.sub_loan_entry("2026-03-05", EntryStatus::Posted);
    journal::void_entry(&mut b.db.conn(), voided.id, None, "duplicate").unwrap();
    assert!(matches!(b.link(&other, &voided, "loan"), Err(Error::Invalid(_))));
}

fn transfer(b: &Books, kind: Option<&str>, status: EntryStatus) -> journal::Transfer {
    journal::Transfer {
        date: day("2026-03-10"),
        from_account_id: b.parent_bank,
        to_account_id: b.sub_bank,
        from_quantity: d("675.00"),
        to_quantity: Some(d("500.00")),
        fee: None,
        fee_account_id: None,
        payee: String::new(),
        notes: String::new(),
        from_contra_account_id: Some(b.parent_loan),
        to_contra_account_id: Some(b.sub_loan),
        status,
        origin: String::new(),
        external: None,
        kind: kind.map(str::to_string),
    }
}

#[test]
fn create_transfer_writes_the_link() {
    let b = books();
    let (a, s) = journal::create_transfer(&mut b.db.conn(), transfer(&b, Some("loan"), EntryStatus::Posted)).unwrap();
    let s = s.unwrap();
    assert_eq!(a.counterpart_id, Some(s.id));
    let links = intervault::links_for_entry(&b.db.conn(), a.id).unwrap();
    assert_eq!(links.len(), 1);
    let link = &links[0];
    assert_eq!((link.kind.as_str(), link.status.as_str()), ("loan", "posted"));
    assert_eq!((link.entry_a_uid.as_str(), link.entry_b_uid.as_str()), (a.uid.as_str(), s.uid.as_str()));
    assert_eq!((link.amount_a.minor(), link.amount_b.minor()), (67500, -50000));

    let (a, _) = journal::create_transfer(&mut b.db.conn(), transfer(&b, None, EntryStatus::Draft)).unwrap();
    let link = &intervault::links_for_entry(&b.db.conn(), a.id).unwrap()[0];
    assert_eq!((link.kind.as_str(), link.status.as_str()), ("transfer", "draft"));

    let before: i64 = b.db.conn().query_row("SELECT COUNT(*) FROM journal_entries", [], |r| r.get(0)).unwrap();
    assert!(journal::create_transfer(&mut b.db.conn(), transfer(&b, Some("gift"), EntryStatus::Posted)).is_err());
    let after: i64 = b.db.conn().query_row("SELECT COUNT(*) FROM journal_entries", [], |r| r.get(0)).unwrap();
    assert_eq!(before, after);
}

#[test]
fn the_list_flags_links_whose_amounts_do_not_agree_at_the_stored_rate() {
    let b = books();
    let a = b.parent_loan_entry("2026-03-02", EntryStatus::Posted);
    let s = b.sub_loan_entry("2026-03-03", EntryStatus::Posted);
    let kept = b.link(&a, &s, "loan").unwrap();

    let a2 = b.entry(b.parent, "2026-03-20", b.parent_loan, "270.00", b.parent_bank, EntryStatus::Draft);
    let s2 = b.entry(b.subsidiary, "2026-03-21", b.sub_bank, "200.00", b.sub_loan, EntryStatus::Draft);
    let changed = b.link(&a2, &s2, "loan").unwrap();
    assert_eq!(changed.rate, kept.rate);
    // The subsidiary books the second loan at another rate after the link.
    let mut input = EntryInput::new(b.subsidiary, day("2026-03-21"));
    input.payee = "Intercompany loan".into();
    input.postings = vec![PostingInput::new(b.sub_bank, d("210.00")), PostingInput::balancing(b.sub_loan)];
    journal::update_entry(&mut b.db.conn(), s2.id, input).unwrap();

    let sub_uid = entities::get_entity(&b.db.conn(), b.subsidiary).unwrap().uid;
    let links = intervault::list_links(&b.db.conn(), None, Some(&sub_uid), day("2026-03-01"), day("2026-03-31"), None).unwrap();
    assert_eq!(links.len(), 2);
    let first = links.iter().find(|l| l.uid == kept.uid).unwrap();
    assert!(first.reconciled);
    assert_eq!(first.entry_b.entity_name, "Blue Harbor LLC");
    let second = links.iter().find(|l| l.uid == changed.uid).unwrap();
    assert!(!second.reconciled);
    assert_eq!(second.entry_b.amount.minor(), -20000);
    assert_eq!(second.entry_b.current_amount.minor(), -21000);

    let posted = intervault::list_links(&b.db.conn(), None, None, day("2026-03-01"), day("2026-03-31"), Some("posted")).unwrap();
    assert_eq!(posted.iter().map(|l| l.uid.as_str()).collect::<Vec<_>>(), [kept.uid.as_str()]);
    let april = intervault::list_links(&b.db.conn(), None, None, day("2026-04-01"), day("2026-04-30"), None).unwrap();
    assert!(april.is_empty());

    // A group list keeps the links of its members on the entry dates.
    let group = groups::create_group(&mut b.db.conn(), "Northwind Group").unwrap();
    assert!(intervault::list_links(&b.db.conn(), Some("Northwind Group"), None, day("2026-03-01"), day("2026-03-31"), None).unwrap().is_empty());
    groups::add_member(&mut b.db.conn(), groups::NewMember::new(group.id, b.parent, day("2026-01-01"))).unwrap();
    let grouped = intervault::list_links(&b.db.conn(), Some(&group.uid), None, day("2026-03-01"), day("2026-03-31"), None).unwrap();
    assert_eq!(grouped.len(), 2);
}

#[test]
fn a_same_commodity_link_needs_equal_amounts() {
    let b = books();
    let usd_loan = {
        let c = b.db.conn();
        let id = accounts::ensure_account(&c, b.parent, AccountType::Asset, &["Loan to Blue Harbor USD"], "receivable", "USD").unwrap().id;
        c.execute("UPDATE accounts SET intercompany = 1 WHERE id = ?1", [id]).unwrap();
        id
    };
    let mut input = EntryInput::new(b.parent, day("2026-03-02"));
    input.postings = vec![PostingInput { amount: Some(d("1350.00")), ..PostingInput::new(usd_loan, d("1000.00")) }, PostingInput::balancing(b.parent_bank)];
    let a = journal::create_entry(&mut b.db.conn(), input).unwrap();
    let s = b.entry(b.subsidiary, "2026-03-03", b.sub_bank, "990.00", b.sub_loan, EntryStatus::Posted);
    let link = b.link(&a, &s, "loan").unwrap();
    assert_eq!(link.rate, "1");
    assert!(!intervault::view(&b.db.conn(), &link).unwrap().reconciled);
}

#[test]
fn the_exceptions_list_finds_unlinked_intercompany_postings() {
    let b = books();
    let linked = b.parent_loan_entry("2026-03-02", EntryStatus::Posted);
    let s = b.sub_loan_entry("2026-03-03", EntryStatus::Posted);
    b.link(&linked, &s, "loan").unwrap();
    let lonely = b.entry(b.parent, "2026-03-15", b.parent_loan, "400.00", b.parent_bank, EntryStatus::Posted);
    b.entry(b.parent, "2026-03-16", b.parent_loan, "80.00", b.parent_bank, EntryStatus::Draft);
    b.entry(b.parent, "2026-03-17", b.parent_office, "12.00", b.parent_bank, EntryStatus::Posted);
    let voided = b.entry(b.parent, "2026-03-18", b.parent_loan, "55.00", b.parent_bank, EntryStatus::Posted);
    journal::void_entry(&mut b.db.conn(), voided.id, None, "duplicate").unwrap();

    let found = intervault::unlinked_intercompany(&b.db.conn(), b.parent, day("2026-03-01"), day("2026-03-31")).unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].entry_uid, lonely.uid);
    assert_eq!(found[0].account_path, accounts::get_account(&b.db.conn(), b.parent_loan).unwrap().path);
    assert_eq!(found[0].quantity.minor(), 40000);
    assert!(intervault::unlinked_intercompany(&b.db.conn(), b.subsidiary, day("2026-03-01"), day("2026-03-31")).unwrap().is_empty());
}

#[test]
fn an_agent_draft_can_link_to_an_entry_in_another_vault() {
    let b = books();
    let s = b.sub_loan_entry("2026-03-03", EntryStatus::Posted);
    let (loan_uid, bank_uid) = {
        let c = b.db.conn();
        (accounts::get_account(&c, b.parent_loan).unwrap().uid, accounts::get_account(&c, b.parent_bank).unwrap().uid)
    };
    let doc: DraftDocument = serde_json::from_value(json!({
        "date": "2026-03-02",
        "payee": "Blue Harbor LLC",
        "postings": [
            {"account": loan_uid, "quantity": "1350.00"},
            {"account": bank_uid}
        ],
        "intervault": {"with": s.uid, "kind": "loan"}
    }))
    .unwrap();
    let drafted = proposal::create_draft(&mut b.db.conn(), b.parent, &doc).unwrap();
    let link = drafted.link.unwrap();
    assert_eq!((link.kind.as_str(), link.status.as_str()), ("loan", "draft"));
    assert_eq!(link.entry_a_uid, drafted.entry.uid);

    let digest = proposal::propose(&b.db.conn(), drafted.entry.id).unwrap().proposal_digest;
    proposal::confirm(&mut b.db.conn(), drafted.entry.id, &digest, "Sample Reviewer").unwrap();
    assert_eq!(b.status_of(&link), "posted");

    // A link to an entry of the same entity fails, and the draft is not written.
    let own = b.entry(b.parent, "2026-03-04", b.parent_office, "10.00", b.parent_bank, EntryStatus::Posted);
    let mut again = doc.clone();
    again.intervault.as_mut().unwrap().with = own.uid.clone();
    let before: i64 = b.db.conn().query_row("SELECT COUNT(*) FROM journal_entries", [], |r| r.get(0)).unwrap();
    assert_eq!(proposal::create_draft(&mut b.db.conn(), b.parent, &again).unwrap_err().code, "invalid_input");
    let after: i64 = b.db.conn().query_row("SELECT COUNT(*) FROM journal_entries", [], |r| r.get(0)).unwrap();
    assert_eq!(before, after);
}

/// The close check of the parent for March 2026.
fn parent_close(b: &Books) -> means_core::close::CloseReport {
    means_core::close::close_check(&b.db.conn(), b.parent, day("2026-03-01"), day("2026-03-31")).unwrap()
}

fn finding<'a>(report: &'a means_core::close::CloseReport, code: &str) -> Option<&'a means_core::close::Finding> {
    report.findings.iter().find(|f| f.code == code)
}

#[test]
fn a_fully_linked_intercompany_flow_raises_no_close_warning() {
    let b = books();
    let a = b.parent_loan_entry("2026-03-02", EntryStatus::Posted);
    let s = b.sub_loan_entry("2026-03-02", EntryStatus::Posted);
    let link = b.link(&a, &s, "loan").unwrap();
    assert_eq!(link.status, "posted");
    let report = parent_close(&b);
    assert!(finding(&report, "intercompany_out_of_balance").is_none(), "{:?}", report.findings);
    let info = finding(&report, "intercompany_linked").unwrap();
    assert_eq!(info.severity, means_core::close::Severity::Info);
    assert_eq!(info.amount.unwrap().minor(), 135000);
    assert_eq!(info.link_uids, vec![link.uid.clone()]);
    assert_eq!(report.warning_count, 0);
}

#[test]
fn an_unlinked_intercompany_posting_is_the_warning_amount() {
    let b = books();
    let a = b.parent_loan_entry("2026-03-02", EntryStatus::Posted);
    let s = b.sub_loan_entry("2026-03-02", EntryStatus::Posted);
    b.link(&a, &s, "loan").unwrap();
    b.entry(b.parent, "2026-03-20", b.parent_loan, "200.00", b.parent_bank, EntryStatus::Posted);
    let report = parent_close(&b);
    let warning = finding(&report, "intercompany_out_of_balance").unwrap();
    assert_eq!(warning.amount.unwrap().minor(), 20000);
    assert!(warning.link_uids.is_empty());
    assert_eq!(finding(&report, "intercompany_linked").unwrap().amount.unwrap().minor(), 135000);
}

#[test]
fn a_link_with_mismatched_amounts_is_named_in_the_warning() {
    let b = books();
    let a = b.parent_loan_entry("2026-03-02", EntryStatus::Posted);
    let s = b.sub_loan_entry("2026-03-02", EntryStatus::Posted);
    let link = b.link(&a, &s, "loan").unwrap();
    // The stored rate no longer matches the two amounts.
    b.db.conn().execute("UPDATE intervault_links SET rate = '0.5' WHERE uid = ?1", [&link.uid]).unwrap();
    let report = parent_close(&b);
    let warning = finding(&report, "intercompany_out_of_balance").unwrap();
    assert_eq!(warning.amount.unwrap().minor(), 135000);
    assert_eq!(warning.link_uids, vec![link.uid.clone()]);
    assert!(warning.message.contains(&link.uid), "{}", warning.message);
    assert!(finding(&report, "intercompany_linked").is_none());
}

/// A third entity, in EUR, with a bank account and an intercompany account. Returns the entity,
/// the bank and the intercompany account.
fn third_entity(b: &Books) -> (i64, i64, i64) {
    let mut c = b.db.conn();
    let third = entities::create_entity(&mut c, "Example Services GmbH", "company", "DE", "EUR").unwrap();
    let bank = accounts::ensure_account(&c, third.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap().id;
    let due = accounts::ensure_account(&c, third.id, AccountType::Liability, &["Due to group"], "payable", "EUR").unwrap().id;
    c.execute("UPDATE accounts SET intercompany = 1 WHERE id = ?1", [due]).unwrap();
    (third.id, bank, due)
}

/// The number of audit events of one action on one row.
fn events(b: &Books, table: &str, id: i64, action: &str) -> Vec<audit::AuditRow> {
    audit::history(&b.db.conn(), table, id).unwrap().into_iter().filter(|h| h.action == action).collect()
}

/// The number of reversals of an entry.
fn reversals(b: &Books, entry: &JournalEntry) -> i64 {
    b.db.conn().query_row("SELECT COUNT(*) FROM journal_entries WHERE reverses_id = ?1", [entry.id], |r| r.get(0)).unwrap()
}

#[test]
fn a_cascade_reaches_three_entities_and_records_the_actor_on_each_event() {
    let b = books();
    let (third, third_bank, third_due) = third_entity(&b);
    let a = b.parent_loan_entry("2026-03-02", EntryStatus::Posted);
    let s = b.sub_loan_entry("2026-03-03", EntryStatus::Posted);
    let t = b.entry(third, "2026-03-04", third_bank, "900.00", third_due, EntryStatus::Posted);
    let first = b.link(&a, &s, "loan").unwrap();
    let second = b.link(&s, &t, "loan").unwrap();

    let review = journal::review_void_by(&mut b.db.conn(), a.id, None, "wrong lender", true, true, "Sample Reviewer").unwrap();
    let mut reached: Vec<i64> = review.linked_entries.iter().map(|l| l.entry_id).collect();
    reached.sort();
    assert_eq!(reached, vec![s.id, t.id]);
    assert!(review.linked_entries.iter().all(|l| l.voided));
    for entry in [&a, &s, &t] {
        assert_eq!(b.get(entry).status, EntryStatus::Void);
        assert_eq!(reversals(&b, entry), 1);
        let voids = events(&b, "journal_entries", entry.id, "void");
        assert_eq!(voids.len(), 1);
        assert_eq!(voids[0].actor, "Sample Reviewer");
    }
    for link in [&first, &second] {
        assert_eq!(b.status_of(link), "void");
        let voids = events(&b, "intervault_links", link.id, "void");
        assert_eq!(voids.len(), 1, "one audit event per link");
        assert_eq!((voids[0].actor.as_str(), voids[0].reason.as_str()), ("Sample Reviewer", "wrong lender"));
    }
}

#[test]
fn a_cascade_through_a_cycle_voids_each_entry_once_and_logs_each_link_once() {
    let b = books();
    let (third, third_bank, third_due) = third_entity(&b);
    let a = b.parent_loan_entry("2026-03-02", EntryStatus::Posted);
    let s = b.sub_loan_entry("2026-03-03", EntryStatus::Posted);
    let t = b.entry(third, "2026-03-04", third_bank, "900.00", third_due, EntryStatus::Posted);
    let links = [b.link(&a, &s, "loan").unwrap(), b.link(&s, &t, "loan").unwrap(), b.link(&t, &a, "loan").unwrap()];

    // The preview lists each other entry once.
    let preview = journal::review_void(&mut b.db.conn(), a.id, None, "wrong lender", false, false).unwrap();
    assert_eq!(preview.linked_entries.len(), 2);

    void_cascade(&b, &a, None, "wrong lender").unwrap();
    for entry in [&a, &s, &t] {
        assert_eq!(b.get(entry).status, EntryStatus::Void);
        assert_eq!(reversals(&b, entry), 1, "one reversal per entry");
        assert_eq!(events(&b, "journal_entries", entry.id, "void").len(), 1);
    }
    for link in &links {
        assert_eq!(b.status_of(link), "void");
        assert_eq!(events(&b, "intervault_links", link.id, "void").len(), 1, "one audit event per link");
    }
}

#[test]
fn a_cascade_preview_never_applies_and_shows_the_locked_entry() {
    let b = books();
    let (third, third_bank, third_due) = third_entity(&b);
    let a = b.parent_loan_entry("2026-03-02", EntryStatus::Posted);
    let s = b.sub_loan_entry("2026-03-03", EntryStatus::Posted);
    let t = b.entry(third, "2026-03-04", third_bank, "900.00", third_due, EntryStatus::Posted);
    let first = b.link(&a, &s, "loan").unwrap();
    b.link(&s, &t, "loan").unwrap();
    b.lock(third, "2026-03-31");
    let audit_before: i64 = b.db.conn().query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0)).unwrap();

    // The preview with cascade does not fail on the lock: it lists the entries and their lock.
    let preview = journal::review_void(&mut b.db.conn(), a.id, None, "wrong lender", false, true).unwrap();
    assert!(!preview.applied && preview.cascade && !preview.needs_cascade);
    let locked: Vec<(i64, bool)> = preview.linked_entries.iter().map(|l| (l.entry_id, l.locked)).collect();
    assert!(locked.contains(&(s.id, false)) && locked.contains(&(t.id, true)), "{locked:?}");
    assert!(preview.linked_entries.iter().all(|l| !l.voided));
    assert_eq!(preview.entry.status, EntryStatus::Posted);
    // The preview saves nothing.
    for entry in [&a, &s, &t] {
        assert_eq!(b.get(entry).status, EntryStatus::Posted);
    }
    assert_eq!(b.status_of(&first), "posted");
    let audit_after: i64 = b.db.conn().query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0)).unwrap();
    assert_eq!(audit_before, audit_after);

    // The apply is refused because of the lock, and names the entity.
    let err = void_cascade(&b, &a, None, "wrong lender").unwrap_err();
    assert!(matches!(&err, Error::Locked(m) if m.contains("Example Services GmbH")), "{err:?}");
}
