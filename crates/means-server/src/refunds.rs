use anyhow::Result;
use clap::Subcommand;
use means_core::{refunds, Db};
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum Command {
    /// List recent equal-and-opposite expense candidates for a credit statement line
    Candidates {
        line: i64,
        #[arg(long, default_value_t = 90)]
        days: i64,
        #[arg(long)]
        json: bool,
    },
    /// Post a full refund against an original expense, preserving its booked splits
    Link {
        line: i64,
        #[arg(long)]
        original: i64,
        #[arg(long)]
        json: bool,
    },
}
pub fn run(path: PathBuf, command: Command) -> Result<()> {
    let db = Db::open(path)?;
    match command {
        Command::Candidates { line, days, json } => {
            let entries = refunds::candidates(&db.conn(), line, days)?;
            if json {
                println!("{}", serde_json::to_string(&entries)?);
            } else {
                for entry in entries {
                    println!("{}\t{}\t{}\t{}", entry.id, entry.date, entry.amount_functional, entry.payee);
                }
            }
        }
        Command::Link { line, original, json } => {
            let entry = refunds::link(&mut db.conn(), line, original)?;
            if json {
                println!("{}", serde_json::to_string(&entry)?);
            } else {
                println!("Posted refund #{} of expense #{} from statement line #{}", entry.id, original, line);
            }
        }
    }
    Ok(())
}
