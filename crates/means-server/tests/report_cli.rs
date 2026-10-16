//! `means report …`: the real binary on a small fictional ledger across two months.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use means_core::{accounts, entities, journal, AccountType, Db, EntryInput, EntryStatus, PostingInput};
use serde_json::{json, Value};

const ENTITY: &str = "Harbor Lane Studio LLC";

struct Ledger {
    dir: PathBuf,
}

impl Drop for Ledger {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn usd(minor: i64) -> Value {
    json!({"minor": minor.to_string(), "commodity": "USD", "precision": 2})
}

impl Ledger {
    /// Capital in January. Revenue, rent and software in February and March. One draft in March.
    fn new() -> Ledger {
        let dir = std::env::temp_dir().join(format!("means-report-cli-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let ledger = Ledger { dir };
        let db = Db::open(ledger.path()).unwrap();
        let mut c = db.conn();
        let e = entities::create_entity(&mut c, ENTITY, "company", "US", "USD").unwrap();
        let bank = accounts::ensure_account(&c, e.id, AccountType::Asset, &["Bank", "Operating"], "bank", "USD").unwrap().id;
        let card = accounts::ensure_account(&c, e.id, AccountType::Liability, &["Card"], "card", "USD").unwrap().id;
        let capital = accounts::ensure_account(&c, e.id, AccountType::Equity, &["Capital"], "equity", "USD").unwrap().id;
        let consulting = accounts::ensure_account(&c, e.id, AccountType::Income, &["Consulting"], "income", "USD").unwrap().id;
        let rent = accounts::ensure_account(&c, e.id, AccountType::Expense, &["Rent"], "expense", "USD").unwrap().id;
        let software = accounts::ensure_account(&c, e.id, AccountType::Expense, &["Software"], "expense", "USD").unwrap().id;
        accounts::ensure_account(&c, e.id, AccountType::Liability, &["Accounts Payable"], "payable", "USD").unwrap();
        let entries: [(&str, i64, i64, &str, EntryStatus); 6] = [
            ("2026-01-05", bank, capital, "10000.00", EntryStatus::Posted),
            ("2026-02-03", bank, consulting, "3000.00", EntryStatus::Posted),
            ("2026-02-10", rent, bank, "1200.00", EntryStatus::Posted),
            ("2026-03-04", bank, consulting, "2500.00", EntryStatus::Posted),
            ("2026-03-15", software, card, "300.00", EntryStatus::Posted),
            ("2026-03-20", rent, bank, "999.00", EntryStatus::Draft),
        ];
        for (date, debit, credit, amount, status) in entries {
            let mut input = EntryInput::new(e.id, date.parse().unwrap());
            input.payee = "Blue Gull Services".into();
            input.status = status;
            input.postings = vec![PostingInput::new(debit, amount.parse().unwrap()), PostingInput::balancing(credit)];
            journal::create_entry(&mut c, input).unwrap();
        }
        ledger
    }

    fn path(&self) -> PathBuf {
        self.dir.join("ledger.db")
    }

    fn output(&self, args: &[&str], stdin: Option<&str>) -> std::process::Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_means"));
        cmd.arg("--db").arg(self.path()).args(args).env_remove("MEANS_AGENT").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("run means");
        let mut pipe = child.stdin.take().unwrap();
        pipe.write_all(stdin.unwrap_or("").as_bytes()).unwrap();
        drop(pipe);
        child.wait_with_output().unwrap()
    }

    /// One JSON object on stdout, with the shared report fields.
    fn json(&self, args: &[&str]) -> Value {
        let out = self.output(args, None);
        let text = String::from_utf8(out.stdout).unwrap();
        assert_eq!(text.lines().count(), 1, "means {args:?} must print one JSON line: {text}\nstderr: {}", String::from_utf8_lossy(&out.stderr));
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["schema_version"], 1, "{value}");
        assert_eq!(value["ok"], true, "{value}");
        assert_eq!(value["entity"]["name"], ENTITY);
        assert!(value["entity"]["uid"].is_string());
        assert_eq!(value["functional_currency"], "USD");
        assert!(value["basis"]["valuation"].is_string());
        let statuses: Vec<&str> = value["basis"]["statuses"].as_array().unwrap().iter().map(|s| s.as_str().unwrap()).collect();
        assert!(statuses.starts_with(&["posted", "void"]), "{statuses:?}");
        value
    }

    fn text(&self, args: &[&str]) -> String {
        let out = self.output(args, None);
        assert!(out.status.success(), "means {args:?}: {}", String::from_utf8_lossy(&out.stdout));
        String::from_utf8(out.stdout).unwrap()
    }
}

fn minor(v: &Value) -> i64 {
    assert_eq!(v["commodity"], "USD", "{v}");
    v["minor"].as_str().unwrap_or_else(|| panic!("money minor is a string: {v}")).parse().unwrap()
}

fn row<'a>(rows: &'a Value, path: &str) -> &'a Value {
    rows.as_array().unwrap().iter().find(|r| r["path"] == path).unwrap_or_else(|| panic!("no row {path} in {rows}"))
}

#[test]
fn trial_balance_balances_and_starts_from_the_opening_balance() {
    let l = Ledger::new();
    let tb = l.json(&["report", "trial-balance", "--entity", ENTITY, "--from", "2026-02-01", "--to", "2026-03-31", "--json"]);
    assert_eq!(tb["report"], "trial_balance");
    assert_eq!(tb["basis"]["valuation"], "book");
    assert_eq!(tb["balanced"], true);
    assert_eq!(tb["totals"]["debits"], tb["totals"]["credits"]);
    assert_eq!(tb["totals"]["debits"], usd(700000));
    assert_eq!(minor(&tb["totals"]["closing"]), 0);
    let bank = row(&tb["accounts"], "Assets:Bank:Operating");
    assert!(bank["uid"].is_string());
    assert!(bank.get("statutory_code").is_some());
    assert_eq!(bank["opening"], usd(1_000_000));
    assert_eq!(bank["debits"], usd(550_000));
    assert_eq!(bank["credits"], usd(120_000));
    assert_eq!(bank["closing"], usd(1_430_000));
    // The draft rent does not count.
    assert_eq!(row(&tb["accounts"], "Expenses:Rent")["closing"], usd(120_000));

    let text = l.text(&["report", "trial-balance", "--entity", ENTITY, "--from", "2026-02-01", "--to", "2026-03-31"]);
    assert!(text.contains("Assets:Bank:Operating") && text.contains("14300.00") && text.contains("Balanced"), "{text}");
}

#[test]
fn income_result_equals_the_change_in_equity_and_the_balance_sheet_balances() {
    let l = Ledger::new();
    let income = l.json(&["report", "income", "--entity", ENTITY, "--from", "2026-02-01", "--to", "2026-03-31", "--json"]);
    assert_eq!(income["report"], "income_statement");
    assert_eq!(income["revenue"]["total"], usd(550_000));
    assert_eq!(income["expenses"]["total"], usd(150_000));
    assert_eq!(income["result"], usd(400_000));
    assert!(row(&income["expenses"]["accounts"], "Expenses:Rent")["uid"].is_string());
    let by_class = income["expenses"]["by_class"].as_array().unwrap();
    assert_eq!(by_class.iter().map(|c| minor(&c["amount"])).sum::<i64>(), 150_000);

    let before = l.json(&["report", "balance-sheet", "--entity", ENTITY, "--as-of", "2026-01-31", "--json"]);
    let after = l.json(&["report", "balance-sheet", "--entity", ENTITY, "--as-of", "2026-03-31", "--json"]);
    assert_eq!(after["report"], "balance_sheet");
    assert_eq!(after["basis"]["valuation"], "book");
    assert_eq!(minor(&after["total_equity"]) - minor(&before["total_equity"]), minor(&income["result"]));
    for bs in [&before, &after] {
        assert_eq!(bs["balanced"], true, "{bs}");
        assert_eq!(minor(&bs["total_assets"]), minor(&bs["total_liabilities"]) + minor(&bs["total_equity"]));
        assert_eq!(bs["assets"]["total"], bs["total_assets"]);
    }
    assert_eq!(after["total_assets"], usd(1_430_000));
    assert_eq!(after["total_liabilities"], usd(30_000));
    assert_eq!(after["total_equity"], usd(1_400_000));
    assert!(row(&after["assets"]["accounts"], "Assets:Bank:Operating")["uid"].is_string());

    let market = l.json(&["report", "balance-sheet", "--entity", ENTITY, "--as-of", "2026-03-31", "--market-values", "--json"]);
    assert_eq!(market["basis"]["valuation"], "market");
    assert_eq!(market["total_assets"], after["total_assets"]);

    let text = l.text(&["report", "balance-sheet", "--entity", ENTITY, "--as-of", "2026-03-31"]);
    assert!(text.contains("Total assets") && text.contains("14300.00") && text.contains("Balanced"), "{text}");
    let text = l.text(&["report", "income", "--entity", ENTITY, "--from", "2026-02-01", "--to", "2026-03-31"]);
    assert!(text.contains("Result") && text.contains("4000.00"), "{text}");
}

#[test]
fn general_ledger_keeps_a_running_balance_per_account() {
    let l = Ledger::new();
    let gl = l.json(&["report", "general-ledger", "--entity", ENTITY, "--from", "2026-02-01", "--to", "2026-03-31", "--account", "Assets:Bank:Operating", "--json"]);
    assert_eq!(gl["report"], "general_ledger");
    assert_eq!(gl["count"], 1);
    let bank = &gl["accounts"][0];
    assert!(bank["uid"].is_string());
    assert_eq!(bank["opening_balance"], usd(1_000_000));
    let running: Vec<i64> = bank["postings"].as_array().unwrap().iter().map(|p| minor(&p["running_balance"])).collect();
    assert_eq!(running, [1_300_000, 1_180_000, 1_430_000]);
    assert_eq!(bank["closing_balance"], usd(1_430_000));
    let first = &bank["postings"][0];
    assert!(first["entry_uid"].is_string() && first["posting_uid"].is_string());
    assert_eq!(first["status"], "posted");
    assert_eq!(first["contra_path"], "Income:Consulting");

    let all = l.json(&["report", "general-ledger", "--entity", ENTITY, "--from", "2026-02-01", "--to", "2026-03-31", "--json"]);
    let paths: Vec<&str> = all["accounts"].as_array().unwrap().iter().map(|a| a["path"].as_str().unwrap()).collect();
    assert_eq!(paths, ["Assets:Bank:Operating", "Liabilities:Card", "Equity:Capital", "Income:Consulting", "Expenses:Rent", "Expenses:Software"]);

    let text = l.text(&["report", "general-ledger", "--entity", ENTITY, "--from", "2026-02-01", "--to", "2026-03-31", "--account", "Assets:Bank:Operating"]);
    assert!(text.contains("Opening balance") && text.contains("11800.00"), "{text}");
}

#[test]
fn cash_flow_net_worth_tax_summary_and_open_items_print_their_contracts() {
    let l = Ledger::new();
    let cf = l.json(&["report", "cash-flow", "--entity", ENTITY, "--from", "2026-02-01", "--to", "2026-03-31", "--json"]);
    assert_eq!(cf["report"], "cash_flow");
    assert_eq!(cf["method"], "direct");
    assert_eq!(cf["basis"]["statuses"], json!(["posted", "void", "draft"]));
    assert_eq!(cf["opening"], usd(1_000_000));
    // The card is a cash account: 10000 + 3000 - 1200 + 2500 - 300.
    assert_eq!(cf["posted_closing"], usd(1_400_000));
    assert_eq!(cf["totals"]["draft_out"], usd(99_900));
    assert_eq!(cf["projected_closing"], usd(1_300_100));

    let nw = l.json(&["report", "net-worth", "--entity", ENTITY, "--as-of", "2026-03-31", "--json"]);
    assert_eq!(nw["report"], "net_worth");
    assert_eq!(nw["net_worth"], usd(1_400_000));
    assert_eq!(nw["assets"], usd(1_430_000));
    assert!(row(&nw["accounts"], "Liabilities:Card")["uid"].is_string());

    let tax = l.json(&["report", "tax-summary", "--entity", ENTITY, "--from", "2026-02-01", "--to", "2026-03-31", "--json"]);
    assert_eq!(tax["report"], "tax_summary");
    assert_eq!(tax["count"], 0);
    assert!(tax["rows"].is_array());

    let invoice = json!({
        "direction": "purchase",
        "number": "BG-7",
        "issue_date": "2026-03-02",
        "due_date": "2026-03-20",
        "commodity": "USD",
        "net": "450.00",
        "gross": "450.00",
        "lines": [{"description": "Design review", "net": "450.00", "account": "Expenses:Software"}],
        "payable_account": "Liabilities:Accounts Payable"
    })
    .to_string();
    let added = l.output(&["invoice", "add", "--entity", ENTITY, "--stdin", "--json"], Some(&invoice));
    assert!(added.status.success(), "{}", String::from_utf8_lossy(&added.stdout));
    let items = l.json(&["report", "open-items", "--entity", ENTITY, "--as-of", "2026-03-31", "--direction", "purchase", "--json"]);
    assert_eq!(items["report"], "open_items");
    assert_eq!(items["count"], 1);
    assert!(items["items"][0]["invoice_uid"].is_string());
    assert_eq!(items["items"][0]["open"], usd(45_000));
    assert_eq!(items["items"][0]["days_overdue"], 11);
    assert_eq!(items["totals"], json!([{"direction": "purchase", "commodity": "USD", "open": usd(45_000)}]));
    let none = l.json(&["report", "open-items", "--entity", ENTITY, "--as-of", "2026-03-31", "--direction", "sales", "--json"]);
    assert_eq!(none["count"], 0);

    for args in [
        &["report", "cash-flow", "--entity", ENTITY, "--from", "2026-02-01", "--to", "2026-03-31"][..],
        &["report", "net-worth", "--entity", ENTITY, "--as-of", "2026-03-31"],
        &["report", "tax-summary", "--entity", ENTITY, "--from", "2026-02-01", "--to", "2026-03-31"],
        &["report", "open-items", "--entity", ENTITY, "--as-of", "2026-03-31"],
    ] {
        let text = l.text(args);
        assert!(text.lines().count() >= 3, "means {args:?}: {text}");
    }
}

#[test]
fn reports_refuse_bad_input_with_an_error_object() {
    let l = Ledger::new();
    for (args, code) in [
        (&["report", "trial-balance", "--entity", "Nobody Ltd", "--from", "2026-02-01", "--to", "2026-03-31", "--json"][..], "not_found"),
        (&["report", "income", "--entity", ENTITY, "--from", "2026-04-01", "--to", "2026-03-31", "--json"], "invalid_input"),
        (&["report", "general-ledger", "--entity", ENTITY, "--from", "2026-02-01", "--to", "2026-03-31", "--account", "Assets:Missing", "--json"], "unknown_account"),
    ] {
        let out = l.output(args, None);
        assert_eq!(out.status.code(), Some(1), "means {args:?}");
        let value: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], code, "{value}");
    }
}
