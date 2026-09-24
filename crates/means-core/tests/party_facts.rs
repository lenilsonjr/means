use chrono::NaiveDate;
use means_core::parties::{self, NewRelationship, PartyFactsPatch};
use means_core::{audit, db, entities, payees, Db, Error};
use rusqlite::Connection;

fn d(s: &str) -> NaiveDate {
    means_core::parse_date(s).unwrap()
}

fn payee(c: &mut Connection, entity: i64, name: &str) -> i64 {
    let change = payees::Change { id: 0, entity_id: entity, name: name.into(), active: true, aliases: vec![] };
    let preview = payees::save(c, change.clone(), None).unwrap();
    payees::save(c, change, Some(&preview.token)).unwrap().payee.unwrap().id
}

fn relation(payee_id: i64, entity_id: i64, kind: &str, from: &str) -> NewRelationship {
    NewRelationship { payee_id, entity_id, kind: kind.into(), ownership_bp: None, effective_from: d(from), basis: String::new() }
}

#[test]
fn party_facts_are_validated_and_audited() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let e = entities::create_entity(&mut c, "Rio Verde SL", "company", "ES", "EUR").unwrap().id;
    let p = payee(&mut c, e, "Blue Harbor LLC");
    let facts = parties::get_facts(&c, p).unwrap();
    assert_eq!((facts.country.as_str(), facts.kind.as_str()), ("", "other"));
    assert!(facts.tax_ids.is_empty());

    for bad in ["SGP", "S", "1A"] {
        assert!(matches!(parties::set_facts(&mut c, p, PartyFactsPatch { country: Some(bad.into()), ..Default::default() }), Err(Error::Invalid(_))), "{bad} must fail");
    }
    assert!(matches!(parties::set_facts(&mut c, p, PartyFactsPatch { kind: Some("robot".into()), ..Default::default() }), Err(Error::Invalid(_))));

    let patch = PartyFactsPatch { country: Some("us".into()), tax_ids: vec![entities::parse_pair("ein=00-0000000").unwrap()], kind: Some("Company".into()), ..Default::default() };
    let facts = parties::set_facts(&mut c, p, patch).unwrap();
    assert_eq!(facts.country, "US");
    assert_eq!(facts.kind, "company");
    assert_eq!(facts.tax_ids.get("EIN").map(String::as_str), Some("00-0000000"));
    assert_eq!(parties::get_facts(&c, p).unwrap(), facts);
    // The name and aliases stay as they were.
    assert_eq!(payees::get(&c, p).unwrap().name, "Blue Harbor LLC");

    let history: Vec<_> = audit::history(&c, "payees", p).unwrap().into_iter().filter(|r| r.action == "set_facts").collect();
    assert_eq!(history.len(), 1);
    assert!(history[0].before.as_deref().unwrap().contains("\"kind\":\"other\""));
    assert!(history[0].after.as_deref().unwrap().contains("\"kind\":\"company\""));
}

#[test]
fn migration_gives_existing_payees_default_facts() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    db::migrate_to(&c, 16).unwrap();
    c.execute("INSERT INTO entities (uid, name, kind, country, currency, created_at, updated_at) VALUES ('u-1', 'Rio Verde SL', 'company', 'ES', 'EUR', 't', 't')", []).unwrap();
    c.execute("INSERT INTO payees (uid, entity_id, name) VALUES ('p-1', 1, 'Sample Person')", []).unwrap();
    db::migrate_to(&c, i64::MAX).unwrap();
    let facts = parties::get_facts(&c, 1).unwrap();
    assert_eq!((facts.country.as_str(), facts.kind.as_str()), ("", "other"));
    assert!(facts.tax_ids.is_empty());
    assert!(parties::list_relationships(&c, 1, None).unwrap().is_empty());
}

#[test]
fn related_parties_follow_as_of_dates() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let e = entities::create_entity(&mut c, "Rio Verde SL", "company", "ES", "EUR").unwrap().id;
    let person = payee(&mut c, e, "Sample Person");
    let customer = payee(&mut c, e, "Blue Harbor LLC");
    let parent = payee(&mut c, e, "Northwind Holdings Pte. Ltd.");

    let owner = parties::relate(&mut c, NewRelationship { ownership_bp: Some(4000), basis: "Share register".into(), ..relation(person, e, "owner", "2022-01-01") }).unwrap();
    parties::relate(&mut c, relation(person, e, "Director", "2022-01-01")).unwrap();
    parties::relate(&mut c, relation(customer, e, "customer", "2023-01-01")).unwrap();
    parties::relate(&mut c, NewRelationship { ownership_bp: Some(6000), ..relation(parent, e, "owner", "2022-01-01") }).unwrap();

    assert!(matches!(parties::relate(&mut c, relation(person, e, "owner", "2023-01-01")), Err(Error::Conflict(_))));
    assert!(matches!(parties::relate(&mut c, relation(person, e, "partner", "2023-01-01")), Err(Error::Invalid(_))));
    assert!(matches!(parties::relate(&mut c, NewRelationship { ownership_bp: Some(-1), ..relation(customer, e, "lender", "2023-01-01") }), Err(Error::Invalid(_))));

    let ended = parties::unrelate(&mut c, person, e, "owner", d("2024-07-01")).unwrap();
    assert_eq!(ended.effective_to, Some(d("2024-07-01")));
    assert!(matches!(parties::unrelate(&mut c, person, e, "owner", d("2025-01-01")), Err(Error::NotFound(_))));
    assert!(matches!(parties::unrelate(&mut c, customer, e, "customer", d("2022-01-01")), Err(Error::Invalid(_))));

    let related = |c: &Connection, date: &str| parties::related_parties(c, e, d(date)).unwrap().into_iter().map(|p| p.payee.name).collect::<Vec<_>>();
    assert_eq!(related(&c, "2021-12-31"), Vec::<String>::new());
    assert_eq!(related(&c, "2024-06-30"), vec!["Sample Person", "Northwind Holdings Pte. Ltd."]);
    // effective_to is exclusive, and the director relationship still holds.
    let july = parties::list_parties(&c, e, d("2024-07-01"), true).unwrap();
    let person_row = july.iter().find(|p| p.payee.id == person).unwrap();
    assert_eq!(person_row.relationships.iter().map(|r| r.kind.as_str()).collect::<Vec<_>>(), vec!["director"]);

    let all = parties::list_parties(&c, e, d("2024-07-01"), false).unwrap();
    assert_eq!(all.len(), 3);
    let customer_row = all.iter().find(|p| p.payee.id == customer).unwrap();
    assert!(!customer_row.related_party);
    assert_eq!(customer_row.relationships[0].kind, "customer");

    let history = audit::history(&c, "party_relationships", owner.id).unwrap();
    assert_eq!(history.iter().map(|r| r.action.as_str()).collect::<Vec<_>>(), vec!["relate", "unrelate"]);
    assert!(history[1].after.as_deref().unwrap().contains("2024-07-01"));
}

#[test]
fn relationships_stay_in_the_payee_entity() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let verde = entities::create_entity(&mut c, "Rio Verde SL", "company", "ES", "EUR").unwrap().id;
    let harbor = entities::create_entity(&mut c, "Blue Harbor LLC", "company", "US", "USD").unwrap().id;
    let person = payee(&mut c, verde, "Sample Person");
    payee(&mut c, harbor, "Sample Person");
    assert!(matches!(parties::relate(&mut c, relation(person, harbor, "director", "2024-01-01")), Err(Error::Invalid(_))));

    assert_eq!(parties::find_payee(&c, Some(verde), "sample  person").unwrap().id, person);
    assert_eq!(parties::find_payee(&c, None, &person.to_string()).unwrap().id, person);
    assert!(matches!(parties::find_payee(&c, None, "Sample Person"), Err(Error::Conflict(_))));
    assert!(matches!(parties::find_payee(&c, Some(verde), "Nobody"), Err(Error::NotFound(_))));
}
