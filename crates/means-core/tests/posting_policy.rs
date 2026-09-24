//! The draft_only posting policy: no automatic route posts, and a human still can.

use chrono::NaiveDate;
use means_core::policy::{self, PostingPolicy};
use means_core::{accounts, entities, imports, journal, rules, templates, AccountType, Db, EntryInput, EntryStatus, EntryTemplate, PostingInput, Rule, RuleCondition, TemplateLine};
use rust_decimal::Decimal;

const CSV: &str = "Booking Date,Value Date,Partner Name,Partner Iban,Type,Payment Reference,Account Name,Amount (EUR)\n2026-02-01,2026-02-01,SAMPLE BAKERY,,Card,,Main,-10.00\n";

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

fn date(s: &str) -> NaiveDate {
    s.parse().unwrap()
}

struct Books {
    db: Db,
    entity: i64,
    bank: i64,
    food: i64,
}

fn books(policy_value: PostingPolicy) -> Books {
    let db = Db::open_memory().unwrap();
    let (entity, bank, food) = {
        let mut c = db.conn();
        let entity = entities::create_entity(&mut c, "Rio Verde SL", "company", "ES", "EUR").unwrap().id;
        let bank = accounts::ensure_account(&c, entity, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap().id;
        let food = accounts::ensure_account(&c, entity, AccountType::Expense, &["Food"], "expense", "EUR").unwrap().id;
        policy::set(&mut c, policy_value).unwrap();
        (entity, bank, food)
    };
    Books { db, entity, bank, food }
}

fn posted_count(db: &Db) -> i64 {
    db.conn().query_row("SELECT COUNT(*) FROM journal_entries WHERE status = 'posted'", [], |r| r.get(0)).unwrap()
}

fn bakery_rule(entity: i64, account: i64) -> Rule {
    Rule {
        id: 0,
        entity_id: entity,
        name: "Bakery".into(),
        position: 10,
        enabled: true,
        conditions: vec![RuleCondition { field: "description".into(), op: "contains".into(), value: "BAKERY".into() }],
        account_id: Some(account),
        template_id: None,
        payee: String::new(),
        hits_count: 0,
        created_at: String::new(),
        tags: String::new(),
    }
}

#[test]
fn policy_defaults_to_normal_and_round_trips() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    assert_eq!(policy::current(&c).unwrap(), PostingPolicy::Normal);
    assert_eq!(policy::set(&mut c, PostingPolicy::DraftOnly).unwrap(), PostingPolicy::Normal);
    assert_eq!(policy::current(&c).unwrap(), PostingPolicy::DraftOnly);
    assert_eq!(PostingPolicy::parse("draft-only").unwrap(), PostingPolicy::DraftOnly);
    assert!(PostingPolicy::parse("sometimes").is_err());
    means_core::db::set_setting(&c, policy::SETTING_KEY, "garbled").unwrap();
    assert!(policy::current(&c).is_err(), "an unknown stored value must not permit posting");
}

#[test]
fn rule_driven_import_creates_drafts_under_draft_only() {
    for (value, expected) in [(PostingPolicy::Normal, EntryStatus::Posted), (PostingPolicy::DraftOnly, EntryStatus::Draft)] {
        let b = books(value);
        let mut c = b.db.conn();
        rules::save_rule(&c, &bakery_rule(b.entity, b.food)).unwrap();
        let out = imports::run_import(&mut c, imports::ImportRequest::new("n26_csv", Some(b.bank), "bank.csv", CSV.as_bytes())).unwrap();
        let entry = journal::get_entry(&c, out.lines[0].journal_entry_id.unwrap()).unwrap();
        assert_eq!(entry.status, expected, "{value:?}");
        assert_eq!(entry.origin, "rule");
        assert_eq!(entry.postings[1].account_id, b.food);
        drop(c);
        if value == PostingPolicy::DraftOnly {
            assert_eq!(posted_count(&b.db), 0);
        }
    }
}

#[test]
fn rule_rerun_on_suspense_drafts_keeps_drafts_under_draft_only() {
    let b = books(PostingPolicy::DraftOnly);
    let mut c = b.db.conn();
    imports::run_import(&mut c, imports::ImportRequest::new("n26_csv", Some(b.bank), "bank.csv", CSV.as_bytes())).unwrap();
    rules::save_rule(&c, &bakery_rule(b.entity, b.food)).unwrap();
    let (_, redrafted) = rules::run_rules(&mut c, Some(b.entity), None, true).unwrap();
    assert_eq!(redrafted, 1);
    drop(c);
    assert_eq!(posted_count(&b.db), 0);
}

#[test]
fn scheduled_template_creates_drafts_under_draft_only() {
    let b = books(PostingPolicy::DraftOnly);
    let c = b.db.conn();
    let line = |account_id, method: &str, value: Option<&str>| TemplateLine { account_id, method: method.into(), value: value.map(d), of_line: None, memo: String::new(), label: String::new() };
    let template = EntryTemplate {
        id: 0,
        uid: String::new(),
        entity_id: b.entity,
        name: "Office rent".into(),
        payee: "Blue Harbor LLC".into(),
        description: "Monthly rent".into(),
        lines: vec![line(b.bank, "fixed", Some("-50")), line(b.food, "balance", None)],
        rrule: "FREQ=MONTHLY;BYMONTHDAY=1".into(),
        starts_on: Some(date("2026-01-01")),
        next_on: None,
        ends_on: None,
        auto_post: true,
        lead_days: 0,
        version: 1,
        active: true,
        created_at: String::new(),
    };
    templates::save_template(&c, &template).unwrap();
    drop(c);
    let created = templates::run_schedules(&mut b.db.conn(), Some(date("2026-03-15"))).unwrap();
    assert_eq!(created, 3);
    assert_eq!(posted_count(&b.db), 0);
}

#[test]
fn matcher_links_evidence_but_keeps_the_draft_under_draft_only() {
    for (value, expected) in [(PostingPolicy::Normal, EntryStatus::Posted), (PostingPolicy::DraftOnly, EntryStatus::Draft)] {
        let b = books(value);
        let mut c = b.db.conn();
        let mut input = EntryInput::new(b.entity, date("2026-02-01"));
        input.payee = "Sample Bakery".into();
        input.status = EntryStatus::Draft;
        input.postings = vec![PostingInput::new(b.bank, d("-10")), PostingInput::balancing(b.food)];
        let draft = journal::create_entry(&mut c, input).unwrap();
        let out = imports::run_import(&mut c, imports::ImportRequest::new("n26_csv", Some(b.bank), "bank.csv", CSV.as_bytes())).unwrap();
        assert_eq!(out.lines[0].status, "matched");
        assert_eq!(out.lines[0].journal_entry_id, Some(draft.id));
        assert_eq!(journal::get_entry(&c, draft.id).unwrap().status, expected, "{value:?}");
    }
}

#[test]
fn transfers_create_draft_pairs_under_draft_only() {
    let b = books(PostingPolicy::DraftOnly);
    let mut c = b.db.conn();
    let other = entities::create_entity(&mut c, "Northwind Holdings Pte. Ltd.", "company", "SG", "EUR").unwrap();
    let other_bank = accounts::ensure_account(&c, other.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
    let savings = accounts::ensure_account(&c, b.entity, AccountType::Asset, &["Savings"], "bank", "EUR").unwrap();
    let transfer = |to: i64| journal::Transfer {
        date: date("2026-02-03"),
        from_account_id: b.bank,
        to_account_id: to,
        from_quantity: d("100"),
        to_quantity: None,
        fee: None,
        fee_account_id: None,
        payee: String::new(),
        notes: String::new(),
        from_contra_account_id: None,
        to_contra_account_id: None,
        status: EntryStatus::Posted,
        origin: String::new(),
        external: None,
        kind: None,
    };
    let (a, pair) = journal::create_transfer(&mut c, transfer(other_bank.id)).unwrap();
    let pair = pair.unwrap();
    assert_eq!((a.status, pair.status), (EntryStatus::Draft, EntryStatus::Draft));
    assert_eq!(a.counterpart_id, Some(pair.id));
    let (inside, none) = journal::create_transfer(&mut c, transfer(savings.id)).unwrap();
    assert!(none.is_none());
    assert_eq!(inside.status, EntryStatus::Draft);
    drop(c);
    assert_eq!(posted_count(&b.db), 0);
}

#[test]
fn create_and_update_with_posted_status_become_drafts_under_draft_only() {
    let b = books(PostingPolicy::DraftOnly);
    let mut c = b.db.conn();
    let mut input = EntryInput::new(b.entity, date("2026-02-04"));
    input.postings = vec![PostingInput::new(b.bank, d("-12.50")), PostingInput::balancing(b.food)];
    assert_eq!(input.status, EntryStatus::Posted);
    let entry = journal::create_entry(&mut c, input.clone()).unwrap();
    assert_eq!(entry.status, EntryStatus::Draft);
    assert!(entry.hash.is_none());
    let history = means_core::audit::history(&c, "journal_entries", entry.id).unwrap();
    assert!(history[0].after.as_deref().unwrap().contains("\"requested_status\":\"posted\""));
    let updated = journal::update_entry(&mut c, entry.id, input).unwrap();
    assert_eq!(updated.status, EntryStatus::Draft);
    drop(c);
    assert_eq!(posted_count(&b.db), 0);
}

#[test]
fn a_human_still_posts_and_voids_under_draft_only() {
    let b = books(PostingPolicy::DraftOnly);
    let mut c = b.db.conn();
    let mut input = EntryInput::new(b.entity, date("2026-02-05"));
    input.postings = vec![PostingInput::new(b.bank, d("-8")), PostingInput::balancing(b.food)];
    let entry = journal::create_entry(&mut c, input).unwrap();
    let posted = journal::post_entry(&mut c, entry.id).unwrap();
    assert_eq!(posted.status, EntryStatus::Posted);
    let voided = journal::void_entry(&mut c, entry.id, None, "duplicate").unwrap();
    assert_eq!(voided.status, EntryStatus::Void);
    let reversal = journal::get_entry(&c, voided.reversed_by_id.unwrap()).unwrap();
    assert_eq!(reversal.status, EntryStatus::Posted);
    assert!(means_core::hashchain::verify(&c, b.entity).unwrap().first_bad_seq.is_none());
}
