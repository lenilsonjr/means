//! Versioned, schema-backed typed records. Only this allowlist generates local SQL.
use crate::crypto;
use anyhow::{bail, ensure, Context, Result};
use means_core::{hashchain, money::Money, Db};
use rusqlite::{
    params,
    types::{Value, ValueRef},
    Connection,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "value", rename_all = "lowercase", deny_unknown_fields)]
pub enum Cell {
    Text(String),
    Integer(String),
    Null,
    Reference(String),
    Opaque(String),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub key: String,
    pub fields: BTreeMap<String, Cell>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    pub kind: String,
    pub records: Vec<Record>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub tables: Vec<Table>,
}
struct Spec {
    table: &'static str,
    fields: &'static str,
    ints: &'static str,
    refs: &'static [(&'static str, &'static str)],
    opaque: &'static [&'static str],
    uid: bool,
}
const SPECS:&[Spec]=&[
 Spec{table:"entities",fields:"name kind country currency lock_date archived_at created_at updated_at",ints:"",refs:&[],opaque:&[],uid:true},
 Spec{table:"commodities",fields:"code kind name precision isin",ints:"precision",refs:&[],opaque:&[],uid:false},
 Spec{table:"prices",fields:"commodity_id currency_id on_date price source",ints:"",refs:&[("commodity_id","commodities"),("currency_id","commodities")],opaque:&[],uid:false},
 Spec{table:"accounts",fields:"entity_id parent_id code name type subtype commodity_id system_role placeholder in_net_worth credit_limit statement_day due_day external_ids notes position closed_at created_at updated_at class",ints:"placeholder in_net_worth statement_day due_day position",refs:&[("entity_id","entities"),("parent_id","accounts"),("commodity_id","commodities")],opaque:&[],uid:true},
 Spec{table:"payees",fields:"entity_id name active",ints:"active",refs:&[("entity_id","entities")],opaque:&[],uid:true},
 Spec{table:"journal_entries",fields:"entity_id date payee description notes status reverses_id counterpart_id template_id template_version origin posted_at seq prev_hash hash created_at updated_at reviewed_at refund_of_id payee_id",ints:"template_version seq",refs:&[("entity_id","entities"),("reverses_id","journal_entries"),("refund_of_id","journal_entries"),("payee_id","payees")],opaque:&["counterpart_id","template_id"],uid:true},
 Spec{table:"postings",fields:"journal_entry_id account_id quantity amount rate rate_source memo metadata external_id fingerprint reconciled_at position",ints:"quantity amount position",refs:&[("journal_entry_id","journal_entries"),("account_id","accounts")],opaque:&[],uid:true},
 Spec{table:"entry_tags",fields:"entry_id key value",ints:"",refs:&[("entry_id","journal_entries")],opaque:&[],uid:false},
 Spec{table:"budgets",fields:"entity_id name scope account_id class tag starts_on ends_on amount created_at updated_at",ints:"amount",refs:&[("entity_id","entities"),("account_id","accounts")],opaque:&[],uid:true},
];
fn columns(s: &str) -> impl Iterator<Item = &str> {
    s.split_whitespace()
}
type RawRecord = (i64, Option<String>, BTreeMap<String, Value>);
fn row_values(conn: &Connection, spec: &Spec) -> Result<Vec<RawRecord>> {
    let id = if spec.table == "entry_tags" { "row_number() OVER (ORDER BY entry_id,key)" } else { "id" };
    let sql = format!("SELECT {id},{},{} FROM {} ORDER BY 1", if spec.uid { "uid" } else { "NULL" }, columns(spec.fields).collect::<Vec<_>>().join(","), spec.table);
    let mut q = conn.prepare(&sql)?;
    let rows = q
        .query_map([], |r| {
            let mut fields = BTreeMap::new();
            for (index, name) in columns(spec.fields).enumerate() {
                fields.insert(
                    name.to_string(),
                    match r.get_ref(index + 2)? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(v) => Value::Integer(v),
                        ValueRef::Text(v) => Value::Text(String::from_utf8_lossy(v).into_owned()),
                        _ => return Err(rusqlite::Error::InvalidQuery),
                    },
                );
            }
            Ok((r.get(0)?, r.get(1)?, fields))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}
fn int(fields: &BTreeMap<String, Value>, name: &str) -> Option<i64> {
    match fields.get(name) {
        Some(Value::Integer(v)) => Some(*v),
        _ => None,
    }
}
fn text(fields: &BTreeMap<String, Value>, name: &str) -> Result<String> {
    match fields.get(name) {
        Some(Value::Text(v)) => Ok(v.clone()),
        _ => bail!("missing text field {name}"),
    }
}

/// Transactional caller snapshot: no cross-vault data or global reference-table dump.
pub fn capture(conn: &Connection, entity: i64) -> Result<Snapshot> {
    let vault = means_core::entities::get_entity(conn, entity)?;
    let raw = SPECS.iter().map(|s| Ok((s.table, row_values(conn, s)?))).collect::<Result<HashMap<_, _>>>()?;
    let ids = |table: &str| raw[table].iter().filter(|(_, _, f)| int(f, "entity_id") == Some(entity)).map(|(id, _, _)| *id).collect::<HashSet<_>>();
    let account_ids = ids("accounts");
    let entry_ids = ids("journal_entries");
    let mut commodity_ids: HashSet<i64> = raw["accounts"].iter().filter(|(id, _, _)| account_ids.contains(id)).filter_map(|(_, _, f)| int(f, "commodity_id")).collect();
    for (id, _, f) in &raw["commodities"] {
        if text(f, "code")? == vault.currency {
            commodity_ids.insert(*id);
        }
    }
    // Retain direct/inverse prices and common bridge quotes used by rate_for, not
    // unrelated assets' prices. Only bridges connecting required commodities qualify.
    let needed = commodity_ids.clone();
    let mut bridges: HashMap<i64, HashSet<i64>> = HashMap::new();
    for (_, _, f) in &raw["prices"] {
        let a = int(f, "commodity_id").context("price commodity")?;
        let b = int(f, "currency_id").context("price currency")?;
        if needed.contains(&a) {
            bridges.entry(b).or_default().insert(a);
        }
        if needed.contains(&b) {
            bridges.entry(a).or_default().insert(b);
        }
    }
    commodity_ids.extend(bridges.into_iter().filter(|(_, s)| s.len() >= 2).map(|(id, _)| id));
    let mut chosen = HashMap::new();
    for spec in SPECS {
        let rows = raw[spec.table]
            .iter()
            .filter(|(id, _, f)| match spec.table {
                "entities" => *id == entity,
                "commodities" => commodity_ids.contains(id),
                "prices" => int(f, "commodity_id").is_some_and(|v| commodity_ids.contains(&v)) && int(f, "currency_id").is_some_and(|v| commodity_ids.contains(&v)),
                "accounts" => account_ids.contains(id),
                "journal_entries" => entry_ids.contains(id),
                "postings" => int(f, "journal_entry_id").is_some_and(|v| entry_ids.contains(&v)),
                "entry_tags" => int(f, "entry_id").is_some_and(|v| entry_ids.contains(&v)),
                _ => int(f, "entity_id") == Some(entity),
            })
            .cloned()
            .collect::<Vec<_>>();
        chosen.insert(spec.table, rows);
    }
    let mut keys = HashMap::<(&str, i64), String>::new();
    for spec in SPECS {
        for (id, uid, f) in &chosen[spec.table] {
            if spec.uid {
                keys.insert((spec.table, *id), uid.clone().context("missing stable UID")?);
            } else if spec.table == "commodities" {
                keys.insert((spec.table, *id), text(f, "code")?);
            }
        }
    }
    for spec in SPECS {
        if spec.table == "prices" || spec.table == "entry_tags" {
            for (id, _, f) in &chosen[spec.table] {
                let key = if spec.table == "prices" {
                    serde_json::to_string(&(
                        keys.get(&("commodities", int(f, "commodity_id").unwrap())).context("commodity reference")?,
                        keys.get(&("commodities", int(f, "currency_id").unwrap())).context("currency reference")?,
                        text(f, "on_date")?,
                    ))?
                } else {
                    serde_json::to_string(&(keys.get(&("journal_entries", int(f, "entry_id").unwrap())).context("entry reference")?, text(f, "key")?))?
                };
                keys.insert((spec.table, *id), key);
            }
        }
    }
    let mut tables = vec![];
    for spec in SPECS {
        let mut records = vec![];
        for (id, _, fields) in &chosen[spec.table] {
            let mut out = BTreeMap::new();
            for (name, v) in fields {
                let value = if matches!(v, Value::Null) {
                    Cell::Null
                } else if let Some((_, kind)) = spec.refs.iter().find(|(n, _)| n == name) {
                    Cell::Reference(keys.get(&(*kind, int(fields, name).context("invalid reference")?)).context("cross-vault or unresolved reference")?.clone())
                } else if spec.opaque.contains(&name.as_str()) {
                    let table = if name == "template_id" { "entry_templates" } else { "journal_entries" };
                    let uid: String = conn.query_row(&format!("SELECT uid FROM {table} WHERE id=?1"), [int(fields, name).context("invalid opaque reference")?], |r| r.get(0))?;
                    Cell::Opaque(uid)
                } else {
                    match v {
                        Value::Integer(n) => Cell::Integer(n.to_string()),
                        Value::Text(v) => Cell::Text(v.clone()),
                        _ => bail!("unsupported snapshot value"),
                    }
                };
                out.insert(name.clone(), value);
            }
            records.push(Record { key: keys[&(spec.table, *id)].clone(), fields: out });
        }
        records.sort_by(|a, b| a.key.cmp(&b.key));
        tables.push(Table { kind: spec.table.into(), records });
    }
    Ok(Snapshot { tables })
}

/// Install into an empty, freshly migrated local database; never execute bundle SQL.
pub fn install(db: &Db, snapshot: &Snapshot, vault: &str, head: &str, count: u64) -> Result<()> {
    ensure!(snapshot.tables.len() == SPECS.len(), "unexpected snapshot table set");
    ensure!(snapshot.tables.iter().map(|t| t.records.len()).sum::<usize>() <= 250_000, "too many snapshot records");
    let mut tables = HashMap::new();
    for t in &snapshot.tables {
        ensure!(tables.insert(t.kind.as_str(), t).is_none(), "duplicate table kind");
    }
    let mut ids = HashMap::new();
    for spec in SPECS {
        let t = tables.get(spec.table).context("missing snapshot table")?;
        for (i, r) in t.records.iter().enumerate() {
            ensure!(!r.key.is_empty() && r.key.len() <= 1024, "invalid record identity");
            ensure!(ids.insert((spec.table, r.key.clone()), i as i64 + 1).is_none(), "duplicate record identity");
            if spec.uid {
                uuid::Uuid::parse_str(&r.key).context("invalid record UID")?;
            }
            let cell_text = |name: &str| -> Result<&str> {
                match r.fields.get(name) {
                    Some(Cell::Text(v)) | Some(Cell::Reference(v)) => Ok(v),
                    _ => bail!("invalid natural key field {name}"),
                }
            };
            let natural = match spec.table {
                "commodities" => Some(cell_text("code")?.to_string()),
                "prices" => Some(serde_json::to_string(&(cell_text("commodity_id")?, cell_text("currency_id")?, cell_text("on_date")?))?),
                "entry_tags" => Some(serde_json::to_string(&(cell_text("entry_id")?, cell_text("key")?))?),
                _ => None,
            };
            if let Some(key) = natural {
                ensure!(key == r.key, "record natural key mismatch");
            }
            let expected: BTreeSet<_> = columns(spec.fields).collect();
            ensure!(r.fields.keys().map(|s| s.as_str()).collect::<BTreeSet<_>>() == expected, "wrong fields for {}", spec.table);
        }
    }
    ensure!(tables["entities"].records.len() == 1 && tables["entities"].records[0].key == vault, "wrong vault in snapshot");
    let mut c = db.conn();
    let tx = c.transaction()?;
    tx.execute_batch("PRAGMA defer_foreign_keys=ON; DELETE FROM prices; DELETE FROM commodities;")?;
    tx.execute_batch("CREATE TABLE replica_references(entry_uid TEXT NOT NULL,field TEXT NOT NULL,target_uid TEXT NOT NULL,PRIMARY KEY(entry_uid,field));")?;
    for spec in SPECS {
        let table = tables[spec.table];
        for r in &table.records {
            let mut cols = vec![];
            let mut values = vec![];
            if spec.table != "entry_tags" {
                cols.push("id");
                values.push(Value::Integer(ids[&(spec.table, r.key.clone())]));
            }
            if spec.uid {
                cols.push("uid");
                values.push(Value::Text(r.key.clone()));
            }
            for name in columns(spec.fields) {
                let cell = &r.fields[name];
                let value = match cell {
                    Cell::Null => Value::Null,
                    Cell::Reference(key) => {
                        let (_, target) = spec.refs.iter().find(|(n, _)| *n == name).context("reference in a scalar field")?;
                        Value::Integer(*ids.get(&(*target, key.clone())).context("unresolved required reference")?)
                    }
                    Cell::Opaque(key) => {
                        ensure!(spec.opaque.contains(&name), "opaque reference in wrong field");
                        ensure!(!key.is_empty() && key.len() <= 256, "bad opaque UID");
                        tx.execute("INSERT INTO replica_references VALUES(?1,?2,?3)", params![r.key, name, key])?;
                        Value::Null
                    }
                    Cell::Integer(n) => {
                        ensure!(columns(spec.ints).any(|c| c == name), "integer in wrong field");
                        let value: i64 = n.parse()?;
                        ensure!(value.to_string() == *n, "noncanonical integer");
                        Value::Integer(value)
                    }
                    Cell::Text(v) => {
                        ensure!(!columns(spec.ints).any(|c| c == name) && !spec.refs.iter().any(|(n, _)| *n == name) && !spec.opaque.contains(&name), "text in wrong field");
                        ensure!(v.len() <= 1024 * 1024, "field too large");
                        Value::Text(v.clone())
                    }
                };
                cols.push(name);
                values.push(value);
            }
            let sql = format!("INSERT INTO {} ({}) VALUES ({})", spec.table, cols.join(","), vec!["?"; cols.len()].join(","));
            tx.execute(&sql, rusqlite::params_from_iter(values))?;
        }
    }
    validate(&tx, vault, head, count)?;
    tx.execute_batch("CREATE TABLE means_replica_marker(version TEXT NOT NULL,vault TEXT NOT NULL); ")?;
    tx.execute("INSERT INTO means_replica_marker VALUES('1',?1)", [vault])?;
    // Persistently reject writes even if callers bypass the read-only Db opener.
    let names = {
        let mut q = tx.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")?;
        let out = q.query_map([], |r| r.get::<_, String>(0))?.collect::<std::result::Result<Vec<_>, _>>()?;
        out
    };
    for (index, name) in names.iter().enumerate() {
        for op in ["INSERT", "UPDATE", "DELETE"] {
            tx.execute_batch(&format!("CREATE TRIGGER replica_{index}_{op} BEFORE {op} ON \"{name}\" BEGIN SELECT RAISE(ABORT,'read-only shared vault'); END;"))?;
        }
    }
    tx.commit()?;
    Ok(())
}
fn validate(c: &Connection, vault: &str, head: &str, count: u64) -> Result<()> {
    let foreign: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check)", [], |r| r.get(0))?;
    ensure!(!foreign, "unresolved foreign key");
    let mut q =
        c.prepare("SELECT lock_date FROM entities WHERE lock_date IS NOT NULL UNION ALL SELECT starts_on FROM budgets UNION ALL SELECT ends_on FROM budgets UNION ALL SELECT on_date FROM prices")?;
    for date in q.query_map([], |r| r.get::<_, String>(0))? {
        let date = date?;
        let parsed: chrono::NaiveDate = date.parse()?;
        ensure!(parsed.to_string() == date, "noncanonical date");
    }
    let mut q = c.prepare("SELECT price FROM prices")?;
    for price in q.query_map([], |r| r.get::<_, String>(0))? {
        let value: rust_decimal::Decimal = price?.parse()?;
        ensure!(value > rust_decimal::Decimal::ZERO, "nonpositive price");
    }
    let mut q = c.prepare("SELECT external_ids FROM accounts UNION ALL SELECT metadata FROM postings")?;
    for value in q.query_map([], |r| r.get::<_, String>(0))? {
        let value: serde_json::Value = serde_json::from_str(&value?)?;
        ensure!(value.is_object(), "invalid metadata object");
    }
    let entity = means_core::entities::list_entities(c, true)?.into_iter().next().context("missing entity")?;
    ensure!(entity.uid == vault, "vault identity mismatch");
    if let Some(date) = entity.lock_date {
        ensure!(date.to_string().len() == 10, "invalid lock date");
    }
    for commodity in means_core::entities::list_commodities(c)? {
        Money::from_minor(0, &commodity.code, commodity.precision)?;
    }
    let accounts = means_core::accounts::list_accounts(c, Some(entity.id), true)?;
    for a in &accounts {
        let mut seen = HashSet::new();
        let mut parent = a.parent_id;
        while let Some(id) = parent {
            ensure!(seen.insert(id) && id != a.id, "cyclic account chart");
            let p = accounts.iter().find(|p| p.id == id).context("missing account parent")?;
            ensure!(p.r#type == a.r#type, "account parent type mismatch");
            parent = p.parent_id;
        }
    }
    let mut q = c.prepare("SELECT id,date,status,seq FROM journal_entries ORDER BY id")?;
    let entries = q.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<i64>>(3)?)))?.collect::<std::result::Result<Vec<_>, _>>()?;
    for (id, date, status, seq) in entries {
        let _: chrono::NaiveDate = date.parse()?;
        ensure!(if status == "draft" { seq.is_none() } else { seq.is_some() }, "entry checkpoint status mismatch");
        let entry = means_core::journal::get_entry(c, id)?;
        ensure!(entry.postings.len() >= 2, "entry has too few postings");
        let mut amount = 0i128;
        for p in &entry.postings {
            amount += p.amount.minor() as i128;
            ensure!(p.amount.commodity() == entity.currency, "posting functional currency mismatch");
        }
        ensure!(amount == 0, "unbalanced shared entry");
    }
    let check = hashchain::verify(c, entity.id)?;
    ensure!(check.first_bad_seq.is_none() && check.checked as u64 == count && check.head.as_deref().unwrap_or("") == head, "ledger checkpoint mismatch");
    let bad: bool =
        c.query_row("SELECT EXISTS(SELECT 1 FROM journal_entries WHERE seq IS NOT NULL AND (seq<1 OR seq>(SELECT COUNT(*) FROM journal_entries WHERE seq IS NOT NULL)))", [], |r| r.get(0))?;
    ensure!(!bad, "invalid ledger sequence");
    Ok(())
}
pub fn payload_digest(snapshot: &Option<Snapshot>) -> Result<String> {
    Ok(crypto::digest(&crypto::canonical(snapshot)?))
}
