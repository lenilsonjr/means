use means_core::{accounts, entities, hashchain, imports, journal, rates, splits::Leg, tags, AccountType, Db, EntryStatus};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    process::{Command, Output},
};

struct Fixture {
    dir: PathBuf,
    db: Db,
    entry: i64,
    line: i64,
    entity: i64,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
impl Fixture {
    fn new(posted: bool) -> Self {
        let dir = std::env::temp_dir().join(format!("means-void-cli-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(dir.join("ledger.db")).unwrap();
        let (entry, line, entity) = {
            let mut c = db.conn();
            let e = entities::create_entity(&mut c, "Test", "person", "US", "USD").unwrap();
            let bank = accounts::ensure_account(&c, e.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
            let food = accounts::ensure_account(&c, e.id, AccountType::Expense, &["Food"], "expense", "USD").unwrap();
            let home = accounts::ensure_account(&c, e.id, AccountType::Expense, &["Home"], "expense", "USD").unwrap();
            rates::set_price(&c, "EUR", "USD", "2026-09-19".parse().unwrap(), "1.2".parse().unwrap(), "test").unwrap();
            let payload = json!({"channel":"wise","version":1,"account":{"id":"1:2","profile_id":1,"balance_id":2,"currency":"EUR"},"transactions":[{"referenceNumber":"TEST-1","type":"DEBIT","date":"2026-09-19T00:00:00Z","amount":{"value":"-90.50","currency":"EUR"},"details":{"description":"Test shop"}}]});
            imports::run_import(&mut c, imports::ImportRequest::new("wise_json", Some(bank.id), "test.json", &serde_json::to_vec(&payload).unwrap())).unwrap();
            let line = imports::list_lines(&c, Some(bank.id), "created", None, 10).unwrap().remove(0);
            let entry = line.journal_entry_id.unwrap();
            if posted {
                journal::post_draft_to(
                    &mut c,
                    entry,
                    &[Leg { account_id: food.id, quantity: Some("72.40".parse().unwrap()), memo: String::new() }, Leg { account_id: home.id, quantity: None, memo: String::new() }],
                    None,
                )
                .unwrap();
            }
            tags::set_tags(&mut c, entry, &[("project".into(), "test".into())]).unwrap();
            (entry, line.id, e.id)
        };
        Self { dir, db, entry, line, entity }
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_means")).args(["--db", self.dir.join("ledger.db").to_str().unwrap(), "void", &self.entry.to_string()]).args(args).output().unwrap()
    }
    fn snapshot(&self) -> Value {
        let c = self.db.conn();
        let counts: (i64, i64) = c.query_row("SELECT (SELECT COUNT(*) FROM journal_entries),(SELECT COUNT(*) FROM audit_log)", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        json!({"entry":journal::get_entry(&c,self.entry).unwrap(),"line":imports::get_line(&c,self.line).unwrap(),"counts":counts,"chain":hashchain::verify(&c,self.entity).unwrap()})
    }
}
#[test]
fn preview_apply_and_repeat_preserve_history_and_negate_book_values() {
    let f = Fixture::new(true);
    let before = f.snapshot();
    for args in [vec!["--json"], vec!["--dry-run", "--json"]] {
        let out = f.run(&args);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let result: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(result["applied"], false);
        assert_eq!(result["evidence_lines"], 1);
        assert_eq!(f.snapshot(), before);
    }
    let out = f.run(&["--yes", "--reason", "Duplicate purchase", "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["applied"], true);
    let after = f.snapshot();
    assert_eq!(after["entry"]["status"], "void");
    assert_eq!(after["entry"]["postings"], before["entry"]["postings"]);
    let c = f.db.conn();
    let reverse = journal::get_entry(&c, result["reversal"]["id"].as_i64().unwrap()).unwrap();
    let original = journal::get_entry(&c, f.entry).unwrap();
    assert_eq!(reverse.date, original.date);
    assert_eq!(reverse.tags, original.tags);
    assert!(reverse.description.contains("Duplicate purchase"));
    for (a, b) in original.postings.iter().zip(&reverse.postings) {
        assert_eq!(a.account_id, b.account_id);
        assert_eq!(a.quantity.minor(), -b.quantity.minor());
        assert_eq!(a.amount.minor(), -b.amount.minor());
    }
    let line = imports::get_line(&c, f.line).unwrap();
    assert!(line.posting_id.is_none());
    assert!(line.journal_entry_id.is_none());
    assert_eq!(line.status, "unmatched");
    assert!(hashchain::verify(&c, f.entity).unwrap().first_bad_seq.is_none());
    drop(c);
    assert!(f.run(&["--yes"]).status.success());
    assert_eq!(f.snapshot(), after);
}
#[test]
fn draft_void_never_deletes_or_changes_evidence() {
    let f = Fixture::new(false);
    let before = f.snapshot();
    for args in [vec![], vec!["--yes"], vec!["--date", "not-a-date", "--yes"]] {
        assert!(!f.run(&args).status.success());
        assert_eq!(f.snapshot(), before);
    }
    assert!(journal::void_entry(&mut f.db.conn(), f.entry, None, "").is_err());
    assert_eq!(journal::get_entry(&f.db.conn(), f.entry).unwrap().status, EntryStatus::Draft);
    assert_eq!(f.snapshot(), before);
}
#[test]
fn locks_refuse_original_date_and_allow_an_explicit_open_period() {
    let f = Fixture::new(true);
    entities::update_entity(&mut f.db.conn(), f.entity, "Test", "person", "US", Some("2026-09-19".parse().unwrap()), false).unwrap();
    let before = f.snapshot();
    for args in [vec![], vec!["--yes"], vec!["--yes", "--dry-run"]] {
        assert!(!f.run(&args).status.success());
        assert_eq!(f.snapshot(), before);
    }
    assert!(f.run(&["--date", "2026-09-20"]).status.success());
    assert_eq!(f.snapshot(), before);
    let out = f.run(&["--date", "2026-09-20", "--yes", "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["reversal"]["date"], "2026-09-20");
    assert!(hashchain::verify(&f.db.conn(), f.entity).unwrap().first_bad_seq.is_none());
}
