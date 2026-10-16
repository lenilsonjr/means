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

/// Edit a draft as a human does: through `journal::update_entry`.
fn human_edit(conn: &mut rusqlite::Connection, id: i64, payee: Option<&str>, contra_account: Option<i64>, bank: &Account) {
    let entry = journal::get_entry(conn, id).unwrap();
    let mut input = EntryInput::new(entry.entity_id, entry.date);
    input.payee = payee.map(str::to_string).unwrap_or(entry.payee.clone());
    input.description = entry.description.clone();
    input.status = EntryStatus::Draft;
    for p in &entry.postings {
        if p.account_id == bank.id {
            input.postings.push(PostingInput::new(p.account_id, p.quantity.major()));
        } else {
            input.postings.push(PostingInput::balancing(contra_account.unwrap_or(p.account_id)));
        }
    }
    journal::update_entry(conn, id, input).unwrap();
}

fn hits(conn: &rusqlite::Connection, name: &str) -> i64 {
    conn.query_row("SELECT hits_count FROM rules WHERE name = ?1", [name], |r| r.get(0)).unwrap()
}

#[test]
fn a_draft_that_a_human_edited_is_skipped_and_keeps_its_payee() {
    let w = world();
    let mut conn = w.db.conn();
    // A human gives the first draft another payee, and the third draft another account.
    human_edit(&mut conn, w.entries[0], Some("Sample Vendor GmbH"), None, &w.bank);
    human_edit(&mut conn, w.entries[2], None, Some(w.other.id), &w.bank);
    let hits_before = (hits(&conn, "sample vendor"), hits(&conn, "owner draw"));

    let r = rules::apply_to_drafts(&mut conn, w.entity.id, None, true).unwrap();
    assert_eq!(r.changed, 0, "{:?}", r.changes);
    assert_eq!(r.skipped_edited, vec![w.entries[0], w.entries[2]]);
    assert_eq!(journal::get_entry(&conn, w.entries[0]).unwrap().payee, "Sample Vendor GmbH");
    assert_eq!(contra(&journal::get_entry(&conn, w.entries[2]).unwrap(), &w.bank), w.other.id);
    // A skipped draft does not raise the hit count of its rule.
    assert_eq!((hits(&conn, "sample vendor"), hits(&conn, "owner draw")), hits_before);
}

#[test]
fn a_second_apply_follows_a_changed_rule_after_its_own_change() {
    let w = world();
    let mut conn = w.db.conn();
    rules::apply_to_drafts(&mut conn, w.entity.id, None, true).unwrap();
    // The rule now selects another account. The first apply is not a human edit.
    conn.execute("UPDATE rules SET account_id = ?1 WHERE name = 'sample vendor'", [w.other.id]).unwrap();
    let r = rules::apply_to_drafts(&mut conn, w.entity.id, None, true).unwrap();
    assert!(r.skipped_edited.is_empty(), "{:?}", r.skipped_edited);
    assert_eq!(r.changes.len(), 1);
    assert_eq!((r.changes[0].from.as_str(), r.changes[0].to.as_str()), (w.domains.path.as_str(), w.other.path.as_str()));
}

#[test]
fn a_rule_with_a_template_leaves_the_draft_alone_also_with_an_account() {
    let w = world();
    let mut conn = w.db.conn();
    let template = means_core::templates::save_template(
        &conn,
        &EntryTemplate {
            id: 0,
            uid: String::new(),
            entity_id: w.entity.id,
            name: "Vendor split".into(),
            payee: "Sample Vendor".into(),
            description: String::new(),
            lines: vec![
                TemplateLine { account_id: w.bank.id, method: "input".into(), value: None, of_line: None, memo: String::new(), label: String::new() },
                TemplateLine { account_id: w.domains.id, method: "balance".into(), value: None, of_line: None, memo: String::new(), label: String::new() },
            ],
            rrule: String::new(),
            starts_on: None,
            next_on: None,
            ends_on: None,
            auto_post: false,
            lead_days: 0,
            version: 1,
            active: true,
            created_at: String::new(),
        },
    )
    .unwrap();
    // The rule has an account and a template. An import gives the template priority.
    conn.execute("UPDATE rules SET template_id = ?1 WHERE name = 'sample vendor'", [template.id]).unwrap();
    let r = rules::apply_to_drafts(&mut conn, w.entity.id, None, true).unwrap();
    assert!(r.changes.iter().all(|c| c.entry_id != w.entries[0]), "{:?}", r.changes);
    let suspense = accounts::find_by_role(&conn, w.entity.id, "suspense").unwrap();
    assert_eq!(contra(&journal::get_entry(&conn, w.entries[0]).unwrap(), &w.bank), suspense.id);
}

#[test]
fn a_rule_that_changes_only_the_payee_changes_the_draft() {
    let w = world();
    let mut conn = w.db.conn();
    rules::apply_to_drafts(&mut conn, w.entity.id, None, true).unwrap();
    let hits_before = hits(&conn, "sample vendor");
    // The account stays; only the payee of the rule changes.
    conn.execute("UPDATE rules SET payee = 'Sample Vendor Ltd' WHERE name = 'sample vendor'", []).unwrap();
    let r = rules::apply_to_drafts(&mut conn, w.entity.id, None, true).unwrap();
    assert_eq!(r.changes.len(), 1, "{r:?}");
    let change = &r.changes[0];
    assert_eq!((change.payee_from.as_str(), change.payee.as_str()), ("Sample Vendor", "Sample Vendor Ltd"));
    assert_eq!(change.from, change.to);
    let vendor = journal::get_entry(&conn, w.entries[0]).unwrap();
    assert_eq!((vendor.payee.as_str(), contra(&vendor, &w.bank)), ("Sample Vendor Ltd", w.domains.id));
    assert_eq!(hits(&conn, "sample vendor"), hits_before + 1);
}

#[test]
fn a_draft_that_fails_is_listed_and_the_run_goes_on() {
    let w = world();
    let mut conn = w.db.conn();
    // The owner account is closed, so the move of the owner draw fails.
    conn.execute("UPDATE accounts SET closed_at = '2026-01-01' WHERE id = ?1", [w.owner.id]).unwrap();
    let hits_before = hits(&conn, "owner draw");
    let r = rules::apply_to_drafts(&mut conn, w.entity.id, None, true).unwrap();
    assert_eq!(r.changes.len(), 1, "{r:?}");
    assert_eq!(r.changes[0].entry_id, w.entries[0]);
    assert_eq!(r.failed.len(), 1, "{r:?}");
    assert_eq!(r.failed[0].entry_id, w.entries[2]);
    assert!(!r.failed[0].message.is_empty());
    // The failed draft keeps its account, and its rule keeps its hit count.
    let suspense = accounts::find_by_role(&conn, w.entity.id, "suspense").unwrap();
    assert_eq!(contra(&journal::get_entry(&conn, w.entries[2]).unwrap(), &w.bank), suspense.id);
    assert_eq!(hits(&conn, "owner draw"), hits_before);
    assert_eq!(contra(&journal::get_entry(&conn, w.entries[0]).unwrap(), &w.bank), w.domains.id);
}
