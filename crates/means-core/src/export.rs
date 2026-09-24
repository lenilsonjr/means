//! Read-only book-value exports from a single database snapshot.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write;

use chrono::{Datelike, NaiveDate};
use rusqlite::Connection;

use crate::{accounts, entities, journal, money, Error, Result};

fn quoted(value: &str) -> String {
    serde_json::to_string(value).expect("strings serialize")
}

fn label(value: &str) -> String {
    value.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

fn currency(code: &str) -> String {
    // Reserve the escape prefix so arbitrary means codes remain collision-free.
    if !code.starts_with("MEANSX") && code.starts_with(|c: char| c.is_ascii_uppercase()) && code.ends_with(|c: char| c.is_ascii_alphanumeric()) {
        code.to_owned()
    } else {
        format!("MEANSX{}", code.bytes().map(|b| format!("{b:02X}")).collect::<String>())
    }
}

/// Export all entities, including archived books and closed accounts. Only posted
/// and void entries contribute to booked balances; reversals remain ordinary entries.
/// The caller receives the complete document before writing any destination file.
pub fn beancount(conn: &mut Connection) -> Result<String> {
    let tx = conn.transaction()?;
    let out = beancount_scoped(&tx, None, None)?;
    tx.commit()?;
    Ok(out)
}

/// Export the entities with the given uids (all entities for `None`), up to `as_of`.
pub fn beancount_for(conn: &mut Connection, entity_uids: Option<&HashSet<String>>, as_of: Option<NaiveDate>) -> Result<String> {
    let tx = conn.transaction()?;
    let ids = match entity_uids {
        None => None,
        Some(uids) => {
            let all = entities::list_entities(&tx, true)?;
            for uid in uids {
                if !all.iter().any(|e| &e.uid == uid) {
                    return Err(Error::NotFound(format!("entity {uid}")));
                }
            }
            Some(all.into_iter().filter(|e| uids.contains(&e.uid)).map(|e| e.id).collect::<HashSet<_>>())
        }
    };
    let out = beancount_scoped(&tx, ids.as_ref(), as_of)?;
    tx.commit()?;
    Ok(out)
}

/// The Beancount document of a scope, read through the transaction of the caller.
/// `None` for `entity_ids` selects every entity. `as_of` drops the entries dated after it.
pub(crate) fn beancount_scoped(tx: &Connection, entity_ids: Option<&HashSet<i64>>, as_of: Option<NaiveDate>) -> Result<String> {
    let in_scope = |id: i64| entity_ids.is_none_or(|ids| ids.contains(&id));
    let mut out = String::from("; means book-value export: original quantities are posting metadata.\noption \"title\" \"means books\"\n\n");
    let entities: Vec<_> = entities::list_entities(tx, true)?.into_iter().filter(|e| in_scope(e.id)).collect();
    let mut units = BTreeMap::new();
    for entity in &entities {
        units.insert(entity.currency.clone(), currency(&entity.currency));
    }
    for (code, unit) in &units {
        writeln!(out, "option \"operating_currency\" {}", quoted(unit)).unwrap();
        writeln!(out, "0001-01-01 commodity {unit}\n  means-currency: {}\n", quoted(code)).unwrap();
    }
    let mut names = HashMap::new();
    for entity in &entities {
        for account in accounts::list_accounts(tx, Some(entity.id), true)? {
            let name = format!("{}:E{}-{}:A{}-{}", account.r#type.root_name(), entity.id, label(&entity.name), account.id, label(&account.name));
            writeln!(out, "0001-01-01 open {name} {}", units[&entity.currency]).unwrap();
            writeln!(out, "  means-entity-id: {}\n  means-entity: {}\n  means-account-id: {}\n  means-path: {}\n", entity.id, quoted(&entity.name), account.id, quoted(&account.path)).unwrap();
            names.insert(account.id, name);
        }
    }
    let entity_by_id: HashMap<_, _> = entities.iter().map(|e| (e.id, e)).collect();
    // No UI page limit: enumerate every booked entry in the same read snapshot.
    let ids = {
        let mut stmt = tx.prepare("SELECT id, entity_id FROM journal_entries WHERE status IN ('posted', 'void') AND (?1 IS NULL OR date <= ?1) ORDER BY date, id")?;
        let rows = stmt.query_map([as_of.map(|d| d.to_string())], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
        let all = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        all.into_iter().filter(|(_, entity)| in_scope(*entity)).map(|(id, _)| id).collect::<Vec<_>>()
    };
    for id in ids {
        let entry = journal::get_entry(tx, id)?;
        if !(1..=9999).contains(&entry.date.year()) {
            return Err(Error::Invalid(format!("entry {id}: date is outside Beancount's supported range")));
        }
        let entity = entity_by_id.get(&entry.entity_id).ok_or_else(|| Error::Invalid(format!("entry {id}: missing entity")))?;
        writeln!(out, "{} * {} {}", entry.date, quoted(&entry.display_payee), quoted(&entry.description)).unwrap();
        writeln!(out, "  means_booked_payee: {}", quoted(&entry.payee)).unwrap();
        writeln!(out, "  means-entry-id: {id}\n  means-entry-uid: {}\n  means-entity-id: {}\n  means-status: {}", quoted(&entry.uid), entity.id, quoted(entry.status.as_str())).unwrap();
        writeln!(out, "  means-notes: {}\n  means-tags: {}", quoted(&entry.notes), quoted(&serde_json::to_string(&entry.tags)?)).unwrap();
        if let Some(reversed) = entry.reverses_id {
            writeln!(out, "  means-reverses-id: {reversed}").unwrap();
        }
        if let Some(original) = entry.refund_of_id {
            writeln!(out, "  means-refund-of-id: {original}").unwrap();
        }
        for posting in entry.postings {
            let name = names.get(&posting.account_id).ok_or_else(|| Error::Invalid(format!("posting {}: missing account", posting.id)))?;
            writeln!(out, "  {name}  {} {}", money::plain(posting.amount.major()), units[&entity.currency]).unwrap();
            writeln!(
                out,
                "    means-posting-id: {}\n    means-quantity: {}\n    means-commodity: {}\n    means-memo: {}",
                posting.id,
                money::plain(posting.quantity.major()),
                quoted(posting.quantity.commodity()),
                quoted(&posting.memo)
            )
            .unwrap();
        }
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn currency_escapes_are_unambiguous() {
        let codes = ["EUR", "A", "1", "EUR-", "-EUR", "MEANSX31", "EUR_USD", "EUR.USD"];
        let mapped: std::collections::HashSet<_> = codes.iter().map(|c| currency(c)).collect();
        assert_eq!(mapped.len(), codes.len());
        assert_eq!(currency("EUR"), "EUR");
        assert_eq!(currency("1"), "MEANSX31");
        assert_eq!(currency("MEANSX31"), "MEANSX4D45414E53583331");
    }
}
