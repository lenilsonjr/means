//! The CLI verbs an accountant works the books with: the real binary on a temp ledger.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use means_core::model::{AccountType, EntryInput, EntryStatus, JournalEntry, PostingInput};
use means_core::{accounts, entities, hashchain, imports, journal, rules, Db};
use rust_decimal::Decimal;

static LEDGERS: AtomicU32 = AtomicU32::new(0);

#[test]
fn currency_migration_cli_previews_then_requires_token_and_new_backup() {
    let ledger = Ledger::new();
    let db = ledger.db();
    let mut conn = db.conn();
    let entity = entities::create_entity(&mut conn, "Personal", "person", "PT", "EUR").unwrap();
    let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["N26"], "bank", "EUR").unwrap();
    let expense = accounts::ensure_account(&conn, entity.id, AccountType::Expense, &["Food"], "expense", "EUR").unwrap();
    means_core::rates::set_price(&conn, "EUR", "USD", date("2026-01-01"), d("1.2"), "manual").unwrap();
    let mut input = EntryInput::new(entity.id, date("2026-01-01"));
    input.postings = vec![PostingInput::new(bank.id, d("-10")), PostingInput::balancing(expense.id)];
    let entry = journal::create_entry(&mut conn, input).unwrap();
    drop(conn);
    let args = ["entity", "migrate-currency", "--entity", "Personal", "--to", "USD"];
    let preview: serde_json::Value = serde_json::from_str(&ledger.ok(&args)).unwrap();
    assert_eq!(entities::get_entity(&db.conn(), entity.id).unwrap().currency, "EUR");
    let token = preview["confirmation"].as_str().unwrap();
    let backup = Ledger::new();
    let backup_path = backup.path.to_str().unwrap();
    let bad = ledger.refused(&[args.as_slice(), &["--apply", "bad", "--backup", backup_path]].concat());
    assert!(bad.contains("confirmation"));
    assert!(!backup.path.exists());
    ledger.ok(&[args.as_slice(), &["--apply", token, "--backup", backup_path]].concat());
    assert_eq!(entities::get_entity(&db.conn(), entity.id).unwrap().currency, "USD");
    let new = journal::get_entry(&db.conn(), entry.id).unwrap();
    assert_eq!(new.postings[0].quantity.commodity(), "EUR");
    assert_eq!(new.postings[0].amount.minor(), -1200);
    assert_eq!(entities::get_entity(&backup.db().conn(), entity.id).unwrap().currency, "EUR");
}

struct Ledger {
    path: PathBuf,
}

impl Drop for Ledger {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

impl Ledger {
    fn new() -> Ledger {
        let n = LEDGERS.fetch_add(1, Ordering::SeqCst);
        Ledger { path: std::env::temp_dir().join(format!("means-cli-{}-{n}.db", std::process::id())) }
    }

    fn db(&self) -> Db {
        Db::open(&self.path).unwrap()
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_means")).arg("--db").arg(&self.path).args(args).output().expect("run means")
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(out.status.success(), "means {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }

    /// The refusal: what the binary said on stderr, after an orderly exit 1. A panic exits 101
    /// and writes to stderr too, so the code is what tells a refusal from a crash.
    fn refused(&self, args: &[&str]) -> String {
        let out = self.run(args);
        let said = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(!said.contains("panicked at"), "means {args:?} panicked: {said}");
        assert_eq!(out.status.code(), Some(1), "means {args:?} should have been refused; it printed: {}", String::from_utf8_lossy(&out.stdout));
        said
    }
}

struct Books {
    entity: i64,
    n26: i64,
    food: i64,
    domains: i64,
    suspense: i64,
}

fn d(s: &str) -> Decimal {
    means_core::money::parse(s).unwrap()
}

fn date(s: &str) -> chrono::NaiveDate {
    means_core::parse_date(s).unwrap()
}

fn seed(l: &Ledger) -> Books {
    let db = l.db();
    let mut conn = db.conn();
    let e = entities::create_entity(&mut conn, "Personal", "person", "PT", "EUR").unwrap();
    let n26 = accounts::ensure_account(&conn, e.id, AccountType::Asset, &["Bank", "N26"], "bank", "EUR").unwrap();
    let food = accounts::ensure_account(&conn, e.id, AccountType::Expense, &["Food"], "expense", "EUR").unwrap();
    let domains = accounts::ensure_account(&conn, e.id, AccountType::Expense, &["Domains"], "expense", "EUR").unwrap();
    let suspense = accounts::find_by_role(&conn, e.id, "suspense").unwrap();
    Books { entity: e.id, n26: n26.id, food: food.id, domains: domains.id, suspense: suspense.id }
}

/// A draft as an import leaves one: the bank posting it knows, the rest on Uncategorized.
fn draft(l: &Ledger, b: &Books, on: &str, payee: &str, quantity: &str) -> JournalEntry {
    let db = l.db();
    let mut conn = db.conn();
    let mut input = EntryInput::new(b.entity, date(on));
    input.payee = payee.into();
    input.status = EntryStatus::Draft;
    input.origin = "import".into();
    input.postings.push(PostingInput::new(b.n26, d(quantity)).memo("card 1234").meta("original", serde_json::json!("EUR")));
    input.postings.push(PostingInput::balancing(b.suspense));
    journal::create_entry(&mut conn, input).unwrap()
}

fn posted(l: &Ledger, b: &Books, on: &str, payee: &str, quantity: &str) -> JournalEntry {
    let db = l.db();
    let mut conn = db.conn();
    let mut input = EntryInput::new(b.entity, date(on));
    input.payee = payee.into();
    input.status = EntryStatus::Posted;
    input.postings.push(PostingInput::new(b.n26, d(quantity)));
    input.postings.push(PostingInput::balancing(b.food));
    journal::create_entry(&mut conn, input).unwrap()
}

fn entry(l: &Ledger, id: i64) -> JournalEntry {
    let db = l.db();
    let conn = db.conn();
    journal::get_entry(&conn, id).unwrap()
}

fn posting_on(e: &JournalEntry, account_id: i64) -> &means_core::model::Posting {
    e.postings.iter().find(|p| p.account_id == account_id).expect("a posting on that account")
}

#[test]
fn entries_filters_by_entity_account_status_date_and_text() {
    let l = Ledger::new();
    let b = seed(&l);
    posted(&l, &b, "2026-02-01", "Cafe Central", "-3.20");
    posted(&l, &b, "2026-03-05", "Lidl", "-41.00");
    draft(&l, &b, "2026-04-10", "Namecheap", "-12.50");
    {
        let db = l.db();
        let mut conn = db.conn();
        let llc = entities::create_entity(&mut conn, "LLC", "company", "US", "USD").unwrap();
        let mercury = accounts::ensure_account(&conn, llc.id, AccountType::Asset, &["Bank", "Mercury"], "bank", "USD").unwrap();
        let hosting = accounts::ensure_account(&conn, llc.id, AccountType::Expense, &["Hosting"], "expense", "USD").unwrap();
        let mut input = EntryInput::new(llc.id, date("2026-02-20"));
        input.payee = "AWS".into();
        input.postings.push(PostingInput::new(mercury.id, d("-90.00")));
        input.postings.push(PostingInput::balancing(hosting.id));
        journal::create_entry(&mut conn, input).unwrap();
    }

    let all = l.ok(&["entries"]);
    assert!(all.ends_with("4 of 4 entries\n"), "{all}");
    assert!(all.contains("AWS"), "{all}");

    let mine = l.ok(&["entries", "--entity", "Personal"]);
    assert!(mine.ends_with("3 of 3 entries\n"), "{mine}");
    assert!(!mine.contains("AWS"), "{mine}");
    let (newest, middle, oldest) = (mine.find("Namecheap").unwrap(), mine.find("Lidl").unwrap(), mine.find("Cafe Central").unwrap());
    assert!(newest < middle && middle < oldest, "newest first: {mine}");
    assert!(mine.contains("Expenses:Uncategorized"), "each entry shows its postings: {mine}");
    assert!(mine.contains("card 1234"), "a posting shows its memo: {mine}");

    let by_id = l.ok(&["entries", "--entity", &b.entity.to_string()]);
    assert_eq!(by_id, mine, "--entity takes an id as well as a name");

    let food = l.ok(&["entries", "--entity", "Personal", "--account", "Expenses:Food"]);
    assert!(food.ends_with("2 of 2 entries\n"), "{food}");
    assert!(!food.contains("Namecheap"), "{food}");

    let drafts = l.ok(&["entries", "--entity", "Personal", "--status", "draft"]);
    assert!(drafts.ends_with("1 of 1 entries\n"), "{drafts}");
    assert!(drafts.contains("Namecheap"), "{drafts}");

    let march = l.ok(&["entries", "--entity", "Personal", "--from", "2026-03-01", "--to", "2026-03-31"]);
    assert!(march.ends_with("1 of 1 entries\n"), "{march}");
    assert!(march.contains("Lidl"), "{march}");

    let found = l.ok(&["entries", "--search", "Namecheap"]);
    assert!(found.ends_with("1 of 1 entries\n"), "{found}");
    let by_memo = l.ok(&["entries", "--search", "card 1234"]);
    assert!(by_memo.ends_with("1 of 1 entries\n"), "the search reads the memos too: {by_memo}");

    let one = l.ok(&["entries", "--entity", "Personal", "--limit", "1"]);
    assert!(one.ends_with("1 of 3 entries\n"), "the count is what was shown against the total: {one}");

    let no_entity = l.refused(&["entries", "--account", "Expenses:Food"]);
    assert!(no_entity.contains("--account needs --entity"), "{no_entity}");
}

#[test]
fn post_moves_the_suspense_posting_to_the_target_account() {
    let l = Ledger::new();
    let b = seed(&l);
    let id = {
        let db = l.db();
        let mut conn = db.conn();
        let mut input = EntryInput::new(b.entity, date("2026-04-10"));
        input.payee = "NAMECHEAP.COM".into();
        input.description = "card purchase".into();
        input.notes = "the yearly renewal, paid on the card".into();
        input.status = EntryStatus::Draft;
        input.origin = "import".into();
        input.postings.push(PostingInput::new(b.n26, d("-50.00")).memo("card 1234").meta("original", serde_json::json!("EUR 50.00")));
        input.postings.push(PostingInput::new(b.food, d("8.00")).memo("snacks"));
        input.postings.push(PostingInput::balancing(b.suspense).memo("from the bank line"));
        journal::create_entry(&mut conn, input).unwrap().id
    };
    let before = entry(&l, id);
    assert_eq!(posting_on(&before, b.suspense).quantity.major(), d("42.00"));

    let out = l.ok(&["post", &id.to_string(), "--to", "Expenses:Domains", "--payee", "Namecheap"]);
    assert!(out.contains("posted") && out.contains("Expenses:Domains"), "{out}");

    let after = entry(&l, id);
    assert_eq!(after.status, EntryStatus::Posted);
    assert!(after.reviewed_at.is_some(), "posting from the CLI is a review, as it is over gRPC");
    assert_eq!(after.payee, "Namecheap");
    assert_eq!(after.description, "card purchase");
    assert_eq!(after.notes, "the yearly renewal, paid on the card", "update_entry writes notes unconditionally, so the rebuild has to carry them");
    assert!(after.postings.iter().all(|p| p.account_id != b.suspense), "no posting is left on Uncategorized");
    let target = posting_on(&after, b.domains);
    assert_eq!(target.quantity, posting_on(&before, b.suspense).quantity);
    assert_eq!(target.amount, posting_on(&before, b.suspense).amount);
    assert_eq!(target.memo, "from the bank line", "the memo of the posting on Uncategorized moves with it");
    let bank = posting_on(&after, b.n26);
    assert_eq!(bank.quantity.major(), d("-50.00"));
    assert_eq!(bank.amount.major(), d("-50.00"));
    assert_eq!(bank.memo, "card 1234");
    assert_eq!(bank.metadata["original"], serde_json::json!("EUR 50.00"));
    let other = posting_on(&after, b.food);
    assert_eq!(other.quantity.major(), d("8.00"));
    assert_eq!(other.memo, "snacks");

    let db = l.db();
    let conn = db.conn();
    let report = hashchain::verify(&conn, b.entity).unwrap();
    assert!(report.first_bad_seq.is_none(), "the chain holds after the post: {report:?}");
    assert_eq!(report.checked, 1);
}

/// A3: a posting in another commodity keeps the value it was booked at. Re-valuing it here with
/// no price in the table would rate it 1:1 and move the books by the whole spread, silently:
/// the entry still balances, because the posting that takes the target account absorbs it.
#[test]
fn post_keeps_the_amount_of_a_posting_in_another_commodity() {
    let l = Ledger::new();
    let b = seed(&l);
    let (inter, id) = {
        let db = l.db();
        let mut conn = db.conn();
        let inter = accounts::ensure_account(&conn, b.entity, AccountType::Asset, &["Bank", "Inter"], "bank", "BRL").unwrap();
        let mut input = EntryInput::new(b.entity, date("2026-04-10"));
        input.payee = "Mercado".into();
        input.status = EntryStatus::Draft;
        input.postings.push(means_core::model::PostingInput { amount: Some(d("-16.00")), ..PostingInput::new(inter.id, d("-100.00")) });
        input.postings.push(PostingInput::balancing(b.suspense));
        (inter.id, journal::create_entry(&mut conn, input).unwrap().id)
    };
    let before = entry(&l, id);
    assert_eq!(posting_on(&before, inter).amount.major(), d("-16.00"), "100 BRL booked at 16 EUR");
    assert_eq!(posting_on(&before, b.suspense).amount.major(), d("16.00"));

    l.ok(&["post", &id.to_string(), "--to", "Expenses:Food"]);

    let after = entry(&l, id);
    let bank = posting_on(&after, inter);
    assert_eq!(bank.quantity.commodity(), "BRL");
    assert_eq!(bank.quantity.major(), d("-100.00"));
    assert_eq!(bank.amount.major(), d("-16.00"), "no price for BRL: re-valuing would make this -100.00 EUR");
    assert_eq!(bank.rate, Some(d("0.16")));
    assert_eq!(bank.rate_source, "input");
    assert_eq!(posting_on(&after, b.food).amount.major(), d("16.00"));

    let db = l.db();
    let conn = db.conn();
    assert!(hashchain::verify(&conn, b.entity).unwrap().first_bad_seq.is_none(), "a re-valued posting still hashes: only the amount says so");
}

#[test]
fn post_refuses_an_entry_that_is_not_a_draft() {
    let l = Ledger::new();
    let b = seed(&l);
    let e = posted(&l, &b, "2026-02-01", "Cafe Central", "-3.20");
    let said = l.refused(&["post", &e.id.to_string(), "--to", "Expenses:Domains"]);
    assert!(said.contains(&format!("#{}", e.id)) && said.contains("not a draft"), "{said}");
    assert_eq!(entry(&l, e.id).postings.len(), 2, "the entry is untouched");
}

#[test]
fn post_refuses_an_entry_without_exactly_one_suspense_posting() {
    let l = Ledger::new();
    let b = seed(&l);
    let id = {
        let db = l.db();
        let mut conn = db.conn();
        let mut input = EntryInput::new(b.entity, date("2026-04-10"));
        input.payee = "Lidl".into();
        input.status = EntryStatus::Draft;
        input.postings.push(PostingInput::new(b.n26, d("-41.00")));
        input.postings.push(PostingInput::balancing(b.food));
        journal::create_entry(&mut conn, input).unwrap().id
    };
    let said = l.refused(&["post", &id.to_string(), "--to", "Expenses:Domains"]);
    assert!(said.contains(&format!("#{id}")) && said.contains("exactly one"), "{said}");
    assert_eq!(entry(&l, id).status, EntryStatus::Draft);
}

#[test]
fn post_refuses_an_unknown_account_path() {
    let l = Ledger::new();
    let b = seed(&l);
    let e = draft(&l, &b, "2026-04-10", "Namecheap", "-12.50");
    let said = l.refused(&["post", &e.id.to_string(), "--to", "Expenses:Dominos"]);
    assert!(said.contains(&format!("#{}", e.id)) && said.contains("no account Expenses:Dominos"), "{said}");
    assert_eq!(entry(&l, e.id).status, EntryStatus::Draft);
}

#[test]
fn post_refuses_an_account_of_another_entity() {
    let l = Ledger::new();
    let b = seed(&l);
    {
        let db = l.db();
        let mut conn = db.conn();
        let llc = entities::create_entity(&mut conn, "LLC", "company", "US", "USD").unwrap();
        accounts::ensure_account(&conn, llc.id, AccountType::Expense, &["Hosting"], "expense", "USD").unwrap();
    }
    let e = draft(&l, &b, "2026-04-10", "Namecheap", "-12.50");
    let said = l.refused(&["post", &e.id.to_string(), "--to", "Expenses:Hosting"]);
    assert!(said.contains(&format!("#{}", e.id)) && said.contains("account of LLC"), "{said}");
    assert_eq!(entry(&l, e.id).status, EntryStatus::Draft);
}

#[test]
fn post_refuses_a_date_on_or_before_the_lock_date() {
    let l = Ledger::new();
    let b = seed(&l);
    let e = draft(&l, &b, "2026-04-10", "Namecheap", "-12.50");
    {
        let db = l.db();
        let mut conn = db.conn();
        entities::update_entity(&mut conn, b.entity, "Personal", "person", "PT", Some(date("2026-04-30")), false).unwrap();
    }
    let said = l.refused(&["post", &e.id.to_string(), "--to", "Expenses:Domains"]);
    assert!(said.contains(&format!("#{}", e.id)) && said.contains("lock date"), "{said}");
    assert_eq!(entry(&l, e.id).status, EntryStatus::Draft);
}

/// A5: what the bank said stays attached to what the books hold.
#[test]
fn post_keeps_the_evidence_of_an_imported_line() {
    let l = Ledger::new();
    let (entity_id, mercury_id) = {
        let db = l.db();
        let mut conn = db.conn();
        let e = entities::create_entity(&mut conn, "LLC", "company", "US", "USD").unwrap();
        let mercury = accounts::ensure_account(&conn, e.id, AccountType::Asset, &["Bank", "Mercury"], "bank", "USD").unwrap();
        accounts::ensure_account(&conn, e.id, AccountType::Expense, &["Domains"], "expense", "USD").unwrap();
        (e.id, mercury.id)
    };
    let csv = "\"Date (UTC)\",\"Description\",\"Amount\",\"Status\",\"Transaction ID\"\n\"03-15-2026\",\"NAMECHEAP.COM\",\"-12.50\",\"Sent\",\"txn_9f3a\"\n";
    let file = std::env::temp_dir().join(format!("means-cli-{}-mercury.csv", std::process::id()));
    std::fs::write(&file, csv).unwrap();
    let out = l.ok(&["import", "--account", &mercury_id.to_string(), file.to_str().unwrap()]);
    let _ = std::fs::remove_file(&file);
    assert!(out.contains("mercury_csv"), "{out}");

    let (line_before, entry_id, bank_before) = {
        let db = l.db();
        let conn = db.conn();
        let line = imports::list_lines(&conn, Some(mercury_id), "created", None, 10).unwrap().pop().expect("the line drafted an entry");
        let entry_id = line.journal_entry_id.unwrap();
        let bank = posting_on(&journal::get_entry(&conn, entry_id).unwrap(), mercury_id).clone();
        (line, entry_id, bank)
    };
    assert_eq!(line_before.reference, "txn_9f3a");
    assert_eq!(line_before.posting_id, Some(bank_before.id));
    assert_eq!(bank_before.external_id.as_deref(), Some("txn_9f3a"));
    assert_eq!(bank_before.fingerprint.as_deref(), Some(line_before.fingerprint.as_str()));
    assert!(bank_before.reconciled_at.is_some());

    l.ok(&["post", &entry_id.to_string(), "--to", "Expenses:Domains"]);

    let db = l.db();
    let conn = db.conn();
    let after = journal::get_entry(&conn, entry_id).unwrap();
    let bank_after = posting_on(&after, mercury_id);
    let line_after = imports::list_lines(&conn, Some(mercury_id), "created", None, 10).unwrap().pop().unwrap();
    assert_eq!(bank_after.uid, bank_before.uid, "posting a draft preserves the bank row identity");
    assert_eq!(bank_after.id, bank_before.id);
    assert_eq!(line_after.journal_entry_id, Some(entry_id));
    assert_eq!(line_after.posting_id, Some(bank_after.id), "the line still points at its original bank posting");
    assert!(bank_after.reconciled_at.is_some(), "the posting stays reconciled with the bank line");
    assert_eq!(bank_after.external_id, bank_before.external_id);
    assert_eq!(bank_after.fingerprint, bank_before.fingerprint);
    assert!(hashchain::verify(&conn, entity_id).unwrap().first_bad_seq.is_none());
}

#[test]
fn rules_are_listed_in_the_order_they_run() {
    let l = Ledger::new();
    let b = seed(&l);
    l.ok(&["rule", "add", "--entity", "Personal", "--contains", "lidl", "--to", "Expenses:Food"]);
    l.ok(&["rule", "add", "--entity", "Personal", "--contains", "namecheap", "--to", "Expenses:Domains", "--payee", "Namecheap"]);
    l.ok(&["rule", "add", "--entity", "Personal", "--contains", "aws", "--to", "Expenses:Domains", "--position", "5"]);
    let appended = l.ok(&["rule", "add", "--entity", "Personal", "--contains", "pingdom", "--to", "Expenses:Domains"]);
    assert!(appended.contains("position 30"), "no --position appends after the last rule: {appended}");

    let rules_now = |b: &Books| {
        let db = l.db();
        let conn = db.conn();
        rules::list_rules(&conn, Some(b.entity)).unwrap()
    };
    let all = rules_now(&b);
    assert_eq!(all.iter().map(|r| r.position).collect::<Vec<_>>(), vec![5, 10, 20, 30]);

    let listed = l.ok(&["rule", "list", "--entity", "Personal"]);
    let lines: Vec<&str> = listed.lines().collect();
    assert_eq!(lines.len(), all.len(), "one line per rule: {listed}");
    // list_rules and the print both run ORDER BY position, id: the nth line is the nth rule to run.
    for (line, r) in lines.iter().zip(all.iter()) {
        let mut field = line.split_whitespace();
        assert_eq!(field.next(), Some(format!("#{}", r.id).as_str()), "the line opens with the rule id: {line}");
        assert_eq!(field.next(), Some(r.position.to_string().as_str()), "then its position: {line}");
        assert!(line.contains(&r.payee), "and its payee: {line}");
    }
    let at = |text: &str| listed.find(text).unwrap_or_else(|| panic!("{text} is missing from:\n{listed}"));
    assert!(at("aws") < at("lidl"), "position 5 runs before position 10: {listed}");
    assert!(at("lidl") < at("Namecheap"), "{listed}");
    assert!(at("Namecheap") < at("pingdom"), "{listed}");
    assert!(listed.contains("description contains"), "{listed}");
    assert!(listed.contains("Expenses:Food"), "{listed}");
    assert!(listed.contains("(0 hits)"), "{listed}");

    let lidl = {
        let db = l.db();
        let conn = db.conn();
        let mut lidl = all.iter().find(|r| r.payee == "lidl").unwrap().clone();
        lidl.enabled = false;
        rules::save_rule(&conn, &lidl).unwrap();
        lidl
    };
    assert!(l.ok(&["rule", "list", "--entity", "Personal"]).contains(" disabled"), "a disabled rule says so");

    let deleted = l.ok(&["rule", "delete", &lidl.id.to_string()]);
    assert!(deleted.contains(&format!("#{}", lidl.id)), "{deleted}");
    let listed = l.ok(&["rule", "list", "--entity", "Personal"]);
    assert!(!listed.contains("lidl"), "the deleted rule is gone: {listed}");
    assert_eq!(rules_now(&b).len(), 3);
}

#[test]
fn rule_add_refuses_an_unknown_account_path() {
    let l = Ledger::new();
    let b = seed(&l);
    let said = l.refused(&["rule", "add", "--entity", "Personal", "--contains", "lidl", "--to", "Expenses:Dominos"]);
    assert!(said.contains("no account Expenses:Dominos"), "{said}");
    let db = l.db();
    let conn = db.conn();
    assert!(rules::list_rules(&conn, Some(b.entity)).unwrap().is_empty());
}

#[test]
fn profiles_list_routes_and_delete_only_the_selected_profile() {
    let l = Ledger::new();
    let b = seed(&l);
    assert!(l.ok(&["profiles"]).contains("No learned inbox profiles"));
    let (default_id, specific_id, other_id) = {
        let db = l.db();
        let mut conn = db.conn();
        let other_entity = entities::create_entity(&mut conn, "Business", "company", "PT", "EUR").unwrap();
        let other = accounts::ensure_account(&conn, other_entity.id, AccountType::Asset, &["Bank", "N26"], "bank", "EUR").unwrap();
        imports::inbox::learn_profile(&conn, "n26_csv", "personal-2026.csv", b.n26).unwrap();
        imports::inbox::learn_profile(&conn, "n26_csv", "personal-2027.csv", b.n26).unwrap();
        imports::inbox::learn_profile(&conn, "n26_csv", "business-2026.csv", other.id).unwrap();
        let profiles = imports::inbox::list_profiles(&conn).unwrap();
        (profiles.iter().find(|p| p.filename_glob.is_empty()).unwrap().id, profiles.iter().find(|p| !p.filename_glob.is_empty()).unwrap().id, other.id)
    };
    let output = l.ok(&["profiles"]);
    assert_eq!(output, l.ok(&["profiles", "list"]));
    for expected in ["n26_csv", "(all filenames)", "business-*.csv", "Personal / Assets:Bank:N26", "Business / Assets:Bank:N26", "2 hits", "1 hits"] {
        assert!(output.contains(expected), "missing {expected}: {output}");
    }
    assert!(output.contains(&format!("account #{other_id}")));
    l.ok(&["profiles", "delete", &specific_id.to_string()]);
    let db = l.db();
    let conn = db.conn();
    assert_eq!(imports::inbox::resolve_account(&conn, "n26_csv", "business-2027.csv").unwrap(), Some(b.n26), "deleting a specific route exposes the source-wide fallback");
    assert_eq!(imports::inbox::list_profiles(&conn).unwrap().len(), 1);
    let history = means_core::audit::history(&conn, "import_profiles", specific_id).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].action, "delete");
    assert!(history[0].before.as_ref().unwrap().contains("business-*.csv"));
    drop(conn);
    assert!(l.refused(&["profiles", "delete", &specific_id.to_string()]).contains("import profile"));
    l.ok(&["profiles", "delete", &default_id.to_string()]);
    assert!(l.ok(&["profiles"]).contains("No learned inbox profiles"));
}

#[test]
fn deleting_a_wrong_profile_lets_the_next_file_teach_the_right_account() {
    let l = Ledger::new();
    let b = seed(&l);
    let original = include_str!("../../means-core/tests/fixtures/n26_booking_date.csv");
    let (profile_id, previous_import, correct_account, previous_postings) = {
        let db = l.db();
        let mut conn = db.conn();
        let correct = accounts::ensure_account(&conn, b.entity, AccountType::Asset, &["Bank", "Correct"], "bank", "EUR").unwrap();
        let first = imports::run_import(&mut conn, imports::ImportRequest::new("n26_csv", Some(b.n26), "n26-2026.csv", original.as_bytes())).unwrap();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM postings", [], |r| r.get(0)).unwrap();
        (imports::inbox::list_profiles(&conn).unwrap()[0].id, first.import.id, correct.id, count)
    };
    l.ok(&["profiles", "delete", &profile_id.to_string()]);
    let dir = l.path.with_extension("inbox");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("n26-2027.csv"), original.replace("2026", "2027")).unwrap();
    let db = l.db();
    let mut conn = db.conn();
    let pending = imports::inbox::scan(&mut conn, &dir).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].action, "pending");
    assert_eq!(imports::get_import(&conn, previous_import).unwrap().0.account_id, Some(b.n26));
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM postings", [], |r| r.get::<_, i64>(0)).unwrap(), previous_postings);
    let completed = imports::inbox::complete_import(&mut conn, pending[0].import_id.unwrap(), correct_account).unwrap();
    assert_eq!(completed.import.account_id, Some(correct_account));
    assert_eq!(imports::inbox::resolve_account(&conn, "n26_csv", "n26-2028.csv").unwrap(), Some(correct_account));
    std::fs::write(dir.join("n26-2028.csv"), original.replace("2026", "2028")).unwrap();
    let next = imports::inbox::scan(&mut conn, &dir).unwrap();
    assert_eq!(next[0].action, "imported", "{next:?}");
    assert_eq!(imports::get_import(&conn, next[0].import_id.unwrap()).unwrap().0.account_id, Some(correct_account));
    assert!(hashchain::verify(&conn, b.entity).unwrap().first_bad_seq.is_none());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn import_retry_resumes_stored_lines_and_keeps_completed_entries() {
    let ledger = Ledger::new();
    let b = seed(&ledger);
    let id = {
        let db = ledger.db();
        let mut conn = db.conn();
        conn.execute_batch("CREATE TEMP TRIGGER interrupt_import BEFORE UPDATE OF journal_entry_id ON statement_lines BEGIN SELECT RAISE(ABORT, 'interrupted'); END;").unwrap();
        let csv = "Booking Date,Value Date,Partner Name,Partner Iban,Type,Payment Reference,Account Name,Amount (EUR)\n2026-02-01,2026-02-01,Coffee,,Card,,Main,-3.20\n";
        imports::run_import(&mut conn, imports::ImportRequest::new("n26_csv", Some(b.n26), "coffee.csv", csv.as_bytes())).unwrap().import.id
    };
    let output = ledger.ok(&["import", "retry", &id.to_string()]);
    assert!(output.contains("1 created"), "{output}");
    assert!(output.contains("0 errors"), "{output}");
    ledger.ok(&["import", "retry", &id.to_string()]);
    let db = ledger.db();
    let conn = db.conn();
    let (_, lines) = imports::get_import(&conn, id).unwrap();
    assert_eq!(lines[0].status, "created");
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM journal_entries WHERE origin = 'import'", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
}

#[test]
fn beancount_export_cli_writes_stdout_or_a_new_file() {
    let ledger = Ledger::new();
    let b = seed(&ledger);
    {
        let db = ledger.db();
        let mut conn = db.conn();
        let mut input = EntryInput::new(b.entity, date("2026-09-01"));
        input.payee = "Corner shop".into();
        input.postings = vec![PostingInput::new(b.n26, d("-10.01")), PostingInput::new(b.food, d("10.01"))];
        journal::create_entry(&mut conn, input).unwrap();
    }
    let stdout = ledger.ok(&["export", "--format", "beancount"]);
    assert!(stdout.contains(" -10.01 EUR"));
    assert!(stdout.contains("means-quantity: -10.01"));
    let path = ledger.path.with_extension("beancount");
    assert!(ledger.ok(&["export", "--format", "beancount", "--output", path.to_str().unwrap()]).is_empty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), stdout);
    ledger.refused(&["export", "--output", path.to_str().unwrap()]);
    ledger.refused(&["export", "--output", ledger.path.to_str().unwrap()]);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), stdout);
    assert!(!ledger.run(&["export", "--format", "unsupported"]).status.success());
    if let Ok(checker) = std::env::var("MEANS_BEAN_CHECK") {
        let result = Command::new(checker).arg(&path).output().unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    }
    std::fs::remove_file(path).unwrap();
}

#[test]
fn entries_json_emits_complete_filtered_objects_with_exact_amounts() {
    let ledger = Ledger::new();
    let books = seed(&ledger);
    let old = posted(&ledger, &books, "2026-01-01", "Old", "-1.00");
    let newest = posted(&ledger, &books, "2026-02-01", "Shop \"quoted\"\nsecond line", "-90071992547409.91");
    let out = ledger.run(&["entries", "--json", "--entity", "Personal", "--limit", "1"]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 1, "no human summary or embedded physical newlines: {text}");
    let value: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(value, serde_json::to_value(entry(&ledger, newest.id)).unwrap());
    assert_eq!(value["postings"][0]["quantity"], serde_json::json!({"minor": "-9007199254740991", "commodity": "EUR", "precision": 2}));
    assert_eq!(String::from_utf8(out.stderr).unwrap().trim(), "1 of 2 entries");
    let filtered = ledger.ok(&["entries", "--json", "--search", "Old", "--to", "2026-01-31", "--status", "posted"]);
    let object: serde_json::Value = serde_json::from_str(&filtered).unwrap();
    assert_eq!(object["id"], old.id);
    let empty = ledger.run(&["entries", "--json", "--search", "missing"]);
    assert!(empty.status.success());
    assert!(empty.stdout.is_empty());
    assert_eq!(String::from_utf8(empty.stderr).unwrap().trim(), "0 of 0 entries");
    assert!(ledger.refused(&["entries", "--json", "--account", "Expenses:Food"]).contains("--account needs --entity"));
}

#[test]
fn reconcile_cli_compares_the_statement_date_and_lists_unfinished_evidence() {
    let ledger = Ledger::new();
    let books = seed(&ledger);
    let shown = posted(&ledger, &books, "2026-01-01", "Unreconciled purchase", "-10.01");
    posted(&ledger, &books, "2026-02-01", "After statement", "-99.00");
    draft(&ledger, &books, "2026-01-01", "Unposted draft", "-88.00");
    let line_id = {
        let db = ledger.db();
        let conn = db.conn();
        conn.execute(
            "INSERT INTO imports(uid, source, account_id, checksum, closing_balance, period_to, options, created_at)
            VALUES ('reconcile-fixture', 'generic_csv', ?1, 'fixture', '-12.01', '2026-01-31', '{\"closing_date\":\"2026-01-31\"}', '2026-02-01')",
            [books.n26],
        )
        .unwrap();
        let import_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO statement_lines(import_id, account_id, position, date, amount, currency, description)
            VALUES (?1, ?2, 0, '2026-01-02', -200, 'EUR', 'Waiting bank line')",
            means_core::rusqlite::params![import_id, books.n26],
        )
        .unwrap();
        conn.last_insert_rowid()
    };
    let before = entry(&ledger, shown.id);
    let output = ledger.ok(&["reconcile", "--account", &books.n26.to_string()]);
    assert!(output.contains("Statement balance: -12.01 EUR (date: 2026-01-31)"), "{output}");
    assert!(output.contains("Ledger balance: -10.01 EUR (as of 2026-01-31)"), "{output}");
    assert!(output.contains("Difference (statement - ledger): -2.00 EUR"), "{output}");
    assert!(output.contains(&format!("line #{line_id} 2026-01-02 -2.00 EUR Waiting bank line")), "{output}");
    assert!(output.contains(&format!("entry #{}", shown.id)), "{output}");
    assert!(output.contains("debit 0.00 credit 10.01 EUR Unreconciled purchase"), "{output}");
    assert!(!output.contains("After statement"));
    assert!(!output.contains("Unposted draft"));
    assert_eq!(output, ledger.ok(&["reconcile", "--account", "Assets:Bank:N26", "--entity", "Personal"]));
    assert_eq!(serde_json::to_value(before).unwrap(), serde_json::to_value(entry(&ledger, shown.id)).unwrap());
    let db = ledger.db();
    let conn = db.conn();
    assert_eq!(imports::get_line(&conn, line_id).unwrap().status, "unmatched");
}

#[test]
fn reconcile_cli_handles_missing_statements_cards_and_invalid_accounts() {
    let ledger = Ledger::new();
    let mut books = seed(&ledger);
    {
        let db = ledger.db();
        let mut conn = db.conn();
        books.n26 = accounts::ensure_account(&conn, books.entity, AccountType::Liability, &["Card"], "credit_card", "EUR").unwrap().id;
        entities::create_entity(&mut conn, "Other", "person", "PT", "EUR").unwrap();
    }
    posted(&ledger, &books, "2026-01-01", "Card purchase", "-12.34");
    let output = ledger.ok(&["reconcile", "--account", &books.n26.to_string()]);
    assert!(output.contains("Statement balance: unavailable"));
    assert!(output.contains("Difference: unavailable"));
    assert!(output.contains("Ledger balance: -12.34 EUR"), "card keeps bank sign: {output}");
    assert!(output.contains("credit 12.34 EUR"));
    assert!(ledger.refused(&["reconcile", "--account", "Liabilities:Card"]).contains("needs --entity"));
    assert!(ledger.refused(&["reconcile", "--account", &books.n26.to_string(), "--entity", "Other"]).contains("does not belong"));
    assert!(ledger.refused(&["reconcile", "--account", "999999"]).contains("account"));
    assert!(!ledger.run(&["reconcile"]).status.success());
}

#[test]
fn serve_no_longer_accepts_browser_launch_flag() {
    let ledger = Ledger::new();
    let help = ledger.ok(&["serve", "--help"]);
    assert!(!help.contains("--open"));
    let out = ledger.run(&["serve", "--open"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--open"));
    assert!(!ledger.path.exists());
}

#[test]
fn draft_delete_refuses_posted_entries_and_keeps_bank_evidence() {
    let l = Ledger::new();
    let b = seed(&l);
    let posted = posted(&l, &b, "2026-08-20", "Keep posted", "-50");
    assert!(l.refused(&["draft", "delete", &posted.id.to_string()]).contains("only drafts"));
    let (import_id, draft_id) = {
        let db = l.db();
        let mut c = db.conn();
        let mapping = imports::CsvMapping { date_column: "Date".into(), amount_column: "Amount".into(), description_column: "Text".into(), ..Default::default() };
        let out = imports::run_import(&mut c, imports::ImportRequest::new("generic_csv", Some(b.n26), "new.csv", b"Date,Amount,Text\n2026-08-21,-10,New draft\n").mapping(Some(&mapping))).unwrap();
        (out.import.id, out.lines[0].journal_entry_id.unwrap())
    };
    assert!(l.ok(&["draft", "delete", &draft_id.to_string()]).contains("remains unmatched"));
    let db = l.db();
    let c = db.conn();
    assert!(journal::get_entry(&c, draft_id).is_err());
    assert!(journal::get_entry(&c, posted.id).is_ok());
    let line = imports::get_import(&c, import_id).unwrap().1.remove(0);
    assert_eq!(line.status, "unmatched");
    assert!(line.posting_id.is_none());
    assert_eq!(line.description, "New draft");
}

#[test]
fn cross_source_cli_requires_the_preview_token_before_merging() {
    cross_source_cli_case(false);
}

#[test]
fn cross_source_cli_previews_and_voids_a_rule_posted_duplicate() {
    cross_source_cli_case(true);
}

fn cross_source_cli_case(rule_posted: bool) {
    let l = Ledger::new();
    let b = seed(&l);
    let original = posted(&l, &b, "2026-08-20", "Keep categories", "-50");
    let (import_id, draft_id) = {
        let db = l.db();
        let mut c = db.conn();
        let mapping = imports::CsvMapping { date_column: "Date".into(), amount_column: "Amount".into(), description_column: "Text".into(), ..Default::default() };
        imports::run_import(&mut c, imports::ImportRequest::new("generic_csv", Some(b.n26), "old.csv", b"Date,Amount,Text\n2026-08-20,-50,Old description\n").mapping(Some(&mapping))).unwrap();
        if rule_posted {
            means_core::rules::save_rule(
                &c,
                &means_core::Rule {
                    id: 0,
                    entity_id: b.entity,
                    name: "Pix".into(),
                    position: 0,
                    enabled: true,
                    conditions: vec![means_core::RuleCondition { field: "description".into(), op: "contains".into(), value: "Pix".into() }],
                    account_id: Some(b.domains),
                    template_id: None,
                    payee: String::new(),
                    hits_count: 0,
                    created_at: String::new(),
                    tags: String::new(),
                },
            )
            .unwrap();
        }
        let file =
            serde_json::json!({"channel":"pluggy","account":{"type":"BANK","currencyCode":"EUR"},"transactions":[{"id":"new-provider-id","date":"2026-08-21","amount":-50,"description":"Pix"}]});
        let out = imports::run_import(&mut c, imports::ImportRequest::new("pluggy_json", Some(b.n26), "new.json", &serde_json::to_vec(&file).unwrap())).unwrap();
        (out.import.id, out.lines[0].journal_entry_id.unwrap())
    };
    let id = import_id.to_string();
    let preview = l.ok(&["rematch", &id, "--cross-source", "--preview"]);
    assert!(preview.contains("preview: 1 cross-source matches"), "{preview}");
    assert_eq!(entry(&l, draft_id).status, if rule_posted { EntryStatus::Posted } else { EntryStatus::Draft });
    assert!(preview.contains("2026-08-20 (1 days apart"), "{preview}");
    if rule_posted {
        assert!(preview.contains("with a reversal on 2026-08-21"), "{preview}");
    }
    assert!(l.refused(&["rematch", &id, "--cross-source", "--apply", "wrong"]).contains("token"));
    let token = preview.split("--apply ").last().unwrap().trim();
    assert!(l.ok(&["rematch", &id, "--cross-source", "--apply", token]).contains("applied: 1"));
    assert_eq!(entry(&l, original.id).payee, "Keep categories");
    if rule_posted {
        let retired = entry(&l, draft_id);
        assert_eq!(retired.status, EntryStatus::Void);
        assert_eq!(entry(&l, retired.reversed_by_id.unwrap()).date, date("2026-08-21"));
    }
    assert!(!l.run(&["rematch", &id, "--apply", token]).status.success());
}

#[test]
fn pluggy_booking_date_is_validated_before_opening_the_ledger() {
    let l = Ledger::new();
    assert!(l.ok(&["pluggy", "pull", "--help"]).contains("--booked-from"));
    assert!(l.refused(&["pluggy", "pull", "--booked-from", "2026-02-30"]).contains("date"));
    assert!(!l.path.exists());
}

#[test]
fn payee_cli_previews_confirms_and_preserves_booked_json() {
    let l = Ledger::new();
    let b = seed(&l);
    let entity = b.entity.to_string();
    let args = ["payee", "save", "--entity", &entity, "--name", "Coffee", "--alias", "cafe"];
    let preview: serde_json::Value = serde_json::from_str(&l.ok(&args)).unwrap();
    assert_eq!(preview["applied"], false);
    let mut confirmed = args.to_vec();
    confirmed.extend(["--confirm", preview["token"].as_str().unwrap()]);
    let saved: serde_json::Value = serde_json::from_str(&l.ok(&confirmed)).unwrap();
    assert_eq!(saved["applied"], true);
    let list: serde_json::Value = serde_json::from_str(&l.ok(&["payee", "list", "--entity", &entity])).unwrap();
    assert_eq!(list[0]["name"], "Coffee");
    assert!(l.refused(&confirmed).contains("already exists") || l.refused(&confirmed).contains("stale"));
    let report: serde_json::Value = serde_json::from_str(&l.ok(&["report", "expenses", "--entity", &entity, "--group-by", "payee", "--json"])).unwrap();
    assert!(report["rows"].is_array());
}

#[test]
fn enable_banking_cutoff_is_validated_before_credentials_or_ledger() {
    let l = Ledger::new();
    let help = l.ok(&["enable-banking", "pull", "--help"]);
    assert!(help.contains("--booked-from"));
    assert!(help.contains("--account"));
    let invalid = l.run(&["enable-banking", "pull", "--booked-from", "2026-02-30"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("date"));
    assert!(!l.path.exists());
}

#[test]
fn mercury_booking_cutoff_is_validated_before_credentials_or_ledger() {
    let l = Ledger::new();
    assert!(l.ok(&["mercury", "pull", "--help"]).contains("--booked-from"));
    let invalid = l.run(&["mercury", "pull", "--booked-from", "2026-02-30"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("date"));
    assert!(!l.path.exists());
}

#[test]
fn inter_and_wise_flags_validate_before_credentials_or_ledger() {
    for provider in ["inter-pj", "wise"] {
        let l = Ledger::new();
        let help = l.ok(&[provider, "pull", "--help"]);
        assert!(help.contains("--account"));
        assert!(help.contains("--booked-from"));
        assert!(help.contains("--dry-run"));
        let invalid = l.run(&[provider, "pull", "--booked-from", "2026-02-30"]);
        assert_eq!(invalid.status.code(), Some(2));
        assert!(!l.path.exists());
    }
}

#[test]
fn post_accepts_repeated_targets_and_final_remainder_atomically() {
    let l = Ledger::new();
    let b = seed(&l);
    for last in ["Expenses:Domains=18.10", "Expenses:Domains"] {
        let before = draft(&l, &b, "2026-04-10", "Shop", "-90.50");
        let id = before.id.to_string();
        l.ok(&["post", &id, "--to", "Expenses:Food=72.40", "--to", last]);
        let after = entry(&l, before.id);
        assert_eq!(posting_on(&after, b.food).quantity.major(), d("72.40"));
        assert_eq!(posting_on(&after, b.domains).quantity.major(), d("18.10"));
        assert_eq!(serde_json::to_value(posting_on(&after, b.n26)).unwrap(), serde_json::to_value(posting_on(&before, b.n26)).unwrap());
    }
    let before = draft(&l, &b, "2026-04-10", "Shop", "-90.50");
    for targets in [["Expenses:Food", "Expenses:Domains=18.10"], ["Expenses:Food=95", "Expenses:Domains"], ["Expenses:Food=oops", "Expenses:Domains"]] {
        l.refused(&["post", &before.id.to_string(), "--to", targets[0], "--to", targets[1]]);
        assert_eq!(serde_json::to_value(entry(&l, before.id)).unwrap(), serde_json::to_value(&before).unwrap());
    }
}

#[test]
fn import_cli_surfaces_split_coverage_warning() {
    let ledger = Ledger::new();
    let db = ledger.db();
    let mut c = db.conn();
    let entity = entities::create_entity(&mut c, "Example Studio", "company", "SG", "USD").unwrap();
    let bank = accounts::ensure_account(&c, entity.id, AccountType::Asset, &["Bank"], "bank", "USD").unwrap();
    let expense = accounts::ensure_account(&c, entity.id, AccountType::Expense, &["Services"], "expense", "USD").unwrap();
    for amount in ["-1500", "-3000"] {
        let mut entry = EntryInput::new(entity.id, date("2026-09-10"));
        entry.postings = vec![PostingInput::new(bank.id, d(amount)), PostingInput::balancing(expense.id)];
        journal::create_entry(&mut c, entry).unwrap();
    }
    drop(c);
    let file = ledger.path.with_extension("csv");
    std::fs::write(&file, "Date (UTC),Description,Amount,Status,Transaction ID\n09-10-2026,Synthetic payment,-4500,Sent,synthetic-1\n").unwrap();
    let output = ledger.ok(&["import", "--source", "mercury_csv", "--account", &bank.id.to_string(), file.to_str().unwrap()]);
    std::fs::remove_file(file).unwrap();
    assert!(output.contains("1 created"), "{output}");
    assert!(output.contains("warning: Possible coverage overlap"), "{output}");
}

#[test]
fn import_lines_exposes_all_errors_read_only_and_retry_recovers_them() {
    let ledger = Ledger::new();
    let bank = seed(&ledger);
    let id = {
        let db = ledger.db();
        let mut conn = db.conn();
        conn.execute_batch(
            "CREATE TEMP TRIGGER fail_import BEFORE UPDATE OF journal_entry_id ON statement_lines WHEN OLD.description = 'Failure' BEGIN SELECT RAISE(ABORT, 'synthetic posting failure'); END;",
        )
        .unwrap();
        let mut csv = String::from("Booking Date,Value Date,Partner Name,Partner Iban,Type,Payment Reference,Account Name,Amount (EUR)\n");
        for n in 1..=31 {
            let name = if n == 31 { "Success" } else { "Failure" };
            csv.push_str(&format!("2026-02-01,2026-02-01,{name},,Card,,Main,-{n}.00\n"));
        }
        let result = imports::run_import(&mut conn, imports::ImportRequest::new("n26_csv", Some(bank.n26), "errors.csv", csv.as_bytes())).unwrap();
        assert_eq!(result.import.error_count, 30);
        assert_eq!(result.import.created_count, 1);
        result.import.id.to_string()
    };
    let before = {
        let db = ledger.db();
        let snapshot = serde_json::to_value(imports::get_import(&db.conn(), id.parse().unwrap()).unwrap()).unwrap();
        snapshot
    };
    let output = ledger.ok(&["import", "lines", &id, "--status", "error"]);
    assert!(output.contains("30 error lines"), "{output}");
    assert_eq!(output.matches("synthetic posting failure").count(), 30);
    let json: serde_json::Value = serde_json::from_str(&ledger.ok(&["import", "lines", &id, "--status", "error", "--json"])).unwrap();
    assert_eq!(json.as_array().unwrap().len(), 30);
    assert!(json.as_array().unwrap().iter().all(|line| line["status"] == "error" && line["journal_entry_id"].is_null()));
    let all: serde_json::Value = serde_json::from_str(&ledger.ok(&["import", "lines", &id, "--json"])).unwrap();
    assert_eq!(all.as_array().unwrap().len(), 31);
    let db = ledger.db();
    assert_eq!(serde_json::to_value(imports::get_import(&db.conn(), id.parse().unwrap()).unwrap()).unwrap(), before);
    assert!(ledger.refused(&["import", "lines", "999999"]).contains("not found"));
    let invalid = ledger.run(&["import", "lines", &id, "--status", "typo"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("invalid value"));
    assert!(ledger.ok(&["import", "retry", &id]).contains("0 errors"));
    let errors: serde_json::Value = serde_json::from_str(&ledger.ok(&["import", "lines", &id, "--status", "error", "--json"])).unwrap();
    assert!(errors.as_array().unwrap().is_empty());
    let missing = Ledger::new();
    missing.refused(&["import", "lines", "1"]);
    assert!(!missing.path.exists(), "inspection must not create a ledger");
}
