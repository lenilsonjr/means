//! Domain records as the rest of the crate and the server see them.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::{money::Money, Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: i64,
    pub uid: String,
    pub name: String,
    pub kind: String,
    pub country: String,
    pub currency: String,
    pub lock_date: Option<NaiveDate>,
    pub archived_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commodity {
    pub id: i64,
    pub code: String,
    pub kind: String,
    pub name: String,
    pub precision: u32,
    pub isin: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Price {
    pub id: i64,
    pub commodity: String,
    pub currency: String,
    pub on: NaiveDate,
    pub price: Decimal,
    pub source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountType {
    Asset,
    Liability,
    Equity,
    Income,
    Expense,
}

impl AccountType {
    pub fn as_str(self) -> &'static str {
        match self {
            AccountType::Asset => "asset",
            AccountType::Liability => "liability",
            AccountType::Equity => "equity",
            AccountType::Income => "income",
            AccountType::Expense => "expense",
        }
    }

    pub fn parse(s: &str) -> Result<AccountType> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "asset" | "assets" => AccountType::Asset,
            "liability" | "liabilities" => AccountType::Liability,
            "equity" => AccountType::Equity,
            "income" | "revenue" => AccountType::Income,
            "expense" | "expenses" => AccountType::Expense,
            other => return Err(Error::Invalid(format!("unknown account type {other:?}"))),
        })
    }

    /// +1 when the account grows by debit (assets, expenses), -1 when it grows by credit.
    pub fn normal_sign(self) -> i64 {
        match self {
            AccountType::Asset | AccountType::Expense => 1,
            _ => -1,
        }
    }

    pub fn root_name(self) -> &'static str {
        match self {
            AccountType::Asset => "Assets",
            AccountType::Liability => "Liabilities",
            AccountType::Equity => "Equity",
            AccountType::Income => "Income",
            AccountType::Expense => "Expenses",
        }
    }

    pub fn is_balance_sheet(self) -> bool {
        matches!(self, AccountType::Asset | AccountType::Liability | AccountType::Equity)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: i64,
    pub uid: String,
    pub entity_id: i64,
    pub parent_id: Option<i64>,
    pub code: String,
    pub name: String,
    /// Names joined by ":" from the type root: "Assets:Bank:N26".
    pub path: String,
    pub depth: i32,
    pub r#type: AccountType,
    pub subtype: String,
    pub commodity_id: i64,
    pub commodity: String,
    pub precision: u32,
    pub system_role: String,
    pub placeholder: bool,
    pub in_net_worth: bool,
    pub credit_limit: Option<Decimal>,
    pub statement_day: Option<i32>,
    pub due_day: Option<i32>,
    pub external_ids: serde_json::Value,
    pub notes: String,
    pub position: i32,
    pub closed_at: Option<String>,
    /// Budget nature of an expense account: fixed | committed | discretionary | savings | not-spending, or ''.
    pub class: String,
    // Derived when listing:
    pub balance: Decimal,
    pub balance_functional: Decimal,
    pub postings_count: i64,
    pub last_reconciled_at: Option<String>,
}

impl Account {
    pub fn is_closed(&self) -> bool {
        self.closed_at.is_some()
    }
}

#[derive(Debug, Clone, Default)]
pub struct NewAccount {
    pub entity_id: i64,
    pub parent_id: Option<i64>,
    pub code: String,
    pub name: String,
    pub r#type: Option<AccountType>,
    pub subtype: String,
    pub commodity: String,
    pub system_role: String,
    pub placeholder: bool,
    pub in_net_worth: bool,
    pub credit_limit: Option<Decimal>,
    pub statement_day: Option<i32>,
    pub due_day: Option<i32>,
    pub external_ids: Option<serde_json::Value>,
    pub notes: String,
    pub position: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryStatus {
    Draft,
    Posted,
    Void,
}

impl EntryStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            EntryStatus::Draft => "draft",
            EntryStatus::Posted => "posted",
            EntryStatus::Void => "void",
        }
    }
    pub fn parse(s: &str) -> Result<EntryStatus> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "" | "posted" => EntryStatus::Posted,
            "draft" => EntryStatus::Draft,
            "void" => EntryStatus::Void,
            other => return Err(Error::Invalid(format!("unknown status {other:?}"))),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Posting {
    pub id: i64,
    pub uid: String,
    pub journal_entry_id: i64,
    pub account_id: i64,
    pub account_path: String,
    pub account_type: AccountType,
    /// In the account's commodity, debit positive.
    pub quantity: Money,
    /// In the entity's functional currency, debit positive.
    pub amount: Money,
    pub rate: Option<Decimal>,
    pub rate_source: String,
    pub memo: String,
    pub metadata: serde_json::Value,
    pub external_id: Option<String>,
    pub fingerprint: Option<String>,
    pub reconciled_at: Option<String>,
    pub position: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub id: i64,
    pub uid: String,
    pub entity_id: i64,
    pub date: NaiveDate,
    pub payee: String,
    /// Canonical metadata; `payee` remains the immutable booked text for display-only changes.
    #[serde(default)]
    pub payee_id: Option<i64>,
    #[serde(default)]
    pub display_payee: String,
    pub description: String,
    pub notes: String,
    pub status: EntryStatus,
    pub reverses_id: Option<i64>,
    pub reversed_by_id: Option<i64>,
    /// Full refund of the named expense; does not void the original.
    #[serde(default)]
    pub refund_of_id: Option<i64>,
    pub counterpart_id: Option<i64>,
    pub template_id: Option<i64>,
    pub template_version: Option<i64>,
    pub origin: String,
    pub posted_at: Option<String>,
    pub created_at: String,
    pub seq: Option<i64>,
    pub hash: Option<String>,
    pub postings: Vec<Posting>,
    // Derived:
    pub kind: String,
    pub amount_functional: Decimal,
    pub statement_line_id: Option<i64>,
    /// key:value lenses, loaded from entry_tags; never part of the hash chain.
    pub tags: Vec<String>,
    /// When a human confirmed (or made) this entry; NULL on machine-posted entries awaiting review.
    /// Outside the hash chain, like tags.
    pub reviewed_at: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PostingInput {
    pub account_id: i64,
    pub quantity: Decimal,
    /// Functional-currency value; computed from rates when None.
    pub amount: Option<Decimal>,
    pub memo: String,
    pub metadata: serde_json::Value,
    pub external_id: Option<String>,
    pub fingerprint: Option<String>,
    /// This posting takes whatever value makes the entry sum to zero (at most one per entry).
    pub balance: bool,
    /// The posting's value expressed in another commodity, e.g. an expense posting valued as (30.00, "BRL")
    /// when the card is in BRL and the expense account is in EUR. Quantity is then derived.
    pub value_in: Option<(Decimal, String)>,
}

impl PostingInput {
    pub fn new(account_id: i64, quantity: Decimal) -> PostingInput {
        PostingInput { account_id, quantity, metadata: serde_json::json!({}), ..Default::default() }
    }
    pub fn balancing(account_id: i64) -> PostingInput {
        PostingInput { account_id, balance: true, metadata: serde_json::json!({}), ..Default::default() }
    }
    pub fn valued(account_id: i64, quantity: Decimal, commodity: &str) -> PostingInput {
        PostingInput { account_id, value_in: Some((quantity, commodity.to_string())), metadata: serde_json::json!({}), ..Default::default() }
    }
    pub fn memo(mut self, memo: &str) -> PostingInput {
        self.memo = memo.to_string();
        self
    }
    pub fn external(mut self, id: Option<String>, fingerprint: Option<String>) -> PostingInput {
        self.external_id = id;
        self.fingerprint = fingerprint;
        self
    }
    pub fn meta(mut self, key: &str, value: serde_json::Value) -> PostingInput {
        if !self.metadata.is_object() {
            self.metadata = serde_json::json!({});
        }
        self.metadata[key] = value;
        self
    }
}

#[derive(Debug, Clone)]
pub struct EntryInput {
    pub entity_id: i64,
    pub date: NaiveDate,
    pub payee: String,
    pub description: String,
    pub notes: String,
    pub status: EntryStatus,
    pub postings: Vec<PostingInput>,
    pub template_id: Option<i64>,
    pub template_version: Option<i64>,
    pub origin: String,
    pub counterpart_id: Option<i64>,
    pub reverses_id: Option<i64>,
    /// Post any residual after rating to the FX gain/loss account instead of failing.
    pub absorb_fx: bool,
}

impl EntryInput {
    pub fn new(entity_id: i64, date: NaiveDate) -> EntryInput {
        EntryInput {
            entity_id,
            date,
            payee: String::new(),
            description: String::new(),
            notes: String::new(),
            status: EntryStatus::Posted,
            postings: Vec::new(),
            template_id: None,
            template_version: None,
            origin: "capture".into(),
            counterpart_id: None,
            reverses_id: None,
            absorb_fx: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateLine {
    pub account_id: i64,
    /// fixed | percent_of | balance | input
    pub method: String,
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub value: Option<Decimal>,
    #[serde(default)]
    pub of_line: Option<usize>,
    #[serde(default)]
    pub memo: String,
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryTemplate {
    pub id: i64,
    pub uid: String,
    pub entity_id: i64,
    pub name: String,
    pub payee: String,
    pub description: String,
    pub lines: Vec<TemplateLine>,
    pub rrule: String,
    pub starts_on: Option<NaiveDate>,
    pub next_on: Option<NaiveDate>,
    pub ends_on: Option<NaiveDate>,
    pub auto_post: bool,
    pub lead_days: i32,
    pub version: i64,
    pub active: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Import {
    pub id: i64,
    pub uid: String,
    pub source: String,
    pub account_id: Option<i64>,
    pub filename: String,
    pub checksum: String,
    pub status: String,
    pub period_from: Option<NaiveDate>,
    pub period_to: Option<NaiveDate>,
    pub opening_balance: Option<Decimal>,
    pub closing_balance: Option<Decimal>,
    pub lines_count: i32,
    pub created_count: i32,
    pub matched_count: i32,
    pub duplicate_count: i32,
    pub skipped_count: i32,
    pub unmatched_count: i32,
    pub error_count: i32,
    pub error: String,
    pub options: serde_json::Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatementLine {
    pub id: i64,
    pub import_id: i64,
    pub account_id: Option<i64>,
    pub position: i32,
    pub raw: serde_json::Value,
    pub date: Option<NaiveDate>,
    /// Signed as the bank shows it: positive is money in. An account-free preview
    /// keeps untyped values in ImportOutcome.parse until the account is chosen.
    pub amount: Option<Money>,
    pub currency: String,
    pub description: String,
    pub reference: String,
    pub balance_after: Option<Money>,
    pub fingerprint: String,
    pub posting_id: Option<i64>,
    pub journal_entry_id: Option<i64>,
    pub duplicate_of_id: Option<i64>,
    pub status: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleCondition {
    pub field: String,
    pub op: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub id: i64,
    pub entity_id: i64,
    pub name: String,
    pub position: i32,
    pub enabled: bool,
    pub conditions: Vec<RuleCondition>,
    pub account_id: Option<i64>,
    pub template_id: Option<i64>,
    pub payee: String,
    pub hits_count: i32,
    pub created_at: String,
    /// Space-separated key:value tags set on entries this rule posts.
    pub tags: String,
}

/// Row of a report: a balance or movement per account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportRow {
    pub account_id: i64,
    pub path: String,
    pub name: String,
    pub r#type: AccountType,
    pub depth: i32,
    pub placeholder: bool,
    /// Normal-balance sign: positive when the account holds what it is expected to hold.
    pub quantity: Money,
    pub amount: Money,
    pub debit: Money,
    pub credit: Money,
    pub market_value: Option<Money>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerRow {
    pub posting_id: i64,
    pub journal_entry_id: i64,
    pub date: NaiveDate,
    pub payee: String,
    pub display_payee: String,
    pub description: String,
    pub status: EntryStatus,
    pub contra_path: String,
    pub debit: Money,
    pub credit: Money,
    pub running_balance: Money,
    pub amount: Money,
    pub reconciled: bool,
    pub statement_line_id: Option<i64>,
}
