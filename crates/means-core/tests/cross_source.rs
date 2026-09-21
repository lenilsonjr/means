use means_core::{accounts, cross_source, entities, imports, journal, tags, AccountType, Db, EntryInput, EntryStatus, PostingInput};
use rust_decimal::Decimal;
use serde_json::json;

struct Fixture {
    db: Db,
    bank: i64,
    entry: i64,
    csv: i64,
    pluggy: i64,
    draft: i64,
}

fn fixture() -> Fixture {
    fixture_on("2026-08-20")
}

fn fixture_on(incoming_date: &str) -> Fixture {
    fixture_with_rule(incoming_date, false)
}

fn fixture_with_rule(incoming_date: &str, posted: bool) -> Fixture {
    fixture_config(incoming_date, posted, "BRL")
}

fn fixture_config(incoming_date: &str, posted: bool, functional: &str) -> Fixture {
    let db = Db::open_memory().unwrap();
    let (bank, entry, csv, pluggy, draft) = {
        let mut c = db.conn();
        let e = entities::create_entity(&mut c, "Personal", "person", "BR", functional).unwrap();
        let bank = accounts::ensure_account(&c, e.id, AccountType::Asset, &["Bank"], "bank", "BRL").unwrap();
        let expense = accounts::ensure_account(&c, e.id, AccountType::Expense, &["Health"], "expense", functional).unwrap();
        if functional != "BRL" {
            means_core::rates::set_price(&c, "BRL", functional, "2026-08-10".parse().unwrap(), Decimal::new(20, 2), "test").unwrap();
        }
        let mut input = EntryInput::new(e.id, "2026-08-20".parse().unwrap());
        input.payee = "EXAMPLE HEALTH original booked payee".into();
        input.description = "Original CSV description".into();
        input.postings = vec![PostingInput::new(bank.id, Decimal::new(-123456, 2)), PostingInput::balancing(expense.id)];
        let entry = journal::create_entry(&mut c, input).unwrap();
        tags::set_tags(&mut c, entry.id, &tags::parse("purpose:health").unwrap()).unwrap();
        let mapping = imports::CsvMapping { date_column: "Date".into(), amount_column: "Amount".into(), description_column: "Text".into(), reference_column: "Ref".into(), ..Default::default() };
        let csv = imports::run_import(
            &mut c,
            imports::ImportRequest::new("generic_csv", Some(bank.id), "bank.csv", b"Date,Amount,Text,Ref\n2026-08-20,-1234.56,Original CSV description,csv-id\n").mapping(Some(&mapping)),
        )
        .unwrap();
        assert_eq!(csv.import.matched_count, 1);
        if functional != "BRL" {
            means_core::rates::set_price(&c, "BRL", functional, incoming_date.parse().unwrap(), Decimal::new(21, 2), "test").unwrap();
        }
        if posted {
            means_core::rules::save_rule(
                &c,
                &means_core::Rule {
                    id: 0,
                    entity_id: e.id,
                    name: "EXAMPLE HEALTH".into(),
                    position: 0,
                    enabled: true,
                    conditions: vec![means_core::RuleCondition { field: "description".into(), op: "contains".into(), value: "EXAMPLE HEALTH".into() }],
                    account_id: Some(accounts::ensure_account(&c, e.id, AccountType::Expense, &["Wrong rule category"], "expense", functional).unwrap().id),
                    template_id: None,
                    payee: "Rule payee".into(),
                    hits_count: 0,
                    created_at: String::new(),
                    tags: "source:rule".into(),
                },
            )
            .unwrap();
        }
        let file = json!({"channel":"pluggy","account":{"type":"BANK","currencyCode":"BRL"},"transactions":[{"id":"pluggy-id","date":incoming_date,"amount":-1234.56,"description":"Bankslip","paymentData":{"receiver":"EXAMPLE HEALTH"}}]});
        let (source, file) = if posted {
            (
                "enable_banking_json",
                json!({"channel":"enable_banking","version":1,"account":{"identification_hash":"bank-hash","uid":"session-account","currency":"BRL"},
                "transactions":[{"entry_reference":"bank-ref","status":"BOOK","booking_date":incoming_date,"value_date":"2026-08-20","credit_debit_indicator":"DBIT",
                    "transaction_amount":{"amount":"1234.56","currency":"BRL"},"creditor":{"name":"EXAMPLE HEALTH"}}]}),
            )
        } else {
            ("pluggy_json", file)
        };
        let pluggy = imports::run_import(&mut c, imports::ImportRequest::new(source, Some(bank.id), "bank.json", &serde_json::to_vec(&file).unwrap())).unwrap();
        assert_eq!(pluggy.import.created_count, 1);
        (bank.id, entry.id, csv.import.id, pluggy.import.id, pluggy.lines[0].journal_entry_id.unwrap())
    };
    Fixture { db, bank, entry, csv, pluggy, draft }
}

#[test]
fn confirmed_overlap_keeps_original_books_and_both_evidence_lines() {
    let f = fixture();
    let mut c = f.db.conn();
    let before = serde_json::to_value(journal::get_entry(&c, f.entry).unwrap()).unwrap();
    let preview = cross_source::rematch(&mut c, f.pluggy, None).unwrap();
    assert_eq!(preview.matches.len(), 1, "{:?}", preview);
    assert_eq!(journal::get_entry(&c, f.draft).unwrap().status, EntryStatus::Draft);
    assert!(!preview.applied);
    assert!(cross_source::rematch(&mut c, f.pluggy, Some("wrong")).is_err());
    let applied = cross_source::rematch(&mut c, f.pluggy, Some(&preview.token)).unwrap();
    assert!(applied.applied);
    assert!(journal::get_entry(&c, f.draft).is_err());
    assert_eq!(serde_json::to_value(journal::get_entry(&c, f.entry).unwrap()).unwrap(), before);
    let a = imports::get_import(&c, f.csv).unwrap().1.remove(0);
    let (summary, mut lines) = imports::get_import(&c, f.pluggy).unwrap();
    let b = lines.remove(0);
    assert_eq!(a.posting_id, b.posting_id);
    assert_eq!(summary.created_count, 0);
    assert_eq!(summary.matched_count, 1);
    assert_eq!(b.reference, "pluggy-id");
    assert_eq!(a.reference, "csv-id");
    assert!(cross_source::rematch(&mut c, f.pluggy, None).unwrap().matches.is_empty());
    assert!(means_core::hashchain::verify(&c, 1).unwrap().first_bad_seq.is_none());
    // Removing only the second source cannot clear the first source's reconciliation.
    imports::delete_import(&mut c, f.pluggy, false).unwrap();
    assert_eq!(serde_json::to_value(journal::get_entry(&c, f.entry).unwrap()).unwrap(), before);
}

#[test]
fn stale_edits_and_locked_periods_are_refused() {
    let f = fixture();
    let mut c = f.db.conn();
    let preview = cross_source::rematch(&mut c, f.pluggy, None).unwrap();
    tags::set_tags(&mut c, f.entry, &tags::parse("purpose:changed").unwrap()).unwrap();
    assert!(cross_source::rematch(&mut c, f.pluggy, Some(&preview.token)).unwrap_err().to_string().contains("stale"));
    assert!(journal::get_entry(&c, f.draft).is_ok());
    c.execute("UPDATE entities SET lock_date='2026-08-31' WHERE id=1", []).unwrap();
    assert!(cross_source::rematch(&mut c, f.pluggy, None).unwrap().matches.is_empty());
}

#[test]
fn edited_duplicates_and_inconsistent_bank_evidence_never_merge() {
    for column in ["date", "amount", "currency", "account_id"] {
        let f = fixture();
        let mut c = f.db.conn();
        let change = match column {
            "date" => "date='2026-08-21'",
            "amount" => "amount=-100",
            "currency" => "currency='USD'",
            _ => "account_id=NULL",
        };
        c.execute(&format!("UPDATE statement_lines SET {change} WHERE import_id=?1"), [f.pluggy]).unwrap();
        assert!(cross_source::rematch(&mut c, f.pluggy, None).unwrap().matches.is_empty(), "{column}");
        assert!(journal::get_entry(&c, f.draft).is_ok());
    }
    let f = fixture();
    let mut c = f.db.conn();
    c.execute("UPDATE statement_lines SET note='by hand' WHERE import_id=?1", [f.pluggy]).unwrap();
    assert!(cross_source::rematch(&mut c, f.pluggy, None).unwrap().matches.is_empty());
    c.execute("UPDATE statement_lines SET note='' WHERE import_id=?1", [f.pluggy]).unwrap();
    tags::set_tags(&mut c, f.draft, &tags::parse("keep:this").unwrap()).unwrap();
    assert!(cross_source::rematch(&mut c, f.pluggy, None).unwrap().matches.is_empty());
    tags::set_tags(&mut c, f.draft, &[]).unwrap();
    assert!(cross_source::rematch(&mut c, f.pluggy, None).unwrap().matches.is_empty(), "clearing an edit does not erase its audit history");
}

#[test]
fn ambiguous_incoming_claims_and_audit_failures_leave_all_books_intact() {
    let f = fixture();
    let mut c = f.db.conn();
    c.execute("INSERT INTO statement_lines(import_id,account_id,position,raw,date,amount,currency,description,reference,fingerprint,status) SELECT import_id,account_id,position+1,raw,date,amount,currency,'Second possible payment','second-id','second-fingerprint','unmatched' FROM statement_lines WHERE import_id=?1",[f.pluggy]).unwrap();
    assert!(cross_source::rematch(&mut c, f.pluggy, None).unwrap().matches.is_empty());
    c.execute("DELETE FROM statement_lines WHERE reference='second-id'", []).unwrap();
    let preview = cross_source::rematch(&mut c, f.pluggy, None).unwrap();
    let before = serde_json::to_value(imports::get_import(&c, f.pluggy).unwrap()).unwrap();
    c.execute_batch("CREATE TRIGGER reject_cross_audit BEFORE INSERT ON audit_log WHEN NEW.action='cross_source_match' BEGIN SELECT RAISE(ABORT,'audit refused'); END;").unwrap();
    assert!(cross_source::rematch(&mut c, f.pluggy, Some(&preview.token)).is_err());
    assert_eq!(serde_json::to_value(imports::get_import(&c, f.pluggy).unwrap()).unwrap(), before);
    assert!(journal::get_entry(&c, f.draft).is_ok());
    assert_eq!(journal::get_entry(&c, f.entry).unwrap().postings.iter().find(|p| p.account_id == f.bank).unwrap().quantity.minor(), -123456);
}

#[test]
fn multiple_target_postings_and_same_source_evidence_are_not_eligible() {
    let f = fixture();
    let mut c = f.db.conn();
    let original = journal::get_entry(&c, f.entry).unwrap();
    let mut input = EntryInput::new(original.entity_id, original.date + chrono::Duration::days(1));
    input.payee = "Another payment of the same amount".into();
    input.postings = original.postings.iter().map(|p| PostingInput::new(p.account_id, p.quantity.major())).collect();
    journal::create_entry(&mut c, input).unwrap();
    let mapping = imports::CsvMapping { date_column: "Date".into(), amount_column: "Amount".into(), description_column: "Text".into(), reference_column: "Ref".into(), ..Default::default() };
    let second = imports::run_import(
        &mut c,
        imports::ImportRequest::new("generic_csv", Some(f.bank), "second.csv", b"Date,Amount,Text,Ref\n2026-08-21,-1234.56,Another real payment,second-csv-id\n").mapping(Some(&mapping)),
    )
    .unwrap();
    assert_eq!(second.import.matched_count, 1);
    assert!(cross_source::rematch(&mut c, f.pluggy, None).unwrap().matches.is_empty());
    c.execute("UPDATE imports SET source='pluggy_json' WHERE id IN (?1,?2)", [f.csv, second.import.id]).unwrap();
    assert!(cross_source::rematch(&mut c, f.pluggy, None).unwrap().matches.is_empty());
}

#[test]
fn original_import_cannot_be_rolled_back_after_its_entry_is_shared() {
    let f = fixture();
    let mut c = f.db.conn();
    // Model the same posted entry as created by the first bank import.
    c.execute("UPDATE statement_lines SET status='created' WHERE import_id=?1", [f.csv]).unwrap();
    let preview = cross_source::rematch(&mut c, f.pluggy, None).unwrap();
    cross_source::rematch(&mut c, f.pluggy, Some(&preview.token)).unwrap();
    assert!(imports::delete_import(&mut c, f.csv, true).unwrap_err().to_string().contains("another import"));
    assert!(journal::get_entry(&c, f.entry).is_ok());
    assert_eq!(imports::get_import(&c, f.pluggy).unwrap().1[0].journal_entry_id, Some(f.entry));
}

#[test]
fn cross_source_uses_normal_five_day_window_in_both_directions() {
    for (date, days, expected) in [("2026-08-19", 1, 1), ("2026-08-21", 1, 1), ("2026-08-15", 5, 1), ("2026-08-25", 5, 1), ("2026-08-14", 6, 0), ("2026-08-26", 6, 0)] {
        let f = fixture_on(date);
        let mut c = f.db.conn();
        let before = serde_json::to_value(journal::get_entry(&c, f.entry).unwrap()).unwrap();
        let preview = cross_source::rematch(&mut c, f.pluggy, None).unwrap();
        assert_eq!(preview.matches.len(), expected, "{date}: {preview:?}");
        if expected == 1 {
            assert_eq!(preview.matches[0].days_apart, days);
            assert_eq!(preview.matches[0].kept_date, "2026-08-20");
            cross_source::rematch(&mut c, f.pluggy, Some(&preview.token)).unwrap();
            assert_eq!(serde_json::to_value(journal::get_entry(&c, f.entry).unwrap()).unwrap(), before);
        }
    }
}

#[test]
fn rule_posted_duplicate_is_voided_on_its_date_and_original_is_unchanged() {
    for date in ["2026-08-19", "2026-08-20", "2026-08-21"] {
        let f = fixture_with_rule(date, true);
        let mut c = f.db.conn();
        let older = serde_json::to_value(journal::get_entry(&c, f.entry).unwrap()).unwrap();
        let duplicate = journal::get_entry(&c, f.draft).unwrap();
        assert_eq!(duplicate.status, EntryStatus::Posted);
        assert_eq!(duplicate.tags, ["source:rule"]);
        let preview = cross_source::rematch(&mut c, f.pluggy, None).unwrap();
        assert_eq!(preview.matches.len(), 1, "{preview:?}");
        assert_eq!(preview.matches[0].voided_entry_id, Some(f.draft));
        assert_eq!(preview.matches[0].removed_draft_id, None);
        assert_eq!(journal::get_entry(&c, f.draft).unwrap().status, EntryStatus::Posted);
        cross_source::rematch(&mut c, f.pluggy, Some(&preview.token)).unwrap();
        assert_eq!(serde_json::to_value(journal::get_entry(&c, f.entry).unwrap()).unwrap(), older);
        let voided = journal::get_entry(&c, f.draft).unwrap();
        assert_eq!(voided.status, EntryStatus::Void);
        let reversal = journal::get_entry(&c, voided.reversed_by_id.unwrap()).unwrap();
        assert_eq!(reversal.date, duplicate.date);
        assert_eq!(reversal.tags, duplicate.tags);
        for p in &duplicate.postings {
            let reversed = reversal.postings.iter().find(|r| r.metadata["reverses_posting"] == p.id).unwrap();
            assert_eq!(reversed.quantity, p.quantity.checked_neg().unwrap());
            assert_eq!(reversed.amount, p.amount.checked_neg().unwrap());
        }
        let bank_total: i64 = c
            .query_row("SELECT SUM(p.quantity) FROM postings p JOIN journal_entries e ON e.id=p.journal_entry_id WHERE p.account_id=?1 AND e.status IN ('posted','void')", [f.bank], |r| r.get(0))
            .unwrap();
        assert_eq!(bank_total, -123456);
        let csv = imports::get_import(&c, f.csv).unwrap().1.remove(0);
        let (summary, lines) = imports::get_import(&c, f.pluggy).unwrap();
        assert_eq!(lines[0].posting_id, csv.posting_id);
        assert_eq!(summary.matched_count, 1);
        assert_eq!(summary.created_count, 0);
        let mut evidence: Vec<u8> = c.query_row("SELECT content FROM imports WHERE id=?1", [f.pluggy], |r| r.get(0)).unwrap();
        evidence.push(b'\n');
        let repeated = imports::run_import(&mut c, imports::ImportRequest::new("auto", Some(f.bank), "repeated.json", &evidence)).unwrap();
        assert_eq!(repeated.import.duplicate_count, 1);
        assert_eq!(repeated.import.error_count, 0);
        assert_eq!(repeated.import.created_count, 0);
        assert_eq!(serde_json::to_value(journal::get_entry(&c, f.entry).unwrap()).unwrap(), older);
        assert!(cross_source::rematch(&mut c, f.pluggy, None).unwrap().matches.is_empty());
        assert!(means_core::hashchain::verify(&c, 1).unwrap().first_bad_seq.is_none());
    }
}

#[test]
fn posted_duplicates_with_human_edits_review_or_shared_evidence_are_skipped() {
    for change in ["review", "tags", "cleared_tags", "note", "evidence", "linked_refund", "lock", "payee"] {
        let f = fixture_with_rule("2026-08-19", true);
        let mut c = f.db.conn();
        match change {
            "review" => {
                journal::mark_reviewed(&c, &[f.draft]).unwrap();
            }
            "tags" | "cleared_tags" => {
                tags::set_tags(&mut c, f.draft, &tags::parse("manual:edit").unwrap()).unwrap();
                if change == "cleared_tags" {
                    tags::set_tags(&mut c, f.draft, &tags::parse("source:rule").unwrap()).unwrap();
                }
            }
            "note" => {
                c.execute("UPDATE statement_lines SET note='by hand' WHERE import_id=?1", [f.pluggy]).unwrap();
            }
            "evidence" => {
                c.execute("UPDATE statement_lines SET journal_entry_id=?1 WHERE import_id=?2", [f.draft, f.csv]).unwrap();
            }
            "linked_refund" => {
                c.execute("UPDATE journal_entries SET refund_of_id=?1 WHERE id=?2", [f.draft, f.entry]).unwrap();
            }
            "lock" => {
                c.execute("UPDATE entities SET lock_date='2026-08-19'", []).unwrap();
            }
            "payee" => {
                means_core::audit::log(&c, "entry_payees", f.draft, "link", None, None).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(cross_source::rematch(&mut c, f.pluggy, None).unwrap().matches.is_empty(), "{change}");
        assert_eq!(journal::get_entry(&c, f.draft).unwrap().status, EntryStatus::Posted);
    }
}

#[test]
fn posted_merge_rechecks_preview_and_rolls_back_reversal_if_audit_fails() {
    let f = fixture_with_rule("2026-08-21", true);
    let mut c = f.db.conn();
    let preview = cross_source::rematch(&mut c, f.pluggy, None).unwrap();
    assert_eq!(preview.matches.len(), 1);
    let before = serde_json::to_value(journal::get_entry(&c, f.draft).unwrap()).unwrap();
    let import_before = serde_json::to_value(imports::get_import(&c, f.pluggy).unwrap()).unwrap();
    c.execute_batch("CREATE TRIGGER reject_cross_audit BEFORE INSERT ON audit_log WHEN NEW.action='cross_source_match' BEGIN SELECT RAISE(ABORT,'audit refused'); END;").unwrap();
    assert!(cross_source::rematch(&mut c, f.pluggy, Some(&preview.token)).is_err());
    assert_eq!(serde_json::to_value(journal::get_entry(&c, f.draft).unwrap()).unwrap(), before);
    assert_eq!(serde_json::to_value(imports::get_import(&c, f.pluggy).unwrap()).unwrap(), import_before);
    assert!(means_core::hashchain::verify(&c, 1).unwrap().first_bad_seq.is_none());
    c.execute_batch("DROP TRIGGER reject_cross_audit").unwrap();
    journal::mark_reviewed(&c, &[f.draft]).unwrap();
    assert!(cross_source::rematch(&mut c, f.pluggy, Some(&preview.token)).unwrap_err().to_string().contains("stale"));
}

#[test]
fn foreign_currency_duplicate_reversal_uses_booked_values_without_new_rates() {
    let f = fixture_config("2026-08-21", true, "USD");
    let mut c = f.db.conn();
    let older = journal::get_entry(&c, f.entry).unwrap();
    let duplicate = journal::get_entry(&c, f.draft).unwrap();
    assert_ne!(older.postings[0].amount, duplicate.postings[0].amount);
    c.execute("DELETE FROM prices", []).unwrap();
    let preview = cross_source::rematch(&mut c, f.pluggy, None).unwrap();
    assert_eq!(preview.matches.len(), 1, "{preview:?}");
    cross_source::rematch(&mut c, f.pluggy, Some(&preview.token)).unwrap();
    let voided = journal::get_entry(&c, f.draft).unwrap();
    let reversal = journal::get_entry(&c, voided.reversed_by_id.unwrap()).unwrap();
    for p in &duplicate.postings {
        let reversed = reversal.postings.iter().find(|r| r.metadata["reverses_posting"] == p.id).unwrap();
        assert_eq!(reversed.amount, p.amount.checked_neg().unwrap());
        assert_eq!(reversed.quantity, p.quantity.checked_neg().unwrap());
    }
    assert_eq!(serde_json::to_value(journal::get_entry(&c, f.entry).unwrap()).unwrap(), serde_json::to_value(older).unwrap());
    assert!(means_core::hashchain::verify(&c, 1).unwrap().first_bad_seq.is_none());
}
