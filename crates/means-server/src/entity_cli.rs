//! Commands for entity names, archive state, facts, addresses, flags, tax classifications and
//! entity groups.
//!
//! Each command prints one JSON object with `"schema_version": 1`. A failure prints
//! `{"schema_version":1,"ok":false,"error":{"code":..,"message":..}}` and exits with code 1.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};
use means_core::entities::{self, Classification, EntityFacts, EntityFactsPatch};
use means_core::entity_flags::{self, FlagPeriod};
use means_core::groups::{self, Group, GroupTree, Member, NewMember, TreeNode};
use means_core::{Db, Entity, Error};
use serde_json::{json, Map, Value};

pub const SCHEMA_VERSION: i64 = 1;

/// Output options that every command in this file shares.
#[derive(Args, Clone, Copy)]
pub struct Output {
    /// Print the JSON object on one line (the default prints it indented)
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct SetArgs {
    /// Entity id, uid or name
    pub entity: String,
    /// New name; it must be unique among the active entities
    #[arg(long)]
    pub name: Option<String>,
    /// Legal form, for example "Private company limited by shares"
    #[arg(long)]
    pub legal_form: Option<String>,
    /// Fiscal year end as MM-DD; an empty value clears it
    #[arg(long)]
    pub fye: Option<String>,
    /// Date of incorporation (YYYY-MM-DD); an empty value clears it
    #[arg(long)]
    pub incorporated_on: Option<String>,
    /// Registration or tax number as KEY=VALUE; repeat it; an empty VALUE removes KEY
    #[arg(long = "registration")]
    pub registrations: Vec<String>,
    /// Address part as KEY=VALUE: street, street2, city, region, postal_code or country; repeat it; an empty VALUE removes KEY
    #[arg(long = "address")]
    pub address: Vec<String>,
    /// Flag as KEY=VALUE, for example sg.gst_registered=true; repeat it; an empty VALUE removes the flag
    #[arg(long = "flag")]
    pub flags: Vec<String>,
    /// First day of the flag values (YYYY-MM-DD); it applies to every --flag of the command; omit it for undated flags
    #[arg(long, requires = "flags")]
    pub from: Option<String>,
    #[command(flatten)]
    pub output: Output,
}

#[derive(Args)]
pub struct ArchiveArgs {
    /// Entity id, uid or name
    pub entity: String,
    /// The person who archives the entity
    #[arg(long)]
    pub actor: String,
    /// Why the books stop, for example "rebuilt as a new entity"
    #[arg(long)]
    pub reason: String,
    #[command(flatten)]
    pub output: Output,
}

#[derive(Args)]
pub struct UnarchiveArgs {
    /// Entity id, uid or name
    pub entity: String,
    /// The person who unarchives the entity
    #[arg(long)]
    pub actor: String,
    /// Why the books open again
    #[arg(long)]
    pub reason: String,
    /// New name, when an active entity has the name of the archived entity
    #[arg(long)]
    pub name: Option<String>,
    #[command(flatten)]
    pub output: Output,
}

#[derive(Args)]
pub struct ClassifyArgs {
    /// Entity id, uid or name
    pub entity: String,
    /// Jurisdiction code, for example US or SG
    #[arg(long)]
    pub jurisdiction: String,
    /// Classification code, for example corporation or disregarded_entity
    #[arg(long)]
    pub classification: String,
    /// First day of the classification (YYYY-MM-DD)
    #[arg(long)]
    pub from: String,
    /// Day after the last day (YYYY-MM-DD); omit it for an open classification
    #[arg(long)]
    pub to: Option<String>,
    #[arg(long, default_value = "")]
    pub note: String,
    #[command(flatten)]
    pub output: Output,
}

#[derive(Args)]
pub struct FactsArgs {
    /// Entity id, uid or name
    pub entity: String,
    /// Date for flags, classifications and group membership (default: today)
    #[arg(long)]
    pub as_of: Option<String>,
    #[command(flatten)]
    pub output: Output,
}

#[derive(Subcommand)]
pub enum GroupCommand {
    /// Create a group of entities
    Add {
        name: String,
        #[command(flatten)]
        output: Output,
    },
    /// List groups
    List {
        #[command(flatten)]
        output: Output,
    },
    /// Add or close a member of a group
    Member {
        #[command(subcommand)]
        command: MemberCommand,
    },
    /// Show the members of a group as a tree with ownership on a date
    Show {
        /// Group uid or name
        name: String,
        /// Date of the tree (default: today)
        #[arg(long)]
        as_of: Option<String>,
        #[command(flatten)]
        output: Output,
    },
}

#[derive(Subcommand)]
pub enum MemberCommand {
    /// Add an entity to a group from a date
    Add {
        /// Group uid or name
        #[arg(long)]
        group: String,
        /// Entity id, uid or name
        #[arg(long)]
        entity: String,
        /// Entity that holds this entity; it must be a member of the same group
        #[arg(long)]
        parent: Option<String>,
        /// Basis points of the entity that the parent holds (10000 is 100%)
        #[arg(long, default_value_t = groups::FULL_OWNERSHIP_BP)]
        ownership_bp: i64,
        /// Record ownership without control
        #[arg(long)]
        no_control: bool,
        /// First day of the membership (default: today)
        #[arg(long)]
        from: Option<String>,
        #[command(flatten)]
        output: Output,
    },
    /// End the open membership of an entity
    Close {
        /// Group uid or name
        #[arg(long)]
        group: String,
        /// Entity id, uid or name
        #[arg(long)]
        entity: String,
        /// Day after the last day of the membership (YYYY-MM-DD)
        #[arg(long)]
        to: String,
        #[command(flatten)]
        output: Output,
    },
}

/// Print one JSON object. A failure prints the error object and exits with code 1.
pub fn emit(output: Output, result: Result<Map<String, Value>>) -> Result<()> {
    let (value, failed) = match result {
        Ok(fields) => {
            let mut out = Map::new();
            out.insert("schema_version".into(), json!(SCHEMA_VERSION));
            out.insert("ok".into(), json!(true));
            out.extend(fields);
            (Value::Object(out), false)
        }
        Err(e) => (json!({"schema_version": SCHEMA_VERSION, "ok": false, "error": {"code": error_code(&e), "message": e.to_string()}}), true),
    };
    println!("{}", if output.json { serde_json::to_string(&value)? } else { serde_json::to_string_pretty(&value)? });
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

fn error_code(e: &anyhow::Error) -> &'static str {
    e.downcast_ref::<Error>().map(means_core::proposal::error_code).unwrap_or("internal")
}

/// Turn a `json!({..})` object into the fields of a reply.
pub fn fields(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// Find an entity by id, uid or name. The name match ignores case. An ambiguous name is refused.
pub fn find_entity(conn: &means_core::rusqlite::Connection, key: &str) -> means_core::Result<Entity> {
    entities::find_by_key(conn, key)
}

pub fn date(s: &str) -> means_core::Result<chrono::NaiveDate> {
    means_core::parse_date(s)
}

pub fn opt_date(s: Option<&str>) -> means_core::Result<chrono::NaiveDate> {
    s.map(date).transpose().map(|d| d.unwrap_or_else(means_core::today))
}

pub fn entity_ref(e: &Entity) -> Value {
    json!({"uid": e.uid, "name": e.name})
}

/// The address and the flags of an entity. The flags are the values on one date.
pub struct Extras {
    pub address: std::collections::BTreeMap<String, String>,
    pub flags: std::collections::BTreeMap<String, Value>,
}

impl Extras {
    pub fn read(conn: &means_core::rusqlite::Connection, entity_id: i64, on: chrono::NaiveDate) -> means_core::Result<Extras> {
        Ok(Extras { address: entity_flags::get_address(conn, entity_id)?, flags: entity_flags::flags_as_of(conn, entity_id, on)? })
    }
}

pub fn flag_period_json(p: &FlagPeriod) -> Value {
    json!({"id": p.id, "key": p.key, "value": p.value, "effective_from": p.effective_from, "effective_to": p.effective_to})
}

fn entity_json(e: &Entity, facts: &EntityFacts, extras: &Extras) -> Value {
    json!({
        "uid": e.uid,
        "name": e.name,
        "kind": e.kind,
        "country": e.country,
        "currency": e.currency,
        "legal_form": facts.legal_form,
        "fiscal_year_end": facts.fiscal_year_end,
        "incorporated_on": facts.incorporated_on,
        "registrations": facts.registrations,
        "address": extras.address,
        "flags": extras.flags,
    })
}

fn classification_json(c: &Classification) -> Value {
    json!({
        "id": c.id,
        "jurisdiction": c.jurisdiction,
        "classification": c.classification,
        "effective_from": c.effective_from,
        "effective_to": c.effective_to,
        "note": c.note,
    })
}

fn group_json(g: &Group) -> Value {
    json!({"uid": g.uid, "name": g.name, "created_at": g.created_at})
}

fn member_json(conn: &means_core::rusqlite::Connection, m: &Member) -> means_core::Result<Value> {
    let entity = entities::get_entity(conn, m.entity_id)?;
    let parent = m.parent_entity_id.map(|p| entities::get_entity(conn, p)).transpose()?;
    Ok(json!({
        "id": m.id,
        "entity_uid": entity.uid,
        "entity_name": entity.name,
        "parent_entity_uid": parent.as_ref().map(|p| p.uid.clone()),
        "parent_entity_name": parent.as_ref().map(|p| p.name.clone()),
        "ownership_bp": m.ownership_bp,
        "control": m.control,
        "effective_from": m.effective_from,
        "effective_to": m.effective_to,
    }))
}

fn node_json(conn: &means_core::rusqlite::Connection, n: &TreeNode) -> means_core::Result<Value> {
    let mut v = member_json(conn, &n.member)?;
    v["effective_ownership_bp"] = json!(n.effective_ownership_bp);
    v["children"] = Value::Array(n.children.iter().map(|c| node_json(conn, c)).collect::<means_core::Result<Vec<_>>>()?);
    Ok(v)
}

fn tree_json(conn: &means_core::rusqlite::Connection, t: &GroupTree) -> means_core::Result<Value> {
    Ok(json!({
        "group": group_json(&t.group),
        "as_of": t.as_of,
        "members": t.roots.iter().map(|n| node_json(conn, n)).collect::<means_core::Result<Vec<_>>>()?,
    }))
}

pub fn set(path: PathBuf, args: SetArgs) -> Result<()> {
    let output = args.output;
    emit(
        output,
        (|| -> Result<Map<String, Value>> {
            let db = Db::open(path)?;
            let mut conn = db.conn();
            let entity = find_entity(&conn, &args.entity)?;
            entities::ensure_active(&conn, entity.id)?;
            let registrations = args.registrations.iter().map(|s| entities::parse_pair(s)).collect::<means_core::Result<Vec<_>>>()?;
            let address = args.address.iter().map(|s| entity_flags::parse_address_pair(s)).collect::<means_core::Result<Vec<_>>>()?;
            let from = args.from.as_deref().map(date).transpose()?;
            let flags = args.flags.iter().map(|s| entity_flags::parse_flag(s, from)).collect::<means_core::Result<Vec<_>>>()?;
            let patch = EntityFactsPatch { legal_form: args.legal_form, fiscal_year_end: args.fye, incorporated_on: args.incorporated_on, registrations };
            // One transaction: the command changes all the facts or none of them.
            let tx = conn.transaction_with_behavior(means_core::rusqlite::TransactionBehavior::Immediate)?;
            let entity = match args.name.as_deref() {
                Some(name) => entities::rename_in(&tx, entity.id, name)?,
                None => entity,
            };
            let facts = entities::set_facts_in(&tx, entity.id, patch)?;
            entity_flags::set_address_in(&tx, entity.id, &address)?;
            entity_flags::set_flags_in(&tx, entity.id, &flags)?;
            let extras = Extras::read(&tx, entity.id, means_core::today())?;
            let history = entity_flags::flag_history(&tx, entity.id)?;
            tx.commit()?;
            Ok(fields(json!({"entity": entity_json(&entity, &facts, &extras), "flag_history": history.iter().map(flag_period_json).collect::<Vec<_>>()})))
        })(),
    )
}

fn archive_json(outcome: &entities::ArchiveOutcome, actor: &str, reason: &str) -> Value {
    let e = &outcome.entity;
    json!({
        "entity": {"uid": e.uid, "name": e.name, "kind": e.kind, "currency": e.currency, "archived": e.archived_at.is_some(), "archived_at": e.archived_at},
        "actor": actor.trim(),
        "reason": reason.trim(),
        "audit_event_id": outcome.audit_event_id,
        "live_links": outcome.live_links,
    })
}

/// Archive an entity: a human action, refused while `MEANS_AGENT=1`.
pub fn archive(path: PathBuf, args: ArchiveArgs) -> Result<()> {
    crate::entry_cli::guard("entity archive");
    emit(
        args.output,
        (|| -> Result<Map<String, Value>> {
            let db = Db::open(path)?;
            let mut conn = db.conn();
            let entity = find_entity(&conn, &args.entity)?;
            let outcome = entities::archive(&mut conn, entity.id, &args.actor, &args.reason)?;
            Ok(fields(archive_json(&outcome, &args.actor, &args.reason)))
        })(),
    )
}

/// Unarchive an entity: a human action, refused while `MEANS_AGENT=1`.
pub fn unarchive(path: PathBuf, args: UnarchiveArgs) -> Result<()> {
    crate::entry_cli::guard("entity unarchive");
    emit(
        args.output,
        (|| -> Result<Map<String, Value>> {
            let db = Db::open(path)?;
            let mut conn = db.conn();
            // An unarchive acts on an archived entity: an archived name match wins.
            let entity = entities::find_archived_by_key(&conn, &args.entity)?;
            let outcome = entities::unarchive(&mut conn, entity.id, &args.actor, &args.reason, args.name.as_deref())?;
            Ok(fields(archive_json(&outcome, &args.actor, &args.reason)))
        })(),
    )
}

pub fn classify(path: PathBuf, args: ClassifyArgs) -> Result<()> {
    emit(
        args.output,
        (|| -> Result<Map<String, Value>> {
            let db = Db::open(path)?;
            let mut conn = db.conn();
            let entity = find_entity(&conn, &args.entity)?;
            let to = args.to.as_deref().map(date).transpose()?;
            let saved = entities::add_classification(&mut conn, entity.id, &args.jurisdiction, &args.classification, date(&args.from)?, to, &args.note)?;
            Ok(fields(json!({"entity": entity_ref(&entity), "classification": classification_json(&saved)})))
        })(),
    )
}

pub fn facts(path: PathBuf, args: FactsArgs) -> Result<()> {
    emit(
        args.output,
        (|| -> Result<Map<String, Value>> {
            let db = Db::open(path)?;
            let conn = db.conn();
            let entity = find_entity(&conn, &args.entity)?;
            let as_of = opt_date(args.as_of.as_deref())?;
            let facts = entities::get_facts(&conn, entity.id)?;
            let extras = Extras::read(&conn, entity.id, as_of)?;
            let history = entity_flags::flag_history(&conn, entity.id)?;
            let classifications = entities::list_classifications(&conn, entity.id, Some(as_of))?;
            let group = match groups::membership_of(&conn, entity.id, as_of)? {
                Some(m) => {
                    let mut v = member_json(&conn, &m)?;
                    v["group"] = group_json(&groups::get_group(&conn, m.group_id)?);
                    v
                }
                None => Value::Null,
            };
            Ok(fields(json!({
                "as_of": as_of,
                "entity": entity_json(&entity, &facts, &extras),
                "flag_history": history.iter().map(flag_period_json).collect::<Vec<_>>(),
                "classifications": classifications.iter().map(classification_json).collect::<Vec<_>>(),
                "group": group,
            })))
        })(),
    )
}

pub fn group(path: PathBuf, command: GroupCommand) -> Result<()> {
    let output = match &command {
        GroupCommand::Add { output, .. } | GroupCommand::List { output } | GroupCommand::Show { output, .. } => *output,
        GroupCommand::Member { command: MemberCommand::Add { output, .. } | MemberCommand::Close { output, .. } } => *output,
    };
    emit(
        output,
        (|| -> Result<Map<String, Value>> {
            let db = Db::open(path)?;
            let mut conn = db.conn();
            Ok(match command {
                GroupCommand::Add { name, .. } => fields(json!({"group": group_json(&groups::create_group(&mut conn, &name)?)})),
                GroupCommand::List { .. } => fields(json!({"groups": groups::list_groups(&conn)?.iter().map(group_json).collect::<Vec<_>>()})),
                GroupCommand::Show { name, as_of, .. } => {
                    let group = groups::find_group(&conn, &name)?;
                    fields(tree_json(&conn, &groups::tree(&conn, group.id, opt_date(as_of.as_deref())?)?)?)
                }
                GroupCommand::Member { command: MemberCommand::Add { group, entity, parent, ownership_bp, no_control, from, .. } } => {
                    let group = groups::find_group(&conn, &group)?;
                    let entity = find_entity(&conn, &entity)?;
                    let parent = parent.map(|p| find_entity(&conn, &p)).transpose()?;
                    let new =
                        NewMember { group_id: group.id, entity_id: entity.id, parent_entity_id: parent.map(|p| p.id), ownership_bp, control: !no_control, effective_from: opt_date(from.as_deref())? };
                    let member = groups::add_member(&mut conn, new)?;
                    fields(json!({"group": group_json(&group), "member": member_json(&conn, &member)?}))
                }
                GroupCommand::Member { command: MemberCommand::Close { group, entity, to, .. } } => {
                    let group = groups::find_group(&conn, &group)?;
                    let entity = find_entity(&conn, &entity)?;
                    let member = groups::close_member(&mut conn, group.id, entity.id, date(&to)?)?;
                    fields(json!({"group": group_json(&group), "member": member_json(&conn, &member)?}))
                }
            })
        })(),
    )
}
