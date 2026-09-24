use anyhow::Result;
use chrono::NaiveDate;
use clap::{Subcommand, ValueEnum};
use means_core::{reports, Db};
use std::path::PathBuf;

use crate::report_cli::{self, AsOfArgs, BalanceSheetArgs, LedgerArgs, OpenItemsArgs, PeriodArgs, Report};

#[derive(Clone, Copy, ValueEnum)]
pub enum GroupBy {
    Class,
    Tag,
    Payee,
}

#[derive(Subcommand)]
pub enum Command {
    /// Sum expense postings by class in the entity's functional currency
    Expenses {
        #[arg(long)]
        entity: i64,
        /// First date, inclusive (YYYY-MM-DD)
        #[arg(long)]
        from: Option<NaiveDate>,
        /// Last date, inclusive (YYYY-MM-DD)
        #[arg(long)]
        to: Option<NaiveDate>,
        /// Select entries with this exact tag (for example, trip:porto)
        #[arg(long)]
        tag: Option<String>,
        #[arg(long, value_enum, default_value = "class")]
        group_by: GroupBy,
        #[arg(long)]
        json: bool,
    },
    /// Trial balance: opening, debits, credits and closing of each account, in the functional currency
    TrialBalance(PeriodArgs),
    /// Balance sheet: assets, liabilities and equity on a date, at book values by default
    ///
    /// The default values every account at its booked functional value. `--market-values`
    /// values holding accounts at the market price on the date, when a price exists, and
    /// adds the difference as unrealised gains. Retained earnings sums every income and
    /// expense entry up to the date.
    BalanceSheet(BalanceSheetArgs),
    /// Income statement: revenue, expenses and result of a period, with subtotals by type and class
    Income(PeriodArgs),
    /// Cash flow by day, direct method, on bank, cash, card and wallet accounts
    ///
    /// Direct method: the report sums the postings on accounts with the subtype bank, cash,
    /// card or wallet, day by day, at booked functional values. Posted and void entries are
    /// actual. Draft entries are projected and show in separate columns. A transfer between
    /// two of these accounts shows as both in and out.
    CashFlow(PeriodArgs),
    /// General ledger: the postings of each account in a period, with a running balance
    GeneralLedger(LedgerArgs),
    /// Open items: invoices with an open balance on a date, oldest due first
    OpenItems(OpenItemsArgs),
    /// Tax summary of a period by code, kind and rate (the same report as `means tax summary`)
    TaxSummary(PeriodArgs),
    /// Net worth of one entity on a date: assets minus liabilities, holdings at market price
    NetWorth(AsOfArgs),
}

pub fn run(path: PathBuf, command: Command) -> Result<()> {
    match command {
        Command::Expenses { entity, from, to, tag, group_by, json } => expenses(path, entity, from, to, tag, group_by, json),
        Command::TrialBalance(a) => report_cli::run(path, Report::TrialBalance(a)),
        Command::BalanceSheet(a) => report_cli::run(path, Report::BalanceSheet(a)),
        Command::Income(a) => report_cli::run(path, Report::Income(a)),
        Command::CashFlow(a) => report_cli::run(path, Report::CashFlow(a)),
        Command::GeneralLedger(a) => report_cli::run(path, Report::GeneralLedger(a)),
        Command::OpenItems(a) => report_cli::run(path, Report::OpenItems(a)),
        Command::TaxSummary(a) => report_cli::run(path, Report::TaxSummary(a)),
        Command::NetWorth(a) => report_cli::run(path, Report::NetWorth(a)),
    }
}

fn expenses(path: PathBuf, entity: i64, from: Option<NaiveDate>, to: Option<NaiveDate>, tag: Option<String>, group_by: GroupBy, json: bool) -> Result<()> {
    let db = Db::open(path)?;
    if matches!(group_by, GroupBy::Payee) {
        let report = means_core::payees::expenses(&db.conn(), entity, from, to, tag.as_deref())?;
        if json {
            println!("{}", serde_json::to_string(&report)?);
        } else {
            println!("Canonical payee / unresolved booked text\tAmount ({})", report.total.commodity());
            for row in report.rows {
                println!("{}{}\t{}", if row.payee_id.is_none() { "[unresolved] " } else { "" }, row.name, means_core::money::plain(row.amount.major()));
            }
            println!("Total\t{}", means_core::money::plain(report.total.major()));
        }
        return Ok(());
    }
    if matches!(group_by, GroupBy::Tag) {
        let report = reports::expenses_by_tag(&db.conn(), entity, from, to, tag.as_deref())?;
        if json {
            println!("{}", serde_json::to_string(&report)?);
        } else {
            println!("Expense tag (overlapping amounts)\tAmount ({})", report.total.commodity());
            for row in &report.rows {
                println!("{}\t{}", row.tag.as_deref().unwrap_or("Untagged"), means_core::money::fmt(row.amount.major(), row.amount.precision()));
            }
            println!("Total (each posting once)\t{}", means_core::money::fmt(report.total.major(), report.total.precision()));
        }
        return Ok(());
    }
    let report = reports::expenses_by_class(&db.conn(), entity, from, to, tag.as_deref())?;
    if json {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("Expense class\tAmount ({})", report.total.commodity());
        for row in &report.rows {
            println!("{}\t{}", if row.class.is_empty() { "Unclassified" } else { &row.class }, means_core::money::fmt(row.amount.major(), row.amount.precision()));
        }
        println!("Total\t{}", means_core::money::fmt(report.total.major(), report.total.precision()));
    }
    Ok(())
}
