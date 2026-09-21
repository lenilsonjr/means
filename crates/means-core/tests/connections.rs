use means_core::{
    accounts,
    connections::{self, ConnectionAccount},
    entities, AccountType, Db,
};
#[test]
fn discovery_preserves_routes_cutoffs_and_watermarks_and_mapping_is_audited() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let e = entities::create_entity(&mut c, "Personal", "person", "PT", "EUR").unwrap();
    let bank = accounts::ensure_account(&c, e.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
    let a = ConnectionAccount { provider_account_id: "a".into(), provider_type: "BANK".into(), name: "Checking".into(), currency: "EUR".into(), ..Default::default() };
    connections::discover(&c, "pluggy", "item", std::slice::from_ref(&a)).unwrap();
    let id = connections::list(&c).unwrap()[0].id;
    connections::configure(&mut c, id, Some(bank.id), "2026-09-01").unwrap();
    c.execute("UPDATE channel_connections SET cursor='saved',last_pull_at='before' WHERE id=?1", [id]).unwrap();
    connections::discover(&c, "pluggy", "item", &[ConnectionAccount { name: "Renamed".into(), ..a }]).unwrap();
    let a = connections::get(&c, id).unwrap();
    assert_eq!(a.name, "Renamed");
    assert_eq!(a.booked_from, "2026-09-01");
    assert_eq!(a.account_id, Some(bank.id));
    assert_eq!(a.last_pull_at, "before");
    assert_eq!(a.entity_id, Some(e.id));
    assert_eq!(c.query_row("SELECT cursor FROM channel_connections WHERE id=?1", [id], |r| r.get::<_, String>(0)).unwrap(), "saved");
    c.execute_batch("CREATE TRIGGER fail_mapping BEFORE INSERT ON audit_log WHEN NEW.table_name='channel_connections' BEGIN SELECT RAISE(ABORT,'test');END;").unwrap();
    assert!(connections::configure(&mut c, id, None, "").is_err());
    assert_eq!(connections::get(&c, id).unwrap().account_id, Some(bank.id));
    c.execute_batch("DROP TRIGGER fail_mapping").unwrap();
    connections::configure(&mut c, id, None, "").unwrap();
    assert!(connections::get(&c, id).unwrap().account_id.is_none());
    assert_eq!(c.query_row("SELECT COUNT(*) FROM audit_log WHERE table_name='channel_connections'", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
}
#[test]
fn rejects_wrong_currency_type_closed_accounts_and_invalid_cutoffs() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let e = entities::create_entity(&mut c, "Personal", "person", "PT", "EUR").unwrap();
    let bank = accounts::ensure_account(&c, e.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
    let usd = accounts::ensure_account(&c, e.id, AccountType::Asset, &["USD"], "bank", "USD").unwrap();
    let expense = accounts::ensure_account(&c, e.id, AccountType::Expense, &["Food"], "expense", "EUR").unwrap();
    connections::discover(&c, "mercury", "", &[ConnectionAccount { provider_account_id: "a".into(), provider_type: "credit".into(), currency: "USD".into(), ..Default::default() }]).unwrap();
    let id = connections::list(&c).unwrap()[0].id;
    for target in [bank.id, usd.id, expense.id] {
        assert!(connections::configure(&mut c, id, Some(target), "").is_err());
    }
    assert!(connections::configure(&mut c, id, None, "invalid").is_err());
    assert_eq!(connections::configure(&mut c, id, None, "2026-09-01").unwrap().booked_from, "2026-09-01");
    let card = accounts::ensure_account(&c, e.id, AccountType::Liability, &["Card"], "credit_card", "USD").unwrap();
    connections::configure(&mut c, id, Some(card.id), "").unwrap();
    accounts::close_account(&c, card.id, false).unwrap();
    assert!(connections::configure(&mut c, id, Some(card.id), "").is_err());
}
