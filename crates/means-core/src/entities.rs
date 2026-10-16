//! Entities (sets of books) and commodities.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::accounts;
use crate::model::{AccountType, Commodity, Entity, NewAccount};
use crate::money;
use crate::{new_uid, now_ts, Error, Result};

/// The entities, the active ones first and then by id. A name lookup that walks this list finds
/// the active entity before an archived entity with the same name.
pub fn list_entities(conn: &Connection, include_archived: bool) -> Result<Vec<Entity>> {
    let mut stmt = conn.prepare(
        "SELECT id, uid, name, kind, country, currency, lock_date, archived_at, created_at
         FROM entities WHERE (?1 = 1 OR archived_at IS NULL) ORDER BY archived_at IS NOT NULL, id",
    )?;
    let rows = stmt.query_map([include_archived as i64], row_to_entity)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub fn get_entity(conn: &Connection, id: i64) -> Result<Entity> {
    conn.query_row("SELECT id, uid, name, kind, country, currency, lock_date, archived_at, created_at FROM entities WHERE id = ?1", [id], row_to_entity)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("entity {id}")))
}

fn row_to_entity(r: &rusqlite::Row<'_>) -> rusqlite::Result<Entity> {
    let lock: Option<String> = r.get(6)?;
    Ok(Entity {
        id: r.get(0)?,
        uid: r.get(1)?,
        name: r.get(2)?,
        kind: r.get(3)?,
        country: r.get(4)?,
        currency: r.get(5)?,
        lock_date: lock.and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()),
        archived_at: r.get(7)?,
        created_at: r.get(8)?,
    })
}

/// Find an entity by id, uid or name. The name match ignores case. One active entity with the
/// name wins over archived entities with the name. A name that matches more than one active
/// entity, or no active entity and more than one archived entity, is ambiguous: the error lists
/// the ids, and the caller gives an id or a uid.
pub fn find_by_key(conn: &Connection, key: &str) -> Result<Entity> {
    let key = key.trim();
    let all = list_entities(conn, true)?;
    if let Some(e) = all.iter().find(|e| key.parse::<i64>().ok() == Some(e.id) || e.uid == key) {
        return Ok(e.clone());
    }
    let named: Vec<&Entity> = all.iter().filter(|e| e.name.eq_ignore_ascii_case(key)).collect();
    let active: Vec<&Entity> = named.iter().copied().filter(|e| e.archived_at.is_none()).collect();
    let candidates = if active.is_empty() { named } else { active };
    match candidates.as_slice() {
        [] => Err(Error::NotFound(format!("entity {key}"))),
        [one] => Ok((*one).clone()),
        many => {
            let list: Vec<String> = many.iter().map(|e| format!("#{} {}{}", e.id, e.name, if e.archived_at.is_some() { " (archived)" } else { "" })).collect();
            Err(Error::Invalid(format!("the entity name {key:?} is ambiguous: it matches {}; give the id or the uid", list.join(", "))))
        }
    }
}

/// Create an entity with its functional currency and the system accounts every set of books needs.
pub fn create_entity(conn: &mut Connection, name: &str, kind: &str, country: &str, currency: &str) -> Result<Entity> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::Invalid("entity name is required".into()));
    }
    let kind = match kind.trim().to_ascii_lowercase().as_str() {
        "" | "person" => "person",
        "company" | "business" => "company",
        other => return Err(Error::Invalid(format!("entity kind must be person or company, got {other:?}"))),
    };
    let currency = normalize_code(currency)?;
    let tx = conn.transaction()?;
    check_active_name(&tx, 0, name)?;
    ensure_currency(&tx, &currency)?;
    let ts = now_ts();
    tx.execute(
        "INSERT INTO entities (uid, name, kind, country, currency, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![new_uid(), name, kind, country.trim().to_ascii_uppercase(), currency, ts],
    )
    .map_err(|e| if crate::is_constraint_violation(&e) { Error::Conflict(format!("an active entity named {name:?} already exists")) } else { Error::from(e) })?;
    let id = tx.last_insert_rowid();
    create_system_accounts(&tx, id, kind, &currency)?;
    crate::audit::log(&tx, "entities", id, "create", None, Some(serde_json::json!({"name": name, "kind": kind, "currency": currency})))?;
    tx.commit()?;
    get_entity(conn, id)
}

/// Change the name, kind, country and lock date of an entity.
/// The lock date can only move forward here. A move back needs `close::unlock`, which records
/// the actor and the reason and checks the filed periods.
/// `archived` must be the stored archive state: an archive or an unarchive needs
/// `archive` or `unarchive`, which record the actor and the reason.
pub fn update_entity(conn: &mut Connection, id: i64, name: &str, kind: &str, country: &str, lock_date: Option<NaiveDate>, archived: bool) -> Result<Entity> {
    update_entity_by(conn, id, name, kind, country, lock_date, archived, "")
}

/// `update_entity` with the actor of the audit events. A name change goes through `rename_by`,
/// with its check of the active names.
#[allow(clippy::too_many_arguments)]
pub fn update_entity_by(conn: &mut Connection, id: i64, name: &str, kind: &str, country: &str, lock_date: Option<NaiveDate>, archived: bool, actor: &str) -> Result<Entity> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let before = get_entity(&tx, id)?;
    if archived != before.archived_at.is_some() {
        let command = if archived { "means entity archive" } else { "means entity unarchive" };
        return Err(Error::Invalid(format!("an update cannot change the archive state of {}; use `{command}`, which records an actor and a reason", before.name)));
    }
    if let Some(lock) = before.lock_date {
        if lock_date.is_none_or(|new| new < lock) {
            return Err(Error::Locked(format!("{} is locked through {lock}; move the lock date back with `means close unlock`, which records an actor and a reason", before.name)));
        }
    }
    let kind = if kind.trim().is_empty() { before.kind.clone() } else { kind.trim().to_ascii_lowercase() };
    if kind != "person" && kind != "company" {
        return Err(Error::Invalid("entity kind must be person or company".into()));
    }
    if !name.trim().is_empty() && name.trim() != before.name {
        rename_by(&tx, id, name, actor)?;
    }
    tx.execute(
        "UPDATE entities SET kind = ?2, country = ?3, lock_date = ?4, updated_at = ?5 WHERE id = ?1",
        params![id, kind, country.trim().to_ascii_uppercase(), lock_date.map(|d| d.to_string()), now_ts()],
    )?;
    let after = get_entity(&tx, id)?;
    let (b, a) = (Some(serde_json::to_value(&before)?), Some(serde_json::to_value(&after)?));
    if actor.trim().is_empty() {
        crate::audit::log(&tx, "entities", id, "update", b, a)?;
    } else {
        crate::audit::log_by(&tx, "entities", id, "update", b, a, actor, "")?;
    }
    tx.commit()?;
    Ok(after)
}

/// Refuse a change to the books of an archived entity. The archive guard triggers refuse the
/// write too; this check gives a message that names the entity.
pub fn ensure_active(conn: &Connection, id: i64) -> Result<Entity> {
    let entity = get_entity(conn, id)?;
    if let Some(at) = &entity.archived_at {
        return Err(Error::Archived(format!("{} is archived since {at}; run `means entity unarchive {}` before you change its books", entity.name, entity.uid)));
    }
    Ok(entity)
}

/// Refuse migration 28 when two active entities have names that differ only in case. The
/// unique index of migration 28 ignores case, as `check_active_name` does.
pub(crate) fn refuse_case_twins(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT group_concat(printf('%Q (#%d)', name, id), ', ') FROM entities WHERE archived_at IS NULL
         GROUP BY name COLLATE NOCASE HAVING COUNT(*) > 1 ORDER BY MIN(id)",
    )?;
    let twins: Vec<String> = stmt.query_map([], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
    if !twins.is_empty() {
        return Err(Error::Invalid(format!(
            "migration 28 makes active entity names unique without case, but these active entities differ only in case: {}; rename one entity of each group with the older means build that opened this ledger, then open the ledger again",
            twins.join("; ")
        )));
    }
    Ok(())
}

/// Refuse a name that another active entity has. The comparison ignores case. Every route
/// that names an entity uses this check: create, rename, unarchive and gRPC.
fn check_active_name(conn: &Connection, id: i64, name: &str) -> Result<()> {
    if let Some(other) = list_entities(conn, false)?.into_iter().find(|e| e.id != id && e.name.eq_ignore_ascii_case(name)) {
        return Err(Error::Conflict(format!("the active entity #{} already has the name {:?}", other.id, other.name)));
    }
    Ok(())
}

/// Give an entity a new name inside the caller's transaction. The name must be unique among
/// the active entities. The audit log records the names before and after.
pub fn rename_in(tx: &Connection, id: i64, name: &str) -> Result<Entity> {
    rename_by(tx, id, name, "")
}

/// `rename_in` with the actor of the audit event.
pub fn rename_by(tx: &Connection, id: i64, name: &str, actor: &str) -> Result<Entity> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::Invalid("entity name is required".into()));
    }
    let before = ensure_active(tx, id)?;
    if before.name == name {
        return Ok(before);
    }
    check_active_name(tx, id, name)?;
    tx.execute("UPDATE entities SET name = ?2, updated_at = ?3 WHERE id = ?1", params![id, name, now_ts()])?;
    let (b, a) = (Some(serde_json::json!({"name": before.name})), Some(serde_json::json!({"name": name})));
    if actor.trim().is_empty() {
        crate::audit::log(tx, "entities", id, "rename", b, a)?;
    } else {
        crate::audit::log_by(tx, "entities", id, "rename", b, a, actor, "")?;
    }
    get_entity(tx, id)
}

/// The result of an archive or an unarchive.
#[derive(Debug, Clone, Serialize)]
pub struct ArchiveOutcome {
    pub entity: Entity,
    pub audit_event_id: i64,
}

fn require(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::Invalid(format!("give the {field}")));
    }
    Ok(())
}

/// Archive an entity. Its books stay readable and verifiable. The engine refuses every write to
/// them until an unarchive. The audit event records the actor and the reason.
pub fn archive(conn: &mut Connection, id: i64, actor: &str, reason: &str) -> Result<ArchiveOutcome> {
    require("name of the person who archives the entity", actor)?;
    require("reason for the archive", reason)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let before = get_entity(&tx, id)?;
    if let Some(at) = &before.archived_at {
        return Err(Error::Invalid(format!("{} is archived already, since {at}", before.name)));
    }
    refuse_live_links(&tx, &before)?;
    let ts = now_ts();
    tx.execute("UPDATE entities SET archived_at = ?2, updated_at = ?2 WHERE id = ?1", params![id, ts])?;
    let audit_event_id = crate::audit::log_by(&tx, "entities", id, "archive", Some(serde_json::json!({"archived_at": null})), Some(serde_json::json!({"archived_at": ts})), actor, reason)?;
    let entity = get_entity(&tx, id)?;
    tx.commit()?;
    Ok(ArchiveOutcome { entity, audit_event_id })
}

/// Refuse the archive of an entity while it has inter-vault links that are not void to entries
/// of active entities. The guards would then refuse a void with cascade from the other side.
/// The message lists each link: the other entity, its entry id and the link uid.
fn refuse_live_links(conn: &Connection, entity: &Entity) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT o.name, CASE WHEN ea.entity_id = ?1 THEN eb.id ELSE ea.id END, l.uid
         FROM intervault_links l
         JOIN journal_entries ea ON ea.id = l.entry_a_id
         JOIN journal_entries eb ON eb.id = l.entry_b_id
         JOIN entities o ON o.id = CASE WHEN ea.entity_id = ?1 THEN eb.entity_id ELSE ea.entity_id END
         WHERE l.status <> 'void' AND ?1 IN (ea.entity_id, eb.entity_id) AND o.id <> ?1 AND o.archived_at IS NULL
         ORDER BY l.id",
    )?;
    let links: Vec<String> =
        stmt.query_map([entity.id], |r| Ok(format!("{} entry #{} via link {}", r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)))?.collect::<std::result::Result<_, _>>()?;
    if !links.is_empty() {
        return Err(Error::Invalid(format!(
            "{} has inter-vault links that are not void to entries of active entities: {}; void the linked entries with cascade, or remove each link first, then archive the entity",
            entity.name,
            links.join("; ")
        )));
    }
    Ok(())
}

/// Make an archived entity active again. `new_name` renames it in the same step, for an
/// archived entity whose name an active entity has taken. The audit event records the actor
/// and the reason.
pub fn unarchive(conn: &mut Connection, id: i64, actor: &str, reason: &str, new_name: Option<&str>) -> Result<ArchiveOutcome> {
    require("name of the person who unarchives the entity", actor)?;
    require("reason for the unarchive", reason)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let before = get_entity(&tx, id)?;
    let Some(archived_at) = before.archived_at.clone() else {
        return Err(Error::Invalid(format!("{} is not archived", before.name)));
    };
    let name = new_name.map(str::trim).unwrap_or(&before.name).to_string();
    if name.is_empty() {
        return Err(Error::Invalid("entity name is required".into()));
    }
    check_active_name(&tx, id, &name).map_err(|e| match e {
        Error::Conflict(m) => Error::Conflict(format!("{m}; give the archived entity another name with --name")),
        other => other,
    })?;
    tx.execute("UPDATE entities SET archived_at = NULL, name = ?2, updated_at = ?3 WHERE id = ?1", params![id, name, now_ts()])?;
    let audit_event_id = crate::audit::log_by(
        &tx,
        "entities",
        id,
        "unarchive",
        Some(serde_json::json!({"archived_at": archived_at, "name": before.name})),
        Some(serde_json::json!({"archived_at": null, "name": name})),
        actor,
        reason,
    )?;
    let entity = get_entity(&tx, id)?;
    tx.commit()?;
    Ok(ArchiveOutcome { entity, audit_event_id })
}

/// Legal and tax facts of an entity. They describe the books and never change postings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityFacts {
    pub entity_id: i64,
    pub legal_form: String,
    /// `MM-DD`, or empty when unknown.
    pub fiscal_year_end: String,
    pub incorporated_on: Option<NaiveDate>,
    /// Registration and tax numbers. Keys are uppercase free text.
    pub registrations: BTreeMap<String, String>,
}

/// A change to the facts of an entity. `None` keeps the stored value.
#[derive(Debug, Clone, Default)]
pub struct EntityFactsPatch {
    pub legal_form: Option<String>,
    /// `MM-DD`. An empty string clears the value.
    pub fiscal_year_end: Option<String>,
    /// `YYYY-MM-DD`. An empty string clears the value.
    pub incorporated_on: Option<String>,
    /// Pairs to merge into the registrations. An empty value removes its key.
    pub registrations: Vec<(String, String)>,
}

/// A tax classification of an entity in one jurisdiction for one period.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Classification {
    pub id: i64,
    pub entity_id: i64,
    pub jurisdiction: String,
    pub classification: String,
    pub effective_from: NaiveDate,
    /// Exclusive. `None` keeps the period open.
    pub effective_to: Option<NaiveDate>,
    pub note: String,
    pub created_at: String,
}

pub fn get_facts(conn: &Connection, entity_id: i64) -> Result<EntityFacts> {
    let (legal_form, fiscal_year_end, incorporated_on, registrations) = conn
        .query_row("SELECT legal_form, fiscal_year_end, incorporated_on, registrations FROM entities WHERE id = ?1", [entity_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, opt_date_at(r, 2)?, r.get::<_, String>(3)?))
        })
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("entity {entity_id}")))?;
    Ok(EntityFacts { entity_id, legal_form, fiscal_year_end, incorporated_on, registrations: serde_json::from_str(&registrations)? })
}

/// Change the facts of an entity and audit the values before and after.
pub fn set_facts(conn: &mut Connection, entity_id: i64, patch: EntityFactsPatch) -> Result<EntityFacts> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let after = set_facts_in(&tx, entity_id, patch)?;
    tx.commit()?;
    Ok(after)
}

/// Change the facts of an entity inside the caller's transaction.
pub fn set_facts_in(tx: &Connection, entity_id: i64, patch: EntityFactsPatch) -> Result<EntityFacts> {
    let before = get_facts(tx, entity_id)?;
    let mut after = before.clone();
    if let Some(form) = patch.legal_form {
        after.legal_form = form.trim().to_string();
    }
    if let Some(fye) = patch.fiscal_year_end {
        after.fiscal_year_end = normalize_fiscal_year_end(&fye)?;
    }
    if let Some(date) = patch.incorporated_on {
        after.incorporated_on = crate::parse_opt_date(&date)?;
    }
    merge_pairs(&mut after.registrations, &patch.registrations)?;
    if after != before {
        tx.execute(
            "UPDATE entities SET legal_form = ?2, fiscal_year_end = ?3, incorporated_on = ?4, registrations = ?5, updated_at = ?6 WHERE id = ?1",
            params![entity_id, after.legal_form, after.fiscal_year_end, after.incorporated_on.map(|d| d.to_string()), serde_json::to_string(&after.registrations)?, now_ts()],
        )?;
        crate::audit::log(tx, "entities", entity_id, "set_facts", Some(serde_json::to_value(&before)?), Some(serde_json::to_value(&after)?))?;
    }
    Ok(after)
}

/// Check an ISO 3166-1 alpha-2 country code. The code becomes uppercase. An empty value stays
/// empty. `what` names the field in the error.
pub fn normalize_country(s: &str, what: &str) -> Result<String> {
    let country = s.trim().to_ascii_uppercase();
    if !country.is_empty() && (country.len() != 2 || !country.chars().all(|c| c.is_ascii_alphabetic())) {
        return Err(Error::Invalid(format!("{what} must be an ISO 3166-1 alpha-2 code such as SG, got {country:?}")));
    }
    Ok(country)
}

/// Check a fiscal year end as `MM-DD`. An empty value stays empty.
pub fn normalize_fiscal_year_end(s: &str) -> Result<String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(String::new());
    }
    let invalid = || Error::Invalid(format!("fiscal year end must be MM-DD, got {s:?}"));
    let (m, d) = s.split_once('-').ok_or_else(invalid)?;
    if m.len() != 2 || d.len() != 2 {
        return Err(invalid());
    }
    let (m, d) = (m.parse::<u32>().map_err(|_| invalid())?, d.parse::<u32>().map_err(|_| invalid())?);
    // A leap year accepts 02-29.
    NaiveDate::from_ymd_opt(2000, m, d).ok_or_else(invalid)?;
    Ok(format!("{m:02}-{d:02}"))
}

/// Parse `KEY=VALUE`. The key becomes uppercase. The value can be empty.
pub fn parse_pair(s: &str) -> Result<(String, String)> {
    let (k, v) = s.split_once('=').ok_or_else(|| Error::Invalid(format!("expected KEY=VALUE, got {s:?}")))?;
    let key = k.trim().to_uppercase();
    if key.is_empty() {
        return Err(Error::Invalid(format!("expected KEY=VALUE, got {s:?}")));
    }
    Ok((key, v.trim().to_string()))
}

/// Merge `KEY=VALUE` pairs into a map. An empty value removes its key.
pub fn merge_pairs(map: &mut BTreeMap<String, String>, pairs: &[(String, String)]) -> Result<()> {
    for (k, v) in pairs {
        let key = k.trim().to_uppercase();
        if key.is_empty() {
            return Err(Error::Invalid("a key is required".into()));
        }
        if v.trim().is_empty() {
            map.remove(&key);
        } else {
            map.insert(key, v.trim().to_string());
        }
    }
    Ok(())
}

/// Reject a period whose end is not after its start.
pub(crate) fn check_period(from: NaiveDate, to: Option<NaiveDate>) -> Result<()> {
    match to {
        Some(to) if to <= from => Err(Error::Invalid(format!("the period must end after it starts: {from} to {to}"))),
        _ => Ok(()),
    }
}

/// True when a period `[from, to)` includes the date.
pub(crate) fn covers(from: NaiveDate, to: Option<NaiveDate>, on: NaiveDate) -> bool {
    from <= on && to.is_none_or(|to| on < to)
}

/// True when two periods `[from, to)` share at least one day.
pub(crate) fn overlaps(a: (NaiveDate, Option<NaiveDate>), b: (NaiveDate, Option<NaiveDate>)) -> bool {
    a.1.is_none_or(|end| b.0 < end) && b.1.is_none_or(|end| a.0 < end)
}

fn row_to_classification(r: &rusqlite::Row<'_>) -> rusqlite::Result<Classification> {
    Ok(Classification {
        id: r.get(0)?,
        entity_id: r.get(1)?,
        jurisdiction: r.get(2)?,
        classification: r.get(3)?,
        effective_from: date_at(r, 4)?,
        effective_to: opt_date_at(r, 5)?,
        note: r.get(6)?,
        created_at: r.get(7)?,
    })
}

/// Read a `YYYY-MM-DD` column.
pub(crate) fn date_at(r: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<NaiveDate> {
    let s: String = r.get(idx)?;
    NaiveDate::parse_from_str(&s, "%Y-%m-%d").map_err(|e| rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(e)))
}

/// Read a nullable `YYYY-MM-DD` column.
pub(crate) fn opt_date_at(r: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<Option<NaiveDate>> {
    match r.get::<_, Option<String>>(idx)? {
        None => Ok(None),
        Some(_) => date_at(r, idx).map(Some),
    }
}

const CLASSIFICATION_COLUMNS: &str = "id, entity_id, jurisdiction, classification, effective_from, effective_to, note, created_at";

pub fn get_classification(conn: &Connection, id: i64) -> Result<Classification> {
    conn.query_row(&format!("SELECT {CLASSIFICATION_COLUMNS} FROM entity_classifications WHERE id = ?1"), [id], row_to_classification)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("classification {id}")))
}

/// All classifications of an entity, or only those in effect on `as_of`.
pub fn list_classifications(conn: &Connection, entity_id: i64, as_of: Option<NaiveDate>) -> Result<Vec<Classification>> {
    get_entity(conn, entity_id)?;
    let mut stmt = conn.prepare(&format!("SELECT {CLASSIFICATION_COLUMNS} FROM entity_classifications WHERE entity_id = ?1 ORDER BY jurisdiction, effective_from, id"))?;
    let all = stmt.query_map([entity_id], row_to_classification)?.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(all.into_iter().filter(|c| as_of.is_none_or(|d| covers(c.effective_from, c.effective_to, d))).collect())
}

/// Record a classification. One classification applies per jurisdiction at a time. An open
/// classification that starts earlier in the same jurisdiction closes on `from`. Any other
/// overlap is a conflict.
pub fn add_classification(conn: &mut Connection, entity_id: i64, jurisdiction: &str, classification: &str, from: NaiveDate, to: Option<NaiveDate>, note: &str) -> Result<Classification> {
    let jurisdiction = jurisdiction.trim().to_ascii_uppercase();
    if jurisdiction.is_empty() || !jurisdiction.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(Error::Invalid(format!("jurisdiction must be a code such as US or SG, got {jurisdiction:?}")));
    }
    let classification = classification.trim().to_ascii_lowercase();
    if classification.is_empty() || !classification.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(Error::Invalid(format!("classification must be a code such as corporation, got {classification:?}")));
    }
    check_period(from, to)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let existing: Vec<_> = list_classifications(&tx, entity_id, None)?.into_iter().filter(|c| c.jurisdiction == jurisdiction).collect();
    for c in &existing {
        if !overlaps((c.effective_from, c.effective_to), (from, to)) {
            continue;
        }
        if c.effective_to.is_none() && c.effective_from < from {
            close_classification_in(&tx, c, from)?;
        } else {
            return Err(Error::Conflict(format!("{jurisdiction} classification #{} ({}) already covers part of this period", c.id, c.classification)));
        }
    }
    tx.execute(
        "INSERT INTO entity_classifications (entity_id, jurisdiction, classification, effective_from, effective_to, note, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![entity_id, jurisdiction, classification, from.to_string(), to.map(|d| d.to_string()), note.trim(), now_ts()],
    )?;
    let saved = get_classification(&tx, tx.last_insert_rowid())?;
    crate::audit::log(&tx, "entity_classifications", saved.id, "create", None, Some(serde_json::to_value(&saved)?))?;
    tx.commit()?;
    Ok(saved)
}

/// End an open classification on `to` (exclusive).
pub fn close_classification(conn: &mut Connection, id: i64, to: NaiveDate) -> Result<Classification> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let c = get_classification(&tx, id)?;
    if c.effective_to.is_some() {
        return Err(Error::Conflict(format!("classification #{id} is already closed")));
    }
    let after = close_classification_in(&tx, &c, to)?;
    tx.commit()?;
    Ok(after)
}

fn close_classification_in(conn: &Connection, c: &Classification, to: NaiveDate) -> Result<Classification> {
    check_period(c.effective_from, Some(to))?;
    conn.execute("UPDATE entity_classifications SET effective_to = ?2 WHERE id = ?1", params![c.id, to.to_string()])?;
    let after = get_classification(conn, c.id)?;
    crate::audit::log(conn, "entity_classifications", c.id, "close", Some(serde_json::to_value(c)?), Some(serde_json::to_value(&after)?))?;
    Ok(after)
}

fn create_system_accounts(conn: &Connection, entity_id: i64, kind: &str, currency: &str) -> Result<()> {
    let mk = |name: &str, t: AccountType, subtype: &str, role: &str| NewAccount {
        entity_id,
        parent_id: None,
        code: String::new(),
        name: name.to_string(),
        r#type: Some(t),
        subtype: subtype.to_string(),
        commodity: currency.to_string(),
        system_role: role.to_string(),
        placeholder: false,
        in_net_worth: true,
        credit_limit: None,
        statement_day: None,
        due_day: None,
        external_ids: None,
        notes: String::new(),
        position: 900,
    };
    accounts::create_account(conn, mk("Opening balances", AccountType::Equity, "equity", "opening_balance"))?;
    accounts::create_account(conn, mk("Uncategorized", AccountType::Expense, "expense", "suspense"))?;
    accounts::create_account(conn, mk("FX gain/loss", AccountType::Expense, "expense", "fx_gain_loss"))?;
    if kind == "company" {
        accounts::create_account(conn, mk("Owner contributions", AccountType::Equity, "equity", "owner_contributions"))?;
        accounts::create_account(conn, mk("Owner distributions", AccountType::Equity, "equity", "owner_distributions"))?;
    } else {
        accounts::create_account(conn, mk("Distributions received", AccountType::Income, "income", "distributions_received"))?;
        accounts::create_account(conn, mk("Investments in entities", AccountType::Asset, "investment", "investment_in_entity"))?;
    }
    Ok(())
}

pub fn normalize_code(code: &str) -> Result<String> {
    let c = code.trim().to_ascii_uppercase();
    if c.is_empty() || c.len() > 12 || !c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_') {
        return Err(Error::Invalid(format!("invalid commodity code {code:?}")));
    }
    Ok(c)
}

/// Make sure a currency commodity exists; returns its id.
pub fn ensure_currency(conn: &Connection, code: &str) -> Result<i64> {
    let code = normalize_code(code)?;
    if let Some(id) = conn.query_row("SELECT id FROM commodities WHERE code = ?1", [&code], |r| r.get::<_, i64>(0)).optional()? {
        return Ok(id);
    }
    let kind = if money::default_precision(&code) == 8 { "crypto" } else { "currency" };
    conn.execute("INSERT INTO commodities (code, kind, name, precision) VALUES (?1, ?2, ?3, ?4)", params![code, kind, currency_name(&code), money::default_precision(&code)])?;
    Ok(conn.last_insert_rowid())
}

pub fn create_commodity(conn: &Connection, code: &str, kind: &str, name: &str, precision: Option<u32>, isin: &str) -> Result<Commodity> {
    let code = normalize_code(code)?;
    let kind = match kind.trim().to_ascii_lowercase().as_str() {
        "" | "currency" => "currency",
        "security" | "stock" | "fund" | "etf" => "security",
        "crypto" => "crypto",
        other => return Err(Error::Invalid(format!("commodity kind must be currency, security or crypto, got {other:?}"))),
    };
    let stored: Option<u32> = conn.query_row("SELECT precision FROM commodities WHERE code = ?1", [&code], |r| r.get::<_, i64>(0)).optional()?.map(|p| p as u32);
    // D14: the precision says what every integer stored in this commodity counts, so it is fixed
    // when the commodity is created. Changing it would reinterpret every quantity and amount
    // already written in it by a power of ten.
    if let (Some(have), Some(want)) = (stored, precision) {
        if have != want {
            return Err(Error::Invalid(format!("commodity {code} was created with precision {have} and it cannot change; asked for {want}")));
        }
    }
    let precision = stored.or(precision).unwrap_or_else(|| match kind {
        "crypto" => 8,
        "security" => 4,
        _ => money::default_precision(&code),
    });
    money::check_precision(precision)?;
    conn.execute(
        "INSERT INTO commodities (code, kind, name, precision, isin) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(code) DO UPDATE SET kind = excluded.kind, name = CASE WHEN excluded.name <> '' THEN excluded.name ELSE commodities.name END,
           isin = CASE WHEN excluded.isin <> '' THEN excluded.isin ELSE commodities.isin END",
        params![code, kind, if name.trim().is_empty() { currency_name(&code) } else { name.trim().to_string() }, precision, isin.trim()],
    )?;
    get_commodity_by_code(conn, &code)
}

pub fn list_commodities(conn: &Connection) -> Result<Vec<Commodity>> {
    let mut stmt = conn.prepare("SELECT id, code, kind, name, precision, isin FROM commodities ORDER BY kind, code")?;
    let rows = stmt.query_map([], row_to_commodity)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub fn get_commodity(conn: &Connection, id: i64) -> Result<Commodity> {
    conn.query_row("SELECT id, code, kind, name, precision, isin FROM commodities WHERE id = ?1", [id], row_to_commodity).optional()?.ok_or_else(|| Error::NotFound(format!("commodity {id}")))
}

/// The precision of an entity's functional currency: the decimals of every `amount` it stores
/// (D14). The one answer to this question, so a write and a read of the same `amount` cannot
/// differ by a power of ten; the grouped queries in `reports` and `accounts` join
/// `commodities.code = entities.currency` to reach the same number a whole column at a time.
pub fn functional_precision(conn: &Connection, entity_id: i64) -> Result<u32> {
    conn.query_row("SELECT c.precision FROM entities e JOIN commodities c ON c.code = e.currency WHERE e.id = ?1", [entity_id], |r| r.get::<_, i64>(0))
        .optional()?
        .map(|p| p as u32)
        .ok_or_else(|| Error::NotFound(format!("functional currency of entity {entity_id}")))
}

pub fn get_commodity_by_code(conn: &Connection, code: &str) -> Result<Commodity> {
    let code = normalize_code(code)?;
    conn.query_row("SELECT id, code, kind, name, precision, isin FROM commodities WHERE code = ?1", [&code], row_to_commodity).optional()?.ok_or_else(|| Error::NotFound(format!("commodity {code}")))
}

fn row_to_commodity(r: &rusqlite::Row<'_>) -> rusqlite::Result<Commodity> {
    Ok(Commodity { id: r.get(0)?, code: r.get(1)?, kind: r.get(2)?, name: r.get(3)?, precision: r.get::<_, i64>(4)? as u32, isin: r.get(5)? })
}

fn currency_name(code: &str) -> String {
    match code {
        "EUR" => "Euro",
        "USD" => "US dollar",
        "BRL" => "Brazilian real",
        "GBP" => "Pound sterling",
        "CHF" => "Swiss franc",
        "IDR" => "Indonesian rupiah",
        "JPY" => "Japanese yen",
        "CAD" => "Canadian dollar",
        "AUD" => "Australian dollar",
        "MXN" => "Mexican peso",
        "ARS" => "Argentine peso",
        "BTC" => "Bitcoin",
        "ETH" => "Ether",
        other => other,
    }
    .to_string()
}
