use anyhow::Result;
use chrono::NaiveDate;
use means_core::{journal, Db};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct Args {
    /// Posted journal entry ID
    id: i64,
    /// Reversal date, YYYY-MM-DD (default: the original booking date)
    #[arg(long)]
    date: Option<NaiveDate>,
    /// Explanation saved in the reversal and audit trail
    #[arg(long, default_value = "")]
    reason: String,
    /// Apply the reversal; without this flag, preview and save nothing
    #[arg(long)]
    yes: bool,
    /// Explicit preview (also the default)
    #[arg(long, conflicts_with = "yes")]
    dry_run: bool,
    /// Print the original entry, reversal, and evidence count as JSON
    #[arg(long)]
    json: bool,
}

pub fn run(path: PathBuf, args: Args) -> Result<()> {
    let db = Db::open(path)?;
    let result = journal::review_void(&mut db.conn(), args.id, args.date, &args.reason, args.yes)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    if result.already_void {
        println!("#{} is already void; reversal #{} on {}. No changes saved.", result.entry.id, result.reversal.id, result.reversal.date);
        return Ok(());
    }
    let action = if result.applied { "Voided" } else { "Preview void of" };
    println!("{action} #{}: {} {}", result.entry.id, result.entry.date, result.entry.payee);
    println!("Reversal on {}: {}", result.reversal.date, result.reversal.description);
    for posting in &result.reversal.postings {
        let side = if posting.quantity.is_negative() { "credit" } else { "debit" };
        println!("  {}: {} {} {} (booked {} {})", posting.account_path, side, posting.quantity.major().abs(), posting.quantity.commodity(), posting.amount.major(), posting.amount.commodity());
    }
    println!("{} statement line(s) {} to unmatched. Original postings are retained.", result.evidence_lines, if result.applied { "returned" } else { "will return" });
    if result.applied {
        println!("Created reversal #{}.", result.reversal.id);
    } else {
        println!("No changes saved. Repeat with --yes to apply to the current entry.");
    }
    Ok(())
}
