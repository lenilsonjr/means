//! Real-world export layouts: one fixture per bank format under `tests/fixtures`, detected,
//! parsed, and imported end to end. `docs/import-formats.md` says what each fixture reproduces.

use chrono::NaiveDate;
use means_core::imports::{self, ParseOutput, ParsedLine};
use means_core::model::*;
use means_core::{accounts, entities, journal, rates, reports, Db, Error};
use rust_decimal::prelude::FromStr;
use rust_decimal::Decimal;

fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

fn live(out: &ParseOutput) -> Vec<&ParsedLine> {
    out.lines.iter().filter(|l| l.skip.is_none()).collect()
}

fn skipped(out: &ParseOutput) -> Vec<(usize, String)> {
    out.lines.iter().enumerate().filter_map(|(i, l)| l.skip.clone().map(|s| (i, s))).collect()
}

/// Each row's running balance is the previous one plus its amount.
fn walks_the_balance(out: &ParseOutput) {
    let mut prev: Option<Decimal> = None;
    for l in live(out) {
        if let (Some(p), Some(a), Some(b)) = (prev, l.amount, l.balance_after) {
            assert_eq!(p + a, b, "balance after {:?}", l.description);
        }
        if l.balance_after.is_some() {
            prev = l.balance_after;
        }
    }
}

struct Bank {
    db: Db,
    account: Account,
}

fn bank(entity_currency: &str, account_name: &str, account_currency: &str) -> Bank {
    let db = Db::open_memory().unwrap();
    let account = {
        let mut conn = db.conn();
        let entity = entities::create_entity(&mut conn, "Owner", "person", "PT", entity_currency).unwrap();
        accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank", account_name], "bank", account_currency).unwrap()
    };
    Bank { db, account }
}

fn import(bank: &Bank, filename: &str, content: &[u8]) -> imports::ImportOutcome {
    let mut conn = bank.db.conn();
    imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(bank.account.id), filename, content)).unwrap()
}

#[test]
fn n26_booking_date_format() {
    let content = fixture("n26_booking_date.csv");
    let source = imports::detect_source("n26-csv-transactions.csv", &content);
    assert_eq!(source, "n26_csv");
    let out = imports::parse(&source, &content, None).unwrap();
    assert_eq!(out.detected_source, "n26_csv");
    assert_eq!(out.currency, "EUR");
    assert_eq!(out.lines.len(), 8);
    assert!(skipped(&out).is_empty());
    let l = &out.lines[0];
    assert_eq!(l.date, Some(date("2026-03-02")));
    assert_eq!(l.amount, Some(d("-3.20")));
    assert_eq!(l.currency, "EUR");
    assert_eq!(l.description, "Cafe Central");
    assert_eq!(out.lines[2].amount, Some(d("2500")));
    assert_eq!(out.lines[2].description, "Acme GmbH Invoice 2026-017, March");
    assert_eq!(out.lines[3].amount, Some(d("-27.35")));
    assert_eq!(out.lines[3].original, Some((d("-29.99"), "USD".into())));
    assert_eq!(out.lines[5].description, "N26 Spaces Savings");
    assert!(out.lines.iter().all(|l| l.reference.is_empty() && l.balance_after.is_none()), "N26 has no id or balance column");
    assert_eq!(out.period_from, Some(date("2026-03-02")));
    assert_eq!(out.period_to, Some(date("2026-03-09")));
    assert_eq!(out.closing_balance, None);
    let b = bank("EUR", "N26", "EUR");
    let o = import(&b, "n26-csv-transactions.csv", &content);
    assert_eq!(o.import.source, "n26_csv");
    assert_eq!(o.import.lines_count, 8);
    assert_eq!(o.import.created_count, 8);
    assert_eq!(o.import.skipped_count, 0);
}

#[test]
fn n26_date_payee_format() {
    let content = fixture("n26_date_payee.csv");
    let source = imports::detect_source("n26-csv-transactions.csv", &content);
    assert_eq!(source, "n26_csv");
    let out = imports::parse(&source, &content, None).unwrap();
    assert_eq!(out.currency, "EUR");
    assert_eq!(out.lines.len(), 6);
    assert!(skipped(&out).is_empty());
    assert_eq!(out.lines[0].date, Some(date("2021-06-01")));
    assert_eq!(out.lines[0].amount, Some(d("-23.45")));
    assert_eq!(out.lines[0].description, "REWE SAGT DANKE");
    assert_eq!(out.lines[1].amount, Some(d("3200")));
    assert_eq!(out.lines[1].description, "Acme GmbH Salary 05/2021");
    assert_eq!(out.lines[4].original, Some((d("-17.30"), "USD".into())));
    assert_eq!(out.lines[4].raw["Category"], "Transport & Car");
    let b = bank("EUR", "N26", "EUR");
    let o = import(&b, "n26-csv-transactions.csv", &content);
    assert_eq!(o.import.created_count, 6);
    assert_eq!(o.import.skipped_count, 0);
}

#[test]
fn revolut_statement() {
    let content = fixture("revolut.csv");
    let source = imports::detect_source("account-statement_2026-02-01_2026-02-28_en_abc123.csv", &content);
    assert_eq!(source, "revolut_csv");
    let out = imports::parse(&source, &content, None).unwrap();
    assert_eq!(out.currency, "EUR");
    assert_eq!(out.lines.len(), 12, "10 rows plus two fee lines");
    assert_eq!(skipped(&out), vec![(5, "zero amount".to_string()), (6, "state pending".to_string()), (10, "state reverted".to_string())]);
    assert_eq!(out.lines[0].date, Some(date("2026-02-01")));
    assert_eq!(out.lines[0].amount, Some(d("500")));
    assert_eq!(out.lines[0].balance_after, Some(d("1250")));
    assert_eq!(out.lines[0].description, "Top-Up by *4321");
    assert_eq!(out.lines[1].date, Some(date("2026-02-03")), "the completed date, even with a one-digit hour");
    assert_eq!(out.lines[3].amount, Some(d("-100")));
    assert_eq!(out.lines[3].balance_after, Some(d("896.30")));
    assert_eq!(out.lines[4].amount, Some(d("-0.50")), "the fee is charged on top of the amount");
    assert_eq!(out.lines[4].description, "Fee: Exchanged to USD");
    assert_eq!(out.lines[4].date, out.lines[3].date);
    assert!(out.lines[4].balance_after.is_none());
    assert_eq!(out.lines[7].description, "Ristorante Da Mario, Roma");
    assert_eq!(out.lines[8].date, Some(date("2026-02-10")));
    assert_eq!(out.lines[9].amount, Some(d("-2")));
    assert_eq!(out.lines[11].amount, Some(d("-59.95")));
    assert!(out.lines.iter().all(|l| l.reference.is_empty()), "Revolut has no id column");
    assert_eq!(out.closing_balance, Some(d("695.64")));
    assert_eq!(out.closing_date, Some(date("2026-02-13")));
    let sum: Decimal = live(&out).iter().filter_map(|l| l.amount).sum();
    assert_eq!(d("1250") - d("500") + sum, d("695.64"), "the kept lines walk the running balance");
    let b = bank("EUR", "Revolut", "EUR");
    let o = import(&b, "account-statement.csv", &content);
    assert_eq!(o.import.source, "revolut_csv");
    assert_eq!(o.import.lines_count, 12);
    assert_eq!(o.import.created_count, 9);
    assert_eq!(o.import.skipped_count, 3);
    assert_eq!(o.import.closing_balance, Some(d("695.64")));
}

#[test]
fn wise_balance_statement() {
    let content = fixture("wise_statement.csv");
    let source = imports::detect_source("statement_12345678_EUR_2026-03-01_2026-03-31.csv", &content);
    assert_eq!(source, "wise_csv");
    let out = imports::parse(&source, &content, None).unwrap();
    assert_eq!(out.currency, "EUR");
    assert_eq!(out.lines.len(), 7);
    assert!(skipped(&out).is_empty());
    assert_eq!(out.lines[0].date, Some(date("2026-03-02")), "dates are DD-MM-YYYY");
    assert_eq!(out.lines[0].amount, Some(d("2500")));
    assert_eq!(out.lines[0].reference, "TRANSFER-1100000001");
    assert_eq!(out.lines[0].description, "Received money from Acme GmbH with reference INV-2026-017");
    assert_eq!(out.lines[0].balance_after, Some(d("3250.40")));
    assert_eq!(out.lines[1].amount, Some(d("-12.50")));
    assert_eq!(out.lines[1].original, Some((d("-35"), "MYR".into())));
    assert_eq!(out.lines[2].amount, Some(d("-0.30")), "Wise books card fees as their own rows; nothing is split");
    assert_eq!(out.lines[2].reference, "FEE-CARD-1100000002");
    assert_eq!(out.lines[3].description, "Sent money to John Smith Rent March");
    assert_eq!(out.lines[3].amount, Some(d("-1000")), "Amount already includes Total fees");
    assert_eq!(out.lines[4].original, Some((d("-540.06"), "USD".into())));
    assert_eq!(out.lines[5].description, "Card transaction of 23.90 EUR issued by Ristorante Da Mario, Roma");
    assert_eq!(out.lines[5].original, None);
    assert_eq!(out.closing_balance, Some(d("1693.71")));
    assert_eq!(out.closing_date, Some(date("2026-03-10")));
    walks_the_balance(&out);
    let b = bank("EUR", "Wise EUR", "EUR");
    let o = import(&b, "statement.csv", &content);
    assert_eq!(o.import.source, "wise_csv");
    assert_eq!(o.import.created_count, 7);
    assert_eq!(o.import.skipped_count, 0);
    assert_eq!(o.import.closing_balance, Some(d("1693.71")));
    // The EUR statement into a USD balance is the wrong file.
    let usd = bank("EUR", "Wise USD", "USD");
    let mut conn = usd.db.conn();
    let err = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(usd.account.id), "statement.csv", &content));
    assert!(matches!(err, Err(Error::Invalid(_))), "{:?}", err.as_ref().err());
}

#[test]
fn wise_transaction_history() {
    let content = fixture("wise_history.csv");
    let source = imports::detect_source("transaction-history.csv", &content);
    assert_eq!(source, "wise_history_csv");
    let out = imports::parse(&source, &content, None).unwrap();
    assert_eq!(out.lines.len(), 10, "7 rows, two fee lines and the other side of a conversion");
    assert_eq!(skipped(&out), vec![(6, "status cancelled".to_string()), (9, "status pending".to_string())]);
    let l = &out.lines[0];
    assert_eq!(l.date, Some(date("2026-03-02")));
    assert_eq!(l.amount, Some(d("2500")));
    assert_eq!(l.currency, "EUR");
    assert_eq!(l.description, "Acme GmbH INV-2026-017");
    assert_eq!(l.reference, "TRANSFER-1100000001");
    assert_eq!((out.lines[1].amount, out.lines[1].currency.as_str(), out.lines[1].description.as_str()), (Some(d("-995.90")), "EUR", "John Smith Rent March"));
    assert_eq!((out.lines[2].amount, out.lines[2].currency.as_str(), out.lines[2].description.as_str()), (Some(d("-4.10")), "EUR", "Fee: John Smith Rent March"));
    assert_eq!((out.lines[3].amount, out.lines[3].currency.as_str()), (Some(d("-497.95")), "EUR"));
    assert_eq!(out.lines[3].description, "Converted 497.95 EUR to 540.06 USD");
    assert_eq!(out.lines[3].original, Some((d("-540.06"), "USD".into())));
    assert_eq!((out.lines[4].amount, out.lines[4].currency.as_str()), (Some(d("-2.05")), "EUR"));
    assert_eq!((out.lines[5].amount, out.lines[5].currency.as_str(), out.lines[5].reference.as_str()), (Some(d("540.06")), "USD", "BALANCE-1100000004"));
    assert_eq!(out.lines[5].original, Some((d("497.95"), "EUR".into())));
    assert_eq!((out.lines[7].amount, out.lines[7].description.as_str()), (Some(d("-5")), "Wise 1100000020"));
    assert_eq!((out.lines[8].amount, out.lines[8].currency.as_str(), out.lines[8].description.as_str()), (Some(d("1200")), "USD", "Client LLC Retainer March"));
    assert!(out.lines.iter().all(|l| l.balance_after.is_none()));
    // Choosing "Wise (CSV balance statement)" for a history file still reads it as history.
    let as_statement = imports::parse("wise_csv", &content, None).unwrap();
    assert_eq!(as_statement.lines.len(), 10);
    assert_eq!(as_statement.lines[1].amount, Some(d("-995.90")));
    // Each balance keeps its own lines.
    let eur = bank("EUR", "Wise EUR", "EUR");
    let o = import(&eur, "transaction-history.csv", &content);
    assert_eq!(o.import.source, "wise_history_csv");
    assert_eq!(o.import.lines_count, 10);
    assert_eq!(o.import.created_count, 6);
    assert_eq!(o.import.skipped_count, 4);
    assert!(o.lines.iter().any(|l| l.status == "skipped" && l.note == "USD line in a EUR account"), "{:?}", o.lines.iter().map(|l| (&l.status, &l.note)).collect::<Vec<_>>());
    let usd = bank("EUR", "Wise USD", "USD");
    let o = import(&usd, "transaction-history.csv", &content);
    assert_eq!(o.import.created_count, 2);
    assert_eq!(o.import.skipped_count, 8);
}

#[test]
fn mercury_transactions() {
    let content = fixture("mercury.csv");
    let source = imports::detect_source("transactions-2026-03.csv", &content);
    assert_eq!(source, "mercury_csv");
    let out = imports::parse(&source, &content, None).unwrap();
    assert_eq!(out.currency, "USD");
    assert_eq!(out.lines.len(), 9);
    assert_eq!(skipped(&out), vec![(3, "status pending".to_string()), (4, "status failed".to_string()), (7, "status cancelled".to_string())]);
    assert_eq!(out.lines[0].date, Some(date("2026-03-02")), "dates are MM-DD-YYYY");
    assert_eq!(out.lines[0].amount, Some(d("12500")));
    assert_eq!(out.lines[0].currency, "USD");
    assert_eq!(out.lines[0].description, "Client LLC CLIENT LLC ACH PAYMENT");
    assert_eq!(out.lines[2].description, "Jane Doe Send Money transaction initiated on Mercury");
    assert_eq!(out.lines[2].amount, Some(d("-3000")));
    assert_eq!(out.lines[2].raw["Reference"], "Payroll, March");
    assert_eq!(out.lines[8].date, Some(date("2026-03-10")));
    assert!(out.lines.iter().all(|l| l.reference.is_empty() && l.balance_after.is_none()), "no id or balance column");
    let b = bank("USD", "Mercury", "USD");
    let o = import(&b, "transactions-2026-03.csv", &content);
    assert_eq!(o.import.source, "mercury_csv");
    assert_eq!(o.import.created_count, 6);
    assert_eq!(o.import.skipped_count, 3);
    // Exports before November 2022 called the date column "Date".
    let legacy = String::from_utf8(content.clone()).unwrap().replacen("Date (UTC)", "Date", 1);
    assert_eq!(imports::detect_source("transactions.csv", legacy.as_bytes()), "mercury_csv");
    let out = imports::parse("mercury_csv", legacy.as_bytes(), None).unwrap();
    assert_eq!(out.lines[0].date, Some(date("2026-03-02")));
    assert_eq!(live(&out).len(), 6);
    // A month/day file written with slashes keeps its field order.
    let slashed = String::from_utf8(content.clone()).unwrap().replace("03-02-2026", "03/02/2026");
    let out = imports::parse("mercury_csv", slashed.as_bytes(), None).unwrap();
    assert_eq!(out.lines[0].date, Some(date("2026-03-02")));
}

#[test]
fn inter_extrato_csv() {
    let content = fixture("inter_extrato.csv");
    let source = imports::detect_source("Extrato-01-03-2026-a-31-03-2026.csv", &content);
    assert_eq!(source, "inter_csv");
    let out = imports::parse(&source, &content, None).unwrap();
    assert_eq!(out.currency, "BRL");
    assert_eq!(out.headers, vec!["Data Lançamento", "Descrição", "Valor", "Saldo"]);
    assert_eq!(out.lines.len(), 7, "the preamble is not data");
    assert!(skipped(&out).is_empty());
    assert_eq!(out.lines[0].date, Some(date("2026-03-02")));
    assert_eq!(out.lines[0].amount, Some(d("1200")));
    assert_eq!(out.lines[0].description, r#"Pix recebido: "Cp :12345678-ACME COMERCIO LTDA""#);
    assert_eq!(out.lines[0].balance_after, Some(d("3581.55")));
    assert_eq!(out.lines[1].amount, Some(d("-318.19")));
    assert_eq!(out.lines[6].amount, Some(d("-411.06")));
    assert_eq!(out.lines[6].description, r#"Pagamento de boleto efetuado: "BANCO XP S/A""#);
    assert_eq!(out.closing_balance, Some(d("2415.86")));
    assert_eq!(out.closing_date, Some(date("2026-03-20")));
    walks_the_balance(&out);
    let b = bank("BRL", "Inter", "BRL");
    let o = import(&b, "extrato.csv", &content);
    assert_eq!(o.import.source, "inter_csv");
    assert_eq!(o.import.created_count, 7);
    assert_eq!(o.import.skipped_count, 0);
    assert_eq!(o.import.closing_balance, Some(d("2415.86")));
}

#[test]
fn inter_extrato_legacy_latin1() {
    let content = fixture("inter_extrato_legacy.csv");
    assert!(std::str::from_utf8(&content).is_err(), "the fixture is ISO-8859-1");
    let source = imports::detect_source("extrato.csv", &content);
    assert_eq!(source, "inter_csv");
    let out = imports::parse(&source, &content, None).unwrap();
    assert_eq!(out.headers[0], "DATA LANÇAMENTO");
    assert_eq!(out.lines.len(), 4);
    assert_eq!(skipped(&out), vec![(0, "balance row".to_string())]);
    assert_eq!(out.lines[1].date, Some(date("2020-10-05")));
    assert_eq!(out.lines[1].amount, Some(d("500")));
    assert_eq!(out.lines[1].description, "PIX RECEBIDO - FULANO DE TAL");
    assert_eq!(out.lines[2].amount, Some(d("-34.04")), "R$ prefixes and a leading minus");
    assert_eq!(out.lines[2].balance_after, Some(d("1465.96")));
    assert_eq!(out.lines[3].description, "PAGAMENTO DE TITULO - ENERGIA ELÉTRICA");
    assert_eq!(out.lines[3].amount, Some(d("-231.40")));
    assert_eq!(out.closing_balance, Some(d("1234.56")));
    walks_the_balance(&out);
    let b = bank("BRL", "Inter", "BRL");
    let o = import(&b, "extrato.csv", &content);
    assert_eq!(o.import.created_count, 3);
    assert_eq!(o.import.skipped_count, 1);
}

#[test]
fn inter_ofx() {
    let content = fixture("inter.ofx");
    assert!(std::str::from_utf8(&content).is_err(), "the fixture is cp1252, as the header says");
    let source = imports::detect_source("extrato.ofx", &content);
    assert_eq!(source, "inter_ofx");
    assert_eq!(imports::detect_source("download.txt", &content), "inter_ofx", "recognised by content too");
    let out = imports::parse(&source, &content, None).unwrap();
    assert_eq!(out.currency, "BRL");
    assert_eq!(out.account_ref, "12345678");
    assert_eq!(out.period_from, Some(date("2026-03-01")));
    assert_eq!(out.period_to, Some(date("2026-03-31")));
    assert_eq!(out.closing_balance, Some(d("2415.86")));
    assert_eq!(out.closing_date, Some(date("2026-03-31")));
    assert_eq!(out.lines.len(), 7);
    assert!(skipped(&out).is_empty());
    assert_eq!(out.lines[0].date, Some(date("2026-03-02")));
    assert_eq!(out.lines[0].amount, Some(d("1200")));
    assert_eq!(out.lines[0].reference, "20260302000001");
    assert_eq!(out.lines[0].description, r#"Pix recebido: "Cp :12345678-ACME COMERCIO LTDA""#, "the NAME is the start of the MEMO");
    assert_eq!(out.lines[0].raw["TRNTYPE"], "CREDIT");
    assert_eq!(out.lines[0].raw["REFNUM"], "E20260302000001");
    assert_eq!(out.lines[1].raw["TRNTYPE"], "PAYMENT");
    assert_eq!(out.lines[1].amount, Some(d("-318.19")));
    assert_eq!(out.lines[4].description, r#"Aplicação: "CDB Porq Obj BANCO INTER SA""#, "cp1252 decoded");
    let b = bank("BRL", "Inter", "BRL");
    let o = import(&b, "extrato.ofx", &content);
    assert_eq!(o.import.source, "inter_ofx");
    assert_eq!(o.import.created_count, 7);
    assert_eq!(o.import.closing_balance, Some(d("2415.86")));
    let rec = reports::reconciliation(&b.db.conn(), b.account.id).unwrap();
    assert_eq!(rec.statement_balance, Some(d("2415.86")));
}

#[test]
fn remessa_online_extrato() {
    let content = fixture("remessa.csv");
    let source = imports::detect_source("extrato-remessa.csv", &content);
    assert_eq!(source, "remessa_csv");
    let out = imports::parse(&source, &content, None).unwrap();
    assert_eq!(out.currency, "BRL");
    assert_eq!(out.lines.len(), 5);
    assert!(skipped(&out).is_empty(), "{:?}", skipped(&out));
    let l = &out.lines[0];
    assert_eq!(l.date, Some(date("2026-03-05")));
    assert_eq!(l.amount, Some(d("-5456.90")), "VET × foreign amount: the BRL column is the total charged");
    assert_eq!(l.currency, "BRL");
    assert_eq!(l.description, "Remessa Online → 1000 USD Jane Doe (Manutenção de residente)");
    assert!(l.reference.is_empty(), "no contract column in this export");
    let ex = &l.raw["_exchange"];
    assert_eq!(ex["outbound"], true);
    assert_eq!(ex["foreign_currency"], "USD");
    assert_eq!(d(ex["foreign_quantity"].as_str().unwrap()), d("1000"));
    assert_eq!(d(ex["iof"].as_str().unwrap()), d("20.54"));
    assert_eq!(d(ex["rate"].as_str().unwrap()), d("5.4569"));
    assert_eq!(ex["spread"], "1,30%");
    assert_eq!(ex["counterparty"], "Jane Doe");
    assert_eq!(out.lines[1].amount, Some(d("13412.50")));
    assert_eq!(out.lines[1].raw["_exchange"]["outbound"], false);
    assert_eq!(out.lines[2].raw["_exchange"]["foreign_currency"], "EUR");
    assert_eq!(out.lines[4].amount, Some(d("-1641")));
    // Round trip: the USD rows become draft exchange entries; the EUR rows wait for their own account.
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let entity = entities::create_entity(&mut conn, "Owner", "person", "BR", "BRL").unwrap();
    let inter = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank", "Inter"], "bank", "BRL").unwrap();
    let wise = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank", "Wise USD"], "bank", "USD").unwrap();
    rates::set_price(&conn, "USD", "BRL", date("2026-03-01"), d("5.40"), "manual").unwrap();
    let preview = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(inter.id), "extrato-remessa.csv", &content).preview(true)).unwrap();
    assert_eq!(preview.import.status, "preview");
    assert_eq!(preview.lines.len(), 5);
    let o = imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(inter.id), "extrato-remessa.csv", &content).options(serde_json::json!({"to_account_id": wise.id}))).unwrap();
    assert_eq!(o.import.source, "remessa_csv");
    assert_eq!(o.import.created_count, 3);
    assert_eq!(o.import.skipped_count, 2);
    assert!(o.lines.iter().any(|l| l.status == "skipped" && l.note == "EUR exchange; choose the EUR account"));
    let first = o.lines.iter().find(|l| l.status == "created").unwrap();
    let e = journal::get_entry(&conn, first.journal_entry_id.unwrap()).unwrap();
    assert_eq!(e.status, EntryStatus::Draft);
    assert_eq!(e.payee, "Remessa Online");
    let bank_leg = e.postings.iter().find(|p| p.account_id == inter.id).unwrap();
    assert_eq!(bank_leg.quantity.major(), d("-5456.90"));
    let fx_leg = e.postings.iter().find(|p| p.account_id == wise.id).unwrap();
    assert_eq!(fx_leg.quantity.major(), d("1000"));
    assert!(
        e.postings.iter().any(|p| p.account_path == "Expenses:Taxes:IOF" && p.quantity.major() == d("20.54")),
        "{:?}",
        e.postings.iter().map(|p| (&p.account_path, p.quantity)).collect::<Vec<_>>()
    );
    let tb = reports::trial_balance(&conn, entity.id, None).unwrap();
    assert_eq!(tb.total_debit, tb.total_credit);
}

#[test]
fn remessa_online_variant_columns() {
    // Comma-separated, dot decimals, a currency column, a contract number, and a BRL column
    // holding the principal rather than the total.
    let csv = "Data,Direção,Moeda,Valor em moeda estrangeira,Valor em reais,IOF,Tarifa,VET,Contrato\n2026-03-05,Envio,USD,\"1,000.00\",\"5,436.36\",20.54,0.00,5.4569,CT-2026-0001\n2026-03-12,Recebimento,EUR,500.00,\"3,050.00\",0.00,0.00,6.1000,CT-2026-0002\n";
    assert_eq!(imports::detect_source("remessa.csv", csv.as_bytes()), "remessa_csv");
    let out = imports::parse("remessa_csv", csv.as_bytes(), None).unwrap();
    assert_eq!(out.lines.len(), 2);
    assert!(skipped(&out).is_empty(), "{:?}", skipped(&out));
    assert_eq!(out.lines[0].amount, Some(d("-5456.90")), "the BRL column was the principal: IOF is added to reach VET × amount");
    assert_eq!(out.lines[0].reference, "CT-2026-0001");
    assert_eq!(out.lines[0].raw["_exchange"]["foreign_currency"], "USD");
    assert_eq!(out.lines[1].amount, Some(d("3050")));
    assert_eq!(out.lines[1].raw["_exchange"]["foreign_currency"], "EUR");
    // No currency anywhere: the row waits for a mapping currency.
    let csv2 = "Data;Tipo;Valor enviado;Valor total;IOF\n05/03/2026;Envio;1.000,00;5.456,90;20,54\n";
    let out = imports::parse("remessa_csv", csv2.as_bytes(), None).unwrap();
    assert_eq!(out.lines[0].skip.as_deref(), Some("missing currency (add a Moeda column or set the currency in the mapping)"));
    let m = imports::CsvMapping { currency: "usd".into(), ..Default::default() };
    let out = imports::parse("remessa_csv", csv2.as_bytes(), Some(&m)).unwrap();
    assert!(out.lines[0].skip.is_none(), "{:?}", out.lines[0].skip);
    assert_eq!(out.lines[0].raw["_exchange"]["foreign_currency"], "USD");
    assert_eq!(out.lines[0].amount, Some(d("-5456.90")), "without a VET the BRL column is the total");
}

#[test]
fn remessa_failed_link_leaves_no_orphan_and_retry_preserves_exchange() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let entity = entities::create_entity(&mut conn, "Owner", "person", "BR", "BRL").unwrap();
    let from = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["BRL"], "bank", "BRL").unwrap();
    let to = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["USD"], "bank", "USD").unwrap();
    conn.execute_batch("CREATE TEMP TRIGGER fail_exchange BEFORE UPDATE OF journal_entry_id ON statement_lines BEGIN SELECT RAISE(ABORT, 'injected failure'); END;").unwrap();
    let out =
        imports::run_import(&mut conn, imports::ImportRequest::new("remessa_csv", Some(from.id), "remessa.csv", &fixture("remessa.csv")).options(serde_json::json!({"to_account_id": to.id}))).unwrap();
    assert_eq!(out.import.error_count, 3);
    assert_eq!(journal::count_by_status(&conn, "draft").unwrap(), 0);
    conn.execute_batch("DROP TRIGGER fail_exchange;").unwrap();
    let done = imports::retry_import(&mut conn, out.import.id).unwrap();
    assert_eq!((done.created_count, done.error_count, done.skipped_count), (3, 0, 2));
    assert_eq!(imports::retry_import(&mut conn, out.import.id).unwrap().created_count, 3);
    let (_, lines) = imports::get_import(&conn, out.import.id).unwrap();
    for line in lines.iter().filter(|l| l.status == "created") {
        let entry = journal::get_entry(&conn, line.journal_entry_id.unwrap()).unwrap();
        assert!(entry.postings.iter().any(|p| p.account_id == to.id));
        assert_eq!(entry.postings.iter().find(|p| p.account_id == from.id).unwrap().quantity, line.amount.unwrap());
    }
}
