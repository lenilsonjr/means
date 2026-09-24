//! The machine write interface: an agent drafts, a proposal previews, a human confirms.
//!
//! An agent creates drafts with origin `agent`. It never posts. `propose` validates a draft and
//! returns a preview with a digest. `confirm` posts the draft only when the digest still matches,
//! so a human confirms exactly the entry that they reviewed. Every error carries a stable code.

use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};

use crate::model::*;
use crate::money::{self, Money};
use crate::{accounts, audit, entities, journal, tags, Error};

/// A refusal with a stable snake_case code for machine clients.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct AgentError {
    pub code: &'static str,
    pub message: String,
}

impl AgentError {
    pub fn new(code: &'static str, message: impl Into<String>) -> AgentError {
        AgentError { code, message: message.into() }
    }
    fn invalid(message: impl Into<String>) -> AgentError {
        AgentError::new("invalid_input", message)
    }
}

impl From<Error> for AgentError {
    fn from(e: Error) -> AgentError {
        match e {
            Error::NotFound(m) => AgentError::new("not_found", m),
            Error::Invalid(m) | Error::Parse(m) | Error::Conflict(m) => AgentError::new("invalid_input", m),
            Error::Locked(m) => AgentError::new("locked_period", m),
            Error::Unbalanced(m) => AgentError::new("unbalanced", m),
            Error::Json(e) => AgentError::new("invalid_input", e.to_string()),
            Error::Db(e) => AgentError::new("internal", format!("database: {e}")),
            Error::Other(e) => AgentError::new("internal", e.to_string()),
        }
    }
}

impl From<rusqlite::Error> for AgentError {
    fn from(e: rusqlite::Error) -> AgentError {
        Error::Db(e).into()
    }
}

pub type AgentResult<T> = std::result::Result<T, AgentError>;

/// The input document of `means entry draft`. Numbers are decimal strings.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftDocument {
    pub date: String,
    #[serde(default)]
    pub payee: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub postings: Vec<DraftPosting>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftPosting {
    /// An account path inside the entity, or an account uid.
    pub account: String,
    /// Omit on exactly one posting to let it balance the entry.
    #[serde(default)]
    pub quantity: Option<String>,
    #[serde(default)]
    pub commodity: Option<String>,
    /// The value in the entity's functional currency.
    #[serde(default)]
    pub amount: Option<String>,
    /// Functional currency per unit of the account's commodity.
    #[serde(default)]
    pub rate: Option<String>,
    #[serde(default)]
    pub memo: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct Drafted {
    pub entry: JournalEntry,
    pub audit_event_id: i64,
}

/// Find an entry by its uid.
pub fn entry_id_by_uid(conn: &Connection, uid: &str) -> AgentResult<i64> {
    conn.query_row("SELECT id FROM journal_entries WHERE uid = ?1", [uid.trim()], |r| r.get(0)).optional()?.ok_or_else(|| AgentError::new("not_found", format!("no journal entry with uid {uid}")))
}

/// Find a uid by an internal id. Returns None for None.
pub fn uid_of(conn: &Connection, table: &str, id: Option<i64>) -> AgentResult<Option<String>> {
    let Some(id) = id else { return Ok(None) };
    let table = match table {
        "journal_entries" | "entities" | "accounts" | "postings" => table,
        other => return Err(AgentError::invalid(format!("no uid column on {other}"))),
    };
    Ok(conn.query_row(&format!("SELECT uid FROM {table} WHERE id = ?1"), [id], |r| r.get(0)).optional()?)
}

/// Resolve an account path or uid inside one entity and check that it can take a posting.
pub fn resolve_account(conn: &Connection, entity: &Entity, key: &str) -> AgentResult<Account> {
    let key = key.trim();
    if key.is_empty() {
        return Err(AgentError::new("unknown_account", "a posting has no account"));
    }
    let by_uid: Option<i64> = conn.query_row("SELECT id FROM accounts WHERE uid = ?1", [key], |r| r.get(0)).optional()?;
    let account = match by_uid {
        Some(id) => accounts::get_account(conn, id)?,
        None => accounts::find_by_path(conn, entity.id, key)?.ok_or_else(|| AgentError::new("unknown_account", format!("{} has no account {key}", entity.name)))?,
    };
    check_account(conn, entity, &account)?;
    Ok(account)
}

fn check_account(conn: &Connection, entity: &Entity, account: &Account) -> AgentResult<()> {
    if account.entity_id != entity.id {
        return Err(AgentError::new("unknown_account", format!("account {} belongs to another entity", account.path)));
    }
    let has_children: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM accounts WHERE parent_id = ?1)", [account.id], |r| r.get(0))?;
    if account.placeholder || has_children {
        return Err(AgentError::new("not_leaf_account", format!("{} groups other accounts; post to a leaf account", account.path)));
    }
    if account.is_closed() {
        return Err(AgentError::invalid(format!("{} is closed", account.path)));
    }
    Ok(())
}

fn decimal(field: &str, value: &str) -> AgentResult<Decimal> {
    money::parse(value).map_err(|_| AgentError::invalid(format!("{field} must be a decimal string, got {value:?}")))
}

fn posting_input(conn: &Connection, entity: &Entity, fprec: u32, index: usize, p: &DraftPosting) -> AgentResult<PostingInput> {
    let account = resolve_account(conn, entity, &p.account)?;
    let metadata = match &p.metadata {
        serde_json::Value::Null => serde_json::json!({}),
        serde_json::Value::Object(_) => p.metadata.clone(),
        _ => return Err(AgentError::invalid(format!("posting {} metadata must be an object", index + 1))),
    };
    let commodity = p.commodity.as_deref().map(|c| c.trim().to_ascii_uppercase()).filter(|c| !c.is_empty());
    let Some(quantity) = p.quantity.as_deref() else {
        if p.amount.is_some() || p.rate.is_some() {
            return Err(AgentError::invalid(format!("posting {} balances the entry, so it cannot carry an amount or a rate", index + 1)));
        }
        if commodity.as_deref().is_some_and(|c| c != account.commodity) {
            return Err(AgentError::invalid(format!("posting {} names {}, but {} holds {}", index + 1, commodity.unwrap_or_default(), account.path, account.commodity)));
        }
        return Ok(PostingInput { memo: p.memo.clone(), metadata, ..PostingInput::balancing(account.id) });
    };
    let quantity = decimal("quantity", quantity)?;
    if let Some(c) = commodity.filter(|c| *c != account.commodity) {
        // A value in another commodity: the engine converts it at the entry-date rate.
        if p.amount.is_some() || p.rate.is_some() {
            return Err(AgentError::invalid(format!("posting {} is valued in {c}; give the quantity in {} to set an amount or a rate", index + 1, account.commodity)));
        }
        entities::get_commodity_by_code(conn, &c)?;
        return Ok(PostingInput { memo: p.memo.clone(), metadata, ..PostingInput::valued(account.id, quantity, &c) });
    }
    let amount = p.amount.as_deref().map(|a| decimal("amount", a)).transpose()?;
    let rate = p.rate.as_deref().map(|r| decimal("rate", r)).transpose()?;
    if rate.is_some() && account.commodity == entity.currency {
        return Err(AgentError::invalid(format!("posting {} is in the functional currency {}; remove its rate", index + 1, entity.currency)));
    }
    let from_rate = rate.map(|r| quantity.checked_mul(r).map(|v| money::round_to(v, fprec)).ok_or_else(|| AgentError::invalid("amount overflow"))).transpose()?;
    let amount = match (amount, from_rate) {
        (Some(a), Some(r)) if money::round_to(a, fprec) != r => {
            return Err(AgentError::invalid(format!("posting {}: quantity times rate is {}, not the amount {}", index + 1, money::plain(r), money::plain(a))));
        }
        (Some(a), _) => Some(a),
        (None, r) => r,
    };
    Ok(PostingInput { amount, memo: p.memo.clone(), metadata, ..PostingInput::new(account.id, quantity) })
}

fn parse_tags(list: &[String]) -> AgentResult<Vec<(String, String)>> {
    let mut specs = Vec::with_capacity(list.len());
    for tag in list {
        let tag = tag.trim();
        if tag.is_empty() || tag.chars().any(char::is_whitespace) {
            return Err(AgentError::invalid(format!("tag {tag:?} must be one word, such as project=alpha")));
        }
        specs.push(tag.replacen('=', ":", 1));
    }
    Ok(tags::parse(&specs.join(" "))?)
}

/// Create a draft with origin `agent`. It never posts, whatever the posting policy.
pub fn create_draft(conn: &mut Connection, entity_id: i64, doc: &DraftDocument) -> AgentResult<Drafted> {
    let tx = conn.transaction()?;
    let entity = entities::get_entity(&tx, entity_id)?;
    let date = crate::parse_date(&doc.date).map_err(|_| AgentError::invalid(format!("date must be YYYY-MM-DD, got {:?}", doc.date)))?;
    if doc.postings.len() < 2 {
        return Err(AgentError::invalid("a journal entry needs at least two postings"));
    }
    if doc.postings.iter().filter(|p| p.quantity.is_none()).count() > 1 {
        return Err(AgentError::invalid("only one posting can omit its quantity to balance the entry"));
    }
    if !(doc.metadata.is_null() || doc.metadata.is_object()) {
        return Err(AgentError::invalid("metadata must be an object"));
    }
    let tag_pairs = parse_tags(&doc.tags)?;
    let fprec = entities::functional_precision(&tx, entity.id)?;
    let mut input = EntryInput::new(entity.id, date);
    input.payee = doc.payee.clone();
    input.description = doc.description.clone();
    input.notes = doc.notes.clone();
    input.status = EntryStatus::Draft;
    input.origin = "agent".into();
    for (i, p) in doc.postings.iter().enumerate() {
        input.postings.push(posting_input(&tx, &entity, fprec, i, p)?);
    }
    let entry = journal::create_entry_in_transaction(&tx, input)?;
    tags::set_tags_in_transaction(&tx, entry.id, &tag_pairs)?;
    let metadata = if doc.metadata.is_null() { serde_json::json!({}) } else { doc.metadata.clone() };
    audit::log(&tx, "journal_entries", entry.id, "agent_draft", None, Some(serde_json::json!({"origin": "agent", "metadata": metadata, "postings": doc.postings.len()})))?;
    let audit_event_id = tx.last_insert_rowid();
    let entry = journal::get_entry(&tx, entry.id)?;
    tx.commit()?;
    Ok(Drafted { entry, audit_event_id })
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PreviewPosting {
    pub account_uid: String,
    pub account_path: String,
    pub quantity: Money,
    /// The value in the entity's functional currency.
    pub amount: Money,
    pub rate: Option<String>,
    pub rate_source: String,
    pub memo: String,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PreviewTotals {
    pub debit: Money,
    pub credit: Money,
    pub balance: Money,
}

/// The preview of a draft that a human confirms. The digest covers every other field.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Proposal {
    pub entry_uid: String,
    pub entity_uid: String,
    pub date: NaiveDate,
    pub status: EntryStatus,
    pub payee: String,
    pub description: String,
    pub notes: String,
    pub tags: Vec<String>,
    pub functional_currency: String,
    pub postings: Vec<PreviewPosting>,
    pub totals: PreviewTotals,
    pub warnings: Vec<String>,
    pub proposal_digest: String,
}

/// Validate a draft and preview it. Writes nothing.
pub fn propose(conn: &Connection, entry_id: i64) -> AgentResult<Proposal> {
    let entry = journal::get_entry(conn, entry_id)?;
    if entry.status != EntryStatus::Draft {
        return Err(AgentError::invalid(format!("entry {} is {}, not a draft", entry.uid, entry.status.as_str())));
    }
    if entry.postings.len() < 2 {
        return Err(AgentError::invalid(format!("entry {} needs at least two postings", entry.uid)));
    }
    let entity = entities::get_entity(conn, entry.entity_id)?;
    let fprec = entities::functional_precision(conn, entity.id)?;
    let mut warnings = Vec::new();
    let mut postings = Vec::with_capacity(entry.postings.len());
    let zero = Money::from_minor(0, &entity.currency, fprec)?;
    let (mut debit, mut credit, mut balance) = (zero, zero, zero);
    for p in &entry.postings {
        let account = accounts::get_account(conn, p.account_id)?;
        check_account(conn, &entity, &account)?;
        if p.rate_source == "missing" {
            return Err(AgentError::new(
                "missing_rate",
                format!("no {} to {} rate on or before {} for {}; add a rate, then revalue the draft", p.quantity.commodity(), entity.currency, entry.date, account.path),
            ));
        }
        match p.rate_source.as_str() {
            "nearest" => warnings.push(format!("{}: the {} rate comes from the nearest earlier date", account.path, p.quantity.commodity())),
            "input" => warnings.push(format!("{}: the rate was given with the draft, not taken from the rate table", account.path)),
            _ => {}
        }
        balance = balance.checked_add(p.amount)?;
        if p.amount.is_negative() {
            credit = credit.checked_add(p.amount.checked_neg()?)?;
        } else {
            debit = debit.checked_add(p.amount)?;
        }
        postings.push(PreviewPosting {
            account_uid: account.uid.clone(),
            account_path: account.path.clone(),
            quantity: p.quantity,
            amount: p.amount,
            rate: p.rate.map(money::plain),
            rate_source: p.rate_source.clone(),
            memo: p.memo.clone(),
            metadata: p.metadata.clone(),
        });
    }
    if !balance.is_zero() {
        return Err(AgentError::new("unbalanced", format!("postings sum to {} {}", money::plain(balance.major()), entity.currency)));
    }
    journal::check_lock(&entity, entry.date)?;
    if entry.date > crate::today() {
        warnings.push(format!("the entry date {} is in the future", entry.date));
    }
    if entry.payee.trim().is_empty() {
        warnings.push("the entry has no payee".into());
    }
    let mut proposal = Proposal {
        entry_uid: entry.uid.clone(),
        entity_uid: entity.uid.clone(),
        date: entry.date,
        status: entry.status,
        payee: entry.payee.clone(),
        description: entry.description.clone(),
        notes: entry.notes.clone(),
        tags: entry.tags.clone(),
        functional_currency: entity.currency.clone(),
        postings,
        totals: PreviewTotals { debit, credit, balance },
        warnings,
        proposal_digest: String::new(),
    };
    proposal.proposal_digest = digest(&proposal)?;
    Ok(proposal)
}

/// sha256 over the canonical JSON of the preview without its digest.
/// `serde_json::Value` keeps object keys sorted, so the bytes do not depend on field order.
fn digest(proposal: &Proposal) -> AgentResult<String> {
    let mut value = serde_json::to_value(proposal).map_err(Error::from)?;
    if let Some(object) = value.as_object_mut() {
        object.remove("proposal_digest");
    }
    let bytes = serde_json::to_vec(&value).map_err(Error::from)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

#[derive(Debug, Clone)]
pub struct Confirmed {
    pub entry: JournalEntry,
    pub audit_event_id: i64,
}

/// Post a draft when `digest` equals its current proposal digest, then mark it reviewed and
/// record the actor. One transaction: a stale digest or a refusal writes nothing.
pub fn confirm(conn: &mut Connection, entry_id: i64, digest: &str, actor: &str) -> AgentResult<Confirmed> {
    let actor = actor.trim();
    if actor.is_empty() {
        return Err(AgentError::invalid("give the name of the person who confirms"));
    }
    let tx = conn.transaction()?;
    let proposal = propose(&tx, entry_id)?;
    if proposal.proposal_digest != digest.trim() {
        return Err(AgentError::new("stale_proposal", format!("entry {} changed since the proposal; propose it again and review the new preview", proposal.entry_uid)));
    }
    journal::post_entry_in_transaction(&tx, entry_id)?;
    journal::mark_reviewed(&tx, &[entry_id])?;
    let audit_event_id =
        audit::log_by(&tx, "journal_entries", entry_id, "confirm", None, Some(serde_json::json!({"actor": actor, "action": "confirm", "proposal_digest": proposal.proposal_digest})), actor, "")?;
    let entry = journal::get_entry(&tx, entry_id)?;
    tx.commit()?;
    Ok(Confirmed { entry, audit_event_id })
}

/// A statement line that evidences an entry or one of its postings.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Evidence {
    pub statement_line_id: i64,
    pub import_id: i64,
    pub posting_uid: Option<String>,
    pub date: Option<String>,
    pub description: String,
    pub status: String,
}

pub fn evidence(conn: &Connection, entry_id: i64) -> AgentResult<Vec<Evidence>> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.import_id, p.uid, s.date, s.description, s.status FROM statement_lines s LEFT JOIN postings p ON p.id = s.posting_id
         WHERE s.journal_entry_id = ?1 OR p.journal_entry_id = ?1 ORDER BY s.id",
    )?;
    let rows =
        stmt.query_map(params![entry_id], |r| Ok(Evidence { statement_line_id: r.get(0)?, import_id: r.get(1)?, posting_uid: r.get(2)?, date: r.get(3)?, description: r.get(4)?, status: r.get(5)? }))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}
