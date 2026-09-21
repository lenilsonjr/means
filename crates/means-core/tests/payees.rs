use means_core::{accounts, entities, hashchain, imports, journal, payees, rules, AccountType, Db, EntryInput, EntryStatus, PostingInput, Rule, RuleCondition};
use rusqlite::Connection;
const CSV: &str = "Booking Date,Value Date,Partner Name,Partner Iban,Type,Payment Reference,Account Name,Amount (EUR)\n2026-02-01,2026-02-01,CAFE CENTRAL,,Card,,Main,-10.00\n";
fn setup() -> (Db, i64, i64, i64) {
    let db = Db::open_memory().unwrap();
    let (e, b, f) = {
        let mut c = db.conn();
        let e = entities::create_entity(&mut c, "Me", "person", "PT", "EUR").unwrap().id;
        let b = accounts::ensure_account(&c, e, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap().id;
        let f = accounts::ensure_account(&c, e, AccountType::Expense, &["Food"], "expense", "EUR").unwrap().id;
        (e, b, f)
    };
    (db, e, b, f)
}
fn change(e: i64, name: &str, alias: &str) -> payees::Change {
    payees::Change { id: 0, entity_id: e, name: name.into(), active: true, aliases: vec![alias.into()] }
}
fn save(c: &mut Connection, p: payees::Change) -> payees::Payee {
    let preview = payees::save(c, p.clone(), None).unwrap();
    assert!(!preview.applied);
    payees::save(c, p, Some(&preview.token)).unwrap().payee.unwrap()
}
fn rule(e: i64, f: i64) -> Rule {
    Rule {
        id: 0,
        entity_id: e,
        name: "Coffee".into(),
        position: 10,
        enabled: true,
        conditions: vec![RuleCondition { field: "payee".into(), op: "equals".into(), value: "Coffee Shop".into() }],
        account_id: Some(f),
        template_id: None,
        payee: String::new(),
        hits_count: 0,
        created_at: String::new(),
        tags: String::new(),
    }
}
#[test]
fn normalization_conflicts_archives_and_entity_isolation() {
    let (db, e, _, _) = setup();
    let mut c = db.conn();
    let p = save(&mut c, change(e, "Coffee Shop", "  Café   CENTRAL "));
    assert_eq!(payees::normalize(" CAFÉ\n CENTRAL "), "café central");
    assert_eq!(payees::resolve(&c, e, "pix Café  Central PT").unwrap().unwrap().id, p.id);
    assert!(payees::resolve(&c, e, "Cafe Central").unwrap().is_none());
    let q = save(&mut c, change(e, "Another", "café"));
    assert!(payees::resolve(&c, e, "Café Central").unwrap().is_none());
    let entity = entities::create_entity(&mut c, "Other", "company", "PT", "EUR").unwrap().id;
    assert!(payees::resolve(&c, entity, "Café Central").unwrap().is_none());
    let mut bad = change(entity, "Moved", "x");
    bad.id = p.id;
    assert!(payees::save(&mut c, bad, None).is_err());
    let mut archive = change(e, "Another", "café");
    archive.id = q.id;
    archive.active = false;
    save(&mut c, archive);
    assert_eq!(payees::resolve(&c, e, "Café Central").unwrap().unwrap().id, p.id);
    assert!(payees::save(&mut c, change(e, "Empty", " \t"), None).is_err());
}
#[test]
fn import_resolves_before_rules_preserves_evidence_and_explicit_actions() {
    let (db, e, b, f) = setup();
    let mut c = db.conn();
    let p = save(&mut c, change(e, "Coffee Shop", "cafe central"));
    rules::save_rule(&c, &rule(e, f)).unwrap();
    let out = imports::run_import(&mut c, imports::ImportRequest::new("n26_csv", Some(b), "bank.csv", CSV.as_bytes())).unwrap();
    let sl = &out.lines[0];
    let entry = journal::get_entry(&c, sl.journal_entry_id.unwrap()).unwrap();
    assert_eq!(entry.payee, sl.description);
    assert_eq!(entry.display_payee, "Coffee Shop");
    assert_eq!(entry.payee_id, Some(p.id));
    assert_eq!(entry.status, EntryStatus::Posted);
    assert_eq!(entry.postings[1].account_id, f);
    assert_eq!(entry.postings[0].fingerprint.as_deref(), Some(sl.fingerprint.as_str()));
    assert!(imports::run_import(&mut c, imports::ImportRequest::new("n26_csv", Some(b), "again.csv", CSV.as_bytes())).is_err());
    let mut r = rule(e, f);
    r.id = rules::list_rules(&c, Some(e)).unwrap()[0].id;
    r.payee = "Explicit choice".into();
    rules::save_rule(&c, &r).unwrap();
    let text = CSV.replace("2026-02-01", "2026-02-02");
    let out = imports::run_import(&mut c, imports::ImportRequest::new("n26_csv", Some(b), "next.csv", text.as_bytes())).unwrap();
    let entry = journal::get_entry(&c, out.lines[0].journal_entry_id.unwrap()).unwrap();
    assert_eq!(entry.payee, "Explicit choice");
    assert_eq!(entry.display_payee, "Explicit choice");
    assert_eq!(entry.payee_id, None);
    assert!(hashchain::verify(&c, e).unwrap().first_bad_entry_id.is_none());
}
#[test]
fn previews_cover_rule_changes_and_refuse_stale_state() {
    let (db, e, b, f) = setup();
    let mut c = db.conn();
    rules::save_rule(&c, &rule(e, f)).unwrap();
    let out = imports::run_import(&mut c, imports::ImportRequest::new("n26_csv", Some(b), "bank.csv", CSV.as_bytes())).unwrap();
    let change = change(e, "Coffee Shop", "cafe central");
    let p = payees::save(&mut c, change.clone(), None).unwrap();
    assert_eq!(p.impacts.len(), 1);
    assert_eq!(p.impacts[0].before_rule, None);
    assert!(p.impacts[0].after_rule.is_some());
    let mut r = rule(e, f);
    r.position = 1;
    r.name = "New precedence".into();
    rules::save_rule(&c, &r).unwrap();
    assert!(payees::save(&mut c, change.clone(), Some(&p.token)).is_err());
    let saved = save(&mut c, change);
    let entry = journal::get_entry(&c, out.lines[0].journal_entry_id.unwrap()).unwrap();
    assert_eq!(entry.payee_id, None);
    let p = payees::backfill(&mut c, e, None, false).unwrap();
    assert_eq!(p.impacts[0].candidates, vec![saved.id]);
    assert!(payees::backfill(&mut c, e, Some(&p.token), false).is_err());
    payees::backfill(&mut c, e, Some(&p.token), true).unwrap();
    assert_eq!(journal::get_entry(&c, entry.id).unwrap().payee_id, Some(saved.id));
    assert!(payees::backfill(&mut c, e, Some(&p.token), true).is_err());
    let p = payees::backfill(&mut c, e, None, false).unwrap();
    assert!(p.impacts.is_empty());
}
#[test]
fn historical_links_renames_merge_and_report_preserve_hashes_and_splits() {
    let (db, e, b, f) = setup();
    let mut c = db.conn();
    let p = save(&mut c, change(e, "Coffee Shop", "cafe central"));
    let mut input = EntryInput::new(e, "2026-02-01".parse().unwrap());
    input.payee = "Old booked text".into();
    input.postings = vec![PostingInput::new(b, (-10).into()), PostingInput::new(f, 6.into()), PostingInput::new(f, 4.into())];
    let entry = journal::create_entry(&mut c, input).unwrap();
    let preview = payees::reassign(&mut c, 0, p.id, Some(entry.id), None).unwrap();
    payees::reassign(&mut c, 0, p.id, Some(entry.id), Some(&preview.token)).unwrap();
    let mut rename = change(e, "Renamed", "cafe central");
    rename.id = p.id;
    save(&mut c, rename);
    let updated = journal::get_entry(&c, entry.id).unwrap();
    assert_eq!(updated.payee, "Old booked text");
    assert_eq!(updated.display_payee, "Renamed");
    assert_eq!(updated.hash, entry.hash);
    let report = payees::expenses(&c, e, None, None, None).unwrap();
    assert_eq!(report.total.minor(), 1000);
    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.rows[0].payee_id, Some(p.id));
    let voided = journal::void_entry(&mut c, entry.id, Some("2026-02-02".parse().unwrap()), "test").unwrap();
    let reverse = journal::get_entry(&c, voided.reversed_by_id.unwrap()).unwrap();
    assert_eq!(reverse.payee_id, Some(p.id));
    assert_eq!(payees::expenses(&c, e, None, None, None).unwrap().total.minor(), 0);
    let q = save(&mut c, change(e, "Merged", "coffee"));
    let merge = payees::reassign(&mut c, p.id, q.id, None, None).unwrap();
    assert_eq!(merge.impacts.len(), 2);
    payees::reassign(&mut c, p.id, q.id, None, Some(&merge.token)).unwrap();
    assert!(!payees::get(&c, p.id).unwrap().active);
    assert!(payees::get(&c, q.id).unwrap().aliases.contains(&"cafe central".to_string()));
    assert_eq!(journal::get_entry(&c, entry.id).unwrap().payee_id, Some(q.id));
    assert!(hashchain::verify(&c, e).unwrap().first_bad_entry_id.is_none());
}
#[test]
fn audit_failure_rolls_back_and_database_rejects_cross_entity_links() {
    let (db, e, b, f) = setup();
    let mut c = db.conn();
    let p = save(&mut c, change(e, "Coffee Shop", "cafe central"));
    let mut i = EntryInput::new(e, "2026-02-01".parse().unwrap());
    i.postings = vec![PostingInput::new(b, (-1).into()), PostingInput::new(f, 1.into())];
    let entry = journal::create_entry(&mut c, i).unwrap();
    let other = entities::create_entity(&mut c, "Other", "person", "PT", "EUR").unwrap().id;
    let q = save(&mut c, change(other, "Other shop", "cafe"));
    assert!(c.execute("UPDATE journal_entries SET payee_id=?1 WHERE id=?2", [q.id, entry.id]).is_err());
    let preview = payees::reassign(&mut c, 0, p.id, Some(entry.id), None).unwrap();
    c.execute_batch("CREATE TRIGGER fail_payee_audit BEFORE INSERT ON audit_log WHEN NEW.table_name='entry_payees' BEGIN SELECT RAISE(ABORT,'test audit failure'); END").unwrap();
    assert!(payees::reassign(&mut c, 0, p.id, Some(entry.id), Some(&preview.token)).is_err());
    assert_eq!(journal::get_entry(&c, entry.id).unwrap().payee_id, None);
    c.execute_batch("DROP TRIGGER fail_payee_audit; CREATE TRIGGER fail_save BEFORE INSERT ON audit_log WHEN NEW.table_name='payees' BEGIN SELECT RAISE(ABORT,'test audit failure'); END").unwrap();
    let mut ch = change(e, "Renamed", "changed");
    ch.id = p.id;
    let preview = payees::save(&mut c, ch.clone(), None).unwrap();
    assert!(payees::save(&mut c, ch, Some(&preview.token)).is_err());
    assert_eq!(payees::get(&c, p.id).unwrap(), p);
}
#[test]
fn backfill_writes_real_pre_apply_backup_and_skips_unevidenced_labels() {
    let path = std::env::temp_dir().join(format!("means-payee-backfill-{}.db", means_core::new_uid()));
    let db = Db::open(&path).unwrap();
    let backup;
    {
        let mut c = db.conn();
        let e = entities::create_entity(&mut c, "Me", "person", "PT", "EUR").unwrap().id;
        let b = accounts::ensure_account(&c, e, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap().id;
        let out = imports::run_import(&mut c, imports::ImportRequest::new("n26_csv", Some(b), "bank.csv", CSV.as_bytes())).unwrap();
        let id = out.lines[0].journal_entry_id.unwrap();
        let p = save(&mut c, change(e, "Coffee", "cafe central"));
        let preview = payees::backfill(&mut c, e, None, false).unwrap();
        let result = payees::backfill(&mut c, e, Some(&preview.token), true).unwrap();
        backup = result.backup.unwrap();
        assert_eq!(journal::get_entry(&c, id).unwrap().payee_id, Some(p.id));
        let before = Db::open(&backup).unwrap();
        assert_eq!(journal::get_entry(&before.conn(), id).unwrap().payee_id, None);
    }
    drop(db);
    for file in [path.to_string_lossy().to_string(), backup] {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{file}{suffix}"));
        }
    }
}
#[test]
fn ambiguous_aliases_do_not_guess_or_change_description_rules() {
    let (db, e, b, f) = setup();
    let mut c = db.conn();
    save(&mut c, change(e, "One", "cafe"));
    save(&mut c, change(e, "Two", "central"));
    let mut r = rule(e, f);
    r.conditions = vec![RuleCondition { field: "description".into(), op: "contains".into(), value: "cafe central".into() }];
    rules::save_rule(&c, &r).unwrap();
    let out = imports::run_import(&mut c, imports::ImportRequest::new("n26_csv", Some(b), "bank.csv", CSV.as_bytes())).unwrap();
    let entry = journal::get_entry(&c, out.lines[0].journal_entry_id.unwrap()).unwrap();
    assert_eq!(entry.status, EntryStatus::Posted);
    assert_eq!(entry.payee_id, None);
    assert_eq!(entry.payee, entry.display_payee);
    let preview = payees::backfill(&mut c, e, None, false).unwrap();
    assert_eq!(preview.impacts[0].candidates.len(), 2);
    payees::backfill(&mut c, e, Some(&preview.token), true).unwrap();
    assert_eq!(journal::get_entry(&c, entry.id).unwrap().payee_id, None);
}

#[test]
fn canonical_template_rules_and_manual_text_precedence() {
    use means_core::{templates, EntryTemplate, TemplateLine};
    let (db, e, b, f) = setup();
    let mut c = db.conn();
    let p = save(&mut c, change(e, "Coffee Shop", "cafe central"));
    let t = templates::save_template(
        &c,
        &EntryTemplate {
            id: 0,
            uid: String::new(),
            entity_id: e,
            name: "Coffee".into(),
            payee: String::new(),
            description: String::new(),
            lines: vec![
                TemplateLine { account_id: b, method: "input".into(), value: None, of_line: None, memo: String::new(), label: "Bank".into() },
                TemplateLine { account_id: f, method: "balance".into(), value: None, of_line: None, memo: String::new(), label: String::new() },
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
    let mut r = rule(e, f);
    r.account_id = None;
    r.template_id = Some(t.id);
    rules::save_rule(&c, &r).unwrap();
    let out = imports::run_import(&mut c, imports::ImportRequest::new("n26_csv", Some(b), "bank.csv", CSV.as_bytes())).unwrap();
    let entry = journal::get_entry(&c, out.lines[0].journal_entry_id.unwrap()).unwrap();
    assert_eq!(entry.payee_id, Some(p.id));
    assert_eq!(entry.template_id, Some(t.id));
    assert_eq!(entry.status, EntryStatus::Posted);
    let mut stored = rules::list_rules(&c, Some(e)).unwrap().remove(0);
    stored.enabled = false;
    rules::save_rule(&c, &stored).unwrap();
    let raw = CSV.replace("2026-02-01", "2026-02-03");
    let out = imports::run_import(&mut c, imports::ImportRequest::new("n26_csv", Some(b), "manual.csv", raw.as_bytes())).unwrap();
    // Delete the automatic draft to review its still-unmatched source line.
    let sl = &out.lines[0];
    journal::delete_entry(&mut c, sl.journal_entry_id.unwrap()).unwrap();
    let manual = rules::create_entry_from_line(&mut c, sl.id, Some(f), "Manual counterparty", None, false, &[]).unwrap();
    assert_eq!(manual.payee, "Manual counterparty");
    assert_eq!(manual.payee_id, None);
}

#[test]
fn old_reversal_backfill_uses_original_evidence_and_never_category_labels() {
    let (db, e, b, f) = setup();
    let mut c = db.conn();
    let mut r = rule(e, f);
    r.conditions[0].field = "description".into();
    r.conditions[0].op = "contains".into();
    r.conditions[0].value = "cafe".into();
    r.payee = "Food label".into();
    rules::save_rule(&c, &r).unwrap();
    let out = imports::run_import(&mut c, imports::ImportRequest::new("n26_csv", Some(b), "bank.csv", CSV.as_bytes())).unwrap();
    let entry = journal::get_entry(&c, out.lines[0].journal_entry_id.unwrap()).unwrap();
    let voided = journal::void_entry(&mut c, entry.id, Some("2026-02-02".parse().unwrap()), "test").unwrap();
    let reverse = journal::get_entry(&c, voided.reversed_by_id.unwrap()).unwrap();
    let p = save(&mut c, change(e, "Coffee Shop", "cafe central"));
    // Voiding releases statement evidence. Do not guess from the old category label.
    let unresolved = payees::backfill(&mut c, e, None, false).unwrap();
    assert!(unresolved.impacts.iter().all(|r| r.candidates.is_empty()));
    // An explicit original link supplies the identity for an old unlinked reversal.
    let explicit = payees::reassign(&mut c, 0, p.id, Some(entry.id), None).unwrap();
    payees::reassign(&mut c, 0, p.id, Some(entry.id), Some(&explicit.token)).unwrap();
    let mut input = EntryInput::new(e, "2026-02-01".parse().unwrap());
    input.payee = "Food label".into();
    input.postings = vec![PostingInput::new(b, (-1).into()), PostingInput::new(f, 1.into())];
    let anonymous = journal::create_entry(&mut c, input).unwrap();
    save(&mut c, change(e, "Do not infer categories", "Food label"));
    let preview = payees::backfill(&mut c, e, None, false).unwrap();
    assert!(preview.impacts.iter().find(|r| r.entry_id == anonymous.id).unwrap().candidates.is_empty());
    let before = hashchain::verify(&c, e).unwrap().head;
    payees::backfill(&mut c, e, Some(&preview.token), true).unwrap();
    assert_eq!(journal::get_entry(&c, reverse.id).unwrap().payee_id, Some(p.id));
    assert_eq!(hashchain::verify(&c, e).unwrap().head, before);
    let report = payees::expenses(&c, e, None, None, None).unwrap();
    assert_eq!(report.rows.iter().find(|r| r.payee_id == Some(p.id)).unwrap().amount.minor(), 0);
    assert_eq!(report.total.minor(), 100);
}

#[test]
fn merge_inherits_aliases_previews_conflicts_and_is_atomic() {
    let (db, e, _, _) = setup();
    let mut c = db.conn();
    let source = save(&mut c, change(e, "Source", "cafe"));
    let target = save(&mut c, change(e, "Target", "central"));
    save(&mut c, change(e, "Conflict", "cafeteria"));
    let p = payees::reassign(&mut c, source.id, target.id, None, None).unwrap();
    assert!(p.warnings.iter().any(|w| w.contains("Conflict")));
    assert_eq!(p.payee.as_ref().unwrap().aliases, vec!["cafe", "central"]);
    c.execute_batch("CREATE TRIGGER fail_merge BEFORE INSERT ON audit_log WHEN NEW.action='merge' BEGIN SELECT RAISE(ABORT,'test merge failure'); END").unwrap();
    assert!(payees::reassign(&mut c, source.id, target.id, None, Some(&p.token)).is_err());
    assert_eq!(payees::get(&c, target.id).unwrap().aliases, vec!["central"]);
    assert!(payees::get(&c, source.id).unwrap().active);
    c.execute_batch("DROP TRIGGER fail_merge").unwrap();
    payees::reassign(&mut c, source.id, target.id, None, Some(&p.token)).unwrap();
    assert!(payees::resolve(&c, e, "cafeteria").unwrap().is_none());
    assert_eq!(payees::resolve(&c, e, "CAFE CENTRAL").unwrap().unwrap().id, target.id);
}
