//! Entity address and flags, account identifiers, party residence, and their snapshot fields.
//! All names, numbers and dates are fictional.

use std::path::PathBuf;

use chrono::NaiveDate;
use means_core::accounts::{self, AccountUpdate, ChartNode};
use means_core::entity_flags::{self, FlagChange};
use means_core::parties::{self, PartyFactsPatch};
use means_core::snapshot::{self, SnapshotOptions};
use means_core::{audit, close_questions, db, entities, journal, payees, AccountType, Db, EntryInput, Error, PostingInput};
use rusqlite::Connection;
use serde_json::{json, Value};

fn d(s: &str) -> NaiveDate {
    means_core::parse_date(s).unwrap()
}

fn company(c: &mut Connection, name: &str, country: &str, currency: &str) -> i64 {
    entities::create_entity(c, name, "company", country, currency).unwrap().id
}

fn flag(key: &str, value: &str, from: Option<&str>) -> FlagChange {
    FlagChange { key: key.into(), value: value.into(), from: from.map(d) }
}

fn payee(c: &mut Connection, entity: i64, name: &str) -> i64 {
    let change = payees::Change { id: 0, entity_id: entity, name: name.into(), active: true, aliases: vec![] };
    let preview = payees::save(c, change.clone(), None).unwrap();
    payees::save(c, change, Some(&preview.token)).unwrap().payee.unwrap().id
}

#[test]
fn flags_keep_dated_history_and_read_as_of_a_date() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let e = company(&mut c, "Northwind Holdings Pte. Ltd.", "SG", "SGD");
    assert!(entity_flags::current_flags(&c, e).unwrap().is_empty());

    // An undated flag applies on every date. Values become scalars.
    entity_flags::set_flags(&mut c, e, &[flag("SG.AGM_Dispensed", "true", None), flag("sg.gst_period", "quarterly", None), flag("pt.special_regime_first_year", "2021", None)]).unwrap();
    let old = entity_flags::flags_as_of(&c, e, d("1999-01-01")).unwrap();
    assert_eq!(old.get("sg.agm_dispensed"), Some(&json!(true)));
    assert_eq!(old.get("sg.gst_period"), Some(&json!("quarterly")));
    assert_eq!(old.get("pt.special_regime_first_year"), Some(&json!(2021)));

    // A dated flag: not registered, then registered from a date.
    entity_flags::set_flags(&mut c, e, &[flag("sg.gst_registered", "false", Some("2022-01-01"))]).unwrap();
    let now = entity_flags::set_flags(&mut c, e, &[flag("sg.gst_registered", "true", Some("2024-07-01")), flag("sg.gst_registration_date", "2024-07-01", Some("2024-07-01"))]).unwrap();
    assert_eq!(now.get("sg.gst_registered"), Some(&json!(true)));
    assert_eq!(now.get("sg.gst_registration_date"), Some(&json!("2024-07-01")));
    let on = |c: &Connection, date: &str, key: &str| entity_flags::flags_as_of(c, e, d(date)).unwrap().get(key).cloned();
    assert_eq!(on(&c, "2021-12-31", "sg.gst_registered"), None);
    assert_eq!(on(&c, "2023-06-30", "sg.gst_registered"), Some(json!(false)));
    assert_eq!(on(&c, "2024-06-30", "sg.gst_registered"), Some(json!(false)));
    assert_eq!(on(&c, "2024-07-01", "sg.gst_registered"), Some(json!(true)));
    let history: Vec<_> = entity_flags::flag_history(&c, e).unwrap().into_iter().filter(|p| p.key == "sg.gst_registered").collect();
    assert_eq!(history.len(), 2);
    assert_eq!((history[0].effective_from, history[0].effective_to), (d("2022-01-01"), Some(d("2024-07-01"))));
    assert_eq!((history[1].effective_from, history[1].effective_to), (d("2024-07-01"), None));

    // An overlap with a closed period and an undated change of a dated key are conflicts.
    assert!(matches!(entity_flags::set_flags(&mut c, e, &[flag("sg.gst_registered", "true", Some("2023-01-01"))]), Err(Error::Conflict(_))));
    assert!(matches!(entity_flags::set_flags(&mut c, e, &[flag("sg.gst_registered", "false", None)]), Err(Error::Conflict(_))));
    assert!(matches!(entity_flags::set_flags(&mut c, e, &[flag("bad key!", "1", None)]), Err(Error::Invalid(_))));
    // A failed change writes nothing.
    assert_eq!(entity_flags::flag_history(&c, e).unwrap().len(), 3);

    // An empty value with a date ends the open period. An empty undated value removes the flag.
    let now = entity_flags::set_flags(&mut c, e, &[flag("sg.gst_registered", "", Some("2025-01-01")), flag("sg.gst_period", "", None)]).unwrap();
    assert!(!now.contains_key("sg.gst_registered") && !now.contains_key("sg.gst_period"));
    assert_eq!(on(&c, "2024-12-31", "sg.gst_registered"), Some(json!(true)));
    assert_eq!(on(&c, "2025-01-01", "sg.gst_registered"), None);

    // A value that starts after today is in the history, not in the current flags.
    let now = entity_flags::set_flags(&mut c, e, &[flag("sg.publicly_accountable", "true", Some("2099-01-01"))]).unwrap();
    assert!(!now.contains_key("sg.publicly_accountable"));
    assert_eq!(on(&c, "2099-01-01", "sg.publicly_accountable"), Some(json!(true)));

    // Every change is audited with its values before and after.
    let events = audit::history(&c, "entities", e).unwrap();
    assert!(events.iter().filter(|r| r.action == "set_flags").all(|r| r.before.is_some() && r.after.is_some()));
    assert!(events.iter().filter(|r| r.action == "set_flags").count() >= 4);
    let closed = history[0].id;
    let period_events: Vec<_> = audit::history(&c, "entity_flag_history", closed).unwrap().into_iter().map(|r| r.action).collect();
    assert_eq!(period_events, ["create", "close"]);
}

#[test]
fn the_address_accepts_known_keys_and_a_country_code() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let e = company(&mut c, "Blue Harbor LLC", "US", "USD");
    assert!(entity_flags::get_address(&c, e).unwrap().is_empty());
    let pairs: Vec<_> = ["street=1 Example Way", "City=Springfield", "region=DE", "postal_code=19901", "country=us"].iter().map(|s| entity_flags::parse_address_pair(s).unwrap()).collect();
    let address = entity_flags::set_address(&mut c, e, &pairs).unwrap();
    assert_eq!(address.get("city").map(String::as_str), Some("Springfield"));
    assert_eq!(address.get("country").map(String::as_str), Some("US"));

    for bad in ["planet=Mars", "country=USA", "country=1A"] {
        let pair = entity_flags::parse_address_pair(bad).unwrap();
        assert!(matches!(entity_flags::set_address(&mut c, e, &[pair]), Err(Error::Invalid(_))), "{bad} must fail");
    }
    assert!(entity_flags::parse_address_pair("no equals sign").is_err());

    let address = entity_flags::set_address(&mut c, e, &[("street2".into(), "Suite 5".into()), ("region".into(), String::new())]).unwrap();
    assert_eq!(address.get("street2").map(String::as_str), Some("Suite 5"));
    assert!(!address.contains_key("region"));
    assert_eq!(entity_flags::get_address(&c, e).unwrap(), address);
    let events: Vec<_> = audit::history(&c, "entities", e).unwrap().into_iter().filter(|r| r.action == "set_address").collect();
    assert_eq!(events.len(), 2);
    assert!(events[1].before.as_deref().unwrap().contains("\"region\":\"DE\""));
}

#[test]
fn account_identifiers_round_trip_and_come_from_a_chart() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let e = company(&mut c, "Rio Verde SL", "ES", "EUR");
    let bank = accounts::ensure_account(&c, e, AccountType::Asset, &["Bank", "Operating"], "bank", "EUR").unwrap();
    assert_eq!((bank.country.as_str(), bank.iban.as_str(), bank.bic.as_str(), bank.institution.as_str(), bank.location.as_str()), ("", "", "", "", ""));

    let upd = AccountUpdate {
        country: Some("de".into()),
        iban: Some("de89 3704 0044 0532 0130 00".into()),
        bic: Some("abcdsgsg".into()),
        institution: Some(" Example Bank ".into()),
        location: Some("Delaware".into()),
        ..Default::default()
    };
    let saved = accounts::update_account(&c, bank.id, upd).unwrap();
    let read = accounts::get_account(&c, bank.id).unwrap();
    assert_eq!((read.country.as_str(), read.iban.as_str(), read.bic.as_str()), ("DE", "DE89370400440532013000", "ABCDSGSG"));
    assert_eq!((read.institution.as_str(), read.location.as_str()), ("Example Bank", "Delaware"));
    assert_eq!(saved.iban, read.iban);
    let listed = accounts::list_accounts_with_balances(&c, Some(e), false, None).unwrap().into_iter().find(|a| a.id == bank.id).unwrap();
    assert_eq!(listed.iban, read.iban);

    for bad in ["DE00370400440532013000", "DE89", "12345678901234567"] {
        assert!(matches!(accounts::update_account(&c, bank.id, AccountUpdate { iban: Some(bad.into()), ..Default::default() }), Err(Error::Invalid(_))), "{bad} must fail");
    }
    for bad in ["ABCD", "1234SGSG", "ABCDSGSGX"] {
        assert!(matches!(accounts::update_account(&c, bank.id, AccountUpdate { bic: Some(bad.into()), ..Default::default() }), Err(Error::Invalid(_))), "{bad} must fail");
    }
    assert!(matches!(accounts::update_account(&c, bank.id, AccountUpdate { country: Some("DEU".into()), ..Default::default() }), Err(Error::Invalid(_))));
    // The audit log keeps the account before and after.
    let last = audit::history(&c, "accounts", bank.id).unwrap().pop().unwrap();
    assert!(last.after.unwrap().contains("DE89370400440532013000"));

    // An empty value clears one identifier. The others stay.
    let cleared = accounts::update_account(&c, bank.id, AccountUpdate { iban: Some(String::new()), ..Default::default() }).unwrap();
    assert_eq!((cleared.iban.as_str(), cleared.bic.as_str()), ("", "ABCDSGSG"));

    // A chart node can carry identifiers. A node without them keeps the stored values.
    let nodes: Vec<ChartNode> = serde_json::from_str(
        r#"[{"code": "1200", "path": "Assets:Brokerage", "type": "asset", "placeholder": false, "country": "US", "institution": "Example Brokerage", "location": "Texas"},
            {"code": "1210", "path": "Assets:Bank:Operating", "type": "asset", "placeholder": false}]"#,
    )
    .unwrap();
    accounts::apply_chart(&mut c, e, &nodes).unwrap();
    let brokerage = accounts::find_by_path(&c, e, "Brokerage").unwrap().unwrap();
    assert_eq!((brokerage.country.as_str(), brokerage.institution.as_str(), brokerage.location.as_str()), ("US", "Example Brokerage", "Texas"));
    assert_eq!(accounts::get_account(&c, bank.id).unwrap().bic, "ABCDSGSG");
}

#[test]
fn party_residence_and_missing_tax_ids() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let e = company(&mut c, "Northwind Holdings Pte. Ltd.", "SG", "SGD");
    let supplier = payee(&mut c, e, "Harbor Supplies Pte. Ltd.");
    let registered = payee(&mut c, e, "Lumen Office Services");
    let _idle = payee(&mut c, e, "Idle Partner");

    let facts = parties::get_facts(&c, supplier).unwrap();
    assert_eq!((facts.residence.as_str(), facts.citizenship.as_str()), ("", ""));
    let facts = parties::set_facts(&mut c, supplier, PartyFactsPatch { residence: Some("sg".into()), citizenship: Some("my".into()), ..Default::default() }).unwrap();
    assert_eq!((facts.residence.as_str(), facts.citizenship.as_str()), ("SG", "MY"));
    assert_eq!(parties::get_facts(&c, supplier).unwrap(), facts);
    assert!(matches!(parties::set_facts(&mut c, supplier, PartyFactsPatch { residence: Some("SGP".into()), ..Default::default() }), Err(Error::Invalid(_))));
    let facts = parties::set_facts(&mut c, supplier, PartyFactsPatch { citizenship: Some(String::new()), ..Default::default() }).unwrap();
    assert_eq!((facts.residence.as_str(), facts.citizenship.as_str()), ("SG", ""));
    parties::set_facts(&mut c, registered, PartyFactsPatch { tax_ids: vec![("GST".into(), "M90000002X".into())], ..Default::default() }).unwrap();

    let bank = accounts::ensure_account(&c, e, AccountType::Asset, &["Bank"], "bank", "SGD").unwrap();
    let cost = accounts::ensure_account(&c, e, AccountType::Expense, &["Supplies"], "expense", "SGD").unwrap();
    for (who, on) in [("Harbor Supplies Pte. Ltd.", "2025-03-01"), ("Harbor Supplies Pte. Ltd.", "2025-04-01"), ("Lumen Office Services", "2025-03-15")] {
        let mut input = EntryInput::new(e, d(on));
        input.payee = who.into();
        input.postings = vec![PostingInput::new(bank.id, "-10".parse().unwrap()), PostingInput::balancing(cost.id)];
        journal::create_entry(&mut c, input).unwrap();
    }
    let missing = parties::missing_tax_id(&c, e, "gst").unwrap();
    assert_eq!(missing.len(), 1, "only the active payee without a GST number");
    assert_eq!(missing[0].payee.id, supplier);
    assert_eq!((missing[0].entry_count, missing[0].invoice_count), (2, 0));
    assert_eq!(missing[0].last_date.as_deref(), Some("2025-04-01"));
    assert_eq!(parties::missing_tax_id(&c, e, "VAT").unwrap().len(), 2);
    assert!(matches!(parties::missing_tax_id(&c, e, " "), Err(Error::Invalid(_))));
}

#[test]
fn year_end_questions_list_answered_and_missing_flags() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let e = company(&mut c, "Northwind Holdings Pte. Ltd.", "SG", "SGD");
    entity_flags::set_flags(&mut c, e, &[flag("sg.gst_registered", "true", Some("2024-07-01")), flag("sg.custom_note", "yes", None), flag("us.trade_or_business", "false", None)]).unwrap();
    let q = close_questions::questionnaire(&c, e, "sg", d("2025-12-31")).unwrap();
    assert_eq!(q.jurisdiction, "SG");
    let answered: Vec<_> = q.answered.iter().map(|a| (a.key.as_str(), a.question.is_some())).collect();
    assert_eq!(answered, [("sg.custom_note", false), ("sg.gst_registered", true)]);
    assert!(q.missing.iter().any(|m| m.key == "sg.agm_dispensed"));
    assert!(!q.missing.iter().any(|m| m.key == "sg.gst_registered"));
    // Before the dated value, the question is open again.
    let early = close_questions::questionnaire(&c, e, "SG", d("2024-01-01")).unwrap();
    assert!(early.missing.iter().any(|m| m.key == "sg.gst_registered"));
    assert!(matches!(close_questions::questionnaire(&c, e, "XX", d("2025-12-31")), Err(Error::Invalid(_))));
    // Every list names keys of its own jurisdiction.
    for (code, questions) in close_questions::all().unwrap() {
        let prefix = format!("{}.", code.to_ascii_lowercase());
        assert!(questions.iter().all(|q| q.key.starts_with(&prefix) && entity_flags::normalize_key(&q.key).unwrap() == q.key), "{code}");
    }
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_snapshot_carries_address_flags_identifiers_and_residence() {
    let tmp = TempDir(std::env::temp_dir().join(format!("means-entity-flags-{}-{}", std::process::id(), means_core::new_uid())));
    std::fs::create_dir_all(&tmp.0).unwrap();
    let db = Db::open(tmp.0.join("ledger.db")).unwrap();
    let mut c = db.conn();
    let e = company(&mut c, "Blue Harbor LLC", "US", "USD");
    entity_flags::set_address(&mut c, e, &[("city".into(), "Springfield".into()), ("country".into(), "US".into())]).unwrap();
    entity_flags::set_flags(&mut c, e, &[flag("us.contractor_payments", "true", None), flag("us.trade_or_business", "true", Some("2023-01-01"))]).unwrap();
    let bank = accounts::ensure_account(&c, e, AccountType::Asset, &["Bank"], "bank", "USD").unwrap();
    accounts::update_account(&c, bank.id, AccountUpdate { country: Some("US".into()), institution: Some("Example Bank".into()), location: Some("Texas".into()), ..Default::default() }).unwrap();
    let p = payee(&mut c, e, "Lumen Consulting");
    parties::set_facts(&mut c, p, PartyFactsPatch { residence: Some("CA".into()), citizenship: Some("GB".into()), ..Default::default() }).unwrap();

    let out = tmp.0.join("snapshot");
    snapshot::export(&mut c, &out, &SnapshotOptions::default()).unwrap();
    let read = |name: &str| -> Value { serde_json::from_slice(&std::fs::read(out.join(name)).unwrap()).unwrap() };
    let entity = &read("entities.json")[0];
    assert_eq!(entity["address"], json!({"city": "Springfield", "country": "US"}));
    assert_eq!(entity["flags"], json!({"us.contractor_payments": true, "us.trade_or_business": true}));
    assert_eq!(entity["flag_history"], json!([{"key": "us.trade_or_business", "value": true, "effective_from": "2023-01-01", "effective_to": null}]));
    let accounts_json = read("accounts.json");
    let row = accounts_json.as_array().unwrap().iter().find(|a| a["uid"] == json!(bank.uid)).unwrap();
    assert_eq!((&row["country"], &row["iban"], &row["bic"], &row["institution"], &row["location"]), (&json!("US"), &json!(""), &json!(""), &json!("Example Bank"), &json!("Texas")));
    let party = &read("parties.json")[0];
    assert_eq!((&party["residence"], &party["citizenship"]), (&json!("CA"), &json!("GB")));

    // With --as-of, the flags are the values on that date.
    let early = tmp.0.join("early");
    snapshot::export(&mut c, &early, &SnapshotOptions { as_of: Some(d("2022-12-31")), ..Default::default() }).unwrap();
    let entity: Value = serde_json::from_slice(&std::fs::read(early.join("entities.json")).unwrap()).unwrap();
    assert_eq!(entity[0]["flags"], json!({"us.contractor_payments": true}));
    assert!(snapshot::verify(&out).unwrap().problems.is_empty());
}

#[test]
fn migration_gives_existing_rows_empty_defaults() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    db::migrate_to(&c, 26).unwrap();
    c.execute("INSERT INTO entities (uid, name, kind, country, currency, created_at, updated_at) VALUES ('u-1', 'Rio Verde SL', 'company', 'ES', 'EUR', 't', 't')", []).unwrap();
    c.execute("INSERT INTO commodities (code, kind, name, precision) VALUES ('EUR', 'currency', 'Euro', 2) ON CONFLICT(code) DO NOTHING", []).unwrap();
    c.execute(
        "INSERT INTO accounts (uid, entity_id, name, type, subtype, commodity_id, created_at, updated_at) VALUES ('a-1', 1, 'Bank', 'asset', 'bank', (SELECT id FROM commodities WHERE code = 'EUR'), 't', 't')",
        [],
    )
    .unwrap();
    c.execute("INSERT INTO payees (uid, entity_id, name) VALUES ('p-1', 1, 'Harbor Supplies')", []).unwrap();
    db::migrate_to(&c, i64::MAX).unwrap();
    let version: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(version, db::schema_version());
    assert!(version >= 27);

    assert!(entity_flags::get_address(&c, 1).unwrap().is_empty());
    assert!(entity_flags::current_flags(&c, 1).unwrap().is_empty());
    assert!(entity_flags::flag_history(&c, 1).unwrap().is_empty());
    let account = accounts::get_account(&c, 1).unwrap();
    assert_eq!((account.country.as_str(), account.iban.as_str(), account.bic.as_str(), account.institution.as_str(), account.location.as_str()), ("", "", "", "", ""));
    let facts = parties::get_facts(&c, 1).unwrap();
    assert_eq!((facts.residence.as_str(), facts.citizenship.as_str()), ("", ""));
}
