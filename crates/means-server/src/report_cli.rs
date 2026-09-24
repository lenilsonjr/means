//! `means report …`: the management reports of one entity.
//!
//! With `--json`, each command prints one JSON object with `"schema_version": 1`. Money uses
//! the `Money` form. Records carry `uid` strings. The `basis` object states the valuation and
//! the entry statuses that the report reads. Without `--json`, each command prints an aligned
//! table. A refusal prints `"ok": false` with an error code and exits with code 1.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use clap::Args;
use means_core::invoices::{self, Direction};
use means_core::model::{Account, AccountType, Entity, ReportRow};
use means_core::money::Money;
use means_core::proposal::{AgentError, AgentResult};
use means_core::rusqlite::{Connection, OptionalExtension};
use means_core::{accounts, reports, tax};
use serde_json::{json, Value};

use crate::entry_cli::{emit, open, parse_date, resolve_entity, Output};

const POSTED_AND_VOID: [&str; 2] = ["posted", "void"];

#[derive(Args)]
pub struct PeriodArgs {
    /// Entity id, name or uid
    #[arg(long)]
    pub entity: String,
    /// First date, inclusive (YYYY-MM-DD)
    #[arg(long)]
    pub from: String,
    /// Last date, inclusive (YYYY-MM-DD)
    #[arg(long)]
    pub to: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct AsOfArgs {
    /// Entity id, name or uid
    #[arg(long)]
    pub entity: String,
    /// Report date, inclusive (YYYY-MM-DD)
    #[arg(long)]
    pub as_of: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct BalanceSheetArgs {
    /// Entity id, name or uid
    #[arg(long)]
    pub entity: String,
    /// Report date, inclusive (YYYY-MM-DD)
    #[arg(long)]
    pub as_of: String,
    /// Value every account at its booked value (the default)
    #[arg(long, conflicts_with = "market_values")]
    pub book_values: bool,
    /// Value holding accounts at their market price when a price exists
    #[arg(long)]
    pub market_values: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct LedgerArgs {
    /// Entity id, name or uid
    #[arg(long)]
    pub entity: String,
    /// First date, inclusive (YYYY-MM-DD)
    #[arg(long)]
    pub from: String,
    /// Last date, inclusive (YYYY-MM-DD)
    #[arg(long)]
    pub to: String,
    /// Only this account (path or uid)
    #[arg(long)]
    pub account: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct OpenItemsArgs {
    /// Entity id, name or uid
    #[arg(long)]
    pub entity: String,
    /// Report date, inclusive (YYYY-MM-DD)
    #[arg(long)]
    pub as_of: String,
    /// sales | purchase
    #[arg(long)]
    pub direction: Option<String>,
    #[arg(long)]
    pub json: bool,
}

/// The report to run.
pub enum Report {
    TrialBalance(PeriodArgs),
    BalanceSheet(BalanceSheetArgs),
    Income(PeriodArgs),
    CashFlow(PeriodArgs),
    GeneralLedger(LedgerArgs),
    OpenItems(OpenItemsArgs),
    TaxSummary(PeriodArgs),
    NetWorth(AsOfArgs),
}

pub fn run(db_path: PathBuf, report: Report) -> anyhow::Result<()> {
    emit(execute(&db_path, report))
}

fn execute(db_path: &Path, report: Report) -> AgentResult<Output> {
    let db = open(db_path)?;
    let conn = db.conn();
    match report {
        Report::TrialBalance(a) => trial_balance(&conn, &a),
        Report::BalanceSheet(a) => balance_sheet(&conn, &a),
        Report::Income(a) => income(&conn, &a),
        Report::CashFlow(a) => cash_flow(&conn, &a),
        Report::GeneralLedger(a) => general_ledger(&conn, &a),
        Report::OpenItems(a) => open_items(&conn, &a),
        Report::TaxSummary(a) => tax_summary(&conn, &a),
        Report::NetWorth(a) => net_worth(&conn, &a),
    }
}

// Shared parts.

fn period(from: &str, to: &str) -> AgentResult<(chrono::NaiveDate, chrono::NaiveDate)> {
    let (from, to) = (parse_date("from", from)?, parse_date("to", to)?);
    if from > to {
        return Err(AgentError::new("invalid_input", format!("from {from} must not follow to {to}")));
    }
    Ok((from, to))
}

fn entity_json(e: &Entity) -> Value {
    json!({"uid": e.uid, "name": e.name, "currency": e.currency})
}

fn basis(valuation: &str, statuses: &[&str], note: &str) -> Value {
    json!({"valuation": valuation, "statuses": statuses, "note": note})
}

fn object(report: &str, entity: &Entity, basis: Value, fields: Value) -> Value {
    let mut value = json!({"report": report, "entity": entity_json(entity), "functional_currency": entity.currency, "basis": basis});
    if let (Some(target), Value::Object(extra)) = (value.as_object_mut(), fields) {
        target.extend(extra);
    }
    value
}

fn accounts_by_id(conn: &Connection, entity_id: i64) -> AgentResult<HashMap<i64, Account>> {
    Ok(accounts::list_accounts(conn, Some(entity_id), true)?.into_iter().map(|a| (a.id, a)).collect())
}

fn non_empty(s: &str) -> Option<&str> {
    Some(s).filter(|s| !s.is_empty())
}

fn m(value: Money) -> String {
    means_core::money::fmt(value.major(), value.precision())
}

fn type_name(t: AccountType) -> &'static str {
    match t {
        AccountType::Asset => "asset",
        AccountType::Liability => "liability",
        AccountType::Equity => "equity",
        AccountType::Income => "income",
        AccountType::Expense => "expense",
    }
}

fn find_account(conn: &Connection, entity_id: i64, key: &str) -> AgentResult<Account> {
    let key = key.trim();
    let by_uid: Option<i64> =
        conn.query_row("SELECT id FROM accounts WHERE uid = ?1 AND entity_id = ?2", means_core::rusqlite::params![key, entity_id], |r| r.get(0)).optional().map_err(means_core::Error::from)?;
    if let Some(id) = by_uid {
        return Ok(accounts::get_account(conn, id)?);
    }
    accounts::find_by_path(conn, entity_id, key)?.ok_or_else(|| AgentError::new("unknown_account", format!("no account {key} in the entity")))
}

fn uid_of(conn: &Connection, table: &str, id: i64) -> AgentResult<Option<String>> {
    Ok(conn.query_row(&format!("SELECT uid FROM {table} WHERE id = ?1"), [id], |r| r.get(0)).optional().map_err(means_core::Error::from)?)
}

/// An aligned table. `align` has one letter per column: `l` for left, `r` for right.
fn table(title: &str, header: &[&str], align: &str, rows: &[Vec<String>]) -> String {
    let align: Vec<char> = align.chars().collect();
    let mut widths: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(cell.chars().count());
            }
        }
    }
    let line = |cells: &[String]| -> String {
        let parts: Vec<String> = cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let w = widths.get(i).copied().unwrap_or(0);
                if align.get(i) == Some(&'r') {
                    format!("{c:>w$}")
                } else {
                    format!("{c:<w$}")
                }
            })
            .collect();
        format!("{}\n", parts.join("  ").trim_end())
    };
    let mut out = format!("{title}\n");
    out.push_str(&line(&header.iter().map(|h| h.to_string()).collect::<Vec<_>>()));
    out.push_str(&line(&widths.iter().map(|w| "-".repeat(*w)).collect::<Vec<_>>()));
    for row in rows {
        out.push_str(&line(row));
    }
    out
}

fn indent(path: &str, depth: i32) -> String {
    format!("{}{}", "  ".repeat(depth.max(0) as usize), path)
}

// Trial balance.

fn trial_balance(conn: &Connection, a: &PeriodArgs) -> AgentResult<Output> {
    let entity = resolve_entity(conn, &a.entity)?;
    let (from, to) = period(&a.from, &a.to)?;
    let tb = reports::trial_balance_period(conn, entity.id, Some(from), Some(to))?;
    let accounts = accounts_by_id(conn, entity.id)?;
    let balanced = tb.debits == tb.credits && tb.closing.is_zero() && tb.opening.is_zero();
    if !a.json {
        let mut rows: Vec<Vec<String>> = tb.rows.iter().map(|r| vec![r.path.clone(), m(r.opening), m(r.debits), m(r.credits), m(r.closing)]).collect();
        rows.push(vec!["Total".into(), m(tb.opening), m(tb.debits), m(tb.credits), m(tb.closing)]);
        let mut text =
            table(&format!("Trial balance of {} from {from} to {to} ({}, debit positive)", entity.name, tb.currency), &["Account", "Opening", "Debits", "Credits", "Closing"], "lrrrr", &rows);
        text.push_str(if balanced { "Balanced\n" } else { "NOT BALANCED\n" });
        return Ok(Output::Text(text));
    }
    let rows: Vec<Value> = tb
        .rows
        .iter()
        .map(|r| {
            let account = accounts.get(&r.account_id);
            json!({
                "uid": account.map(|x| x.uid.clone()),
                "path": r.path,
                "type": type_name(r.r#type),
                "statutory_code": account.and_then(|x| non_empty(&x.statutory_code)),
                "opening": r.opening,
                "debits": r.debits,
                "credits": r.credits,
                "closing": r.closing,
            })
        })
        .collect();
    let note = "Booked functional values. Balances are debit positive. The opening balance sums every entry before the first date.";
    Ok(Output::Json(object(
        "trial_balance",
        &entity,
        basis("book", &POSTED_AND_VOID, note),
        json!({
            "from": from, "to": to, "count": rows.len(), "accounts": rows,
            "totals": {"opening": tb.opening, "debits": tb.debits, "credits": tb.credits, "closing": tb.closing},
            "balanced": balanced,
        }),
    )))
}

// Balance sheet.

fn report_row_json(r: &ReportRow, accounts: &HashMap<i64, Account>, market: bool) -> Value {
    let account = accounts.get(&r.account_id);
    let value = if market { r.market_value.unwrap_or(r.amount) } else { r.amount };
    json!({
        "uid": account.map(|x| x.uid.clone()),
        "path": r.path,
        "name": r.name,
        "type": type_name(r.r#type),
        "depth": r.depth,
        "placeholder": r.placeholder,
        "statutory_code": account.and_then(|x| non_empty(&x.statutory_code)),
        "quantity": r.quantity,
        "book_value": r.amount,
        "market_value": if market { r.market_value } else { None },
        "value": value,
    })
}

fn balance_sheet(conn: &Connection, a: &BalanceSheetArgs) -> AgentResult<Output> {
    let entity = resolve_entity(conn, &a.entity)?;
    let as_of = parse_date("as-of", &a.as_of)?;
    let market = a.market_values;
    let bs = reports::balance_sheet(conn, Some(entity.id), Some(as_of), "")?;
    let accounts = accounts_by_id(conn, entity.id)?;
    let zero = Money::from_minor(0, &entity.currency, bs.total_debit.precision())?;
    let value = |r: &ReportRow| if market { r.market_value.unwrap_or(r.amount) } else { r.amount };
    // The core adds an "Unrealised gains" row for holdings at market price. Book values omit it.
    let rows: Vec<&ReportRow> = bs.rows.iter().filter(|r| market || !(r.account_id == 0 && r.name == "Unrealised gains")).collect();
    let section = |t: AccountType| -> AgentResult<(Vec<&ReportRow>, Money)> {
        let list: Vec<&ReportRow> = rows.iter().copied().filter(|r| r.r#type == t).collect();
        let total = zero.checked_sum(list.iter().filter(|r| r.depth == 0).map(|r| value(r)))?;
        Ok((list, total))
    };
    let (assets, total_assets) = section(AccountType::Asset)?;
    let (liabilities, total_liabilities) = section(AccountType::Liability)?;
    let (equity, total_equity) = section(AccountType::Equity)?;
    let liabilities_and_equity = total_liabilities.checked_add(total_equity)?;
    let balanced = total_assets == liabilities_and_equity;
    let valuation = if market { "market" } else { "book" };
    if !a.json {
        let mut out = Vec::new();
        for (name, list, total) in [("Assets", &assets, total_assets), ("Liabilities", &liabilities, total_liabilities), ("Equity", &equity, total_equity)] {
            out.push(vec![name.to_string(), String::new()]);
            out.extend(list.iter().map(|r| vec![indent(&r.path, r.depth + 1), m(value(r))]));
            out.push(vec![format!("Total {}", name.to_lowercase()), m(total)]);
        }
        out.push(vec!["Liabilities and equity".into(), m(liabilities_and_equity)]);
        let mut text = table(&format!("Balance sheet of {} on {as_of} ({}, {valuation} values)", entity.name, entity.currency), &["Account", "Amount"], "lr", &out);
        text.push_str(if balanced { "Balanced\n" } else { "NOT BALANCED\n" });
        return Ok(Output::Text(text));
    }
    let rows_json = |list: &[&ReportRow]| list.iter().map(|r| report_row_json(r, &accounts, market)).collect::<Vec<_>>();
    let note = if market {
        "Booked functional values. Holding accounts use the market price on the report date when a price exists. The difference goes to Unrealised gains. Retained earnings sums every income and expense entry up to the report date."
    } else {
        "Booked functional values for every account. Retained earnings sums every income and expense entry up to the report date. The rows of parent accounts include their children. Subtotals add the top-level rows."
    };
    Ok(Output::Json(object(
        "balance_sheet",
        &entity,
        basis(valuation, &POSTED_AND_VOID, note),
        json!({
            "as_of": as_of,
            "assets": {"accounts": rows_json(&assets), "total": total_assets},
            "liabilities": {"accounts": rows_json(&liabilities), "total": total_liabilities},
            "equity": {"accounts": rows_json(&equity), "total": total_equity},
            "total_assets": total_assets,
            "total_liabilities": total_liabilities,
            "total_equity": total_equity,
            "liabilities_and_equity": liabilities_and_equity,
            "balanced": balanced,
        }),
    )))
}

// Income statement.

fn income(conn: &Connection, a: &PeriodArgs) -> AgentResult<Output> {
    let entity = resolve_entity(conn, &a.entity)?;
    let (from, to) = period(&a.from, &a.to)?;
    let report = reports::income_statement(conn, entity.id, Some(from), Some(to))?;
    let classes = reports::expenses_by_class(conn, entity.id, Some(from), Some(to), None)?;
    let accounts = accounts_by_id(conn, entity.id)?;
    let revenue: Vec<&ReportRow> = report.rows.iter().filter(|r| r.r#type == AccountType::Income).collect();
    let expenses: Vec<&ReportRow> = report.rows.iter().filter(|r| r.r#type == AccountType::Expense).collect();
    let class_name = |c: &str| if c.is_empty() { "unclassified".to_string() } else { c.to_string() };
    if !a.json {
        let mut out = vec![vec!["Revenue".to_string(), String::new()]];
        out.extend(revenue.iter().map(|r| vec![indent(&r.path, r.depth + 1), m(r.amount)]));
        out.push(vec!["Total revenue".into(), m(report.total_credit)]);
        out.push(vec!["Expenses".into(), String::new()]);
        out.extend(expenses.iter().map(|r| vec![indent(&r.path, r.depth + 1), m(r.amount)]));
        out.push(vec!["Total expenses".into(), m(report.total_debit)]);
        out.push(vec!["Expenses by class".into(), String::new()]);
        out.extend(classes.rows.iter().map(|r| vec![format!("  {}", class_name(&r.class)), m(r.amount)]));
        out.push(vec!["Result".into(), m(report.net)]);
        return Ok(Output::Text(table(&format!("Income statement of {} from {from} to {to} ({})", entity.name, report.currency), &["Account", "Amount"], "lr", &out)));
    }
    let row = |r: &&ReportRow| {
        let account = accounts.get(&r.account_id);
        json!({
            "uid": account.map(|x| x.uid.clone()),
            "path": r.path,
            "name": r.name,
            "depth": r.depth,
            "placeholder": r.placeholder,
            "class": account.and_then(|x| non_empty(&x.class)),
            "statutory_code": account.and_then(|x| non_empty(&x.statutory_code)),
            "amount": r.amount,
        })
    };
    let by_class: Vec<Value> = classes.rows.iter().map(|r| json!({"class": class_name(&r.class), "amount": r.amount})).collect();
    let note = "Booked functional values. Amounts are positive in the normal direction of the account. The rows of parent accounts include their children. Subtotals add the top-level rows.";
    Ok(Output::Json(object(
        "income_statement",
        &entity,
        basis("book", &POSTED_AND_VOID, note),
        json!({
            "from": from, "to": to,
            "revenue": {"accounts": revenue.iter().map(row).collect::<Vec<_>>(), "total": report.total_credit},
            "expenses": {"accounts": expenses.iter().map(row).collect::<Vec<_>>(), "total": report.total_debit, "by_class": by_class},
            "result": report.net,
        }),
    )))
}

// Cash flow.

fn cash_flow(conn: &Connection, a: &PeriodArgs) -> AgentResult<Output> {
    let entity = resolve_entity(conn, &a.entity)?;
    let (from, to) = period(&a.from, &a.to)?;
    let opening = reports::cashflow_opening(conn, entity.id, from)?;
    let days = reports::cashflow(conn, entity.id, from, to)?;
    let (code, precision) = (opening.commodity().to_string(), opening.precision());
    let money = |d: rust_decimal::Decimal| Money::from_major(d, &code, precision);
    let zero = Money::from_minor(0, &code, precision)?;
    let mut rows = Vec::new();
    let (mut posted_in, mut posted_out, mut draft_in, mut draft_out) = (zero, zero, zero, zero);
    for d in &days {
        let r = (money(d.posted_in)?, money(d.posted_out)?, money(d.draft_in)?, money(d.draft_out)?, money(d.balance)?);
        posted_in = posted_in.checked_add(r.0)?;
        posted_out = posted_out.checked_add(r.1)?;
        draft_in = draft_in.checked_add(r.2)?;
        draft_out = draft_out.checked_add(r.3)?;
        rows.push((d.date, r));
    }
    let posted_closing = opening.checked_add(posted_in)?.checked_sub(posted_out)?;
    let projected_closing = posted_closing.checked_add(draft_in)?.checked_sub(draft_out)?;
    if !a.json {
        let mut out: Vec<Vec<String>> = vec![vec!["Opening".into(), String::new(), String::new(), String::new(), String::new(), m(opening)]];
        out.extend(rows.iter().map(|(date, r)| vec![date.to_string(), m(r.0), m(r.1), m(r.2), m(r.3), m(r.4)]));
        out.push(vec!["Total".into(), m(posted_in), m(posted_out), m(draft_in), m(draft_out), m(projected_closing)]);
        let mut text = table(
            &format!("Cash flow of {} from {from} to {to} ({}, direct method on bank, cash, card and wallet accounts)", entity.name, code),
            &["Date", "In", "Out", "Draft in", "Draft out", "Balance"],
            "lrrrrr",
            &out,
        );
        text.push_str(&format!("Posted closing balance: {}\n", m(posted_closing)));
        return Ok(Output::Text(text));
    }
    let days_json: Vec<Value> = rows.iter().map(|(date, r)| json!({"date": date, "posted_in": r.0, "posted_out": r.1, "draft_in": r.2, "draft_out": r.3, "projected_balance": r.4})).collect();
    let note = "Direct method. The report reads postings on accounts with the subtype bank, cash, card or wallet, at booked functional values. Transfers between these accounts show as both in and out. Posted and void entries are actual. Draft entries are projected and show in separate columns.";
    Ok(Output::Json(object(
        "cash_flow",
        &entity,
        basis("book", &["posted", "void", "draft"], note),
        json!({
            "from": from, "to": to, "method": "direct",
            "opening": opening,
            "days": days_json,
            "totals": {"posted_in": posted_in, "posted_out": posted_out, "draft_in": draft_in, "draft_out": draft_out},
            "posted_closing": posted_closing,
            "projected_closing": projected_closing,
        }),
    )))
}

// General ledger.

fn general_ledger(conn: &Connection, a: &LedgerArgs) -> AgentResult<Output> {
    let entity = resolve_entity(conn, &a.entity)?;
    let (from, to) = period(&a.from, &a.to)?;
    let selected: Vec<Account> = match &a.account {
        Some(key) => vec![find_account(conn, entity.id, key)?],
        None => {
            let mut list: Vec<Account> = accounts::list_accounts(conn, Some(entity.id), true)?.into_iter().filter(|x| !x.placeholder).collect();
            list.sort_by(|x, y| (type_order(x.r#type), &x.path).cmp(&(type_order(y.r#type), &y.path)));
            list
        }
    };
    let mut ledgers = Vec::new();
    for account in selected {
        let ledger = reports::general_ledger(conn, account.id, Some(from), Some(to), i64::MAX, false)?;
        if a.account.is_none() && ledger.rows.is_empty() && ledger.opening_balance.is_zero() {
            continue;
        }
        ledgers.push((account, ledger));
    }
    if !a.json {
        let mut text = format!("General ledger of {} from {from} to {to}\n", entity.name);
        for (account, ledger) in &ledgers {
            let mut rows = vec![vec![from.to_string(), "Opening balance".into(), String::new(), String::new(), String::new(), m(ledger.opening_balance)]];
            rows.extend(ledger.rows.iter().map(|r| {
                let payee = if r.display_payee.is_empty() { r.description.clone() } else { r.display_payee.clone() };
                vec![r.date.to_string(), payee, r.contra_path.clone(), m(r.debit), m(r.credit), m(r.running_balance)]
            }));
            rows.push(vec![to.to_string(), "Closing balance".into(), String::new(), String::new(), String::new(), m(ledger.closing_balance)]);
            text.push('\n');
            text.push_str(&table(&format!("{} ({})", account.path, ledger.commodity), &["Date", "Payee", "Contra account", "Debit", "Credit", "Balance"], "lllrrr", &rows));
        }
        return Ok(Output::Text(text));
    }
    let mut accounts_json = Vec::new();
    for (account, ledger) in &ledgers {
        let zero = Money::from_minor(0, &ledger.commodity, ledger.opening_balance.precision())?;
        let debits = zero.checked_sum(ledger.rows.iter().map(|r| r.debit))?;
        let credits = zero.checked_sum(ledger.rows.iter().map(|r| r.credit))?;
        let mut postings = Vec::new();
        for r in &ledger.rows {
            postings.push(json!({
                "entry_uid": uid_of(conn, "journal_entries", r.journal_entry_id)?,
                "posting_uid": uid_of(conn, "postings", r.posting_id)?,
                "date": r.date,
                "status": r.status.as_str(),
                "payee": r.display_payee,
                "description": r.description,
                "contra_path": r.contra_path,
                "debit": r.debit,
                "credit": r.credit,
                "amount": r.amount,
                "running_balance": r.running_balance,
            }));
        }
        accounts_json.push(json!({
            "uid": account.uid,
            "path": account.path,
            "type": type_name(account.r#type),
            "commodity": ledger.commodity,
            "statutory_code": non_empty(&account.statutory_code),
            "opening_balance": ledger.opening_balance,
            "debits": debits,
            "credits": credits,
            "closing_balance": ledger.closing_balance,
            "postings": postings,
        }));
    }
    let note = "Quantities in the account commodity. The running balance is positive in the normal direction of the account. `amount` is the booked functional value of the posting.";
    Ok(Output::Json(object("general_ledger", &entity, basis("book", &POSTED_AND_VOID, note), json!({"from": from, "to": to, "count": accounts_json.len(), "accounts": accounts_json}))))
}

fn type_order(t: AccountType) -> u8 {
    match t {
        AccountType::Asset => 0,
        AccountType::Liability => 1,
        AccountType::Equity => 2,
        AccountType::Income => 3,
        AccountType::Expense => 4,
    }
}

// Open items.

fn open_items(conn: &Connection, a: &OpenItemsArgs) -> AgentResult<Output> {
    let entity = resolve_entity(conn, &a.entity)?;
    let as_of = parse_date("as-of", &a.as_of)?;
    let direction = a.direction.as_deref().map(Direction::parse).transpose()?;
    let items = invoices::open_items(conn, entity.id, as_of, direction)?;
    let mut totals: BTreeMap<(String, String), Money> = BTreeMap::new();
    for item in &items {
        let key = (item.direction.as_str().to_string(), item.open.commodity().to_string());
        let sum = match totals.get(&key) {
            Some(total) => total.checked_add(item.open)?,
            None => item.open,
        };
        totals.insert(key, sum);
    }
    if !a.json {
        let mut rows: Vec<Vec<String>> = items
            .iter()
            .map(|i| {
                let number = if i.series.is_empty() { i.number.clone() } else { format!("{}-{}", i.series, i.number) };
                vec![
                    i.direction.as_str().to_string(),
                    number,
                    i.party_name.clone().unwrap_or_default(),
                    i.issue_date.to_string(),
                    i.due_date.map(|d| d.to_string()).unwrap_or_default(),
                    i.days_overdue.to_string(),
                    i.open.commodity().to_string(),
                    m(i.gross),
                    m(i.open),
                ]
            })
            .collect();
        for ((dir, code), total) in &totals {
            rows.push(vec![format!("Total {dir}"), String::new(), String::new(), String::new(), String::new(), String::new(), code.clone(), String::new(), m(*total)]);
        }
        return Ok(Output::Text(table(
            &format!("Open items of {} on {as_of}", entity.name),
            &["Direction", "Number", "Party", "Issued", "Due", "Overdue", "Currency", "Gross", "Open"],
            "lllllrlrr",
            &rows,
        )));
    }
    let totals_json: Vec<Value> = totals.iter().map(|((dir, code), total)| json!({"direction": dir, "commodity": code, "open": total})).collect();
    let items_json = serde_json::to_value(&items).map_err(means_core::Error::from)?;
    let note = "Invoice commodity. Open is the gross amount minus the settlements dated on or before the report date. Settlements of void entries do not count. Cancelled invoices and credit notes are not open items.";
    Ok(Output::Json(object(
        "open_items",
        &entity,
        basis("invoice", &POSTED_AND_VOID, note),
        json!({"as_of": as_of, "direction": direction.map(|d| d.as_str()), "count": items.len(), "items": items_json, "totals": totals_json}),
    )))
}

// Tax summary.

fn tax_summary(conn: &Connection, a: &PeriodArgs) -> AgentResult<Output> {
    let entity = resolve_entity(conn, &a.entity)?;
    let (from, to) = period(&a.from, &a.to)?;
    let rows = tax::tax_summary(conn, entity.id, from, to)?;
    if !a.json {
        let out: Vec<Vec<String>> =
            rows.iter().map(|r| vec![r.code.clone(), r.kind.clone(), r.rate_bp.to_string(), r.count.to_string(), m(r.base), m(r.tax), m(r.recoverable), m(r.withheld)]).collect();
        return Ok(Output::Text(table(
            &format!("Tax summary of {} from {from} to {to} ({})", entity.name, entity.currency),
            &["Code", "Kind", "Rate bp", "Events", "Base", "Tax", "Recoverable", "Withheld"],
            "llrrrrrr",
            &out,
        )));
    }
    let rows: Vec<Value> = rows.iter().map(crate::tax_cli::summary_json).collect();
    let note = "Tax events by tax point, at booked functional values. A void entry and its reversal both count, each on its own tax point. This is the same report as `means tax summary`.";
    Ok(Output::Json(object("tax_summary", &entity, basis("book", &POSTED_AND_VOID, note), json!({"from": from, "to": to, "count": rows.len(), "rows": rows}))))
}

// Net worth.

fn net_worth(conn: &Connection, a: &AsOfArgs) -> AgentResult<Output> {
    let entity = resolve_entity(conn, &a.entity)?;
    let as_of = parse_date("as-of", &a.as_of)?;
    let nw = reports::net_worth(conn, entity.id, "", Some(as_of))?;
    let accounts = accounts_by_id(conn, entity.id)?;
    let zero = Money::from_minor(0, &nw.currency, nw.total.precision())?;
    let value = |r: &ReportRow| r.market_value.unwrap_or(r.amount);
    let assets = zero.checked_sum(nw.by_entity.iter().map(|r| r.debit))?;
    let liabilities = zero.checked_sum(nw.by_entity.iter().map(|r| r.credit))?;
    if !a.json {
        let mut out: Vec<Vec<String>> = nw.by_account.iter().map(|r| vec![r.path.clone(), m(value(r))]).collect();
        out.push(vec!["Assets".into(), m(assets)]);
        out.push(vec!["Liabilities".into(), m(liabilities)]);
        out.push(vec!["Net worth".into(), m(nw.total)]);
        return Ok(Output::Text(table(&format!("Net worth of {} on {as_of} ({})", entity.name, nw.currency), &["Account", "Amount"], "lr", &out)));
    }
    let rows: Vec<Value> = nw.by_account.iter().map(|r| report_row_json(r, &accounts, true)).collect();
    let note = "Booked functional values. Holding accounts use the market price on the report date when a price exists. Net worth is assets minus liabilities of this entity only.";
    Ok(Output::Json(object(
        "net_worth",
        &entity,
        basis("market", &POSTED_AND_VOID, note),
        json!({"as_of": as_of, "count": rows.len(), "accounts": rows, "assets": assets, "liabilities": liabilities, "net_worth": nw.total}),
    )))
}
