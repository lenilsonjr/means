//! Entity names, archive and unarchive: an archived entity leaves the lists, keeps its books
//! readable and verifiable, and refuses every write.

use chrono::NaiveDate;
use means_core::accounts::{self, AccountUpdate};
use means_core::model::{AccountType, EntryInput, EntryStatus, PostingInput};
use means_core::{audit, db, entities, hashchain, journal, rates, Db, Error};
use rust_decimal::Decimal;

fn day(s: &str) -> NaiveDate {
    means_core::parse_date(s).unwrap()
}

fn dec(s: &str) -> Decimal {
    means_core::money::parse(s).unwrap()
}

fn archived<T: std::fmt::Debug>(r: means_core::Result<T>) -> String {
    match r {
        Err(Error::Archived(m)) => m,
        other => panic!("expected an entity_archived refusal, got {other:?}"),
    }
}

#[test]
fn rename_keeps_names_unique_among_active_entities_and_is_audited() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let first = entities::create_entity(&mut c, "Northwind Trading LLC", "company", "US", "USD").unwrap();
    let second = entities::create_entity(&mut c, "Northwind Trading LLC (rebuilt)", "company", "US", "USD").unwrap();

    let taken = entities::rename_in(&c, second.id, "northwind trading llc");
    assert!(matches!(taken, Err(Error::Conflict(_))), "{taken:?}");
    assert!(matches!(entities::rename_in(&c, second.id, "  "), Err(Error::Invalid(_))));

    entities::archive(&mut c, first.id, "Test User", "rebuilt as a new entity").unwrap();
    let renamed = entities::rename_in(&c, second.id, "Northwind Trading LLC").unwrap();
    assert_eq!(renamed.name, "Northwind Trading LLC");
    let events = audit::history(&c, "entities", second.id).unwrap();
    let rename = events.iter().find(|e| e.action == "rename").unwrap();
    assert_eq!(rename.before.as_deref(), Some(r#"{"name":"Northwind Trading LLC (rebuilt)"}"#));
    assert_eq!(rename.after.as_deref(), Some(r#"{"name":"Northwind Trading LLC"}"#));

    // A name lookup over the list finds the active entity first.
    let all = entities::list_entities(&c, true).unwrap();
    assert_eq!(all.iter().find(|e| e.name == "Northwind Trading LLC").unwrap().id, second.id);
    // A new entity can take the name of an archived entity, but not of an active one.
    assert!(matches!(entities::create_entity(&mut c, "Northwind Trading LLC", "company", "US", "USD"), Err(Error::Conflict(_))));
}

#[test]
fn archive_hides_the_entity_keeps_the_chain_verifiable_and_refuses_writes() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let old = entities::create_entity(&mut c, "Blue Harbor Pte. Ltd.", "company", "SG", "SGD").unwrap();
    let bank = accounts::ensure_account(&c, old.id, AccountType::Asset, &["Bank", "Example Bank"], "bank", "SGD").unwrap();
    let fees = accounts::ensure_account(&c, old.id, AccountType::Expense, &["Bank fees"], "expense", "SGD").unwrap();
    let mut input = EntryInput::new(old.id, day("2026-01-05"));
    input.postings = vec![PostingInput::new(bank.id, dec("-12")), PostingInput::balancing(fees.id)];
    let posted = journal::create_entry(&mut c, input).unwrap();
    let mut draft = EntryInput::new(old.id, day("2026-01-06"));
    draft.status = EntryStatus::Draft;
    draft.postings = vec![PostingInput::new(bank.id, dec("-3")), PostingInput::balancing(fees.id)];
    let draft = journal::create_entry(&mut c, draft).unwrap();
    let head_before = hashchain::verify(&c, old.id).unwrap();

    assert!(matches!(entities::archive(&mut c, old.id, "", "reason"), Err(Error::Invalid(_))));
    assert!(matches!(entities::archive(&mut c, old.id, "Test User", " "), Err(Error::Invalid(_))));
    let outcome = entities::archive(&mut c, old.id, "Test User", "rebuilt as a new entity").unwrap();
    assert!(outcome.entity.archived_at.is_some());
    let event = audit::get(&c, outcome.audit_event_id).unwrap();
    assert_eq!((event.action.as_str(), event.actor.as_str(), event.reason.as_str()), ("archive", "Test User", "rebuilt as a new entity"));
    assert!(matches!(entities::archive(&mut c, old.id, "Test User", "again"), Err(Error::Invalid(_))));

    assert!(entities::list_entities(&c, false).unwrap().is_empty());
    assert_eq!(entities::list_entities(&c, true).unwrap().len(), 1);

    // The books stay readable and verifiable.
    assert_eq!(journal::get_entry(&c, posted.id).unwrap().postings.len(), 2);
    let head = hashchain::verify(&c, old.id).unwrap();
    assert_eq!((head.checked, head.first_bad_seq, head.head.clone()), (head_before.checked, None, head_before.head));

    // Every write is refused: the entity row, accounts, entries and their postings.
    assert!(archived(entities::ensure_active(&c, old.id)).contains("Blue Harbor Pte. Ltd."));
    archived(entities::rename_in(&c, old.id, "Another name"));
    archived(entities::set_facts(&mut c, old.id, entities::EntityFactsPatch { legal_form: Some("Private company".into()), ..Default::default() }));
    archived(accounts::ensure_account(&c, old.id, AccountType::Asset, &["Bank", "Second Bank"], "bank", "SGD"));
    archived(accounts::update_account(&c, bank.id, AccountUpdate { code: Some("1010".into()), ..Default::default() }));
    let mut more = EntryInput::new(old.id, day("2026-02-01"));
    more.postings = vec![PostingInput::new(bank.id, dec("-1")), PostingInput::balancing(fees.id)];
    archived(journal::create_entry(&mut c, more));
    archived(journal::post_entry(&mut c, draft.id));
    archived(journal::delete_entry(&mut c, draft.id));
    archived(journal::void_entry(&mut c, posted.id, None, "wrong books"));
    assert_eq!(journal::get_entry(&c, posted.id).unwrap().status, EntryStatus::Posted);

    // An unarchive opens the books again.
    let back = entities::unarchive(&mut c, old.id, "Test User", "the first books stay", None).unwrap();
    assert!(back.entity.archived_at.is_none());
    assert_eq!(audit::get(&c, back.audit_event_id).unwrap().action, "unarchive");
    assert!(matches!(entities::unarchive(&mut c, old.id, "Test User", "again", None), Err(Error::Invalid(_))));
    journal::post_entry(&mut c, draft.id).unwrap();
    accounts::update_account(&c, bank.id, AccountUpdate { code: Some("1010".into()), ..Default::default() }).unwrap();
}

#[test]
fn unarchive_needs_a_free_name_and_can_rename() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let old = entities::create_entity(&mut c, "Contoso Studio", "company", "PT", "EUR").unwrap();
    entities::archive(&mut c, old.id, "Test User", "rebuilt").unwrap();
    let new = entities::create_entity(&mut c, "Contoso Studio", "company", "PT", "EUR").unwrap();
    assert_ne!(new.id, old.id);
    let taken = entities::unarchive(&mut c, old.id, "Test User", "reopen", None);
    assert!(matches!(&taken, Err(Error::Conflict(m)) if m.contains("--name")), "{taken:?}");
    let back = entities::unarchive(&mut c, old.id, "Test User", "reopen", Some("Contoso Studio (first books)")).unwrap();
    assert_eq!(back.entity.name, "Contoso Studio (first books)");
    assert_eq!(entities::list_entities(&c, false).unwrap().len(), 2);
}

#[test]
fn revaluation_leaves_archived_books_alone() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let old = entities::create_entity(&mut c, "Fabrikam GmbH", "company", "DE", "EUR").unwrap();
    let usd = accounts::ensure_account(&c, old.id, AccountType::Asset, &["Bank", "USD account"], "bank", "USD").unwrap();
    let fees = accounts::ensure_account(&c, old.id, AccountType::Expense, &["Fees"], "expense", "EUR").unwrap();
    let mut input = EntryInput::new(old.id, day("2026-03-02"));
    input.postings = vec![PostingInput::new(usd.id, dec("-10")), PostingInput::balancing(fees.id)];
    journal::create_entry(&mut c, input).unwrap();
    entities::archive(&mut c, old.id, "Test User", "rebuilt").unwrap();
    rates::set_price(&c, "USD", "EUR", day("2026-03-01"), dec("0.9"), "manual").unwrap();
    let report = rates::revalue_missing(&mut c).unwrap();
    assert_eq!((report.fixed, report.failed), (0, 0));
    // The same entry revalues once the books are active again.
    entities::unarchive(&mut c, old.id, "Test User", "reopen", None).unwrap();
    assert_eq!(rates::revalue_missing(&mut c).unwrap().fixed, 1);
}

#[test]
fn migration_28_keeps_entities_and_their_references() {
    // A ledger at version 27 with books, then the later migrations.
    let raw = rusqlite::Connection::open_in_memory().unwrap();
    raw.execute_batch("PRAGMA foreign_keys = ON").unwrap();
    db::migrate_to(&raw, 27).unwrap();
    // The readers of today need later columns, so the seed uses SQL.
    raw.execute_batch(
        "INSERT INTO commodities (code, kind, name, precision) VALUES ('GBP', 'currency', 'Pound sterling', 2);
         INSERT INTO entities (uid, name, kind, country, currency, created_at, updated_at, legal_form, flags)
           VALUES ('e-1', 'Woodgrove Ltd', 'company', 'GB', 'GBP', 'now', 'now', 'Private limited company', '{\"gb.vat\":true}');
         INSERT INTO accounts (uid, entity_id, name, type, commodity_id, created_at, updated_at)
           VALUES ('a-1', 1, 'Bank', 'asset', 1, 'now', 'now'), ('a-2', 1, 'Fees', 'expense', 1, 'now', 'now');",
    )
    .unwrap();
    let accounts_before: i64 = raw.query_row("SELECT COUNT(*) FROM accounts WHERE entity_id = ?1", [1], |r| r.get(0)).unwrap();
    db::migrate_to(&raw, db::schema_version()).unwrap();
    let (name, legal_form, flags): (String, String, String) = raw.query_row("SELECT name, legal_form, flags FROM entities WHERE id = ?1", [1], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
    assert_eq!((name.as_str(), legal_form.as_str(), flags.as_str()), ("Woodgrove Ltd", "Private limited company", "{\"gb.vat\":true}"));
    let accounts_after: i64 = raw.query_row("SELECT COUNT(*) FROM accounts WHERE entity_id = ?1", [1], |r| r.get(0)).unwrap();
    assert_eq!(accounts_before, accounts_after);
    let broken: i64 = raw.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r.get(0)).unwrap();
    assert_eq!(broken, 0);
    let fk_on: i64 = raw.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap();
    assert_eq!(fk_on, 1, "the runner turns foreign keys on again");
    // The child tables still reference the rebuilt table.
    let refs: String = raw.query_row("SELECT sql FROM sqlite_master WHERE name = 'accounts'", [], |r| r.get(0)).unwrap();
    assert!(refs.contains("REFERENCES entities(id)"), "{refs}");
    // Two active entities cannot share a name. An archived entity can share it.
    let twin = "INSERT INTO entities (uid, name, currency, created_at, updated_at) VALUES ('e-2', 'Woodgrove Ltd', 'GBP', 'now', 'now')";
    assert!(raw.execute(twin, []).is_err());
    raw.execute("UPDATE entities SET archived_at = 'now' WHERE id = 1", []).unwrap();
    raw.execute(twin, []).unwrap();
}

#[test]
fn active_entity_names_are_unique_without_case() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let first = entities::create_entity(&mut c, "Litware Studio", "company", "PT", "EUR").unwrap();
    // The engine check and the index both ignore case.
    assert!(matches!(entities::create_entity(&mut c, "LITWARE studio", "company", "PT", "EUR"), Err(Error::Conflict(_))));
    let raw = c.execute("INSERT INTO entities (uid, name, currency, created_at, updated_at) VALUES ('e-twin', 'litware STUDIO', 'EUR', 'now', 'now')", []);
    assert!(raw.is_err(), "the index of migration 28 ignores case");
    // An archived entity frees the name for every case.
    entities::archive(&mut c, first.id, "Test User", "rebuilt").unwrap();
    let second = entities::create_entity(&mut c, "LITWARE STUDIO", "company", "PT", "EUR").unwrap();
    let taken = entities::unarchive(&mut c, first.id, "Test User", "reopen", None);
    assert!(matches!(&taken, Err(Error::Conflict(m)) if m.contains("--name")), "{taken:?}");
    let other = entities::create_entity(&mut c, "Litware Studio Two", "company", "PT", "EUR").unwrap();
    assert!(matches!(entities::rename_in(&c, other.id, "litware studio"), Err(Error::Conflict(_))));
    assert_ne!(second.id, other.id);
}

#[test]
fn migration_28_refuses_active_names_that_differ_only_in_case() {
    let raw = rusqlite::Connection::open_in_memory().unwrap();
    db::migrate_to(&raw, 27).unwrap();
    raw.execute_batch(
        "INSERT INTO commodities (code, kind, name, precision) VALUES ('GBP', 'currency', 'Pound sterling', 2);
         INSERT INTO entities (uid, name, currency, created_at, updated_at) VALUES ('e-1', 'Woodgrove Ltd', 'GBP', 'now', 'now');
         INSERT INTO entities (uid, name, currency, created_at, updated_at) VALUES ('e-2', 'WOODGROVE LTD', 'GBP', 'now', 'now');",
    )
    .unwrap();
    let err = db::migrate_to(&raw, 28).unwrap_err().to_string();
    assert!(err.contains("'Woodgrove Ltd' (#1)") && err.contains("'WOODGROVE LTD' (#2)") && err.contains("differ only in case"), "{err}");
    let version: i64 = raw.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(version, 27);
    // A case twin that is archived does not block the migration.
    raw.execute("UPDATE entities SET archived_at = 'now' WHERE id = 2", []).unwrap();
    db::migrate_to(&raw, 28).unwrap();
}

#[test]
fn update_entity_refuses_an_archive_change_and_checks_a_rename() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let first = entities::create_entity(&mut c, "Adatum Holdings", "company", "SG", "SGD").unwrap();
    let second = entities::create_entity(&mut c, "Adatum Trading", "company", "SG", "SGD").unwrap();
    // The archive state changes only through archive and unarchive.
    let err = entities::update_entity_by(&mut c, first.id, "", "", "SG", None, true, "grpc").unwrap_err();
    assert!(matches!(&err, Error::Invalid(m) if m.contains("means entity archive")), "{err:?}");
    assert!(entities::get_entity(&c, first.id).unwrap().archived_at.is_none());
    entities::archive(&mut c, first.id, "Test User", "rebuilt").unwrap();
    let err = entities::update_entity_by(&mut c, first.id, "", "", "SG", None, false, "grpc").unwrap_err();
    assert!(matches!(&err, Error::Invalid(m) if m.contains("means entity unarchive")), "{err:?}");
    assert!(entities::get_entity(&c, first.id).unwrap().archived_at.is_some());
    // A rename checks the active names without case, and records the actor.
    let third = entities::create_entity(&mut c, "Adatum Services", "company", "SG", "SGD").unwrap();
    assert!(matches!(entities::update_entity_by(&mut c, third.id, "ADATUM TRADING", "", "SG", None, false, "grpc"), Err(Error::Conflict(_))));
    let renamed = entities::update_entity_by(&mut c, second.id, "Adatum Trading Pte. Ltd.", "", "SG", None, false, "grpc").unwrap();
    assert_eq!(renamed.name, "Adatum Trading Pte. Ltd.");
    let rename = audit::history(&c, "entities", second.id).unwrap().into_iter().find(|e| e.action == "rename").unwrap();
    assert_eq!(rename.actor, "grpc");
}

#[test]
fn archive_is_refused_while_links_to_active_entities_are_not_void() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let old = entities::create_entity(&mut c, "Tailspin Toys Pte. Ltd.", "company", "SG", "SGD").unwrap();
    let other = entities::create_entity(&mut c, "Wingtip Toys LLC", "company", "US", "USD").unwrap();
    let old_bank = accounts::ensure_account(&c, old.id, AccountType::Asset, &["Bank"], "bank", "SGD").unwrap();
    let old_loan = accounts::ensure_account(&c, old.id, AccountType::Asset, &["Loan to Wingtip"], "receivable", "SGD").unwrap();
    let other_bank = accounts::ensure_account(&c, other.id, AccountType::Asset, &["Bank"], "bank", "USD").unwrap();
    let other_loan = accounts::ensure_account(&c, other.id, AccountType::Liability, &["Loan from Tailspin"], "loan", "USD").unwrap();
    let mut a = EntryInput::new(old.id, day("2026-03-02"));
    a.postings = vec![PostingInput::new(old_loan.id, dec("1350")), PostingInput::balancing(old_bank.id)];
    let a = journal::create_entry(&mut c, a).unwrap();
    let mut b = EntryInput::new(other.id, day("2026-03-03"));
    b.postings = vec![PostingInput::new(other_bank.id, dec("1000")), PostingInput::balancing(other_loan.id)];
    let b = journal::create_entry(&mut c, b).unwrap();
    let link = means_core::intervault::link(&mut c, &a.uid, &b.uid, "loan", "").unwrap();

    let err = entities::archive(&mut c, old.id, "Test User", "rebuilt").unwrap_err();
    match &err {
        Error::Invalid(m) => assert!(m.contains("Wingtip Toys LLC") && m.contains(&format!("entry #{}", b.id)) && m.contains(&link.uid), "{m}"),
        other => panic!("expected invalid input, got {other:?}"),
    }
    assert!(entities::get_entity(&c, old.id).unwrap().archived_at.is_none());

    // After a cascade void the link is void, and the archive goes through.
    journal::review_void(&mut c, a.id, None, "rebuilt", true, true).unwrap();
    entities::archive(&mut c, old.id, "Test User", "rebuilt").unwrap();
    // The trigger message names the archived entity.
    let message = archived(accounts::ensure_account(&c, old.id, AccountType::Asset, &["Bank", "Second"], "bank", "SGD"));
    assert!(message.contains("Tailspin Toys Pte. Ltd. is archived"), "{message}");
}

#[test]
fn archive_ignores_links_to_archived_entities() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let first = entities::create_entity(&mut c, "Proseware One", "company", "SG", "SGD").unwrap();
    let second = entities::create_entity(&mut c, "Proseware Two", "company", "SG", "SGD").unwrap();
    let mut ids = Vec::new();
    for e in [&first, &second] {
        let bank = accounts::ensure_account(&c, e.id, AccountType::Asset, &["Bank"], "bank", "SGD").unwrap();
        let due = accounts::ensure_account(&c, e.id, AccountType::Asset, &["Due"], "receivable", "SGD").unwrap();
        let mut input = EntryInput::new(e.id, day("2026-03-02"));
        input.postings = vec![PostingInput::new(due.id, dec("10")), PostingInput::balancing(bank.id)];
        ids.push(journal::create_entry(&mut c, input).unwrap().uid);
    }
    means_core::intervault::link(&mut c, &ids[0], &ids[1], "loan", "").unwrap();
    // The two sets of books go together: the second archive sees only an archived entity.
    entities::archive(&mut c, second.id, "Test User", "rebuilt").unwrap_err();
    c.execute("UPDATE entities SET archived_at = 'now' WHERE id = ?1", [second.id]).unwrap();
    entities::archive(&mut c, first.id, "Test User", "rebuilt").unwrap();
}

#[test]
fn schedules_skip_an_archived_entity_and_resume_after_unarchive() {
    use means_core::model::{EntryTemplate, TemplateLine};
    use means_core::templates;
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let old = entities::create_entity(&mut c, "Alpine Ski House", "company", "CH", "CHF").unwrap();
    let active = entities::create_entity(&mut c, "Coho Winery", "company", "CH", "CHF").unwrap();
    let save = |entity: i64, name: &str| {
        let bank = accounts::ensure_account(&c, entity, AccountType::Asset, &["Bank"], "bank", "CHF").unwrap();
        let rent = accounts::ensure_account(&c, entity, AccountType::Expense, &["Rent"], "expense", "CHF").unwrap();
        let line = |account_id, method: &str, value: Option<&str>| TemplateLine { account_id, method: method.into(), value: value.map(dec), of_line: None, memo: String::new(), label: String::new() };
        templates::save_template(
            &c,
            &EntryTemplate {
                id: 0,
                uid: String::new(),
                entity_id: entity,
                name: name.into(),
                payee: "Example Landlord".into(),
                description: "Monthly rent".into(),
                lines: vec![line(bank.id, "fixed", Some("-50")), line(rent.id, "balance", None)],
                rrule: "FREQ=MONTHLY;BYMONTHDAY=1".into(),
                starts_on: Some(day("2026-01-01")),
                next_on: None,
                ends_on: None,
                auto_post: false,
                lead_days: 0,
                version: 1,
                active: true,
                created_at: String::new(),
            },
        )
        .unwrap()
    };
    // The template of the archived entity comes first in the selection order (by name).
    let first = save(old.id, "A rent");
    let second = save(active.id, "B rent");
    entities::archive(&mut c, old.id, "Test User", "rebuilt").unwrap();

    let created = templates::run_schedules(&mut c, Some(day("2026-03-15"))).unwrap();
    assert_eq!(created, 3, "the active entity still runs");
    let count = |c: &rusqlite::Connection, t: i64| -> i64 { c.query_row("SELECT COUNT(*) FROM journal_entries WHERE template_id = ?1", [t], |r| r.get(0)).unwrap() };
    assert_eq!((count(&c, first.id), count(&c, second.id)), (0, 3));
    // The archived template keeps its schedule state.
    assert_eq!(templates::get_template(&c, first.id).unwrap().next_on, first.next_on);

    entities::unarchive(&mut c, old.id, "Test User", "reopen", None).unwrap();
    assert_eq!(templates::run_schedules(&mut c, Some(day("2026-03-15"))).unwrap(), 3);
    assert_eq!(count(&c, first.id), 3);
}
