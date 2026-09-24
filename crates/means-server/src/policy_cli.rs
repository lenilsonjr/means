//! `means policy`: show or set the ledger-wide posting policy.

use std::path::{Path, PathBuf};

use clap::Subcommand;
use means_core::policy::{self, PostingPolicy};
use means_core::proposal::AgentResult;
use serde_json::json;

use crate::entry_cli::{emit, open, refuse_agent, Output};

#[derive(Subcommand)]
pub enum Command {
    /// Show the posting policy
    Show {
        #[arg(long)]
        json: bool,
    },
    /// Set the posting policy: normal or draft-only (a human action)
    Set {
        /// normal | draft-only
        policy: String,
        #[arg(long)]
        json: bool,
    },
}

pub fn run(db_path: PathBuf, command: Command) -> anyhow::Result<()> {
    emit(execute(&db_path, command))
}

fn execute(db_path: &Path, command: Command) -> AgentResult<Output> {
    match command {
        Command::Show { json } => {
            let db = open(db_path)?;
            let current = policy::current(&db.conn())?;
            Ok(if json { Output::Json(json!({"posting_policy": current.as_str()})) } else { Output::Text(format!("posting policy: {}\n", current.as_str())) })
        }
        Command::Set { policy: wanted, json } => {
            refuse_agent("policy set")?;
            let wanted = PostingPolicy::parse(&wanted)?;
            let db = open(db_path)?;
            let previous = policy::set(&mut db.conn(), wanted)?;
            Ok(if json {
                Output::Json(json!({"posting_policy": wanted.as_str(), "previous": previous.as_str()}))
            } else {
                Output::Text(format!("posting policy: {} (was {})\n", wanted.as_str(), previous.as_str()))
            })
        }
    }
}
