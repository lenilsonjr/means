use chrono::NaiveDate;
use means_core::{
    accounts,
    budgets::{self, BudgetInput},
    entities, journal, rates, tags, AccountType, Db, EntryInput, EntryStatus, PostingInput,
};
use rust_decimal::Decimal;
fn day(s: &str) -> NaiveDate {
    s.parse().unwrap()
}
fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}
fn input(entity: i64, account: i64) -> BudgetInput {
    BudgetInput {
        entity_id: entity,
        name: "Food".into(),
        scope: "category".into(),
        account_id: Some(account),
        class: None,
        tag: None,
        starts_on: Some(day("2026-09-01")),
        ends_on: Some(day("2026-11-30")),
        amount: d("600"),
    }
}
struct Fixture {
    db: Db,
    e: i64,
    bank: i64,
    parent: i64,
    food: i64,
    other: i64,
}
fn setup() -> Fixture {
    let db = Db::open_memory().unwrap();
    let (e, bank, parent, food, other) = {
        let mut c = db.conn();
        let e = entities::create_entity(&mut c, "Personal", "person", "PT", "EUR").unwrap().id;
        let bank = accounts::ensure_account(&c, e, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap().id;
        let food = accounts::ensure_account(&c, e, AccountType::Expense, &["Living", "Food"], "expense", "JPY").unwrap();
        let parent = food.parent_id.unwrap();
        let other = accounts::ensure_account(&c, e, AccountType::Expense, &["Fun"], "expense", "EUR").unwrap().id;
        accounts::update_account(&c, food.id, accounts::AccountUpdate { class: Some("committed".into()), ..Default::default() }).unwrap();
        rates::set_price(&c, "JPY", "EUR", day("2026-01-01"), d("0.01"), "test").unwrap();
        (e, bank, parent, food.id, other)
    };
    Fixture { db, e, bank, parent, food, other }
}
fn expense(f: &Fixture, date: &str, food: &str, other: &str, tag: &str, draft: bool) -> i64 {
    let mut c = f.db.conn();
    let mut i = EntryInput::new(f.e, day(date));
    if draft {
        i.status = EntryStatus::Draft;
    }
    i.postings = vec![PostingInput::new(f.bank, -d(food) * d("0.01") - d(other)), PostingInput::new(f.food, d(food)), PostingInput::new(f.other, d(other))];
    i.postings.retain(|p| !p.quantity.is_zero());
    let e = journal::create_entry(&mut c, i).unwrap();
    tags::set_tags(&mut c, e.id, &tags::parse(tag).unwrap()).unwrap();
    e.id
}
#[test]
fn whole_period_scopes_booked_values_and_overlapping_tags() {
    let f = setup();
    expense(&f, "2026-08-31", "10000", "50", "trip:porto", false);
    expense(&f, "2026-09-01", "10000", "50", "trip:porto with:friends", false);
    expense(&f, "2026-10-15", "20000", "25", "trip:porto", false);
    expense(&f, "2026-11-30", "30000", "0", "trip:elsewhere", false);
    expense(&f, "2026-12-01", "10000", "20", "trip:porto", false);
    expense(&f, "2026-10-02", "90000", "5", "trip:porto", true);
    expense(&f, "2026-10-03", "-2000", "-5", "trip:porto", false);
    let mut c = f.db.conn();
    rates::set_price(&c, "JPY", "EUR", day("2026-12-02"), d("0.02"), "test").unwrap();
    let cat = budgets::save(&mut c, None, input(f.e, f.parent)).unwrap();
    let mut i = input(f.e, f.food);
    i.scope = "class".into();
    i.account_id = None;
    i.class = Some("committed".into());
    i.tag = Some("#TRIP:PORTO".into());
    i.name = "Class".into();
    let class = budgets::save(&mut c, None, i).unwrap();
    let mut i = input(f.e, f.food);
    i.scope = "tag".into();
    i.account_id = None;
    i.tag = Some("trip:porto".into());
    i.name = "Trip".into();
    let tag = budgets::save(&mut c, None, i).unwrap();
    for on in ["2026-09-01", "2026-10-01", "2026-11-30"] {
        let rows = budgets::list(&c, f.e, Some(day(on)), None).unwrap();
        assert_eq!(rows.len(), 3);
        let r = rows.iter().find(|r| r.budget.id == cat.id).unwrap();
        assert_eq!(r.spent.major(), d("580"));
        assert_eq!(r.remaining.major(), d("20"));
        assert_eq!(r.group, "mixed");
        let r = rows.iter().find(|r| r.budget.id == class.id).unwrap();
        assert_eq!(r.spent.major(), d("280"));
        assert_eq!(r.group, "committed");
        let r = rows.iter().find(|r| r.budget.id == tag.id).unwrap();
        assert_eq!(r.spent.major(), d("350"));
    }
    assert!(budgets::list(&c, f.e, Some(day("2026-12-01")), None).unwrap().is_empty());
    assert_eq!(budgets::list(&c, f.e, None, Some("#TRIP:PORTO")).unwrap().len(), 2);
    assert!(budgets::list(&c, f.e, None, Some("trip:port")).unwrap().is_empty());
    let json = serde_json::to_value(cat).unwrap();
    assert_eq!(json["limit"]["minor"], "60000");
}
#[test]
fn validation_defaults_audits_and_atomic_failures() {
    let f = setup();
    let mut c = f.db.conn();
    let e2 = entities::create_entity(&mut c, "Other", "person", "PT", "EUR").unwrap().id;
    for invalid in 0..9 {
        let mut i = input(f.e, f.food);
        match invalid {
            0 => i.entity_id = e2,
            1 => i.account_id = Some(f.bank),
            2 => i.starts_on = Some(day("2026-12-01")),
            3 => i.ends_on = None,
            4 => i.amount = d("-0.001"),
            5 => i.tag = Some("one two".into()),
            6 => i.name = " ".into(),
            7 => {
                i.scope = "class".into();
                i.account_id = None;
                i.class = Some("bad".into());
            }
            _ => {
                i.scope = "tag".into();
                i.account_id = None;
            }
        }
        assert!(budgets::save(&mut c, None, i).is_err(), "invalid case {invalid}");
    }
    assert!(budgets::list(&c, f.e, None, None).unwrap().is_empty());
    let mut i = input(f.e, f.food);
    i.starts_on = None;
    i.ends_on = None;
    let b = budgets::save(&mut c, None, i).unwrap();
    assert_eq!((b.starts_on, b.ends_on), budgets::month_range(means_core::today()));
    assert_eq!(budgets::month_range(day("2024-02-12")), (day("2024-02-01"), day("2024-02-29")));
    assert_eq!(budgets::month_range(day("2026-12-31")), (day("2026-12-01"), day("2026-12-31")));
    let mut i = input(f.e, f.food);
    i.name = "Revised".into();
    i.amount = d("0");
    let b = budgets::save(&mut c, Some(b.id), i).unwrap();
    assert_eq!(b.name, "Revised");
    c.execute_batch("CREATE TRIGGER fail_budget_audit BEFORE INSERT ON audit_log WHEN NEW.table_name='budgets' BEGIN SELECT RAISE(ABORT,'test'); END;").unwrap();
    assert!(budgets::delete(&mut c, b.id).is_err());
    assert_eq!(budgets::get(&c, b.id).unwrap().name, "Revised");
    assert!(budgets::save(&mut c, Some(b.id), input(f.e, f.food)).is_err());
    assert_eq!(budgets::get(&c, b.id).unwrap().name, "Revised");
    assert!(budgets::save(&mut c, None, input(f.e, f.food)).is_err());
    assert_eq!(budgets::list(&c, f.e, None, None).unwrap().len(), 1);
    c.execute_batch("DROP TRIGGER fail_budget_audit").unwrap();
    budgets::delete(&mut c, b.id).unwrap();
    assert!(budgets::get(&c, b.id).is_err());
    let n: i64 = c.query_row("SELECT COUNT(*) FROM audit_log WHERE table_name='budgets'", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 3);
    assert_eq!(c.query_row("SELECT COUNT(*) FROM journal_entries", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
}
#[test]
fn void_dates_and_category_merge_preserve_budget_reference() {
    let f = setup();
    let entry = expense(&f, "2026-09-01", "1000", "0", "trip:porto", false);
    let mut c = f.db.conn();
    let mut i = input(f.e, f.food);
    i.ends_on = Some(day("2026-09-01"));
    i.tag = Some("trip:porto".into());
    i.amount = d("5");
    let b = budgets::save(&mut c, None, i).unwrap();
    journal::void_entry(&mut c, entry, Some(day("2026-09-02")), "test").unwrap();
    let rows = budgets::list(&c, f.e, None, None).unwrap();
    assert_eq!(rows[0].spent.major(), d("10"));
    assert_eq!(rows[0].remaining.major(), d("-5"));
    let mut i = input(f.e, f.food);
    i.tag = Some("trip:porto".into());
    budgets::save(&mut c, Some(b.id), i).unwrap();
    assert_eq!(budgets::list(&c, f.e, None, None).unwrap()[0].spent.major(), d("0"));
    accounts::merge_accounts(&mut c, f.food, f.other).unwrap();
    assert_eq!(budgets::get(&c, b.id).unwrap().account_id, Some(f.other));
    let income = accounts::ensure_account(&c, f.e, AccountType::Income, &["Income"], "income", "EUR").unwrap();
    assert!(accounts::merge_accounts(&mut c, f.other, income.id).is_err());
    assert!(accounts::get_account(&c, f.other).is_ok());
    assert!(means_core::hashchain::verify(&c, f.e).unwrap().first_bad_seq.is_none());
}

#[test]
fn migration_adds_plans_without_changing_existing_books() {
    let mut c = means_core::rusqlite::Connection::open_in_memory().unwrap();
    means_core::db::migrate_to(&c, 13).unwrap();
    let e = entities::create_entity(&mut c, "Personal", "person", "PT", "EUR").unwrap();
    let bank = accounts::ensure_account(&c, e.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
    let food = accounts::ensure_account(&c, e.id, AccountType::Expense, &["Food"], "expense", "EUR").unwrap();
    // Seed version-13 rows directly: today's journal API expects today's schema.
    let mut postings =
        vec![serde_json::json!({"account":bank.uid,"quantity":"-10","amount":"-10","commodity":"EUR"}), serde_json::json!({"account":food.uid,"quantity":"10","amount":"10","commodity":"EUR"})];
    postings.sort_by_key(|v| v.to_string());
    let canonical = serde_json::json!({"uid":"budget-fixture","date":"2026-09-01","payee":"","description":"","postings":postings}).to_string();
    let hash = means_core::hashchain::digest("", &canonical);
    c.execute(
        "INSERT INTO journal_entries(id,uid,entity_id,date,created_at,updated_at,seq,prev_hash,hash) VALUES(1,'budget-fixture',?1,'2026-09-01','test','test',1,'',?2)",
        means_core::rusqlite::params![e.id, hash],
    )
    .unwrap();
    for (uid, account, amount) in [("bank-leg", bank.id, -1000), ("food-leg", food.id, 1000)] {
        c.execute("INSERT INTO postings(uid,journal_entry_id,account_id,quantity,amount) VALUES(?1,1,?2,?3,?3)", means_core::rusqlite::params![uid, account, amount]).unwrap();
    }
    let snapshot = |c: &means_core::rusqlite::Connection| {
        c.query_row("SELECT json_object('entry',json_object('uid',uid,'entity',entity_id,'payee',payee,'date',date,'hash',hash),'postings',(SELECT json_group_array(json_object('account',account_id,'quantity',quantity,'amount',amount)) FROM postings)) FROM journal_entries",[],|r|r.get::<_,String>(0)).unwrap()
    };
    let before = snapshot(&c);
    means_core::db::migrate_to(&c, 14).unwrap();
    means_core::db::migrate_to(&c, 14).unwrap();
    assert_eq!(before, snapshot(&c));
    assert!(budgets::list(&c, e.id, None, None).unwrap().is_empty());
    let b = budgets::save(&mut c, None, input(e.id, food.id)).unwrap();
    budgets::delete(&mut c, b.id).unwrap();
    let newer = budgets::save(&mut c, None, input(e.id, food.id)).unwrap();
    assert!(newer.id > b.id, "audit row IDs must not be recycled");
    means_core::db::migrate_to(&c, i64::MAX).unwrap();
    assert!(means_core::hashchain::verify(&c, e.id).unwrap().first_bad_seq.is_none());
}
