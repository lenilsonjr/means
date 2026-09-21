//! Provider account routing and pull preferences. No credentials belong in the ledger.
use crate::{accounts, AccountType, Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
pub struct ConnectionAccount {
    pub id: i64,
    pub channel: String,
    pub item_id: String,
    pub provider_account_id: String,
    pub provider_type: String,
    pub name: String,
    pub currency: String,
    pub account_id: Option<i64>,
    pub account_path: String,
    pub entity_id: Option<i64>,
    pub last_pull_at: String,
    pub booked_from: String,
}
const SELECT: &str = "SELECT id,channel,item_id,provider_account_id,provider_type,name,currency,account_id,last_pull_at,booked_from FROM channel_connections";
fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ConnectionAccount> {
    Ok(ConnectionAccount {
        id: r.get(0)?,
        channel: r.get(1)?,
        item_id: r.get(2)?,
        provider_account_id: r.get(3)?,
        provider_type: r.get(4)?,
        name: r.get(5)?,
        currency: r.get(6)?,
        account_id: r.get(7)?,
        last_pull_at: r.get(8)?,
        booked_from: r.get(9)?,
        account_path: String::new(),
        entity_id: None,
    })
}
fn decorate(c: &Connection, mut a: ConnectionAccount) -> Result<ConnectionAccount> {
    if let Some(id) = a.account_id {
        let target = accounts::get_account(c, id)?;
        a.account_path = target.path;
        a.entity_id = Some(target.entity_id);
    }
    Ok(a)
}
pub fn list(c: &Connection) -> Result<Vec<ConnectionAccount>> {
    let mut stmt = c.prepare(&format!("{SELECT} ORDER BY channel,item_id,name,id"))?;
    let rows = stmt.query_map([], row)?.collect::<std::result::Result<Vec<_>, _>>()?;
    rows.into_iter().map(|r| decorate(c, r)).collect()
}
pub fn get(c: &Connection, id: i64) -> Result<ConnectionAccount> {
    let a = c.query_row(&format!("{SELECT} WHERE id=?1"), [id], row).optional()?.ok_or_else(|| Error::NotFound(format!("connection {id}")))?;
    decorate(c, a)
}
/// Discovery changes provider labels, not existing routes, cutoffs, cursors, or pull timestamps.
pub fn discover(c: &Connection, channel: &str, item: &str, accounts: &[ConnectionAccount]) -> Result<()> {
    for a in accounts {
        if a.provider_account_id.is_empty() {
            return Err(Error::Invalid("provider account has no identity".into()));
        }
        c.execute("INSERT INTO channel_connections(channel,item_id,provider_account_id,provider_type,name,currency,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(channel,provider_account_id) DO UPDATE SET item_id=excluded.item_id,provider_type=excluded.provider_type,name=excluded.name,currency=excluded.currency",params![channel,item,a.provider_account_id,a.provider_type,a.name,a.currency,crate::now_ts()])?;
    }
    Ok(())
}
/// Routing edits affect future imports. Existing evidence never moves with a mapping change.
pub fn configure(c: &mut Connection, id: i64, account_id: Option<i64>, booked_from: &str) -> Result<ConnectionAccount> {
    let tx = c.transaction()?;
    let before = get(&tx, id)?;
    let date = crate::parse_opt_date(booked_from)?;
    if date.is_some() && !matches!(before.channel.as_str(), "pluggy" | "enable_banking" | "mercury" | "wise" | "inter_pj") {
        return Err(Error::Invalid("booking cutoff is unsupported for this provider".into()));
    }
    if let Some(target) = account_id {
        let a = accounts::get_account(&tx, target)?;
        if matches!(before.channel.as_str(), "wise" | "inter_pj") && a.r#type != AccountType::Asset {
            return Err(Error::Invalid("Wise and Inter PJ statements require an asset destination".into()));
        }
        if a.placeholder || a.is_closed() {
            return Err(Error::Invalid("choose an open account that accepts postings".into()));
        }
        if !matches!(a.r#type, AccountType::Asset | AccountType::Liability) {
            return Err(Error::Invalid("bank connections require an asset or liability account".into()));
        }
        if before.currency.is_empty() || a.commodity != before.currency {
            return Err(Error::Invalid("destination currency must match the provider account".into()));
        }
        if (before.provider_type.eq_ignore_ascii_case("credit") || before.provider_type.eq_ignore_ascii_case("credit_card")) && a.r#type != AccountType::Liability {
            return Err(Error::Invalid("credit accounts require a liability destination".into()));
        }
    }
    tx.execute("UPDATE channel_connections SET account_id=?2,booked_from=?3 WHERE id=?1", params![id, account_id, date.map(|d| d.to_string()).unwrap_or_default()])?;
    let after = get(&tx, id)?;
    crate::audit::log(&tx, "channel_connections", id, "configure", Some(serde_json::to_value(before)?), Some(serde_json::to_value(&after)?))?;
    tx.commit()?;
    Ok(after)
}
