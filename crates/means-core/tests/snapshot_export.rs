//! The snapshot export: a complete, hashed and atomic read of the books.
//! All names, dates and amounts are fictional.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use means_core::documents::{self, Link};
use means_core::groups::{self, NewMember};
use means_core::invoices::{self, InvoiceInput, InvoiceLineInput, SettlementInput, SettlementKind};
use means_core::snapshot::{self, SnapshotOptions};
use means_core::tax::{self, NewTaxCode};
use means_core::{accounts, entities, journal, payees, proposal, reports, AccountType, Db, EntryInput, EntryStatus, JournalEntry, PostingInput, TaxInput};
use rusqlite::{params, Connection};
use rust_decimal::Decimal;
use serde_json::Value;

const BULK: usize = 2_100;

fn day(s: &str) -> NaiveDate {
    s.parse().unwrap()
}

fn dec(s: &str) -> Decimal {
    s.parse().unwrap()
}

struct Fixture {
    dir: PathBuf,
    db: Db,
    parent: i64,
    child: i64,
    voided: String,
    reversal: String,
    draft: String,
    confirmed: String,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn payee(c: &mut Connection, entity: i64, name: &str) -> i64 {
    let change = payees::Change { id: 0, entity_id: entity, name: name.into(), active: true, aliases: vec![] };
    let preview = payees::save(c, change.clone(), None).unwrap();
    payees::save(c, change, Some(&preview.token)).unwrap().payee.unwrap().id
}

#[allow(clippy::too_many_arguments)]
fn simple(c: &mut Connection, entity: i64, date: &str, payee: &str, from: i64, to: i64, amount: &str, status: EntryStatus) -> JournalEntry {
    let mut input = EntryInput::new(entity, day(date));
    input.payee = payee.into();
    input.status = status;
    input.postings = vec![PostingInput::new(from, -dec(amount)), PostingInput::balancing(to)];
    journal::create_entry(c, input).unwrap()
}

fn fixture(name: &str) -> Fixture {
    let dir = std::env::temp_dir().join(format!("means-snapshot-{name}-{}", means_core::new_uid()));
    let db = Db::open(dir.join("ledger.db")).unwrap();
    let mut c = db.conn();
    let parent = entities::create_entity(&mut c, "Harbor Holdings Ltd", "company", "IE", "EUR").unwrap().id;
    let child = entities::create_entity(&mut c, "Harbor Trading Ltd", "company", "IE", "EUR").unwrap().id;
    let group = groups::create_group(&mut c, "Harbor Group").unwrap();
    groups::add_member(&mut c, NewMember::new(group.id, parent, day("2025-01-01"))).unwrap();
    groups::add_member(&mut c, NewMember { parent_entity_id: Some(parent), ownership_bp: 8000, ..NewMember::new(group.id, child, day("2025-01-01")) }).unwrap();

    // The parent: a large journal.
    let bank = accounts::ensure_account(&c, parent, AccountType::Asset, &["Bank", "Operating"], "bank", "EUR").unwrap().id;
    let office = accounts::ensure_account(&c, parent, AccountType::Expense, &["Office"], "expense", "EUR").unwrap().id;
    let capital = accounts::ensure_account(&c, parent, AccountType::Equity, &["Capital"], "equity", "EUR").unwrap().id;
    simple(&mut c, parent, "2025-01-01", "Opening", capital, bank, "500000.00", EntryStatus::Posted);
    for i in 0..BULK {
        let date = day("2025-01-02") + chrono::Days::new((i % 360) as u64);
        let amount = format!("{}.{:02}", 1 + i % 90, i % 100);
        simple(&mut c, parent, &date.to_string(), "Sample Stationers", bank, office, &amount, EntryStatus::Posted);
    }

    // The child: tax, a void across a month end, a draft, a document, an invoice and evidence.
    let cbank = accounts::ensure_account(&c, child, AccountType::Asset, &["Bank", "Main"], "bank", "EUR").unwrap().id;
    let sales = accounts::ensure_account(&c, child, AccountType::Income, &["Sales"], "income", "EUR").unwrap().id;
    let vat_out = accounts::ensure_account(&c, child, AccountType::Liability, &["Tax", "Output VAT"], "tax", "EUR").unwrap().id;
    let software = accounts::ensure_account(&c, child, AccountType::Expense, &["Software"], "expense", "EUR").unwrap().id;
    accounts::ensure_account(&c, child, AccountType::Liability, &["Accounts Payable"], "payable", "EUR").unwrap();
    accounts::ensure_account(&c, child, AccountType::Asset, &["Tax", "Input VAT"], "tax", "EUR").unwrap();
    payee(&mut c, child, "Sample Supplier Ltd");
    tax::add_tax_code(
        &mut c,
        NewTaxCode {
            entity_id: child,
            code: "SR".into(),
            name: "Standard rated".into(),
            kind: "output".into(),
            rate_bp: 2300,
            tax_account_id: Some(vat_out),
            recoverable_bp: None,
            jurisdiction: "IE".into(),
            effective_from: day("2025-01-01"),
        },
    )
    .unwrap();
    let mut sale = EntryInput::new(child, day("2026-01-10"));
    sale.payee = "Example Customer".into();
    sale.postings = vec![PostingInput::balancing(cbank), PostingInput { tax: Some(TaxInput { code: "SR".into(), ..Default::default() }), ..PostingInput::new(sales, dec("-1000.00")) }];
    let sale = journal::create_entry(&mut c, sale).unwrap();

    let voided = simple(&mut c, child, "2026-01-30", "Sample Cloud", cbank, software, "49.00", EntryStatus::Posted);
    let reversal = journal::review_void(&mut c, voided.id, Some(day("2026-02-02")), "duplicate charge", true).unwrap().reversal;
    let draft = simple(&mut c, child, "2026-01-20", "Sample Cloud", cbank, software, "12.00", EntryStatus::Draft);
    let to_confirm = simple(&mut c, child, "2026-01-21", "Sample Cloud", cbank, software, "15.00", EntryStatus::Draft);
    let digest = proposal::propose(&c, to_confirm.id).unwrap().proposal_digest;
    proposal::confirm(&mut c, to_confirm.id, &digest, "Alex Example").unwrap();

    let doc = documents::add_document(&mut c, child, b"%PDF-1.4 fictional receipt 0042", "receipt-0042.pdf", "upload", "2026-01-11", serde_json::json!({})).unwrap();
    documents::link_document(&mut c, doc.id, Link::Entry(sale.id), "invoice").unwrap();

    let recorded = invoices::record_invoice(
        &mut c,
        child,
        &InvoiceInput {
            direction: "purchase".into(),
            party: Some("Sample Supplier Ltd".into()),
            number: "F-7".into(),
            issue_date: "2026-01-05".into(),
            due_date: Some("2026-02-04".into()),
            commodity: "EUR".into(),
            net: "100.00".into(),
            tax: Some("23.00".into()),
            gross: "123.00".into(),
            lines: vec![InvoiceLineInput {
                description: "Licence".into(),
                quantity: Some("1".into()),
                unit_price: Some("100.00".into()),
                tax_code: "VAT23".into(),
                tax: Some("23.00".into()),
                account: Some("Expenses:Software".into()),
                ..Default::default()
            }],
            payable_account: Some("Liabilities:Accounts Payable".into()),
            tax_account: Some("Assets:Tax:Input VAT".into()),
            recognize: true,
            ..Default::default()
        },
    )
    .unwrap();
    journal::post_entry(&mut c, recorded.recognition.unwrap().id).unwrap();
    let payable = accounts::find_by_path(&c, child, "Liabilities:Accounts Payable").unwrap().unwrap().id;
    let payment = simple(&mut c, child, "2026-01-25", "Sample Supplier Ltd", cbank, payable, "123.00", EntryStatus::Posted);
    invoices::settle(
        &mut c,
        SettlementInput {
            invoice_uid: recorded.invoice.uid.clone(),
            entry_uid: Some(payment.uid.clone()),
            posting_uid: None,
            date: day("2026-01-25"),
            amount: dec("123.00"),
            kind: SettlementKind::Payment,
            fx_account: None,
            control_account: None,
        },
    )
    .unwrap();

    // Statement lines that evidence the bank postings of the sale and the payment.
    c.execute("INSERT INTO imports (uid, source, account_id, checksum, created_at) VALUES ('import-fixture-1', 'generic_csv', ?1, 'fixture', '2026-02-01')", [cbank]).unwrap();
    let import_id = c.last_insert_rowid();
    for (position, entry, amount, date) in [(0, &sale, 123000, "2026-01-10"), (1, &payment, -12300, "2026-01-25")] {
        let posting = entry.postings.iter().find(|p| p.account_id == cbank).unwrap().id;
        c.execute(
            "INSERT INTO statement_lines (import_id, account_id, position, raw, date, amount, currency, description, reference, fingerprint, posting_id, journal_entry_id, status)
             VALUES (?1, ?2, ?3, '{\"counterparty\":\"Example Counterparty\"}', ?4, ?5, 'EUR', 'Card movement', ?6, ?7, ?8, ?9, 'matched')",
            params![import_id, cbank, position, date, amount, format!("REF-{position}"), format!("fp-{position}"), posting, entry.id],
        )
        .unwrap();
    }
    drop(c);
    Fixture { dir, db, parent, child, voided: voided.uid, reversal: reversal.uid, draft: draft.uid, confirmed: to_confirm.uid }
}

fn read_lines(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn read_array(path: &Path) -> Vec<Value> {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn uid(c: &Connection, table: &str, id: i64) -> String {
    c.query_row(&format!("SELECT uid FROM {table} WHERE id = ?1"), [id], |r| r.get(0)).unwrap()
}

#[test]
fn a_snapshot_holds_every_record_once_and_verifies() {
    let f = fixture("full");
    let out = f.dir.join("snapshot");
    let outcome = snapshot::export(&mut f.db.conn(), &out, &SnapshotOptions::default()).unwrap();
    let manifest = &outcome.manifest;
    assert_eq!(manifest["schema_version"], 1);
    assert_eq!(manifest["generator"]["name"], "means");
    assert_eq!(manifest["posting_policy"], "normal");
    assert!(manifest["scope"]["as_of"].is_null());

    // Every entry appears once. Drafts are separate.
    let entries = read_lines(&out.join("entries.jsonl"));
    let drafts = read_lines(&out.join("drafts.jsonl"));
    let c = f.db.conn();
    let all: HashSet<String> = {
        let mut stmt = c.prepare("SELECT uid FROM journal_entries").unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    };
    let exported: Vec<String> = entries.iter().chain(&drafts).map(|e| e["uid"].as_str().unwrap().to_string()).collect();
    assert_eq!(exported.len(), all.len(), "each entry appears once");
    assert_eq!(exported.iter().cloned().collect::<HashSet<_>>(), all);
    assert!(entries.len() > BULK);
    assert!(entries.iter().all(|e| e["status"] == "posted" || e["status"] == "void"));
    assert!(drafts.iter().all(|e| e["status"] == "draft"));
    assert!(drafts.iter().any(|e| e["uid"] == f.draft.as_str()));
    assert_eq!(manifest["counts"]["entries"], entries.len());
    assert_eq!(manifest["counts"]["drafts"], drafts.len());

    // The void and its reversal are linked across the month boundary.
    let by_uid: HashMap<&str, &Value> = entries.iter().map(|e| (e["uid"].as_str().unwrap(), e)).collect();
    assert_eq!(by_uid[f.voided.as_str()]["status"], "void");
    assert_eq!(by_uid[f.reversal.as_str()]["reverses_uid"], f.voided.as_str());
    assert_eq!(by_uid[f.reversal.as_str()]["date"], "2026-02-02");
    assert_eq!(by_uid[f.confirmed.as_str()]["reviewed_by"], "Alex Example");

    // The trial balance from entries.jsonl equals the report of each entity.
    for entity in [f.parent, f.child] {
        let entity_uid = uid(&c, "entities", entity);
        let mut from_file: BTreeMap<String, i64> = BTreeMap::new();
        let (mut debits, mut credits) = (0i64, 0i64);
        for e in entries.iter().filter(|e| e["entity_uid"] == entity_uid.as_str()) {
            for p in e["postings"].as_array().unwrap() {
                let minor = p["amount"]["minor"].as_str().unwrap().parse::<i64>().unwrap();
                *from_file.entry(p["account_uid"].as_str().unwrap().to_string()).or_default() += minor;
                if minor > 0 {
                    debits += minor;
                } else {
                    credits -= minor;
                }
            }
        }
        from_file.retain(|_, v| *v != 0);
        let report = reports::trial_balance(&c, entity, None).unwrap();
        let from_report: BTreeMap<String, i64> = report.rows.iter().map(|r| (uid(&c, "accounts", r.account_id), r.debit.minor() - r.credit.minor())).collect();
        assert_eq!(from_file, from_report, "trial balance of entity {entity}");
        let totals = &manifest["control_totals"][entity_uid.as_str()];
        assert_eq!(report.total_debit, report.total_credit);
        assert_eq!(debits, credits);
        assert_eq!(totals["debits"]["minor"], debits.to_string());
        assert_eq!(totals["credits"]["minor"], credits.to_string());
        assert_eq!(totals["debits"]["commodity"], "EUR");
        let head: (i64, String) =
            c.query_row("SELECT seq, hash FROM journal_entries WHERE entity_id = ?1 AND seq IS NOT NULL ORDER BY seq DESC LIMIT 1", [entity], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(manifest["chain_heads"][entity_uid.as_str()]["seq"], head.0);
        assert_eq!(manifest["chain_heads"][entity_uid.as_str()]["hash"], head.1);
    }

    // The group, the evidence, the tax events and the invoice.
    let ents = read_array(&out.join("entities.json"));
    let child_row = ents.iter().find(|e| e["name"] == "Harbor Trading Ltd").unwrap();
    assert_eq!(child_row["group"]["group_name"], "Harbor Group");
    assert_eq!(child_row["group"]["ownership_bp"], 8000);
    assert_eq!(child_row["group"]["parent_entity_uid"], uid(&c, "entities", f.parent));
    let docs = read_array(&out.join("documents.json"));
    assert_eq!(docs.len(), 1);
    let doc_path = docs[0]["path"].as_str().unwrap();
    assert_eq!(std::fs::read(out.join(doc_path)).unwrap(), b"%PDF-1.4 fictional receipt 0042");
    assert!(manifest["files"][doc_path]["sha256"].is_string());
    assert_eq!(docs[0]["links"][0]["role"], "invoice");
    let lines = read_array(&out.join("statement_lines.json"));
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["counterparty"], "Example Counterparty");
    for line in &lines {
        let posting = line["posting_uids"][0].as_str().unwrap();
        let linked = entries.iter().flat_map(|e| e["postings"].as_array().unwrap()).find(|p| p["uid"] == posting).unwrap();
        assert_eq!(linked["statement_line_ids"][0], line["id"]);
    }
    let events = read_lines(&out.join("tax_events.jsonl"));
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["tax"], serde_json::json!({"minor": "-23000", "commodity": "EUR", "precision": 2}));
    let taxed = entries.iter().flat_map(|e| e["postings"].as_array().unwrap()).find(|p| p["uid"] == events[0]["posting_uid"]).unwrap();
    assert_eq!(taxed["tax_event_uids"][0], events[0]["uid"]);
    let invoices = read_array(&out.join("invoices.json"));
    assert_eq!(invoices.len(), 1);
    assert_eq!(invoices[0]["status"], "settled");
    assert_eq!(invoices[0]["settlements"][0]["amount"]["minor"], "12300");
    assert_eq!(manifest["counts"]["settlements"], 1);
    assert!(out.join("books.beancount").exists());
    // The inter-vault table exists in the current schema, so the file is always written.
    assert!(out.join("intervault_links.json").exists());
    assert_eq!(manifest["counts"]["intervault_links"], 0);
    let remaining: Vec<_> = std::fs::read_dir(&f.dir).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().contains(".tmp-")).collect();
    assert!(remaining.is_empty(), "no temporary directory stays");
    drop(c);

    // The manifest verifies.
    let report = snapshot::verify(&out).unwrap();
    assert!(report.ok, "{:?}", report.problems);
    assert_eq!(report.files_checked, manifest["files"].as_object().unwrap().len());

    // A second export to the same directory fails.
    let again = snapshot::export(&mut f.db.conn(), &out, &SnapshotOptions::default());
    assert!(matches!(again, Err(means_core::Error::Conflict(_))), "{again:?}");

    // A changed file fails the check.
    let path = out.join("entries.jsonl");
    let text = std::fs::read_to_string(&path).unwrap().replacen("Sample Stationers", "Sample Stationerz", 1);
    std::fs::write(&path, text).unwrap();
    let report = snapshot::verify(&out).unwrap();
    assert!(!report.ok);
    assert!(report.problems.iter().any(|p| p.contains("entries.jsonl")), "{:?}", report.problems);
}

#[test]
fn as_of_and_entity_scope_limit_the_snapshot() {
    let f = fixture("scope");
    let out = f.dir.join("january");
    let child_uid = uid(&f.db.conn(), "entities", f.child);
    let options = SnapshotOptions { entity_uids: vec![child_uid.clone()], as_of: Some(day("2026-01-31")) };
    let outcome = snapshot::export(&mut f.db.conn(), &out, &options).unwrap();
    assert_eq!(outcome.manifest["scope"]["as_of"], "2026-01-31");
    assert_eq!(outcome.manifest["scope"]["entity_uids"], serde_json::json!([child_uid]));
    let entries = read_lines(&out.join("entries.jsonl"));
    assert!(entries.iter().all(|e| e["entity_uid"] == child_uid.as_str() && e["date"].as_str().unwrap() <= "2026-01-31"));
    assert!(entries.iter().any(|e| e["uid"] == f.voided.as_str()), "the void original stays");
    assert!(!entries.iter().any(|e| e["uid"] == f.reversal.as_str()), "the reversal is after the day");
    let ents = read_array(&out.join("entities.json"));
    assert_eq!(ents.len(), 1);
    assert!(read_array(&out.join("accounts.json")).iter().all(|a| a["entity_uid"] == child_uid.as_str()));
    let beancount = std::fs::read_to_string(out.join("books.beancount")).unwrap();
    assert!(!beancount.contains("Sample Stationers"));
    assert!(!beancount.contains(&f.reversal));
    // The void without its reversal leaves the control totals equal: each entry balances.
    let report = snapshot::verify(&out).unwrap();
    assert!(report.ok, "{:?}", report.problems);
}

#[test]
fn only_postings_with_a_derived_rate_carry_the_marker() {
    let dir = std::env::temp_dir().join(format!("means-snapshot-derived-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::open(dir.join("ledger.db")).unwrap();
    let mut c = db.conn();
    let entity = entities::create_entity(&mut c, "Harbor Pte", "company", "SG", "SGD").unwrap();
    let usd = accounts::ensure_account(&c, entity.id, AccountType::Asset, &["Bank", "USD"], "bank", "USD").unwrap();
    let sgd = accounts::ensure_account(&c, entity.id, AccountType::Asset, &["Bank", "SGD"], "bank", "SGD").unwrap();
    let fees = accounts::ensure_account(&c, entity.id, AccountType::Expense, &["Fees"], "expense", "SGD").unwrap();
    means_core::rates::set_price(&c, "USD", "EUR", day("2026-03-02"), dec("0.9"), "ecb").unwrap();
    means_core::rates::set_price(&c, "SGD", "EUR", day("2026-03-02"), dec("0.7"), "ecb").unwrap();
    simple(&mut c, entity.id, "2026-03-02", "Wire fee", usd.id, fees.id, "10", EntryStatus::Posted);
    simple(&mut c, entity.id, "2026-03-02", "Card fee", sgd.id, fees.id, "5", EntryStatus::Posted);
    let out = dir.join("snapshot");
    snapshot::export(&mut c, &out, &SnapshotOptions::default()).unwrap();
    let postings: Vec<Value> = read_lines(&out.join("entries.jsonl")).iter().flat_map(|e| e["postings"].as_array().unwrap().clone()).collect();
    let usd_uid = uid(&c, "accounts", usd.id);
    assert_eq!(postings.len(), 4);
    for p in &postings {
        if p["account_uid"] == usd_uid.as_str() {
            assert_eq!((p["rate_source"].as_str(), p["derived"].as_bool()), (Some("cross:ECB"), Some(true)));
        } else {
            assert!(p.get("derived").is_none(), "{p}");
        }
    }
    assert_eq!(read_array(&out.join("rates.json")).len(), 2, "rates.json holds the stored rows only");
    assert!(snapshot::verify(&out).unwrap().ok);
    drop(c);
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}
