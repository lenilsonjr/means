//! means-core: the accounting layer.
//!
//! Books that survive an audit: a chart of accounts per entity, journal entries
//! made of postings that balance in the entity's functional currency, statement
//! lines as evidence, rules and templates that draft entries, reports as queries.
//!
//! Money is exact: quantities and amounts are `i64` counts of their commodity's minor unit
//! in storage and `rust_decimal::Decimal` in code (D14). Nothing here touches the network.

pub mod accounts;
pub mod audit;
pub mod budgets;
pub mod connections;
pub mod cross_source;
pub mod currency_migration;
pub mod db;
pub mod entities;
pub mod export;
pub mod hashchain;
pub mod imports;
pub mod journal;
pub mod matcher;
mod minor_units;
pub mod model;
pub mod money;
pub mod payees;
pub mod rates;
pub mod refunds;
pub mod reports;
pub mod rules;
pub mod tags;
pub mod templates;

pub use db::Db;
pub use model::*;
pub use rusqlite;

/// Errors surfaced to clients. The server maps them to gRPC status codes.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("locked: {0}")]
    Locked(String),
    #[error("unbalanced entry: {0}")]
    Unbalanced(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Today's date in the local timezone of the machine running the engine.
pub fn today() -> chrono::NaiveDate {
    chrono::Local::now().date_naive()
}

/// RFC 3339 timestamp for "now", UTC, millisecond precision.
pub fn now_ts() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// A new globally unique, time-ordered id (UUID v7) for records that may one day
/// be synced between servers.
pub fn new_uid() -> String {
    uuid::Uuid::now_v7().to_string()
}

pub fn parse_date(s: &str) -> Result<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").map_err(|_| Error::Invalid(format!("date must be YYYY-MM-DD, got {s:?}")))
}

pub fn parse_opt_date(s: &str) -> Result<Option<chrono::NaiveDate>> {
    if s.trim().is_empty() {
        Ok(None)
    } else {
        parse_date(s).map(Some)
    }
}

pub mod splits;
