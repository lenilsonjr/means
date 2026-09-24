//! Inter-vault links: typed, audited links between two journal entries of different entities.
//!
//! A link joins entry A in one vault to entry B in another vault. It records the native amount
//! of the intercompany side of each entry and the implied rate between them. The pair has one
//! lifecycle:
//!
//! - The link is `draft` while either entry is a draft. It becomes `posted` when both are posted.
//! - A void of one entry voids the other entry in the same transaction and marks the link `void`.
//!   The engine refuses the void when the period of the other entity is locked.
//! - A deleted draft deletes its links.
//!
//! `counterpart_id` on a journal entry stays as it is. A transfer between entities sets it and
//! also writes a link.

use std::collections::HashSet;

use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};
use rust_decimal::Decimal;
use serde::Serialize;

use crate::model::{EntryStatus, JournalEntry};
use crate::money::{self, Money};
use crate::{accounts, audit, entities, groups, journal, new_uid, now_ts, Error, Result};

/// The kinds of a link.
pub const KINDS: &[&str] = &["transfer", "capital", "loan", "interest", "dividend", "service", "recharge", "settlement", "other"];

/// The kind that a transfer gets when the caller gives none.
pub const DEFAULT_KIND: &str = "transfer";

/// Check a kind and return it in its stored form. An empty kind gives `transfer`.
pub fn parse_kind(kind: &str) -> Result<String> {
    let kind = kind.trim().to_ascii_lowercase();
    if kind.is_empty() {
        return Ok(DEFAULT_KIND.into());
    }
    if !KINDS.contains(&kind.as_str()) {
        return Err(Error::Invalid(format!("unknown link kind {kind:?}; use one of {}", KINDS.join(", "))));
    }
    Ok(kind)
}

/// One stored link.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Link {
    #[serde(skip)]
    pub id: i64,
    pub uid: String,
    #[serde(skip)]
    pub entry_a_id: i64,
    pub entry_a_uid: String,
    #[serde(skip)]
    pub entry_b_id: i64,
    pub entry_b_uid: String,
    pub kind: String,
    /// The native amount of the intercompany side of entry A when the link was made.
    pub amount_a: Money,
    /// The native amount of the intercompany side of entry B when the link was made.
    pub amount_b: Money,
    /// Units of the commodity of B per unit of the commodity of A. `1` for one commodity.
    pub rate: String,
    pub note: String,
    /// draft | posted | void
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    /// Notes about the amounts that the engine chose. The engine does not store them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// One side of a link in a list.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LinkSide {
    pub entry_uid: String,
    pub entity_uid: String,
    pub entity_name: String,
    pub date: NaiveDate,
    pub entry_status: EntryStatus,
    /// The amount that the link stored.
    pub amount: Money,
    /// The amount that the entry holds now.
    pub current_amount: Money,
}

/// A link with both entries, for lists and group reports.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LinkView {
    pub uid: String,
    pub kind: String,
    pub status: String,
    pub rate: String,
    pub note: String,
    pub entry_a: LinkSide,
    pub entry_b: LinkSide,
    /// True when the current amount of A at the stored rate equals the current amount of B,
    /// within one minor unit of the commodity of B.
    pub reconciled: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// A booked posting on an intercompany account whose entry has no link.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UnlinkedPosting {
    pub entry_uid: String,
    pub posting_uid: String,
    pub date: NaiveDate,
    pub payee: String,
    pub account_uid: String,
    pub account_path: String,
    pub quantity: Money,
    pub amount: Money,
    pub memo: String,
}

const LINK_SELECT: &str = "SELECT l.id, l.uid, l.entry_a_id, ea.uid, l.entry_b_id, eb.uid, l.kind,
        l.amount_a, l.commodity_a, ca.precision, l.amount_b, l.commodity_b, cb.precision,
        l.rate, l.note, l.status, l.created_at, l.updated_at
     FROM intervault_links l
     JOIN journal_entries ea ON ea.id = l.entry_a_id
     JOIN journal_entries eb ON eb.id = l.entry_b_id
     JOIN commodities ca ON ca.code = l.commodity_a
     JOIN commodities cb ON cb.code = l.commodity_b";

fn row_to_link(r: &rusqlite::Row<'_>) -> rusqlite::Result<Link> {
    Ok(Link {
        id: r.get(0)?,
        uid: r.get(1)?,
        entry_a_id: r.get(2)?,
        entry_a_uid: r.get(3)?,
        entry_b_id: r.get(4)?,
        entry_b_uid: r.get(5)?,
        kind: r.get(6)?,
        amount_a: money::from_row(r, 7, 8, 9)?,
        amount_b: money::from_row(r, 10, 11, 12)?,
        rate: r.get(13)?,
        note: r.get(14)?,
        status: r.get(15)?,
        created_at: r.get(16)?,
        updated_at: r.get(17)?,
        warnings: Vec::new(),
    })
}

fn get_link(conn: &Connection, id: i64) -> Result<Link> {
    conn.query_row(&format!("{LINK_SELECT} WHERE l.id = ?1"), [id], row_to_link).optional()?.ok_or_else(|| Error::NotFound(format!("inter-vault link {id}")))
}

/// Find a link by its uid.
pub fn get_link_by_uid(conn: &Connection, uid: &str) -> Result<Link> {
    conn.query_row(&format!("{LINK_SELECT} WHERE l.uid = ?1"), [uid.trim()], row_to_link).optional()?.ok_or_else(|| Error::NotFound(format!("no inter-vault link with uid {}", uid.trim())))
}

/// The links of one entry, in any status.
pub fn links_for_entry(conn: &Connection, entry_id: i64) -> Result<Vec<Link>> {
    let mut stmt = conn.prepare(&format!("{LINK_SELECT} WHERE l.entry_a_id = ?1 OR l.entry_b_id = ?1 ORDER BY l.id"))?;
    let rows = stmt.query_map([entry_id], row_to_link)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn entry_by_uid(conn: &Connection, uid: &str) -> Result<JournalEntry> {
    let id: i64 =
        conn.query_row("SELECT id FROM journal_entries WHERE uid = ?1", [uid.trim()], |r| r.get(0)).optional()?.ok_or_else(|| Error::NotFound(format!("no journal entry with uid {}", uid.trim())))?;
    journal::get_entry(conn, id)
}

/// The first posting with the largest functional amount. Ties keep the earlier posting.
fn largest<'a>(postings: impl Iterator<Item = &'a crate::model::Posting>) -> Option<&'a crate::model::Posting> {
    postings.fold(None, |best: Option<&crate::model::Posting>, p| match best {
        Some(b) if b.amount.minor().unsigned_abs() >= p.amount.minor().unsigned_abs() => Some(b),
        _ => Some(p),
    })
}

/// The native amount of the intercompany side of an entry, and a warning when the engine had
/// to choose. The side is the sum of the postings on accounts with `intercompany = 1`. Without
/// such postings, the side is the largest posting of the entry.
fn side_amount(conn: &Connection, entry: &JournalEntry) -> Result<(Money, Option<String>)> {
    let mut flagged_accounts = HashSet::new();
    for p in &entry.postings {
        if accounts::get_account(conn, p.account_id)?.intercompany {
            flagged_accounts.insert(p.account_id);
        }
    }
    let flagged: Vec<_> = entry.postings.iter().filter(|p| flagged_accounts.contains(&p.account_id)).collect();
    let mut warning = None;
    if let Some(lead) = largest(flagged.iter().copied()) {
        let commodity = lead.quantity.commodity().to_string();
        let same: Vec<_> = flagged.iter().filter(|p| p.quantity.commodity() == commodity).collect();
        if same.len() != flagged.len() {
            warning = Some(format!("entry {} has intercompany postings in more than one commodity; the link uses the {commodity} postings", entry.uid));
        }
        let mut sum = Money::from_minor(0, &commodity, lead.quantity.precision())?;
        for p in same {
            sum = sum.checked_add(p.quantity)?;
        }
        if !sum.is_zero() {
            return Ok((sum, warning));
        }
        warning = Some(format!("the intercompany postings of entry {} sum to zero; the link uses the largest posting", entry.uid));
    }
    let lead = largest(entry.postings.iter()).ok_or_else(|| Error::Invalid(format!("entry {} has no postings", entry.uid)))?;
    let warning = warning.unwrap_or_else(|| format!("entry {} has no intercompany posting; the link uses its largest posting on {}", entry.uid, lead.account_path));
    Ok((lead.quantity, Some(warning)))
}

/// Units of the commodity of B per unit of the commodity of A. One commodity gives `1`.
fn implied_rate(a: Money, b: Money) -> Result<String> {
    if a.commodity() == b.commodity() {
        return Ok("1".into());
    }
    if a.is_zero() {
        return Err(Error::Invalid("the amount of entry A is zero, so the link has no rate".into()));
    }
    let rate = b.major().abs().checked_div(a.major().abs()).ok_or_else(|| Error::Invalid("the link rate overflows".into()))?;
    Ok(money::plain(rate.round_dp(12).normalize()))
}

/// True when `a` at `rate` equals `b` within one minor unit of the commodity of `b`.
fn amounts_agree(a: Money, b: Money, rate: &str) -> bool {
    let Ok(rate) = money::parse(rate) else { return false };
    let Some(expected) = a.major().abs().checked_mul(rate) else { return false };
    let tolerance = Decimal::new(1, b.precision());
    (money::round_to(expected, b.precision()) - b.major().abs()).abs() <= tolerance
}

fn status_of_pair(a: &JournalEntry, b: &JournalEntry) -> &'static str {
    if a.status == EntryStatus::Posted && b.status == EntryStatus::Posted {
        "posted"
    } else {
        "draft"
    }
}

/// Link two entries by their uids. See `link_in_transaction` for the rules.
pub fn link(conn: &mut Connection, entry_a_uid: &str, entry_b_uid: &str, kind: &str, note: &str) -> Result<Link> {
    let tx = conn.transaction()?;
    let a = entry_by_uid(&tx, entry_a_uid)?;
    let b = entry_by_uid(&tx, entry_b_uid)?;
    let link = link_in_transaction(&tx, a.id, b.id, kind, note)?;
    tx.commit()?;
    Ok(link)
}

/// Link entry A to entry B inside the caller's transaction.
///
/// Both entries must exist, belong to different entities, and not be void. The pair must not
/// have a link yet, in either order. The link is `draft` when either entry is a draft.
pub(crate) fn link_in_transaction(conn: &Connection, entry_a_id: i64, entry_b_id: i64, kind: &str, note: &str) -> Result<Link> {
    let kind = parse_kind(kind)?;
    if entry_a_id == entry_b_id {
        return Err(Error::Invalid("an entry cannot link to itself".into()));
    }
    let a = journal::get_entry(conn, entry_a_id)?;
    let b = journal::get_entry(conn, entry_b_id)?;
    if a.entity_id == b.entity_id {
        let entity = entities::get_entity(conn, a.entity_id)?;
        return Err(Error::Invalid(format!("entries {} and {} both belong to {}; a link joins entries of two different entities", a.uid, b.uid, entity.name)));
    }
    for e in [&a, &b] {
        if e.status == EntryStatus::Void {
            return Err(Error::Invalid(format!("entry {} is void; link entries that are drafts or posted", e.uid)));
        }
    }
    let existing: Option<String> =
        conn.query_row("SELECT uid FROM intervault_links WHERE (entry_a_id = ?1 AND entry_b_id = ?2) OR (entry_a_id = ?2 AND entry_b_id = ?1)", params![a.id, b.id], |r| r.get(0)).optional()?;
    if let Some(uid) = existing {
        return Err(Error::Conflict(format!("entries {} and {} already have link {uid}", a.uid, b.uid)));
    }
    let (amount_a, warning_a) = side_amount(conn, &a)?;
    let (amount_b, warning_b) = side_amount(conn, &b)?;
    let rate = implied_rate(amount_a, amount_b)?;
    let ts = now_ts();
    conn.execute(
        "INSERT INTO intervault_links (uid, entry_a_id, entry_b_id, kind, amount_a, commodity_a, amount_b, commodity_b, rate, note, status, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)",
        params![new_uid(), a.id, b.id, kind, amount_a.minor(), amount_a.commodity(), amount_b.minor(), amount_b.commodity(), rate, note.trim(), status_of_pair(&a, &b), ts],
    )?;
    let mut link = get_link(conn, conn.last_insert_rowid())?;
    audit::log(conn, "intervault_links", link.id, "link", None, Some(serde_json::to_value(&link)?))?;
    link.warnings = [warning_a, warning_b].into_iter().flatten().collect();
    Ok(link)
}

/// Remove a link while both entries are drafts. Returns the removed link.
pub fn unlink(conn: &mut Connection, link_uid: &str, reason: &str) -> Result<Link> {
    if reason.trim().is_empty() {
        return Err(Error::Invalid("give a reason to remove the link".into()));
    }
    let tx = conn.transaction()?;
    let link = get_link_by_uid(&tx, link_uid)?;
    for id in [link.entry_a_id, link.entry_b_id] {
        let entry = journal::get_entry(&tx, id)?;
        if entry.status != EntryStatus::Draft {
            return Err(Error::Invalid(format!("entry {} is {}; a link can only be removed while both entries are drafts; void the pair instead", entry.uid, entry.status.as_str())));
        }
    }
    tx.execute("DELETE FROM intervault_links WHERE id = ?1", [link.id])?;
    audit::log_by(&tx, "intervault_links", link.id, "unlink", Some(serde_json::to_value(&link)?), None, "", reason)?;
    tx.commit()?;
    Ok(link)
}

fn set_status(conn: &Connection, link: &Link, status: &str, action: &str, reason: &str) -> Result<()> {
    conn.execute("UPDATE intervault_links SET status = ?2, updated_at = ?3 WHERE id = ?1", params![link.id, status, now_ts()])?;
    audit::log_by(conn, "intervault_links", link.id, action, Some(serde_json::json!({"status": link.status})), Some(serde_json::json!({"status": status})), "", reason)?;
    Ok(())
}

/// An entry was posted: a draft link becomes `posted` when both of its entries are posted.
pub(crate) fn on_post(conn: &Connection, entry_id: i64) -> Result<()> {
    for link in links_for_entry(conn, entry_id)?.into_iter().filter(|l| l.status == "draft") {
        let a = journal::get_entry(conn, link.entry_a_id)?;
        let b = journal::get_entry(conn, link.entry_b_id)?;
        if status_of_pair(&a, &b) == "posted" {
            set_status(conn, &link, "posted", "post", "")?;
        }
    }
    Ok(())
}

/// An entry was voided: void each linked entry that is posted, with the same reason, and mark
/// the links `void`. A linked draft stays a draft. `date` is the reversal date that the caller
/// asked for; without it, each linked entry reverses on its own date.
pub(crate) fn on_void(tx: &rusqlite::Transaction<'_>, entry_id: i64, date: Option<NaiveDate>, reason: &str) -> Result<()> {
    for link in links_for_entry(tx, entry_id)?.into_iter().filter(|l| l.status != "void") {
        set_status(tx, &link, "void", "void", reason)?;
        let other_id = if link.entry_a_id == entry_id { link.entry_b_id } else { link.entry_a_id };
        let other = journal::get_entry(tx, other_id)?;
        if other.status != EntryStatus::Posted {
            continue;
        }
        let entity = entities::get_entity(tx, other.entity_id)?;
        let day = date.unwrap_or(other.date);
        if let Some(lock) = entity.lock_date {
            if day <= lock {
                return Err(Error::Locked(format!(
                    "linked entry {} belongs to {}, and {day} is on or before its lock date {lock}; unlock {} first or choose a later void date",
                    other.uid, entity.name, entity.name
                )));
            }
        }
        journal::void_entry_in_transaction(tx, other_id, Some(day), reason)?;
    }
    Ok(())
}

/// A draft is about to be deleted: delete its links.
pub(crate) fn on_delete(conn: &Connection, entry_id: i64) -> Result<()> {
    for link in links_for_entry(conn, entry_id)? {
        conn.execute("DELETE FROM intervault_links WHERE id = ?1", [link.id])?;
        audit::log_by(conn, "intervault_links", link.id, "delete", Some(serde_json::to_value(&link)?), None, "", "linked draft deleted")?;
    }
    Ok(())
}

fn side(conn: &Connection, entry_id: i64, stored: Money) -> Result<LinkSide> {
    let entry = journal::get_entry(conn, entry_id)?;
    let entity = entities::get_entity(conn, entry.entity_id)?;
    let (current, _) = side_amount(conn, &entry)?;
    Ok(LinkSide { entry_uid: entry.uid, entity_uid: entity.uid, entity_name: entity.name, date: entry.date, entry_status: entry.status, amount: stored, current_amount: current })
}

/// A link with both entries and the reconciled flag.
pub fn view(conn: &Connection, link: &Link) -> Result<LinkView> {
    let entry_a = side(conn, link.entry_a_id, link.amount_a)?;
    let entry_b = side(conn, link.entry_b_id, link.amount_b)?;
    let reconciled = entry_a.current_amount.commodity() == link.amount_a.commodity()
        && entry_b.current_amount.commodity() == link.amount_b.commodity()
        && amounts_agree(entry_a.current_amount, entry_b.current_amount, &link.rate);
    Ok(LinkView {
        uid: link.uid.clone(),
        kind: link.kind.clone(),
        status: link.status.clone(),
        rate: link.rate.clone(),
        note: link.note.clone(),
        entry_a,
        entry_b,
        reconciled,
        created_at: link.created_at.clone(),
        updated_at: link.updated_at.clone(),
    })
}

/// The links with an entry dated from `from` to `to` (both included).
///
/// `group` (a uid or a name) keeps the links where either entity is a member of the group on
/// the date of its entry. `entity_uid` keeps the links where either entry belongs to the entity.
/// `status` keeps one status: draft, posted or void.
pub fn list_links(conn: &Connection, group: Option<&str>, entity_uid: Option<&str>, from: NaiveDate, to: NaiveDate, status: Option<&str>) -> Result<Vec<LinkView>> {
    if to < from {
        return Err(Error::Invalid(format!("the period ends on {to}, before it starts on {from}")));
    }
    let status = status.map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty());
    if let Some(s) = status.as_deref() {
        if !["draft", "posted", "void"].contains(&s) {
            return Err(Error::Invalid(format!("unknown link status {s:?}; use draft, posted or void")));
        }
    }
    let entity_id = match entity_uid {
        Some(uid) => Some(
            conn.query_row("SELECT id FROM entities WHERE uid = ?1", [uid.trim()], |r| r.get::<_, i64>(0)).optional()?.ok_or_else(|| Error::NotFound(format!("no entity with uid {}", uid.trim())))?,
        ),
        None => None,
    };
    let group = group.map(|g| groups::find_group(conn, g)).transpose()?;
    let sql = format!(
        "{LINK_SELECT}
         WHERE ((ea.date >= ?1 AND ea.date <= ?2) OR (eb.date >= ?1 AND eb.date <= ?2))
           AND (?3 IS NULL OR ea.entity_id = ?3 OR eb.entity_id = ?3)
           AND (?4 IS NULL OR l.status = ?4)
         ORDER BY MIN(ea.date, eb.date), l.id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let links = stmt.query_map(params![from.to_string(), to.to_string(), entity_id, status], row_to_link)?.collect::<std::result::Result<Vec<_>, _>>()?;
    let mut out = Vec::with_capacity(links.len());
    for link in links {
        let view = view(conn, &link)?;
        if let Some(g) = &group {
            let mut member = false;
            for (entry_id, date) in [(link.entry_a_id, view.entry_a.date), (link.entry_b_id, view.entry_b.date)] {
                let entity_id: i64 = conn.query_row("SELECT entity_id FROM journal_entries WHERE id = ?1", [entry_id], |r| r.get(0))?;
                if groups::membership_of(conn, entity_id, date)?.is_some_and(|m| m.group_id == g.id) {
                    member = true;
                }
            }
            if !member {
                continue;
            }
        }
        out.push(view);
    }
    Ok(out)
}

/// Booked postings on intercompany accounts of an entity whose entry has no link.
///
/// Booked means posted. Void entries and reversals cancel each other, so the list leaves them
/// out. A group close resolves each row: link the entry, or explain it.
pub fn unlinked_intercompany(conn: &Connection, entity_id: i64, from: NaiveDate, to: NaiveDate) -> Result<Vec<UnlinkedPosting>> {
    if to < from {
        return Err(Error::Invalid(format!("the period ends on {to}, before it starts on {from}")));
    }
    entities::get_entity(conn, entity_id)?;
    let mut stmt = conn.prepare(
        "SELECT e.uid, p.uid, e.date, e.payee, a.uid, a.id, p.quantity, ac.code, ac.precision, p.amount, fc.code, fc.precision, p.memo
         FROM postings p
         JOIN journal_entries e ON e.id = p.journal_entry_id
         JOIN accounts a ON a.id = p.account_id
         JOIN commodities ac ON ac.id = a.commodity_id
         JOIN entities en ON en.id = e.entity_id
         JOIN commodities fc ON fc.code = en.currency
         WHERE e.entity_id = ?1 AND a.intercompany = 1 AND e.status = 'posted' AND e.reverses_id IS NULL
           AND e.date >= ?2 AND e.date <= ?3
           AND NOT EXISTS (SELECT 1 FROM intervault_links l WHERE l.entry_a_id = e.id OR l.entry_b_id = e.id)
         ORDER BY e.date, e.id, p.position, p.id",
    )?;
    let rows = stmt.query_map(params![entity_id, from.to_string(), to.to_string()], |r| {
        let date: String = r.get(2)?;
        Ok((
            UnlinkedPosting {
                entry_uid: r.get(0)?,
                posting_uid: r.get(1)?,
                date: NaiveDate::parse_from_str(&date, "%Y-%m-%d").unwrap_or_default(),
                payee: r.get(3)?,
                account_uid: r.get(4)?,
                account_path: String::new(),
                quantity: money::from_row(r, 6, 7, 8)?,
                amount: money::from_row(r, 9, 10, 11)?,
                memo: r.get(12)?,
            },
            r.get::<_, i64>(5)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (mut posting, account_id) = row?;
        posting.account_path = accounts::get_account(conn, account_id)?.path;
        out.push(posting);
    }
    Ok(out)
}
