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
pub mod close;
/// The year-end questionnaire of entity flags.
pub mod close_questions;
pub mod connections;
pub mod cross_source;
pub mod currency_migration;
pub mod db;
pub mod documents;
pub mod entities;
/// The address and the dated flags of an entity.
pub mod entity_flags;
pub mod export;
pub mod filings;
pub mod groups;
pub mod hashchain;
pub mod imports;
pub mod intervault;
pub mod invoices;
pub mod journal;
pub mod matcher;
mod minor_units;
pub mod model;
pub mod money;
pub mod parties;
pub mod payees;
pub mod policy;
pub mod proposal;
pub mod rates;
pub mod refunds;
pub mod reports;
pub mod rules;
pub mod snapshot;
pub mod tags;
pub mod tax;
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
    /// A void of an entry with inter-vault links that are not void, without cascade. The CLI
    /// gives `invalid_input`, and gRPC gives FAILED_PRECONDITION.
    #[error("needs cascade: {0}")]
    NeedsCascade(String),
    #[error("parse error: {0}")]
    Parse(String),
    /// The entity is archived. Its books are read-only until an unarchive.
    #[error("entity_archived: {0}")]
    Archived(String),
    #[error("database error: {0}")]
    Db(rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

/// The prefix of the trigger message that refuses a write to an archived entity.
/// Migration 29 raises it.
pub const ARCHIVED_PREFIX: &str = "entity_archived:";

impl From<rusqlite::Error> for Error {
    /// A write that an archive guard trigger refuses becomes `Error::Archived`. Every other
    /// database error stays `Error::Db`.
    fn from(e: rusqlite::Error) -> Error {
        match archived_message(&e) {
            Some(m) => Error::Archived(m),
            None => Error::Db(e),
        }
    }
}

/// The message of an archive guard refusal, without its prefix.
pub fn archived_message(e: &rusqlite::Error) -> Option<String> {
    match e {
        rusqlite::Error::SqliteFailure(_, Some(m)) => m.strip_prefix(ARCHIVED_PREFIX).map(|rest| rest.trim().to_string()),
        _ => None,
    }
}

/// True for a constraint violation of the schema, such as a unique key. An archive guard
/// refusal is not one: it maps to `Error::Archived`.
pub(crate) fn is_constraint_violation(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation) && archived_message(e).is_none()
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
