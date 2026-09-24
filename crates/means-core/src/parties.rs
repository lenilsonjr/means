//! Payees as parties: their country, tax ids and kind, and their dated relationships to an entity.
//!
//! A relationship holds for a period. The period starts on `effective_from` (inclusive) and ends
//! on `effective_to` (exclusive). A relationship stays in the books of the payee's own entity.
//! A related party for reporting is a payee with an open owner, subsidiary, director or
//! related_party relationship to the entity on the date.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::entities::{self, check_period, covers, date_at, opt_date_at, overlaps};
use crate::groups::FULL_OWNERSHIP_BP;
use crate::payees::{self, Payee};
use crate::{audit, now_ts, Error, Result};

pub const PARTY_KINDS: &[&str] = &["person", "company", "authority", "other"];
pub const RELATIONSHIP_KINDS: &[&str] = &["owner", "subsidiary", "director", "related_party", "contractor", "employee", "customer", "supplier", "lender", "borrower"];
/// Relationship kinds that make a payee a related party of the entity.
pub const RELATED_PARTY_KINDS: &[&str] = &["owner", "subsidiary", "director", "related_party"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartyFacts {
    pub payee_id: i64,
    /// ISO 3166-1 alpha-2, or empty when unknown.
    pub country: String,
    /// Tax ids by uppercase key, for example `VAT`.
    pub tax_ids: BTreeMap<String, String>,
    /// person | company | authority | other
    pub kind: String,
}

/// A change to the facts of a payee. `None` keeps the stored value.
#[derive(Debug, Clone, Default)]
pub struct PartyFactsPatch {
    /// An empty string clears the country.
    pub country: Option<String>,
    /// Pairs to merge into the tax ids. An empty value removes its key.
    pub tax_ids: Vec<(String, String)>,
    pub kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relationship {
    pub id: i64,
    pub payee_id: i64,
    pub entity_id: i64,
    pub kind: String,
    /// Basis points: 10000 is 100%.
    pub ownership_bp: Option<i64>,
    pub effective_from: NaiveDate,
    /// Exclusive. `None` keeps the relationship open.
    pub effective_to: Option<NaiveDate>,
    pub basis: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct NewRelationship {
    pub payee_id: i64,
    pub entity_id: i64,
    pub kind: String,
    pub ownership_bp: Option<i64>,
    pub effective_from: NaiveDate,
    pub basis: String,
}

/// A payee of an entity with its facts and the relationships in effect on a date.
#[derive(Debug, Clone, Serialize)]
pub struct Party {
    pub payee: Payee,
    pub facts: PartyFacts,
    pub relationships: Vec<Relationship>,
    pub related_party: bool,
}

pub fn is_related_kind(kind: &str) -> bool {
    RELATED_PARTY_KINDS.contains(&kind)
}

/// Find a payee by id, uid or exact name. With an entity, the search stays in that entity.
pub fn find_payee(conn: &Connection, entity_id: Option<i64>, key: &str) -> Result<Payee> {
    let key = key.trim();
    let mut stmt = conn.prepare("SELECT id, entity_id, name, uid FROM payees WHERE ?1 IS NULL OR entity_id = ?1 ORDER BY id")?;
    let rows = stmt.query_map(params![entity_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))?.collect::<std::result::Result<Vec<_>, _>>()?;
    if let Some(row) = rows.iter().find(|r| key.parse::<i64>().ok() == Some(r.0) || r.3 == key) {
        return payees::get(conn, row.0);
    }
    let named: Vec<_> = rows.iter().filter(|r| payees::normalize(&r.2) == payees::normalize(key)).collect();
    match named.as_slice() {
        [one] => payees::get(conn, one.0),
        [] => Err(Error::NotFound(format!("payee {key}"))),
        _ => Err(Error::Conflict(format!("{} payees are named {key:?}; choose an entity or use an id", named.len()))),
    }
}

pub fn get_facts(conn: &Connection, payee_id: i64) -> Result<PartyFacts> {
    let (country, tax_ids, kind) = conn
        .query_row("SELECT country, tax_ids, kind FROM payees WHERE id = ?1", [payee_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("payee {payee_id}")))?;
    Ok(PartyFacts { payee_id, country, tax_ids: serde_json::from_str(&tax_ids)?, kind })
}

/// Change the facts of a payee and audit the values before and after.
pub fn set_facts(conn: &mut Connection, payee_id: i64, patch: PartyFactsPatch) -> Result<PartyFacts> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let before = get_facts(&tx, payee_id)?;
    let mut after = before.clone();
    if let Some(country) = patch.country {
        let country = country.trim().to_ascii_uppercase();
        if !country.is_empty() && (country.len() != 2 || !country.chars().all(|c| c.is_ascii_alphabetic())) {
            return Err(Error::Invalid(format!("country must be an ISO 3166-1 alpha-2 code such as SG, got {country:?}")));
        }
        after.country = country;
    }
    if let Some(kind) = patch.kind {
        let kind = kind.trim().to_ascii_lowercase();
        if !PARTY_KINDS.contains(&kind.as_str()) {
            return Err(Error::Invalid(format!("party kind must be one of {}, got {kind:?}", PARTY_KINDS.join(", "))));
        }
        after.kind = kind;
    }
    entities::merge_pairs(&mut after.tax_ids, &patch.tax_ids)?;
    if after != before {
        tx.execute("UPDATE payees SET country = ?2, tax_ids = ?3, kind = ?4 WHERE id = ?1", params![payee_id, after.country, serde_json::to_string(&after.tax_ids)?, after.kind])?;
        audit::log(&tx, "payees", payee_id, "set_facts", Some(serde_json::to_value(&before)?), Some(serde_json::to_value(&after)?))?;
    }
    tx.commit()?;
    Ok(after)
}

const RELATIONSHIP_COLUMNS: &str = "id, payee_id, entity_id, kind, ownership_bp, effective_from, effective_to, basis, created_at";

fn row_to_relationship(r: &rusqlite::Row<'_>) -> rusqlite::Result<Relationship> {
    Ok(Relationship {
        id: r.get(0)?,
        payee_id: r.get(1)?,
        entity_id: r.get(2)?,
        kind: r.get(3)?,
        ownership_bp: r.get(4)?,
        effective_from: date_at(r, 5)?,
        effective_to: opt_date_at(r, 6)?,
        basis: r.get(7)?,
        created_at: r.get(8)?,
    })
}

pub fn get_relationship(conn: &Connection, id: i64) -> Result<Relationship> {
    conn.query_row(&format!("SELECT {RELATIONSHIP_COLUMNS} FROM party_relationships WHERE id = ?1"), [id], row_to_relationship).optional()?.ok_or_else(|| Error::NotFound(format!("relationship {id}")))
}

/// Every relationship of an entity, or only those in effect on `as_of`.
pub fn list_relationships(conn: &Connection, entity_id: i64, as_of: Option<NaiveDate>) -> Result<Vec<Relationship>> {
    entities::get_entity(conn, entity_id)?;
    let mut stmt = conn.prepare(&format!("SELECT {RELATIONSHIP_COLUMNS} FROM party_relationships WHERE entity_id = ?1 ORDER BY payee_id, kind, effective_from, id"))?;
    let all = stmt.query_map([entity_id], row_to_relationship)?.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(all.into_iter().filter(|r| as_of.is_none_or(|d| covers(r.effective_from, r.effective_to, d))).collect())
}

/// Add an open relationship. The payee must belong to the entity. The same payee, entity and
/// kind cannot have two relationships on one date.
pub fn relate(conn: &mut Connection, new: NewRelationship) -> Result<Relationship> {
    let kind = new.kind.trim().to_ascii_lowercase();
    if !RELATIONSHIP_KINDS.contains(&kind.as_str()) {
        return Err(Error::Invalid(format!("relationship kind must be one of {}, got {kind:?}", RELATIONSHIP_KINDS.join(", "))));
    }
    if let Some(bp) = new.ownership_bp {
        if !(0..=FULL_OWNERSHIP_BP).contains(&bp) {
            return Err(Error::Invalid(format!("ownership must be 0 to {FULL_OWNERSHIP_BP} basis points, got {bp}")));
        }
    }
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let payee = payees::get(&tx, new.payee_id)?;
    entities::get_entity(&tx, new.entity_id)?;
    if payee.entity_id != new.entity_id {
        return Err(Error::Invalid(format!("payee {} belongs to another entity; record the relationship on a payee of entity {}", payee.id, new.entity_id)));
    }
    let existing = list_relationships(&tx, new.entity_id, None)?;
    if let Some(r) = existing.iter().find(|r| r.payee_id == payee.id && r.kind == kind && overlaps((r.effective_from, r.effective_to), (new.effective_from, None))) {
        return Err(Error::Conflict(format!("payee {} already has a {kind} relationship from {} (relationship #{})", payee.id, r.effective_from, r.id)));
    }
    tx.execute(
        "INSERT INTO party_relationships (payee_id, entity_id, kind, ownership_bp, effective_from, basis, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![payee.id, new.entity_id, kind, new.ownership_bp, new.effective_from.to_string(), new.basis.trim(), now_ts()],
    )?;
    let saved = get_relationship(&tx, tx.last_insert_rowid())?;
    audit::log(&tx, "party_relationships", saved.id, "relate", None, Some(serde_json::to_value(&saved)?))?;
    tx.commit()?;
    Ok(saved)
}

/// End the open relationship of a kind on `to` (exclusive).
pub fn unrelate(conn: &mut Connection, payee_id: i64, entity_id: i64, kind: &str, to: NaiveDate) -> Result<Relationship> {
    let kind = kind.trim().to_ascii_lowercase();
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let before = list_relationships(&tx, entity_id, None)?
        .into_iter()
        .find(|r| r.payee_id == payee_id && r.kind == kind && r.effective_to.is_none())
        .ok_or_else(|| Error::NotFound(format!("open {kind} relationship of payee {payee_id} with entity {entity_id}")))?;
    check_period(before.effective_from, Some(to))?;
    tx.execute("UPDATE party_relationships SET effective_to = ?2 WHERE id = ?1", params![before.id, to.to_string()])?;
    let after = get_relationship(&tx, before.id)?;
    audit::log(&tx, "party_relationships", before.id, "unrelate", Some(serde_json::to_value(&before)?), Some(serde_json::to_value(&after)?))?;
    tx.commit()?;
    Ok(after)
}

/// The payees of an entity with their facts and the relationships in effect on `as_of`.
/// With `related_only`, the list keeps only related parties.
pub fn list_parties(conn: &Connection, entity_id: i64, as_of: NaiveDate, related_only: bool) -> Result<Vec<Party>> {
    let relationships = list_relationships(conn, entity_id, Some(as_of))?;
    let mut out = vec![];
    for payee in payees::list(conn, entity_id)? {
        let rels: Vec<_> = relationships.iter().filter(|r| r.payee_id == payee.id).cloned().collect();
        let related_party = rels.iter().any(|r| is_related_kind(&r.kind));
        if related_only && !related_party {
            continue;
        }
        out.push(Party { facts: get_facts(conn, payee.id)?, payee, relationships: rels, related_party });
    }
    Ok(out)
}

/// The related parties of an entity on a date.
pub fn related_parties(conn: &Connection, entity_id: i64, as_of: NaiveDate) -> Result<Vec<Party>> {
    list_parties(conn, entity_id, as_of, true)
}
