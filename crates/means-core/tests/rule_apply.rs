//! `rules::apply_to_drafts`: rules added after an import re-route the import's drafts.

use means_core::model::*;
use means_core::{accounts, entities, hashchain, imports, journal, rules, splits, Db};

const CSV: &str = r#""Booking Date","Value Date","Partner Name","Partner Iban","Type","Payment Reference","Account Name","Amount (EUR)","Original Amount","Original Currency","Exchange Rate"
"2026-03-01","2026-03-01","Sample Vendor","","MasterCard Payment","","Main Account","-12.00","","",""
"2026-03-02","2026-03-02","Sample Vendor","","MasterCard Payment","","Main Account","-15.00","","",""
"2026-03-03","2026-03-03","Owner Draw Example","","Debit Transfer","","Main Account","-500.00","","",""
"2026-03-04","2026-03-04","Corner Cafe","","MasterCard Payment","","Main Account","-3.20","","",""
"#;

struct World {
    db: Db,
    entity: Entity,
    bank: Account,
    domains: Account,
    other: Account,
    owner: Account,
    import_id: i64,
    /// Entry ids by line, in file order.
    entries: Vec<i64>,
}

/// Import four lines with no rules, so every line becomes an Uncategorized draft.
/// Then post the second Sample Vendor line by hand and add the rules.
fn world() -> World {
    let db = Db::open_memory().unwrap();
    let (entity, bank, domains, other, owner, import_id, entries) = {
        let mut conn = db.conn();
        let entity = entities::create_entity(&mut conn, "Northwind Holdings", "company", "PT", "EUR").unwrap();
        let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank", "Example Bank"], "bank", "EUR").unwrap();
        let domains = accounts::ensure_account(&conn, entity.id, AccountType::Expense, &["Domains"], "expense", "EUR").unwrap();
        let other = accounts::ensure_account(&conn, entity.id, AccountType::Expense, &["Other"], "expense", "EUR").unwrap();
        let owner = accounts::ensure_account(&conn, entity.id, AccountType::Equity, &["Owner"], "equity", "EUR").unwrap();
        let out = imports::run_import(&mut conn, imports::ImportRequest::new("n26_csv", Some(bank.id), "bank.csv", CSV.as_bytes())).unwrap();
        let entries: Vec<i64> = out.lines.iter().map(|l| l.journal_entry_id.unwrap()).collect();
        journal::post_draft_to(&mut conn, entries[1], &[splits::Leg { account_id: other.id, quantity: None, memo: String::new() }], None).unwrap();
        rule(&conn, entity.id, "sample vendor", domains.id, "Sample Vendor");
        rule(&conn, entity.id, "owner draw", owner.id, "");
        (entity, bank, domains, other, owner, out.import.id, entries)
    };
    World { db, entity, bank, domains, other, owner, import_id, entries }
}

fn rule(conn: &rusqlite::Connection, entity_id: i64, contains: &str, account_id: i64, payee: &str) -> Rule {
    rules::save_rule(
        conn,
        &Rule {
            id: 0,
            entity_id,
            name: contains.into(),
            position: 0,
            enabled: true,
            conditions: vec![RuleCondition { field: "description".into(), op: "contains".into(), value: contains.into() }],
            account_id: Some(account_id),
            template_id: None,
            payee: payee.into(),
            hits_count: 0,
            created_at: String::new(),
            tags: String::new(),
        },
    )
    .unwrap()
}

/// The account of the posting that is not on the bank account.
fn contra(entry: &JournalEntry, bank: &Account) -> i64 {
    let others: Vec<_> = entry.postings.iter().filter(|p| p.account_id != bank.id).collect();
    assert_eq!(others.len(), 1);
    others[0].account_id
}

fn snapshot(conn: &rusqlite::Connection) -> Vec<String> {
    ["journal_entries", "postings", "statement_lines", "payees", "rules", "audit_log"]
        .iter()
        .map(|table| {
            let mut stmt = conn.prepare(&format!("SELECT * FROM {table} ORDER BY id")).unwrap();
            let columns = stmt.column_count();
            stmt.query_map([], |row| Ok((0..columns).map(|i| format!("{:?}", row.get_ref(i).unwrap())).collect::<Vec<_>>().join("|"))).unwrap().collect::<Result<Vec<_>, _>>().unwrap().join("\n")
        })
        .collect()
}

#[test]
fn a_new_rule_reroutes_uncategorized_drafts_and_leaves_posted_entries() {
    let w = world();
    let mut conn = w.db.conn();
    let suspense = accounts::find_by_role(&conn, w.entity.id, "suspense").unwrap();
    let posted_before = journal::get_entry(&conn, w.entries[1]).unwrap();
    assert_eq!(posted_before.status, EntryStatus::Posted);

    let r = rules::apply_to_drafts(&mut conn, w.entity.id, None, true).unwrap();
    // The posted line is not a draft, so three drafts are examined.
    assert_eq!((r.examined, r.changed, r.unchanged, r.applied), (3, 2, 1, true));
    assert_eq!(r.changes[0].entry_id, w.entries[0]);
    assert_eq!((r.changes[0].from.as_str(), r.changes[0].to.as_str()), (suspense.path.as_str(), w.domains.path.as_str()));
    assert_eq!(r.changes[1].to, w.owner.path);

    let vendor = journal::get_entry(&conn, w.entries[0]).unwrap();
    assert_eq!(vendor.status, EntryStatus::Draft);
    assert_eq!(vendor.payee, "Sample Vendor");
    assert_eq!(contra(&vendor, &w.bank), w.domains.id);
    // The bank posting keeps its statement evidence.
    let line: Option<i64> = conn.query_row("SELECT posting_id FROM statement_lines WHERE journal_entry_id = ?1", [vendor.id], |r| r.get(0)).unwrap();
    assert_eq!(line, vendor.postings.iter().find(|p| p.account_id == w.bank.id).map(|p| p.id));

    // A cross-type move is allowed on a draft: expense to equity. The rule has no payee, so the line's description is the payee.
    let draw = journal::get_entry(&conn, w.entries[2]).unwrap();
    assert_eq!(draw.status, EntryStatus::Draft);
    assert_eq!(contra(&draw, &w.bank), w.owner.id);
    assert!(draw.payee.contains("Owner Draw Example"), "{}", draw.payee);

    // No rule matches the cafe: the draft stays on Uncategorized.
    let cafe = journal::get_entry(&conn, w.entries[3]).unwrap();
    assert_eq!(contra(&cafe, &w.bank), suspense.id);

    // The posted entry with the same description does not change, and the chain still verifies.
    let posted_after = journal::get_entry(&conn, w.entries[1]).unwrap();
    assert_eq!(contra(&posted_after, &w.bank), w.other.id);
    assert_eq!(serde_json::to_value(&posted_before).unwrap(), serde_json::to_value(&posted_after).unwrap());
    assert!(hashchain::verify(&conn, w.entity.id).unwrap().first_bad_seq.is_none());
}

#[test]
fn a_draft_already_on_the_selected_account_is_unchanged() {
    let w = world();
    let mut conn = w.db.conn();
    rules::apply_to_drafts(&mut conn, w.entity.id, None, true).unwrap();
    let before = snapshot(&conn);
    let again = rules::apply_to_drafts(&mut conn, w.entity.id, None, true).unwrap();
    assert_eq!((again.examined, again.changed, again.unchanged), (3, 0, 3));
    assert!(again.changes.is_empty());
    assert_eq!(snapshot(&conn), before);
}

#[test]
fn dry_run_reports_the_changes_and_writes_nothing() {
    let w = world();
    let mut conn = w.db.conn();
    let before = snapshot(&conn);
    let preview = rules::apply_to_drafts(&mut conn, w.entity.id, None, false).unwrap();
    assert_eq!((preview.examined, preview.changed, preview.applied), (3, 2, false));
    assert_eq!(snapshot(&conn), before);
    // The real run makes exactly the changes the preview showed.
    let applied = rules::apply_to_drafts(&mut conn, w.entity.id, None, true).unwrap();
    assert_eq!(serde_json::to_value(&preview.changes).unwrap(), serde_json::to_value(&applied.changes).unwrap());
}

#[test]
fn the_import_filter_limits_the_drafts() {
    let w = world();
    let mut conn = w.db.conn();
    let other_import = rules::apply_to_drafts(&mut conn, w.entity.id, Some(w.import_id + 1), true).unwrap();
    assert_eq!((other_import.examined, other_import.changed), (0, 0));
    let this_import = rules::apply_to_drafts(&mut conn, w.entity.id, Some(w.import_id), true).unwrap();
    assert_eq!((this_import.examined, this_import.changed), (3, 2));
}
