//! Inter-vault links: typed, audited links between two journal entries of different entities.
//!
//! A link joins entry A in one vault to entry B in another vault. It records the native amount
//! of the intercompany side of each entry and the implied rate between them. The pair has one
//! lifecycle:
//!
//! - The link is `draft` while either entry is a draft. It becomes `posted` when both are posted.
//! - A void of one entry is refused while the entry has a link that is not void. A void with
//!   cascade voids the other posted entries of the link group in the same transaction and marks
//!   the links `void`. The engine refuses a cascade when the period of another entity is locked.
//! - To void one entry only, remove the link first. A human can remove the link of posted
//!   entries with an actor and a reason.
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

/// One entry of another entity that a void reaches through inter-vault links.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LinkedEntry {
    /// The link that leads to the entry.
    pub link_uid: String,
    pub link_kind: String,
    pub entity_uid: String,
    pub entity_name: String,
    pub entry_id: i64,
    pub entry_uid: String,
    pub date: NaiveDate,
    pub status: EntryStatus,
    /// The native amount of the intercompany side of the entry now.
    pub amount: Money,
    /// What a void with cascade does to the entry: `void` for a posted entry, `keep` for a
    /// draft. The link becomes `void` in both cases.
    pub cascade_action: String,
    /// True when a cascade reversal of this entry falls on or before the lock date of its
    /// entity. A cascade is then refused.
    pub locked: bool,
    /// True when this void voided the entry.
    pub voided: bool,
}

impl LinkedEntry {
    /// One line for messages: entity, id, uid, date, amount and status.
    pub fn describe(&self) -> String {
        format!("{} entry #{} ({}) on {}, {} {}, {}", self.entity_name, self.entry_id, self.entry_uid, self.date, money::plain(self.amount.major()), self.amount.commodity(), self.status.as_str())
    }
}

/// The entries of other entities that a void of `entry_id` reaches through links that are not
/// void. The walk follows the links of each posted entry that a cascade would void, so the
/// list is the whole link group. `date` is the requested reversal date: without it, each entry
/// reverses on its own date.
pub fn linked_entries(conn: &Connection, entry_id: i64, date: Option<NaiveDate>) -> Result<Vec<LinkedEntry>> {
    let mut seen = HashSet::from([entry_id]);
    let mut queue = vec![entry_id];
    let mut out = Vec::new();
    while let Some(id) = queue.pop() {
        for link in links_for_entry(conn, id)?.into_iter().filter(|l| l.status != "void") {
            let other_id = if link.entry_a_id == id { link.entry_b_id } else { link.entry_a_id };
            if !seen.insert(other_id) {
                continue;
            }
            let other = journal::get_entry(conn, other_id)?;
            let entity = entities::get_entity(conn, other.entity_id)?;
            let (amount, _) = side_amount(conn, &other)?;
            let posted = other.status == EntryStatus::Posted;
            let day = date.unwrap_or(other.date);
            if posted {
                queue.push(other_id);
            }
            out.push(LinkedEntry {
                link_uid: link.uid,
                link_kind: link.kind,
                entity_uid: entity.uid,
                entity_name: entity.name,
                entry_id: other.id,
                entry_uid: other.uid,
                date: other.date,
                status: other.status,
                amount,
                cascade_action: if posted { "void" } else { "keep" }.into(),
                locked: posted && entity.lock_date.is_some_and(|lock| day <= lock),
                voided: false,
            });
        }
    }
    Ok(out)
}

/// The message of a void that the links refuse.
pub(crate) fn refusal(entry: &JournalEntry, linked: &[LinkedEntry]) -> String {
    let list: Vec<String> = linked.iter().map(LinkedEntry::describe).collect();
    format!(
        "entry #{} ({}) has inter-vault links to entries of other entities: {}; void them too with --cascade, or remove each link first with `means intervault unlink` to void only this entry",
        entry.id,
        entry.uid,
        list.join("; ")
    )
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

/// A removed link and the facts of its audit event.
#[derive(Debug, Clone, Serialize)]
pub struct Unlinked {
    pub link: Link,
    pub actor: String,
    pub reason: String,
    /// The entries that are dated on or before the lock date of their entity. The list is
    /// empty unless the caller gave `force_locked`.
    pub locked_entry_uids: Vec<String>,
    /// True when the removal went through a locked period.
    pub force_locked: bool,
    pub audit_event_id: i64,
}

/// Remove a link. Returns the removed link.
///
/// The removal needs a reason. While both entries are drafts, it needs nothing more. When an
/// entry is posted or void, the removal is a human action: it needs the actor, and it is
/// refused when that entry is dated on or before the lock date of its entity, unless the
/// caller gives `force_locked`.
///
/// The entries keep their postings. Without the link, each entry is not reconciled for
/// intercompany purposes: the close check counts it in `intercompany_out_of_balance`, and the
/// exception list shows its intercompany postings. A void of one entry then leaves the other.
pub fn unlink(conn: &mut Connection, link_uid: &str, actor: &str, reason: &str, force_locked: bool) -> Result<Unlinked> {
    if reason.trim().is_empty() {
        return Err(Error::Invalid("give a reason to remove the link".into()));
    }
    let tx = conn.transaction()?;
    let link = get_link_by_uid(&tx, link_uid)?;
    let a = journal::get_entry(&tx, link.entry_a_id)?;
    let b = journal::get_entry(&tx, link.entry_b_id)?;
    let booked: Vec<&JournalEntry> = [&a, &b].into_iter().filter(|e| e.status != EntryStatus::Draft).collect();
    if let Some(first) = booked.first() {
        if actor.trim().is_empty() {
            return Err(Error::Invalid(format!("entry {} is {}; the removal of a link of a posted entry is a human action, so give --actor", first.uid, first.status.as_str())));
        }
    }
    let mut locked = Vec::new();
    for entry in booked {
        let entity = entities::get_entity(&tx, entry.entity_id)?;
        if let Some(lock) = entity.lock_date.filter(|lock| entry.date <= *lock) {
            if !force_locked {
                return Err(Error::Locked(format!("entry {} of {} is dated {}, on or before its lock date {lock}; give --force-locked to remove the link anyway", entry.uid, entity.name, entry.date)));
            }
            locked.push(entry.uid.clone());
        }
    }
    tx.execute("DELETE FROM intervault_links WHERE id = ?1", [link.id])?;
    let after = serde_json::json!({
        "entry_a_uid": link.entry_a_uid,
        "entry_b_uid": link.entry_b_uid,
        "intercompany_reconciled": false,
        "force_locked": !locked.is_empty(),
        "locked_entry_uids": locked,
    });
    let audit_event_id = audit::log_by(&tx, "intervault_links", link.id, "unlink", Some(serde_json::to_value(&link)?), Some(after), actor, reason)?;
    tx.commit()?;
    Ok(Unlinked { link, actor: actor.trim().into(), reason: reason.trim().into(), force_locked: !locked.is_empty(), locked_entry_uids: locked, audit_event_id })
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

/// An entry was voided with cascade: void each linked entry that is posted, with the same
/// reason, and mark the links `void`. A linked draft stays a draft. `date` is the reversal date
/// that the caller asked for; without it, each linked entry reverses on its own date.
pub(crate) fn on_void_cascade(tx: &rusqlite::Transaction<'_>, entry_id: i64, entry_uid: &str, date: Option<NaiveDate>, reason: &str) -> Result<()> {
    for link in links_for_entry(tx, entry_id)?.into_iter().filter(|l| l.status != "void") {
        tx.execute("UPDATE intervault_links SET status = 'void', updated_at = ?2 WHERE id = ?1", params![link.id, now_ts()])?;
        audit::log_by(
            tx,
            "intervault_links",
            link.id,
            "void",
            Some(serde_json::json!({"status": link.status})),
            Some(serde_json::json!({"status": "void", "cascade": true, "voided_from": entry_uid})),
            "",
            reason,
        )?;
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
        journal::void_cascaded(tx, other_id, Some(day), reason, entry_uid)?;
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
