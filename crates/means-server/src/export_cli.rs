//! `means export`: a Beancount file, or a hashed snapshot directory and its check.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use means_core::snapshot::{self, SnapshotOptions};
use means_core::Db;
use serde_json::json;

#[derive(Subcommand)]
pub enum Command {
    /// Hash every file of a snapshot again and check its manifest (exit 1 on a mismatch)
    Verify {
        dir: PathBuf,
        #[arg(long)]
        json: bool,
    },
}

pub fn run(db_path: &Path, command: Option<Command>, format: &str, output: Option<PathBuf>, entity: &[String], as_of: Option<&str>, json: bool) -> Result<()> {
    if let Some(Command::Verify { dir, json }) = command {
        return verify(&dir, json);
    }
    let outcome = export(db_path, format, output, entity, as_of, json);
    match outcome {
        // A snapshot with --json prints the error object, like every other JSON command.
        Err(e) if json && format == "snapshot" => crate::entry_cli::fail(error_code(&e), &e.to_string()),
        other => other,
    }
}

/// The code of a failed export in the error vocabulary.
fn error_code(e: &anyhow::Error) -> &'static str {
    if let Some(core) = e.downcast_ref::<means_core::Error>() {
        return match core {
            means_core::Error::Conflict(m) if m.starts_with("hash mismatch") => "hash_mismatch",
            means_core::Error::Conflict(m) if m.contains("a snapshot needs a new directory") => "invalid_path",
            means_core::Error::Other(_) => "io_error",
            other => means_core::proposal::error_code(other),
        };
    }
    if e.downcast_ref::<std::io::Error>().is_some() {
        return "io_error";
    }
    "invalid_input"
}

fn export(db_path: &Path, format: &str, output: Option<PathBuf>, entity: &[String], as_of: Option<&str>, json: bool) -> Result<()> {
    let as_of = as_of.map(means_core::parse_date).transpose()?;
    let db = Db::open(db_path)?;
    let mut conn = db.conn();
    let entity_uids = entity.iter().map(|key| crate::entity_cli::find_entity(&conn, key).map(|e| e.uid)).collect::<means_core::Result<Vec<_>>>()?;
    match format {
        "snapshot" => {
            let Some(dir) = output else { bail!("a snapshot needs --output DIR") };
            let outcome = snapshot::export(&mut conn, &dir, &SnapshotOptions { entity_uids, as_of })?;
            let counts = &outcome.manifest["counts"];
            if json {
                println!("{}", json!({"schema_version": 1, "ok": true, "output": dir, "counts": counts}));
            } else {
                println!("Snapshot written to {}: {} entries, {} drafts, {} documents.", dir.display(), counts["entries"], counts["drafts"], counts["documents"]);
            }
            Ok(())
        }
        _ => {
            let document = if entity_uids.is_empty() && as_of.is_none() {
                means_core::export::beancount(&mut conn)?
            } else {
                let ids: HashSet<String> = entity_uids.into_iter().collect();
                means_core::export::beancount_for(&mut conn, if ids.is_empty() { None } else { Some(&ids) }, as_of)?
            };
            match output {
                Some(path) => {
                    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).with_context(|| format!("create {}", path.display()))?;
                    file.write_all(document.as_bytes())?;
                }
                None => std::io::stdout().lock().write_all(document.as_bytes())?,
            }
            Ok(())
        }
    }
}

fn verify(dir: &Path, json: bool) -> Result<()> {
    let report = match snapshot::verify(dir) {
        Ok(report) => report,
        Err(e) => means_core::snapshot::VerifyReport { ok: false, files_checked: 0, problems: vec![e.to_string()] },
    };
    if json {
        println!("{}", json!({"schema_version": 1, "ok": report.ok, "files_checked": report.files_checked, "problems": report.problems}));
    } else if report.ok {
        println!("Snapshot {} is intact: {} files checked.", dir.display(), report.files_checked);
    } else {
        for problem in &report.problems {
            eprintln!("{problem}");
        }
        eprintln!("Snapshot {} failed the check: {} problems.", dir.display(), report.problems.len());
    }
    if !report.ok {
        std::process::exit(1);
    }
    Ok(())
}
