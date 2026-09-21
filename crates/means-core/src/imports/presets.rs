//! Known bank export formats, expressed as column mappings over the generic CSV engine.
//!
//! Each preset recognises a header signature and fills a `CsvMapping`; a few add
//! per-row behaviour (Revolut fees and states, Mercury statuses, N26 original amounts,
//! Wise directions and conversions, Inter balance rows). `docs/import-formats.md` records
//! which headers were verified against real exports and which are assumed.

use rust_decimal::Decimal;

use super::csvkit::{self, exact_col, find_col, Table};
use super::{CsvMapping, ParsedLine};
use crate::money;

pub struct Preset {
    pub source: &'static str,
    pub label: &'static str,
}

pub const PRESETS: &[Preset] = &[
    Preset { source: "n26_csv", label: "N26 (CSV)" },
    Preset { source: "revolut_csv", label: "Revolut (CSV statement)" },
    Preset { source: "wise_csv", label: "Wise (CSV balance statement)" },
    Preset { source: "wise_history_csv", label: "Wise (CSV transaction history)" },
    Preset { source: "mercury_csv", label: "Mercury (CSV)" },
    Preset { source: "inter_csv", label: "Banco Inter (CSV extrato)" },
    Preset { source: "inter_ofx", label: "Banco Inter (OFX)" },
    Preset { source: "remessa_csv", label: "Remessa Online (CSV)" },
    Preset { source: "generic_csv", label: "Any CSV (map the columns)" },
    Preset { source: "account_tracker", label: "Account Tracker backup (.atb)" },
];

/// Every name present as a column, matched exactly (case-insensitive).
fn exact(headers: &[String], names: &[&str]) -> bool {
    names.iter().all(|n| exact_col(headers, n).is_some())
}

/// Every name present as a column: exact first, then as a substring.
fn has(headers: &[String], names: &[&str]) -> bool {
    names.iter().all(|n| find_col(headers, &[n]).is_some())
}

/// Recognise a bank format from the header row.
pub fn detect(headers: &[String]) -> Option<&'static str> {
    if exact(headers, &["Booking Date", "Amount (EUR)"]) || exact(headers, &["Date", "Payee", "Amount (EUR)"]) {
        return Some("n26_csv");
    }
    if exact(headers, &["Started Date", "Completed Date", "Amount", "State"]) {
        return Some("revolut_csv");
    }
    if exact(headers, &["Direction", "Source amount (after fees)", "Target amount (after fees)"]) {
        return Some("wise_history_csv");
    }
    if exact(headers, &["TransferWise ID", "Date", "Amount"]) || exact(headers, &["Date", "Amount", "Currency", "Running Balance"]) {
        return Some("wise_csv");
    }
    if exact(headers, &["Bank Description", "Amount"]) || exact(headers, &["Date (UTC)", "Description", "Amount"]) {
        return Some("mercury_csv");
    }
    if exact(headers, &["IOF"]) && (has(headers, &["Data"]) || has(headers, &["Date"])) {
        return Some("remessa_csv");
    }
    if (has(headers, &["Data Lançamento"]) || has(headers, &["Data Lancamento"]) || exact(headers, &["Data", "Histórico"])) && has(headers, &["Valor"]) {
        return Some("inter_csv");
    }
    None
}

/// The concrete layout behind a source: N26 and Wise changed their columns over time, and a
/// user who picked "Wise" may upload either of its two exports.
fn variant(source: &str, headers: &[String]) -> &'static str {
    match source {
        "n26_csv" => {
            if exact_col(headers, "Booking Date").is_some() {
                "n26_new"
            } else {
                "n26_old"
            }
        }
        "revolut_csv" => "revolut",
        "wise_csv" | "wise_history_csv" => {
            if exact_col(headers, "Source amount (after fees)").is_some() {
                "wise_history"
            } else {
                "wise_statement"
            }
        }
        "mercury_csv" => "mercury",
        "inter_csv" => "inter",
        _ => "",
    }
}

/// The column mapping for a known source, given its headers.
pub fn mapping_for(source: &str, headers: &[String]) -> Option<CsvMapping> {
    let h = |names: &[&str]| find_col(headers, names).map(|i| headers[i].clone()).unwrap_or_default();
    let e = |names: &[&str]| names.iter().find_map(|n| exact_col(headers, n)).map(|i| headers[i].clone()).unwrap_or_default();
    let extras = |names: &[&str], not: &str| -> Vec<String> { names.iter().filter_map(|n| exact_col(headers, n)).map(|i| headers[i].clone()).filter(|c| c != not).collect() };
    let m = match variant(source, headers) {
        "n26_new" => CsvMapping {
            date_column: e(&["Booking Date"]),
            date_format: "%Y-%m-%d".into(),
            amount_column: e(&["Amount (EUR)"]),
            description_column: e(&["Partner Name"]),
            extra_description_columns: extras(&["Payment Reference"], ""),
            currency: "EUR".into(),
            decimal_separator: ".".into(),
            ..Default::default()
        },
        "n26_old" => CsvMapping {
            date_column: e(&["Date"]),
            date_format: "%Y-%m-%d".into(),
            amount_column: e(&["Amount (EUR)"]),
            description_column: e(&["Payee"]),
            extra_description_columns: extras(&["Payment reference"], ""),
            currency: "EUR".into(),
            decimal_separator: ".".into(),
            ..Default::default()
        },
        "revolut" => CsvMapping {
            date_column: e(&["Completed Date"]),
            date_format: "%Y-%m-%d %H:%M:%S".into(),
            amount_column: e(&["Amount"]),
            description_column: e(&["Description"]),
            balance_column: e(&["Balance"]),
            currency_column: e(&["Currency"]),
            decimal_separator: ".".into(),
            ..Default::default()
        },
        "wise_statement" => CsvMapping {
            date_column: e(&["Date"]),
            date_format: "%d-%m-%Y".into(),
            amount_column: e(&["Amount"]),
            description_column: e(&["Description"]),
            extra_description_columns: extras(&["Payment Reference"], ""),
            reference_column: e(&["TransferWise ID"]),
            balance_column: e(&["Running Balance"]),
            currency_column: e(&["Currency"]),
            decimal_separator: ".".into(),
            ..Default::default()
        },
        // The amount, currency and description depend on Direction; `post_process` sets them.
        "wise_history" => CsvMapping {
            date_column: e(&["Finished on"]),
            date_format: "%Y-%m-%d %H:%M:%S".into(),
            amount_column: e(&["Source amount (after fees)"]),
            description_column: e(&["Target name"]),
            extra_description_columns: extras(&["Reference"], ""),
            reference_column: e(&["ID"]),
            currency_column: e(&["Source currency"]),
            decimal_separator: ".".into(),
            ..Default::default()
        },
        "mercury" => CsvMapping {
            date_column: e(&["Date (UTC)", "Date"]),
            date_format: "%m-%d-%Y".into(),
            amount_column: e(&["Amount"]),
            description_column: e(&["Description"]),
            extra_description_columns: extras(&["Bank Description"], ""),
            reference_column: e(&["Transaction ID", "ID"]),
            currency: "USD".into(),
            decimal_separator: ".".into(),
            ..Default::default()
        },
        "inter" => {
            let desc = h(&["Histórico", "Historico", "Descrição", "Descricao"]);
            CsvMapping {
                delimiter: ";".into(),
                date_column: h(&["Data Lançamento", "Data Lancamento", "Data"]),
                date_format: "%d/%m/%Y".into(),
                amount_column: h(&["Valor"]),
                description_column: desc.clone(),
                extra_description_columns: extras(&["Descrição", "Descricao"], &desc),
                balance_column: h(&["Saldo"]),
                currency: "BRL".into(),
                decimal_separator: ",".into(),
                ..Default::default()
            }
        }
        _ => return None,
    };
    Some(m)
}

fn cell(table: &Table, row: &[String], names: &[&str]) -> String {
    names.iter().find_map(|n| exact_col(&table.headers, n)).and_then(|i| row.get(i)).map(|s| s.trim().to_string()).unwrap_or_default()
}

fn num(s: &str) -> Option<Decimal> {
    if s.trim().is_empty() {
        None
    } else {
        money::parse_localized(s, '.').ok()
    }
}

/// A foreign amount carries the sign of the booked amount (banks differ on whether they sign it).
fn signed_like(value: Decimal, like: Decimal) -> Decimal {
    if like.is_sign_negative() {
        -value.abs()
    } else {
        value.abs()
    }
}

/// A separate line for a fee the bank charged on top of the amount.
fn fee_line(of: &ParsedLine, fee: Decimal, currency: &str) -> ParsedLine {
    ParsedLine {
        date: of.date,
        amount: Some(-fee.abs()),
        currency: currency.to_string(),
        description: format!("Fee: {}", of.description),
        reference: String::new(),
        balance_after: None,
        raw: serde_json::json!({"fee_of": of.raw.clone()}),
        skip: of.skip.clone(),
        original: None,
    }
}

/// Per-row adjustments for a source. May turn one row into extra lines (fees, the other side
/// of a conversion) or skip rows (pending, reverted, failed, balance rows).
pub fn post_process(source: &str, table: &Table, row: &[String], line: &mut ParsedLine, extra: &mut Vec<ParsedLine>) {
    let get = |names: &[&str]| cell(table, row, names);
    match variant(source, &table.headers) {
        "n26_new" | "n26_old" => {
            let oc = get(&["Original Currency", "Type Foreign Currency"]).to_ascii_uppercase();
            let oa = get(&["Original Amount", "Amount (Foreign Currency)"]);
            if !oc.is_empty() && oc != "EUR" {
                if let (Some(a), Some(amount)) = (num(&oa), line.amount) {
                    line.original = Some((signed_like(a, amount), oc));
                }
            }
        }
        "revolut" => {
            // Only COMPLETED rows moved money; PENDING rows have no completed date or balance,
            // REVERTED, DECLINED and FAILED never settled.
            let state = get(&["State"]).to_ascii_uppercase();
            if !state.is_empty() && state != "COMPLETED" {
                line.skip = Some(format!("state {}", state.to_lowercase()));
                return;
            }
            let fee = num(&get(&["Fee"])).unwrap_or(Decimal::ZERO);
            if fee.is_zero() && line.amount.is_some_and(|a| a.is_zero()) {
                line.skip = Some("zero amount".into()); // card authorisation holds
                return;
            }
            // The fee is charged on top of Amount: Balance = previous + Amount - Fee.
            if !fee.is_zero() {
                extra.push(fee_line(line, fee, &line.currency));
            }
        }
        "wise_statement" => {
            // Amount is the whole movement (Total fees is a breakdown of it, and Wise books card
            // fees as their own FEE-CARD rows), so nothing is split. A card payment or conversion
            // in another currency keeps that side as the original amount.
            let ex_to = get(&["Exchange To"]).to_ascii_uppercase();
            if !ex_to.is_empty() && ex_to != line.currency {
                if let (Some(a), Some(amount)) = (num(&get(&["Exchange To Amount"])), line.amount) {
                    line.original = Some((signed_like(a, amount), ex_to));
                }
            }
        }
        "wise_history" => wise_history(table, row, line, extra),
        "mercury" => {
            // Mercury statuses: pending, sent, cancelled, failed, reversed, blocked (title case in the CSV).
            let status = get(&["Status"]).to_ascii_lowercase();
            if !status.is_empty() && !matches!(status.as_str(), "sent" | "completed" | "posted" | "settled") {
                line.skip = Some(format!("status {status}"));
                return;
            }
            if line.amount.is_some_and(|a| a.is_zero()) {
                line.skip = Some("zero amount".into());
                return;
            }
            let oc = get(&["Original Currency"]).to_ascii_uppercase();
            if !oc.is_empty() && oc != "USD" {
                if let (Some(a), Some(amount)) = (num(&get(&["Original Amount"])), line.amount) {
                    line.original = Some((signed_like(a, amount), oc));
                }
            }
        }
        "inter" => {
            let d = line.description.to_uppercase();
            if d.starts_with("SALDO DO DIA") || d.starts_with("SALDO ANTERIOR") || d.starts_with("SALDO FINAL") {
                line.skip = Some("balance row".into());
            }
        }
        _ => {}
    }
}

/// Wise's transaction history is one multi-currency file. Direction says which side moved on a
/// balance: the target side for IN, the source side for OUT, both for NEUTRAL (a conversion
/// between two of the user's balances). Fees are charged on top of the "(after fees)" amounts.
fn wise_history(table: &Table, row: &[String], line: &mut ParsedLine, extra: &mut Vec<ParsedLine>) {
    let get = |names: &[&str]| cell(table, row, names);
    let status = get(&["Status"]).to_ascii_uppercase();
    if status != "COMPLETED" {
        line.skip = Some(format!("status {}", if status.is_empty() { "blank".to_string() } else { status.to_lowercase() }));
        return;
    }
    if line.date.is_none() {
        if let Ok(d) = csvkit::parse_date(&get(&["Created on"]), "%Y-%m-%d %H:%M:%S") {
            line.date = Some(d);
        }
    }
    let src_amt = num(&get(&["Source amount (after fees)"])).map(|a| a.abs());
    let src_cur = get(&["Source currency"]).to_ascii_uppercase();
    let src_fee = num(&get(&["Source fee amount"])).unwrap_or(Decimal::ZERO).abs();
    let src_fee_cur = match get(&["Source fee currency"]).to_ascii_uppercase() {
        c if c.is_empty() => src_cur.clone(),
        c => c,
    };
    let tgt_amt = num(&get(&["Target amount (after fees)"])).map(|a| a.abs());
    let tgt_cur = get(&["Target currency"]).to_ascii_uppercase();
    let src_name = get(&["Source name"]);
    let tgt_name = get(&["Target name"]);
    let reference = get(&["Reference"]);
    let with_ref = |who: &str| {
        let who = if who.is_empty() { "Wise".to_string() } else { who.to_string() };
        if reference.is_empty() || who.contains(&reference) {
            who
        } else {
            format!("{who} {reference}")
        }
    };
    let direction = get(&["Direction"]).to_ascii_uppercase();
    let mut other_side: Option<ParsedLine> = None;
    let mut fee: Option<ParsedLine> = None;
    match direction.as_str() {
        "IN" => {
            line.amount = tgt_amt;
            line.currency = if tgt_cur.is_empty() { src_cur.clone() } else { tgt_cur.clone() };
            line.description = with_ref(&src_name);
        }
        "NEUTRAL" => {
            line.amount = src_amt.map(|a| -a);
            line.currency = src_cur.clone();
            line.description = format!("Converted {} {} to {} {}", src_amt.map(money::plain).unwrap_or_default(), src_cur, tgt_amt.map(money::plain).unwrap_or_default(), tgt_cur);
            if let Some(t) = tgt_amt {
                if !tgt_cur.is_empty() && tgt_cur != src_cur {
                    line.original = Some((-t, tgt_cur.clone()));
                    let mut in_line = line.clone();
                    in_line.amount = Some(t);
                    in_line.currency = tgt_cur.clone();
                    in_line.original = src_amt.map(|a| (a, src_cur.clone()));
                    other_side = Some(in_line);
                }
            }
            if !src_fee.is_zero() {
                fee = Some(fee_line(line, src_fee, &src_fee_cur));
            }
        }
        _ => {
            // OUT, and anything unexpected: the source side left the balance.
            line.amount = src_amt.map(|a| -a);
            line.currency = src_cur.clone();
            line.description = with_ref(&tgt_name);
            if !src_fee.is_zero() {
                fee = Some(fee_line(line, src_fee, &src_fee_cur));
            }
        }
    }
    line.skip = if line.amount.is_none() {
        Some("no amount".into())
    } else if line.date.is_none() {
        Some("no date".into())
    } else {
        None
    };
    for mut l in [fee, other_side].into_iter().flatten() {
        l.skip = line.skip.clone();
        l.date = line.date;
        extra.push(l);
    }
}
