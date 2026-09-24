//! `means invoice`: the real binary, the JSON contract and the agent refusal.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use means_core::{accounts, entities, journal, AccountType, Db, EntryInput, PostingInput};
use serde_json::{json, Value};

struct Ledger {
    dir: PathBuf,
}

impl Drop for Ledger {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Ledger {
    fn new() -> Ledger {
        let dir = std::env::temp_dir().join(format!("means-invoice-cli-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let ledger = Ledger { dir };
        let db = Db::open(ledger.path()).unwrap();
        let mut c = db.conn();
        let e = entities::create_entity(&mut c, "Rio Verde SL", "company", "ES", "EUR").unwrap();
        accounts::ensure_account(&c, e.id, AccountType::Asset, &["Bank", "Operating"], "bank", "EUR").unwrap();
        accounts::ensure_account(&c, e.id, AccountType::Expense, &["Software"], "expense", "EUR").unwrap();
        accounts::ensure_account(&c, e.id, AccountType::Liability, &["Accounts Payable"], "payable", "EUR").unwrap();
        ledger
    }

    fn path(&self) -> PathBuf {
        self.dir.join("ledger.db")
    }

    fn json(&self, args: &[&str], agent: bool, stdin: Option<&str>) -> (Value, i32) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_means"));
        cmd.arg("--db").arg(self.path()).args(args).env_remove("MEANS_AGENT").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        if agent {
            cmd.env("MEANS_AGENT", "1");
        }
        let mut child = cmd.spawn().expect("run means");
        let mut pipe = child.stdin.take().unwrap();
        pipe.write_all(stdin.unwrap_or("").as_bytes()).unwrap();
        drop(pipe);
        let out = child.wait_with_output().unwrap();
        let text = String::from_utf8(out.stdout).unwrap();
        assert_eq!(text.lines().count(), 1, "means {args:?} must print one JSON line: {text}\nstderr: {}", String::from_utf8_lossy(&out.stderr));
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["schema_version"], 1);
        (value, out.status.code().unwrap_or(-1))
    }
}

#[test]
fn invoice_commands_record_settle_and_refuse() {
    let ledger = Ledger::new();
    let document = json!({
        "direction": "purchase",
        "number": "F-1",
        "issue_date": "2026-03-02",
        "due_date": "2026-04-01",
        "commodity": "EUR",
        "net": "100.00",
        "gross": "100.00",
        "lines": [{"description": "Hosting", "net": "100.00", "account": "Expenses:Software"}],
        "payable_account": "Liabilities:Accounts Payable"
    })
    .to_string();
    let (added, code) = ledger.json(&["invoice", "add", "--entity", "Rio Verde SL", "--stdin", "--recognize", "--json"], true, Some(&document));
    assert_eq!(code, 0, "{added}");
    assert_eq!(added["ok"], true);
    assert_eq!(added["invoice"]["gross"], json!({"minor": "10000", "commodity": "EUR", "precision": 2}));
    assert_eq!(added["recognition_entry"]["status"], "draft");
    assert!(added["invoice"].get("id").is_none());
    let uid = added["invoice"]["uid"].as_str().unwrap().to_string();

    let (listed, _) = ledger.json(&["invoice", "list", "--entity", "Rio Verde SL", "--open", "--as-of", "2026-04-05", "--json"], true, None);
    assert_eq!(listed["count"], 1);
    assert_eq!(listed["invoices"][0]["days_overdue"], 4);

    // The agent cannot cancel.
    let (refused, code) = ledger.json(&["invoice", "cancel", &uid, "--reason", "entered twice", "--json"], true, None);
    assert_eq!((code, refused["error"]["code"].as_str()), (1, Some("agent_not_allowed")));

    let entry_uid = {
        let db = Db::open(ledger.path()).unwrap();
        let mut c = db.conn();
        let entity = entities::list_entities(&c, false).unwrap()[0].id;
        let bank = accounts::find_by_path(&c, entity, "Assets:Bank:Operating").unwrap().unwrap().id;
        let payable = accounts::find_by_path(&c, entity, "Liabilities:Accounts Payable").unwrap().unwrap().id;
        let mut input = EntryInput::new(entity, "2026-03-10".parse().unwrap());
        input.postings = vec![PostingInput::new(bank, "-100.00".parse().unwrap()), PostingInput::balancing(payable)];
        journal::create_entry(&mut c, input).unwrap().uid
    };
    let (too_much, code) = ledger.json(&["invoice", "settle", &uid, "--entry", &entry_uid, "--date", "2026-03-10", "--amount", "100.01", "--json"], true, None);
    assert_eq!((code, too_much["error"]["code"].as_str()), (1, Some("over_settlement")));
    let (settled, code) = ledger.json(&["invoice", "settle", &uid, "--entry", &entry_uid, "--date", "2026-03-10", "--amount", "100.00", "--kind", "payment", "--json"], true, None);
    assert_eq!(code, 0, "{settled}");
    assert_eq!(settled["invoice"]["status"], "settled");

    let (shown, _) = ledger.json(&["invoice", "show", &uid, "--json"], true, None);
    assert_eq!(shown["settlements"].as_array().unwrap().len(), 1);
    assert_eq!(shown["lines"][0]["account_path"], "Expenses:Software");
    let roles: Vec<&str> = shown["linked_entries"].as_array().unwrap().iter().map(|e| e["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["recognition", "payment"]);

    let (cancel, code) = ledger.json(&["invoice", "cancel", &uid, "--reason", "entered twice", "--json"], false, None);
    assert_eq!((code, cancel["error"]["code"].as_str()), (1, Some("settled")));
}
