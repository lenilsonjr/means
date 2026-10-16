//! `means rates set`, `list` and `import`: exchange rates without a network.
//!
//! A rate row says that one unit of the base costs `rate` units of the quote on a date. The rows
//! go in the prices table, the same table that `means rates fetch` fills. Drafts, imports and
//! revaluations find them through the usual rate lookup. With `--json`, each command prints one
//! JSON object with `"schema_version": 1`.

use std::path::{Path, PathBuf};

use clap::Args;
use means_core::money;
use means_core::proposal::{AgentError, AgentResult};
use means_core::rates;
use serde_json::{json, Value};

use crate::entry_cli::{emit, open, parse_date, Output};

#[derive(Args)]
pub struct SetArgs {
    /// The day of the rate, YYYY-MM-DD
    #[arg(long)]
    date: String,
    /// The currency that the rate prices, for example EUR
    #[arg(long)]
    base: String,
    /// The currency of the price, for example USD
    #[arg(long)]
    quote: String,
    /// Units of the quote for one unit of the base, as a decimal, for example 1.0850
    #[arg(long)]
    rate: String,
    /// Where the rate comes from
    #[arg(long, default_value = "manual")]
    source: String,
    /// Store the rate without changing existing entry or draft book values
    #[arg(long)]
    no_revalue: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
pub struct ListArgs {
    /// Only rows with this base currency
    #[arg(long)]
    base: Option<String>,
    /// Only rows with this quote currency
    #[arg(long)]
    quote: Option<String>,
    /// Earliest day, YYYY-MM-DD
    #[arg(long)]
    from: Option<String>,
    /// Latest day, YYYY-MM-DD
    #[arg(long)]
    to: Option<String>,
    /// Most rows to return, newest first
    #[arg(long, default_value_t = 500)]
    limit: i64,
    /// Show the rate that the lookup finds for one pair, such as USD/SGD: direct, inverse or cross
    #[arg(long, requires = "date", conflicts_with_all = ["base", "quote", "from", "to"])]
    derive: Option<String>,
    /// The day of the derived rate, YYYY-MM-DD (with --derive)
    #[arg(long, requires = "derive")]
    date: Option<String>,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
pub struct ImportArgs {
    /// A CSV file with the header date,base,quote,rate
    file: PathBuf,
    /// The source written on each row
    #[arg(long, default_value = "manual")]
    source: String,
    /// Store the rates without changing existing entry or draft book values
    #[arg(long)]
    no_revalue: bool,
    #[arg(long)]
    json: bool,
}

pub fn set(db_path: PathBuf, args: SetArgs) -> anyhow::Result<()> {
    emit(run_set(&db_path, args))
}

pub fn list(db_path: PathBuf, args: ListArgs) -> anyhow::Result<()> {
    emit(run_list(&db_path, args))
}

pub fn import(db_path: PathBuf, args: ImportArgs) -> anyhow::Result<()> {
    emit(run_import(&db_path, args))
}

fn price_json(p: &means_core::model::Price) -> Value {
    json!({"id": p.id, "date": p.on.to_string(), "base": p.commodity, "quote": p.currency, "rate": money::plain(p.price), "source": p.source})
}

/// Revalue the postings that were booked without a rate, and the drafts at the new rates.
/// Returns the number of fixed entries and of revalued drafts.
fn revalue(conn: &mut means_core::rusqlite::Connection, skip: bool) -> AgentResult<Value> {
    if skip {
        return Ok(json!({"fixed": 0, "drafts": 0, "skipped_tax_events": [], "failed": 0, "first_error": null}));
    }
    let r = rates::revalue_missing(conn)?;
    Ok(json!({"fixed": r.fixed, "drafts": r.drafts, "skipped_tax_events": r.skipped_tax_events, "failed": r.failed, "first_error": r.first_error}))
}

fn run_set(db_path: &Path, a: SetArgs) -> AgentResult<Output> {
    let on = parse_date("date", &a.date)?;
    let rate = money::parse(&a.rate).map_err(|_| AgentError::new("invalid_input", format!("rate must be a decimal number, got {:?}", a.rate)))?;
    let db = open(db_path)?;
    let mut conn = db.conn();
    rates::set_rate(&conn, on, &a.base, &a.quote, rate, &a.source)?;
    let revalued = revalue(&mut conn, a.no_revalue)?;
    let row = rates::list_prices(&conn, &a.base, &a.quote, Some(on), Some(on), 1)?;
    let rate = row.first().map(price_json).unwrap_or(Value::Null);
    if a.json {
        return Ok(Output::Json(json!({"rate": rate, "revalued": revalued})));
    }
    Ok(Output::Text(format!(
        "rate {} 1 {} = {} {} ({}); {} entries revalued, {} drafts revalued\n{}",
        on,
        a.base.trim().to_ascii_uppercase(),
        rate["rate"].as_str().unwrap_or_default(),
        a.quote.trim().to_ascii_uppercase(),
        a.source.trim(),
        revalued["fixed"],
        revalued["drafts"],
        skipped_text(&revalued)
    )))
}

/// A line that names the drafts with tax events that the revaluation left as they are.
fn skipped_text(revalued: &Value) -> String {
    let ids: Vec<String> = revalued["skipped_tax_events"].as_array().map(|a| a.iter().map(|id| format!("#{id}")).collect()).unwrap_or_default();
    if ids.is_empty() {
        return String::new();
    }
    format!("drafts with tax events not revalued (review them and propose them again): {}\n", ids.join(", "))
}

fn run_list(db_path: &Path, a: ListArgs) -> AgentResult<Output> {
    if a.limit < 1 {
        return Err(AgentError::new("invalid_input", "limit must be 1 or more"));
    }
    if let Some(pair) = a.derive.as_deref() {
        return derive(db_path, pair, a.date.as_deref().unwrap_or_default(), a.json);
    }
    let from = a.from.as_deref().map(|d| parse_date("from", d)).transpose()?;
    let to = a.to.as_deref().map(|d| parse_date("to", d)).transpose()?;
    let db = open(db_path)?;
    let conn = db.conn();
    let rows = rates::list_prices(&conn, a.base.as_deref().unwrap_or(""), a.quote.as_deref().unwrap_or(""), from, to, a.limit)?;
    if a.json {
        let rates: Vec<Value> = rows.iter().map(price_json).collect();
        return Ok(Output::Json(json!({"count": rates.len(), "rates": rates})));
    }
    let mut text: String = rows.iter().map(|p| format!("{}  1 {} = {} {}  {}\n", p.on, p.commodity, money::plain(p.price), p.currency, p.source)).collect();
    text.push_str(&format!("{} rates\n", rows.len()));
    Ok(Output::Text(text))
}

/// The rate that the lookup finds for one pair on one day. `derived` is true for a cross rate.
fn derive(db_path: &Path, pair: &str, date: &str, json: bool) -> AgentResult<Output> {
    let on = parse_date("date", date)?;
    let pairs = rates::parse_pairs(pair)?;
    let [(base, quote)] = pairs.as_slice() else {
        return Err(AgentError::new("invalid_input", format!("--derive takes one pair, such as USD/SGD, got {pair:?}")));
    };
    let db = open(db_path)?;
    let conn = db.conn();
    let Some(found) = rates::rate_for(&conn, base, quote, on)? else {
        return Err(AgentError::new("missing_rate", format!("no {base} to {quote} rate on or before {on}, directly, as the inverse or through a common quote")));
    };
    let rate = money::plain(found.rate.normalize());
    let derived = rates::is_cross(&found.source);
    if json {
        return Ok(Output::Json(json!({"rate": {"date": on.to_string(), "base": base, "quote": quote, "rate": rate, "source": found.source, "derived": derived}})));
    }
    Ok(Output::Text(format!("{on}  1 {base} = {rate} {quote}  {}\n", found.source)))
}

fn run_import(db_path: &Path, a: ImportArgs) -> AgentResult<Output> {
    let text = std::fs::read_to_string(&a.file).map_err(|e| AgentError::new("invalid_input", format!("read {}: {e}", a.file.display())))?;
    let db = open(db_path)?;
    let mut conn = db.conn();
    let written = rates::import_rates_csv(&mut conn, &text, &a.source)?;
    let revalued = revalue(&mut conn, a.no_revalue)?;
    if a.json {
        return Ok(Output::Json(json!({"written": written, "source": a.source.trim(), "revalued": revalued})));
    }
    Ok(Output::Text(format!("rates: {written} written, {} entries revalued, {} drafts revalued\n{}", revalued["fixed"], revalued["drafts"], skipped_text(&revalued))))
}
