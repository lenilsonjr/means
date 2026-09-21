//! The chart of accounts.

use std::collections::HashMap;

use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};
use rust_decimal::Decimal;

use crate::entities;
use crate::model::{Account, AccountType, NewAccount};
use crate::money::{self, MinorUnits};
use crate::{new_uid, now_ts, Error, Result};

const SELECT: &str = "SELECT a.id, a.uid, a.entity_id, a.parent_id, a.code, a.name, a.type, a.subtype, a.commodity_id, c.code, c.precision,
        a.system_role, a.placeholder, a.in_net_worth, a.credit_limit, a.statement_day, a.due_day, a.external_ids, a.notes, a.position, a.closed_at, a.class
     FROM accounts a JOIN commodities c ON c.id = a.commodity_id";

fn row_to_account(r: &rusqlite::Row<'_>) -> rusqlite::Result<Account> {
    let t: String = r.get(6)?;
    let credit: Option<String> = r.get(14)?;
    let ext: String = r.get(17)?;
    Ok(Account {
        id: r.get(0)?,
        uid: r.get(1)?,
        entity_id: r.get(2)?,
        parent_id: r.get(3)?,
        code: r.get(4)?,
        name: r.get(5)?,
        path: String::new(),
        depth: 0,
        r#type: AccountType::parse(&t).unwrap_or(AccountType::Asset),
        subtype: r.get(7)?,
        commodity_id: r.get(8)?,
        commodity: r.get(9)?,
        precision: r.get::<_, i64>(10)? as u32,
        system_role: r.get(11)?,
        placeholder: r.get::<_, i64>(12)? != 0,
        in_net_worth: r.get::<_, i64>(13)? != 0,
        credit_limit: credit.and_then(|s| money::parse(&s).ok()),
        statement_day: r.get(15)?,
        due_day: r.get(16)?,
        external_ids: serde_json::from_str(&ext).unwrap_or(serde_json::json!({})),
        notes: r.get(18)?,
        position: r.get(19)?,
        closed_at: r.get(20)?,
        class: r.get(21)?,
        balance: Decimal::ZERO,
        balance_functional: Decimal::ZERO,
        postings_count: 0,
        last_reconciled_at: None,
    })
}

/// All accounts of an entity (or all entities when `entity_id` is None), with paths, ordered as a tree.
pub fn list_accounts(conn: &Connection, entity_id: Option<i64>, include_closed: bool) -> Result<Vec<Account>> {
    let sql = format!("{SELECT} WHERE (?1 IS NULL OR a.entity_id = ?1) AND (?2 = 1 OR a.closed_at IS NULL) ORDER BY a.entity_id, a.type, a.position, a.name");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![entity_id, include_closed as i64], row_to_account)?;
    let mut accounts: Vec<Account> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    // Parents may be closed while children are listed; fetch all for path building.
    let all = if include_closed {
        accounts.clone()
    } else {
        let sql = format!("{SELECT} WHERE (?1 IS NULL OR a.entity_id = ?1)");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![entity_id], row_to_account)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let by_id: HashMap<i64, (Option<i64>, String, AccountType)> = all.iter().map(|a| (a.id, (a.parent_id, a.name.clone(), a.r#type))).collect();
    for a in accounts.iter_mut() {
        let (path, depth) = build_path(&by_id, a.id);
        a.path = path;
        a.depth = depth;
    }
    accounts.sort_by(|x, y| (x.entity_id, type_order(x.r#type), &x.path).cmp(&(y.entity_id, type_order(y.r#type), &y.path)));
    Ok(accounts)
}

fn type_order(t: AccountType) -> u8 {
    match t {
        AccountType::Asset => 0,
        AccountType::Liability => 1,
        AccountType::Equity => 2,
        AccountType::Income => 3,
        AccountType::Expense => 4,
    }
}

fn build_path(by_id: &HashMap<i64, (Option<i64>, String, AccountType)>, id: i64) -> (String, i32) {
    let mut names = Vec::new();
    let mut cur = Some(id);
    let mut root = AccountType::Asset;
    let mut guard = 0;
    while let Some(i) = cur {
        guard += 1;
        if guard > 64 {
            break;
        }
        match by_id.get(&i) {
            Some((parent, name, t)) => {
                names.push(name.clone());
                root = *t;
                cur = *parent;
            }
            None => break,
        }
    }
    names.reverse();
    let depth = names.len() as i32 - 1;
    let mut path = root.root_name().to_string();
    for n in names {
        path.push(':');
        path.push_str(&n);
    }
    (path, depth)
}

pub fn get_account(conn: &Connection, id: i64) -> Result<Account> {
    let sql = format!("{SELECT} WHERE a.id = ?1");
    let mut acc = conn.query_row(&sql, [id], row_to_account).optional()?.ok_or_else(|| Error::NotFound(format!("account {id}")))?;
    // Walk the parent chain for the path (a handful of rows, no full index).
    let mut stmt = conn.prepare_cached(
        "WITH RECURSIVE chain(id, parent_id, name, depth) AS (
            SELECT id, parent_id, name, 0 FROM accounts WHERE id = ?1
            UNION ALL SELECT a.id, a.parent_id, a.name, chain.depth + 1 FROM accounts a JOIN chain ON a.id = chain.parent_id WHERE chain.depth < 64)
         SELECT name FROM chain ORDER BY depth DESC",
    )?;
    let names: Vec<String> = stmt.query_map([id], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
    let mut path = acc.r#type.root_name().to_string();
    for n in &names {
        path.push(':');
        path.push_str(n);
    }
    acc.path = path;
    acc.depth = names.len() as i32 - 1;
    Ok(acc)
}

/// A lean view of an account for path lookups and report rows.
#[derive(Debug, Clone)]
pub struct AccountBrief {
    pub id: i64,
    pub entity_id: i64,
    pub parent_id: Option<i64>,
    pub name: String,
    pub path: String,
    pub depth: i32,
    pub r#type: AccountType,
    pub subtype: String,
    pub commodity: String,
    pub precision: u32,
    pub system_role: String,
    pub placeholder: bool,
    pub in_net_worth: bool,
    pub closed: bool,
}

/// Every account with its path, cheap enough to call per operation.
pub fn path_index(conn: &Connection) -> Result<HashMap<i64, AccountBrief>> {
    let mut stmt = conn.prepare(
        "SELECT a.id, a.entity_id, a.parent_id, a.name, a.type, a.subtype, c.code, c.precision, a.system_role, a.placeholder, a.in_net_worth, a.closed_at IS NOT NULL
         FROM accounts a JOIN commodities c ON c.id = a.commodity_id",
    )?;
    let rows = stmt.query_map([], |r| {
        let t: String = r.get(4)?;
        Ok(AccountBrief {
            id: r.get(0)?,
            entity_id: r.get(1)?,
            parent_id: r.get(2)?,
            name: r.get(3)?,
            path: String::new(),
            depth: 0,
            r#type: AccountType::parse(&t).unwrap_or(AccountType::Asset),
            subtype: r.get(5)?,
            commodity: r.get(6)?,
            precision: r.get::<_, i64>(7)? as u32,
            system_role: r.get(8)?,
            placeholder: r.get::<_, i64>(9)? != 0,
            in_net_worth: r.get::<_, i64>(10)? != 0,
            closed: r.get::<_, i64>(11)? != 0,
        })
    })?;
    let mut map: HashMap<i64, AccountBrief> = HashMap::new();
    for row in rows {
        let b = row?;
        map.insert(b.id, b);
    }
    let by_id: HashMap<i64, (Option<i64>, String, AccountType)> = map.values().map(|a| (a.id, (a.parent_id, a.name.clone(), a.r#type))).collect();
    for b in map.values_mut() {
        let (path, depth) = build_path(&by_id, b.id);
        b.path = path;
        b.depth = depth;
    }
    Ok(map)
}

pub fn find_by_role(conn: &Connection, entity_id: i64, role: &str) -> Result<Account> {
    let id: Option<i64> = conn.query_row("SELECT id FROM accounts WHERE entity_id = ?1 AND system_role = ?2", params![entity_id, role], |r| r.get(0)).optional()?;
    match id {
        Some(id) => get_account(conn, id),
        None => Err(Error::NotFound(format!("system account {role} for entity {entity_id}"))),
    }
}

/// Find an account by its path within an entity, e.g. "Expenses:Food" (root name optional).
pub fn find_by_path(conn: &Connection, entity_id: i64, path: &str) -> Result<Option<Account>> {
    let wanted = path.trim();
    let all = list_accounts(conn, Some(entity_id), true)?;
    Ok(all.into_iter().find(|a| a.path == wanted || a.path.split_once(':').map(|(_, rest)| rest == wanted).unwrap_or(false)))
}

/// Find or create an account under a parent by name. Creates intermediate parents as placeholders.
pub fn ensure_account(conn: &Connection, entity_id: i64, t: AccountType, names: &[&str], subtype: &str, commodity: &str) -> Result<Account> {
    let mut parent: Option<i64> = None;
    let mut last: Option<i64> = None;
    for (i, name) in names.iter().enumerate() {
        let is_last = i == names.len() - 1;
        // Case-insensitive: "health:fitness:Boxing" must land under an existing "Health:Fitness",
        // not grow a lowercase twin next to it.
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM accounts WHERE entity_id = ?1 AND COALESCE(parent_id, 0) = COALESCE(?2, 0) AND type = ?4 AND name = ?3 COLLATE NOCASE ORDER BY name = ?3 DESC LIMIT 1",
                params![entity_id, parent, name, t.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        let id = match existing {
            Some(id) => id,
            None => {
                let created = create_account(
                    conn,
                    NewAccount {
                        entity_id,
                        parent_id: parent,
                        name: name.to_string(),
                        r#type: Some(t),
                        subtype: if is_last { subtype.to_string() } else { "placeholder".to_string() },
                        commodity: commodity.to_string(),
                        placeholder: !is_last,
                        in_net_worth: true,
                        ..Default::default()
                    },
                )?;
                created.id
            }
        };
        parent = Some(id);
        last = Some(id);
    }
    get_account(conn, last.ok_or_else(|| Error::Invalid("account name is required".into()))?)
}

pub fn create_account(conn: &Connection, input: NewAccount) -> Result<Account> {
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err(Error::Invalid("account name is required".into()));
    }
    if name.contains(':') {
        return Err(Error::Invalid("account names cannot contain ':'".into()));
    }
    let entity = entities::get_entity(conn, input.entity_id)?;
    let (t, parent_commodity) = match input.parent_id {
        Some(pid) => {
            let parent = get_account(conn, pid)?;
            if parent.entity_id != input.entity_id {
                return Err(Error::Invalid("parent account belongs to another entity".into()));
            }
            if let Some(t) = input.r#type {
                if t != parent.r#type {
                    return Err(Error::Invalid(format!("account type {} does not match parent type {}", t.as_str(), parent.r#type.as_str())));
                }
            }
            (parent.r#type, Some(parent.commodity))
        }
        None => (input.r#type.ok_or_else(|| Error::Invalid("account type is required".into()))?, None),
    };
    let commodity_code = if input.commodity.trim().is_empty() {
        match (&parent_commodity, t) {
            (_, AccountType::Income | AccountType::Expense | AccountType::Equity) => entity.currency.clone(),
            (Some(c), _) => c.clone(),
            (None, _) => entity.currency.clone(),
        }
    } else {
        input.commodity.trim().to_ascii_uppercase()
    };
    let commodity_id = match entities::get_commodity_by_code(conn, &commodity_code) {
        Ok(c) => c.id,
        Err(Error::NotFound(_)) => entities::ensure_currency(conn, &commodity_code)?,
        Err(e) => return Err(e),
    };
    let subtype = if !input.subtype.trim().is_empty() {
        input.subtype.trim().to_ascii_lowercase()
    } else if input.placeholder {
        "placeholder".to_string()
    } else {
        match t {
            AccountType::Asset => "bank",
            AccountType::Liability => "card",
            AccountType::Equity => "equity",
            AccountType::Income => "income",
            AccountType::Expense => "expense",
        }
        .to_string()
    };
    let ts = now_ts();
    conn.execute(
        "INSERT INTO accounts (uid, entity_id, parent_id, code, name, type, subtype, commodity_id, system_role, placeholder, in_net_worth,
            credit_limit, statement_day, due_day, external_ids, notes, position, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?18)",
        params![
            new_uid(),
            input.entity_id,
            input.parent_id,
            input.code.trim(),
            name,
            t.as_str(),
            subtype,
            commodity_id,
            input.system_role,
            input.placeholder as i64,
            input.in_net_worth as i64,
            input.credit_limit.map(money::plain),
            input.statement_day,
            input.due_day,
            input.external_ids.map(|v| v.to_string()).unwrap_or_else(|| "{}".into()),
            input.notes.trim(),
            input.position,
            ts
        ],
    )
    .map_err(|e| match e {
        rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => Error::Conflict(format!("an account named {name:?} already exists under this parent")),
        other => Error::Db(other),
    })?;
    let id = conn.last_insert_rowid();
    crate::audit::log(conn, "accounts", id, "create", None, Some(serde_json::json!({"name": name, "type": t.as_str(), "commodity": commodity_code})))?;
    get_account(conn, id)
}

#[derive(Debug, Clone, Default)]
pub struct AccountUpdate {
    pub parent_id: Option<Option<i64>>,
    pub code: Option<String>,
    pub name: Option<String>,
    pub subtype: Option<String>,
    pub in_net_worth: Option<bool>,
    pub credit_limit: Option<Option<Decimal>>,
    pub statement_day: Option<Option<i32>>,
    pub due_day: Option<Option<i32>>,
    pub notes: Option<String>,
    pub position: Option<i32>,
    pub external_ids: Option<serde_json::Value>,
    pub placeholder: Option<bool>,
    pub commodity: Option<String>,
    pub class: Option<String>,
}

pub fn update_account(conn: &Connection, id: i64, upd: AccountUpdate) -> Result<Account> {
    let before = get_account(conn, id)?;
    let mut a = before.clone();
    if let Some(p) = upd.parent_id {
        if let Some(pid) = p {
            if pid == id {
                return Err(Error::Invalid("an account cannot be its own parent".into()));
            }
            let parent = get_account(conn, pid)?;
            if parent.entity_id != a.entity_id {
                return Err(Error::Invalid("parent account belongs to another entity".into()));
            }
            if parent.r#type != a.r#type {
                return Err(Error::Invalid("parent must have the same account type".into()));
            }
            if parent.path.starts_with(&format!("{}:", a.path)) {
                return Err(Error::Invalid("cannot move an account under its own descendant".into()));
            }
        }
        a.parent_id = p;
    }
    if let Some(v) = upd.code {
        a.code = v.trim().to_string();
    }
    if let Some(v) = upd.name {
        if v.trim().is_empty() || v.contains(':') {
            return Err(Error::Invalid("invalid account name".into()));
        }
        a.name = v.trim().to_string();
    }
    if let Some(v) = upd.subtype {
        a.subtype = v.trim().to_ascii_lowercase();
    }
    if let Some(v) = upd.in_net_worth {
        a.in_net_worth = v;
    }
    if let Some(v) = upd.credit_limit {
        a.credit_limit = v;
    }
    if let Some(v) = upd.statement_day {
        a.statement_day = v;
    }
    if let Some(v) = upd.due_day {
        a.due_day = v;
    }
    if let Some(v) = upd.notes {
        a.notes = v;
    }
    if let Some(v) = upd.position {
        a.position = v;
    }
    if let Some(v) = upd.external_ids {
        a.external_ids = v;
    }
    if let Some(v) = upd.class {
        let v = v.trim().to_ascii_lowercase();
        if !matches!(v.as_str(), "" | "fixed" | "committed" | "discretionary" | "savings" | "not-spending") {
            return Err(Error::Invalid(format!("unknown class {v:?}; one of fixed, committed, discretionary, savings, not-spending, or empty")));
        }
        if !v.is_empty() && a.r#type != AccountType::Expense {
            return Err(Error::Invalid("class describes expense accounts only".into()));
        }
        a.class = v;
    }
    if let Some(v) = upd.placeholder {
        if v && before.postings_count_db(conn)? > 0 {
            return Err(Error::Invalid("an account with postings cannot become a placeholder".into()));
        }
        a.placeholder = v;
    }
    if let Some(c) = upd.commodity {
        let c = c.trim().to_ascii_uppercase();
        if c != a.commodity {
            if before.postings_count_db(conn)? > 0 {
                return Err(Error::Invalid("the commodity of an account with postings cannot change".into()));
            }
            a.commodity_id = entities::ensure_currency(conn, &c)?;
            a.commodity = c;
        }
    }
    conn.execute(
        "UPDATE accounts SET parent_id = ?2, code = ?3, name = ?4, subtype = ?5, in_net_worth = ?6, credit_limit = ?7, statement_day = ?8,
            due_day = ?9, notes = ?10, position = ?11, external_ids = ?12, placeholder = ?13, commodity_id = ?14, updated_at = ?15, class = ?16 WHERE id = ?1",
        params![
            id,
            a.parent_id,
            a.code,
            a.name,
            a.subtype,
            a.in_net_worth as i64,
            a.credit_limit.map(money::plain),
            a.statement_day,
            a.due_day,
            a.notes,
            a.position,
            a.external_ids.to_string(),
            a.placeholder as i64,
            a.commodity_id,
            now_ts(),
            a.class
        ],
    )
    .map_err(|e| match e {
        rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => Error::Conflict("an account with that name already exists under this parent".into()),
        other => Error::Db(other),
    })?;
    let after = get_account(conn, id)?;
    crate::audit::log(conn, "accounts", id, "update", Some(serde_json::to_value(&before)?), Some(serde_json::to_value(&after)?))?;
    Ok(after)
}

impl Account {
    fn postings_count_db(&self, conn: &Connection) -> Result<i64> {
        Ok(conn.query_row("SELECT COUNT(*) FROM postings WHERE account_id = ?1", [self.id], |r| r.get(0))?)
    }
}

pub fn close_account(conn: &Connection, id: i64, reopen: bool) -> Result<Account> {
    let before = get_account(conn, id)?;
    if reopen {
        conn.execute("UPDATE accounts SET closed_at = NULL, updated_at = ?2 WHERE id = ?1", params![id, now_ts()])?;
    } else {
        if !before.system_role.is_empty() {
            return Err(Error::Invalid("system accounts cannot be closed".into()));
        }
        conn.execute("UPDATE accounts SET closed_at = ?2, updated_at = ?2 WHERE id = ?1", params![id, now_ts()])?;
    }
    let after = get_account(conn, id)?;
    crate::audit::log(conn, "accounts", id, if reopen { "reopen" } else { "close" }, None, None)?;
    Ok(after)
}

/// The precision of an account's commodity: the decimals of every `quantity` on it (D14).
pub fn commodity_precision(conn: &Connection, account_id: i64) -> Result<u32> {
    conn.query_row("SELECT c.precision FROM accounts a JOIN commodities c ON c.id = a.commodity_id WHERE a.id = ?1", [account_id], |r| r.get::<_, i64>(0))
        .optional()?
        .map(|p| p as u32)
        .ok_or_else(|| Error::NotFound(format!("account {account_id}")))
}

/// Balance of every account: posted entries only, dated on or before `as_of`.
/// Returns (quantity, amount) sums in storage sign (debit positive), keyed by account id.
/// Each sum is in one commodity: the account's for `quantity`, its entity's functional currency
/// for `amount` (D14).
pub fn raw_balances(conn: &Connection, entity_id: Option<i64>, as_of: Option<NaiveDate>) -> Result<HashMap<i64, (Decimal, Decimal, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT p.account_id, COALESCE(SUM(p.quantity), 0), COALESCE(SUM(p.amount), 0), COUNT(*), ac.precision, fc.precision
         FROM postings p JOIN journal_entries e ON e.id = p.journal_entry_id
              JOIN accounts a ON a.id = p.account_id
              JOIN commodities ac ON ac.id = a.commodity_id
              JOIN entities en ON en.id = a.entity_id
              JOIN commodities fc ON fc.code = en.currency
         WHERE e.status IN ('posted','void') AND (?1 IS NULL OR e.entity_id = ?1) AND (?2 IS NULL OR e.date <= ?2)
         GROUP BY p.account_id",
    )?;
    let rows = stmt.query_map(params![entity_id, as_of.map(|d| d.to_string())], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)? as u32, r.get::<_, i64>(5)? as u32))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (id, q, a, n, qprec, aprec) = row?;
        out.insert(id, (MinorUnits::from_minor(q, qprec)?.major(), MinorUnits::from_minor(a, aprec)?.major(), n));
    }
    Ok(out)
}

/// Accounts with balances filled in (normal-balance sign, placeholders rolled up from descendants).
pub fn list_accounts_with_balances(conn: &Connection, entity_id: Option<i64>, include_closed: bool, as_of: Option<NaiveDate>) -> Result<Vec<Account>> {
    let mut accounts = list_accounts(conn, entity_id, include_closed)?;
    let balances = raw_balances(conn, entity_id, as_of)?;
    let reconciled: HashMap<i64, String> = {
        let mut stmt = conn.prepare("SELECT account_id, MAX(reconciled_at) FROM postings WHERE reconciled_at IS NOT NULL GROUP BY account_id")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        rows.collect::<std::result::Result<HashMap<_, _>, _>>()?
    };
    for a in accounts.iter_mut() {
        if let Some((q, amt, n)) = balances.get(&a.id) {
            let sign = Decimal::from(a.r#type.normal_sign());
            a.balance = *q * sign;
            a.balance_functional = *amt * sign;
            a.postings_count = *n;
        }
        a.last_reconciled_at = reconciled.get(&a.id).cloned();
    }
    // Roll up: children add to every ancestor (functional amounts always; quantities only when the commodity matches).
    let index: HashMap<i64, usize> = accounts.iter().enumerate().map(|(i, a)| (a.id, i)).collect();
    let snapshot: Vec<(Option<i64>, Decimal, Decimal, String, i64)> = accounts.iter().map(|a| (a.parent_id, a.balance, a.balance_functional, a.commodity.clone(), a.postings_count)).collect();
    for (i, (parent, q, amt, commodity, n)) in snapshot.iter().enumerate() {
        let mut cur = *parent;
        let _ = i;
        while let Some(pid) = cur {
            let Some(&pi) = index.get(&pid) else { break };
            accounts[pi].balance_functional += *amt;
            accounts[pi].postings_count += *n;
            if accounts[pi].commodity == *commodity {
                accounts[pi].balance += *q;
            }
            cur = accounts[pi].parent_id;
        }
    }
    Ok(accounts)
}

/// What a merge did.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MergeReport {
    pub moved_postings: i64,
    pub moved_rules: i64,
    pub moved_template_lines: i64,
    pub source_deleted: bool,
}

/// Rules for merging `source` into `target`; returns the source's number of children.
fn check_merge(conn: &Connection, source: &Account, target: &Account) -> Result<i64> {
    if source.id == target.id {
        return Err(Error::Invalid("an account cannot be merged into itself".into()));
    }
    if source.entity_id != target.entity_id {
        return Err(Error::Invalid("accounts belong to different entities".into()));
    }
    if target.placeholder {
        return Err(Error::Invalid(format!("{} is a placeholder; merge into an account that takes postings", target.path)));
    }
    if target.is_closed() {
        return Err(Error::Invalid(format!("{} is closed", target.path)));
    }
    if !source.system_role.is_empty() {
        return Err(Error::Invalid(format!("{} is a system account and cannot be merged away", source.path)));
    }
    let categories = |t: AccountType| matches!(t, AccountType::Income | AccountType::Expense);
    if source.r#type != target.r#type && !(categories(source.r#type) && categories(target.r#type)) {
        return Err(Error::Invalid(format!("cannot merge a {} account into a {} account", source.r#type.as_str(), target.r#type.as_str())));
    }
    if source.commodity != target.commodity && !(categories(source.r#type) && categories(target.r#type)) {
        return Err(Error::Invalid("accounts with different commodities cannot be merged".into()));
    }
    // Merging a parent into one of its own children is allowed: the parent keeps its children and becomes a placeholder.
    Ok(conn.query_row("SELECT COUNT(*) FROM accounts WHERE parent_id = ?1", [source.id], |r| r.get(0))?)
}

/// Move every posting, rule, template line and statement line from `source` to `target` inside an open
/// transaction, then remove `source` (or keep it as an empty placeholder when it has children).
/// Returns the report and the lowest posted sequence touched, for the caller to rechain.
fn merge_in(tx: &Connection, source: &Account, target: &Account, children: i64) -> Result<(MergeReport, Option<i64>)> {
    let min_seq: Option<i64> = tx
        .query_row("SELECT MIN(e.seq) FROM postings p JOIN journal_entries e ON e.id = p.journal_entry_id WHERE p.account_id = ?1 AND e.seq IS NOT NULL", [source.id], |r| r.get(0))
        .optional()?
        .flatten();
    let moved_postings = tx.execute("UPDATE postings SET account_id = ?2 WHERE account_id = ?1", params![source.id, target.id])? as i64;
    if moved_postings > 0 {
        tx.execute("UPDATE journal_entries SET updated_at = ?2 WHERE id IN (SELECT journal_entry_id FROM postings WHERE account_id = ?1)", params![target.id, now_ts()])?;
    }
    let moved_rules = tx.execute("UPDATE rules SET account_id = ?2 WHERE account_id = ?1", params![source.id, target.id])? as i64;
    // Template lines are JSON; rewrite the ones that point at the source.
    let mut moved_template_lines = 0i64;
    {
        let mut stmt = tx.prepare("SELECT id, lines FROM entry_templates WHERE lines LIKE '%\"account_id\":' || ?1 || '%' OR lines LIKE '%\"account_id\": ' || ?1 || '%'")?;
        let rows: Vec<(i64, String)> = stmt.query_map([source.id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<std::result::Result<_, _>>()?;
        drop(stmt);
        for (tid, lines) in rows {
            if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&lines) {
                if let Some(arr) = v.as_array_mut() {
                    for l in arr.iter_mut() {
                        if l.get("account_id").and_then(|x| x.as_i64()) == Some(source.id) {
                            l["account_id"] = serde_json::json!(target.id);
                            moved_template_lines += 1;
                        }
                    }
                }
                tx.execute("UPDATE entry_templates SET lines = ?2, version = version + 1, updated_at = ?3 WHERE id = ?1", params![tid, v.to_string(), now_ts()])?;
            }
        }
    }
    tx.execute("UPDATE statement_lines SET account_id = ?2 WHERE account_id = ?1", params![source.id, target.id])?;
    tx.execute("UPDATE import_profiles SET account_id = ?2 WHERE account_id = ?1", params![source.id, target.id])?;
    tx.execute("UPDATE channel_connections SET account_id = ?2 WHERE account_id = ?1", params![source.id, target.id])?;
    tx.execute("UPDATE imports SET account_id = ?2 WHERE account_id = ?1", params![source.id, target.id])?;
    if children == 0 {
        let mut stmt = tx.prepare("SELECT id FROM budgets WHERE account_id=?1")?;
        let ids = stmt.query_map([source.id], |r| r.get::<_, i64>(0))?.collect::<std::result::Result<Vec<_>, _>>()?;
        if !ids.is_empty() && target.r#type != AccountType::Expense {
            return Err(Error::Invalid("move or delete category budgets before merging into an income account".into()));
        }
        for id in ids {
            let before = crate::budgets::get(tx, id)?;
            tx.execute("UPDATE budgets SET account_id=?2,updated_at=?3 WHERE id=?1", params![id, target.id, now_ts()])?;
            let after = crate::budgets::get(tx, id)?;
            crate::audit::log(tx, "budgets", id, "merge_account", Some(serde_json::to_value(before)?), Some(serde_json::to_value(after)?))?;
        }
    }
    let source_deleted = if children == 0 {
        tx.execute("DELETE FROM accounts WHERE id = ?1", [source.id])?;
        true
    } else {
        tx.execute("UPDATE accounts SET placeholder = 1, updated_at = ?2 WHERE id = ?1", params![source.id, now_ts()])?;
        false
    };
    crate::audit::log(
        tx,
        "accounts",
        source.id,
        "merge",
        Some(serde_json::json!({"path": source.path})),
        Some(serde_json::json!({"into": target.path, "target_id": target.id, "moved_postings": moved_postings})),
    )?;
    Ok((MergeReport { moved_postings, moved_rules, moved_template_lines, source_deleted }, min_seq))
}

/// Move every posting (and rule, and template line) from `source` to `target`, then remove `source`.
/// Both must belong to the same entity and be categories (income/expense) or of the same type.
pub fn merge_accounts(conn: &mut Connection, source_id: i64, target_id: i64) -> Result<MergeReport> {
    let source = get_account(conn, source_id)?;
    let target = get_account(conn, target_id)?;
    let children = check_merge(conn, &source, &target)?;
    let tx = conn.transaction()?;
    let (report, min_seq) = merge_in(&tx, &source, &target, children)?;
    if let Some(seq) = min_seq {
        crate::hashchain::rechain(&tx, source.entity_id, seq)?;
    }
    tx.commit()?;
    Ok(report)
}

#[derive(Debug, Clone, Default)]
pub struct RecatReport {
    pub entries: usize,
    pub postings: usize,
}

/// Move every posting of `from` whose entry's payee matches into `to`, optionally tagging the moved
/// entries, in one transaction with one hash-chain recompute. `payee`: Some("x") matches exactly
/// (case-insensitive), Some("") matches blank payees only, None matches every posting.
pub fn recategorize(conn: &mut Connection, from_id: i64, to_id: i64, payee: Option<&str>, tag: &str) -> Result<RecatReport> {
    recategorize_filtered(conn, from_id, to_id, RecatFilter::Payee(payee), tag, false)
}

/// Split a category by literal, case-insensitive text in payee, description or posting memo.
/// A preview validates the same set of postings but writes nothing.
pub fn split_category(conn: &mut Connection, from_id: i64, to_id: i64, query: &str, preview: bool) -> Result<RecatReport> {
    if query.trim().is_empty() {
        return Err(Error::Invalid("enter a text filter before splitting a category".into()));
    }
    for id in [from_id, to_id] {
        if !matches!(get_account(conn, id)?.r#type, AccountType::Income | AccountType::Expense) {
            return Err(Error::Invalid("category splitting requires income or expense accounts".into()));
        }
    }
    recategorize_filtered(conn, from_id, to_id, RecatFilter::Text(query.trim()), "", preview)
}

enum RecatFilter<'a> {
    Payee(Option<&'a str>),
    Text(&'a str),
}

fn recategorize_filtered(conn: &mut Connection, from_id: i64, to_id: i64, filter: RecatFilter<'_>, tag: &str, preview: bool) -> Result<RecatReport> {
    let tx = conn.transaction()?;
    let conn = &tx;

    if from_id == to_id {
        return Err(Error::Invalid("the source and the target are the same account".into()));
    }
    let from = get_account(conn, from_id)?;
    let to = get_account(conn, to_id)?;
    if from.entity_id != to.entity_id {
        return Err(Error::Invalid("accounts belong to different entities".into()));
    }
    if to.placeholder {
        return Err(Error::Invalid(format!("{} is a placeholder; postings go to a leaf", to.path)));
    }
    if to.is_closed() {
        return Err(Error::Invalid(format!("{} is closed", to.path)));
    }
    let categories = |t: AccountType| matches!(t, AccountType::Income | AccountType::Expense);
    if from.r#type != to.r#type && !(categories(from.r#type) && categories(to.r#type)) {
        return Err(Error::Invalid(format!("cannot move postings from a {} account into a {} account", from.r#type.as_str(), to.r#type.as_str())));
    }
    if from.commodity != to.commodity {
        return Err(Error::Invalid("accounts with different commodities".into()));
    }
    let tags = crate::tags::parse(tag)?;
    let mut sql = String::from("SELECT p.id, e.id, e.seq FROM postings p JOIN journal_entries e ON e.id = p.journal_entry_id WHERE p.account_id = ?1 AND e.status <> 'void'");
    let value = match &filter {
        RecatFilter::Payee(Some("")) => {
            sql.push_str(" AND TRIM(e.payee) = ''");
            None
        }
        RecatFilter::Payee(Some(p)) => {
            sql.push_str(" AND LOWER(TRIM(e.payee)) = LOWER(TRIM(?2))");
            Some(*p)
        }
        RecatFilter::Payee(None) => None,
        RecatFilter::Text(q) => {
            sql.push_str(" AND (instr(LOWER(e.payee), LOWER(?2)) > 0 OR instr(LOWER(e.description), LOWER(?2)) > 0 OR EXISTS (SELECT 1 FROM postings m WHERE m.journal_entry_id = e.id AND m.account_id = p.account_id AND instr(LOWER(m.memo), LOWER(?2)) > 0))");
            Some(*q)
        }
    };
    let rows: Vec<(i64, i64, Option<i64>)> = {
        let mut stmt = conn.prepare(&sql)?;
        let map = |r: &rusqlite::Row<'_>| Ok((r.get(0)?, r.get(1)?, r.get(2)?));
        let out = match value {
            Some(value) => stmt.query_map(params![from_id, value], map)?.collect::<std::result::Result<_, _>>()?,
            None => stmt.query_map(params![from_id], map)?.collect::<std::result::Result<_, _>>()?,
        };
        out
    };
    if rows.is_empty() {
        return Ok(RecatReport::default());
    }
    let min_seq = rows.iter().filter_map(|(_, _, s)| *s).min();
    let posting_ids: Vec<i64> = rows.iter().map(|(p, _, _)| *p).collect();
    let mut entry_ids: Vec<i64> = rows.iter().map(|(_, e, _)| *e).collect();
    entry_ids.sort_unstable();
    entry_ids.dedup();
    for id in &entry_ids {
        let entry = crate::journal::get_entry(conn, *id)?;
        crate::journal::check_mutation_lock(conn, &entry)?;
        if entry.postings.iter().any(|p| p.account_id == from_id && p.reconciled_at.is_some()) {
            return Err(Error::Locked(format!("entry #{id} has reconciled postings on the source account")));
        }
    }
    if preview {
        return Ok(RecatReport { entries: entry_ids.len(), postings: posting_ids.len() });
    }
    for chunk in posting_ids.chunks(500) {
        let marks = vec!["?"; chunk.len()].join(",");
        let mut p: Vec<&dyn rusqlite::ToSql> = vec![&to_id];
        p.extend(chunk.iter().map(|x| x as &dyn rusqlite::ToSql));
        tx.execute(&format!("UPDATE postings SET account_id = ?1 WHERE id IN ({marks})"), &p[..])?;
    }
    let ts = now_ts();
    for chunk in entry_ids.chunks(500) {
        let marks = vec!["?"; chunk.len()].join(",");
        let mut p: Vec<&dyn rusqlite::ToSql> = vec![&ts];
        p.extend(chunk.iter().map(|x| x as &dyn rusqlite::ToSql));
        tx.execute(&format!("UPDATE journal_entries SET updated_at = ?1 WHERE id IN ({marks})"), &p[..])?;
    }
    for eid in &entry_ids {
        for (k, v) in &tags {
            tx.execute("INSERT OR IGNORE INTO entry_tags (entry_id, key, value) VALUES (?1, ?2, ?3)", params![eid, k, v])?;
        }
    }
    if let Some(seq) = min_seq {
        crate::hashchain::rechain(&tx, from.entity_id, seq)?;
    }
    crate::audit::log(
        &tx,
        "accounts",
        from_id,
        "recat",
        Some(serde_json::json!({"path": from.path, "filter": value, "filter_kind": match filter { RecatFilter::Payee(_) => "payee", RecatFilter::Text(_) => "text" }})),
        Some(serde_json::json!({"into": to.path, "entries": entry_ids.len(), "postings": posting_ids.len(), "tag": tag})),
    )?;
    tx.commit()?;
    Ok(RecatReport { entries: entry_ids.len(), postings: posting_ids.len() })
}

/// Add tags to every entry that touches `account`. Tags live outside the chain: no rechain.
pub fn tag_account(conn: &mut Connection, account_id: i64, tag: &str) -> Result<usize> {
    let account = get_account(conn, account_id)?;
    let tags = crate::tags::parse(tag)?;
    if tags.is_empty() {
        return Err(Error::Invalid("give at least one tag".into()));
    }
    let entry_ids: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT DISTINCT e.id FROM postings p JOIN journal_entries e ON e.id = p.journal_entry_id WHERE p.account_id = ?1 AND e.status <> 'void'")?;
        let out: Vec<i64> = stmt.query_map([account_id], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
        out
    };
    let tx = conn.transaction()?;
    for eid in &entry_ids {
        for (k, v) in &tags {
            tx.execute("INSERT OR IGNORE INTO entry_tags (entry_id, key, value) VALUES (?1, ?2, ?3)", params![eid, k, v])?;
        }
    }
    crate::audit::log(&tx, "accounts", account_id, "tag_all", None, Some(serde_json::json!({"path": account.path, "tag": tag, "entries": entry_ids.len()})))?;
    tx.commit()?;
    Ok(entry_ids.len())
}

/// One node of a chart template.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChartNode {
    pub code: String,
    pub path: String,
    pub r#type: String,
    #[serde(default)]
    pub placeholder: bool,
    #[serde(default)]
    pub description: String,
}

/// Create the accounts of a chart in an entity: existing paths are kept (their code is set when empty), missing ones created.
/// Paths are "Income:..." / "Expenses:..." with the root name dropped, or bare names under the type root.
pub fn apply_chart(conn: &mut Connection, entity_id: i64, nodes: &[ChartNode]) -> Result<(usize, usize)> {
    let tx = conn.transaction()?;
    let r = apply_chart_in(&tx, entity_id, nodes)?;
    tx.commit()?;
    Ok(r)
}

fn apply_chart_in(conn: &Connection, entity_id: i64, nodes: &[ChartNode]) -> Result<(usize, usize)> {
    let entity = entities::get_entity(conn, entity_id)?;
    let mut created = 0usize;
    let mut existing = 0usize;
    let mut sorted: Vec<&ChartNode> = nodes.iter().collect();
    sorted.sort_by_key(|n| n.path.matches(':').count());
    for n in sorted {
        let t = AccountType::parse(&n.r#type)?;
        let mut segments: Vec<&str> = n.path.split(':').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        if let Some(first) = segments.first() {
            if first.eq_ignore_ascii_case(t.root_name()) {
                segments.remove(0);
            }
        }
        if segments.is_empty() {
            continue;
        }
        let before: Option<i64> = find_by_path(conn, entity_id, &segments.join(":"))?.filter(|a| a.r#type == t).map(|a| a.id);
        let leaf_subtype = match t {
            AccountType::Income => "income",
            AccountType::Expense => "expense",
            AccountType::Asset => "bank",
            AccountType::Liability => "card",
            AccountType::Equity => "equity",
        };
        let acc = ensure_account(conn, entity_id, t, &segments, if n.placeholder { "placeholder" } else { leaf_subtype }, &entity.currency)?;
        if before.is_some() {
            existing += 1;
        } else {
            created += 1;
        }
        let mut upd = AccountUpdate::default();
        if !n.code.trim().is_empty() && acc.code.trim().is_empty() {
            upd.code = Some(n.code.trim().to_string());
        }
        // An existing account with postings stays a normal account even when the chart wants a placeholder.
        if n.placeholder && !acc.placeholder && acc.postings_count_db(conn)? == 0 {
            upd.placeholder = Some(true);
        }
        if !n.description.trim().is_empty() && acc.notes.trim().is_empty() {
            upd.notes = Some(n.description.trim().to_string());
        }
        if upd.code.is_some() || upd.placeholder.is_some() || upd.notes.is_some() {
            update_account(conn, acc.id, upd)?;
        }
    }
    Ok((created, existing))
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RemapReport {
    pub created: usize,
    pub existing: usize,
    pub merged: usize,
    pub moved_postings: i64,
    pub skipped: Vec<String>,
}

/// Create a target chart and merge old accounts into it in one transaction: `moves` pairs a source account id
/// with a target path in the chart. Moves that cannot apply are reported in `skipped`, the rest go through;
/// the hash chain is recomputed once at the end.
pub fn remap_accounts(conn: &mut Connection, entity_id: i64, chart: &[ChartNode], moves: &[(i64, String)]) -> Result<RemapReport> {
    let tx = conn.transaction()?;
    let (created, existing) = apply_chart_in(&tx, entity_id, chart)?;
    let mut report = RemapReport { created, existing, ..Default::default() };
    let mut min_seq: Option<i64> = None;
    for (source_id, target_path) in moves {
        let source = match get_account(&tx, *source_id) {
            Ok(a) => a,
            Err(Error::NotFound(_)) => {
                report.skipped.push(format!("#{source_id}: no such account"));
                continue;
            }
            Err(e) => return Err(e),
        };
        if source.entity_id != entity_id {
            report.skipped.push(format!("{}: belongs to another entity", source.path));
            continue;
        }
        let target = match find_by_path(&tx, entity_id, target_path)? {
            Some(t) => t,
            None => {
                report.skipped.push(format!("{}: target {} not found", source.path, target_path));
                continue;
            }
        };
        if target.id == source.id {
            continue;
        }
        let children = match check_merge(&tx, &source, &target) {
            Ok(c) => c,
            Err(Error::Invalid(m)) => {
                report.skipped.push(format!("{}: {}", source.path, m));
                continue;
            }
            Err(e) => return Err(e),
        };
        let (r, seq) = merge_in(&tx, &source, &target, children)?;
        report.merged += 1;
        report.moved_postings += r.moved_postings;
        min_seq = match (min_seq, seq) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
    }
    if let Some(seq) = min_seq {
        crate::hashchain::rechain(&tx, entity_id, seq)?;
    }
    crate::audit::log(&tx, "entities", entity_id, "remap", None, Some(serde_json::to_value(&report).unwrap_or_default()))?;
    tx.commit()?;
    Ok(report)
}
