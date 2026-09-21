//! Explicit historical changes of a vault's accounting currency.
//! Native accounts, posting quantities and statement evidence keep their identities.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::NaiveDate;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use rust_decimal::Decimal;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{accounts, audit, entities, hashchain, journal, money, money::Money, AccountType, Error, JournalEntry, NewAccount, Result};

#[derive(Debug, Clone, Serialize)]
pub struct PostingChange {
    pub posting_id: i64,
    pub before: Money,
    pub after: Money,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EntryChange {
    pub entry_id: i64,
    pub date: NaiveDate,
    pub postings: Vec<PostingChange>,
    pub fx_adjustment: Money,
    pub reverses_entry: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BudgetChange {
    pub budget_id: i64,
    pub starts_on: NaiveDate,
    pub before: Money,
    pub after: Money,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Preview {
    pub entity_id: i64,
    pub entity_name: String,
    pub from: String,
    pub to: String,
    pub old_chain_head: Option<String>,
    pub locked_entries: usize,
    pub reconciled_postings: usize,
    pub fx_account_to_replace: Option<i64>,
    pub fx_account_name: String,
    pub entries: Vec<EntryChange>,
    pub budgets: Vec<BudgetChange>,
    pub database_digest: String,
    pub confirmation: String,
}

/// Open an existing owned ledger without schema migrations or other implicit writes.
pub fn open_existing(path: &Path, writable: bool) -> Result<Connection> {
    let flags = if writable { OpenFlags::SQLITE_OPEN_READ_WRITE } else { OpenFlags::SQLITE_OPEN_READ_ONLY };
    let conn = Connection::open_with_flags(path, flags)?;
    if crate::db::replica_marker(&conn)? {
        return Err(Error::Locked("shared vaults cannot change accounting currency".into()));
    }
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version != 16 {
        return Err(Error::Invalid(format!("currency migration requires ledger schema 16; found {version}")));
    }
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")?;
    Ok(conn)
}

/// Hold one database snapshot throughout preview generation.
pub fn preview(conn: &mut Connection, entity_id: i64, target: &str) -> Result<Preview> {
    let tx = conn.transaction()?;
    build_preview(&tx, entity_id, target)
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

fn multiply(value: Decimal, rate: Decimal) -> Result<Decimal> {
    value.checked_mul(rate).ok_or_else(|| invalid("currency migration amount overflow"))
}

// This command does not use the general 400-day quote fallback. An export-date
// Account Tracker snapshot is not historical evidence. Seven days allow weekends
// and holidays; every actual quote date remains visible in the preview.
fn quote(conn: &Connection, from: &str, to: &str, date: NaiveDate) -> Result<Option<(Decimal, String)>> {
    let row: Option<(String, String, String)> = conn
        .query_row(
            "SELECT p.price,p.on_date,p.source FROM prices p JOIN commodities c ON c.id=p.commodity_id
         JOIN commodities t ON t.id=p.currency_id WHERE c.code=?1 AND t.code=?2 AND p.on_date<=?3
         AND p.source<>'import' ORDER BY p.on_date DESC LIMIT 1",
            params![from, to, date.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((rate, on, source)) = row else { return Ok(None) };
    if (date - crate::parse_date(&on)?).num_days() > 7 {
        return Ok(None);
    }
    let rate = money::parse(&rate)?;
    if rate <= Decimal::ZERO {
        return Err(invalid(format!("invalid historical rate {from}/{to} on {on}")));
    }
    Ok(Some((rate, format!("{from}/{to}={rate} on {on} ({source})"))))
}

fn pair(conn: &Connection, from: &str, to: &str, date: NaiveDate) -> Result<Option<(Decimal, String)>> {
    if from == to {
        return Ok(Some((Decimal::ONE, "same currency".into())));
    }
    if let Some(q) = quote(conn, from, to, date)? {
        return Ok(Some(q));
    }
    Ok(quote(conn, to, from, date)?.map(|(r, s)| (Decimal::ONE / r, format!("inverse {s}"))))
}

fn historical(conn: &Connection, from: &str, to: &str, date: NaiveDate) -> Result<(Decimal, String)> {
    if let Some(q) = pair(conn, from, to, date)? {
        return Ok(q);
    }
    let mut stmt = conn.prepare("SELECT code FROM commodities WHERE kind='currency' ORDER BY code")?;
    for via in stmt.query_map([], |r| r.get::<_, String>(0))? {
        let via = via?;
        if via == from || via == to {
            continue;
        }
        if let (Some((a, sa)), Some((b, sb))) = (pair(conn, from, &via, date)?, pair(conn, &via, to, date)?) {
            return Ok((multiply(a, b)?, format!("cross {sa}; {sb}")));
        }
    }
    Err(invalid(format!("missing historical rate {from}/{to} for {date}; need a dated quote within the preceding seven days (export snapshots do not qualify)")))
}

fn evidence(post: &crate::Posting, target: &str, precision: u32) -> Result<Option<(Money, String)>> {
    if post.quantity.commodity() == target {
        return Ok(Some((post.quantity, "native target-currency quantity".into())));
    }
    if post.external_id.as_deref().is_some_and(|s| s.starts_with("at:")) {
        if let Some(original) = post.metadata.get("original").filter(|v| v["commodity"].as_str() == Some(target)) {
            let raw = original["quantity"].as_str().ok_or_else(|| invalid(format!("invalid original amount on posting {}", post.id)))?;
            let raw = money::parse(raw)?.abs();
            if raw.is_zero() || post.quantity.is_zero() {
                return Err(invalid(format!("ambiguous zero original amount on posting {}", post.id)));
            }
            let signed = if post.quantity.major() < Decimal::ZERO { -raw } else { raw };
            return Ok(Some((Money::from_major(signed, target, precision)?, "Account Tracker original transaction amount".into())));
        }
    }
    if let Some(v) = post.metadata.get("value_in").filter(|v| v["commodity"].as_str() == Some(target)) {
        let raw = v["quantity"].as_str().ok_or_else(|| invalid(format!("invalid value_in on posting {}", post.id)))?;
        return Ok(Some((Money::from_major(money::parse(raw)?, target, precision)?, "explicit transaction value_in".into())));
    }
    Ok(None)
}

fn build_preview(conn: &Connection, entity_id: i64, target: &str) -> Result<Preview> {
    let entity = entities::get_entity(conn, entity_id)?;
    let target = entities::get_commodity_by_code(conn, &entities::normalize_code(target)?)?;
    if target.kind != "currency" || target.code == entity.currency {
        return Err(invalid("select a different accounting currency"));
    }
    let verified = hashchain::verify(conn, entity_id)?;
    if verified.first_bad_seq.is_some() {
        return Err(invalid("repair the existing hash chain before currency migration"));
    }
    let fx = accounts::find_by_role(conn, entity_id, "fx_gain_loss")?;
    let replace_fx = (fx.commodity != target.code).then_some(fx.id);
    let fx_name = if replace_fx.is_some() { format!("FX gain/loss ({} migration)", target.code) } else { fx.name };
    if replace_fx.is_some() && accounts::list_accounts(conn, Some(entity_id), true)?.iter().any(|a| a.name == fx_name && a.parent_id.is_none() && a.r#type == AccountType::Expense) {
        return Err(invalid(format!("account name already exists: {fx_name}; rename it before migration")));
    }
    let mut stmt = conn.prepare("SELECT id FROM journal_entries WHERE entity_id=?1 ORDER BY id")?;
    let ids = stmt.query_map([entity_id], |r| r.get::<_, i64>(0))?.collect::<std::result::Result<Vec<_>, _>>()?;
    let mut pending = ids.into_iter().map(|id| journal::get_entry(conn, id)).collect::<Result<Vec<_>>>()?;
    let mut result = Preview {
        entity_id,
        entity_name: entity.name,
        from: entity.currency.clone(),
        to: target.code.clone(),
        old_chain_head: verified.head,
        locked_entries: pending.iter().filter(|e| entity.lock_date.is_some_and(|d| e.date <= d)).count(),
        reconciled_postings: pending.iter().flat_map(|e| &e.postings).filter(|p| p.reconciled_at.is_some()).count(),
        fx_account_to_replace: replace_fx,
        fx_account_name: fx_name,
        entries: vec![],
        budgets: vec![],
        database_digest: String::new(),
        confirmation: String::new(),
    };
    let mut done: BTreeMap<i64, EntryChange> = BTreeMap::new();
    let mut originals: BTreeMap<i64, JournalEntry> = BTreeMap::new();
    while !pending.is_empty() {
        let index = pending
            .iter()
            .position(|e| e.reverses_id.or(e.refund_of_id).is_none_or(|id| done.contains_key(&id)))
            .ok_or_else(|| invalid("refund/reversal references are missing, outside this vault, or cyclic"))?;
        let entry = pending.remove(index);
        let change = convert_entry(conn, &entry, &entity.currency, &target.code, target.precision, &done, &originals)?;
        done.insert(entry.id, change.clone());
        originals.insert(entry.id, entry);
        result.entries.push(change);
    }
    let mut stmt = conn.prepare("SELECT id FROM budgets WHERE entity_id=?1 ORDER BY id")?;
    for id in stmt.query_map([entity_id], |r| r.get::<_, i64>(0))? {
        let b = crate::budgets::get(conn, id?)?;
        let (rate, source) = historical(conn, &entity.currency, &target.code, b.starts_on)?;
        result.budgets.push(BudgetChange {
            budget_id: b.id,
            starts_on: b.starts_on,
            before: b.limit,
            after: Money::from_major(multiply(b.limit.major(), rate)?, &target.code, target.precision)?,
            source,
        });
    }
    result.database_digest = database_digest(conn)?;
    result.confirmation = hex::encode(Sha256::digest(serde_json::to_vec(&result)?));
    Ok(result)
}

fn convert_entry(conn: &Connection, entry: &JournalEntry, from: &str, to: &str, precision: u32, done: &BTreeMap<i64, EntryChange>, originals: &BTreeMap<i64, JournalEntry>) -> Result<EntryChange> {
    let mut direct = BTreeMap::new();
    for p in &entry.postings {
        if let Some(e) = evidence(p, to, precision)? {
            direct.insert(p.id, e);
        }
    }
    // Provisional 1:1 old values cannot establish reliable conversion or split
    // proportions. Fully evidenced target amounts need no old conversion.
    if entry.postings.iter().any(|p| p.rate_source == "missing") && direct.len() != entry.postings.len() && entry.reverses_id.is_none() {
        return Err(invalid(format!("entry {} has an unresolved original book rate; repair it on a copy before migration", entry.id)));
    }
    // One transaction-specific value establishes the conversion for its category
    // splits. Multiple incompatible values retain their evidence and use dated FX
    // for other postings; the difference is an explicit FX adjustment.
    let anchors =
        entry.postings.iter().filter_map(|p| direct.get(&p.id).filter(|_| !p.amount.is_zero() && p.rate_source != "missing").map(|(m, _)| (m.major() / p.amount.major(), p.id))).collect::<Vec<_>>();
    let anchor = anchors.first().copied().filter(|(r, _)| entry.reverses_id.is_none() && entry.refund_of_id.is_none() && *r > Decimal::ZERO && anchors.iter().all(|(v, _)| v == r));
    let mut dated = None;
    let mut postings = Vec::new();
    for p in &entry.postings {
        let linked = entry.reverses_id.map(|id| (id, "reverses_posting")).or_else(|| entry.refund_of_id.filter(|_| p.metadata.get("refund_of_posting").is_some()).map(|id| (id, "refund_of_posting")));
        let (after, source) = if let Some((original_id, key)) = linked {
            let post_id = p.metadata[key].as_i64().ok_or_else(|| invalid(format!("entry {} lacks {key} on posting {}", entry.id, p.id)))?;
            let original = originals[&original_id].postings.iter().find(|o| o.id == post_id).ok_or_else(|| invalid(format!("entry {} has an invalid {key}", entry.id)))?;
            if p.account_id != original.account_id || p.quantity != original.quantity.checked_neg()? {
                return Err(invalid(format!("entry {} changes a linked refund/reversal quantity", entry.id)));
            }
            let value = done[&original_id].postings.iter().find(|o| o.posting_id == post_id).ok_or_else(|| invalid("missing migrated posting"))?.after.checked_neg()?;
            (value, format!("preserved book value of posting {post_id}"))
        } else if entry.refund_of_id.is_some() && p.account_type == AccountType::Expense && p.metadata.get("fx_auto").is_none() {
            return Err(invalid(format!("refund entry {} has an expense without its original posting link", entry.id)));
        } else if let Some(value) = direct.get(&p.id) {
            value.clone()
        } else {
            if p.rate_source == "missing" && anchor.is_none() {
                return Err(invalid(format!("entry {} posting {} has a missing original book rate and no target-currency evidence", entry.id, p.id)));
            }
            let (rate, source) = if let Some((rate, id)) = anchor {
                (rate, format!("transaction conversion {rate} from posting {id}"))
            } else {
                if dated.is_none() {
                    dated = Some(historical(conn, from, to, entry.date)?);
                }
                dated.clone().expect("loaded rate")
            };
            (Money::from_major(multiply(p.amount.major(), rate)?, to, precision)?, source)
        };
        if p.quantity.commodity() == to && after != p.quantity {
            return Err(invalid(format!("entry {} cannot preserve both the linked book value and native {to} quantity on posting {}", entry.id, p.id)));
        }
        if !after.is_zero() && !p.quantity.is_zero() && after.major().is_sign_negative() != p.quantity.major().is_sign_negative() {
            return Err(invalid(format!("entry {} has conflicting amount evidence on posting {}", entry.id, p.id)));
        }
        postings.push(PostingChange { posting_id: p.id, before: p.amount, after, source });
    }
    let sum = Money::from_minor(0, to, precision)?.checked_sum(postings.iter().map(|p| p.after))?;
    let mut residual = sum.checked_neg()?;
    // Keep the existing manual-split rule: the final balancing split absorbs
    // minor-unit rounding. Never use it to hide a real exchange difference.
    if entry.reverses_id.is_none() && entry.refund_of_id.is_none() && (direct.is_empty() || anchor.is_some()) && residual.minor().unsigned_abs() <= postings.len() as u64 {
        if let Some(index) = entry.postings.iter().rposition(|p| p.metadata["balance"] == true && p.quantity.commodity() != to && !direct.contains_key(&p.id)) {
            let adjusted = postings[index].after.checked_add(residual)?;
            let q = entry.postings[index].quantity.major();
            if adjusted.is_zero() || q.is_zero() || adjusted.major().is_sign_negative() == q.is_sign_negative() {
                postings[index].after = adjusted;
                postings[index].source.push_str("; final split absorbs rounding");
                residual = Money::from_minor(0, to, precision)?;
            }
        }
    }
    Ok(EntryChange { entry_id: entry.id, date: entry.date, postings, fx_adjustment: residual, reverses_entry: entry.reverses_id })
}

/// A token covers all persistent rows, including rates, evidence and settings.
/// A changed ledger requires a fresh preview, even when displayed totals agree.
fn database_digest(conn: &Connection) -> Result<String> {
    let mut hash = Sha256::new();
    let mut schema = conn.prepare("SELECT type,name,tbl_name,sql FROM sqlite_master ORDER BY type,name")?;
    for row in schema.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?)))? {
        hash.update(serde_json::to_vec(&row?)?);
    }
    let mut stmt = conn.prepare("SELECT name,sql FROM sqlite_master WHERE type='table' ORDER BY name")?;
    let tables = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))?.collect::<std::result::Result<Vec<_>, _>>()?;
    for (name, sql) in tables {
        hash.update(serde_json::to_vec(&(name.clone(), sql))?);
        let escaped = name.replace('"', "\"\"");
        let count = conn.prepare(&format!("SELECT * FROM \"{escaped}\""))?.column_count();
        let order = (1..=count).map(|i| i.to_string()).collect::<Vec<_>>().join(",");
        let mut stmt = conn.prepare(&format!("SELECT * FROM \"{escaped}\" ORDER BY {order}"))?;
        let count = stmt.column_count();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let mut values = Vec::new();
            for i in 0..count {
                use rusqlite::types::ValueRef;
                let value = match row.get_ref(i)? {
                    ValueRef::Null => ("null", String::new()),
                    ValueRef::Integer(n) => ("integer", n.to_string()),
                    ValueRef::Real(n) => ("real", n.to_bits().to_string()),
                    ValueRef::Text(t) => ("text", hex::encode(t)),
                    ValueRef::Blob(b) => ("blob", hex::encode(b)),
                };
                values.push(value);
            }
            hash.update(serde_json::to_vec(&values)?);
        }
    }
    Ok(hex::encode(hash.finalize()))
}

/// Apply only after an exact preview match and a verified, new backup file.
pub fn apply_file(path: &Path, entity_id: i64, target: &str, confirmation: &str, backup: &Path, include_locked: bool) -> Result<Preview> {
    let mut conn = open_existing(path, true)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let plan = build_preview(&tx, entity_id, target)?;
    if confirmation != plan.confirmation {
        return Err(Error::Conflict("currency migration preview is stale or the confirmation token is wrong; preview again".into()));
    }
    if plan.locked_entries != 0 && !include_locked {
        return Err(Error::Locked("migration includes locked periods; accountant must review the preview and pass --include-locked".into()));
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(backup).map_err(|e| anyhow::anyhow!("create new backup {}: {e}", backup.display()))?;
    // The immediate transaction prevents another writer from changing the source.
    let source = open_existing(path, false)?;
    source.execute("VACUUM INTO ?1", [backup.to_str().ok_or_else(|| invalid("backup path must be UTF-8"))?])?;
    let copy = open_existing(backup, false)?;
    let integrity: String = copy.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if integrity != "ok" || database_digest(&copy)? != plan.database_digest {
        return Err(invalid("backup verification failed; ledger was not changed"));
    }
    file.sync_all().map_err(|e| anyhow::anyhow!("sync backup: {e}"))?;
    #[cfg(unix)]
    {
        let parent = backup.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        std::fs::File::open(parent).and_then(|d| d.sync_all()).map_err(|e| anyhow::anyhow!("sync backup directory: {e}"))?;
    }
    apply_plan(&tx, &plan, backup)?;
    tx.commit()?;
    Ok(plan)
}

fn apply_plan(conn: &Connection, plan: &Preview, backup: &Path) -> Result<()> {
    let before_entity = entities::get_entity(conn, plan.entity_id)?;
    let before_entries = plan.entries.iter().map(|e| journal::get_entry(conn, e.entry_id)).collect::<Result<Vec<_>>>()?;
    let fx = if let Some(id) = plan.fx_account_to_replace {
        let before = accounts::get_account(conn, id)?;
        conn.execute("UPDATE accounts SET system_role='fx_gain_loss_legacy',updated_at=?2 WHERE id=?1", params![id, crate::now_ts()])?;
        audit::log(conn, "accounts", id, "currency_migration", Some(serde_json::to_value(before)?), Some(serde_json::to_value(accounts::get_account(conn, id)?)?))?;
        accounts::create_account(
            conn,
            NewAccount {
                entity_id: plan.entity_id,
                name: plan.fx_account_name.clone(),
                r#type: Some(AccountType::Expense),
                commodity: plan.to.clone(),
                system_role: "fx_gain_loss".into(),
                subtype: "expense".into(),
                in_net_worth: true,
                ..Default::default()
            },
        )?
        .id
    } else {
        accounts::find_by_role(conn, plan.entity_id, "fx_gain_loss")?.id
    };
    conn.execute("UPDATE entities SET currency=?2,updated_at=?3 WHERE id=?1", params![plan.entity_id, plan.to, crate::now_ts()])?;
    let mut added_fx = BTreeMap::new();
    for (change, before) in plan.entries.iter().zip(&before_entries) {
        for (p, old) in change.postings.iter().zip(&before.postings) {
            let rate = if old.quantity.is_zero() { None } else { Some((p.after.major() / old.quantity.major()).to_string()) };
            let mut metadata = old.metadata.clone();
            let history = metadata.as_object_mut().ok_or_else(|| invalid("posting metadata must be an object"))?.entry("currency_migrations").or_insert_with(|| serde_json::json!([]));
            history.as_array_mut().ok_or_else(|| invalid("currency_migrations metadata must be an array"))?.push(serde_json::json!({
                "confirmation": plan.confirmation, "before": p.before, "after": p.after, "source": p.source, "old_rate": old.rate, "old_rate_source": old.rate_source
            }));
            conn.execute("UPDATE postings SET amount=?2,rate=?3,rate_source='currency_migration',metadata=?4 WHERE id=?1", params![p.posting_id, p.after.minor(), rate, metadata.to_string()])?;
        }
        if !change.fx_adjustment.is_zero() {
            let mut meta = serde_json::json!({"fx_auto":true,"currency_migration":plan.confirmation});
            if let Some(id) = change.reverses_entry.and_then(|id| added_fx.get(&id)) {
                meta["reverses_posting"] = serde_json::json!(id);
            }
            conn.execute(
                "INSERT INTO postings (uid,journal_entry_id,account_id,quantity,amount,rate,rate_source,memo,metadata,position)
                VALUES (?1,?2,?3,?4,?4,'1','currency_migration','Accounting currency migration FX adjustment',?5,?6)",
                params![crate::new_uid(), change.entry_id, fx, change.fx_adjustment.minor(), meta.to_string(), before.postings.iter().map(|p| p.position).max().unwrap_or(0) + 1],
            )?;
            added_fx.insert(change.entry_id, conn.last_insert_rowid());
        }
    }
    for b in &plan.budgets {
        conn.execute("UPDATE budgets SET amount=?2,updated_at=?3 WHERE id=?1", params![b.budget_id, b.after.minor(), crate::now_ts()])?;
        audit::log(conn, "budgets", b.budget_id, "currency_migration", Some(serde_json::json!({"limit":b.before})), Some(serde_json::to_value(b)?))?;
    }
    hashchain::rechain(conn, plan.entity_id, 1)?;
    let verified = hashchain::verify(conn, plan.entity_id)?;
    if verified.first_bad_seq.is_some() {
        return Err(invalid("migrated hash chain failed verification"));
    }
    for before in before_entries {
        let after = journal::get_entry(conn, before.id)?;
        let total = after.postings.iter().try_fold(0i128, |sum, p| sum.checked_add(p.amount.minor() as i128).ok_or_else(|| invalid("amount overflow")))?;
        if total != 0 {
            return Err(invalid(format!("migrated entry {} is not balanced", before.id)));
        }
        audit::log(conn, "journal_entries", before.id, "currency_migration", Some(serde_json::to_value(&before)?), Some(serde_json::to_value(after)?))?;
    }
    audit::log(
        conn,
        "entities",
        plan.entity_id,
        "currency_migration",
        Some(serde_json::to_value(before_entity)?),
        Some(serde_json::json!({
            "entity":entities::get_entity(conn,plan.entity_id)?,"preview":plan,"new_chain_head":verified.head,"backup":backup
        })),
    )?;
    Ok(())
}
