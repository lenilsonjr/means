//! `means`: books that survive an audit. One binary: engine, terminal UI, CLI.

mod budgets;
mod connections;
mod email;
mod enable_banking;
mod grpc;
mod inter_pj;
mod local_api;
mod mercury;
mod payees;
mod refunds;
mod reports;
mod sharing;
mod statement_http;
mod void;
mod wise;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use clap::{Args, Parser, Subcommand};
use means_core::Db;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const ECB_HIST_URL: &str = "https://www.ecb.europa.eu/stats/eurofxref/eurofxref-hist.xml";

#[derive(Parser)]
#[command(name = "means", version, about = "Books that survive an audit: a local accounting engine with a terminal UI and CLI.")]
struct Cli {
    /// Path of the ledger file (default: ~/.means/ledger.db, or $MEANS_DB)
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Exchange encrypted read-only copies of one vault
    Share(sharing::Cli),
    /// Receive attachments from a dedicated IMAP mailbox
    Email {
        #[command(subcommand)]
        command: email::Command,
    },
    /// Canonical payees, alias previews and explicit historical linking
    Payee {
        #[command(subcommand)]
        command: payees::Command,
    },
    /// Manage spending limits for an inclusive date range
    Budget {
        #[command(subcommand)]
        command: budgets::Command,
    },
    /// Report booked expenses by account class, tag or payee
    Report {
        #[command(subcommand)]
        command: reports::Command,
    },
    /// Match a full bank refund to its original expense
    Refund {
        #[command(subcommand)]
        command: refunds::Command,
    },
    /// Pull Mercury depository or IO credit accounts with a read-only API token
    Mercury {
        #[command(subcommand)]
        command: mercury::Command,
    },
    /// Pull Wise business balance statements
    Wise {
        #[command(subcommand)]
        command: wise::Command,
    },
    /// Pull Banco Inter PJ current-account statements
    InterPj {
        #[command(subcommand)]
        command: inter_pj::Command,
    },
    /// Connect personal European bank accounts through Enable Banking
    EnableBanking {
        #[command(subcommand)]
        command: enable_banking::Command,
    },
    /// Export booked balances for Beancount and Fava (all entities)
    Export {
        #[arg(long, value_parser = ["beancount"], default_value = "beancount")]
        format: String,
        /// Write to a new file instead of stdout (refuses to overwrite)
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Run the local gRPC engine for the terminal UI
    Serve {
        /// Address to listen on
        #[arg(long, default_value = "127.0.0.1:7770")]
        listen: SocketAddr,
        /// Folder watched for bank files (default: inbox/ next to the ledger)
        #[arg(long)]
        inbox: Option<PathBuf>,
    },
    /// Terminal UI (talks to a running engine)
    Tui {
        #[arg(long, default_value = "http://127.0.0.1:7770")]
        server: String,
    },
    /// Import a bank export or an Account Tracker backup
    #[command(subcommand_negates_reqs = true)]
    Import {
        /// Source: auto, n26_csv, mercury_csv, revolut_csv, wise_csv, inter_ofx, inter_csv, remessa_csv, pluggy_json, enable_banking_json, mercury_json, generic_csv, account_tracker
        #[arg(long, default_value = "auto")]
        source: String,
        /// Account id the statement belongs to (bank sources)
        #[arg(long)]
        account: Option<i64>,
        /// Default entity id (Account Tracker backups)
        #[arg(long)]
        entity: Option<i64>,
        #[arg(required = true)]
        file: Option<PathBuf>,
        #[command(subcommand)]
        command: Option<ImportCommand>,
    },
    /// Re-match a bank import against captured or migrated entries (merges duplicates, keeps your categories)
    Rematch {
        import_id: i64,
        /// Preview exact, unambiguous overlaps with another bank source
        #[arg(long)]
        cross_source: bool,
        /// Explicit preview (cross-source mode is read-only by default)
        #[arg(long, requires = "cross_source", conflicts_with = "apply")]
        preview: bool,
        /// Apply exactly the cross-source preview identified by this token
        #[arg(long, requires = "cross_source")]
        apply: Option<String>,
    },
    /// Delete a draft; posted and void entries are refused
    Draft {
        #[command(subcommand)]
        command: DraftCommand,
    },
    /// Roll an import back (its entries and lines); --force discards entries edited after the import
    Rollback {
        import_id: i64,
        #[arg(long)]
        force: bool,
    },
    /// Pull from Pluggy (the Brazilian aggregator) into the inbox folder
    Pluggy {
        #[command(subcommand)]
        command: PluggyCommand,
    },
    /// Inspect or delete learned inbox routing profiles (defaults to list)
    Profiles {
        #[command(subcommand)]
        command: Option<ProfilesCommand>,
    },
    /// Exchange rates
    Rates {
        #[command(subcommand)]
        command: RatesCommand,
    },
    /// Entities (sets of books)
    Entity {
        #[command(subcommand)]
        command: EntityCommand,
    },
    /// Chart of accounts: apply a template, or remap old categories into a clean chart
    Chart {
        #[command(subcommand)]
        command: ChartCommand,
    },
    /// List accounts with balances
    Accounts {
        #[arg(long)]
        entity: Option<i64>,
    },
    /// Compare a bank statement with the ledger; inspect unmatched evidence
    Reconcile {
        /// Account id, or an account path when --entity is provided
        #[arg(long)]
        account: String,
        /// Entity id or name (required for an account path)
        #[arg(long)]
        entity: Option<String>,
    },
    /// List journal entries with their postings, newest first
    Entries(EntriesArgs),
    /// Post a draft to one account or split its Uncategorized posting across accounts
    Post {
        id: i64,
        /// Account path, or repeated Account=amount in bank currency; last amount may be omitted
        #[arg(long, required = true)]
        to: Vec<String>,
        /// Payee written on the entry (default: the one it carries)
        #[arg(long)]
        payee: Option<String>,
    },
    /// Preview a posted entry's reversal; --yes applies it (drafts are never deleted)
    Void(void::Args),
    /// Rules: statement-line patterns that post an import to an account
    Rule {
        #[command(subcommand)]
        command: RuleCommand,
    },
    /// Counts and health of the ledger
    Status,
    /// Verify the hash chain of every entity
    Verify,
}

#[derive(Args)]
struct EntriesArgs {
    /// Emit one JSON object per entry; the summary count goes to stderr
    #[arg(long)]
    json: bool,
    /// Entity id or name
    #[arg(long)]
    entity: Option<String>,
    /// Only entries that touch this account path (needs --entity)
    #[arg(long)]
    account: Option<String>,
    /// draft | posted | void
    #[arg(long)]
    status: Option<String>,
    /// Earliest date (2026-01-31)
    #[arg(long)]
    from: Option<String>,
    /// Latest date
    #[arg(long)]
    to: Option<String>,
    /// Text in the payee, description, notes, memos, references or tags
    #[arg(long, default_value = "")]
    search: String,
    #[arg(long, default_value = "50")]
    limit: i64,
}

#[derive(Subcommand)]
enum ProfilesCommand {
    /// List learned source/glob routes with destination accounts and hit counts
    List,
    /// Forget one route; other matching profiles can still route future files
    Delete { id: i64 },
}

#[derive(Subcommand)]
enum RuleCommand {
    /// List the rules of an entity in the order they run
    List {
        /// Entity id or name
        #[arg(long)]
        entity: String,
    },
    /// Add a rule: lines whose description contains the text post to the account
    Add {
        /// Entity id or name
        #[arg(long)]
        entity: String,
        /// Text the statement line's description contains (case-insensitive)
        #[arg(long)]
        contains: String,
        /// Account path the matching lines post to
        #[arg(long)]
        to: String,
        /// Payee written on the entries (default: the --contains text)
        #[arg(long)]
        payee: Option<String>,
        /// Place in the run order (default: after the last rule)
        #[arg(long)]
        position: Option<i32>,
    },
    /// Delete a rule
    Delete { id: i64 },
}

#[derive(Subcommand)]
enum EntityCommand {
    /// Preview a historical accounting-currency change. Native account currencies stay unchanged.
    MigrateCurrency {
        #[arg(long)]
        entity: String,
        #[arg(long)]
        to: String,
        /// Apply the exact confirmation token from a reviewed preview
        #[arg(long, requires = "backup")]
        apply: Option<String>,
        /// New backup file, required for apply; never overwritten
        #[arg(long, requires = "apply")]
        backup: Option<PathBuf>,
        /// Permit the reviewed conversion of locked periods; lock dates stay unchanged
        #[arg(long, requires = "apply")]
        include_locked: bool,
    },
    /// Create an entity
    Add {
        name: String,
        #[arg(long, default_value = "EUR")]
        currency: String,
        #[arg(long, default_value = "person")]
        kind: String,
        #[arg(long, default_value = "")]
        country: String,
    },
    /// List entities
    List,
}

#[derive(Subcommand)]
enum PluggyCommand {
    /// Fetch what is new from Pluggy and write one file per Pluggy account into the inbox folder.
    /// Needs PLUGGY_CLIENT_ID and PLUGGY_CLIENT_SECRET in the environment.
    Pull {
        /// Pluggy item (one connected bank). Without it, every item this ledger has pulled before.
        #[arg(long)]
        item: Option<String>,
        /// Fetch what Pluggy recorded since this date (YYYY-MM-DD) instead of from the stored cursor
        #[arg(long)]
        since: Option<String>,
        /// Keep transactions booked on or after this bank date (YYYY-MM-DD), ignoring the stored cursor
        #[arg(long)]
        booked_from: Option<String>,
        /// Pull only this Pluggy account UUID (not a means account ID)
        #[arg(long)]
        account: Option<String>,
        /// Report what would be fetched; write no file, move no cursor, refresh no item
        #[arg(long)]
        dry_run: bool,
        /// Folder the files land in (default: inbox/ next to the ledger)
        #[arg(long)]
        inbox: Option<PathBuf>,
        /// Pluggy API base: an https URL, or http only to a loopback host (the credentials go to it)
        #[arg(long, default_value = "https://api.pluggy.ai")]
        api: String,
    },
}

#[derive(Subcommand)]
enum RatesCommand {
    /// Fetch ECB reference rates (EUR against USD, BRL, IDR, ...) and revalue postings booked without a rate
    Fetch {
        #[arg(long, default_value = "2020-01-01")]
        from: String,
        /// Store rates without changing existing entry book values
        #[arg(long)]
        no_revalue: bool,
    },
}

#[derive(Subcommand)]
enum ImportCommand {
    /// Inspect stored statement lines and their failure messages without changing the ledger
    Lines {
        id: i64,
        /// Filter by processing status (omit to list all lines)
        #[arg(long, value_parser = ["error", "unmatched", "created", "matched", "duplicate", "skipped"])]
        status: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Retry unfinished or failed bank-import lines using stored evidence
    Retry { id: i64 },
}

#[derive(Subcommand)]
enum DraftCommand {
    /// Delete one draft and retain its statement evidence as unmatched
    Delete { id: i64 },
}

#[derive(Subcommand)]
enum ChartCommand {
    /// Create the accounts of a chart (charts/*.json, or the chart of a remap plan) in an entity; existing accounts are kept and coded
    Apply {
        /// Entity id or name
        #[arg(long)]
        entity: String,
        file: PathBuf,
    },
    /// Apply a remap plan (charts/remap/*.json): create the target chart and merge the old accounts into it, atomically
    Remap {
        /// Entity id or name (default: every entity the plan names)
        #[arg(long)]
        entity: Option<String>,
        /// Show what would happen without touching the ledger
        #[arg(long)]
        dry_run: bool,
        file: PathBuf,
    },
    /// Create one account, missing parents become placeholders: chart new --entity Personal "Expenses:Food:Ramen" --code 6215
    New {
        /// Entity id or name
        #[arg(long)]
        entity: String,
        /// Full path; the root name (Income, Expenses, ...) is optional
        path: String,
        #[arg(long, default_value = "")]
        code: String,
        /// asset | liability | equity | income | expense
        #[arg(long, default_value = "expense")]
        kind: String,
    },
    /// Rename an account; children keep their place
    Rename {
        /// Entity id or name
        #[arg(long)]
        entity: String,
        path: String,
        new_name: String,
    },
    /// Set the account's code ("" clears it)
    Code {
        /// Entity id or name
        #[arg(long)]
        entity: String,
        path: String,
        code: String,
    },
    /// Move an account under a new parent of the same type; without --under it moves to the top level
    Move {
        /// Entity id or name
        #[arg(long)]
        entity: String,
        path: String,
        #[arg(long)]
        under: Option<String>,
    },
    /// Merge one account into another: postings, rules and template lines follow, the source goes away
    Merge {
        /// Entity id or name
        #[arg(long)]
        entity: String,
        source: String,
        target: String,
    },
    /// Set the class of an expense account: fixed | committed | discretionary | savings | not-spending | ""
    Class {
        /// Entity id or name
        #[arg(long)]
        entity: String,
        path: String,
        class: String,
    },
    /// Move postings between accounts by the entry's payee, tagging them; one hash-chain recompute
    Recat {
        /// Entity id or name
        #[arg(long)]
        entity: String,
        from: String,
        to: String,
        /// Exact payee (case-insensitive). Without --payee and --blank, every posting moves.
        #[arg(long)]
        payee: Option<String>,
        /// Only postings whose entry has a blank payee
        #[arg(long)]
        blank: bool,
        /// Tags added to every moved entry, space-separated key:value
        #[arg(long, default_value = "")]
        tag: String,
    },
    /// Add tags to every entry that touches an account
    Tag {
        /// Entity id or name
        #[arg(long)]
        entity: String,
        path: String,
        tag: String,
    },
    /// Make an empty account a group (placeholder), or a leaf again with --leaf
    Group {
        /// Entity id or name
        #[arg(long)]
        entity: String,
        path: String,
        #[arg(long)]
        leaf: bool,
    },
    /// Set the screen order of top-level expense groups: first name gets position 10, then 20, ...
    Order {
        /// Entity id or name
        #[arg(long)]
        entity: String,
        names: Vec<String>,
    },
    /// Close an empty-of-children account (history stays; it leaves the pickers)
    Close {
        /// Entity id or name
        #[arg(long)]
        entity: String,
        path: String,
    },
}

/// A chart template file, or a remap plan file (one plan per entity).
#[derive(serde::Deserialize)]
struct ChartFile {
    #[serde(default)]
    name: String,
    #[serde(default)]
    accounts: Vec<means_core::accounts::ChartNode>,
    #[serde(default)]
    plans: Vec<PlanFile>,
}

#[derive(serde::Deserialize, Clone)]
struct PlanFile {
    entity: String,
    #[serde(default)]
    chart: Vec<means_core::accounts::ChartNode>,
    #[serde(default)]
    moves: Vec<PlanMove>,
}

#[derive(serde::Deserialize, Clone)]
struct PlanMove {
    #[serde(rename = "type")]
    kind: String,
    from: String,
    to: String,
}

fn find_account(conn: &means_core::rusqlite::Connection, entity_id: i64, path: &str) -> Result<means_core::model::Account> {
    means_core::accounts::find_by_path(conn, entity_id, path)?.ok_or_else(|| anyhow::anyhow!("no account {path} in this entity (paths look like Expenses:Food:Groceries; the root name is optional)"))
}

fn resolve_entity(conn: &means_core::rusqlite::Connection, key: &str) -> Result<means_core::model::Entity> {
    let all = means_core::entities::list_entities(conn, true)?;
    if let Ok(id) = key.trim().parse::<i64>() {
        if let Some(e) = all.iter().find(|e| e.id == id) {
            return Ok(e.clone());
        }
    }
    all.into_iter().find(|e| e.name.eq_ignore_ascii_case(key.trim())).ok_or_else(|| anyhow::anyhow!("no entity named {key}"))
}

fn chart(db_path: PathBuf, command: ChartCommand) -> Result<()> {
    let db = Db::open(&db_path)?;
    let mut conn = db.conn();
    match command {
        ChartCommand::Apply { entity, file } => {
            let f: ChartFile = serde_json::from_str(&std::fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?)?;
            let e = resolve_entity(&conn, &entity)?;
            let nodes = if !f.accounts.is_empty() {
                f.accounts
            } else {
                f.plans
                    .iter()
                    .find(|p| p.entity.eq_ignore_ascii_case(&e.name))
                    .map(|p| p.chart.clone())
                    .ok_or_else(|| anyhow::anyhow!("{} has no accounts and no plan for {}", file.display(), e.name))?
            };
            let (created, existing) = means_core::accounts::apply_chart(&mut conn, e.id, &nodes)?;
            println!("{}: {} accounts created, {} already there{}", e.name, created, existing, if f.name.is_empty() { String::new() } else { format!(" ({})", f.name) });
            Ok(())
        }
        ChartCommand::New { entity, path, code, kind } => {
            let e = resolve_entity(&conn, &entity)?;
            let t = means_core::model::AccountType::parse(&kind)?;
            let mut segments: Vec<&str> = path.split(':').map(|x| x.trim()).filter(|x| !x.is_empty()).collect();
            if let Some(first) = segments.first() {
                if first.eq_ignore_ascii_case(t.root_name()) {
                    segments.remove(0);
                }
            }
            if segments.is_empty() {
                anyhow::bail!("give the account a name");
            }
            let subtype = match t {
                means_core::model::AccountType::Income => "income",
                means_core::model::AccountType::Expense => "expense",
                means_core::model::AccountType::Asset => "bank",
                means_core::model::AccountType::Liability => "card",
                means_core::model::AccountType::Equity => "equity",
            };
            let a = means_core::accounts::ensure_account(&conn, e.id, t, &segments, subtype, &e.currency)?;
            if !code.trim().is_empty() {
                means_core::accounts::update_account(&conn, a.id, means_core::accounts::AccountUpdate { code: Some(code.clone()), ..Default::default() })?;
            }
            println!("{} #{}{}", a.path, a.id, if code.trim().is_empty() { String::new() } else { format!(" · code {}", code.trim()) });
            Ok(())
        }
        ChartCommand::Rename { entity, path, new_name } => {
            let e = resolve_entity(&conn, &entity)?;
            let a = find_account(&conn, e.id, &path)?;
            let up = means_core::accounts::update_account(&conn, a.id, means_core::accounts::AccountUpdate { name: Some(new_name), ..Default::default() })?;
            println!("{} -> {}", a.path, up.path);
            Ok(())
        }
        ChartCommand::Code { entity, path, code } => {
            let e = resolve_entity(&conn, &entity)?;
            let a = find_account(&conn, e.id, &path)?;
            let up = means_core::accounts::update_account(&conn, a.id, means_core::accounts::AccountUpdate { code: Some(code.clone()), ..Default::default() })?;
            println!("{}: code {}", up.path, if code.trim().is_empty() { "(cleared)".to_string() } else { code });
            Ok(())
        }
        ChartCommand::Move { entity, path, under } => {
            let e = resolve_entity(&conn, &entity)?;
            let a = find_account(&conn, e.id, &path)?;
            let parent = match &under {
                Some(p) => Some(find_account(&conn, e.id, p)?.id),
                None => None,
            };
            let up = means_core::accounts::update_account(&conn, a.id, means_core::accounts::AccountUpdate { parent_id: Some(parent), ..Default::default() })?;
            println!("{} -> {}", a.path, up.path);
            Ok(())
        }
        ChartCommand::Merge { entity, source, target } => {
            let e = resolve_entity(&conn, &entity)?;
            let sa = find_account(&conn, e.id, &source)?;
            let ta = find_account(&conn, e.id, &target)?;
            let r = means_core::accounts::merge_accounts(&mut conn, sa.id, ta.id)?;
            println!(
                "{} -> {}: {} postings, {} rules, {} template lines moved{}",
                sa.path,
                ta.path,
                r.moved_postings,
                r.moved_rules,
                r.moved_template_lines,
                if r.source_deleted { "; the source is gone" } else { "; the source stays as a placeholder (it has children)" }
            );
            Ok(())
        }
        ChartCommand::Class { entity, path, class } => {
            let e = resolve_entity(&conn, &entity)?;
            let a = find_account(&conn, e.id, &path)?;
            let up = means_core::accounts::update_account(&conn, a.id, means_core::accounts::AccountUpdate { class: Some(class.clone()), ..Default::default() })?;
            println!("{}: class {}", up.path, if up.class.is_empty() { "(none)".into() } else { up.class });
            Ok(())
        }
        ChartCommand::Recat { entity, from, to, payee, blank, tag } => {
            let e = resolve_entity(&conn, &entity)?;
            let fa = find_account(&conn, e.id, &from)?;
            let ta = find_account(&conn, e.id, &to)?;
            let owned;
            let filter: Option<&str> = if blank {
                Some("")
            } else {
                match &payee {
                    Some(p) => {
                        owned = p.clone();
                        Some(owned.as_str())
                    }
                    None => None,
                }
            };
            let r = means_core::accounts::recategorize(&mut conn, fa.id, ta.id, filter, &tag)?;
            println!("{} -> {}: {} postings in {} entries{}", fa.path, ta.path, r.postings, r.entries, if tag.is_empty() { String::new() } else { format!(" · tagged {tag}") });
            Ok(())
        }
        ChartCommand::Tag { entity, path, tag } => {
            let e = resolve_entity(&conn, &entity)?;
            let a = find_account(&conn, e.id, &path)?;
            let n = means_core::accounts::tag_account(&mut conn, a.id, &tag)?;
            println!("{}: {} entries tagged {}", a.path, n, tag);
            Ok(())
        }
        ChartCommand::Group { entity, path, leaf } => {
            let e = resolve_entity(&conn, &entity)?;
            let a = find_account(&conn, e.id, &path)?;
            let up = means_core::accounts::update_account(&conn, a.id, means_core::accounts::AccountUpdate { placeholder: Some(!leaf), ..Default::default() })?;
            println!("{}: {}", up.path, if up.placeholder { "group" } else { "leaf" });
            Ok(())
        }
        ChartCommand::Order { entity, names } => {
            let e = resolve_entity(&conn, &entity)?;
            let all = means_core::accounts::list_accounts(&conn, Some(e.id), true)?;
            for (i, name) in names.iter().enumerate() {
                let Some(a) = all.iter().find(|a| a.parent_id.is_none() && a.r#type == means_core::model::AccountType::Expense && a.name.eq_ignore_ascii_case(name.trim())) else {
                    println!("  {name}: no top-level expense group with this name, skipped");
                    continue;
                };
                means_core::accounts::update_account(&conn, a.id, means_core::accounts::AccountUpdate { position: Some(((i + 1) * 10) as i32), ..Default::default() })?;
            }
            println!("{}: {} groups ordered", e.name, names.len());
            Ok(())
        }
        ChartCommand::Close { entity, path } => {
            let e = resolve_entity(&conn, &entity)?;
            let a = find_account(&conn, e.id, &path)?;
            let up = means_core::accounts::close_account(&conn, a.id, false)?;
            println!("{} closed at {}", up.path, up.closed_at.unwrap_or_default());
            Ok(())
        }
        ChartCommand::Remap { entity, dry_run, file } => {
            let f: ChartFile = serde_json::from_str(&std::fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?)?;
            if f.plans.is_empty() {
                anyhow::bail!("{} holds no plans", file.display());
            }
            let only = entity.as_deref().map(|k| resolve_entity(&conn, k)).transpose()?;
            for plan in &f.plans {
                let e = match resolve_entity(&conn, &plan.entity) {
                    Ok(e) => e,
                    Err(_) => {
                        println!("{}: no such entity here, skipped", plan.entity);
                        continue;
                    }
                };
                if let Some(o) = &only {
                    if o.id != e.id {
                        continue;
                    }
                }
                let accounts = means_core::accounts::list_accounts(&conn, Some(e.id), true)?;
                let mut moves: Vec<(i64, String)> = Vec::new();
                println!("{} ({} target accounts):", e.name, plan.chart.len());
                for m in &plan.moves {
                    match accounts.iter().find(|a| a.r#type.as_str() == m.kind && a.name.trim() == m.from.trim() && a.parent_id.is_none()) {
                        Some(a) => {
                            println!("  {:<44} -> {}", a.path, m.to);
                            moves.push((a.id, m.to.clone()));
                        }
                        None => println!("  {:<44}    (not found, skipped)", format!("{}: {}", m.kind, m.from.trim())),
                    }
                }
                if dry_run {
                    println!("  dry run: {} merges would apply", moves.len());
                    continue;
                }
                let r = means_core::accounts::remap_accounts(&mut conn, e.id, &plan.chart, &moves)?;
                for s in &r.skipped {
                    println!("  skipped {s}");
                }
                println!("  done: {} accounts created, {} existing, {} merged, {} postings moved", r.created, r.existing, r.merged, r.moved_postings);
            }
            Ok(())
        }
    }
}

fn reconcile(db_path: PathBuf, account: &str, entity: Option<&str>) -> Result<()> {
    let db = Db::open(&db_path)?;
    let mut conn = db.conn();
    let tx = conn.transaction()?;
    let entity = entity.map(|e| resolve_entity(&tx, e)).transpose()?;
    let account = match account.parse::<i64>() {
        Ok(id) => means_core::accounts::get_account(&tx, id)?,
        Err(_) => find_account(&tx, entity.as_ref().context("an account path needs --entity; otherwise use --account ID")?.id, account)?,
    };
    if entity.as_ref().is_some_and(|e| e.id != account.entity_id) {
        anyhow::bail!("account #{} does not belong to the selected entity", account.id);
    }
    let report = means_core::reports::reconciliation(&tx, account.id)?;
    tx.commit()?;
    let fmt = |v| means_core::money::fmt(v, account.precision);
    println!("#{} {} ({})", account.id, account.path, account.commodity);
    match report.statement_balance {
        Some(balance) => println!("Statement balance: {} {} (date: {})", fmt(balance), account.commodity, report.statement_date.map(|d| d.to_string()).unwrap_or_else(|| "unknown".into())),
        None => println!("Statement balance: unavailable (no statement closing balance)"),
    }
    println!("Ledger balance: {} {} (as of {})", fmt(report.ledger_balance), account.commodity, report.statement_date.map(|d| d.to_string()).unwrap_or_else(|| "all booked dates".into()));
    match report.difference {
        Some(difference) => println!("Difference (statement - ledger): {} {}", fmt(difference), account.commodity),
        None => println!("Difference: unavailable"),
    }
    println!("Unmatched statement lines: {} shown (report limit: 500)", report.unmatched_lines.len());
    for line in &report.unmatched_lines {
        println!(
            "  line #{} {} {} {} {}",
            line.id,
            line.date.map(|d| d.to_string()).unwrap_or_else(|| "undated".into()),
            line.amount.map(|a| fmt(a.major())).unwrap_or_else(|| "unknown".into()),
            account.commodity,
            line.description
        );
    }
    println!("Unreconciled postings: {} shown (from up to 5000 ledger rows)", report.unreconciled.len());
    for row in &report.unreconciled {
        println!(
            "  posting #{} entry #{} {} debit {} credit {} {} {} {}",
            row.posting_id,
            row.journal_entry_id,
            row.date,
            fmt(row.debit.major()),
            fmt(row.credit.major()),
            account.commodity,
            row.payee,
            row.description
        );
    }
    Ok(())
}

fn entries(db_path: PathBuf, a: EntriesArgs) -> Result<()> {
    let db = Db::open(&db_path)?;
    let conn = db.conn();
    let e = a.entity.as_deref().map(|k| resolve_entity(&conn, k)).transpose()?;
    let account_id = match &a.account {
        Some(p) => Some(find_account(&conn, e.as_ref().context("--account needs --entity: an account path is read inside one entity")?.id, p)?.id),
        None => None,
    };
    let filter = means_core::journal::EntryFilter {
        entity_id: e.as_ref().map(|e| e.id),
        account_id,
        status: a.status.as_deref().map(means_core::model::EntryStatus::parse).transpose()?,
        from: a.from.as_deref().map(means_core::parse_date).transpose()?,
        to: a.to.as_deref().map(means_core::parse_date).transpose()?,
        query: a.search,
        limit: a.limit,
        ..Default::default()
    };
    let (list, total) = means_core::journal::list_entries(&conn, &filter)?;
    if a.json {
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        for entry in &list {
            serde_json::to_writer(&mut stdout, entry)?;
            writeln!(stdout)?;
        }
        eprintln!("{} of {} entries", list.len(), total);
        return Ok(());
    }
    for j in &list {
        println!("#{:<6} {} {:<6} {}", j.id, j.date, j.status.as_str(), j.display_payee);
        for p in &j.postings {
            println!(
                "         {:<48} {:>14} {}{}",
                p.account_path,
                means_core::money::fmt(p.quantity.major(), p.quantity.precision()),
                p.quantity.commodity(),
                if p.memo.is_empty() { String::new() } else { format!("  {}", p.memo) }
            );
        }
    }
    println!("{} of {} entries", list.len(), total);
    Ok(())
}

/// The account `--to` names, refused by an error that names the entry when the path is unknown
/// or belongs to another set of books.
fn target_account(conn: &means_core::rusqlite::Connection, entry: &means_core::model::JournalEntry, path: &str) -> Result<means_core::model::Account> {
    if let Some(a) = means_core::accounts::find_by_path(conn, entry.entity_id, path)? {
        return Ok(a);
    }
    let entity = means_core::entities::get_entity(conn, entry.entity_id)?;
    for other in means_core::entities::list_entities(conn, true)? {
        if other.id != entity.id && means_core::accounts::find_by_path(conn, other.id, path)?.is_some() {
            anyhow::bail!("#{} is in {}, and {path} is an account of {}", entry.id, entity.name, other.name);
        }
    }
    anyhow::bail!("#{} is in {}, which has no account {path} (paths look like Expenses:Food:Groceries; the root name is optional)", entry.id, entity.name);
}

fn post(db_path: PathBuf, id: i64, to: &[String], payee: Option<String>) -> Result<()> {
    let db = Db::open(&db_path)?;
    let mut conn = db.conn();
    let entry = means_core::journal::get_entry(&conn, id)?;
    if entry.status != means_core::model::EntryStatus::Draft {
        anyhow::bail!("#{id} is {}, not a draft", entry.status.as_str());
    }
    let legs = to
        .iter()
        .map(|spec| {
            let (path, amount) = spec.rsplit_once('=').map_or((spec.as_str(), None), |(path, amount)| (path, Some(amount)));
            let target = target_account(&conn, &entry, path.trim())?;
            let quantity = amount.filter(|s| !s.trim().is_empty()).map(|s| s.trim().parse::<rust_decimal::Decimal>().context("invalid split amount")).transpose()?;
            Ok(means_core::splits::Leg { account_id: target.id, quantity, memo: String::new() })
        })
        .collect::<Result<Vec<_>>>()?;
    let posted = means_core::journal::post_draft_to(&mut conn, id, &legs, payee.as_deref()).with_context(|| format!("#{id} cannot be posted"))?;
    println!("#{} {} {} {} -> {}", posted.id, posted.date, posted.status.as_str(), posted.payee, to.join(", "));
    Ok(())
}

fn rule(db_path: PathBuf, command: RuleCommand) -> Result<()> {
    let db = Db::open(&db_path)?;
    let conn = db.conn();
    match command {
        RuleCommand::List { entity } => {
            let e = resolve_entity(&conn, &entity)?;
            for r in means_core::rules::list_rules(&conn, Some(e.id))? {
                let conds: Vec<String> = r.conditions.iter().map(|c| format!("{} {} {:?}", c.field, c.op, c.value)).collect();
                let target = match r.account_id {
                    Some(a) => means_core::accounts::get_account(&conn, a)?.path,
                    None => format!("template #{}", r.template_id.unwrap_or(0)),
                };
                println!("#{:<4} {:>4} {:<32} {} -> {} ({} hits){}", r.id, r.position, r.payee, conds.join(" and "), target, r.hits_count, if r.enabled { "" } else { " disabled" });
            }
            Ok(())
        }
        RuleCommand::Add { entity, contains, to, payee, position } => {
            let e = resolve_entity(&conn, &entity)?;
            let target = find_account(&conn, e.id, &to)?;
            let payee = payee.unwrap_or_else(|| contains.clone());
            let rule = means_core::model::Rule {
                id: 0,
                entity_id: e.id,
                name: payee.clone(),
                // save_rule reads position 0 as MAX(position) + 10, so no --position appends.
                position: position.unwrap_or(0),
                enabled: true,
                // rules::matches lowercases the line and the condition: the text is stored as typed.
                conditions: vec![means_core::model::RuleCondition { field: "description".into(), op: "contains".into(), value: contains }],
                account_id: Some(target.id),
                template_id: None,
                payee,
                hits_count: 0,
                created_at: String::new(),
                tags: String::new(),
            };
            let r = means_core::rules::save_rule(&conn, &rule)?;
            println!("rule #{} {} -> {} (position {})", r.id, r.payee, target.path, r.position);
            Ok(())
        }
        RuleCommand::Delete { id } => {
            let r = means_core::rules::get_rule(&conn, id)?;
            means_core::rules::delete_rule(&conn, id)?;
            println!("rule #{} {} deleted", r.id, r.payee);
            Ok(())
        }
    }
}

fn profiles(db_path: PathBuf, command: Option<ProfilesCommand>) -> Result<()> {
    let db = Db::open(&db_path)?;
    let mut conn = db.conn();
    match command.unwrap_or(ProfilesCommand::List) {
        ProfilesCommand::List => {
            let profiles = means_core::imports::inbox::list_profiles(&conn)?;
            if profiles.is_empty() {
                println!("No learned inbox profiles.");
            }
            for p in profiles {
                let account = means_core::accounts::get_account(&conn, p.account_id)?;
                let entity = means_core::entities::get_entity(&conn, account.entity_id)?;
                let glob = if p.filename_glob.is_empty() { "(all filenames)" } else { &p.filename_glob };
                println!(
                    "#{} {} {} -> {} / {} (account #{}, {} hits){}",
                    p.id,
                    p.source,
                    glob,
                    entity.name,
                    account.path,
                    account.id,
                    p.hits_count,
                    if account.is_closed() { " [closed]" } else { "" }
                );
            }
        }
        ProfilesCommand::Delete { id } => {
            let p = means_core::imports::inbox::delete_profile(&mut conn, id)?;
            println!("Deleted inbox profile #{} ({}). Other matching profiles still apply; past imports are unchanged.", p.id, p.source);
        }
    }
    Ok(())
}

fn default_db() -> PathBuf {
    if let Ok(p) = std::env::var("MEANS_DB") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
    home.join(".means").join("ledger.db")
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,h2=warn,hyper=warn".into())).init();
    let cli = Cli::parse();
    let db_path = cli.db.unwrap_or_else(default_db);
    if db_path.exists() {
        let c = means_core::rusqlite::Connection::open_with_flags(&db_path, means_core::rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        anyhow::ensure!(!means_core::db::replica_marker(&c)?, "received vault is read-only; use means share view with its grant ID");
    }
    match cli.command.unwrap_or(Command::Serve { listen: "127.0.0.1:7770".parse().unwrap(), inbox: None }) {
        Command::Share(args) => sharing::run(db_path, args).await,
        Command::Export { format: _, output } => {
            use std::io::Write;
            let db = Db::open(&db_path)?;
            let document = means_core::export::beancount(&mut db.conn())?;
            match output {
                Some(path) => {
                    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).with_context(|| format!("create {}", path.display()))?;
                    file.write_all(document.as_bytes())?;
                }
                None => std::io::stdout().lock().write_all(document.as_bytes())?,
            }
            Ok(())
        }
        Command::Serve { listen, inbox } => serve(db_path, listen, inbox).await,
        Command::Tui { server } => means_tui::run(&server).await,
        Command::Import { source, account, entity, file, command } => match command {
            Some(ImportCommand::Lines { id, status, json }) => {
                let conn = means_core::rusqlite::Connection::open_with_flags(&db_path, means_core::rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
                let (_, lines) = means_core::imports::get_import(&conn, id)?;
                let lines: Vec<_> = lines.into_iter().filter(|line| status.as_deref().is_none_or(|status| line.status == status)).collect();
                if json {
                    println!("{}", serde_json::to_string_pretty(&lines)?);
                } else {
                    println!("import #{id}: {} {}lines", lines.len(), status.as_ref().map(|s| format!("{s} ")).unwrap_or_default());
                    for line in lines {
                        println!(
                            "#{}  {}  {}  {}  {}",
                            line.id,
                            line.status,
                            line.date.map(|d| d.to_string()).unwrap_or_default(),
                            line.amount.map(|m| format!("{} {}", means_core::money::fmt(m.major(), m.precision()), m.commodity())).unwrap_or_default(),
                            line.description.escape_default()
                        );
                        if !line.reference.is_empty() {
                            println!("  reference: {}", line.reference.escape_default());
                        }
                        if !line.note.is_empty() {
                            println!("  note: {}", line.note.escape_default());
                        }
                    }
                }
                Ok(())
            }
            Some(ImportCommand::Retry { id }) => {
                let db = Db::open(&db_path)?;
                let i = means_core::imports::retry_import(&mut db.conn(), id)?;
                println!(
                    "import #{}: {} created, {} matched, {} duplicates, {} skipped, {} unfinished, {} errors",
                    i.id, i.created_count, i.matched_count, i.duplicate_count, i.skipped_count, i.unmatched_count, i.error_count
                );
                if i.error_count > 0 {
                    println!("  inspect failures: means import lines {} --status error", i.id);
                }
                if let Some(warning) = i.options.get("coverage_warning").and_then(|v| v.as_str()) {
                    println!("  warning: {warning}");
                }
                Ok(())
            }
            None => import(db_path, &source, account, entity, &file.context("an import file is required")?),
        },
        Command::Draft { command: DraftCommand::Delete { id } } => {
            let db = Db::open(&db_path)?;
            means_core::journal::delete_entry(&mut db.conn(), id)?;
            println!("deleted draft #{id}; any statement evidence remains unmatched");
            Ok(())
        }
        Command::Rematch { import_id, cross_source, preview: _, apply } => {
            let db = Db::open(&db_path)?;
            let mut conn = db.conn();
            if cross_source {
                let r = means_core::cross_source::rematch(&mut conn, import_id, apply.as_deref())?;
                println!("{}: {} cross-source matches for import #{import_id}", if r.applied { "applied" } else { "preview" }, r.matches.len());
                for m in &r.matches {
                    let action = if let Some(id) = m.voided_entry_id {
                        format!("void posted entry #{id} with a reversal on {}", m.date)
                    } else if let Some(id) = m.removed_draft_id {
                        format!("delete draft #{id}")
                    } else {
                        "attach evidence only".into()
                    };
                    println!(
                        "  line #{}: {} {} {} {} -> keep entry #{} {} on {} ({} days apart; posting #{}); {}",
                        m.line_id, m.date, m.amount, m.currency, m.description, m.kept_entry_id, m.kept_payee, m.kept_date, m.days_apart, m.posting_id, action
                    );
                }
                for note in &r.notes {
                    println!("  note: {note}");
                }
                if !r.applied && !r.matches.is_empty() {
                    println!("Review these matches. On the same --db, confirm with: means rematch {import_id} --cross-source --apply {}", r.token);
                }
                return Ok(());
            }
            let r = means_core::imports::rematch_import(&mut conn, import_id)?;
            println!("import #{import_id}: {} lines, {} re-matched to existing entries ({} kept their category), {} untouched", r.lines, r.rematched, r.category_kept, r.untouched);
            for n in &r.notes {
                println!("  note: {n}");
            }
            Ok(())
        }
        Command::Rollback { import_id, force } => {
            let db = Db::open(&db_path)?;
            let mut conn = db.conn();
            means_core::imports::delete_import(&mut conn, import_id, force)?;
            println!("import #{import_id} rolled back");
            Ok(())
        }
        Command::Email { command } => email::run(db_path, command).await,
        Command::Report { command } => reports::run(db_path, command),
        Command::Payee { command } => payees::run(db_path, command),
        Command::Budget { command } => budgets::run(db_path, command),
        Command::Refund { command } => refunds::run(db_path, command),
        Command::Mercury { command } => mercury::run(db_path, command).await,
        Command::Wise { command } => wise::run(db_path, command).await,
        Command::InterPj { command } => inter_pj::run(db_path, command).await,
        Command::EnableBanking { command } => enable_banking::run(db_path, command).await,
        Command::Pluggy { command: PluggyCommand::Pull { item, since, booked_from, account, dry_run, inbox, api } } => {
            let options = means_core::imports::pluggy::PullOptions {
                base_url: api,
                item,
                account,
                dry_run,
                since: since.as_deref().map(means_core::parse_date).transpose()?,
                booked_from: booked_from.as_deref().map(means_core::parse_date).transpose()?,
                inbox: inbox.unwrap_or_else(|| db_path.parent().map(|p| p.join("inbox")).unwrap_or_else(|| PathBuf::from("inbox"))),
                ..Default::default()
            };
            // A thread of its own: the pull is synchronous, and blocking inside the CLI's runtime is not allowed.
            let done = std::thread::spawn(move || pluggy_pull(db_path, options));
            done.join().map_err(|_| anyhow::anyhow!("the pluggy pull thread panicked"))?
        }
        Command::Rates { command: RatesCommand::Fetch { from, no_revalue } } => {
            let db = Db::open(&db_path)?;
            let from = means_core::parse_date(&from)?;
            let (inserted, revalued, latest) = fetch_rates_with_revaluation(&db, Some(from), !no_revalue).await?;
            println!("rates: {inserted} written, {revalued} entries revalued, latest {latest}");
            Ok(())
        }
        Command::Entity { command: EntityCommand::MigrateCurrency { entity, to, apply, backup, include_locked } } => {
            let mut conn = means_core::currency_migration::open_existing(&db_path, false)?;
            let entity = resolve_entity(&conn, &entity)?;
            let applying = apply.is_some();
            let result = if let Some(token) = apply {
                drop(conn);
                means_core::currency_migration::apply_file(&db_path, entity.id, &to, &token, &backup.context("--backup is required")?, include_locked)?
            } else {
                means_core::currency_migration::preview(&mut conn, entity.id, &to)?
            };
            println!("{}", serde_json::to_string_pretty(&result)?);
            eprintln!("{}", if applying { "Currency migration applied; the verified backup contains the old books." } else { "Preview only. Review the JSON before applying its confirmation token." });
            Ok(())
        }
        Command::Entity { command: EntityCommand::Add { name, currency, kind, country } } => {
            let db = Db::open(&db_path)?;
            let mut conn = db.conn();
            let e = means_core::entities::create_entity(&mut conn, &name, &kind, &country, &currency)?;
            println!("entity #{} {} ({}, {})", e.id, e.name, e.kind, e.currency);
            Ok(())
        }
        Command::Entity { command: EntityCommand::List } => {
            let db = Db::open(&db_path)?;
            let conn = db.conn();
            for e in means_core::entities::list_entities(&conn, true)? {
                println!("#{:<3} {:<24} {:<8} {}{}", e.id, e.name, e.kind, e.currency, if e.archived_at.is_some() { " (archived)" } else { "" });
            }
            Ok(())
        }
        Command::Chart { command } => chart(db_path, command),
        Command::Accounts { entity } => {
            let db = Db::open(&db_path)?;
            let conn = db.conn();
            for a in means_core::accounts::list_accounts_with_balances(&conn, entity, false, None)? {
                if a.placeholder {
                    println!("{:<48} {:>16}", a.path, "");
                } else {
                    println!("{:<48} {:>16} {}", a.path, means_core::money::fmt(a.balance, a.precision), a.commodity);
                }
            }
            Ok(())
        }
        Command::Reconcile { account, entity } => reconcile(db_path, &account, entity.as_deref()),
        Command::Entries(args) => entries(db_path, args),
        Command::Post { id, to, payee } => post(db_path, id, &to, payee),
        Command::Void(args) => void::run(db_path, args),
        Command::Rule { command } => rule(db_path, command),
        Command::Profiles { command } => profiles(db_path, command),
        Command::Status => {
            let db = Db::open(&db_path)?;
            let s = grpc::status_of(&db)?;
            println!("{}", serde_json::to_string_pretty(&s)?);
            Ok(())
        }
        Command::Verify => {
            let db = Db::open(&db_path)?;
            let conn = db.conn();
            for e in means_core::entities::list_entities(&conn, true)? {
                let r = means_core::hashchain::verify(&conn, e.id)?;
                match r.first_bad_seq {
                    None => println!("{}: {} entries verified, head {}", e.name, r.checked, r.head.unwrap_or_default()),
                    Some(seq) => println!("{}: chain BROKEN at seq {seq} (entry #{})", e.name, r.first_bad_entry_id.unwrap_or(0)),
                }
            }
            Ok(())
        }
    }
}

async fn serve(db_path: PathBuf, listen: SocketAddr, inbox: Option<PathBuf>) -> Result<()> {
    anyhow::ensure!(listen.ip().is_loopback(), "means serves local clients only; --listen must use a loopback address");
    let listener = tokio::net::TcpListener::bind(listen).await.with_context(|| format!("bind {listen}"))?;
    let listen = listener.local_addr()?;
    let access = Arc::new(local_api::LocalAccess::new(listen));
    let db = Arc::new(Db::open(&db_path).with_context(|| format!("open {}", db_path.display()))?);
    tracing::info!(db = %db_path.display(), "ledger opened");
    let inbox_dir = inbox.unwrap_or_else(|| db_path.parent().map(|p| p.join("inbox")).unwrap_or_else(|| PathBuf::from("inbox")));
    std::fs::create_dir_all(&inbox_dir).with_context(|| format!("create {}", inbox_dir.display()))?;
    watch_inbox(db.clone(), inbox_dir.clone());
    let service = grpc::MeansService::new(db, inbox_dir.clone())?;
    let grpc = means_proto::v1::means_server::MeansServer::new(service).max_decoding_message_size(128 * 1024 * 1024).max_encoding_message_size(128 * 1024 * 1024);
    let app: Router =
        tonic::service::Routes::new(grpc).into_axum_router().fallback(|| async { axum::http::StatusCode::NOT_FOUND }).layer(axum::middleware::from_fn_with_state(access, local_api::guard));
    let url = format!("http://{listen}");
    tracing::info!(%url, "means is listening for local gRPC clients");
    println!("means {VERSION}\n  ledger  {}\n  inbox   {}  (bank files dropped here import themselves)\n  grpc    {url}\n  tui     means tui --server {url}", db_path.display(), inbox_dir.display());
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutting down");
        })
        .await?;
    Ok(())
}

/// Every few seconds: new files in the inbox folder become imports (or pending ones).
fn watch_inbox(db: Arc<Db>, dir: PathBuf) {
    std::thread::spawn(move || {
        let mut reported: std::collections::HashSet<String> = std::collections::HashSet::new();
        loop {
            let outcomes = {
                let mut conn = db.conn();
                means_core::imports::inbox::scan(&mut conn, &dir)
            };
            match outcomes {
                Ok(list) => {
                    for o in list {
                        if o.action == "ignored" && !reported.insert(format!("{}|{}", o.file, o.detail)) {
                            continue;
                        }
                        tracing::info!(file = %o.file, action = %o.action, detail = %o.detail, "inbox");
                    }
                }
                Err(e) => tracing::warn!("inbox scan: {e}"),
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
    });
}

fn import(db_path: PathBuf, source: &str, account: Option<i64>, entity: Option<i64>, file: &PathBuf) -> Result<()> {
    let db = Db::open(&db_path)?;
    let content = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let filename = file.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let mut conn = db.conn();
    let source = if source == "auto" { means_core::imports::detect_source(&filename, &content) } else { source.to_string() };
    if source == "account_tracker" {
        let entity = entity.context("--entity is required for Account Tracker backups")?;
        let r = means_core::imports::account_tracker::import(&mut conn, &content, &filename, &[], entity, true, true)?;
        println!("import #{}: {} entries created, {} already present", r.import.id, r.created, r.skipped_existing);
        for w in &r.warnings {
            println!("  warning: {w}");
        }
        for c in &r.checks {
            println!("  {} {} expected {} actual {}", if c.ok { "ok  " } else { "DIFF" }, c.name, c.expected, c.actual);
        }
        return Ok(());
    }
    let out = means_core::imports::run_import(&mut conn, means_core::imports::ImportRequest::new(&source, account, &filename, &content))?;
    let i = &out.import;
    println!(
        "import #{} ({}): {} lines, {} created, {} matched, {} duplicates, {} skipped, {} unmatched, {} errors",
        i.id, i.source, i.lines_count, i.created_count, i.matched_count, i.duplicate_count, i.skipped_count, i.unmatched_count, i.error_count
    );
    if i.error_count > 0 {
        println!("  inspect failures: means import lines {} --status error", i.id);
    }
    for warning in &out.parse.warnings {
        println!("  warning: {warning}");
    }
    Ok(())
}

/// Download the ECB history and store it; then revalue postings booked without a rate.
pub async fn fetch_rates(db: &Db, from: Option<chrono::NaiveDate>) -> Result<(usize, usize, String)> {
    fetch_rates_with_revaluation(db, from, true).await
}

async fn fetch_rates_with_revaluation(db: &Db, from: Option<chrono::NaiveDate>, revalue: bool) -> Result<(usize, usize, String)> {
    let client = reqwest::Client::builder().user_agent(format!("means/{VERSION}")).build()?;
    let xml = client.get(ECB_HIST_URL).send().await?.error_for_status()?.text().await?;
    let mut conn = db.conn();
    let inserted = means_core::rates::import_ecb_xml(&mut conn, &xml, from)?;
    let fixed = if revalue {
        let revalued = means_core::rates::revalue_missing(&mut conn)?;
        if revalued.failed > 0 {
            tracing::warn!(failed = revalued.failed, first = ?revalued.first_error, "some entries could not be revalued");
        }
        revalued.fixed
    } else {
        0
    };
    let (_, latest) = means_core::rates::price_count(&conn)?;
    Ok((inserted, fixed, latest.unwrap_or_default()))
}

/// The Pluggy channel's network half. means-core opens no socket, so the HTTP client lives here
/// behind the channel's `Transport`; Enable Banking will bring its own implementation of the same.
struct PluggyHttp {
    client: reqwest::Client,
    rt: tokio::runtime::Runtime,
}

impl means_core::imports::pluggy::Transport for PluggyHttp {
    fn send(&self, request: means_core::imports::pluggy::Request<'_>) -> means_core::Result<means_core::imports::pluggy::Response> {
        let method = reqwest::Method::from_bytes(request.method.as_bytes()).map_err(|e| means_core::Error::Invalid(format!("{:?} is not an HTTP method: {e}", request.method)))?;
        let mut call = self.client.request(method, request.url).header("accept", "application/json");
        if !request.api_key.is_empty() {
            call = call.header("X-API-KEY", request.api_key);
        }
        if let Some(body) = request.body {
            call = call.header("content-type", "application/json").body(body);
        }
        self.rt.block_on(async {
            let response = call.send().await.map_err(|e| means_core::Error::Invalid(format!("pluggy could not be reached: {e}")))?;
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            Ok(means_core::imports::pluggy::Response { status, body })
        })
    }
}

/// Pull from Pluggy into the inbox folder. The files are all this leaves: the inbox imports them,
/// and a Pluggy account no import has placed yet waits there as a pending import.
fn pluggy_pull(db_path: PathBuf, options: means_core::imports::pluggy::PullOptions) -> Result<()> {
    use means_core::imports::pluggy;
    let db = Db::open(&db_path).with_context(|| format!("open {}", db_path.display()))?;
    let inbox_dir = options.inbox.clone();
    let credentials = pluggy::Credentials::from_env()?;
    let transport = PluggyHttp {
        client: reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).user_agent(format!("means/{VERSION}")).build()?,
        rt: tokio::runtime::Builder::new_current_thread().enable_all().build()?,
    };
    let mut conn = db.conn();
    let report = pluggy::pull(&mut conn, &transport, &credentials, &options)?;
    for i in &report.items {
        println!("item {} ({})", i.id, i.status);
    }
    for a in &report.accounts {
        let placed = match a.account_id.and_then(|id| means_core::accounts::get_account(&conn, id).ok()) {
            Some(account) => account.path,
            None => "no account yet: the inbox will hold the file as pending".to_string(),
        };
        let window = if a.created_at_from.is_empty() { "everything Pluggy holds".to_string() } else { format!("recorded since {}", a.created_at_from) };
        println!(
            "  {} · {} · {} · {} transactions over {} page(s), {window} · {}",
            a.name,
            a.kind,
            a.currency,
            a.transactions,
            a.pages,
            if report.dry_run { "nothing written (dry run)".to_string() } else { a.file.clone() }
        );
        println!("     Pluggy account {} · {placed}", a.provider_account_id);
        if !a.booked_from.is_empty() {
            println!("     booked from {} (inclusive); {} older transactions excluded; recorded-at cursor unchanged", a.booked_from, a.excluded_before_booking_date);
        }
    }
    if !report.dry_run {
        println!("{} file(s) in {}: the inbox imports them", report.accounts.len(), inbox_dir.display());
    }
    Ok(())
}
