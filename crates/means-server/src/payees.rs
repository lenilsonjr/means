use anyhow::Result;
use clap::Subcommand;
use means_core::{payees, Db};
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum Command {
    /// List active and archived canonical payees
    List {
        #[arg(long)]
        entity: i64,
    },
    /// Preview a new payee, or replace the name, active state and complete alias list of ID
    Save {
        #[arg(long)]
        entity: i64,
        #[arg(long, default_value = "0")]
        id: i64,
        #[arg(long)]
        name: String,
        #[arg(long = "alias")]
        aliases: Vec<String>,
        #[arg(long)]
        archived: bool,
        /// Exact token from the preceding preview
        #[arg(long)]
        confirm: Option<String>,
    },
    /// Preview statement-backed canonical links for unlinked historical entries
    Backfill {
        #[arg(long)]
        entity: i64,
        #[arg(long)]
        confirm: Option<String>,
        /// Attest chart, classes, per-payee splits and ongoing rules have been reviewed
        #[arg(long)]
        chart_reviewed: bool,
    },
    /// Explicitly assign an entry to a canonical payee, preserving booked text
    Link {
        entry: i64,
        #[arg(long)]
        payee: i64,
        #[arg(long)]
        confirm: Option<String>,
    },
    /// Transfer a source's entry links and aliases to a target, then archive the source
    Merge {
        source: i64,
        target: i64,
        #[arg(long)]
        confirm: Option<String>,
    },
}
pub fn run(path: PathBuf, command: Command) -> Result<()> {
    let db = Db::open(path)?;
    let mut c = db.conn();
    let value = match command {
        Command::List { entity } => serde_json::to_value(payees::list(&c, entity)?)?,
        Command::Save { entity, id, name, aliases, archived, confirm } => {
            serde_json::to_value(payees::save(&mut c, payees::Change { id, entity_id: entity, name, active: !archived, aliases }, confirm.as_deref())?)?
        }
        Command::Backfill { entity, confirm, chart_reviewed } => serde_json::to_value(payees::backfill(&mut c, entity, confirm.as_deref(), chart_reviewed)?)?,
        Command::Link { entry, payee, confirm } => serde_json::to_value(payees::reassign(&mut c, 0, payee, Some(entry), confirm.as_deref())?)?,
        Command::Merge { source, target, confirm } => serde_json::to_value(payees::reassign(&mut c, source, target, None, confirm.as_deref())?)?,
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
