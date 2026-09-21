use std::{path::PathBuf, str::FromStr};

use means_core::{accounts, budgets, currency_migration as migration, entities, hashchain, journal, rates, refunds, AccountType, Db, EntryInput, PostingInput};
use rust_decimal::Decimal;

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}
fn day(s: &str) -> chrono::NaiveDate {
    means_core::parse_date(s).unwrap()
}

struct Fixture {
    dir: PathBuf,
    db: Db,
    entity: i64,
    bank: i64,
    expense: i64,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("means-currency-{}", means_core::new_uid()));
        std::fs::create_dir(&dir).unwrap();
        let db = Db::open(dir.join("ledger.db")).unwrap();
        let (entity, bank, expense) = {
            let mut conn = db.conn();
            let e = entities::create_entity(&mut conn, "Personal", "person", "PT", "EUR").unwrap();
            entities::ensure_currency(&conn, "USD").unwrap();
            let b = accounts::ensure_account(&conn, e.id, AccountType::Asset, &["N26"], "bank", "EUR").unwrap();
            let x = accounts::ensure_account(&conn, e.id, AccountType::Expense, &["Travel"], "expense", "EUR").unwrap();
            (e.id, b.id, x.id)
        };
        Self { dir, db, entity, bank, expense }
    }
    fn purchase(&self, original: Option<&str>, date: &str) -> means_core::JournalEntry {
        let mut input = EntryInput::new(self.entity, day(date));
        let mut bank = PostingInput::new(self.bank, d("-100"));
        if let Some(original) = original {
            bank = bank.external(Some(format!("at:{}", means_core::new_uid())), None).meta("original", serde_json::json!({"quantity":original,"commodity":"USD"}));
        }
        input.postings = vec![bank, PostingInput::balancing(self.expense)];
        journal::create_entry(&mut self.db.conn(), input).unwrap()
    }
    fn preview(&self) -> migration::Preview {
        migration::preview(&mut self.db.conn(), self.entity, "USD").unwrap()
    }
    fn apply(&self, p: &migration::Preview) -> migration::Preview {
        migration::apply_file(self.db.path(), self.entity, "USD", &p.confirmation, &self.dir.join("backup.db"), false).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn evidenced_values_preserve_native_accounts_ids_evidence_and_other_vaults() {
    let f = Fixture::new();
    let original = f.purchase(Some("123.45"), "2026-01-01");
    let company = entities::create_entity(&mut f.db.conn(), "Company", "company", "PT", "EUR").unwrap();
    let before_accounts = accounts::list_accounts(&f.db.conn(), Some(f.entity), true).unwrap();
    f.db.conn().execute("UPDATE postings SET reconciled_at='2026-01-02' WHERE id=?1", [original.postings[0].id]).unwrap();
    let plan = f.preview();
    assert_eq!(plan.reconciled_postings, 1);
    assert_eq!(plan.entries[0].postings[0].after.minor(), -12345);
    assert_eq!(plan.entries[0].postings[1].after.minor(), 12345);
    assert!(plan.entries[0].fx_adjustment.is_zero());
    assert_eq!(entities::get_entity(&f.db.conn(), f.entity).unwrap().currency, "EUR");
    let read_only = migration::open_existing(f.db.path(), false).unwrap();
    assert!(read_only.execute("UPDATE entities SET name='Should fail'", []).is_err());
    drop(read_only);
    f.apply(&plan);
    let conn = f.db.conn();
    let migrated = journal::get_entry(&conn, original.id).unwrap();
    for (old, new) in original.postings.iter().zip(&migrated.postings) {
        assert_eq!(old.id, new.id);
        assert_eq!(old.uid, new.uid);
        assert_eq!(old.quantity, new.quantity);
        assert_eq!(old.account_id, new.account_id);
        assert_eq!(old.external_id, new.external_id);
    }
    for old in before_accounts {
        assert_eq!(accounts::get_account(&conn, old.id).unwrap().commodity, old.commodity);
    }
    assert_eq!(migrated.postings[0].reconciled_at.as_deref(), Some("2026-01-02"));
    assert_eq!(entities::get_entity(&conn, f.entity).unwrap().currency, "USD");
    assert_eq!(entities::get_entity(&conn, company.id).unwrap().currency, "EUR");
    assert_eq!(accounts::find_by_role(&conn, f.entity, "fx_gain_loss").unwrap().commodity, "USD");
    assert!(hashchain::verify(&conn, f.entity).unwrap().first_bad_seq.is_none());
    assert_ne!(migrated.hash, original.hash);
    let backup = migration::open_existing(&f.dir.join("backup.db"), false).unwrap();
    assert_eq!(journal::get_entry(&backup, original.id).unwrap().hash, original.hash);
    assert_eq!(entities::get_entity(&backup, f.entity).unwrap().currency, "EUR");
    assert!(!means_core::audit::history(&conn, "entities", f.entity).unwrap().is_empty());
}

#[test]
fn historical_rates_and_budget_start_dates_are_explicit_and_missing_rates_fail() {
    let f = Fixture::new();
    f.purchase(None, "2026-01-02");
    rates::set_price(&f.db.conn(), "EUR", "USD", day("2026-01-02"), d("1.8"), "import").unwrap();
    assert!(migration::preview(&mut f.db.conn(), f.entity, "USD").unwrap_err().to_string().contains("missing historical rate"));
    rates::set_price(&f.db.conn(), "EUR", "USD", day("2026-01-01"), d("1.2"), "manual").unwrap();
    let b = budgets::save(
        &mut f.db.conn(),
        None,
        budgets::BudgetInput {
            entity_id: f.entity,
            name: "Trip".into(),
            scope: "tag".into(),
            account_id: None,
            class: None,
            tag: Some("trip:one".into()),
            starts_on: Some(day("2026-02-01")),
            ends_on: Some(day("2026-02-28")),
            amount: d("600"),
        },
    )
    .unwrap();
    assert!(migration::preview(&mut f.db.conn(), f.entity, "USD").is_err());
    rates::set_price(&f.db.conn(), "EUR", "USD", day("2026-02-01"), d("1.3"), "manual").unwrap();
    let plan = f.preview();
    assert_eq!(plan.entries[0].postings[0].after.minor(), -12000);
    assert!(plan.entries[0].postings[0].source.contains("2026-01-01"));
    assert_eq!(plan.budgets[0].after.minor(), 78000);
    f.apply(&plan);
    assert_eq!(budgets::get(&f.db.conn(), b.id).unwrap().limit.commodity(), "USD");
    assert_eq!(budgets::get(&f.db.conn(), b.id).unwrap().limit.minor(), 78000);
}

#[test]
fn stale_previews_backup_collisions_locks_and_late_errors_never_partially_apply() {
    let f = Fixture::new();
    let entry = f.purchase(Some("110"), "2026-01-01");
    let old = f.preview();
    f.db.conn().execute("UPDATE entities SET lock_date='2026-01-31' WHERE id=?1", [f.entity]).unwrap();
    let backup = f.dir.join("backup.db");
    assert!(migration::apply_file(f.db.path(), f.entity, "USD", &old.confirmation, &backup, true).unwrap_err().to_string().contains("stale"));
    assert!(!backup.exists());
    let current = f.preview();
    assert_eq!(current.locked_entries, 1);
    assert!(migration::apply_file(f.db.path(), f.entity, "USD", &current.confirmation, &backup, false).is_err());
    assert!(!backup.exists());
    std::fs::write(&backup, "keep me").unwrap();
    assert!(migration::apply_file(f.db.path(), f.entity, "USD", &current.confirmation, &backup, true).is_err());
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), "keep me");
    // Force a failure after the entity and role changes; the transaction must roll back.
    f.db.conn().execute_batch("CREATE TRIGGER reject_currency BEFORE UPDATE OF amount ON postings BEGIN SELECT RAISE(ABORT,'test failure'); END;").unwrap();
    let current = f.preview();
    assert!(migration::apply_file(f.db.path(), f.entity, "USD", &current.confirmation, &f.dir.join("before-failure.db"), true).is_err());
    assert_eq!(entities::get_entity(&f.db.conn(), f.entity).unwrap().currency, "EUR");
    assert_eq!(journal::get_entry(&f.db.conn(), entry.id).unwrap().hash, entry.hash);
    assert_eq!(accounts::find_by_role(&f.db.conn(), f.entity, "fx_gain_loss").unwrap().commodity, "EUR");
}

fn refund(f: &Fixture, original: i64) -> means_core::JournalEntry {
    let mut conn = f.db.conn();
    conn.execute("INSERT INTO imports(uid,source,account_id,checksum,created_at) VALUES ('refund','test',?1,'refund','now')", [f.bank]).unwrap();
    let import = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO statement_lines(import_id,account_id,position,date,amount,currency,description,fingerprint)
        VALUES (?1,?2,0,'2026-02-01',10000,'EUR','Refund','refund-line')",
        rusqlite::params![import, f.bank],
    )
    .unwrap();
    let line = conn.last_insert_rowid();
    refunds::link(&mut conn, line, original).unwrap()
}

#[test]
fn refunds_preserve_original_usd_expense_and_reversals_cancel_migration_fx() {
    let f = Fixture::new();
    let original = f.purchase(Some("110"), "2026-01-01");
    rates::set_price(&f.db.conn(), "EUR", "USD", day("2026-02-01"), d("1.2"), "manual").unwrap();
    let refund = refund(&f, original.id);
    let reversed = journal::void_entry(&mut f.db.conn(), refund.id, Some(day("2026-03-01")), "test").unwrap();
    let reversal = reversed.reversed_by_id.unwrap();
    let plan = f.preview();
    let r = plan.entries.iter().find(|e| e.entry_id == refund.id).unwrap();
    assert_eq!(r.postings.iter().find(|p| p.posting_id == refund.postings[0].id).unwrap().after.minor(), 12000);
    assert_eq!(r.fx_adjustment.minor(), -1000);
    let rev = plan.entries.iter().find(|e| e.entry_id == reversal).unwrap();
    assert_eq!(rev.fx_adjustment.minor(), 1000);
    f.apply(&plan);
    let conn = f.db.conn();
    let migrated_refund = journal::get_entry(&conn, refund.id).unwrap();
    let migrated_reversal = journal::get_entry(&conn, reversal).unwrap();
    for p in &migrated_refund.postings {
        let reverse = migrated_reversal.postings.iter().find(|r| r.metadata["reverses_posting"] == p.id).unwrap();
        assert_eq!(reverse.amount, p.amount.checked_neg().unwrap());
        assert_eq!(reverse.quantity, p.quantity.checked_neg().unwrap());
    }
    assert!(hashchain::verify(&conn, f.entity).unwrap().first_bad_seq.is_none());
    assert_eq!(conn.query_row("SELECT count(*) FROM statement_lines WHERE description='Refund'", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
}

#[test]
fn target_native_quantities_and_explicit_evidence_cannot_absorb_real_fx() {
    let f = Fixture::new();
    let entry = f.purchase(None, "2026-01-01");
    let conn = f.db.conn();
    for (p, value) in entry.postings.iter().zip(["-10.00", "9.99"]) {
        let mut meta = p.metadata.clone();
        meta["value_in"] = serde_json::json!({"quantity":value,"commodity":"USD"});
        conn.execute("UPDATE postings SET metadata=?2 WHERE id=?1", rusqlite::params![p.id, meta.to_string()]).unwrap();
    }
    drop(conn);
    let plan = f.preview();
    assert_eq!(plan.entries[0].postings[1].after.minor(), 999);
    assert_eq!(plan.entries[0].fx_adjustment.minor(), 1);
    f.apply(&plan);
    // The old EUR category still accepts EUR input in the new USD vault.
    rates::set_price(&f.db.conn(), "EUR", "USD", day("2026-03-01"), d("1.2"), "manual").unwrap();
    let next = f.purchase(None, "2026-03-01");
    assert_eq!(next.postings[0].quantity.commodity(), "EUR");
    assert_eq!(next.postings[0].amount.commodity(), "USD");
    assert_eq!(next.postings[0].amount.minor(), -12000);
}

#[test]
fn native_usd_is_authoritative_and_large_balanced_entries_use_a_wide_sum() {
    let f = Fixture::new();
    let mut conn = f.db.conn();
    let usd = accounts::ensure_account(&conn, f.entity, AccountType::Asset, &["USD bank"], "bank", "USD").unwrap();
    let usd_equity = accounts::ensure_account(&conn, f.entity, AccountType::Equity, &["USD equity"], "equity", "USD").unwrap();
    rates::set_price(&conn, "USD", "EUR", day("2026-01-01"), d("1"), "manual").unwrap();
    let mut input = EntryInput::new(f.entity, day("2026-01-01"));
    input.postings = vec![
        PostingInput::new(usd.id, d("60000000000000000")),
        PostingInput::new(usd.id, d("60000000000000000")),
        PostingInput::new(usd_equity.id, d("-60000000000000000")),
        PostingInput::new(usd_equity.id, d("-60000000000000000")),
    ];
    let entry = journal::create_entry(&mut conn, input).unwrap();
    drop(conn);
    let plan = f.preview();
    assert!(plan.entries[0].fx_adjustment.is_zero());
    f.apply(&plan);
    for p in journal::get_entry(&f.db.conn(), entry.id).unwrap().postings {
        assert_eq!(p.amount, p.quantity);
    }
}

#[test]
fn rate_edits_invalidate_confirmation_and_failed_audit_rolls_back_every_change() {
    let f = Fixture::new();
    let original = f.purchase(None, "2026-01-01");
    rates::set_price(&f.db.conn(), "EUR", "USD", day("2026-01-01"), d("1.1"), "manual").unwrap();
    let old = f.preview();
    rates::set_price(&f.db.conn(), "EUR", "USD", day("2026-01-01"), d("1.2"), "manual").unwrap();
    assert!(migration::apply_file(f.db.path(), f.entity, "USD", &old.confirmation, &f.dir.join("stale.db"), false).is_err());
    assert!(!f.dir.join("stale.db").exists());
    f.db.conn()
        .execute_batch(
            "CREATE TRIGGER reject_migration_audit BEFORE INSERT ON audit_log WHEN NEW.table_name='entities' AND NEW.action='currency_migration'
        BEGIN SELECT RAISE(ABORT,'audit failed'); END;",
        )
        .unwrap();
    let current = f.preview();
    let error = migration::apply_file(f.db.path(), f.entity, "USD", &current.confirmation, &f.dir.join("backup.db"), false).unwrap_err();
    assert!(error.to_string().contains("audit failed"), "{error}");
    assert_eq!(f.preview().confirmation, current.confirmation);
    assert_eq!(journal::get_entry(&f.db.conn(), original.id).unwrap().hash, original.hash);
}

#[test]
fn explicit_locked_apply_keeps_lock_date_and_future_fx_posting_works() {
    let f = Fixture::new();
    f.purchase(Some("110"), "2026-01-01");
    f.db.conn().execute("UPDATE entities SET lock_date='2026-01-31' WHERE id=?1", [f.entity]).unwrap();
    let plan = f.preview();
    migration::apply_file(f.db.path(), f.entity, "USD", &plan.confirmation, &f.dir.join("backup.db"), true).unwrap();
    let mut conn = f.db.conn();
    assert_eq!(entities::get_entity(&conn, f.entity).unwrap().lock_date, Some(day("2026-01-31")));
    rates::set_price(&conn, "EUR", "USD", day("2026-02-01"), d("1.2"), "manual").unwrap();
    let usd = accounts::ensure_account(&conn, f.entity, AccountType::Asset, &["USD bank"], "bank", "USD").unwrap();
    let mut input = EntryInput::new(f.entity, day("2026-02-01"));
    input.absorb_fx = true;
    input.postings = vec![PostingInput::new(f.bank, d("-100")), PostingInput::new(usd.id, d("121"))];
    let exchange = journal::create_entry(&mut conn, input).unwrap();
    assert_eq!(exchange.postings.len(), 3);
    let fx = accounts::find_by_role(&conn, f.entity, "fx_gain_loss").unwrap();
    assert_eq!(exchange.postings.iter().find(|p| p.account_id == fx.id).unwrap().amount.minor(), -100);
}

#[test]
fn provisional_book_values_never_supply_a_false_transaction_rate() {
    let f = Fixture::new();
    let mut conn = f.db.conn();
    let usd = accounts::ensure_account(&conn, f.entity, AccountType::Expense, &["USD expense"], "expense", "USD").unwrap();
    let mut input = EntryInput::new(f.entity, day("2026-01-01"));
    input.postings = vec![PostingInput::new(f.bank, d("-100")), PostingInput::new(usd.id, d("10")), PostingInput::balancing(f.expense)];
    let entry = journal::create_entry(&mut conn, input).unwrap();
    assert_eq!(entry.postings[1].rate_source, "missing");
    rates::set_price(&conn, "EUR", "USD", day("2026-01-01"), d("1.2"), "manual").unwrap();
    let error = migration::preview(&mut conn, f.entity, "USD").unwrap_err();
    assert!(error.to_string().contains("unresolved original book rate"), "{error}");
    assert_eq!(entities::get_entity(&conn, f.entity).unwrap().currency, "EUR");
}
