use anyhow::Result;
use chrono::NaiveDate;
use clap::{Subcommand, ValueEnum};
use means_core::{reports, Db};
use std::path::PathBuf;

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
}

pub fn run(path: PathBuf, command: Command) -> Result<()> {
    let db = Db::open(path)?;
    match command {
        Command::Expenses { entity, from, to, tag, group_by, json } => {
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
        }
    }
    Ok(())
}
