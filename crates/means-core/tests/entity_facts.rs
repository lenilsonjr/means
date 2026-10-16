use chrono::NaiveDate;
use means_core::entities::{self, EntityFactsPatch};
use means_core::groups::{self, NewMember};
use means_core::{audit, db, Db, Error};
use rusqlite::Connection;

fn d(s: &str) -> NaiveDate {
    means_core::parse_date(s).unwrap()
}

fn company(c: &mut Connection, name: &str, country: &str, currency: &str) -> i64 {
    entities::create_entity(c, name, "company", country, currency).unwrap().id
}

#[test]
fn facts_start_empty_and_changes_are_audited() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let e = company(&mut c, "Northwind Holdings Pte. Ltd.", "SG", "SGD");
    let empty = entities::get_facts(&c, e).unwrap();
    assert_eq!((empty.legal_form.as_str(), empty.fiscal_year_end.as_str(), empty.incorporated_on), ("", "", None));
    assert!(empty.registrations.is_empty());

    let patch = EntityFactsPatch {
        legal_form: Some(" Private company limited by shares ".into()),
        fiscal_year_end: Some("12-31".into()),
        incorporated_on: Some("2020-03-15".into()),
        registrations: vec![entities::parse_pair("uen=202000001X").unwrap(), entities::parse_pair("GST = M90000001X").unwrap()],
    };
    let facts = entities::set_facts(&mut c, e, patch).unwrap();
    assert_eq!(facts.legal_form, "Private company limited by shares");
    assert_eq!(facts.fiscal_year_end, "12-31");
    assert_eq!(facts.incorporated_on, Some(d("2020-03-15")));
    assert_eq!(facts.registrations.get("UEN").map(String::as_str), Some("202000001X"));
    assert_eq!(facts.registrations.get("GST").map(String::as_str), Some("M90000001X"));
    assert_eq!(entities::get_facts(&c, e).unwrap(), facts);

    // An empty value removes the key. Other facts stay.
    let facts = entities::set_facts(&mut c, e, EntityFactsPatch { registrations: vec![("gst".into(), String::new())], ..Default::default() }).unwrap();
    assert_eq!(facts.registrations.len(), 1);
    assert_eq!(facts.fiscal_year_end, "12-31");

    let history: Vec<_> = audit::history(&c, "entities", e).unwrap().into_iter().filter(|r| r.action == "set_facts").collect();
    assert_eq!(history.len(), 2);
    let before: serde_json::Value = serde_json::from_str(history[1].before.as_deref().unwrap()).unwrap();
    let after: serde_json::Value = serde_json::from_str(history[1].after.as_deref().unwrap()).unwrap();
    assert_eq!(before["registrations"]["GST"], "M90000001X");
    assert!(after["registrations"].get("GST").is_none());

    // A patch that changes nothing writes no audit row.
    entities::set_facts(&mut c, e, EntityFactsPatch::default()).unwrap();
    assert_eq!(audit::history(&c, "entities", e).unwrap().iter().filter(|r| r.action == "set_facts").count(), 2);
}

#[test]
fn fiscal_year_end_and_pairs_are_validated() {
    for bad in ["13-01", "12-32", "1231", "2-28", "02-30", "ab-cd", "12-31-01"] {
        assert!(matches!(entities::normalize_fiscal_year_end(bad), Err(Error::Invalid(_))), "{bad} must fail");
    }
    assert_eq!(entities::normalize_fiscal_year_end("02-29").unwrap(), "02-29");
    assert_eq!(entities::normalize_fiscal_year_end(" ").unwrap(), "");
    assert!(entities::parse_pair("NOEQUALS").is_err());
    assert!(entities::parse_pair("=value").is_err());

    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let e = company(&mut c, "Blue Harbor LLC", "US", "USD");
    let err = entities::set_facts(&mut c, e, EntityFactsPatch { fiscal_year_end: Some("06-31".into()), ..Default::default() }).unwrap_err();
    assert!(matches!(err, Error::Invalid(_)));
    let err = entities::set_facts(&mut c, e, EntityFactsPatch { incorporated_on: Some("2021-02-30".into()), ..Default::default() }).unwrap_err();
    assert!(matches!(err, Error::Invalid(_)));
    let facts = entities::set_facts(&mut c, e, EntityFactsPatch { incorporated_on: Some("2021-02-01".into()), ..Default::default() }).unwrap();
    assert_eq!(facts.incorporated_on, Some(d("2021-02-01")));
    let facts = entities::set_facts(&mut c, e, EntityFactsPatch { incorporated_on: Some(String::new()), ..Default::default() }).unwrap();
    assert_eq!(facts.incorporated_on, None);
    assert!(matches!(entities::get_facts(&c, 999), Err(Error::NotFound(_))));
}

#[test]
fn migration_gives_existing_entities_empty_facts() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    db::migrate_to(&c, 16).unwrap();
    c.execute("INSERT INTO entities (uid, name, kind, country, currency, created_at, updated_at) VALUES ('u-1', 'Rio Verde SL', 'company', 'ES', 'EUR', 't', 't')", []).unwrap();
    db::migrate_to(&c, i64::MAX).unwrap();
    let version: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(version, db::latest_version());
    let facts = entities::get_facts(&c, 1).unwrap();
    assert_eq!(facts.legal_form, "");
    assert_eq!(facts.fiscal_year_end, "");
    assert_eq!(facts.incorporated_on, None);
    assert!(facts.registrations.is_empty());
    assert!(entities::list_classifications(&c, 1, None).unwrap().is_empty());
}

#[test]
fn classifications_follow_as_of_dates() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let e = company(&mut c, "Blue Harbor LLC", "US", "USD");
    let first = entities::add_classification(&mut c, e, "us", "Disregarded_Entity", d("2021-01-01"), None, "Single member").unwrap();
    assert_eq!((first.jurisdiction.as_str(), first.classification.as_str()), ("US", "disregarded_entity"));
    // A later election closes the open classification on its start date.
    let second = entities::add_classification(&mut c, e, "US", "corporation", d("2023-01-01"), None, "").unwrap();
    entities::add_classification(&mut c, e, "SG", "company", d("2022-05-01"), None, "").unwrap();

    let closed = entities::get_classification(&c, first.id).unwrap();
    assert_eq!(closed.effective_to, Some(d("2023-01-01")));

    let on = |c: &Connection, date: &str| entities::list_classifications(c, e, Some(d(date))).unwrap().into_iter().map(|x| format!("{}:{}", x.jurisdiction, x.classification)).collect::<Vec<_>>();
    assert_eq!(on(&c, "2020-12-31"), Vec::<String>::new());
    assert_eq!(on(&c, "2022-12-31"), vec!["SG:company", "US:disregarded_entity"]);
    // effective_to is exclusive.
    assert_eq!(on(&c, "2023-01-01"), vec!["SG:company", "US:corporation"]);

    // A period inside a closed classification is a conflict.
    let err = entities::add_classification(&mut c, e, "US", "partnership", d("2022-06-01"), Some(d("2022-09-01")), "").unwrap_err();
    assert!(matches!(err, Error::Conflict(_)));
    // A closed period before all others is accepted.
    entities::add_classification(&mut c, e, "US", "partnership", d("2020-01-01"), Some(d("2021-01-01")), "").unwrap();
    assert!(matches!(entities::add_classification(&mut c, e, "US", "x", d("2024-01-01"), Some(d("2024-01-01")), ""), Err(Error::Invalid(_))));
    assert!(matches!(entities::add_classification(&mut c, e, "U S", "x", d("2024-01-01"), None, ""), Err(Error::Invalid(_))));

    let err = entities::close_classification(&mut c, second.id, d("2022-12-31")).unwrap_err();
    assert!(matches!(err, Error::Invalid(_)));
    let ended = entities::close_classification(&mut c, second.id, d("2025-01-01")).unwrap();
    assert_eq!(ended.effective_to, Some(d("2025-01-01")));
    assert!(matches!(entities::close_classification(&mut c, second.id, d("2026-01-01")), Err(Error::Conflict(_))));

    let history = audit::history(&c, "entity_classifications", first.id).unwrap();
    assert_eq!(history.iter().map(|r| r.action.as_str()).collect::<Vec<_>>(), vec!["create", "close"]);
    assert!(history[1].before.as_deref().unwrap().contains("\"effective_to\":null"));
    assert!(history[1].after.as_deref().unwrap().contains("2023-01-01"));
}

#[test]
fn group_tree_reports_ownership_as_of_a_date() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let holdings = company(&mut c, "Northwind Holdings Pte. Ltd.", "SG", "SGD");
    let harbor = company(&mut c, "Blue Harbor LLC", "US", "USD");
    let verde = company(&mut c, "Rio Verde SL", "ES", "EUR");
    let group = groups::create_group(&mut c, "Northwind Group").unwrap();
    assert!(matches!(groups::create_group(&mut c, "northwind group"), Err(Error::Conflict(_))));
    assert_eq!(groups::find_group(&c, &group.uid).unwrap(), group);
    assert_eq!(groups::find_group(&c, "NORTHWIND GROUP").unwrap(), group);

    groups::add_member(&mut c, NewMember::new(group.id, holdings, d("2020-01-01"))).unwrap();
    groups::add_member(&mut c, NewMember { parent_entity_id: Some(holdings), ..NewMember::new(group.id, harbor, d("2021-01-01")) }).unwrap();
    groups::add_member(&mut c, NewMember { parent_entity_id: Some(harbor), ownership_bp: 6000, control: true, ..NewMember::new(group.id, verde, d("2024-01-01")) }).unwrap();

    let before = groups::tree(&c, group.id, d("2023-06-01")).unwrap();
    assert_eq!(before.roots.len(), 1);
    assert_eq!(before.roots[0].entity.id, holdings);
    assert_eq!(before.roots[0].children.len(), 1);
    assert!(before.roots[0].children[0].children.is_empty());

    let after = groups::tree(&c, group.id, d("2024-06-01")).unwrap();
    let harbor_node = &after.roots[0].children[0];
    assert_eq!(harbor_node.entity.id, harbor);
    assert_eq!(harbor_node.effective_ownership_bp, 10_000);
    let verde_node = &harbor_node.children[0];
    assert_eq!(verde_node.entity.id, verde);
    assert_eq!(verde_node.member.ownership_bp, 6000);
    assert_eq!(verde_node.effective_ownership_bp, 6000);
    assert_eq!(groups::membership_of(&c, verde, d("2024-06-01")).unwrap().unwrap().group_id, group.id);
    assert!(groups::membership_of(&c, verde, d("2023-12-31")).unwrap().is_none());
}

#[test]
fn an_entity_is_in_one_group_at_a_time() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let holdings = company(&mut c, "Northwind Holdings Pte. Ltd.", "SG", "SGD");
    let harbor = company(&mut c, "Blue Harbor LLC", "US", "USD");
    let first = groups::create_group(&mut c, "Northwind Group").unwrap();
    let second = groups::create_group(&mut c, "Harbor Group").unwrap();
    groups::add_member(&mut c, NewMember::new(first.id, holdings, d("2020-01-01"))).unwrap();
    let child = groups::add_member(&mut c, NewMember { parent_entity_id: Some(holdings), ..NewMember::new(first.id, harbor, d("2021-01-01")) }).unwrap();

    assert!(matches!(groups::add_member(&mut c, NewMember::new(second.id, harbor, d("2022-01-01"))), Err(Error::Conflict(_))));
    assert!(matches!(groups::add_member(&mut c, NewMember::new(first.id, harbor, d("2022-01-01"))), Err(Error::Conflict(_))));
    // A parent must be an open member of the same group.
    let verde = company(&mut c, "Rio Verde SL", "ES", "EUR");
    assert!(matches!(groups::add_member(&mut c, NewMember { parent_entity_id: Some(holdings), ..NewMember::new(second.id, verde, d("2022-01-01")) }), Err(Error::Invalid(_))));
    assert!(matches!(groups::add_member(&mut c, NewMember { parent_entity_id: Some(holdings), ..NewMember::new(first.id, verde, d("2019-01-01")) }), Err(Error::Invalid(_))));
    assert!(matches!(groups::add_member(&mut c, NewMember { ownership_bp: 10_001, ..NewMember::new(second.id, verde, d("2022-01-01")) }), Err(Error::Invalid(_))));

    // A parent with an open child cannot close.
    assert!(matches!(groups::close_member(&mut c, first.id, holdings, d("2025-01-01")), Err(Error::Conflict(_))));
    let closed = groups::close_member(&mut c, first.id, harbor, d("2025-01-01")).unwrap();
    assert_eq!(closed.effective_to, Some(d("2025-01-01")));
    assert!(matches!(groups::close_member(&mut c, first.id, harbor, d("2026-01-01")), Err(Error::NotFound(_))));
    // The move starts on the day the old membership ends.
    assert!(matches!(groups::add_member(&mut c, NewMember::new(second.id, harbor, d("2024-12-31"))), Err(Error::Conflict(_))));
    groups::add_member(&mut c, NewMember::new(second.id, harbor, d("2025-01-01"))).unwrap();
    assert_eq!(groups::membership_of(&c, harbor, d("2024-12-31")).unwrap().unwrap().group_id, first.id);
    assert_eq!(groups::membership_of(&c, harbor, d("2025-01-01")).unwrap().unwrap().group_id, second.id);
    assert!(groups::tree(&c, first.id, d("2025-01-01")).unwrap().roots[0].children.is_empty());

    let history = audit::history(&c, "entity_group_members", child.id).unwrap();
    assert_eq!(history.iter().map(|r| r.action.as_str()).collect::<Vec<_>>(), vec!["add", "close"]);
    assert_eq!(audit::history(&c, "entity_groups", first.id).unwrap().len(), 1);
}
