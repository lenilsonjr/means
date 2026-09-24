//! `means entry` and `means policy`: the real binary, JSON contracts and agent refusals.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use means_core::{accounts, entities, AccountType, Db};
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
        let dir = std::env::temp_dir().join(format!("means-entry-cli-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let ledger = Ledger { dir };
        let db = ledger.db();
        let mut c = db.conn();
        let e = entities::create_entity(&mut c, "Northwind Holdings Pte. Ltd.", "company", "SG", "USD").unwrap();
        accounts::ensure_account(&c, e.id, AccountType::Asset, &["Bank", "Operating"], "bank", "USD").unwrap();
        accounts::ensure_account(&c, e.id, AccountType::Expense, &["Travel"], "expense", "USD").unwrap();
        ledger
    }

    fn path(&self) -> PathBuf {
        self.dir.join("ledger.db")
    }

    fn db(&self) -> Db {
        Db::open(self.path()).unwrap()
    }

    fn run(&self, args: &[&str], agent: bool, stdin: Option<&str>) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_means"));
        cmd.arg("--db").arg(self.path()).args(args).env_remove("MEANS_AGENT").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        if agent {
            cmd.env("MEANS_AGENT", "1");
        }
        let mut child = cmd.spawn().expect("run means");
        let mut pipe = child.stdin.take().unwrap();
        pipe.write_all(stdin.unwrap_or("").as_bytes()).unwrap();
        drop(pipe);
        child.wait_with_output().unwrap()
    }

    /// One JSON object on stdout, and the exit code.
    fn json(&self, args: &[&str], agent: bool, stdin: Option<&str>) -> (Value, i32) {
        let out = self.run(args, agent, stdin);
        let text = String::from_utf8(out.stdout).unwrap();
        assert_eq!(text.lines().count(), 1, "means {args:?} must print one JSON line: {text}\nstderr: {}", String::from_utf8_lossy(&out.stderr));
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["schema_version"], 1);
        (value, out.status.code().unwrap())
    }

    fn ok(&self, args: &[&str], agent: bool, stdin: Option<&str>) -> Value {
        let (value, code) = self.json(args, agent, stdin);
        assert_eq!(code, 0, "means {args:?}: {value}");
        assert_eq!(value["ok"], true);
        value
    }

    fn refused(&self, args: &[&str], agent: bool, stdin: Option<&str>) -> String {
        let (value, code) = self.json(args, agent, stdin);
        assert_eq!(code, 1, "means {args:?}: {value}");
        assert_eq!(value["ok"], false);
        assert!(value["error"]["message"].is_string());
        value["error"]["code"].as_str().unwrap().to_string()
    }
}

fn taxi() -> String {
    json!({
        "date": "2026-04-02",
        "payee": "Sample Taxi",
        "tags": ["trip=sample"],
        "postings": [
            {"account": "Bank:Operating", "quantity": "-23.40", "commodity": "USD"},
            {"account": "Expenses:Travel", "memo": "airport"}
        ]
    })
    .to_string()
}

#[test]
fn an_agent_drafts_and_proposes_and_only_a_human_confirms() {
    let ledger = Ledger::new();
    let drafted = ledger.ok(&["entry", "draft", "--entity", "Northwind Holdings Pte. Ltd.", "--stdin"], true, Some(&taxi()));
    let entry = &drafted["entry"];
    assert_eq!(entry["status"], "draft");
    assert!(entry["hash"].is_null());
    assert!(entry["entity_uid"].is_string());
    assert!(drafted["audit_event_id"].is_i64());
    let uid = entry["uid"].as_str().unwrap().to_string();

    let proposal = ledger.ok(&["entry", "propose", &uid], true, None);
    let digest = proposal["proposal_digest"].as_str().unwrap().to_string();
    assert_eq!(proposal["postings"][0]["account_path"], "Assets:Bank:Operating");
    assert_eq!(proposal["postings"][0]["quantity"], json!({"minor": "-2340", "commodity": "USD", "precision": 2}));
    assert_eq!(proposal["totals"]["debit"]["minor"], "2340");

    assert_eq!(ledger.refused(&["entry", "confirm", &uid, "--digest", &digest], true, None), "agent_not_allowed");
    assert_eq!(ledger.refused(&["entry", "confirm", &uid, "--digest", "stale"], false, None), "stale_proposal");
    let confirmed = ledger.ok(&["entry", "confirm", &uid, "--digest", &digest, "--actor", "Sample Person"], false, None);
    assert_eq!(confirmed["entry"]["status"], "posted");
    assert!(confirmed["entry"]["hash"].is_string());
    assert_eq!(confirmed["actor"], "Sample Person");

    let shown = ledger.ok(&["entry", "show", &uid, "--json"], true, None);
    assert_eq!(shown["entry"]["review"]["state"], "reviewed");
    assert_eq!(shown["entry"]["origin"], "agent");
    assert_eq!(shown["entry"]["tags"], json!(["trip:sample"]));
    assert!(shown["entry"]["postings"][0]["account_uid"].is_string());
    assert_eq!(shown["entry"]["evidence"], json!([]));

    let listed = ledger.ok(&["entry", "list", "--entity", "Northwind Holdings Pte. Ltd.", "--status", "posted", "--json"], true, None);
    assert_eq!(listed["count"], 1);
    assert_eq!(listed["entries"][0]["uid"], uid.as_str());
    let drafts = ledger.ok(&["entry", "list", "--entity", "Northwind Holdings Pte. Ltd.", "--status", "draft", "--json"], true, None);
    assert_eq!(drafts["count"], 0);

    assert_eq!(ledger.refused(&["entry", "void", &uid, "--reason", "duplicate"], true, None), "agent_not_allowed");
    let voided = ledger.ok(&["entry", "void", &uid, "--reason", "duplicate"], false, None);
    assert_eq!(voided["entry"]["status"], "void");
    assert_eq!(voided["reversal"]["status"], "posted");
}

#[test]
fn refusals_are_json_with_stable_codes() {
    let ledger = Ledger::new();
    let bad_account = json!({"date": "2026-04-02", "postings": [{"account": "Bank:Missing", "quantity": "-1"}, {"account": "Expenses:Travel"}]}).to_string();
    assert_eq!(ledger.refused(&["entry", "draft", "--entity", "Northwind Holdings Pte. Ltd.", "--stdin"], true, Some(&bad_account)), "unknown_account");
    assert_eq!(ledger.refused(&["entry", "draft", "--entity", "Northwind Holdings Pte. Ltd.", "--stdin"], true, Some("{not json")), "invalid_input");
    assert_eq!(ledger.refused(&["entry", "draft", "--entity", "Nobody", "--stdin"], true, Some(&taxi())), "not_found");
    assert_eq!(ledger.refused(&["entry", "show", "missing-uid", "--json"], true, None), "not_found");
    assert_eq!(ledger.refused(&["entry", "list", "--entity", "Northwind Holdings Pte. Ltd.", "--status", "maybe", "--json"], true, None), "invalid_input");
    let file = ledger.dir.join("draft.json");
    std::fs::write(&file, taxi()).unwrap();
    let drafted = ledger.ok(&["entry", "draft", "--entity", "Northwind Holdings Pte. Ltd.", "--json-file", file.to_str().unwrap()], true, None);
    assert_eq!(drafted["entry"]["status"], "draft");
}

#[test]
fn policy_is_set_by_a_human_and_holds_automatic_posting() {
    let ledger = Ledger::new();
    assert_eq!(ledger.ok(&["policy", "show", "--json"], true, None)["posting_policy"], "normal");
    assert_eq!(ledger.refused(&["policy", "set", "draft-only", "--json"], true, None), "agent_not_allowed");
    assert_eq!(ledger.refused(&["policy", "set", "sometimes", "--json"], false, None), "invalid_input");
    let set = ledger.ok(&["policy", "set", "draft-only", "--json"], false, None);
    assert_eq!(set["posting_policy"], "draft_only");
    assert_eq!(set["previous"], "normal");
    assert_eq!(ledger.ok(&["policy", "show", "--json"], true, None)["posting_policy"], "draft_only");
    let db = ledger.db();
    assert_eq!(means_core::policy::current(&db.conn()).unwrap(), means_core::policy::PostingPolicy::DraftOnly);
}
