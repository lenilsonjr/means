//! Invoices, open items and settlements.
//!
//! The engine does not issue invoices. It records invoices that a party received or issued
//! elsewhere as facts, and links them to the ledger:
//!
//! - A recognition entry books the invoice on its tax point: the expense or the revenue, the
//!   tax, and the control account (accounts payable or accounts receivable). The engine creates
//!   it as a draft. A human posts it.
//! - A settlement links an entry or a posting to the invoice and reduces its open balance. The
//!   amount is in the invoice commodity. The status follows from the settlements.
//! - A credit note is an invoice of kind `credit_note`. It settles the invoice that it credits.
//!
//! Duplicate rule: a bank import can book an expense before the invoice arrives. When the
//! invoice has a recognition entry and a payment settles it from such a bank entry, the
//! bank entry books the expense a second time. The settlement then converts the bank entry: its
//! expense (or revenue) postings move to the control account through the ordinary entry edit, in
//! the same transaction. The expense stays once, on the recognition entry.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::entities::{date_at, opt_date_at};
use crate::model::*;
use crate::money::{self, Money};
use crate::proposal::{self, AgentError, AgentResult};
use crate::{accounts, audit, entities, journal, new_uid, now_ts, rates};

/// The default window in days around a date for the duplicate expense guard.
pub const DUPLICATE_WINDOW_DAYS: i64 = 3;

/// The origin of the entries that this module creates.
pub const ORIGIN: &str = "invoice";

fn invalid(message: impl Into<String>) -> AgentError {
    AgentError::new("invalid_input", message)
}

// ---------------------------------------------------------------------------
// Enumerations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Sales,
    Purchase,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Sales => "sales",
            Direction::Purchase => "purchase",
        }
    }

    pub fn parse(s: &str) -> AgentResult<Direction> {
        match s.trim().to_ascii_lowercase().as_str() {
            "sales" | "sale" => Ok(Direction::Sales),
            "purchase" | "purchases" => Ok(Direction::Purchase),
            other => Err(invalid(format!("unknown direction {other:?}; use sales or purchase"))),
        }
    }

    /// The account type of the lines: expense for a purchase, income for a sale.
    fn line_type(self) -> AccountType {
        match self {
            Direction::Sales => AccountType::Income,
            Direction::Purchase => AccountType::Expense,
        }
    }

    /// +1 when the lines of an invoice are debits (purchase), -1 when they are credits (sales).
    fn line_sign(self) -> Decimal {
        match self {
            Direction::Sales => Decimal::NEGATIVE_ONE,
            Direction::Purchase => Decimal::ONE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InvoiceStatus {
    Open,
    PartiallySettled,
    Settled,
    Cancelled,
}

impl InvoiceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            InvoiceStatus::Open => "open",
            InvoiceStatus::PartiallySettled => "partially_settled",
            InvoiceStatus::Settled => "settled",
            InvoiceStatus::Cancelled => "cancelled",
        }
    }

    /// The status that follows from the settled amount.
    fn derive(settled: i64, gross: i64) -> InvoiceStatus {
        if settled >= gross {
            InvoiceStatus::Settled
        } else if settled > 0 {
            InvoiceStatus::PartiallySettled
        } else {
            InvoiceStatus::Open
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InvoiceKind {
    Invoice,
    CreditNote,
}

impl InvoiceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            InvoiceKind::Invoice => "invoice",
            InvoiceKind::CreditNote => "credit_note",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SettlementKind {
    Payment,
    CreditNote,
    Withholding,
    Fee,
    FxDifference,
    WriteOff,
    Refund,
}

impl SettlementKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SettlementKind::Payment => "payment",
            SettlementKind::CreditNote => "credit_note",
            SettlementKind::Withholding => "withholding",
            SettlementKind::Fee => "fee",
            SettlementKind::FxDifference => "fx_difference",
            SettlementKind::WriteOff => "write_off",
            SettlementKind::Refund => "refund",
        }
    }

    pub fn parse(s: &str) -> AgentResult<SettlementKind> {
        Ok(match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "payment" => SettlementKind::Payment,
            "credit_note" => SettlementKind::CreditNote,
            "withholding" => SettlementKind::Withholding,
            "fee" => SettlementKind::Fee,
            "fx_difference" => SettlementKind::FxDifference,
            "write_off" => SettlementKind::WriteOff,
            "refund" => SettlementKind::Refund,
            other => return Err(invalid(format!("unknown settlement kind {other:?}; use payment, withholding, fee, fx_difference, write_off or refund"))),
        })
    }

    /// Kinds that move money through a bank. They need an entry and can carry an exchange difference.
    fn moves_money(self) -> bool {
        matches!(self, SettlementKind::Payment | SettlementKind::Refund)
    }
}

// ---------------------------------------------------------------------------
// Input documents
// ---------------------------------------------------------------------------

/// The input document of `record_invoice` and `credit_note`. Numbers are decimal strings in
/// the invoice commodity. Accounts are paths inside the entity or account uids.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvoiceInput {
    /// `sales` or `purchase`. A credit note takes the direction of the invoice that it credits.
    #[serde(default)]
    pub direction: String,
    /// A payee of the entity: its uid or its name.
    #[serde(default)]
    pub party: Option<String>,
    pub number: String,
    #[serde(default)]
    pub series: String,
    pub issue_date: String,
    /// The tax and accrual date. Default: the issue date.
    #[serde(default)]
    pub tax_point: Option<String>,
    #[serde(default)]
    pub due_date: Option<String>,
    /// Default for a credit note: the commodity of the invoice that it credits.
    #[serde(default)]
    pub commodity: String,
    pub net: String,
    #[serde(default)]
    pub tax: Option<String>,
    pub gross: String,
    /// The part of the gross that the payer keeps back and pays to an authority.
    #[serde(default)]
    pub withheld: Option<String>,
    #[serde(default)]
    pub lines: Vec<InvoiceLineInput>,
    /// The line account for lines without an account, and for an invoice without lines.
    #[serde(default)]
    pub account: Option<String>,
    /// The control account of a purchase invoice (accounts payable).
    #[serde(default)]
    pub payable_account: Option<String>,
    /// The control account of a sales invoice (accounts receivable).
    #[serde(default)]
    pub receivable_account: Option<String>,
    /// The account of the tax postings.
    #[serde(default)]
    pub tax_account: Option<String>,
    /// Functional currency per unit of the invoice commodity. Default: the rate on the tax point.
    #[serde(default)]
    pub rate: Option<String>,
    /// The uid of a stored document of the entity.
    #[serde(default)]
    pub document: Option<String>,
    #[serde(default)]
    pub external_ref: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Create the recognition entry as a draft.
    #[serde(default)]
    pub recognize: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvoiceLineInput {
    #[serde(default)]
    pub description: String,
    /// A decimal string. Default: 1.
    #[serde(default)]
    pub quantity: Option<String>,
    #[serde(default)]
    pub unit_price: Option<String>,
    /// Default: quantity times unit price.
    #[serde(default)]
    pub net: Option<String>,
    #[serde(default)]
    pub tax_code: String,
    #[serde(default)]
    pub tax: Option<String>,
    #[serde(default)]
    pub account: Option<String>,
}

/// The input of `settle`.
#[derive(Debug, Clone)]
pub struct SettlementInput {
    pub invoice_uid: String,
    pub entry_uid: Option<String>,
    pub posting_uid: Option<String>,
    pub date: NaiveDate,
    /// In the invoice commodity. Positive reduces the open balance.
    pub amount: Decimal,
    pub kind: SettlementKind,
    /// The account of the exchange difference. Without it, the engine reports the difference and books nothing.
    pub fx_account: Option<String>,
    /// Overrides the control account that the invoice keeps.
    pub control_account: Option<String>,
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Invoice {
    #[serde(skip_serializing)]
    pub id: i64,
    pub uid: String,
    #[serde(skip_serializing)]
    pub entity_id: i64,
    pub entity_uid: String,
    pub direction: Direction,
    #[serde(skip_serializing)]
    pub party_id: Option<i64>,
    pub party_uid: Option<String>,
    pub party_name: Option<String>,
    pub number: String,
    pub series: String,
    pub issue_date: NaiveDate,
    pub tax_point: NaiveDate,
    pub due_date: Option<NaiveDate>,
    pub commodity: String,
    pub net: Money,
    pub tax: Money,
    pub gross: Money,
    pub withheld: Money,
    /// The sum of the settlements whose entry is not void. For a credit note: the applied amount.
    pub settled: Money,
    pub open: Money,
    pub status: InvoiceStatus,
    pub kind: InvoiceKind,
    #[serde(skip_serializing)]
    pub credits_invoice_id: Option<i64>,
    pub credits_invoice_uid: Option<String>,
    #[serde(skip_serializing)]
    pub recognition_entry_id: Option<i64>,
    pub recognition_entry_uid: Option<String>,
    #[serde(skip_serializing)]
    pub document_id: Option<i64>,
    pub document_uid: Option<String>,
    pub external_ref: String,
    pub metadata: serde_json::Value,
    pub created_at: String,
    pub updated_at: String,
}

impl Invoice {
    /// The series and the number, as printed.
    pub fn label(&self) -> String {
        if self.series.is_empty() {
            self.number.clone()
        } else {
            format!("{}-{}", self.series, self.number)
        }
    }

    fn money(&self, minor: i64) -> AgentResult<Money> {
        Ok(Money::from_minor(minor, &self.commodity, self.gross.precision())?)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct InvoiceLine {
    #[serde(skip_serializing)]
    pub id: i64,
    pub position: i64,
    pub description: String,
    pub quantity: String,
    pub unit_price: Money,
    pub net: Money,
    pub tax_code: String,
    pub tax: Money,
    #[serde(skip_serializing)]
    pub account_id: Option<i64>,
    pub account_uid: Option<String>,
    pub account_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Settlement {
    #[serde(skip_serializing)]
    pub id: i64,
    pub uid: String,
    #[serde(skip_serializing)]
    pub invoice_id: i64,
    pub invoice_uid: String,
    #[serde(skip_serializing)]
    pub entry_id: Option<i64>,
    pub entry_uid: Option<String>,
    pub entry_status: Option<String>,
    #[serde(skip_serializing)]
    pub posting_id: Option<i64>,
    pub posting_uid: Option<String>,
    pub date: NaiveDate,
    pub amount: Money,
    pub kind: SettlementKind,
    #[serde(skip_serializing)]
    pub fx_difference_entry_id: Option<i64>,
    pub fx_difference_entry_uid: Option<String>,
    #[serde(skip_serializing)]
    pub credit_note_id: Option<i64>,
    pub credit_note_uid: Option<String>,
    pub created_at: String,
}

/// One line of the open items report.
#[derive(Debug, Clone, Serialize)]
pub struct OpenItem {
    pub invoice_uid: String,
    pub direction: Direction,
    pub series: String,
    pub number: String,
    pub party_uid: Option<String>,
    pub party_name: Option<String>,
    pub issue_date: NaiveDate,
    pub due_date: Option<NaiveDate>,
    pub gross: Money,
    pub settled: Money,
    pub open: Money,
    /// Days after the due date on the report date. Zero when the invoice is not due yet or has no due date.
    pub days_overdue: i64,
}

/// A bank entry that looks like the payment of an invoice and has no settlement.
#[derive(Debug, Clone, Serialize)]
pub struct DuplicateCandidate {
    #[serde(skip_serializing)]
    pub entry_id: i64,
    pub entry_uid: String,
    pub date: NaiveDate,
    pub status: EntryStatus,
    pub payee: String,
    pub bank_posting_uid: String,
    pub category_posting_uids: Vec<String>,
    pub amount: Money,
}

#[derive(Debug, Clone)]
pub struct Recorded {
    pub invoice: Invoice,
    pub lines: Vec<InvoiceLine>,
    pub recognition: Option<JournalEntry>,
    pub audit_event_id: i64,
}

#[derive(Debug, Clone)]
pub struct Settled {
    pub settlement: Settlement,
    pub invoice: Invoice,
    /// True when the settlement moved the expense or revenue postings of the entry to the control account.
    pub converted: bool,
    /// The exchange difference in the functional currency: the value of the payment less the booked value.
    pub fx_difference: Option<Money>,
    pub fx_entry: Option<JournalEntry>,
    pub audit_event_id: i64,
}

#[derive(Debug, Clone)]
pub struct CreditNoted {
    pub credit_note: Recorded,
    pub settlement: Settlement,
    pub invoice: Invoice,
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// `?1` is the report date: settlements after it do not count. NULL counts every settlement.
const SETTLED_SQL: &str = "CASE WHEN i.kind = 'credit_note' THEN
        (SELECT COALESCE(SUM(s.amount), 0) FROM settlements s LEFT JOIN journal_entries se ON se.id = s.entry_id
         WHERE s.credit_note_id = i.id AND COALESCE(se.status, '') <> 'void' AND (?1 IS NULL OR s.date <= ?1))
    ELSE
        (SELECT COALESCE(SUM(s.amount), 0) FROM settlements s LEFT JOIN journal_entries se ON se.id = s.entry_id
         WHERE s.invoice_id = i.id AND COALESCE(se.status, '') <> 'void' AND (?1 IS NULL OR s.date <= ?1))
    END";

fn invoice_select() -> String {
    format!(
        "SELECT i.id, i.uid, i.entity_id, en.uid, i.direction, i.party_id, p.uid, p.name, i.number, i.series, i.issue_date, i.tax_point, i.due_date,
                i.commodity, c.precision, i.net, i.tax, i.gross, i.withheld, i.status, i.kind, i.credits_invoice_id, ci.uid, i.recognition_entry_id, re.uid,
                i.document_id, d.uid, i.external_ref, i.metadata, i.created_at, i.updated_at, {SETTLED_SQL}
         FROM invoices i JOIN entities en ON en.id = i.entity_id JOIN commodities c ON c.code = i.commodity
              LEFT JOIN payees p ON p.id = i.party_id LEFT JOIN invoices ci ON ci.id = i.credits_invoice_id
              LEFT JOIN journal_entries re ON re.id = i.recognition_entry_id LEFT JOIN documents d ON d.id = i.document_id"
    )
}

fn conversion(idx: usize, e: impl std::error::Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(e))
}

fn row_to_invoice(r: &rusqlite::Row<'_>) -> rusqlite::Result<Invoice> {
    let commodity: String = r.get(13)?;
    let precision: u32 = r.get(14)?;
    let m = |idx: usize| -> rusqlite::Result<Money> { Money::from_minor(r.get(idx)?, &commodity, precision).map_err(|e| conversion(idx, e)) };
    let direction: String = r.get(4)?;
    let stored: String = r.get(19)?;
    let kind: String = r.get(20)?;
    let metadata: String = r.get(28)?;
    let gross: i64 = r.get(17)?;
    let settled: i64 = r.get(31)?;
    let status = if stored == "cancelled" { InvoiceStatus::Cancelled } else { InvoiceStatus::derive(settled, gross) };
    let open = if status == InvoiceStatus::Cancelled { 0 } else { gross - settled };
    Ok(Invoice {
        id: r.get(0)?,
        uid: r.get(1)?,
        entity_id: r.get(2)?,
        entity_uid: r.get(3)?,
        direction: Direction::parse(&direction).map_err(|e| conversion(4, e))?,
        party_id: r.get(5)?,
        party_uid: r.get(6)?,
        party_name: r.get(7)?,
        number: r.get(8)?,
        series: r.get(9)?,
        issue_date: date_at(r, 10)?,
        tax_point: date_at(r, 11)?,
        due_date: opt_date_at(r, 12)?,
        net: m(15)?,
        tax: m(16)?,
        gross: m(17)?,
        withheld: m(18)?,
        settled: m(31)?,
        open: Money::from_minor(open, &commodity, precision).map_err(|e| conversion(17, e))?,
        status,
        kind: if kind == "credit_note" { InvoiceKind::CreditNote } else { InvoiceKind::Invoice },
        credits_invoice_id: r.get(21)?,
        credits_invoice_uid: r.get(22)?,
        recognition_entry_id: r.get(23)?,
        recognition_entry_uid: r.get(24)?,
        document_id: r.get(25)?,
        document_uid: r.get(26)?,
        external_ref: r.get(27)?,
        metadata: serde_json::from_str(&metadata).unwrap_or_else(|_| serde_json::json!({})),
        created_at: r.get(29)?,
        updated_at: r.get(30)?,
        commodity,
    })
}

pub fn get_invoice(conn: &Connection, id: i64) -> AgentResult<Invoice> {
    let sql = format!("{} WHERE i.id = ?2", invoice_select());
    conn.query_row(&sql, params![Option::<String>::None, id], row_to_invoice).optional()?.ok_or_else(|| AgentError::new("not_found", format!("no invoice {id}")))
}

pub fn get_invoice_by_uid(conn: &Connection, uid: &str) -> AgentResult<Invoice> {
    let sql = format!("{} WHERE i.uid = ?2", invoice_select());
    conn.query_row(&sql, params![Option::<String>::None, uid.trim()], row_to_invoice).optional()?.ok_or_else(|| AgentError::new("not_found", format!("no invoice with uid {}", uid.trim())))
}

/// The invoices of an entity, oldest issue date first. `as_of` drops invoices issued after it
/// and settlements dated after it. `open_only` keeps invoices of kind `invoice` with an open balance.
pub fn list_invoices(conn: &Connection, entity_id: i64, direction: Option<Direction>, open_only: bool, as_of: Option<NaiveDate>) -> AgentResult<Vec<Invoice>> {
    entities::get_entity(conn, entity_id)?;
    let sql = format!(
        "{} WHERE i.entity_id = ?2 AND (?3 IS NULL OR i.direction = ?3) AND (?1 IS NULL OR i.issue_date <= ?1)
         ORDER BY i.issue_date, i.series, i.number, i.id",
        invoice_select()
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![as_of.map(|d| d.to_string()), entity_id, direction.map(Direction::as_str)], row_to_invoice)?;
    let mut out = Vec::new();
    for row in rows {
        let invoice = row?;
        if open_only && (invoice.kind != InvoiceKind::Invoice || invoice.status == InvoiceStatus::Cancelled || invoice.open.minor() <= 0) {
            continue;
        }
        out.push(invoice);
    }
    Ok(out)
}

/// The open items of an entity on a date: invoices with an open balance, oldest due first.
pub fn open_items(conn: &Connection, entity_id: i64, as_of: NaiveDate, direction: Option<Direction>) -> AgentResult<Vec<OpenItem>> {
    let mut items: Vec<OpenItem> = list_invoices(conn, entity_id, direction, true, Some(as_of))?
        .into_iter()
        .map(|i| OpenItem {
            days_overdue: i.due_date.map(|due| (as_of - due).num_days().max(0)).unwrap_or(0),
            invoice_uid: i.uid,
            direction: i.direction,
            series: i.series,
            number: i.number,
            party_uid: i.party_uid,
            party_name: i.party_name,
            issue_date: i.issue_date,
            due_date: i.due_date,
            gross: i.gross,
            settled: i.settled,
            open: i.open,
        })
        .collect();
    items.sort_by(|a, b| a.due_date.unwrap_or(a.issue_date).cmp(&b.due_date.unwrap_or(b.issue_date)).then(a.issue_date.cmp(&b.issue_date)));
    Ok(items)
}

pub fn lines(conn: &Connection, invoice_id: i64) -> AgentResult<Vec<InvoiceLine>> {
    let invoice = get_invoice(conn, invoice_id)?;
    let precision = invoice.gross.precision();
    let mut stmt = conn.prepare(
        "SELECT l.id, l.position, l.description, l.quantity, l.unit_price, l.net, l.tax_code, l.tax, l.account_id, a.uid
         FROM invoice_lines l LEFT JOIN accounts a ON a.id = l.account_id WHERE l.invoice_id = ?1 ORDER BY l.position",
    )?;
    let paths = accounts::path_index(conn)?;
    let rows = stmt.query_map([invoice_id], |r| {
        let m = |idx: usize| -> rusqlite::Result<Money> { Money::from_minor(r.get(idx)?, &invoice.commodity, precision).map_err(|e| conversion(idx, e)) };
        let account_id: Option<i64> = r.get(8)?;
        Ok(InvoiceLine {
            id: r.get(0)?,
            position: r.get(1)?,
            description: r.get(2)?,
            quantity: r.get(3)?,
            unit_price: m(4)?,
            net: m(5)?,
            tax_code: r.get(6)?,
            tax: m(7)?,
            account_id,
            account_uid: r.get(9)?,
            account_path: account_id.and_then(|id| paths.get(&id).map(|a| a.path.clone())),
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// The settlements of an invoice, oldest first. For a credit note: the settlements that apply it.
pub fn settlements(conn: &Connection, invoice_id: i64) -> AgentResult<Vec<Settlement>> {
    let invoice = get_invoice(conn, invoice_id)?;
    let column = if invoice.kind == InvoiceKind::CreditNote { "s.credit_note_id" } else { "s.invoice_id" };
    let mut stmt = conn.prepare(&format!(
        "SELECT s.id, s.uid, s.invoice_id, i.uid, s.entry_id, e.uid, e.status, s.posting_id, p.uid, s.date, s.amount, s.kind, s.fx_difference_entry_id, fx.uid,
                s.credit_note_id, cn.uid, s.created_at, i.commodity, c.precision
         FROM settlements s JOIN invoices i ON i.id = s.invoice_id JOIN commodities c ON c.code = i.commodity
              LEFT JOIN journal_entries e ON e.id = s.entry_id LEFT JOIN postings p ON p.id = s.posting_id
              LEFT JOIN journal_entries fx ON fx.id = s.fx_difference_entry_id LEFT JOIN invoices cn ON cn.id = s.credit_note_id
         WHERE {column} = ?1 ORDER BY s.date, s.id"
    ))?;
    let rows = stmt.query_map([invoice_id], row_to_settlement)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn row_to_settlement(r: &rusqlite::Row<'_>) -> rusqlite::Result<Settlement> {
    let kind: String = r.get(11)?;
    let commodity: String = r.get(17)?;
    Ok(Settlement {
        id: r.get(0)?,
        uid: r.get(1)?,
        invoice_id: r.get(2)?,
        invoice_uid: r.get(3)?,
        entry_id: r.get(4)?,
        entry_uid: r.get(5)?,
        entry_status: r.get(6)?,
        posting_id: r.get(7)?,
        posting_uid: r.get(8)?,
        date: date_at(r, 9)?,
        amount: Money::from_minor(r.get(10)?, &commodity, r.get(18)?).map_err(|e| conversion(10, e))?,
        kind: SettlementKind::parse(&kind).map_err(|e| conversion(11, e))?,
        fx_difference_entry_id: r.get(12)?,
        fx_difference_entry_uid: r.get(13)?,
        credit_note_id: r.get(14)?,
        credit_note_uid: r.get(15)?,
        created_at: r.get(16)?,
    })
}

fn get_settlement(conn: &Connection, id: i64) -> AgentResult<Settlement> {
    let invoice_id: i64 = conn.query_row("SELECT invoice_id FROM settlements WHERE id = ?1", [id], |r| r.get(0))?;
    settlements(conn, invoice_id)?.into_iter().find(|s| s.id == id).ok_or_else(|| AgentError::new("not_found", format!("no settlement {id}")))
}

/// The credit notes that credit an invoice.
pub fn credit_notes(conn: &Connection, invoice_id: i64) -> AgentResult<Vec<Invoice>> {
    let ids: Vec<i64> = conn.prepare("SELECT id FROM invoices WHERE credits_invoice_id = ?1 ORDER BY id")?.query_map([invoice_id], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
    ids.into_iter().map(|id| get_invoice(conn, id)).collect()
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

/// A decimal string in minor units of the commodity. More decimals than the commodity allows are refused.
fn minor(field: &str, value: &str, commodity: &str, precision: u32) -> AgentResult<i64> {
    let d = money::parse(value).map_err(|_| invalid(format!("{field} must be a decimal string, got {value:?}")))?;
    if money::round_to(d, precision) != d {
        return Err(invalid(format!("{field} {value} has more decimals than {commodity} allows ({precision})")));
    }
    Ok(Money::from_major(d, commodity, precision)?.minor())
}

fn date(field: &str, value: &str) -> AgentResult<NaiveDate> {
    crate::parse_date(value).map_err(|_| invalid(format!("{field} must be YYYY-MM-DD, got {value:?}")))
}

/// A payee of the entity by uid or by name.
fn resolve_party(conn: &Connection, entity_id: i64, key: &str) -> AgentResult<i64> {
    let key = key.trim();
    let by_uid: Option<(i64, i64)> = conn.query_row("SELECT id, entity_id FROM payees WHERE uid = ?1", [key], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
    if let Some((id, entity)) = by_uid {
        if entity != entity_id {
            return Err(invalid(format!("party {key} belongs to another entity")));
        }
        return Ok(id);
    }
    let by_name: Option<i64> = conn.query_row("SELECT id FROM payees WHERE entity_id = ?1 AND name = ?2 COLLATE NOCASE ORDER BY id LIMIT 1", params![entity_id, key], |r| r.get(0)).optional()?;
    by_name.ok_or_else(|| AgentError::new("not_found", format!("no party {key} in this entity")))
}

fn resolve_account(conn: &Connection, entity: &Entity, key: Option<&str>) -> AgentResult<Option<Account>> {
    match key.map(str::trim).filter(|k| !k.is_empty()) {
        Some(k) => Ok(Some(proposal::resolve_account(conn, entity, k)?)),
        None => Ok(None),
    }
}

/// The control account that the invoice keeps, or the override.
fn control_account(conn: &Connection, entity: &Entity, invoice: &Invoice, override_key: Option<&str>) -> AgentResult<Option<Account>> {
    let stored = invoice.metadata.get("control_account_uid").and_then(|v| v.as_str()).map(str::to_string);
    resolve_account(conn, entity, override_key.or(stored.as_deref()))
}

/// The tax account: the account in the input, or the one open leaf account of the entity with
/// the tax treatment `input_tax` (purchase) or `output_tax` (sales).
fn tax_account(conn: &Connection, entity: &Entity, direction: Direction, key: Option<&str>) -> AgentResult<Account> {
    if let Some(account) = resolve_account(conn, entity, key)? {
        return Ok(account);
    }
    let treatment = match direction {
        Direction::Purchase => "input_tax",
        Direction::Sales => "output_tax",
    };
    let found: Vec<i64> = conn
        .prepare("SELECT id FROM accounts a WHERE entity_id = ?1 AND tax_treatment = ?2 AND placeholder = 0 AND closed_at IS NULL AND NOT EXISTS (SELECT 1 FROM accounts c WHERE c.parent_id = a.id)")?
        .query_map(params![entity.id, treatment], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    match found.as_slice() {
        [id] => Ok(accounts::get_account(conn, *id)?),
        _ => Err(invalid(format!("the invoice has tax; name a tax_account, or give one account the tax treatment {treatment}"))),
    }
}

/// A validated line before insert.
struct LineRow {
    description: String,
    quantity: String,
    unit_price: i64,
    net: i64,
    tax_code: String,
    tax: i64,
    account: Option<Account>,
}

fn line_rows(conn: &Connection, entity: &Entity, commodity: &str, precision: u32, input: &[InvoiceLineInput]) -> AgentResult<Vec<LineRow>> {
    let mut rows = Vec::with_capacity(input.len());
    for (i, l) in input.iter().enumerate() {
        let n = i + 1;
        let quantity = match l.quantity.as_deref() {
            Some(q) => money::parse(q).map_err(|_| invalid(format!("line {n}: quantity must be a decimal string, got {q:?}")))?,
            None => Decimal::ONE,
        };
        if quantity.is_zero() {
            return Err(invalid(format!("line {n}: quantity must not be zero")));
        }
        let unit_price = l.unit_price.as_deref().map(|v| money::parse(v).map_err(|_| invalid(format!("line {n}: unit_price must be a decimal string, got {v:?}")))).transpose()?;
        let net = match (l.net.as_deref(), unit_price) {
            (Some(v), _) => minor(&format!("line {n}: net"), v, commodity, precision)?,
            (None, Some(p)) => {
                let value = quantity.checked_mul(p).ok_or_else(|| invalid(format!("line {n}: amount overflow")))?;
                Money::from_major(money::round_to(value, precision), commodity, precision)?.minor()
            }
            (None, None) => return Err(invalid(format!("line {n}: give net or unit_price"))),
        };
        let unit_price = match unit_price {
            Some(p) => Money::from_major(money::round_to(p, precision), commodity, precision)?.minor(),
            None => {
                let net_major = Money::from_minor(net, commodity, precision)?.major();
                Money::from_major(money::round_to(net_major / quantity, precision), commodity, precision)?.minor()
            }
        };
        let tax = l.tax.as_deref().map(|v| minor(&format!("line {n}: tax"), v, commodity, precision)).transpose()?.unwrap_or(0);
        let account = resolve_account(conn, entity, l.account.as_deref())?;
        rows.push(LineRow { description: l.description.trim().to_string(), quantity: money::plain(quantity), unit_price, net, tax_code: l.tax_code.trim().to_string(), tax, account });
    }
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Recording
// ---------------------------------------------------------------------------

/// Record an invoice of an entity. With `recognize`, create the recognition entry as a draft.
pub fn record_invoice(conn: &mut Connection, entity_id: i64, input: &InvoiceInput) -> AgentResult<Recorded> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let recorded = record_in(&tx, entity_id, input, None)?;
    tx.commit()?;
    Ok(recorded)
}

fn record_in(tx: &Transaction<'_>, entity_id: i64, input: &InvoiceInput, credits: Option<&Invoice>) -> AgentResult<Recorded> {
    let entity = entities::get_entity(tx, entity_id)?;
    let direction = match (credits, input.direction.trim()) {
        (Some(original), "") => original.direction,
        (Some(original), d) => {
            let d = Direction::parse(d)?;
            if d != original.direction {
                return Err(invalid(format!("a credit note has the direction of the invoice that it credits ({})", original.direction.as_str())));
            }
            d
        }
        (None, "") => return Err(invalid("give the direction: sales or purchase")),
        (None, d) => Direction::parse(d)?,
    };
    let code = match (credits, input.commodity.trim()) {
        (Some(original), "") => original.commodity.clone(),
        (_, "") => return Err(invalid("give the commodity of the invoice")),
        (_, c) => c.to_ascii_uppercase(),
    };
    if let Some(original) = credits {
        if original.commodity != code {
            return Err(invalid(format!("a credit note uses the commodity of the invoice that it credits ({})", original.commodity)));
        }
    }
    let commodity = entities::get_commodity_by_code(tx, &code)?;
    let prec = commodity.precision;
    let number = input.number.trim();
    if number.is_empty() {
        return Err(invalid("give the invoice number"));
    }
    let series = input.series.trim();
    let issue_date = date("issue_date", &input.issue_date)?;
    let tax_point = input.tax_point.as_deref().map(|d| date("tax_point", d)).transpose()?.unwrap_or(issue_date);
    let due_date = input.due_date.as_deref().filter(|d| !d.trim().is_empty()).map(|d| date("due_date", d)).transpose()?;
    if due_date.is_some_and(|d| d < issue_date) {
        return Err(invalid("the due date must not be before the issue date"));
    }
    let net = minor("net", &input.net, &code, prec)?;
    let tax = input.tax.as_deref().map(|v| minor("tax", v, &code, prec)).transpose()?.unwrap_or(0);
    let gross = minor("gross", &input.gross, &code, prec)?;
    let withheld = input.withheld.as_deref().map(|v| minor("withheld", v, &code, prec)).transpose()?.unwrap_or(0);
    if net.checked_add(tax) != Some(gross) {
        return Err(invalid(format!("net plus tax must equal gross: {} + {} is not {}", input.net, input.tax.as_deref().unwrap_or("0"), input.gross)));
    }
    if gross <= 0 {
        return Err(invalid("the gross amount must be positive"));
    }
    if withheld < 0 || withheld > gross {
        return Err(invalid("the withheld amount must be from zero to the gross amount"));
    }
    let rows = line_rows(tx, &entity, &code, prec, &input.lines)?;
    if !rows.is_empty() {
        let (line_net, line_tax) = rows.iter().fold((0i64, 0i64), |(n, t), l| (n.saturating_add(l.net), t.saturating_add(l.tax)));
        if line_net != net || line_tax != tax {
            return Err(invalid("the lines must add up to the net and the tax of the invoice"));
        }
    }
    let party_id = match input.party.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        Some(key) => Some(resolve_party(tx, entity.id, key)?),
        None => credits.and_then(|c| c.party_id),
    };
    if !(input.metadata.is_null() || input.metadata.is_object()) {
        return Err(invalid("metadata must be an object"));
    }
    let exists: bool =
        tx.query_row("SELECT EXISTS(SELECT 1 FROM invoices WHERE entity_id = ?1 AND direction = ?2 AND series = ?3 AND number = ?4)", params![entity.id, direction.as_str(), series, number], |r| {
            r.get(0)
        })?;
    if exists {
        return Err(AgentError::new(
            "duplicate",
            format!("{} already has a {} invoice {}{}", entity.name, direction.as_str(), if series.is_empty() { String::new() } else { format!("{series}-") }, number),
        ));
    }
    let control_key = match direction {
        Direction::Purchase => input.payable_account.as_deref(),
        Direction::Sales => input.receivable_account.as_deref(),
    };
    let stored_control = credits.and_then(|c| c.metadata.get("control_account_uid").and_then(|v| v.as_str()).map(str::to_string));
    let control = resolve_account(tx, &entity, control_key.or(stored_control.as_deref()))?;
    if let Some(c) = &control {
        if !matches!(c.r#type, AccountType::Asset | AccountType::Liability) {
            return Err(invalid(format!("the control account {} must be an asset or a liability account", c.path)));
        }
    }
    let default_account = resolve_account(tx, &entity, input.account.as_deref())?;
    let document_id = match input.document.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        Some(uid) => {
            let doc = crate::documents::get_document(tx, uid)?;
            if doc.entity_id != entity.id {
                return Err(invalid(format!("document {uid} belongs to another entity")));
            }
            Some(doc.id)
        }
        None => None,
    };
    let mut metadata = if input.metadata.is_object() { input.metadata.clone() } else { serde_json::json!({}) };
    if let Some(c) = &control {
        metadata["control_account_uid"] = serde_json::json!(c.uid);
    }
    if let Some(rate) = input.rate.as_deref() {
        let r = money::parse(rate).map_err(|_| invalid(format!("rate must be a decimal string, got {rate:?}")))?;
        if r <= Decimal::ZERO {
            return Err(invalid("rate must be positive"));
        }
        metadata["rate"] = serde_json::json!(money::plain(r));
    }
    let kind = if credits.is_some() { InvoiceKind::CreditNote } else { InvoiceKind::Invoice };
    let ts = now_ts();
    tx.execute(
        "INSERT INTO invoices (uid, entity_id, direction, party_id, number, series, issue_date, tax_point, due_date, commodity, net, tax, gross, withheld, status, kind,
                               credits_invoice_id, document_id, external_ref, metadata, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, 'open', ?15, ?16, ?17, ?18, ?19, ?20, ?20)",
        params![
            new_uid(),
            entity.id,
            direction.as_str(),
            party_id,
            number,
            series,
            issue_date.to_string(),
            tax_point.to_string(),
            due_date.map(|d| d.to_string()),
            code,
            net,
            tax,
            gross,
            withheld,
            kind.as_str(),
            credits.map(|c| c.id),
            document_id,
            input.external_ref.trim(),
            metadata.to_string(),
            ts
        ],
    )?;
    let id = tx.last_insert_rowid();
    for (i, l) in rows.iter().enumerate() {
        tx.execute(
            "INSERT INTO invoice_lines (invoice_id, position, description, quantity, unit_price, net, tax_code, tax, account_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![id, i as i64 + 1, l.description, l.quantity, l.unit_price, l.net, l.tax_code, l.tax, l.account.as_ref().map(|a| a.id)],
        )?;
    }
    let mut invoice = get_invoice(tx, id)?;
    let recognition = if input.recognize {
        let control = control.ok_or_else(|| {
            invalid(match direction {
                Direction::Purchase => "recognition needs the payable_account",
                Direction::Sales => "recognition needs the receivable_account",
            })
        })?;
        let entry = recognize_in(tx, &entity, &invoice, &rows, default_account.as_ref(), &control, input.tax_account.as_deref())?;
        tx.execute("UPDATE invoices SET recognition_entry_id = ?2 WHERE id = ?1", params![id, entry.id])?;
        if let Some(doc) = document_id {
            tx.execute("INSERT OR IGNORE INTO document_links (document_id, entry_id, role, created_at) VALUES (?1, ?2, 'invoice', ?3)", params![doc, entry.id, now_ts()])?;
            if tx.changes() > 0 {
                let link_id = tx.last_insert_rowid();
                audit::log(tx, "document_links", link_id, "create", None, Some(serde_json::json!({"document_id": doc, "entry_id": entry.id, "role": "invoice"})))?;
            }
        }
        invoice = get_invoice(tx, id)?;
        Some(entry)
    } else {
        None
    };
    let action = if credits.is_some() { "record_credit_note" } else { "record" };
    let audit_event_id = audit::log_by(tx, "invoices", id, action, None, Some(serde_json::to_value(&invoice).map_err(crate::Error::from)?), "", "")?;
    let lines = lines(tx, id)?;
    Ok(Recorded { invoice, lines, recognition, audit_event_id })
}

/// The functional currency per unit of the invoice commodity on the tax point.
fn invoice_rate(conn: &Connection, entity: &Entity, invoice: &Invoice) -> AgentResult<Option<Decimal>> {
    if invoice.commodity == entity.currency {
        return Ok(None);
    }
    if let Some(rate) = invoice.metadata.get("rate").and_then(|v| v.as_str()).and_then(|s| money::parse(s).ok()) {
        return Ok(Some(rate));
    }
    match rates::rate_for(conn, &invoice.commodity, &entity.currency, invoice.tax_point)? {
        Some(l) => Ok(Some(l.rate)),
        None => Err(AgentError::new("missing_rate", format!("no {} to {} rate on or before {}; add a rate or give the rate in the invoice", invoice.commodity, entity.currency, invoice.tax_point))),
    }
}

/// A posting in the invoice commodity with its functional value. The account holds the
/// invoice commodity or the functional currency.
fn valued_posting(entity: &Entity, invoice: &Invoice, account: &Account, value: Decimal, amount: Decimal) -> AgentResult<PostingInput> {
    let quantity = if account.commodity == invoice.commodity {
        value
    } else if account.commodity == entity.currency {
        amount
    } else {
        return Err(invalid(format!("{} holds {}; use an account in {} or {}", account.path, account.commodity, invoice.commodity, entity.currency)));
    };
    Ok(PostingInput { amount: Some(amount), ..PostingInput::new(account.id, quantity) })
}

/// The tax block of an invoice line whose tax code exists for the entity on the tax point.
/// Returns the block, whether the engine adds tax lines for it, and the sum of those lines as an
/// unsigned value in the invoice commodity. Returns `None` when the code is absent or unknown,
/// or when its kind does not fit the invoice. The caller then books the tax on the tax account.
fn line_tax_block(conn: &Connection, entity: &Entity, invoice: &Invoice, line: &LineRow, functional: &dyn Fn(Decimal) -> AgentResult<Decimal>) -> AgentResult<Option<(TaxInput, bool, Decimal)>> {
    if line.tax_code.is_empty() {
        return Ok(None);
    }
    let code = match crate::tax::get_tax_code(conn, entity.id, &line.tax_code, invoice.tax_point) {
        Ok(code) => code,
        Err(crate::Error::NotFound(_) | crate::Error::Invalid(_)) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    // The engine signs the tax like the posting, so the tax must have the sign of the net.
    if line.tax != 0 && (line.tax < 0) != (line.net < 0) {
        return Ok(None);
    }
    let tax_value = Money::from_minor(line.tax, &invoice.commodity, invoice.gross.precision())?.major().abs();
    let own_kind = match invoice.direction {
        Direction::Purchase => "input",
        Direction::Sales => "output",
    };
    let block = TaxInput { code: code.code.clone(), tax_point: Some(invoice.tax_point), party_id: invoice.party_id, document_ref: invoice.label(), ..Default::default() };
    let kind = code.kind.as_str();
    Ok(if kind == own_kind {
        // The tax lines sum to the tax: the recoverable part on the tax account, the rest on the line account.
        let block = TaxInput { tax_amount: Some(functional(tax_value)?), ..block };
        Some((block, code.tax_account_id.is_some(), tax_value))
    } else if crate::tax::ZERO_TAX_KINDS.contains(&kind) && line.tax == 0 {
        Some((TaxInput { tax_amount: Some(Decimal::ZERO), ..block }, false, Decimal::ZERO))
    } else if kind == "reverse_charge" && invoice.direction == Direction::Purchase && line.tax == 0 {
        // The engine computes the tax from the rate. Its lines sum to zero.
        Some((block, false, Decimal::ZERO))
    } else {
        None
    })
}

/// Create the recognition entry of an invoice as a draft, dated on the tax point.
/// Purchase: debit the lines and the tax, credit the payable. Sales: debit the receivable,
/// credit the lines and the tax. A credit note turns every side around.
fn recognize_in(tx: &Transaction<'_>, entity: &Entity, invoice: &Invoice, rows: &[LineRow], default_account: Option<&Account>, control: &Account, tax_key: Option<&str>) -> AgentResult<JournalEntry> {
    let rate = invoice_rate(tx, entity, invoice)?;
    let fprec = entities::functional_precision(tx, entity.id)?;
    let prec = invoice.gross.precision();
    let major = |m: i64| -> AgentResult<Decimal> { Ok(Money::from_minor(m, &invoice.commodity, prec)?.major()) };
    let functional = |v: Decimal| -> AgentResult<Decimal> {
        Ok(match rate {
            Some(r) => money::round_to(v.checked_mul(r).ok_or_else(|| invalid("amount overflow"))?, fprec),
            None => v,
        })
    };
    let mut sign = invoice.direction.line_sign();
    if invoice.kind == InvoiceKind::CreditNote {
        sign = -sign;
    }
    let line_type = invoice.direction.line_type();
    // (account, value in the invoice commodity, memo, metadata, tax block and the sum of its tax lines in the invoice commodity)
    let mut legs: Vec<(Account, Decimal, String, serde_json::Value, Option<(TaxInput, Decimal)>)> = Vec::new();
    let mut taxes: BTreeMap<String, i64> = BTreeMap::new();
    if rows.is_empty() {
        let account = default_account.ok_or_else(|| invalid("recognition needs an account: give lines with accounts, or the account of the invoice"))?;
        legs.push((account.clone(), major(invoice.net.minor())?, String::new(), serde_json::json!({"invoice": invoice.uid}), None));
        if invoice.tax.minor() != 0 {
            taxes.insert(String::new(), invoice.tax.minor());
        }
    } else {
        for (i, l) in rows.iter().enumerate() {
            let account = l.account.as_ref().or(default_account).ok_or_else(|| invalid(format!("line {} has no account; give one, or the account of the invoice", i + 1)))?;
            let block = if l.net != 0 { line_tax_block(tx, entity, invoice, l, &functional)? } else { None };
            // A block without engine tax lines still needs the tax on the tax account.
            let fallback = match &block {
                Some((_, lines, _)) => !lines,
                None => true,
            };
            if l.net != 0 {
                let block = block.map(|(b, lines, tax_value)| (b, if lines { tax_value } else { Decimal::ZERO }));
                legs.push((account.clone(), major(l.net)?, l.description.clone(), serde_json::json!({"invoice": invoice.uid, "invoice_line": i + 1}), block));
            }
            if l.tax != 0 && fallback {
                *taxes.entry(l.tax_code.clone()).or_default() += l.tax;
            }
        }
    }
    for (account, _, _, _, _) in &legs {
        if account.r#type != line_type && !matches!(account.r#type, AccountType::Asset) {
            return Err(invalid(format!("{} is not an {} or an asset account", account.path, line_type.as_str())));
        }
    }
    if !taxes.is_empty() {
        let account = tax_account(tx, entity, invoice.direction, tax_key)?;
        for (code, tax) in &taxes {
            let memo = if code.is_empty() { "tax".to_string() } else { code.clone() };
            legs.push((account.clone(), major(*tax)?, memo, serde_json::json!({"invoice": invoice.uid, "tax_code": code}), None));
        }
    }
    let mut input = EntryInput::new(entity.id, invoice.tax_point);
    input.status = EntryStatus::Draft;
    input.origin = ORIGIN.into();
    input.payee = invoice.party_name.clone().unwrap_or_default();
    input.description = match invoice.kind {
        InvoiceKind::Invoice => format!("Invoice {}", invoice.label()),
        InvoiceKind::CreditNote => format!("Credit note {}", invoice.label()),
    };
    let mut total = Decimal::ZERO;
    for (account, value, memo, metadata, block) in legs {
        let value = value * sign;
        let amount = functional(value)?;
        total += amount;
        let mut p = valued_posting(entity, invoice, &account, value, amount)?;
        p.memo = memo;
        p.metadata = metadata;
        if let Some((block, tax_value)) = block {
            // The engine adds the tax lines of the block. The control posting must absorb them too.
            total += functional(tax_value * sign)?;
            p.tax = Some(block);
        }
        input.postings.push(p);
    }
    // The control posting takes the rest, so rounding never unbalances the entry.
    let control_value = -major(invoice.gross.minor())? * sign;
    let mut p = valued_posting(entity, invoice, control, control_value, -total)?;
    p.metadata = serde_json::json!({"invoice": invoice.uid, "invoice_control": true});
    input.postings.push(p);
    Ok(journal::create_entry_in_transaction(tx, input)?)
}

// ---------------------------------------------------------------------------
// Settlement
// ---------------------------------------------------------------------------

fn refresh_status(conn: &Connection, invoice_id: i64) -> AgentResult<Invoice> {
    let invoice = get_invoice(conn, invoice_id)?;
    conn.execute("UPDATE invoices SET status = ?2, updated_at = ?3 WHERE id = ?1 AND status <> 'cancelled'", params![invoice_id, invoice.status.as_str(), now_ts()])?;
    get_invoice(conn, invoice_id)
}

/// The booked functional value of `amount` of the invoice: from the control posting of a
/// recognition entry that is not void, else at the invoice rate. None when no value is known.
fn carrying_value(conn: &Connection, entity: &Entity, invoice: &Invoice, amount: i64) -> AgentResult<Option<i64>> {
    let fprec = entities::functional_precision(conn, entity.id)?;
    if let Some(entry_id) = invoice.recognition_entry_id {
        let entry = journal::get_entry(conn, entry_id)?;
        if entry.status != EntryStatus::Void {
            if let Some(p) = entry.postings.iter().find(|p| p.metadata.get("invoice_control").and_then(|v| v.as_bool()) == Some(true)) {
                let value = Decimal::from(p.amount.minor().abs()) * Decimal::from(amount) / Decimal::from(invoice.gross.minor());
                return Ok(Some(value.round().try_into().unwrap_or(0)));
            }
        }
    }
    let major = invoice.money(amount)?.major();
    let value = match invoice_rate(conn, entity, invoice) {
        Ok(None) => major,
        Ok(Some(rate)) => major * rate,
        Err(_) => return Ok(None),
    };
    Ok(Some(Money::from_major(money::round_to(value, fprec), &entity.currency, fprec)?.minor()))
}

/// True when the entry books the invoice lines again: it has expense postings (purchase) or
/// income postings (sales), and no posting on the control account.
fn books_lines_again(entry: &JournalEntry, invoice: &Invoice, control: Option<&Account>) -> bool {
    let line_type = invoice.direction.line_type();
    let has_lines = entry.postings.iter().any(|p| p.account_type == line_type);
    let on_control = control.is_some_and(|c| entry.postings.iter().any(|p| p.account_id == c.id));
    has_lines && !on_control
}

/// Move the expense or revenue postings of a bank entry to the control account, through the
/// ordinary entry edit. Amounts, the bank posting and its evidence stay.
fn convert_entry(tx: &Transaction<'_>, entity: &Entity, entry: &JournalEntry, invoice: &Invoice, control: &Account, amount: i64) -> AgentResult<()> {
    let line_type = invoice.direction.line_type();
    let lines: Vec<&Posting> = entry.postings.iter().filter(|p| p.account_type == line_type).collect();
    let mut input = EntryInput::new(entity.id, entry.date);
    input.payee = entry.payee.clone();
    input.description = entry.description.clone();
    input.notes = entry.notes.clone();
    input.status = entry.status;
    for p in &entry.postings {
        let mut pi = PostingInput {
            account_id: p.account_id,
            quantity: p.quantity.major(),
            amount: Some(p.amount.major()),
            memo: p.memo.clone(),
            metadata: p.metadata.clone(),
            external_id: p.external_id.clone(),
            fingerprint: p.fingerprint.clone(),
            ..Default::default()
        };
        if p.account_type == line_type {
            pi.quantity = if control.commodity == p.quantity.commodity() {
                p.quantity.major()
            } else if control.commodity == entity.currency {
                p.amount.major()
            } else if control.commodity == invoice.commodity && lines.len() == 1 {
                let value = invoice.money(amount)?.major();
                if p.amount.is_negative() {
                    -value
                } else {
                    value
                }
            } else {
                return Err(invalid(format!("cannot move {} to {}: the commodities differ", p.account_path, control.path)));
            };
            pi.account_id = control.id;
            if !pi.metadata.is_object() {
                pi.metadata = serde_json::json!({});
            }
            pi.metadata["invoice"] = serde_json::json!(invoice.uid);
            pi.metadata["converted_from_account"] = serde_json::json!(p.account_path);
        }
        input.postings.push(pi);
    }
    journal::update_entry_in_transaction(tx, entry.id, input)?;
    Ok(())
}

/// The bank posting of an entry: the one asset or liability posting that is not on the control account.
fn bank_posting(entry: &JournalEntry, control: Option<&Account>) -> Option<usize> {
    let found: Vec<usize> = entry
        .postings
        .iter()
        .enumerate()
        .filter(|(_, p)| matches!(p.account_type, AccountType::Asset | AccountType::Liability) && control.is_none_or(|c| c.id != p.account_id))
        .map(|(i, _)| i)
        .collect();
    (found.len() == 1).then(|| found[0])
}

/// Link an entry or a posting to an invoice and reduce its open balance. See the module
/// documentation for the duplicate rule and `docs/data-model.md` for the exchange difference.
pub fn settle(conn: &mut Connection, input: SettlementInput) -> AgentResult<Settled> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let settled = settle_in(&tx, input)?;
    tx.commit()?;
    Ok(settled)
}

fn settle_in(tx: &Transaction<'_>, input: SettlementInput) -> AgentResult<Settled> {
    let invoice = get_invoice_by_uid(tx, &input.invoice_uid)?;
    let entity = entities::get_entity(tx, invoice.entity_id)?;
    if invoice.status == InvoiceStatus::Cancelled {
        return Err(invalid(format!("invoice {} is cancelled", invoice.label())));
    }
    if invoice.kind == InvoiceKind::CreditNote {
        return Err(invalid("a credit note settles the invoice that it credits; settle that invoice instead"));
    }
    if input.kind == SettlementKind::CreditNote {
        return Err(invalid("record a credit note with credit_note; it settles the invoice"));
    }
    let prec = invoice.gross.precision();
    if money::round_to(input.amount, prec) != input.amount {
        return Err(invalid(format!("amount {} has more decimals than {} allows ({prec})", money::plain(input.amount), invoice.commodity)));
    }
    let amount = Money::from_major(input.amount, &invoice.commodity, prec)?.minor();
    if amount == 0 {
        return Err(invalid("the amount must not be zero"));
    }
    if amount < 0 && !matches!(input.kind, SettlementKind::Refund | SettlementKind::FxDifference) {
        return Err(invalid("only a refund or an fx_difference can have a negative amount"));
    }

    // The entry and the posting.
    let posting_ref: Option<(i64, i64)> = match input.posting_uid.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        Some(uid) => Some(
            tx.query_row("SELECT id, journal_entry_id FROM postings WHERE uid = ?1", [uid], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
                .ok_or_else(|| AgentError::new("not_found", format!("no posting with uid {uid}")))?,
        ),
        None => None,
    };
    let entry_id = match (input.entry_uid.as_deref().map(str::trim).filter(|e| !e.is_empty()), posting_ref) {
        (Some(uid), Some((_, posting_entry))) => {
            let id = proposal::entry_id_by_uid(tx, uid)?;
            if id != posting_entry {
                return Err(invalid(format!("posting {} does not belong to entry {uid}", input.posting_uid.as_deref().unwrap_or_default())));
            }
            Some(id)
        }
        (Some(uid), None) => Some(proposal::entry_id_by_uid(tx, uid)?),
        (None, Some((_, posting_entry))) => Some(posting_entry),
        (None, None) => None,
    };
    if entry_id.is_none() && input.kind.moves_money() {
        return Err(invalid(format!("a {} needs the entry that moves the money", input.kind.as_str())));
    }
    let mut entry = entry_id.map(|id| journal::get_entry(tx, id)).transpose()?;
    if let Some(e) = &entry {
        if e.entity_id != invoice.entity_id {
            return Err(invalid("the entry belongs to another entity"));
        }
        if e.status == EntryStatus::Void {
            return Err(invalid(format!("entry {} is void", e.uid)));
        }
        let same: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM settlements WHERE invoice_id = ?1 AND entry_id = ?2 AND kind = ?3 AND COALESCE(posting_id, 0) = COALESCE(?4, 0))",
            params![invoice.id, e.id, input.kind.as_str(), posting_ref.map(|p| p.0)],
            |r| r.get(0),
        )?;
        if same {
            return Err(AgentError::new("duplicate", format!("entry {} already settles invoice {} as a {}", e.uid, invoice.label(), input.kind.as_str())));
        }
    }

    // Limits.
    let settled = invoice.settled.minor();
    let after = settled.checked_add(amount).ok_or_else(|| invalid("amount overflow"))?;
    if after > invoice.gross.minor() && input.kind != SettlementKind::WriteOff {
        return Err(AgentError::new(
            "over_settlement",
            format!("invoice {} has {} open; a settlement of {} is too large", invoice.label(), money::plain(invoice.open.major()), money::plain(input.amount)),
        ));
    }
    if after < 0 {
        return Err(invalid("the settled total cannot fall below zero"));
    }
    if input.kind == SettlementKind::Withholding {
        let withheld: i64 = tx.query_row("SELECT COALESCE(SUM(amount), 0) FROM settlements WHERE invoice_id = ?1 AND kind = 'withholding'", [invoice.id], |r| r.get(0))?;
        if withheld + amount > invoice.withheld.minor() {
            return Err(AgentError::new("over_settlement", format!("invoice {} withholds {}; the withholding settlements exceed it", invoice.label(), money::plain(invoice.withheld.major()))));
        }
    }

    // The duplicate rule.
    let control = control_account(tx, &entity, &invoice, input.control_account.as_deref())?;
    let mut converted = false;
    let posting_index = match (&entry, posting_ref) {
        (Some(e), Some((pid, _))) => e.postings.iter().position(|p| p.id == pid),
        (Some(e), None) => bank_posting(e, control.as_ref()),
        _ => None,
    };
    if let Some(e) = entry.clone() {
        let recognized = match invoice.recognition_entry_id {
            Some(id) => id != e.id && journal::get_entry(tx, id)?.status != EntryStatus::Void,
            None => false,
        };
        if input.kind.moves_money() && recognized && books_lines_again(&e, &invoice, control.as_ref()) {
            let control = control.as_ref().ok_or_else(|| invalid("the entry books the invoice again; name the control account to convert it"))?;
            convert_entry(tx, &entity, &e, &invoice, control, amount)?;
            entry = Some(journal::get_entry(tx, e.id)?);
            converted = true;
        }
    }
    let posting = match (&entry, posting_index) {
        (Some(e), Some(i)) => e.postings.get(i).cloned(),
        _ => None,
    };

    // The exchange difference.
    let mut fx_difference = None;
    let mut fx_entry = None;
    if let (true, Some(p)) = (input.kind.moves_money(), &posting) {
        let paid = if p.quantity.commodity() == invoice.commodity && !p.quantity.is_zero() {
            let v = Decimal::from(p.amount.minor().abs()) * Decimal::from(amount.abs()) / Decimal::from(p.quantity.minor().abs());
            v.round().try_into().unwrap_or(0i64)
        } else {
            p.amount.minor().abs()
        };
        let paid = if amount < 0 { -paid } else { paid };
        if let Some(carrying) = carrying_value(tx, &entity, &invoice, amount)? {
            let d = paid - carrying;
            if d != 0 {
                let fprec = entities::functional_precision(tx, entity.id)?;
                let diff = Money::from_minor(d, &entity.currency, fprec)?;
                fx_difference = Some(diff);
                if let Some(fx) = resolve_account(tx, &entity, input.fx_account.as_deref())? {
                    let control = control.as_ref().ok_or_else(|| invalid("the exchange difference needs the control account of the invoice"))?;
                    fx_entry = Some(fx_entry_in(tx, &entity, &invoice, control, &fx, input.date, diff)?);
                }
            }
        }
    }

    tx.execute(
        "INSERT INTO settlements (uid, invoice_id, entry_id, posting_id, date, amount, kind, fx_difference_entry_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            new_uid(),
            invoice.id,
            entry.as_ref().map(|e| e.id),
            posting.as_ref().map(|p| p.id),
            input.date.to_string(),
            amount,
            input.kind.as_str(),
            fx_entry.as_ref().map(|e: &JournalEntry| e.id),
            now_ts()
        ],
    )?;
    let settlement = get_settlement(tx, tx.last_insert_rowid())?;
    let invoice = refresh_status(tx, invoice.id)?;
    let audit_event_id = audit::log_by(
        tx,
        "invoices",
        invoice.id,
        "settle",
        None,
        Some(serde_json::json!({"settlement": settlement, "status": invoice.status, "converted": converted, "fx_difference": fx_difference})),
        "",
        "",
    )?;
    Ok(Settled { settlement, invoice, converted, fx_difference, fx_entry, audit_event_id })
}

/// A draft entry that clears the exchange difference from the control account.
/// Purchase: a loss (positive difference) debits the FX account and credits the payable.
/// Sales: a gain (positive difference) debits the receivable and credits the FX account.
fn fx_entry_in(tx: &Transaction<'_>, entity: &Entity, invoice: &Invoice, control: &Account, fx: &Account, date: NaiveDate, diff: Money) -> AgentResult<JournalEntry> {
    let d = diff.major();
    let (control_amount, fx_amount) = match invoice.direction {
        Direction::Purchase => (-d, d),
        Direction::Sales => (d, -d),
    };
    let leg = |account: &Account, amount: Decimal| {
        let quantity = if account.commodity == entity.currency { amount } else { Decimal::ZERO };
        PostingInput { amount: Some(amount), metadata: serde_json::json!({"invoice": invoice.uid, "fx_difference": true}), ..PostingInput::new(account.id, quantity) }
    };
    let mut input = EntryInput::new(entity.id, date);
    input.status = EntryStatus::Draft;
    input.origin = ORIGIN.into();
    input.payee = invoice.party_name.clone().unwrap_or_default();
    input.description = format!("Exchange difference on invoice {}", invoice.label());
    input.postings = vec![leg(control, control_amount), leg(fx, fx_amount)];
    Ok(journal::create_entry_in_transaction(tx, input)?)
}

/// Record a credit note for an invoice. The credit note settles the invoice by its gross amount.
pub fn credit_note(conn: &mut Connection, credits_uid: &str, input: &InvoiceInput) -> AgentResult<CreditNoted> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let original = get_invoice_by_uid(&tx, credits_uid)?;
    if original.kind != InvoiceKind::Invoice {
        return Err(invalid("a credit note credits an invoice, not another credit note"));
    }
    if original.status == InvoiceStatus::Cancelled {
        return Err(invalid(format!("invoice {} is cancelled", original.label())));
    }
    let recorded = record_in(&tx, original.entity_id, input, Some(&original))?;
    if recorded.invoice.gross.minor() > original.open.minor() {
        return Err(AgentError::new(
            "over_settlement",
            format!("invoice {} has {} open; the credit note of {} is too large", original.label(), money::plain(original.open.major()), money::plain(recorded.invoice.gross.major())),
        ));
    }
    tx.execute(
        "INSERT INTO settlements (uid, invoice_id, entry_id, date, amount, kind, credit_note_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5, 'credit_note', ?6, ?7)",
        params![new_uid(), original.id, recorded.invoice.recognition_entry_id, recorded.invoice.issue_date.to_string(), recorded.invoice.gross.minor(), recorded.invoice.id, now_ts()],
    )?;
    let settlement = get_settlement(&tx, tx.last_insert_rowid())?;
    let invoice = refresh_status(&tx, original.id)?;
    let credit = refresh_status(&tx, recorded.invoice.id)?;
    audit::log_by(&tx, "invoices", invoice.id, "settle", None, Some(serde_json::json!({"settlement": settlement, "status": invoice.status})), "", "")?;
    tx.commit()?;
    Ok(CreditNoted { credit_note: Recorded { invoice: credit, ..recorded }, settlement, invoice })
}

/// Cancel an invoice that nothing settles. A draft recognition entry goes away. A posted
/// recognition entry must be void first. A cancelled credit note stops settling its invoice.
pub fn cancel(conn: &mut Connection, uid: &str, actor: &str, reason: &str) -> AgentResult<Invoice> {
    if reason.trim().is_empty() {
        return Err(invalid("give a reason for the cancellation"));
    }
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let before = get_invoice_by_uid(&tx, uid)?;
    if before.status == InvoiceStatus::Cancelled {
        return Err(invalid(format!("invoice {} is already cancelled", before.label())));
    }
    let count: i64 = tx.query_row("SELECT COUNT(*) FROM settlements WHERE invoice_id = ?1", [before.id], |r| r.get(0))?;
    if count > 0 {
        return Err(AgentError::new("settled", format!("invoice {} has {count} settlement(s); it cannot be cancelled", before.label())));
    }
    if let Some(entry_id) = before.recognition_entry_id {
        let entry = journal::get_entry(&tx, entry_id)?;
        match entry.status {
            EntryStatus::Posted => return Err(invalid(format!("the recognition entry {} is posted; void it first", entry.uid))),
            EntryStatus::Void => {}
            EntryStatus::Draft => {
                tx.execute("UPDATE statement_lines SET status = 'unmatched', posting_id = NULL, journal_entry_id = NULL WHERE journal_entry_id = ?1", [entry_id])?;
                tx.execute("DELETE FROM journal_entries WHERE id = ?1", [entry_id])?;
                audit::log(&tx, "journal_entries", entry_id, "delete", Some(serde_json::to_value(&entry).map_err(crate::Error::from)?), None)?;
            }
        }
    }
    if before.kind == InvoiceKind::CreditNote {
        tx.execute("DELETE FROM settlements WHERE credit_note_id = ?1", [before.id])?;
    }
    tx.execute("UPDATE invoices SET status = 'cancelled', updated_at = ?2 WHERE id = ?1", params![before.id, now_ts()])?;
    let after = get_invoice(&tx, before.id)?;
    audit::log_by(
        &tx,
        "invoices",
        before.id,
        "cancel",
        Some(serde_json::to_value(&before).map_err(crate::Error::from)?),
        Some(serde_json::to_value(&after).map_err(crate::Error::from)?),
        actor,
        reason,
    )?;
    tx.commit()?;
    Ok(after)
}

// ---------------------------------------------------------------------------
// Duplicate guard
// ---------------------------------------------------------------------------

/// Bank entries that look like the payment of an expense of `amount` to the party within
/// `window_days` of `date`, and that no settlement links yet. A bank entry has a statement line,
/// or the origin `import` or `rule`. It is a draft or posted. It has a bank posting of the amount
/// (money out) and at least one expense posting.
pub fn find_duplicate_expense(conn: &Connection, entity_id: i64, party_id: Option<i64>, amount: Money, date: NaiveDate, window_days: i64) -> AgentResult<Vec<DuplicateCandidate>> {
    let from = date - chrono::Duration::days(window_days.max(0));
    let to = date + chrono::Duration::days(window_days.max(0));
    find_duplicates(conn, entity_id, Direction::Purchase, party_id, amount, from, to)
}

fn find_duplicates(conn: &Connection, entity_id: i64, direction: Direction, party_id: Option<i64>, amount: Money, from: NaiveDate, to: NaiveDate) -> AgentResult<Vec<DuplicateCandidate>> {
    let ids: Vec<i64> = conn
        .prepare(
            "SELECT e.id FROM journal_entries e
             WHERE e.entity_id = ?1 AND e.status IN ('draft', 'posted') AND e.date BETWEEN ?2 AND ?3
               AND (?4 IS NULL OR e.payee_id = ?4)
               AND (e.origin IN ('import', 'rule') OR EXISTS (SELECT 1 FROM statement_lines s WHERE s.journal_entry_id = e.id))
               AND NOT EXISTS (SELECT 1 FROM settlements s WHERE s.entry_id = e.id)
               AND NOT EXISTS (SELECT 1 FROM invoices i WHERE i.recognition_entry_id = e.id)
             ORDER BY e.date, e.id",
        )?
        .query_map(params![entity_id, from.to_string(), to.to_string(), party_id], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    let line_type = direction.line_type();
    let mut out = Vec::new();
    for id in ids {
        let entry = journal::get_entry(conn, id)?;
        // Money out for a purchase, money in for a sale.
        let wanted = match direction {
            Direction::Purchase => -amount.minor(),
            Direction::Sales => amount.minor(),
        };
        let bank =
            entry.postings.iter().find(|p| matches!(p.account_type, AccountType::Asset | AccountType::Liability) && p.quantity.commodity() == amount.commodity() && p.quantity.minor() == wanted);
        let categories: Vec<String> = entry.postings.iter().filter(|p| p.account_type == line_type).map(|p| p.uid.clone()).collect();
        if let (Some(bank), false) = (bank, categories.is_empty()) {
            out.push(DuplicateCandidate {
                entry_id: entry.id,
                entry_uid: entry.uid.clone(),
                date: entry.date,
                status: entry.status,
                payee: entry.display_payee.clone(),
                bank_posting_uid: bank.uid.clone(),
                category_posting_uids: categories,
                amount,
            });
        }
    }
    Ok(out)
}

/// Possible duplicate bookings of an open invoice with a recognition entry: bank entries of the
/// amount to pay (gross less withheld), from the issue date less the window to the due date
/// (or the tax point) plus the window.
pub fn invoice_duplicates(conn: &Connection, invoice_id: i64) -> AgentResult<Vec<DuplicateCandidate>> {
    let invoice = get_invoice(conn, invoice_id)?;
    if invoice.kind != InvoiceKind::Invoice || invoice.status == InvoiceStatus::Cancelled || invoice.open.is_zero() {
        return Ok(Vec::new());
    }
    let due = invoice.due_date.unwrap_or(invoice.tax_point).max(invoice.tax_point);
    let from = invoice.issue_date.min(invoice.tax_point) - chrono::Duration::days(DUPLICATE_WINDOW_DAYS);
    let to = due + chrono::Duration::days(DUPLICATE_WINDOW_DAYS);
    let pay = invoice.money(invoice.gross.minor() - invoice.withheld.minor())?;
    find_duplicates(conn, invoice.entity_id, invoice.direction, invoice.party_id, pay, from, to)
}

// ---------------------------------------------------------------------------
// Entry edits
// ---------------------------------------------------------------------------

/// A settlement link set aside while an entry edit replaces its postings.
pub(crate) struct DetachedSettlement {
    id: i64,
    posting_id: i64,
}

/// The settlements that point to postings of an entry, before the edit replaces them.
pub(crate) fn detach_posting_settlements(conn: &Connection, entry_id: i64) -> crate::Result<Vec<DetachedSettlement>> {
    let mut stmt = conn.prepare("SELECT s.id, s.posting_id FROM settlements s JOIN postings p ON p.id = s.posting_id WHERE p.journal_entry_id = ?1")?;
    let rows = stmt.query_map([entry_id], |r| Ok(DetachedSettlement { id: r.get(0)?, posting_id: r.get(1)? }))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// Point each settlement to the replacement of its posting. Without a replacement, the
/// settlement keeps its entry and loses its posting.
pub(crate) fn reattach_posting_settlements(conn: &Connection, detached: Vec<DetachedSettlement>, replaced: &std::collections::HashMap<i64, i64>) -> crate::Result<()> {
    for s in detached {
        conn.execute("UPDATE settlements SET posting_id = ?2 WHERE id = ?1", params![s.id, replaced.get(&s.posting_id)])?;
    }
    Ok(())
}
