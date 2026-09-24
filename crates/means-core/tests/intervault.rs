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

#[test]
fn a_void_of_one_side_voids_the_other_and_the_link() {
    let b = books();
    let a = b.parent_loan_entry("2026-03-02", EntryStatus::Posted);
    let s = b.sub_loan_entry("2026-03-03", EntryStatus::Posted);
    let link = b.link(&a, &s, "loan").unwrap();
    journal::void_entry(&mut b.db.conn(), a.id, None, "wrong lender").unwrap();

    let s = b.get(&s);
    assert_eq!(s.status, EntryStatus::Void);
    let reversal = journal::get_entry(&b.db.conn(), s.reversed_by_id.unwrap()).unwrap();
    assert_eq!(reversal.date, day("2026-03-03"));
    assert!(reversal.description.contains("wrong lender"), "{}", reversal.description);
    assert_eq!(b.get(&a).status, EntryStatus::Void);
    assert_eq!(b.status_of(&link), "void");
}

#[test]
fn a_void_is_refused_when_the_other_period_is_locked() {
    let b = books();
    let a = b.parent_loan_entry("2026-03-02", EntryStatus::Posted);
    let s = b.sub_loan_entry("2026-03-03", EntryStatus::Posted);
    let link = b.link(&a, &s, "loan").unwrap();
    b.lock(b.subsidiary, "2026-03-31");

    let err = journal::void_entry(&mut b.db.conn(), a.id, None, "wrong lender").unwrap_err();
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
    journal::void_entry(&mut b.db.conn(), a.id, Some(day("2026-04-01")), "wrong lender").unwrap();
    assert_eq!(b.get(&s).status, EntryStatus::Void);
}

#[test]
fn unlink_needs_two_drafts_and_a_reason() {
    let b = books();
    let a = b.parent_loan_entry("2026-03-02", EntryStatus::Draft);
    let s = b.sub_loan_entry("2026-03-03", EntryStatus::Draft);
    let link = b.link(&a, &s, "loan").unwrap();
    assert!(matches!(intervault::unlink(&mut b.db.conn(), &link.uid, " "), Err(Error::Invalid(_))));
    intervault::unlink(&mut b.db.conn(), &link.uid, "linked by mistake").unwrap();
    assert!(intervault::links_for_entry(&b.db.conn(), a.id).unwrap().is_empty());
    let history = audit::history(&b.db.conn(), "intervault_links", link.id).unwrap();
    assert_eq!(history.last().unwrap().reason, "linked by mistake");

    let link = b.link(&a, &s, "loan").unwrap();
    journal::post_entry(&mut b.db.conn(), a.id).unwrap();
    assert!(matches!(intervault::unlink(&mut b.db.conn(), &link.uid, "late change"), Err(Error::Invalid(_))));
    assert_eq!(b.status_of(&link), "draft");
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
