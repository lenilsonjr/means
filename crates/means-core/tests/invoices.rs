//! Invoices, open items and settlements: recognition, payments, credit notes, withholding,
//! exchange differences, the duplicate expense guard and the limits.

use chrono::NaiveDate;
use means_core::invoices::{self, InvoiceInput, InvoiceLineInput, InvoiceStatus, SettlementInput, SettlementKind};
use means_core::{accounts, audit, entities, journal, payees, rates, AccountType, Db, EntryInput, EntryStatus, JournalEntry, PostingInput};
use rusqlite::Connection;
use rust_decimal::Decimal;

fn day(s: &str) -> NaiveDate {
    s.parse().unwrap()
}

fn dec(s: &str) -> Decimal {
    s.parse().unwrap()
}

struct Books {
    db: Db,
    entity: i64,
    bank: i64,
    software: i64,
    supplier: i64,
}

fn payee(c: &mut Connection, entity: i64, name: &str) -> i64 {
    let change = payees::Change { id: 0, entity_id: entity, name: name.into(), active: true, aliases: vec![] };
    let preview = payees::save(c, change.clone(), None).unwrap();
    payees::save(c, change, Some(&preview.token)).unwrap().payee.unwrap().id
}

fn books() -> Books {
    let db = Db::open_memory().unwrap();
    let (entity, bank, software, supplier) = {
        let mut c = db.conn();
        let e = entities::create_entity(&mut c, "Rio Verde SL", "company", "ES", "EUR").unwrap().id;
        let bank = accounts::ensure_account(&c, e, AccountType::Asset, &["Bank", "Operating"], "bank", "EUR").unwrap().id;
        let software = accounts::ensure_account(&c, e, AccountType::Expense, &["Software"], "expense", "EUR").unwrap().id;
        accounts::ensure_account(&c, e, AccountType::Liability, &["Accounts Payable"], "payable", "EUR").unwrap();
        accounts::ensure_account(&c, e, AccountType::Asset, &["Accounts Receivable"], "receivable", "EUR").unwrap();
        accounts::ensure_account(&c, e, AccountType::Asset, &["Tax", "Input VAT"], "tax", "EUR").unwrap();
        accounts::ensure_account(&c, e, AccountType::Liability, &["Tax", "Withholding"], "tax", "EUR").unwrap();
        accounts::ensure_account(&c, e, AccountType::Expense, &["Exchange Differences"], "expense", "EUR").unwrap();
        accounts::ensure_account(&c, e, AccountType::Income, &["Consulting"], "income", "EUR").unwrap();
        let supplier = payee(&mut c, e, "Sample Supplier Ltd");
        (e, bank, software, supplier)
    };
    Books { db, entity, bank, software, supplier }
}

fn purchase(number: &str, net: &str, tax: &str, gross: &str) -> InvoiceInput {
    InvoiceInput {
        direction: "purchase".into(),
        party: Some("Sample Supplier Ltd".into()),
        number: number.into(),
        issue_date: "2026-03-02".into(),
        due_date: Some("2026-04-01".into()),
        commodity: "EUR".into(),
        net: net.into(),
        tax: Some(tax.into()),
        gross: gross.into(),
        lines: vec![InvoiceLineInput {
            description: "Annual licence".into(),
            quantity: Some("1".into()),
            unit_price: Some(net.into()),
            tax_code: if tax == "0.00" { String::new() } else { "VAT21".into() },
            tax: Some(tax.into()),
            account: Some("Expenses:Software".into()),
            ..Default::default()
        }],
        payable_account: Some("Liabilities:Accounts Payable".into()),
        tax_account: Some("Assets:Tax:Input VAT".into()),
        recognize: true,
        ..Default::default()
    }
}

impl Books {
    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.conn()
    }

    fn account(&self, path: &str) -> i64 {
        accounts::find_by_path(&self.conn(), self.entity, path).unwrap().unwrap().id
    }

    /// A payment from the bank to the payable: the usual shape after an invoice is known.
    fn payment(&self, date: &str, amount: &str) -> JournalEntry {
        let payable = self.account("Liabilities:Accounts Payable");
        let mut input = EntryInput::new(self.entity, day(date));
        input.payee = "Sample Supplier Ltd".into();
        input.origin = "import".into();
        input.postings = vec![PostingInput::new(self.bank, -dec(amount)), PostingInput::balancing(payable)];
        journal::create_entry(&mut self.conn(), input).unwrap()
    }

    /// A bank import that booked the expense directly, before the invoice arrived.
    fn bank_expense(&self, date: &str, amount: &str) -> JournalEntry {
        let mut input = EntryInput::new(self.entity, day(date));
        input.payee = "Sample Supplier Ltd".into();
        input.origin = "import".into();
        input.postings = vec![PostingInput::new(self.bank, -dec(amount)), PostingInput::balancing(self.software)];
        journal::create_entry(&mut self.conn(), input).unwrap()
    }

    fn settle(&self, uid: &str, entry: Option<&JournalEntry>, date: &str, amount: &str, kind: SettlementKind) -> means_core::proposal::AgentResult<invoices::Settled> {
        let input = SettlementInput {
            invoice_uid: uid.into(),
            entry_uid: entry.map(|e| e.uid.clone()),
            posting_uid: None,
            date: day(date),
            amount: dec(amount),
            kind,
            fx_account: None,
            control_account: None,
        };
        invoices::settle(&mut self.conn(), input)
    }

    /// The posted balance of an account, in minor units of the functional currency.
    fn balance(&self, account: i64) -> i64 {
        self.conn()
            .query_row("SELECT COALESCE(SUM(p.amount), 0) FROM postings p JOIN journal_entries e ON e.id = p.journal_entry_id WHERE p.account_id = ?1 AND e.status <> 'void'", [account], |r| r.get(0))
            .unwrap()
    }
}

#[test]
fn a_purchase_invoice_is_recorded_and_recognised_as_a_draft() {
    let b = books();
    let recorded = invoices::record_invoice(&mut b.conn(), b.entity, &purchase("F-100", "1000.00", "210.00", "1210.00")).unwrap();
    let invoice = &recorded.invoice;
    assert_eq!(invoice.status, InvoiceStatus::Open);
    assert_eq!((invoice.gross.minor(), invoice.open.minor(), invoice.settled.minor()), (121000, 121000, 0));
    assert_eq!(invoice.party_name.as_deref(), Some("Sample Supplier Ltd"));
    assert_eq!(recorded.lines.len(), 1);
    assert_eq!(recorded.lines[0].account_path.as_deref(), Some("Expenses:Software"));

    let entry = recorded.recognition.expect("recognition entry");
    assert_eq!(entry.status, EntryStatus::Draft);
    assert_eq!(entry.origin, "invoice");
    assert!(entry.reviewed_at.is_none());
    assert_eq!(entry.date, day("2026-03-02"));
    let by_path = |path: &str| entry.postings.iter().find(|p| p.account_path == path).unwrap().amount.minor();
    assert_eq!(by_path("Expenses:Software"), 100000);
    assert_eq!(by_path("Assets:Tax:Input VAT"), 21000);
    assert_eq!(by_path("Liabilities:Accounts Payable"), -121000);
    assert_eq!(invoice.recognition_entry_uid.as_deref(), Some(entry.uid.as_str()));

    // The audit log records the invoice.
    let history = audit::history(&b.conn(), "invoices", invoice.id).unwrap();
    assert_eq!(history[0].action, "record");

    // The same number again is a duplicate. Totals that do not add up are refused.
    let again = invoices::record_invoice(&mut b.conn(), b.entity, &purchase("F-100", "1000.00", "210.00", "1210.00")).unwrap_err();
    assert_eq!(again.code, "duplicate");
    let wrong = invoices::record_invoice(&mut b.conn(), b.entity, &purchase("F-101", "1000.00", "210.00", "1200.00")).unwrap_err();
    assert_eq!(wrong.code, "invalid_input");
    let mut withheld = purchase("F-102", "1000.00", "0.00", "1000.00");
    withheld.withheld = Some("1000.01".into());
    assert_eq!(invoices::record_invoice(&mut b.conn(), b.entity, &withheld).unwrap_err().code, "invalid_input");

    // A party of another entity is refused.
    let other = entities::create_entity(&mut b.conn(), "Northwind Holdings Pte. Ltd.", "company", "SG", "SGD").unwrap().id;
    let foreign = payee(&mut b.conn(), other, "Blue Harbor LLC");
    let uid: String = b.conn().query_row("SELECT uid FROM payees WHERE id = ?1", [foreign], |r| r.get(0)).unwrap();
    let mut input = purchase("F-103", "10.00", "0.00", "10.00");
    input.party = Some(uid);
    assert_eq!(invoices::record_invoice(&mut b.conn(), b.entity, &input).unwrap_err().code, "invalid_input");
}

#[test]
fn a_bank_payment_settles_the_invoice_in_full() {
    let b = books();
    let recorded = invoices::record_invoice(&mut b.conn(), b.entity, &purchase("F-200", "1000.00", "210.00", "1210.00")).unwrap();
    let uid = recorded.invoice.uid.clone();
    let bank = b.payment("2026-03-20", "1210.00");
    let settled = b.settle(&uid, Some(&bank), "2026-03-20", "1210.00", SettlementKind::Payment).unwrap();
    assert_eq!(settled.invoice.status, InvoiceStatus::Settled);
    assert!(settled.invoice.open.is_zero());
    assert!(!settled.converted);
    assert!(settled.fx_difference.is_none());
    // The settlement links the bank posting.
    let bank_posting = bank.postings.iter().find(|p| p.account_id == b.bank).unwrap();
    assert_eq!(settled.settlement.posting_uid.as_deref(), Some(bank_posting.uid.as_str()));
    assert_eq!(invoices::open_items(&b.conn(), b.entity, day("2026-04-30"), None).unwrap().len(), 0);
    // Before the payment date the invoice is still open.
    let items = invoices::open_items(&b.conn(), b.entity, day("2026-03-10"), None).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].open.minor(), 121000);
}

#[test]
fn a_partial_payment_leaves_the_invoice_partially_settled_and_a_credit_note_settles_the_rest() {
    let b = books();
    let recorded = invoices::record_invoice(&mut b.conn(), b.entity, &purchase("F-300", "1000.00", "0.00", "1000.00")).unwrap();
    let uid = recorded.invoice.uid.clone();
    let bank = b.payment("2026-03-15", "600.00");
    let settled = b.settle(&uid, Some(&bank), "2026-03-15", "600.00", SettlementKind::Payment).unwrap();
    assert_eq!(settled.invoice.status, InvoiceStatus::PartiallySettled);
    assert_eq!(settled.invoice.open.minor(), 40000);

    let items = invoices::open_items(&b.conn(), b.entity, day("2026-04-11"), Some(invoices::Direction::Purchase)).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!((items[0].open.minor(), items[0].settled.minor(), items[0].days_overdue), (40000, 60000, 10));

    // A credit note larger than the open balance is refused.
    let mut too_big = purchase("CN-1", "500.00", "0.00", "500.00");
    too_big.direction = String::new();
    too_big.commodity = String::new();
    assert_eq!(invoices::credit_note(&mut b.conn(), &uid, &too_big).unwrap_err().code, "over_settlement");

    let mut note = purchase("CN-1", "400.00", "0.00", "400.00");
    note.direction = String::new();
    note.commodity = String::new();
    note.payable_account = None;
    let credited = invoices::credit_note(&mut b.conn(), &uid, &note).unwrap();
    assert_eq!(credited.invoice.status, InvoiceStatus::Settled);
    assert_eq!(credited.settlement.kind, SettlementKind::CreditNote);
    assert_eq!(credited.credit_note.invoice.kind, invoices::InvoiceKind::CreditNote);
    assert_eq!(credited.credit_note.invoice.status, InvoiceStatus::Settled);
    assert_eq!(credited.credit_note.invoice.credits_invoice_uid.as_deref(), Some(uid.as_str()));
    // The credit note takes the control account of the invoice and turns the sides around.
    let entry = credited.credit_note.recognition.expect("credit note recognition");
    let by_path = |path: &str| entry.postings.iter().find(|p| p.account_path == path).unwrap().amount.minor();
    assert_eq!(by_path("Liabilities:Accounts Payable"), 40000);
    assert_eq!(by_path("Expenses:Software"), -40000);
}

#[test]
fn withholding_settles_the_part_that_the_payer_keeps_back() {
    let b = books();
    let mut input = purchase("F-400", "1000.00", "0.00", "1000.00");
    input.withheld = Some("150.00".into());
    let recorded = invoices::record_invoice(&mut b.conn(), b.entity, &input).unwrap();
    let uid = recorded.invoice.uid.clone();
    assert_eq!(recorded.invoice.withheld.minor(), 15000);

    let bank = b.payment("2026-03-25", "850.00");
    let paid = b.settle(&uid, Some(&bank), "2026-03-25", "850.00", SettlementKind::Payment).unwrap();
    assert_eq!(paid.invoice.status, InvoiceStatus::PartiallySettled);

    // The withholding moves from the payable to the withholding liability.
    let payable = b.account("Liabilities:Accounts Payable");
    let withholding = b.account("Liabilities:Tax:Withholding");
    let mut move_input = EntryInput::new(b.entity, day("2026-03-25"));
    move_input.postings = vec![PostingInput::new(payable, dec("150.00")), PostingInput::balancing(withholding)];
    let moved = journal::create_entry(&mut b.conn(), move_input).unwrap();
    // More than the withheld amount is refused.
    assert_eq!(b.settle(&uid, Some(&moved), "2026-03-25", "150.01", SettlementKind::Withholding).unwrap_err().code, "over_settlement");
    let done = b.settle(&uid, Some(&moved), "2026-03-25", "150.00", SettlementKind::Withholding).unwrap();
    assert_eq!(done.invoice.status, InvoiceStatus::Settled);
    assert!(done.fx_difference.is_none());
}

#[test]
fn an_exchange_difference_creates_a_draft_entry() {
    let b = books();
    {
        let c = b.conn();
        entities::ensure_currency(&c, "USD").unwrap();
        rates::set_price(&c, "USD", "EUR", day("2026-03-01"), dec("0.90"), "test").unwrap();
    }
    let mut input = purchase("US-1", "1000.00", "0.00", "1000.00");
    input.commodity = "USD".into();
    input.issue_date = "2026-03-02".into();
    let recorded = invoices::record_invoice(&mut b.conn(), b.entity, &input).unwrap();
    let entry = recorded.recognition.unwrap();
    let payable = b.account("Liabilities:Accounts Payable");
    assert_eq!(entry.postings.iter().find(|p| p.account_id == payable).unwrap().amount.minor(), -90000);

    // The EUR bank pays the USD invoice at a later rate: 920.00 EUR for 1000.00 USD.
    let bank = b.payment("2026-03-30", "920.00");
    let input = SettlementInput {
        invoice_uid: recorded.invoice.uid.clone(),
        entry_uid: Some(bank.uid.clone()),
        posting_uid: None,
        date: day("2026-03-30"),
        amount: dec("1000.00"),
        kind: SettlementKind::Payment,
        fx_account: Some("Expenses:Exchange Differences".into()),
        control_account: None,
    };
    let settled = invoices::settle(&mut b.conn(), input).unwrap();
    assert_eq!(settled.invoice.status, InvoiceStatus::Settled);
    assert_eq!(settled.fx_difference.map(|m| m.minor()), Some(2000));
    let fx = settled.fx_entry.expect("fx entry");
    assert_eq!(fx.status, EntryStatus::Draft);
    assert_eq!(settled.settlement.fx_difference_entry_uid.as_deref(), Some(fx.uid.as_str()));
    let fx_account = b.account("Expenses:Exchange Differences");
    assert_eq!(fx.postings.iter().find(|p| p.account_id == fx_account).unwrap().amount.minor(), 2000);
    assert_eq!(fx.postings.iter().find(|p| p.account_id == payable).unwrap().amount.minor(), -2000);
    // After the recognition entry and the payment, the difference clears the payable.
    let total: i64 = [&entry, &bank, &fx].iter().flat_map(|e| e.postings.iter()).filter(|p| p.account_id == payable).map(|p| p.amount.minor()).sum();
    assert_eq!(total, 0);
}

#[test]
fn a_duplicate_bank_expense_is_found_and_converted_without_a_second_expense() {
    let b = books();
    let bank = b.bank_expense("2026-03-04", "1210.00");
    assert_eq!(b.balance(b.software), 121000);

    let mut recorded = invoices::record_invoice(&mut b.conn(), b.entity, &purchase("F-500", "1000.00", "210.00", "1210.00")).unwrap();
    // The recognition entry is a draft; post it as a human would.
    let recognition = recorded.recognition.take().unwrap();
    journal::post_entry(&mut b.conn(), recognition.id).unwrap();

    let gross = recorded.invoice.gross;
    let found = invoices::find_duplicate_expense(&b.conn(), b.entity, Some(b.supplier), gross, day("2026-03-02"), invoices::DUPLICATE_WINDOW_DAYS).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].entry_uid, bank.uid);
    assert!(invoices::find_duplicate_expense(&b.conn(), b.entity, Some(b.supplier), gross, day("2026-03-20"), invoices::DUPLICATE_WINDOW_DAYS).unwrap().is_empty());
    assert_eq!(invoices::invoice_duplicates(&b.conn(), recorded.invoice.id).unwrap().len(), 1);

    // Without the conversion the expense counts twice.
    assert_eq!(b.balance(b.software), 221000);
    let settled = b.settle(&recorded.invoice.uid, Some(&bank), "2026-03-04", "1210.00", SettlementKind::Payment).unwrap();
    assert!(settled.converted);
    assert_eq!(settled.invoice.status, InvoiceStatus::Settled);
    assert_eq!(b.balance(b.software), 100000);
    assert_eq!(b.balance(b.account("Liabilities:Accounts Payable")), 0);
    // The bank posting and its link stay; the settlement points to the replacement posting.
    let after = journal::get_entry(&b.conn(), bank.id).unwrap();
    assert_eq!(after.status, EntryStatus::Posted);
    let bank_posting = after.postings.iter().find(|p| p.account_id == b.bank).unwrap();
    assert_eq!(settled.settlement.posting_uid.as_deref(), Some(bank_posting.uid.as_str()));
    // A settled entry is no longer a duplicate candidate.
    assert!(invoices::find_duplicate_expense(&b.conn(), b.entity, Some(b.supplier), gross, day("2026-03-02"), 3).unwrap().is_empty());
}

#[test]
fn over_settlement_is_refused_unless_it_is_a_write_off() {
    let b = books();
    let recorded = invoices::record_invoice(&mut b.conn(), b.entity, &purchase("F-600", "100.00", "0.00", "100.00")).unwrap();
    let uid = recorded.invoice.uid.clone();
    let bank = b.payment("2026-03-10", "120.00");
    let refused = b.settle(&uid, Some(&bank), "2026-03-10", "120.00", SettlementKind::Payment).unwrap_err();
    assert_eq!(refused.code, "over_settlement");
    // A payment needs an entry.
    assert_eq!(b.settle(&uid, None, "2026-03-10", "10.00", SettlementKind::Payment).unwrap_err().code, "invalid_input");
    let written = b.settle(&uid, None, "2026-03-31", "100.00", SettlementKind::WriteOff).unwrap();
    assert_eq!(written.invoice.status, InvoiceStatus::Settled);
}

#[test]
fn cancel_is_refused_after_a_settlement() {
    let b = books();
    let settled_invoice = invoices::record_invoice(&mut b.conn(), b.entity, &purchase("F-700", "100.00", "0.00", "100.00")).unwrap();
    let bank = b.payment("2026-03-10", "40.00");
    b.settle(&settled_invoice.invoice.uid, Some(&bank), "2026-03-10", "40.00", SettlementKind::Payment).unwrap();
    let refused = invoices::cancel(&mut b.conn(), &settled_invoice.invoice.uid, "ana", "entered twice").unwrap_err();
    assert_eq!(refused.code, "settled");

    // An invoice without settlements cancels, and its draft recognition entry goes away.
    let open = invoices::record_invoice(&mut b.conn(), b.entity, &purchase("F-701", "100.00", "0.00", "100.00")).unwrap();
    let draft = open.recognition.unwrap();
    assert_eq!(invoices::cancel(&mut b.conn(), &open.invoice.uid, "ana", "  ").unwrap_err().code, "invalid_input");
    let cancelled = invoices::cancel(&mut b.conn(), &open.invoice.uid, "ana", "entered twice").unwrap();
    assert_eq!(cancelled.status, InvoiceStatus::Cancelled);
    assert!(cancelled.open.is_zero());
    assert!(journal::get_entry(&b.conn(), draft.id).is_err());
    let history = audit::history(&b.conn(), "invoices", cancelled.id).unwrap();
    let event = history.last().unwrap();
    assert_eq!((event.action.as_str(), event.actor.as_str(), event.reason.as_str()), ("cancel", "ana", "entered twice"));
    // A cancelled invoice takes no settlement.
    assert_eq!(b.settle(&open.invoice.uid, Some(&bank), "2026-03-10", "10.00", SettlementKind::Payment).unwrap_err().code, "invalid_input");
}

#[test]
fn a_sales_invoice_debits_the_receivable() {
    let b = books();
    let input = InvoiceInput {
        direction: "sales".into(),
        number: "S-1".into(),
        series: "2026".into(),
        issue_date: "2026-03-05".into(),
        commodity: "EUR".into(),
        net: "500.00".into(),
        gross: "500.00".into(),
        account: Some("Income:Consulting".into()),
        receivable_account: Some("Assets:Accounts Receivable".into()),
        recognize: true,
        ..Default::default()
    };
    let recorded = invoices::record_invoice(&mut b.conn(), b.entity, &input).unwrap();
    let entry = recorded.recognition.unwrap();
    let by_path = |path: &str| entry.postings.iter().find(|p| p.account_path == path).unwrap().amount.minor();
    assert_eq!(by_path("Assets:Accounts Receivable"), 50000);
    assert_eq!(by_path("Income:Consulting"), -50000);
    assert_eq!(recorded.invoice.label(), "2026-S-1");
    let listed = invoices::list_invoices(&b.conn(), b.entity, Some(invoices::Direction::Sales), true, None).unwrap();
    assert_eq!(listed.len(), 1);
}

#[test]
fn coded_invoice_lines_record_tax_events_on_the_recognition_entry() {
    let b = books();
    let input_vat = b.account("Assets:Tax:Input VAT");
    let hosting = accounts::ensure_account(&b.conn(), b.entity, AccountType::Expense, &["Hosting"], "expense", "EUR").unwrap().id;
    let code = means_core::tax::NewTaxCode {
        entity_id: b.entity,
        code: "IVAS21".into(),
        name: "Input VAT 21%".into(),
        kind: "input".into(),
        rate_bp: 2100,
        tax_account_id: Some(input_vat),
        jurisdiction: "ES".into(),
        effective_from: day("2026-01-01"),
        ..Default::default()
    };
    means_core::tax::add_tax_code(&mut b.conn(), code).unwrap();

    let line = |description: &str, net: &str, tax: &str, account: &str| InvoiceLineInput {
        description: description.into(),
        net: Some(net.into()),
        tax_code: "IVAS21".into(),
        tax: Some(tax.into()),
        account: Some(account.into()),
        ..Default::default()
    };
    let mut input = purchase("F-900", "1500.00", "315.00", "1815.00");
    input.lines = vec![line("Annual licence", "1000.00", "210.00", "Expenses:Software"), line("Hosting", "500.00", "105.00", "Expenses:Hosting")];
    input.tax_account = None;
    let recorded = invoices::record_invoice(&mut b.conn(), b.entity, &input).unwrap();
    let entry = recorded.recognition.expect("recognition entry");

    // Two tax events, one per line. Their sum is the tax of the invoice.
    let events = means_core::tax::tax_events_for_entry(&b.conn(), entry.id).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events.iter().map(|e| e.tax.minor()).sum::<i64>(), 31500);
    assert_eq!(events.iter().map(|e| e.base.minor()).sum::<i64>(), 150000);
    assert!(events.iter().all(|e| e.document_ref == "F-900" && e.party_id == Some(b.supplier)));

    // The entry balances. The tax lines are on the tax account and the control takes the gross.
    assert_eq!(entry.postings.iter().map(|p| p.amount.minor()).sum::<i64>(), 0);
    let sum_on = |account: i64| entry.postings.iter().filter(|p| p.account_id == account).map(|p| p.amount.minor()).sum::<i64>();
    assert_eq!(sum_on(input_vat), 31500);
    assert_eq!(sum_on(b.software), 100000);
    assert_eq!(sum_on(hosting), 50000);
    assert_eq!(sum_on(b.account("Liabilities:Accounts Payable")), -181500);

    // After the post, the tax summary of the period includes both events.
    journal::post_entry(&mut b.conn(), entry.id).unwrap();
    let summary = means_core::tax::tax_summary(&b.conn(), b.entity, day("2026-03-01"), day("2026-03-31")).unwrap();
    let row = summary.iter().find(|r| r.code == "IVAS21").unwrap();
    assert_eq!((row.count, row.tax.minor(), row.base.minor()), (2, 31500, 150000));
}

#[test]
fn an_unknown_tax_code_keeps_the_tax_account_fallback() {
    let b = books();
    let recorded = invoices::record_invoice(&mut b.conn(), b.entity, &purchase("F-901", "1000.00", "210.00", "1210.00")).unwrap();
    let entry = recorded.recognition.unwrap();
    assert!(means_core::tax::tax_events_for_entry(&b.conn(), entry.id).unwrap().is_empty());
    let tax = entry.postings.iter().find(|p| p.account_path == "Assets:Tax:Input VAT").unwrap();
    assert_eq!(tax.amount.minor(), 21000);
}
