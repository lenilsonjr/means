//! Tax codes with effective dates, tax events on drafts, voids and the tax summary.
use chrono::NaiveDate;
use means_core::model::*;
use means_core::proposal::{self, DraftDocument};
use means_core::tax::{self, NewTaxCode, TaxEventInput};
use means_core::{accounts, audit, db, entities, journal, payees, rates, Db, Error};
use rusqlite::Connection;
use rust_decimal::Decimal;

fn d(s: &str) -> NaiveDate {
    means_core::parse_date(s).unwrap()
}
fn n(s: &str) -> Decimal {
    s.parse().unwrap()
}

struct Books {
    entity: Entity,
    bank: Account,
    sales: Account,
    export: Account,
    gst_out: Account,
    gst_in: Account,
    services: Account,
    payables: Account,
    wht: Account,
}

fn books(c: &mut Connection) -> Books {
    let entity = entities::create_entity(c, "Northwind Holdings Pte. Ltd.", "company", "SG", "SGD").unwrap();
    let e = entity.id;
    let books = Books {
        bank: accounts::ensure_account(c, e, AccountType::Asset, &["Bank", "Operating"], "bank", "SGD").unwrap(),
        sales: accounts::ensure_account(c, e, AccountType::Income, &["Sales"], "income", "SGD").unwrap(),
        export: accounts::ensure_account(c, e, AccountType::Income, &["Export sales"], "income", "USD").unwrap(),
        gst_out: accounts::ensure_account(c, e, AccountType::Liability, &["GST output"], "tax", "SGD").unwrap(),
        gst_in: accounts::ensure_account(c, e, AccountType::Asset, &["GST input"], "tax", "SGD").unwrap(),
        services: accounts::ensure_account(c, e, AccountType::Expense, &["Services"], "expense", "SGD").unwrap(),
        payables: accounts::ensure_account(c, e, AccountType::Liability, &["Payables"], "payable", "SGD").unwrap(),
        wht: accounts::ensure_account(c, e, AccountType::Liability, &["Withholding tax"], "tax", "SGD").unwrap(),
        entity,
    };
    let code = |code: &str, kind: &str, rate_bp: i64, account: Option<i64>, recoverable_bp: Option<i64>| NewTaxCode {
        entity_id: e,
        code: code.into(),
        name: format!("{code} sample"),
        kind: kind.into(),
        rate_bp,
        tax_account_id: account,
        recoverable_bp,
        jurisdiction: "SG".into(),
        effective_from: d("2026-01-01"),
    };
    tax::add_tax_code(c, code("SR", "output", 900, Some(books.gst_out.id), None)).unwrap();
    tax::add_tax_code(c, code("TX", "input", 900, Some(books.gst_in.id), Some(5000))).unwrap();
    tax::add_tax_code(c, code("WHT", "withholding", 1000, Some(books.wht.id), None)).unwrap();
    tax::add_tax_code(c, code("ZR", "zero_rated", 0, None, None)).unwrap();
    books
}

fn taxed(account: i64, quantity: &str, code: &str) -> PostingInput {
    PostingInput { tax: Some(TaxInput { code: code.into(), ..Default::default() }), ..PostingInput::new(account, n(quantity)) }
}

fn entry(b: &Books, date: &str, postings: Vec<PostingInput>) -> EntryInput {
    let mut input = EntryInput::new(b.entity.id, d(date));
    input.payee = "Sample Person".into();
    input.postings = postings;
    input
}

fn sale(c: &mut Connection, b: &Books, date: &str, net: &str) -> JournalEntry {
    journal::create_entry(c, entry(b, date, vec![PostingInput::balancing(b.bank.id), taxed(b.sales.id, &format!("-{net}"), "SR")])).unwrap()
}

fn amounts(e: &JournalEntry) -> Vec<(i64, i64)> {
    e.postings.iter().map(|p| (p.account_id, p.amount.minor())).collect()
}

#[test]
fn tax_codes_follow_effective_dates() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let b = books(&mut c);
    let e = b.entity.id;
    let v2 = NewTaxCode {
        entity_id: e,
        code: "sr".into(),
        name: "Standard rated".into(),
        kind: "output".into(),
        rate_bp: 1000,
        tax_account_id: Some(b.gst_out.id),
        effective_from: d("2027-01-01"),
        ..Default::default()
    };
    let saved = tax::add_tax_code(&mut c, v2.clone()).unwrap();
    assert_eq!((saved.code.as_str(), saved.rate_bp, saved.recoverable_bp), ("SR", 1000, 10000));

    // The new version closes the open one on its first day.
    assert_eq!(tax::get_tax_code(&c, e, "SR", d("2026-12-31")).unwrap().rate_bp, 900);
    assert_eq!(tax::get_tax_code(&c, e, "SR", d("2027-01-01")).unwrap().rate_bp, 1000);
    let all: Vec<_> = tax::list_tax_codes(&c, e, None).unwrap().into_iter().filter(|t| t.code == "SR").collect();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].effective_to, Some(d("2027-01-01")));
    assert_eq!(tax::list_tax_codes(&c, e, Some(d("2027-06-01"))).unwrap().iter().filter(|t| t.code == "SR").count(), 1);
    assert!(matches!(tax::get_tax_code(&c, e, "SR", d("2025-12-31")), Err(Error::Invalid(_))));
    assert!(matches!(tax::get_tax_code(&c, e, "NOPE", d("2026-06-01")), Err(Error::NotFound(_))));

    // Overlaps and bad values fail.
    assert!(matches!(tax::add_tax_code(&mut c, NewTaxCode { effective_from: d("2026-06-01"), ..v2.clone() }), Err(Error::Conflict(_))));
    assert!(matches!(tax::add_tax_code(&mut c, NewTaxCode { code: "EX".into(), kind: "exempt".into(), rate_bp: 500, ..v2.clone() }), Err(Error::Invalid(_))));
    assert!(matches!(tax::add_tax_code(&mut c, NewTaxCode { code: "BAD".into(), kind: "sales".into(), ..v2.clone() }), Err(Error::Invalid(_))));
    assert!(matches!(tax::add_tax_code(&mut c, NewTaxCode { code: "FX".into(), tax_account_id: Some(b.export.id), ..v2.clone() }), Err(Error::Invalid(_))));
    assert!(matches!(tax::add_tax_code(&mut c, NewTaxCode { code: "RB".into(), recoverable_bp: Some(10001), ..v2.clone() }), Err(Error::Invalid(_))));

    // Close ends the open version.
    let closed = tax::close_tax_code(&mut c, e, "SR", d("2028-01-01")).unwrap();
    assert_eq!(closed.effective_to, Some(d("2028-01-01")));
    assert!(matches!(tax::get_tax_code(&c, e, "SR", d("2028-01-01")), Err(Error::Invalid(_))));
    assert!(matches!(tax::close_tax_code(&mut c, e, "SR", d("2029-01-01")), Err(Error::NotFound(_))));

    let history: Vec<_> = audit::history(&c, "tax_codes", all[0].id).unwrap().into_iter().map(|r| r.action).collect();
    assert_eq!(history, vec!["create", "close"]);
    assert_eq!(audit::history(&c, "tax_codes", closed.id).unwrap().len(), 2);
}

#[test]
fn rates_round_half_even() {
    assert_eq!(tax::apply_bp(10050, 900).unwrap(), 904); // 9.045 -> 9.04
    assert_eq!(tax::apply_bp(10150, 900).unwrap(), 914); // 9.135 -> 9.14
    assert_eq!(tax::apply_bp(-10050, 900).unwrap(), -904);
    assert_eq!(tax::apply_bp(1800, 5000).unwrap(), 900);
}

#[test]
fn sales_draft_with_output_code_adds_the_tax_posting_and_one_event() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let b = books(&mut c);
    let change = payees::Change { id: 0, entity_id: b.entity.id, name: "Rio Verde SL".into(), active: true, aliases: vec![] };
    let preview = payees::save(&mut c, change.clone(), None).unwrap();
    let customer = payees::save(&mut c, change, Some(&preview.token)).unwrap().payee.unwrap();

    let doc: DraftDocument = serde_json::from_value(serde_json::json!({
        "date": "2026-03-10",
        "payee": "Rio Verde SL",
        "postings": [
            {"account": "Assets:Bank:Operating"},
            {"account": "Income:Sales", "quantity": "-100.50", "tax": {"code": "sr", "document_ref": "INV-0001", "party": "Rio Verde SL"}}
        ]
    }))
    .unwrap();
    let drafted = proposal::create_draft(&mut c, b.entity.id, &doc).unwrap().entry;
    assert_eq!(drafted.status, EntryStatus::Draft);
    // The caller gives the net amount. The engine adds the tax line, and the bank absorbs it.
    assert_eq!(amounts(&drafted), vec![(b.bank.id, 10954), (b.sales.id, -10050), (b.gst_out.id, -904)]);
    assert_eq!(drafted.postings[2].metadata["tax_line"], "SR");

    let events = tax::tax_events_for_entry(&c, drafted.id).unwrap();
    assert_eq!(events.len(), 1);
    let ev = &events[0];
    assert_eq!((ev.code.as_str(), ev.kind.as_str(), ev.rate_bp), ("SR", "output", 900));
    assert_eq!((ev.base.minor(), ev.tax.minor(), ev.recoverable.minor(), ev.withheld.minor()), (-10050, -904, 0, 0));
    assert_eq!(ev.base.commodity(), "SGD");
    assert_eq!(ev.posting_id, Some(drafted.postings[1].id));
    assert_eq!(ev.tax_point, d("2026-03-10"));
    assert_eq!(ev.party_id, Some(customer.id));
    assert_eq!(ev.document_ref, "INV-0001");
    assert_eq!(ev.rule_version, "SR@2026-01-01");
    assert!(ev.native_base.is_none());

    // The preview shows the tax line and the event, and the digest covers them.
    let p = proposal::propose(&c, drafted.id).unwrap();
    assert_eq!(p.postings.len(), 3);
    assert_eq!(p.tax_events.len(), 1);
    assert_eq!(p.tax_events[0].posting_index, Some(1));
    assert_eq!(p.tax_events[0].tax.minor(), -904);
    assert_eq!(p.tax_events[0].party_uid.as_deref(), Some(customer.uid.as_str()));
    c.execute("UPDATE tax_events SET document_ref = 'INV-0002' WHERE entry_id = ?1", [drafted.id]).unwrap();
    let changed = proposal::propose(&c, drafted.id).unwrap();
    assert_ne!(changed.proposal_digest, p.proposal_digest);
    let posted = proposal::confirm(&mut c, drafted.id, &changed.proposal_digest, "Sample Person").unwrap().entry;
    assert_eq!(posted.status, EntryStatus::Posted);

    // Drafts do not count in the summary; posted entries do.
    let rows = tax::tax_summary(&c, b.entity.id, d("2026-03-01"), d("2026-03-31")).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].base.minor(), rows[0].tax.minor(), rows[0].count), (-10050, -904, 1));
}

#[test]
fn purchase_with_input_code_recovers_half_the_tax() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let b = books(&mut c);
    let e = journal::create_entry(&mut c, entry(&b, "2026-04-02", vec![taxed(b.services.id, "200", "TX"), PostingInput::balancing(b.bank.id)])).unwrap();
    // 18.00 tax: 9.00 recoverable on GST input, 9.00 a cost on the expense account.
    assert_eq!(amounts(&e), vec![(b.services.id, 20000), (b.bank.id, -21800), (b.gst_in.id, 900), (b.services.id, 900)]);
    assert_eq!(e.postings[3].metadata["tax_role"], "non_recoverable");
    let ev = &tax::tax_events_for_entry(&c, e.id).unwrap()[0];
    assert_eq!((ev.base.minor(), ev.tax.minor(), ev.recoverable.minor(), ev.withheld.minor()), (20000, 1800, 900, 0));
    assert_eq!(ev.kind, "input");
}

#[test]
fn withholding_code_on_a_supplier_invoice_reduces_the_payable() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let b = books(&mut c);
    let e = journal::create_entry(&mut c, entry(&b, "2026-05-15", vec![taxed(b.services.id, "1000", "WHT"), PostingInput::balancing(b.payables.id)])).unwrap();
    assert_eq!(amounts(&e), vec![(b.services.id, 100000), (b.payables.id, -90000), (b.wht.id, -10000)]);
    let ev = &tax::tax_events_for_entry(&c, e.id).unwrap()[0];
    assert_eq!((ev.base.minor(), ev.tax.minor(), ev.recoverable.minor(), ev.withheld.minor()), (100000, 10000, 0, 10000));
    let rows = tax::tax_summary(&c, b.entity.id, d("2026-05-01"), d("2026-05-31")).unwrap();
    assert_eq!((rows[0].code.as_str(), rows[0].withheld.minor()), ("WHT", 10000));
}

#[test]
fn foreign_currency_posting_keeps_native_values() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let b = books(&mut c);
    rates::set_price(&c, "USD", "SGD", d("2026-01-01"), n("1.35"), "manual").unwrap();
    let e = journal::create_entry(&mut c, entry(&b, "2026-06-01", vec![PostingInput::balancing(b.bank.id), taxed(b.export.id, "-100", "SR")])).unwrap();
    let ev = &tax::tax_events_for_entry(&c, e.id).unwrap()[0];
    assert_eq!((ev.base.minor(), ev.tax.minor()), (-13500, -1215));
    assert_eq!((ev.native_base.unwrap().minor(), ev.native_tax.unwrap().minor(), ev.native_base.unwrap().commodity()), (-10000, -900, "USD"));
    let zero = journal::create_entry(&mut c, entry(&b, "2026-06-02", vec![PostingInput::balancing(b.bank.id), taxed(b.export.id, "-50", "ZR")])).unwrap();
    assert_eq!(zero.postings.len(), 2);
    let ev = &tax::tax_events_for_entry(&c, zero.id).unwrap()[0];
    assert_eq!((ev.kind.as_str(), ev.base.minor(), ev.tax.minor()), ("zero_rated", -6750, 0));
}

#[test]
fn void_creates_mirror_events() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let b = books(&mut c);
    let original = sale(&mut c, &b, "2026-03-10", "100");
    let first = tax::tax_events_for_entry(&c, original.id).unwrap().remove(0);
    let voided = journal::void_entry(&mut c, original.id, Some(d("2026-04-05")), "wrong customer").unwrap();
    let reversal = journal::get_entry(&c, voided.reversed_by_id.unwrap()).unwrap();
    let mirror = tax::tax_events_for_entry(&c, reversal.id).unwrap();
    assert_eq!(mirror.len(), 1);
    let m = &mirror[0];
    assert_eq!(m.corrects_event_id, Some(first.id));
    assert_eq!((m.base.minor(), m.tax.minor()), (10000, 900));
    assert_eq!(m.tax_point, d("2026-04-05"));
    assert_eq!(m.rule_version, first.rule_version);
    let reversed_sales = reversal.postings.iter().find(|p| p.account_id == b.sales.id).unwrap();
    assert_eq!(m.posting_id, Some(reversed_sales.id));
    // The original stays in its period; the correction falls in the period of the reversal.
    let march = tax::tax_summary(&c, b.entity.id, d("2026-03-01"), d("2026-03-31")).unwrap();
    assert_eq!((march[0].tax.minor(), march[0].count), (-900, 1));
    let april = tax::tax_summary(&c, b.entity.id, d("2026-04-01"), d("2026-04-30")).unwrap();
    assert_eq!((april[0].tax.minor(), april[0].count), (900, 1));
    let both = tax::tax_summary(&c, b.entity.id, d("2026-01-01"), d("2026-12-31")).unwrap();
    assert_eq!((both[0].base.minor(), both[0].tax.minor(), both[0].count), (0, 0, 2));
    // A second void request changes nothing.
    journal::review_void(&mut c, original.id, None, "again", true, false).unwrap();
    assert_eq!(tax::tax_events_for_entry(&c, reversal.id).unwrap().len(), 1);
}

#[test]
fn deleting_a_draft_deletes_its_events_and_an_edit_replaces_them() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let b = books(&mut c);
    let mut input = entry(&b, "2026-03-10", vec![PostingInput::balancing(b.bank.id), taxed(b.sales.id, "-100", "SR")]);
    input.status = EntryStatus::Draft;
    let draft = journal::create_entry(&mut c, input.clone()).unwrap();

    // An edit with tax blocks drops the old tax line and records a new event.
    input.postings[1] = PostingInput { tax: Some(TaxInput { code: "SR".into(), tax_amount: Some(n("8.99")), ..Default::default() }), ..PostingInput::new(b.sales.id, n("-100")) };
    let mut with_old_line = input.clone();
    with_old_line.postings.push(PostingInput::new(b.gst_out.id, n("-9")).meta("tax_line", serde_json::json!("SR")));
    let edited = journal::update_entry(&mut c, draft.id, with_old_line).unwrap();
    assert_eq!(amounts(&edited), vec![(b.bank.id, 10899), (b.sales.id, -10000), (b.gst_out.id, -899)]);
    let events = tax::tax_events_for_entry(&c, draft.id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].tax.minor(), -899);
    assert_eq!(events[0].posting_id, Some(edited.postings[1].id));

    // An edit without tax blocks keeps the event on the replacement posting.
    let mut plain = EntryInput::new(b.entity.id, d("2026-03-11"));
    plain.status = EntryStatus::Draft;
    plain.postings = edited.postings.iter().map(|p| PostingInput { amount: Some(p.amount.major()), metadata: p.metadata.clone(), ..PostingInput::new(p.account_id, p.quantity.major()) }).collect();
    let moved = journal::update_entry(&mut c, draft.id, plain).unwrap();
    let events = tax::tax_events_for_entry(&c, draft.id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].posting_id, Some(moved.postings[1].id));

    journal::delete_entry(&mut c, draft.id).unwrap();
    let left: i64 = c.query_row("SELECT COUNT(*) FROM tax_events", [], |r| r.get(0)).unwrap();
    assert_eq!(left, 0);
}

#[test]
fn summary_sums_by_code_and_period() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let b = books(&mut c);
    sale(&mut c, &b, "2026-01-05", "100");
    sale(&mut c, &b, "2026-01-20", "50");
    sale(&mut c, &b, "2026-02-03", "200");
    journal::create_entry(&mut c, entry(&b, "2026-01-25", vec![taxed(b.services.id, "300", "TX"), PostingInput::balancing(b.bank.id)])).unwrap();
    let mut draft = entry(&b, "2026-01-28", vec![PostingInput::balancing(b.bank.id), taxed(b.sales.id, "-1000", "SR")]);
    draft.status = EntryStatus::Draft;
    journal::create_entry(&mut c, draft).unwrap();
    // A tax point in another period moves the event there.
    let mut late = entry(&b, "2026-01-31", vec![PostingInput::balancing(b.bank.id), taxed(b.sales.id, "-10", "SR")]);
    late.postings[1].tax.as_mut().unwrap().tax_point = Some(d("2026-02-01"));
    journal::create_entry(&mut c, late).unwrap();

    let jan = tax::tax_summary(&c, b.entity.id, d("2026-01-01"), d("2026-01-31")).unwrap();
    let rows: Vec<_> = jan.iter().map(|r| (r.code.as_str(), r.kind.as_str(), r.base.minor(), r.tax.minor(), r.recoverable.minor(), r.count)).collect();
    assert_eq!(rows, vec![("SR", "output", -15000, -1350, 0, 2), ("TX", "input", 30000, 2700, 1350, 1)]);
    let feb = tax::tax_summary(&c, b.entity.id, d("2026-02-01"), d("2026-02-28")).unwrap();
    assert_eq!(feb.len(), 1);
    assert_eq!((feb[0].base.minor(), feb[0].tax.minor(), feb[0].count), (-21000, -1890, 2));

    let listed = tax::tax_events_list(&c, b.entity.id, d("2026-01-01"), d("2026-01-31"), Some("sr")).unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().all(|e| e.code == "SR" && e.entry_status == "posted"));
    assert!(matches!(tax::tax_summary(&c, b.entity.id, d("2026-02-01"), d("2026-01-01")), Err(Error::Invalid(_))));
}

#[test]
fn validation_rejects_inactive_codes_other_entities_and_other_currencies() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let b = books(&mut c);
    let other = entities::create_entity(&mut c, "Rio Verde SL", "company", "ES", "EUR").unwrap();
    let other_bank = accounts::ensure_account(&c, other.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
    let other_sales = accounts::ensure_account(&c, other.id, AccountType::Income, &["Sales"], "income", "EUR").unwrap();

    // Inactive code: the tax point comes before the first version.
    let early = entry(&b, "2025-12-31", vec![PostingInput::balancing(b.bank.id), taxed(b.sales.id, "-100", "SR")]);
    assert!(matches!(journal::create_entry(&mut c, early), Err(Error::Invalid(_))));
    // Unknown code, and a code of another entity.
    let mut foreign = EntryInput::new(other.id, d("2026-03-01"));
    foreign.postings = vec![PostingInput::balancing(other_bank.id), taxed(other_sales.id, "-100", "SR")];
    assert!(matches!(journal::create_entry(&mut c, foreign), Err(Error::NotFound(_))));
    // A balancing posting cannot carry a tax block.
    let balancing =
        entry(&b, "2026-03-01", vec![PostingInput::new(b.bank.id, n("109")), PostingInput { tax: Some(TaxInput { code: "SR".into(), ..Default::default() }), ..PostingInput::balancing(b.sales.id) }]);
    assert!(matches!(journal::create_entry(&mut c, balancing), Err(Error::Invalid(_))));
    // A signed magnitude is refused.
    let mut signed = entry(&b, "2026-03-01", vec![PostingInput::balancing(b.bank.id), taxed(b.sales.id, "-100", "SR")]);
    signed.postings[1].tax.as_mut().unwrap().base_amount = Some(n("-100"));
    assert!(matches!(journal::create_entry(&mut c, signed), Err(Error::Invalid(_))));
    // A zero-rated code cannot carry tax.
    let mut zero = entry(&b, "2026-03-01", vec![PostingInput::balancing(b.bank.id), taxed(b.sales.id, "-100", "ZR")]);
    zero.postings[1].tax.as_mut().unwrap().tax_amount = Some(n("1"));
    assert!(matches!(journal::create_entry(&mut c, zero), Err(Error::Invalid(_))));
    // The draft interface reports the same refusals with stable codes.
    let doc: DraftDocument = serde_json::from_value(serde_json::json!({
        "date": "2026-03-01",
        "postings": [{"account": "Assets:Bank:Operating"}, {"account": "Income:Sales", "quantity": "-100", "tax": {"code": "SR", "tax_amount": "-9"}}]
    }))
    .unwrap();
    assert_eq!(proposal::create_draft(&mut c, b.entity.id, &doc).unwrap_err().code, "invalid_input");
    assert!(serde_json::from_value::<DraftDocument>(serde_json::json!({"date": "2026-03-01", "postings": [{"account": "x", "tax": {"code": "SR", "rate": "9"}}]})).is_err());

    // record_tax_event validates the entry, the code, the currency and the amounts.
    let posted = sale(&mut c, &b, "2026-03-10", "100");
    let mut other_entry = EntryInput::new(other.id, d("2026-03-10"));
    other_entry.postings = vec![PostingInput::new(other_bank.id, n("10")), PostingInput::balancing(other_sales.id)];
    let other_entry = journal::create_entry(&mut c, other_entry).unwrap();
    let base = TaxEventInput {
        entity_id: b.entity.id,
        entry_id: posted.id,
        posting_id: Some(posted.postings[1].id),
        code: "SR".into(),
        tax_point: d("2026-03-10"),
        base_amount: n("-10"),
        tax_amount: n("-0.90"),
        commodity: "SGD".into(),
        ..Default::default()
    };
    assert!(matches!(tax::record_tax_event(&mut c, TaxEventInput { entry_id: other_entry.id, posting_id: None, ..base.clone() }), Err(Error::Invalid(_))));
    assert!(matches!(tax::record_tax_event(&mut c, TaxEventInput { commodity: "EUR".into(), ..base.clone() }), Err(Error::Invalid(_))));
    assert!(matches!(tax::record_tax_event(&mut c, TaxEventInput { tax_point: d("2025-06-30"), ..base.clone() }), Err(Error::Invalid(_))));
    assert!(matches!(tax::record_tax_event(&mut c, TaxEventInput { base_amount: n("10"), tax_amount: n("0.90"), ..base.clone() }), Err(Error::Invalid(_))));
    assert!(matches!(tax::record_tax_event(&mut c, TaxEventInput { code: "TX".into(), recoverable_amount: Some(n("-1")), ..base.clone() }), Err(Error::Invalid(_))));
    assert!(matches!(tax::record_tax_event(&mut c, TaxEventInput { tax_amount: n("0.90"), ..base.clone() }), Err(Error::Invalid(_))));
    let ok = tax::record_tax_event(&mut c, TaxEventInput { rule_version: Some("manual-2026".into()), document_ref: "ADJ-1".into(), ..base }).unwrap();
    assert_eq!((ok.base.minor(), ok.tax.minor(), ok.rule_version.as_str()), (-1000, -90, "manual-2026"));
    assert_eq!(audit::history(&c, "tax_events", ok.id).unwrap()[0].action, "record");
}

#[test]
fn migration_22_adds_empty_tax_tables() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    db::migrate_to(&conn, 21).unwrap();
    conn.execute("INSERT INTO entities (uid, name, kind, country, currency, created_at, updated_at) VALUES ('e-1', 'Rio Verde SL', 'company', 'ES', 'EUR', 't', 't')", []).unwrap();
    db::migrate_to(&conn, i64::MAX).unwrap();
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(version, db::schema_version());
    assert!(tax::list_tax_codes(&conn, 1, None).unwrap().is_empty());
    assert!(tax::tax_summary(&conn, 1, d("2026-01-01"), d("2026-12-31")).unwrap().is_empty());
}

/// A draft dated after the only USD rate: its foreign posting has a rate that the lookup found.
fn foreign_draft(c: &mut Connection, b: &Books, postings: Vec<PostingInput>) -> JournalEntry {
    rates::set_price(c, "USD", "SGD", d("2026-03-01"), n("1.30"), "test").unwrap();
    let mut input = entry(b, "2026-03-05", postings);
    input.status = EntryStatus::Draft;
    journal::create_entry(c, input).unwrap()
}

#[test]
fn the_draft_revaluation_skips_a_draft_with_tax_events_and_lists_it() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let b = books(&mut c);
    let draft = foreign_draft(&mut c, &b, vec![PostingInput::balancing(b.bank.id), taxed(b.export.id, "-100", "ZR")]);
    assert!(!tax::tax_events_for_entry(&c, draft.id).unwrap().is_empty());
    let before = amounts(&journal::get_entry(&c, draft.id).unwrap());

    rates::set_price(&c, "USD", "SGD", d("2026-03-05"), n("1.35"), "test").unwrap();
    let report = rates::revalue_missing(&mut c).unwrap();
    assert_eq!(report.skipped_tax_events, vec![draft.id]);
    assert_eq!((report.drafts, report.failed), (0, 0));
    assert_eq!(amounts(&journal::get_entry(&c, draft.id).unwrap()), before);
}

#[test]
fn the_draft_revaluation_keeps_rate_overrides_and_memos() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let b = books(&mut c);
    let looked_up = PostingInput { memo: "invoice 7".into(), ..PostingInput::new(b.export.id, n("-100")) };
    // An explicit book value is a rate override: 50 USD booked at 70 SGD.
    let fixed = PostingInput { memo: "agreed rate".into(), amount: Some(n("-70")), ..PostingInput::new(b.export.id, n("-50")) };
    let draft = foreign_draft(&mut c, &b, vec![looked_up, fixed, PostingInput { memo: "bank leg".into(), ..PostingInput::balancing(b.bank.id) }]);
    let before = journal::get_entry(&c, draft.id).unwrap();
    assert_eq!(before.postings[1].rate_source, "input");

    rates::set_price(&c, "USD", "SGD", d("2026-03-05"), n("1.35"), "test").unwrap();
    let report = rates::revalue_missing(&mut c).unwrap();
    assert!(report.skipped_tax_events.is_empty());
    assert_eq!(report.drafts, 1);
    let after = journal::get_entry(&c, draft.id).unwrap();
    let memos: Vec<&str> = after.postings.iter().map(|p| p.memo.as_str()).collect();
    assert_eq!(memos, ["invoice 7", "agreed rate", "bank leg"]);
    // The looked-up posting takes the new rate; the override keeps its book value and source.
    assert_eq!(after.postings[0].amount.minor(), -13500);
    assert_eq!((after.postings[1].amount.minor(), after.postings[1].rate_source.as_str()), (-7000, "input"));
    assert_eq!(after.postings[2].amount.minor(), 20500);
}
