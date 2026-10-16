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
    /// Also void the linked entries of other entities (inter-vault links)
    #[arg(long)]
    cascade: bool,
    /// The person who voids with --cascade; saved on each audit event of the cascade (default: $USER)
    #[arg(long, requires = "cascade")]
    actor: Option<String>,
    /// Print the original entry, reversal, linked entries, evidence count and `applied` as JSON
    #[arg(long)]
    json: bool,
}

pub fn run(path: PathBuf, args: Args) -> Result<()> {
    if args.yes {
        crate::entry_cli::guard("void --yes");
    }
    let db = Db::open(path)?;
    let actor = if args.cascade { crate::entry_cli::actor_or_user(args.actor.clone()) } else { String::new() };
    let result = journal::review_void_by(&mut db.conn(), args.id, args.date, &args.reason, args.yes, args.cascade, &actor)?;
    if args.json {
        let mut value = serde_json::to_value(&result)?;
        // A preview rolls the reversal back. Its ids do not exist, so the preview leaves them out,
        // as `means entry void --dry-run` does.
        if !result.applied && !result.already_void {
            value["reversal"] = result.reversal_ref();
        }
        if args.cascade {
            value["actor"] = serde_json::json!(actor);
        }
        println!("{}", serde_json::to_string_pretty(&value)?);
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
    print!("{}", linked_text(&result));
    if result.applied {
        println!("Created reversal #{}.", result.reversal.id);
    } else if result.needs_cascade {
        println!("No changes saved. Repeat with --yes --cascade to void the linked entries too.");
        println!("To void only this entry, remove each link first: means intervault unlink <link uid> --actor <name> --reason <text>.");
    } else {
        println!("No changes saved. Repeat with --yes{} to apply to the current entry.", if result.cascade { " --cascade" } else { "" });
    }
    Ok(())
}

/// The linked entries of other entities that the void reaches, one line each.
fn linked_text(result: &journal::VoidReview) -> String {
    if result.linked_entries.is_empty() {
        return String::new();
    }
    let mut out = format!("Linked entries in other entities ({}):\n", result.linked_entries.len());
    for l in &result.linked_entries {
        let effect = match (result.applied, l.voided, l.cascade_action.as_str()) {
            (true, true, _) => "voided".to_string(),
            (true, false, _) => "kept as a draft; link void".to_string(),
            (false, _, "void") if result.cascade => "will be voided".to_string(),
            (false, _, "void") => "--cascade voids it".to_string(),
            _ if result.cascade => "stays a draft; the link becomes void".to_string(),
            _ => "stays a draft; --cascade marks the link void".to_string(),
        };
        let lock = if l.locked { "; its period is locked" } else { "" };
        out.push_str(&format!("  {} via link {} ({}): {}{lock}\n", l.describe(), l.link_uid, l.link_kind, effect));
    }
    out
}
