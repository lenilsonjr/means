//! The inbox: a watched folder where bank files land and become imports on their own.
//!
//! `scan` runs one pass. Every statement file is recorded exactly once (the imports table's
//! checksum remembers it), imported straight away when a learned profile knows the account,
//! and kept as a `pending` import otherwise so the review surfaces can finish it with one
//! keystroke. Processed files move to `done/` inside the folder, so the folder stays an inbox.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};

use crate::model::AccountType;
use crate::{accounts, new_uid, now_ts, Error, Result};

use super::{bank_api, checksum, detect_source, enable_banking, get_import, mercury, pluggy, run_import, ImportOutcome, ImportRequest};

/// What one file became during a scan.
#[derive(Debug, Clone)]
pub struct InboxOutcome {
    pub file: String,
    /// imported | pending | duplicate | failed | ignored
    pub action: String,
    pub import_id: Option<i64>,
    pub detail: String,
}

/// A learned routing rule for statement files. An empty glob is the source-wide fallback.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportProfile {
    pub id: i64,
    pub source: String,
    pub filename_glob: String,
    pub account_id: i64,
    pub hits_count: i64,
    pub updated_at: String,
}

fn profile_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImportProfile> {
    Ok(ImportProfile { id: row.get(0)?, source: row.get(1)?, filename_glob: row.get(2)?, account_id: row.get(3)?, hits_count: row.get(4)?, updated_at: row.get(5)? })
}

/// List every learned route, including routes to closed accounts, so they can be inspected or removed.
pub fn list_profiles(conn: &Connection) -> Result<Vec<ImportProfile>> {
    let mut stmt = conn.prepare("SELECT id, source, filename_glob, account_id, hits_count, updated_at FROM import_profiles ORDER BY source, LENGTH(filename_glob) DESC, hits_count DESC, id")?;
    let rows = stmt.query_map([], profile_row)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Forget one learned route without changing any past imports or postings.
pub fn delete_profile(conn: &mut Connection, id: i64) -> Result<ImportProfile> {
    let tx = conn.transaction()?;
    let profile = tx
        .query_row("SELECT id, source, filename_glob, account_id, hits_count, updated_at FROM import_profiles WHERE id = ?1", [id], profile_row)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("import profile #{id}")))?;
    tx.execute("DELETE FROM import_profiles WHERE id = ?1", [id])?;
    crate::audit::log(&tx, "import_profiles", id, "delete", Some(serde_json::to_value(&profile)?), None)?;
    tx.commit()?;
    Ok(profile)
}

const EXTENSIONS: [&str; 6] = ["csv", "ofx", "qfx", "xml", "txt", "json"];

/// One pass over the folder. Safe to run every few seconds: recorded files are skipped by checksum.
pub fn scan(conn: &mut Connection, dir: &Path) -> Result<Vec<InboxOutcome>> {
    std::fs::create_dir_all(dir).map_err(|e| Error::Invalid(format!("create {}: {e}", dir.display())))?;
    let done = dir.join("done");
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir).map_err(|e| Error::Invalid(format!("read {}: {e}", dir.display())))?.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    files.sort();
    let mut out = Vec::new();
    for path in files {
        let name = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        if name.starts_with('.') {
            continue;
        }
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if ext == "atb" {
            out.push(InboxOutcome { file: name, action: "ignored".into(), import_id: None, detail: "an Account Tracker backup: import it with `means import`".into() });
            continue;
        }
        if !EXTENSIONS.contains(&ext.as_str()) {
            out.push(InboxOutcome { file: name, action: "ignored".into(), import_id: None, detail: format!(".{ext} is not a statement file") });
            continue;
        }
        let content = match std::fs::read(&path) {
            Ok(c) => c,
            Err(e) => {
                out.push(InboxOutcome { file: name, action: "failed".into(), import_id: None, detail: format!("cannot read: {e}") });
                continue;
            }
        };
        if content.is_empty() {
            continue; // still being written; the next pass gets it
        }
        // A .json file is a channel's own payload or nothing: detect_source calls anything else a
        // generic CSV, and such an import can never be completed. Leave it where it lies.
        if ext == "json" && !pluggy::is_pluggy_file(&content) && !enable_banking::is_file(&content) && !mercury::is_file(&content) && bank_api::source(&content).is_none() {
            out.push(InboxOutcome { file: name, action: "ignored".into(), import_id: None, detail: format!(".{ext} is not a statement file") });
            continue;
        }
        out.push(take_file(conn, &path, &name, &content, &done)?);
    }
    Ok(out)
}

/// Record one file: duplicate, imported into a learned account, or pending.
fn take_file(conn: &mut Connection, path: &Path, name: &str, content: &[u8], done: &Path) -> Result<InboxOutcome> {
    let sum = checksum(content);
    let known: Option<i64> = conn.query_row("SELECT id FROM imports WHERE checksum = ?1 ORDER BY id LIMIT 1", [sum.clone()], |r| r.get(0)).optional()?;
    if let Some(id) = known {
        stash(path, done, name)?;
        return Ok(InboxOutcome { file: name.into(), action: "duplicate".into(), import_id: Some(id), detail: format!("already recorded as import #{id}") });
    }
    let routing_name = match super::email::routing_filename(path.parent().unwrap_or(Path::new(".")), name, content) {
        Ok(value) => value.unwrap_or_else(|| name.to_owned()),
        Err(e) => return Ok(InboxOutcome { file: name.into(), action: "failed".into(), import_id: None, detail: e.to_string() }),
    };
    let source = detect_source(name, content);
    if source == "account_tracker" {
        return Ok(InboxOutcome { file: name.into(), action: "ignored".into(), import_id: None, detail: "an Account Tracker backup: import it with `means import`".into() });
    }
    let resolved = match source.as_str() {
        enable_banking::SOURCE => enable_banking::account_for_file(conn, content)?.filter(|id| takes_statements(conn, *id)),
        bank_api::INTER | bank_api::WISE => bank_api::account_for_file(conn, content)?.filter(|id| takes_statements(conn, *id)),
        mercury::SOURCE => mercury::account_for_file(conn, content)?.filter(|id| takes_statements(conn, *id)),
        _ => resolve_account(conn, &source, &routing_name)?,
    };
    match resolved {
        Some(account_id) => match run_import(conn, ImportRequest::new(&source, Some(account_id), &routing_name, content)) {
            Ok(o) => {
                stash(path, done, name)?;
                let account = accounts::get_account(conn, account_id)?;
                Ok(InboxOutcome {
                    file: name.into(),
                    action: "imported".into(),
                    import_id: Some(o.import.id),
                    detail: format!(
                        "{}: {} lines, {} created, {} matched, {} duplicates{}",
                        account.path,
                        o.import.lines_count,
                        o.import.created_count,
                        o.import.matched_count,
                        o.import.duplicate_count,
                        o.import.options.get("coverage_warning").and_then(|v| v.as_str()).map(|w| format!("; warning: {w}")).unwrap_or_default()
                    ),
                })
            }
            Err(Error::Conflict(m)) => {
                stash(path, done, name)?;
                Ok(InboxOutcome { file: name.into(), action: "duplicate".into(), import_id: None, detail: m })
            }
            Err(e) => {
                let id = record(conn, &source, &routing_name, &sum, content, "failed", &e.to_string())?;
                stash(path, done, name)?;
                Ok(InboxOutcome { file: name.into(), action: "failed".into(), import_id: Some(id), detail: e.to_string() })
            }
        },
        None => {
            let id = record(conn, &source, &routing_name, &sum, content, "pending", "")?;
            stash(path, done, name)?;
            Ok(InboxOutcome { file: name.into(), action: "pending".into(), import_id: Some(id), detail: "no learned account for this source yet: assign one on the Imports screen".into() })
        }
    }
}

/// Finish a pending import: run the stored file into the chosen account.
pub fn complete_import(conn: &mut Connection, id: i64, account_id: i64) -> Result<ImportOutcome> {
    let import = get_import(conn, id)?.0;
    if import.status != "pending" {
        return Err(Error::Invalid(format!("import #{id} is {}; only a pending import takes an account", import.status)));
    }
    let content: Option<Vec<u8>> = conn.query_row("SELECT content FROM imports WHERE id = ?1", [id], |r| r.get(0))?;
    let content = content.ok_or_else(|| Error::Invalid(format!("import #{id} kept no file content")))?;
    let out = match run_import(conn, ImportRequest::new(&import.source, Some(account_id), &import.filename, &content).options(import.options.clone())) {
        Ok(o) => o,
        Err(e) => {
            conn.execute("UPDATE imports SET error = ?2 WHERE id = ?1", params![id, e.to_string()])?;
            return Err(e);
        }
    };
    conn.execute("DELETE FROM imports WHERE id = ?1", [id])?;
    crate::audit::log(conn, "imports", out.import.id, "complete", Some(serde_json::json!({"pending_id": id})), Some(serde_json::json!({"account_id": account_id, "filename": import.filename})))?;
    Ok(out)
}

/// An import row that holds the file and waits (pending), or remembers a failure.
fn record(conn: &Connection, source: &str, filename: &str, sum: &str, content: &[u8], status: &str, error: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO imports (uid, source, account_id, filename, checksum, status, error, options, content, created_at) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, '{}', ?7, ?8)",
        params![new_uid(), source, filename, sum, status, error, content, now_ts()],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Move a processed file into done/, keeping the name unique.
fn stash(path: &Path, done: &Path, name: &str) -> Result<()> {
    std::fs::create_dir_all(done).map_err(|e| Error::Invalid(format!("create {}: {e}", done.display())))?;
    let mut target = done.join(name);
    let mut n = 1;
    while target.exists() {
        n += 1;
        target = done.join(format!("{n}-{name}"));
    }
    std::fs::rename(path, &target).map_err(|e| Error::Invalid(format!("move {} to {}: {e}", path.display(), target.display())))?;
    Ok(())
}

/// The account files of this source and name land in: the longest matching filename glob wins,
/// the source-level default ('' glob) is the fallback. Closed or merged-away accounts drop out.
pub fn resolve_account(conn: &Connection, source: &str, filename: &str) -> Result<Option<i64>> {
    if matches!(source, enable_banking::SOURCE | mercury::SOURCE | bank_api::INTER | bank_api::WISE) {
        return Ok(None); // The envelope, not the filename, identifies this account.
    }
    // A channel names the account it pulled from inside the file name, so its files do not share a
    // source-level profile: an account nobody has placed yet stays unknown instead of landing in
    // the account of the first Pluggy file that was imported.
    if source == pluggy::SOURCE {
        return Ok(pluggy::account_for_file(conn, filename)?.filter(|id| takes_statements(conn, *id)));
    }
    let mut stmt = conn.prepare("SELECT account_id, filename_glob FROM import_profiles WHERE source = ?1 ORDER BY LENGTH(filename_glob) DESC, hits_count DESC")?;
    let rows: Vec<(i64, String)> = stmt.query_map([source], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<std::result::Result<_, _>>()?;
    for (account_id, glob) in rows {
        if !glob.is_empty() && !glob_match(&glob, filename) {
            continue;
        }
        if takes_statements(conn, account_id) {
            return Ok(Some(account_id));
        }
    }
    Ok(None)
}

/// An account a statement may land in: it still exists, it is not a placeholder or closed, and it
/// is one of the two types a bank line belongs to. A file whose account fails this waits as a
/// pending import instead of posting into an account that cannot take it.
fn takes_statements(conn: &Connection, account_id: i64) -> bool {
    match accounts::get_account(conn, account_id) {
        Ok(a) => !a.placeholder && !a.is_closed() && matches!(a.r#type, AccountType::Asset | AccountType::Liability),
        Err(_) => false,
    }
}

/// Remember where this import landed. A bank source gets a source-level default; when the source
/// already lands elsewhere (two accounts at one bank), or the source is a generic CSV, the
/// filename's shape (digit runs become *) tells the files apart.
pub fn learn_profile(conn: &Connection, source: &str, filename: &str, account_id: i64) -> Result<()> {
    if source.is_empty() || source == "account_tracker" || matches!(source, enable_banking::SOURCE | mercury::SOURCE | bank_api::INTER | bank_api::WISE) {
        return Ok(());
    }
    if source == pluggy::SOURCE {
        return pluggy::learn(conn, filename, account_id);
    }
    let ts = now_ts();
    let default: Option<i64> = conn.query_row("SELECT account_id FROM import_profiles WHERE source = ?1 AND filename_glob = ''", [source], |r| r.get(0)).optional()?;
    match default {
        None if source != "generic_csv" => {
            conn.execute("INSERT INTO import_profiles (source, filename_glob, account_id, hits_count, updated_at) VALUES (?1, '', ?2, 1, ?3)", params![source, account_id, ts])?;
            return Ok(());
        }
        Some(acc) if acc == account_id => {
            conn.execute("UPDATE import_profiles SET hits_count = hits_count + 1, updated_at = ?2 WHERE source = ?1 AND filename_glob = ''", params![source, ts])?;
            return Ok(());
        }
        _ => {}
    }
    let glob = glob_of(filename);
    if glob.is_empty() || glob == "*" {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO import_profiles (source, filename_glob, account_id, hits_count, updated_at) VALUES (?1, ?2, ?3, 1, ?4)
         ON CONFLICT (source, filename_glob) DO UPDATE SET account_id = excluded.account_id, hits_count = hits_count + 1, updated_at = excluded.updated_at",
        params![source, glob, account_id, ts],
    )?;
    Ok(())
}

/// "n26-csv-transactions-2026-08.csv" → "n26-csv-transactions-*.csv": digit runs bounded by
/// separators (dates, sequence numbers), and the separators between them, become one *.
/// Digits inside a word ("n26") stay, so a bank's name keeps telling files apart.
fn glob_of(filename: &str) -> String {
    let sep = |c: char| matches!(c, '-' | '_' | ' ' | '.' | '(' | ')');
    let chars: Vec<char> = filename.to_lowercase().chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            let bounded = (start == 0 || sep(chars[start - 1])) && (i == chars.len() || sep(chars[i]));
            if bounded {
                if !out.ends_with('*') {
                    out.push('*');
                }
            } else {
                out.extend(chars[start..i].iter());
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    for pat in ["*-*", "*_*", "* *", "*.*", "**"] {
        while out.contains(pat) {
            out = out.replace(pat, "*");
        }
    }
    out
}

/// Case-insensitive match where * spans anything.
fn glob_match(glob: &str, name: &str) -> bool {
    fn rec(g: &[char], n: &[char]) -> bool {
        match (g.first(), n.first()) {
            (None, None) => true,
            (Some('*'), _) => rec(&g[1..], n) || (!n.is_empty() && rec(g, &n[1..])),
            (Some(c), Some(d)) if c == d => rec(&g[1..], &n[1..]),
            _ => false,
        }
    }
    let g: Vec<char> = glob.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    rec(&g, &n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_from_filenames() {
        assert_eq!(glob_of("n26-csv-transactions-2026-08.csv"), "n26-csv-transactions-*.csv");
        assert_eq!(glob_of("Extrato-01-06-2026-a-30-06-2026.ofx"), "extrato-*-a-*.ofx");
        assert_eq!(glob_of("statement 2026.csv"), "statement *.csv");
        assert_eq!(glob_of("Extrato01234.ofx"), "extrato01234.ofx", "digits inside a word stay");
        assert!(glob_match("n26-csv-transactions-*.csv", "N26-CSV-Transactions-2027-01.CSV"));
        assert!(!glob_match("n26-*.csv", "revolut-2026.csv"));
    }
}
