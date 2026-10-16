//! `means void`, `means entry void` and `means intervault unlink` on linked entries: the real
//! binary, the preview of the linked entries, the cascade and the human unlink.

use std::path::PathBuf;
use std::process::{Command, Output};

use means_core::{accounts, audit, entities, intervault, journal, AccountType, Db, EntryInput, EntryStatus, JournalEntry, PostingInput};
use serde_json::Value;

struct Books {
    dir: PathBuf,
    parent: i64,
    subsidiary: i64,
    a: JournalEntry,
    b: JournalEntry,
    link_uid: String,
}

impl Drop for Books {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A parent in SGD lends to its subsidiary in USD. Each entity books one side, and a link joins them.
fn books() -> Books {
    let dir = std::env::temp_dir().join(format!("means-intervault-cli-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::open(dir.join("ledger.db")).unwrap();
    let mut c = db.conn();
    let parent = entities::create_entity(&mut c, "Northwind Holdings Pte. Ltd.", "company", "SG", "SGD").unwrap();
    let subsidiary = entities::create_entity(&mut c, "Blue Harbor LLC", "company", "US", "USD").unwrap();
    let parent_bank = accounts::ensure_account(&c, parent.id, AccountType::Asset, &["Bank"], "bank", "SGD").unwrap().id;
    let parent_loan = accounts::ensure_account(&c, parent.id, AccountType::Asset, &["Loan to Blue Harbor"], "receivable", "SGD").unwrap().id;
    let sub_bank = accounts::ensure_account(&c, subsidiary.id, AccountType::Asset, &["Bank"], "bank", "USD").unwrap().id;
    let sub_loan = accounts::ensure_account(&c, subsidiary.id, AccountType::Liability, &["Loan from Northwind"], "loan", "USD").unwrap().id;
    for id in [parent_loan, sub_loan] {
        c.execute("UPDATE accounts SET intercompany = 1 WHERE id = ?1", [id]).unwrap();
    }
    let mut entry = |entity: i64, date: &str, debit: i64, amount: &str, credit: i64| {
        let mut input = EntryInput::new(entity, date.parse().unwrap());
        input.status = EntryStatus::Posted;
        input.payee = "Intercompany loan".into();
        input.postings = vec![PostingInput::new(debit, amount.parse().unwrap()), PostingInput::balancing(credit)];
        journal::create_entry(&mut c, input).unwrap()
    };
    let a = entry(parent.id, "2026-03-02", parent_loan, "1350.00", parent_bank);
    let b = entry(subsidiary.id, "2026-03-03", sub_bank, "1000.00", sub_loan);
    let link_uid = intervault::link(&mut c, &a.uid, &b.uid, "loan", "").unwrap().uid;
    drop(c);
    Books { dir, parent: parent.id, subsidiary: subsidiary.id, a, b, link_uid }
}

impl Books {
    fn db(&self) -> Db {
        Db::open(self.dir.join("ledger.db")).unwrap()
    }

    fn run(&self, args: &[&str], agent: bool) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_means"));
        cmd.arg("--db").arg(self.dir.join("ledger.db")).args(args).env_remove("MEANS_AGENT");
        if agent {
            cmd.env("MEANS_AGENT", "1");
        }
        cmd.output().expect("run means")
    }

    /// A command that succeeds; its stdout.
    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args, false);
        assert!(out.status.success(), "means {args:?} failed: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    }

    /// A compact command that prints one JSON object; the object and the exit code.
    fn json(&self, args: &[&str], agent: bool) -> (Value, i32) {
        let out = self.run(args, agent);
        let text = String::from_utf8(out.stdout).unwrap();
        assert_eq!(text.lines().count(), 1, "means {args:?} must print one JSON line: {text}\nstderr: {}", String::from_utf8_lossy(&out.stderr));
        (serde_json::from_str(&text).unwrap(), out.status.code().unwrap())
    }

    fn status(&self, entry: &JournalEntry) -> EntryStatus {
        journal::get_entry(&self.db().conn(), entry.id).unwrap().status
    }

    fn lock(&self, entity: i64, through: &str) {
        entities::update_entity(&mut self.db().conn(), entity, "", "", "", Some(through.parse().unwrap()), false).unwrap();
    }
}

#[test]
fn the_void_preview_lists_the_other_leg_and_the_apply_needs_cascade() {
    let f = books();
    let id = f.a.id.to_string();

    // The text preview names the entity, the id, the date and the amount of the other leg.
    let text = f.ok(&["void", &id, "--reason", "wrong lender"]);
    assert!(text.contains("Linked entries in other entities (1)"), "{text}");
    assert!(text.contains(&format!("Blue Harbor LLC entry #{} ({}) on 2026-03-03, -1000 USD, posted", f.b.id, f.b.uid)), "{text}");
    assert!(text.contains("--yes --cascade") && text.contains("means intervault unlink"), "{text}");

    let out = f.run(&["void", &id, "--reason", "wrong lender", "--json"], false);
    assert!(out.status.success());
    let preview: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!((preview["applied"].as_bool(), preview["needs_cascade"].as_bool()), (Some(false), Some(true)));
    // The preview shows the stored entry with its real status, not the entry as the void would leave it.
    assert_eq!(preview["entry"]["status"], "posted");
    assert!(preview["entry"]["reversed_by_id"].is_null(), "{preview}");
    let other = &preview["linked_entries"][0];
    assert_eq!(other["entity_name"], "Blue Harbor LLC");
    assert_eq!(other["entry_id"], f.b.id);
    assert_eq!(other["entry_uid"], f.b.uid.as_str());
    assert_eq!(other["date"], "2026-03-03");
    assert_eq!(other["amount"]["minor"], "-100000");
    assert_eq!(other["cascade_action"], "void");

    // `means entry void --dry-run` lists the same entry and saves nothing. An agent can run it.
    let (dry, code) = f.json(&["entry", "void", &f.a.uid, "--reason", "wrong lender", "--dry-run"], true);
    assert_eq!(code, 0, "{dry}");
    assert_eq!((dry["applied"].as_bool(), dry["needs_cascade"].as_bool()), (Some(false), Some(true)));
    assert_eq!(dry["entry"]["status"], "posted");
    assert_eq!(dry["linked_entries"][0]["entry_uid"], f.b.uid.as_str());
    assert_eq!((f.status(&f.a), f.status(&f.b)), (EntryStatus::Posted, EntryStatus::Posted));
}

#[test]
fn a_cross_entity_void_is_refused_without_cascade() {
    let f = books();
    let out = f.run(&["void", &f.a.id.to_string(), "--reason", "wrong lender", "--yes"], false);
    assert_eq!(out.status.code(), Some(1));
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("Blue Harbor LLC") && said.contains(&f.b.uid) && said.contains("void the linked entries with cascade"), "{said}");

    let (refused, code) = f.json(&["entry", "void", &f.a.uid, "--reason", "wrong lender"], false);
    assert_eq!(code, 1);
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["error"]["code"], "invalid_input");
    assert!(refused["error"]["message"].as_str().unwrap().contains(&f.b.uid), "{refused}");
    assert_eq!((f.status(&f.a), f.status(&f.b)), (EntryStatus::Posted, EntryStatus::Posted));
}

#[test]
fn a_cascade_void_voids_both_legs_and_says_so() {
    let f = books();
    let (out, code) = f.json(&["entry", "void", &f.a.uid, "--reason", "wrong lender", "--cascade"], false);
    assert_eq!(code, 0, "{out}");
    assert_eq!((out["applied"].as_bool(), out["cascade"].as_bool()), (Some(true), Some(true)));
    assert_eq!(out["linked_entries"][0]["entry_uid"], f.b.uid.as_str());
    assert_eq!(out["linked_entries"][0]["voided"], true);
    assert_eq!((f.status(&f.a), f.status(&f.b)), (EntryStatus::Void, EntryStatus::Void));

    let history = audit::history(&f.db().conn(), "journal_entries", f.a.id).unwrap();
    let event: Value = serde_json::from_str(history.iter().rev().find(|h| h.action == "void").unwrap().after.as_deref().unwrap()).unwrap();
    assert_eq!(event["cascade"], true);
    assert_eq!(event["linked_entry_uids"][0], f.b.uid.as_str());
}

#[test]
fn the_human_unlink_is_audited_and_a_leg_only_void_ignores_the_other_lock() {
    let f = books();
    f.lock(f.subsidiary, "2026-03-31");

    // A posted pair: an agent cannot unlink it, and a human must name the actor.
    let (agent, _) = f.json(&["intervault", "unlink", &f.link_uid, "--actor", "Sample Reviewer", "--reason", "wrong lender", "--json"], true);
    assert_eq!(agent["error"]["code"], "agent_not_allowed");
    let (no_actor, _) = f.json(&["intervault", "unlink", &f.link_uid, "--reason", "wrong lender", "--json"], false);
    assert_eq!(no_actor["error"]["code"], "invalid_input");
    // The entry of the subsidiary is in a locked period.
    let (locked, _) = f.json(&["intervault", "unlink", &f.link_uid, "--actor", "Sample Reviewer", "--reason", "wrong lender", "--json"], false);
    assert_eq!(locked["error"]["code"], "locked_period");

    let (out, code) = f.json(&["intervault", "unlink", &f.link_uid, "--actor", "Sample Reviewer", "--reason", "wrong lender", "--force-locked", "--json"], false);
    assert_eq!(code, 0, "{out}");
    assert_eq!((out["actor"].as_str(), out["reason"].as_str(), out["force_locked"].as_bool()), (Some("Sample Reviewer"), Some("wrong lender"), Some(true)));
    assert_eq!(out["locked_entry_uids"][0], f.b.uid.as_str());
    let event = audit::get(&f.db().conn(), out["audit_event_id"].as_i64().unwrap()).unwrap();
    assert_eq!((event.action.as_str(), event.actor.as_str(), event.reason.as_str()), ("unlink", "Sample Reviewer", "wrong lender"));
    let after: Value = serde_json::from_str(event.after.as_deref().unwrap()).unwrap();
    assert_eq!((after["entry_a_uid"].as_str(), after["entry_b_uid"].as_str()), (Some(f.a.uid.as_str()), Some(f.b.uid.as_str())));
    assert_eq!(after["force_locked"], true);

    // The close check of the parent reports the entry again.
    let (close, _) = f.json(&["close", "check", "--entity", &f.parent.to_string(), "--from", "2026-03-01", "--to", "2026-03-31", "--json"], false);
    let codes: Vec<&str> = close["report"]["findings"].as_array().unwrap().iter().filter_map(|x| x["code"].as_str()).collect();
    assert!(codes.contains(&"intercompany_out_of_balance"), "{codes:?}");

    // The void of A now touches A only, although the period of the subsidiary is locked.
    let text = f.ok(&["void", &f.a.id.to_string(), "--reason", "wrong lender", "--yes"]);
    assert!(!text.contains("Linked entries"), "{text}");
    assert_eq!((f.status(&f.a), f.status(&f.b)), (EntryStatus::Void, EntryStatus::Posted));
}

#[test]
fn a_cascade_records_the_actor_on_each_audit_event() {
    let f = books();
    let (out, code) = f.json(&["entry", "void", &f.a.uid, "--reason", "wrong lender", "--cascade", "--actor", "Sample Reviewer"], false);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out["actor"], "Sample Reviewer");
    let conn = f.db();
    let conn = conn.conn();
    for id in [f.a.id, f.b.id] {
        let history = audit::history(&conn, "journal_entries", id).unwrap();
        let event = history.iter().rev().find(|h| h.action == "void").unwrap();
        assert_eq!(event.actor, "Sample Reviewer");
    }
    let link_id: i64 = conn.query_row("SELECT id FROM intervault_links WHERE uid = ?1", [&f.link_uid], |r| r.get(0)).unwrap();
    let link_event = audit::history(&conn, "intervault_links", link_id).unwrap().pop().unwrap();
    assert_eq!((link_event.action.as_str(), link_event.actor.as_str()), ("void", "Sample Reviewer"));
}

#[test]
fn means_void_with_cascade_takes_the_actor_from_user() {
    let f = books();
    let out = Command::new(env!("CARGO_BIN_EXE_means"))
        .arg("--db")
        .arg(f.dir.join("ledger.db"))
        .args(["void", &f.a.id.to_string(), "--reason", "wrong lender", "--yes", "--cascade"])
        .env_remove("MEANS_AGENT")
        .env("USER", "sample-user")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let conn = f.db();
    let conn = conn.conn();
    for id in [f.a.id, f.b.id] {
        let history = audit::history(&conn, "journal_entries", id).unwrap();
        assert_eq!(history.iter().rev().find(|h| h.action == "void").unwrap().actor, "sample-user");
    }
    // --actor needs --cascade.
    let refused = f.run(&["void", &f.a.id.to_string(), "--reason", "x", "--actor", "Sample Reviewer"], false);
    assert!(!refused.status.success());
}

#[test]
fn a_cascade_preview_saves_nothing_and_shows_the_lock() {
    let f = books();
    f.lock(f.subsidiary, "2026-03-31");
    let (dry, code) = f.json(&["entry", "void", &f.a.uid, "--reason", "wrong lender", "--cascade", "--dry-run"], false);
    assert_eq!(code, 0, "{dry}");
    assert_eq!((dry["applied"].as_bool(), dry["cascade"].as_bool()), (Some(false), Some(true)));
    assert_eq!(dry["linked_entries"][0]["locked"], true);
    assert_eq!(dry["linked_entries"][0]["voided"], false);
    let text = f.ok(&["void", &f.a.id.to_string(), "--reason", "wrong lender", "--cascade"]);
    assert!(text.contains("its period is locked"), "{text}");
    assert_eq!((f.status(&f.a), f.status(&f.b)), (EntryStatus::Posted, EntryStatus::Posted));
}
