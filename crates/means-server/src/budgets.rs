use anyhow::Result;
use chrono::NaiveDate;
use clap::{Args, Subcommand};
use means_core::{budgets, money, Db};
use std::path::PathBuf;

#[derive(Args)]
pub struct Definition {
    #[arg(long)]
    entity: i64,
    #[arg(long)]
    name: String,
    /// Expense category, including descendants
    #[arg(long, conflicts_with = "class")]
    account: Option<i64>,
    /// Expense class; use unclassified for accounts with no class
    #[arg(long)]
    class: Option<String>,
    /// Exact tag: optional for category/class, required for tag-only budgets
    #[arg(long)]
    tag: Option<String>,
    #[arg(long)]
    amount: String,
    /// Inclusive first day; omit both dates for the current calendar month
    #[arg(long)]
    from: Option<NaiveDate>,
    #[arg(long)]
    to: Option<NaiveDate>,
}
impl Definition {
    fn input(self) -> Result<budgets::BudgetInput> {
        Ok(budgets::BudgetInput {
            entity_id: self.entity,
            name: self.name,
            scope: if self.account.is_some() {
                "category"
            } else if self.class.is_some() {
                "class"
            } else {
                "tag"
            }
            .into(),
            account_id: self.account,
            class: self.class.map(|c| if c == "unclassified" { String::new() } else { c }),
            tag: self.tag,
            starts_on: self.from,
            ends_on: self.to,
            amount: money::parse(&self.amount)?,
        })
    }
}
#[derive(Subcommand)]
pub enum Command {
    /// Create one spending limit for a whole period
    Create(Definition),
    /// Replace a budget's definition (same entity)
    Update {
        id: i64,
        #[command(flatten)]
        definition: Definition,
    },
    /// Show limits and booked spending; rows can overlap and are not additive
    List {
        #[arg(long)]
        entity: i64,
        /// Select budgets valid on this day; omit to include every period
        #[arg(long)]
        on: Option<NaiveDate>,
        /// Select budgets with this exact tag filter
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Delete a plan without changing any entries
    Delete { id: i64 },
}
pub fn run(path: PathBuf, command: Command) -> Result<()> {
    let db = Db::open(path)?;
    let mut conn = db.conn();
    match command {
        Command::Create(definition) => println!("{}", serde_json::to_string(&budgets::save(&mut conn, None, definition.input()?)?)?),
        Command::Update { id, definition } => println!("{}", serde_json::to_string(&budgets::save(&mut conn, Some(id), definition.input()?)?)?),
        Command::Delete { id } => {
            budgets::delete(&mut conn, id)?;
            println!("Deleted budget #{id}");
        }
        Command::List { entity, on, tag, json } => {
            let rows = budgets::list(&conn, entity, on, tag.as_deref())?;
            if json {
                println!("{}", serde_json::to_string(&rows)?);
            } else {
                println!("Budgets (overlapping limits; no combined total)");
                println!("ID\tName\tClass\tTarget\tTag\tPeriod\tLimit\tSpent\tRemaining\tCurrency");
                for row in rows {
                    let b = row.budget;
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}..{}\t{}\t{}\t{}\t{}",
                        b.id,
                        b.name,
                        row.group,
                        row.target,
                        b.tag.unwrap_or_default(),
                        b.starts_on,
                        b.ends_on,
                        money::fmt(b.limit.major(), b.limit.precision()),
                        money::fmt(row.spent.major(), row.spent.precision()),
                        money::fmt(row.remaining.major(), row.remaining.precision()),
                        b.limit.commodity()
                    );
                }
            }
        }
    }
    Ok(())
}
