//! Period close: each finding, the lock, the unlock and the lock history.

use chrono::NaiveDate;
use means_core::close::{self, CloseReport, Severity};
use means_core::proposal::AgentError;
use means_core::{accounts, audit, entities, journal, AccountType, Db, EntryInput, EntryStatus, Error, PostingInput};
use rust_decimal::Decimal;

struct Books {
    db: Db,
    entity: i64,
    bank: i64,
    euro: i64,
    office: i64,
    affiliate: i64,
    suspense: i64,
}

fn day(s: &str) -> NaiveDate {
    s.parse().unwrap()
}

fn d(s: &str) -> Decimal {
    s.parse().unwrap()
}

fn books() -> Books {
    let db = Db::open_memory().unwrap();
    let b = {
        let mut c = db.conn();
        let e = entities::create_entity(&mut c, "Juniper Row LLC", "company", "US", "USD").unwrap();
        let bank = accounts::ensure_account(&c, e.id, AccountType::Asset, &["Bank", "Checking"], "bank", "USD").unwrap().id;
        let euro = accounts::ensure_account(&c, e.id, AccountType::Asset, &["Bank", "Euro"], "bank", "EUR").unwrap().id;
        let office = accounts::ensure_account(&c, e.id, AccountType::Expense, &["Office"], "expense", "USD").unwrap().id;
        let affiliate = accounts::ensure_account(&c, e.id, AccountType::Asset, &["Due from affiliate"], "receivable", "USD").unwrap().id;
        c.execute("UPDATE accounts SET intercompany = 1 WHERE id = ?1", [affiliate]).unwrap();
        let suspense = accounts::find_by_role(&c, e.id, "suspense").unwrap().id;
        (e.id, bank, euro, office, affiliate, suspense)
    };
    Books { db, entity: b.0, bank: b.1, euro: b.2, office: b.3, affiliate: b.4, suspense: b.5 }
}

impl Books {
    fn entry(&self, date: &str, from: i64, amount: &str, to: i64, status: EntryStatus, origin: &str) -> i64 {
        let mut c = self.db.conn();
        let mut input = EntryInput::new(self.entity, day(date));
        input.status = status;
        input.origin = origin.into();
        input.payee = "Sample Supplier".into();
        input.postings = vec![PostingInput::new(from, d(amount)), PostingInput::balancing(to)];
        journal::create_entry(&mut c, input).unwrap().id
    }

    fn clean_entry(&self, date: &str) -> i64 {
        self.entry(date, self.bank, "-40.00", self.office, EntryStatus::Posted, "")
    }

    fn check(&self, from: &str, to: &str) -> CloseReport {
        close::close_check(&self.db.conn(), self.entity, day(from), day(to)).unwrap()
    }

    fn lock_date(&self) -> Option<NaiveDate> {
        entities::get_entity(&self.db.conn(), self.entity).unwrap().lock_date
    }
}

fn codes(report: &CloseReport) -> Vec<&str> {
    report.findings.iter().map(|f| f.code.as_str()).collect()
}

fn code_of<T: std::fmt::Debug>(result: Result<T, AgentError>) -> &'static str {
    result.expect_err("expected a refusal").code
}

#[test]
fn a_clean_period_passes() {
    let b = books();
    b.clean_entry("2026-03-05");
    let report = b.check("2026-03-01", "2026-03-31");
    assert!(report.ok, "{:?}", report.findings);
    assert!(report.findings.is_empty());
    assert_eq!(report.totals.booked_entries, 1);
    assert_eq!(report.totals.debit.minor(), 4000);
    assert_eq!(report.totals.credit.minor(), 4000);
    assert!(report.totals.balance.is_zero());
}

#[test]
fn a_draft_in_the_period_is_an_error() {
    let b = books();
    let id = b.entry("2026-03-07", b.bank, "-12.00", b.office, EntryStatus::Draft, "");
    let uid = journal::get_entry(&b.db.conn(), id).unwrap().uid;
    let report = b.check("2026-03-01", "2026-03-31");
    assert!(!report.ok);
    let f = report.findings.iter().find(|f| f.code == "unresolved_draft").unwrap();
    assert_eq!(f.severity, Severity::Error);
    assert_eq!(f.entry_uid.as_deref(), Some(uid.as_str()));
    assert_eq!(report.totals.draft_entries, 1);
    // A draft outside the period does not count.
    assert!(b.check("2026-04-01", "2026-04-30").findings.is_empty());
}

#[test]
fn an_unreviewed_posted_entry_is_an_error() {
    let b = books();
    b.entry("2026-03-08", b.bank, "-9.50", b.office, EntryStatus::Posted, "rule");
    let report = b.check("2026-03-01", "2026-03-31");
    assert_eq!(codes(&report), vec!["unreviewed_entry"]);
    assert!(!report.ok);
}

#[test]
fn a_missing_rate_is_an_error() {
    let b = books();
    // No EUR rate exists, so the engine stores the value with rate_source 'missing'.
    b.entry("2026-03-09", b.euro, "-25.00", b.office, EntryStatus::Posted, "");
    let report = b.check("2026-03-01", "2026-03-31");
    let f = report.findings.iter().find(|f| f.code == "missing_rate").expect("missing_rate");
    assert_eq!(f.severity, Severity::Error);
    assert_eq!(f.count, Some(1));
}

#[test]
fn a_foreign_posting_without_a_rate_is_an_error() {
    let b = books();
    means_core::rates::set_price(&b.db.conn(), "EUR", "USD", day("2026-03-01"), d("1.10"), "manual").unwrap();
    let id = b.entry("2026-03-09", b.euro, "-25.00", b.office, EntryStatus::Posted, "");
    assert!(b.check("2026-03-01", "2026-03-31").ok);
    b.db.conn().execute("UPDATE postings SET rate = NULL, rate_source = '' WHERE journal_entry_id = ?1 AND account_id = ?2", [id, b.euro]).unwrap();
    assert_eq!(codes(&b.check("2026-03-01", "2026-03-31")), vec!["missing_rate"]);
}

#[test]
fn a_balance_on_uncategorized_is_an_error() {
    let b = books();
    b.entry("2026-03-10", b.bank, "-18.00", b.suspense, EntryStatus::Posted, "");
    let report = b.check("2026-03-01", "2026-03-31");
    let f = report.findings.iter().find(|f| f.code == "suspense_balance").expect("suspense_balance");
    assert_eq!(f.amount.unwrap().minor(), 1800);
    assert!(f.account_uid.is_some());
    assert!(!report.ok);
}

#[test]
fn an_account_named_suspense_counts_as_suspense() {
    let b = books();
    let other = accounts::ensure_account(&b.db.conn(), b.entity, AccountType::Asset, &["Suspense"], "other", "USD").unwrap().id;
    b.entry("2026-03-10", b.bank, "-5.00", other, EntryStatus::Posted, "");
    assert_eq!(codes(&b.check("2026-03-01", "2026-03-31")), vec!["suspense_balance"]);
}

#[test]
fn an_unmatched_statement_line_is_a_warning() {
    let b = books();
    b.clean_entry("2026-03-05");
    {
        let c = b.db.conn();
        c.execute("INSERT INTO imports (uid, source, account_id, checksum, created_at) VALUES ('imp-1', 'csv', ?1, 'abc', '2026-03-31T00:00:00Z')", [b.bank]).unwrap();
        let import = c.last_insert_rowid();
        for (pos, status) in [(1, "unmatched"), (2, "review"), (3, "skipped")] {
            c.execute(
                "INSERT INTO statement_lines (import_id, account_id, position, date, amount, description, status) VALUES (?1, ?2, ?3, '2026-03-12', -700, 'Sample Cafe', ?4)",
                means_core::rusqlite::params![import, b.bank, pos, status],
            )
            .unwrap();
        }
    }
    let report = b.check("2026-03-01", "2026-03-31");
    assert!(report.ok, "a warning does not block the close");
    let f = report.findings.iter().find(|f| f.code == "unmatched_evidence").unwrap();
    assert_eq!(f.severity, Severity::Warning);
    assert_eq!(f.count, Some(2), "a skipped line is a decision, not open evidence");
    assert_eq!(report.warning_count, 1);
    assert_eq!(report.totals.statement_lines, 3);
}

#[test]
fn an_intercompany_balance_is_a_warning() {
    let b = books();
    b.entry("2026-03-11", b.bank, "-300.00", b.affiliate, EntryStatus::Posted, "");
    let report = b.check("2026-03-01", "2026-03-31");
    assert!(report.ok);
    let f = report.findings.iter().find(|f| f.code == "intercompany_out_of_balance").unwrap();
    assert_eq!(f.severity, Severity::Warning);
    assert_eq!(f.amount.unwrap().minor(), 30000);
}

#[test]
fn an_unbalanced_period_is_an_error() {
    let b = books();
    let id = b.clean_entry("2026-03-05");
    b.db.conn().execute("INSERT INTO postings (uid, journal_entry_id, account_id, quantity, amount) VALUES ('stray-posting', ?1, ?2, 100, 100)", [id, b.office]).unwrap();
    let report = b.check("2026-03-01", "2026-03-31");
    let f = report.findings.iter().find(|f| f.code == "unbalanced_period").expect("unbalanced_period");
    assert_eq!(f.amount.unwrap().minor(), 100);
    assert!(!report.ok);
}

#[test]
fn a_lock_moved_back_outside_unlock_is_an_error() {
    let b = books();
    b.clean_entry("2026-03-05");
    close::close_lock(&mut b.db.conn(), b.entity, day("2026-03-31"), "Avery Stone", "March close", false).unwrap();
    assert!(b.check("2026-04-01", "2026-04-30").ok);
    b.db.conn().execute("UPDATE entities SET lock_date = '2026-01-31' WHERE id = ?1", [b.entity]).unwrap();
    let report = b.check("2026-04-01", "2026-04-30");
    assert_eq!(codes(&report), vec!["lock_moved_back"]);
    // An unlock with a reason authorises the move back.
    b.db.conn().execute("UPDATE entities SET lock_date = '2026-03-31' WHERE id = ?1", [b.entity]).unwrap();
    close::unlock(&mut b.db.conn(), b.entity, day("2026-01-31"), "Avery Stone", "Correct a February invoice", false).unwrap();
    assert!(b.check("2026-04-01", "2026-04-30").ok);
}

#[test]
fn lock_refuses_on_errors_and_passes_with_force() {
    let b = books();
    b.entry("2026-03-07", b.bank, "-12.00", b.office, EntryStatus::Draft, "");
    let audits = |b: &Books| b.db.conn().query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get::<_, i64>(0)).unwrap();
    let before = audits(&b);
    let refused = close::close_lock(&mut b.db.conn(), b.entity, day("2026-03-31"), "Avery Stone", "March close", false);
    assert_eq!(code_of(refused), "close_blocked");
    assert_eq!(b.lock_date(), None);
    assert_eq!(audits(&b), before, "a refused lock writes nothing");

    let out = close::close_lock(&mut b.db.conn(), b.entity, day("2026-03-31"), "Avery Stone", "March close, draft accepted", true).unwrap();
    assert!(out.forced);
    assert_eq!(out.lock_date, day("2026-03-31"));
    assert_eq!(b.lock_date(), Some(day("2026-03-31")));
    let event = audit::get(&b.db.conn(), out.audit_event_id).unwrap();
    assert_eq!(event.action, "close_lock");
    assert_eq!(event.actor, "Avery Stone");
    assert_eq!(event.reason, "March close, draft accepted");
    let after: serde_json::Value = serde_json::from_str(event.after.as_deref().unwrap()).unwrap();
    assert_eq!(after["forced"], true);
    assert_eq!(after["lock_date"], "2026-03-31");
    assert_eq!(after["finding_counts"]["unresolved_draft"], 1);
    assert_eq!(after["report_digest"], out.report_digest.as_str());
    assert_eq!(out.report.digest().unwrap(), out.report_digest);
    let before_lock: serde_json::Value = serde_json::from_str(event.before.as_deref().unwrap()).unwrap();
    assert!(before_lock["lock_date"].is_null());
}

#[test]
fn lock_checks_only_the_days_it_closes() {
    let b = books();
    b.clean_entry("2026-02-05");
    close::close_lock(&mut b.db.conn(), b.entity, day("2026-02-28"), "Avery Stone", "February close", false).unwrap();
    // A later draft does not block an earlier lock, and a lock must move forward.
    b.entry("2026-04-07", b.bank, "-12.00", b.office, EntryStatus::Draft, "");
    let out = close::close_lock(&mut b.db.conn(), b.entity, day("2026-03-31"), "Avery Stone", "March close", false).unwrap();
    assert_eq!(out.previous_lock, Some(day("2026-02-28")));
    assert_eq!(out.report.period_start, day("2026-03-01"));
    assert_eq!(code_of(close::close_lock(&mut b.db.conn(), b.entity, day("2026-03-15"), "Avery Stone", "again", false)), "invalid_input");
    assert_eq!(code_of(close::close_lock(&mut b.db.conn(), b.entity, day("2026-05-31"), " ", "May close", false)), "invalid_input");
    assert_eq!(code_of(close::close_lock(&mut b.db.conn(), b.entity, day("2026-05-31"), "Avery Stone", "", false)), "invalid_input");
}

#[test]
fn unlock_records_actor_and_reason() {
    let b = books();
    b.clean_entry("2026-03-05");
    close::close_lock(&mut b.db.conn(), b.entity, day("2026-03-31"), "Avery Stone", "March close", false).unwrap();
    assert_eq!(code_of(close::unlock(&mut b.db.conn(), b.entity, day("2026-02-28"), "Avery Stone", "", false)), "invalid_input");
    assert_eq!(code_of(close::unlock(&mut b.db.conn(), b.entity, day("2026-04-30"), "Avery Stone", "wrong way", false)), "invalid_input");
    let out = close::unlock(&mut b.db.conn(), b.entity, day("2026-02-28"), "Robin Vale", "Reclassify a March expense", false).unwrap();
    assert_eq!(out.previous_lock, day("2026-03-31"));
    assert_eq!(b.lock_date(), Some(day("2026-02-28")));
    let event = audit::get(&b.db.conn(), out.audit_event_id).unwrap();
    assert_eq!((event.action.as_str(), event.actor.as_str(), event.reason.as_str()), ("unlock", "Robin Vale", "Reclassify a March expense"));
    let history = audit::history(&b.db.conn(), "entities", b.entity).unwrap();
    assert!(history.iter().any(|e| e.action == "close_lock" && e.actor == "Avery Stone"));
}

#[test]
fn update_entity_cannot_move_the_lock_back() {
    let b = books();
    close::close_lock(&mut b.db.conn(), b.entity, day("2026-03-31"), "Avery Stone", "March close", false).unwrap();
    for lock in [Some(day("2026-01-31")), None] {
        let result = entities::update_entity(&mut b.db.conn(), b.entity, "", "", "US", lock, false);
        assert!(matches!(result, Err(Error::Locked(_))), "{result:?}");
        assert_eq!(b.lock_date(), Some(day("2026-03-31")));
    }
    // The same lock date and a later one still work.
    entities::update_entity(&mut b.db.conn(), b.entity, "Juniper Row Holdings LLC", "", "US", Some(day("2026-03-31")), false).unwrap();
    entities::update_entity(&mut b.db.conn(), b.entity, "", "", "US", Some(day("2026-04-30")), false).unwrap();
    assert_eq!(b.lock_date(), Some(day("2026-04-30")));
    assert!(b.check("2026-05-01", "2026-05-31").ok);
}

#[test]
fn entry_confirm_records_the_actor_in_the_audit_columns() {
    let b = books();
    let doc: means_core::proposal::DraftDocument = serde_json::from_value(serde_json::json!({
        "date": "2026-03-10",
        "payee": "Sample Vendor",
        "postings": [{"account": "Bank:Checking", "quantity": "-15.00"}, {"account": "Expenses:Office"}]
    }))
    .unwrap();
    let mut c = b.db.conn();
    let drafted = means_core::proposal::create_draft(&mut c, b.entity, &doc).unwrap();
    let preview = means_core::proposal::propose(&c, drafted.entry.id).unwrap();
    let confirmed = means_core::proposal::confirm(&mut c, drafted.entry.id, &preview.proposal_digest, "Robin Vale").unwrap();
    assert_eq!(audit::get(&c, confirmed.audit_event_id).unwrap().actor, "Robin Vale");
}
