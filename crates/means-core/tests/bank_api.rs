use means_core::{
    accounts, connections, entities,
    imports::{self, bank_api, inbox},
    AccountType, Db,
};
use serde_json::{json, Value};
fn wise() -> Value {
    json!({"channel":"wise","version":1,"account":{"id":"1:2","profile_id":1,"balance_id":2,"currency":"USD","name":"Wise USD"},"transactions":[{"referenceNumber":"CARD-1","type":"DEBIT","date":"2026-09-19T00:00:00Z","amount":{"value":-10.25,"currency":"USD"},"totalFees":{"value":0.25,"currency":"USD"},"details":{"description":"Coffee","merchant":{"name":"Cafe"},"amount":{"value":9.1,"currency":"EUR"}}}]})
}
fn inter() -> Value {
    json!({"channel":"inter_pj","version":1,"account":{"id":"0012345","currency":"BRL","name":"Inter PJ"},"transactions":[{"idTransacao":"pix-1","dataTransacao":"2026-09-19","dataInclusao":"2026-09-20","tipoOperacao":"C","valor":"2750.00","titulo":"Pix recebido","descricao":"Invoice 42","detalhes":{"nomePagador":"Example Software"}}]})
}
#[test]
fn native_movements_preserve_descriptions_fees_foreign_amounts_and_raw_evidence() {
    let w = wise();
    let parsed = bank_api::parse(&serde_json::to_vec(&w).unwrap()).unwrap();
    assert_eq!(parsed.account_ref, "1:2");
    assert_eq!(parsed.lines[0].amount.unwrap().to_string(), "-10.25");
    assert_eq!(parsed.lines[0].raw, w["transactions"][0]);
    assert!(parsed.lines[0].description.contains("Cafe"));
    let i = inter();
    let parsed = bank_api::parse(&serde_json::to_vec(&i).unwrap()).unwrap();
    assert_eq!(parsed.lines[0].amount.unwrap().to_string(), "2750.00");
    assert_eq!(parsed.lines[0].date.unwrap().to_string(), "2026-09-19");
    assert!(parsed.lines[0].description.contains("Example Software"));
}
#[test]
fn malformed_evidence_cannot_silently_round_change_currency_or_duplicate_references() {
    for change in ["currency", "sign", "date", "amount", "reference", "identity", "duplicate"] {
        let mut v = wise();
        match change {
            "currency" => v["transactions"][0]["amount"]["currency"] = json!("EUR"),
            "sign" => v["transactions"][0]["amount"]["value"] = json!(10.25),
            "date" => v["transactions"][0]["date"] = json!("bad"),
            "amount" => v["transactions"][0]["amount"]["value"] = json!(-10.255),
            "reference" => v["transactions"][0]["referenceNumber"] = json!(""),
            "identity" => v["account"]["id"] = json!("1:3"),
            "duplicate" => {
                let row = v["transactions"][0].clone();
                v["transactions"].as_array_mut().unwrap().push(row);
            }
            _ => unreachable!(),
        };
        assert!(bank_api::parse(&serde_json::to_vec(&v).unwrap()).is_err(), "{change}");
    }
    let mut v = inter();
    v["transactions"][0]["valor"] = json!("-2750.00");
    assert!(bank_api::parse(&serde_json::to_vec(&v).unwrap()).is_err());
}
#[test]
fn files_route_only_to_their_own_account_and_replays_do_not_create_postings() {
    for envelope in [wise(), inter()] {
        let db = Db::open_memory().unwrap();
        let mut c = db.conn();
        let currency = envelope["account"]["currency"].as_str().unwrap();
        let channel = envelope["channel"].as_str().unwrap();
        let entity = entities::create_entity(&mut c, "Business", "company", "US", currency).unwrap();
        let bank = accounts::ensure_account(&c, entity.id, AccountType::Asset, &["Bank"], "bank", currency).unwrap();
        let bytes = serde_json::to_vec(&envelope).unwrap();
        let source = bank_api::source(&bytes).unwrap();
        c.execute("INSERT INTO import_profiles(source,filename_glob,account_id,updated_at) VALUES(?1,'',?2,'before')", means_core::rusqlite::params![source, bank.id]).unwrap();
        assert_eq!(bank_api::account_for_file(&c, &bytes).unwrap(), None);
        assert_eq!(inbox::resolve_account(&c, source, "anything.json").unwrap(), None);
        let first = imports::run_import(&mut c, imports::ImportRequest::new(source, Some(bank.id), "first.json", &bytes)).unwrap();
        assert_eq!(first.import.created_count, 1);
        assert_eq!(bank_api::account_for_file(&c, &bytes).unwrap(), Some(bank.id));
        let mut repeated = envelope.clone();
        repeated["pulled_at"] = json!("later");
        let second = imports::run_import(&mut c, imports::ImportRequest::new(source, Some(bank.id), "second.json", &serde_json::to_vec(&repeated).unwrap())).unwrap();
        assert_eq!(second.import.created_count, 0);
        assert_eq!(second.import.duplicate_count, 1);
        let mut other = envelope.clone();
        other["account"]["id"] = if channel == "wise" { json!("1:3") } else { json!("99999") };
        if channel == "wise" {
            other["account"]["balance_id"] = json!(3)
        };
        assert_eq!(bank_api::account_for_file(&c, &serde_json::to_vec(&other).unwrap()).unwrap(), None);
        let connection = connections::list(&c).unwrap().remove(0);
        connections::configure(&mut c, connection.id, Some(bank.id), "2026-09-19").unwrap();
        let root = std::env::temp_dir().join(format!("means-bank-api-{}", means_core::new_uid()));
        assert!(bank_api::publish(&mut c, &root, &envelope, true).unwrap().is_none());
        assert!(!root.exists());
        let file = bank_api::publish(&mut c, &root, &envelope, false).unwrap().unwrap();
        assert_eq!(connections::get(&c, connection.id).unwrap().booked_from, "2026-09-19");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(file).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
