//! Groups of entities: who holds whom, and how much, over time.
//!
//! A member row holds one entity in one group for a period. The period starts on
//! `effective_from` (inclusive) and ends on `effective_to` (exclusive). An entity is in at most
//! one group on any date. A parent must be a member of the same group for the whole period of
//! its child.

use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::entities::{self, check_period, covers, date_at, opt_date_at, overlaps};
use crate::{audit, new_uid, now_ts, Entity, Error, Result};

/// 10000 basis points are 100%.
pub const FULL_OWNERSHIP_BP: i64 = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub id: i64,
    pub uid: String,
    pub name: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub id: i64,
    pub group_id: i64,
    pub entity_id: i64,
    pub parent_entity_id: Option<i64>,
    /// Basis points of the entity that the parent holds.
    pub ownership_bp: i64,
    pub control: bool,
    pub effective_from: NaiveDate,
    /// Exclusive. `None` keeps the membership open.
    pub effective_to: Option<NaiveDate>,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct NewMember {
    pub group_id: i64,
    pub entity_id: i64,
    pub parent_entity_id: Option<i64>,
    pub ownership_bp: i64,
    pub control: bool,
    pub effective_from: NaiveDate,
}

impl NewMember {
    /// A wholly owned, controlled member.
    pub fn new(group_id: i64, entity_id: i64, effective_from: NaiveDate) -> NewMember {
        NewMember { group_id, entity_id, parent_entity_id: None, ownership_bp: FULL_OWNERSHIP_BP, control: true, effective_from }
    }
}

/// One member in a group tree on a date.
#[derive(Debug, Clone, Serialize)]
pub struct TreeNode {
    pub member: Member,
    pub entity: Entity,
    /// The product of the ownership along the path from the root, rounded down.
    pub effective_ownership_bp: i64,
    pub children: Vec<TreeNode>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupTree {
    pub group: Group,
    pub as_of: NaiveDate,
    pub roots: Vec<TreeNode>,
}

fn row_to_group(r: &rusqlite::Row<'_>) -> rusqlite::Result<Group> {
    Ok(Group { id: r.get(0)?, uid: r.get(1)?, name: r.get(2)?, created_at: r.get(3)? })
}

const MEMBER_COLUMNS: &str = "id, group_id, entity_id, parent_entity_id, ownership_bp, control, effective_from, effective_to, created_at";

fn row_to_member(r: &rusqlite::Row<'_>) -> rusqlite::Result<Member> {
    Ok(Member {
        id: r.get(0)?,
        group_id: r.get(1)?,
        entity_id: r.get(2)?,
        parent_entity_id: r.get(3)?,
        ownership_bp: r.get(4)?,
        control: r.get(5)?,
        effective_from: date_at(r, 6)?,
        effective_to: opt_date_at(r, 7)?,
        created_at: r.get(8)?,
    })
}

pub fn create_group(conn: &mut Connection, name: &str) -> Result<Group> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::Invalid("group name is required".into()));
    }
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if find_group(&tx, name).is_ok() {
        return Err(Error::Conflict(format!("a group named {name:?} already exists")));
    }
    tx.execute("INSERT INTO entity_groups (uid, name, created_at) VALUES (?1, ?2, ?3)", params![new_uid(), name, now_ts()])?;
    let group = get_group(&tx, tx.last_insert_rowid())?;
    audit::log(&tx, "entity_groups", group.id, "create", None, Some(serde_json::to_value(&group)?))?;
    tx.commit()?;
    Ok(group)
}

pub fn get_group(conn: &Connection, id: i64) -> Result<Group> {
    conn.query_row("SELECT id, uid, name, created_at FROM entity_groups WHERE id = ?1", [id], row_to_group).optional()?.ok_or_else(|| Error::NotFound(format!("group {id}")))
}

/// Find a group by uid or by name. The name match ignores case.
pub fn find_group(conn: &Connection, key: &str) -> Result<Group> {
    let key = key.trim();
    conn.query_row("SELECT id, uid, name, created_at FROM entity_groups WHERE uid = ?1 OR lower(name) = lower(?1) ORDER BY uid = ?1 DESC LIMIT 1", [key], row_to_group)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("group {key}")))
}

pub fn list_groups(conn: &Connection) -> Result<Vec<Group>> {
    let mut stmt = conn.prepare("SELECT id, uid, name, created_at FROM entity_groups ORDER BY name")?;
    let rows = stmt.query_map([], row_to_group)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub fn get_member(conn: &Connection, id: i64) -> Result<Member> {
    conn.query_row(&format!("SELECT {MEMBER_COLUMNS} FROM entity_group_members WHERE id = ?1"), [id], row_to_member).optional()?.ok_or_else(|| Error::NotFound(format!("group member {id}")))
}

/// Every membership row of a group, or only the rows in effect on `as_of`.
pub fn list_members(conn: &Connection, group_id: i64, as_of: Option<NaiveDate>) -> Result<Vec<Member>> {
    get_group(conn, group_id)?;
    let mut stmt = conn.prepare(&format!("SELECT {MEMBER_COLUMNS} FROM entity_group_members WHERE group_id = ?1 ORDER BY effective_from, id"))?;
    let all = stmt.query_map([group_id], row_to_member)?.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(all.into_iter().filter(|m| as_of.is_none_or(|d| covers(m.effective_from, m.effective_to, d))).collect())
}

/// The membership of an entity on a date, in any group.
pub fn membership_of(conn: &Connection, entity_id: i64, as_of: NaiveDate) -> Result<Option<Member>> {
    let mut stmt = conn.prepare(&format!("SELECT {MEMBER_COLUMNS} FROM entity_group_members WHERE entity_id = ?1 ORDER BY effective_from, id"))?;
    let all = stmt.query_map([entity_id], row_to_member)?.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(all.into_iter().find(|m| covers(m.effective_from, m.effective_to, as_of)))
}

/// Add an open membership. The entity must not be in any group on or after `effective_from`.
/// A parent must hold an open membership in the same group that starts on or before it.
pub fn add_member(conn: &mut Connection, new: NewMember) -> Result<Member> {
    if !(0..=FULL_OWNERSHIP_BP).contains(&new.ownership_bp) {
        return Err(Error::Invalid(format!("ownership must be 0 to {FULL_OWNERSHIP_BP} basis points, got {}", new.ownership_bp)));
    }
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let group = get_group(&tx, new.group_id)?;
    entities::get_entity(&tx, new.entity_id)?;
    let mut stmt = tx.prepare(&format!("SELECT {MEMBER_COLUMNS} FROM entity_group_members WHERE entity_id = ?1 ORDER BY effective_from, id"))?;
    let rows = stmt.query_map([new.entity_id], row_to_member)?.collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);
    if let Some(m) = rows.iter().find(|m| overlaps((m.effective_from, m.effective_to), (new.effective_from, None))) {
        let other = get_group(&tx, m.group_id)?;
        return Err(Error::Conflict(format!("entity {} is already in group {:?} from {} (membership #{})", new.entity_id, other.name, m.effective_from, m.id)));
    }
    if let Some(parent) = new.parent_entity_id {
        if parent == new.entity_id {
            return Err(Error::Invalid("an entity cannot be its own parent".into()));
        }
        entities::get_entity(&tx, parent)?;
        let open = list_members(&tx, group.id, None)?.into_iter().any(|m| m.entity_id == parent && m.effective_to.is_none() && m.effective_from <= new.effective_from);
        if !open {
            return Err(Error::Invalid(format!("parent entity {parent} must be an open member of group {:?} on {}", group.name, new.effective_from)));
        }
    }
    tx.execute(
        "INSERT INTO entity_group_members (group_id, entity_id, parent_entity_id, ownership_bp, control, effective_from, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![group.id, new.entity_id, new.parent_entity_id, new.ownership_bp, new.control, new.effective_from.to_string(), now_ts()],
    )?;
    let member = get_member(&tx, tx.last_insert_rowid())?;
    audit::log(&tx, "entity_group_members", member.id, "add", None, Some(serde_json::to_value(&member)?))?;
    tx.commit()?;
    Ok(member)
}

/// End the open membership of an entity in a group on `to` (exclusive). Close or end the
/// memberships of its children first.
pub fn close_member(conn: &mut Connection, group_id: i64, entity_id: i64, to: NaiveDate) -> Result<Member> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let rows = list_members(&tx, group_id, None)?;
    let before =
        rows.iter().find(|m| m.entity_id == entity_id && m.effective_to.is_none()).cloned().ok_or_else(|| Error::NotFound(format!("open membership of entity {entity_id} in group {group_id}")))?;
    check_period(before.effective_from, Some(to))?;
    if let Some(child) = rows.iter().find(|m| m.parent_entity_id == Some(entity_id) && m.effective_to.is_none_or(|end| end > to)) {
        return Err(Error::Conflict(format!("entity {} is a child of entity {entity_id} after {to}; close membership #{} first", child.entity_id, child.id)));
    }
    tx.execute("UPDATE entity_group_members SET effective_to = ?2 WHERE id = ?1", params![before.id, to.to_string()])?;
    let after = get_member(&tx, before.id)?;
    audit::log(&tx, "entity_group_members", before.id, "close", Some(serde_json::to_value(&before)?), Some(serde_json::to_value(&after)?))?;
    tx.commit()?;
    Ok(after)
}

/// The members of a group on a date, as a tree of parents and children.
pub fn tree(conn: &Connection, group_id: i64, as_of: NaiveDate) -> Result<GroupTree> {
    let group = get_group(conn, group_id)?;
    let members = list_members(conn, group_id, Some(as_of))?;
    let present: std::collections::BTreeSet<i64> = members.iter().map(|m| m.entity_id).collect();
    let mut visited = std::collections::BTreeSet::new();
    let mut roots = vec![];
    for m in members.iter().filter(|m| m.parent_entity_id.is_none_or(|p| !present.contains(&p))) {
        roots.push(node(conn, &members, m, FULL_OWNERSHIP_BP, &mut visited)?);
    }
    if visited.len() != members.len() {
        return Err(Error::Conflict(format!("group {:?} has a parent cycle on {as_of}", group.name)));
    }
    Ok(GroupTree { group, as_of, roots })
}

fn node(conn: &Connection, members: &[Member], m: &Member, parent_bp: i64, visited: &mut std::collections::BTreeSet<i64>) -> Result<TreeNode> {
    if !visited.insert(m.entity_id) {
        return Err(Error::Conflict(format!("entity {} appears twice in the group tree", m.entity_id)));
    }
    let effective = parent_bp * m.ownership_bp / FULL_OWNERSHIP_BP;
    let mut children = vec![];
    for child in members.iter().filter(|c| c.parent_entity_id == Some(m.entity_id)) {
        children.push(node(conn, members, child, effective, visited)?);
    }
    Ok(TreeNode { member: m.clone(), entity: entities::get_entity(conn, m.entity_id)?, effective_ownership_bp: effective, children })
}
