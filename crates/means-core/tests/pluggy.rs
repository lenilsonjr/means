//! The Pluggy channel: a recorded payload in, statement lines out, and a pull that leaves one
//! canonical file per Pluggy account in the inbox. No test here reaches the network: the transport
//! is a stub over the same fixtures `docs/import-formats.md` describes.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use chrono::NaiveDate;
use means_core::imports::pluggy::{self, Credentials, PullOptions, PullReport, Request, Response, Transport};
use means_core::imports::{self, inbox};
use means_core::model::*;
use means_core::{accounts, entities, Db, Result};
use rust_decimal::prelude::FromStr;
use rust_decimal::Decimal;
use serde_json::{json, Value};

const BASE: &str = "https://stub.pluggy.test";
const API_KEY: &str = "api-key-from-the-stub";
const ITEM: &str = "a1b2c3d4-0000-4000-8000-000000000001";
const BANK_ACCOUNT: &str = "b8e1c1a2-1111-4000-8000-000000000011";
const CREDIT_ACCOUNT: &str = "b8e1c1a2-1111-4000-8000-000000000012";

fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn fixture_json(name: &str) -> Value {
    serde_json::from_slice(&fixture(name)).unwrap()
}

fn d(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap()
}

fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

fn credentials() -> Credentials {
    Credentials { client_id: "client-id-of-the-application".into(), client_secret: "pluggy-secret-9f2c1e".into() }
}

/// A scratch folder that goes away with the test.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("means-pluggy-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn files(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.0).unwrap().flatten().filter(|e| e.path().is_file()).map(|e| e.file_name().to_string_lossy().to_string()).collect();
        names.sort();
        names
    }

    fn read(&self, name: &str) -> Value {
        serde_json::from_slice(&std::fs::read(self.0.join(name)).unwrap()).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Pluggy, from the two canonical fixtures: the accounts it lists and the transactions it pages.
struct Stub {
    files: Vec<Value>,
    page_size: usize,
    connector_id: u64,
    calls: Mutex<Vec<String>>,
    /// Item states, read in order; the last one repeats.
    statuses: Mutex<Vec<String>>,
    /// What `/auth` answers, when it is not an API key.
    auth: Option<(u16, String)>,
    /// Every page of transactions answers HTTP 429.
    rate_limited: bool,
    /// Every page of transactions offers another cursor, for ever.
    endless: bool,
}

impl Stub {
    fn new() -> Stub {
        Stub {
            files: vec![fixture_json("pluggy_bank.json"), fixture_json("pluggy_credit.json")],
            page_size: 2,
            connector_id: 1,
            calls: Mutex::new(Vec::new()),
            statuses: Mutex::new(vec!["UPDATING".into(), "UPDATED".into()]),
            auth: None,
            rate_limited: false,
            endless: false,
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

fn ok(body: Value) -> Result<Response> {
    Ok(Response { status: 200, body: body.to_string() })
}

/// The stub answers on any host: the pull is checked against a real base URL, and a test may give
/// it a loopback one.
fn path_and_query(url: &str) -> &str {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    rest.find('/').map(|i| &rest[i..]).unwrap_or("/")
}

fn param(url: &str, name: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    query.split('&').find_map(|p| p.strip_prefix(&format!("{name}="))).map(|v| v.to_string())
}

impl Transport for Stub {
    fn send(&self, request: Request<'_>) -> Result<Response> {
        self.calls.lock().unwrap().push(format!("{} {} key={}", request.method, request.url, request.api_key));
        let url = path_and_query(request.url);
        if url.starts_with("/auth") {
            return match &self.auth {
                Some((status, body)) => Ok(Response { status: *status, body: body.clone() }),
                None => ok(json!({"apiKey": API_KEY})),
            };
        }
        if request.api_key != API_KEY {
            return Ok(Response { status: 403, body: json!({"message": "missing api key"}).to_string() });
        }
        if let Some(item) = url.strip_prefix("/items/") {
            if request.method == "PATCH" {
                if self.connector_id == 200 {
                    return Ok(Response { status: 400, body: json!({"message": "MeuPluggy item cant be updated"}).to_string() });
                }
                return ok(json!({"id": item, "status": "UPDATING"}));
            }
            let mut statuses = self.statuses.lock().unwrap();
            let status = if statuses.len() > 1 { statuses.remove(0) } else { statuses[0].clone() };
            return ok(json!({"id": item, "status": status, "connector": {"id": self.connector_id}}));
        }
        if url.starts_with("/accounts") {
            return ok(json!({"results": self.files.iter().map(|f| f["account"].clone()).collect::<Vec<Value>>(), "total": self.files.len()}));
        }
        if url.starts_with("/v2/transactions") {
            if self.rate_limited {
                return Ok(Response { status: 429, body: json!({"message": "too many requests"}).to_string() });
            }
            let account = param(url, "accountId").unwrap_or_default();
            let all = self.files.iter().find(|f| f["account"]["id"] == Value::String(account.clone())).map(|f| f["transactions"].as_array().cloned().unwrap_or_default()).unwrap_or_default();
            let from: usize = param(url, "after").and_then(|a| a.strip_prefix("cursor-").and_then(|n| n.parse().ok())).unwrap_or(0);
            let to = (from + self.page_size).min(all.len());
            let next = if self.endless {
                json!(format!("cursor-{from}"))
            } else if to < all.len() {
                json!(format!("cursor-{to}"))
            } else {
                Value::Null
            };
            return ok(json!({"results": all[from..to], "next": next, "total": all.len()}));
        }
        Ok(Response { status: 404, body: json!({"message": format!("the stub has no {} {url}", request.method)}).to_string() })
    }
}

struct Bank {
    db: Db,
    account: Account,
}

fn bank() -> Bank {
    let db = Db::open_memory().unwrap();
    let account = {
        let mut conn = db.conn();
        let entity = entities::create_entity(&mut conn, "Owner", "person", "BR", "BRL").unwrap();
        accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank", "Inter"], "bank", "BRL").unwrap()
    };
    Bank { db, account }
}

fn postings(db: &Db) -> i64 {
    db.conn().query_row("SELECT COUNT(*) FROM postings", [], |r| r.get(0)).unwrap()
}

fn pull(db: &Db, stub: &Stub, options: &PullOptions) -> Result<PullReport> {
    let mut conn = db.conn();
    pluggy::pull(&mut conn, stub, &credentials(), options)
}

fn options(inbox: &Scratch) -> PullOptions {
    PullOptions { base_url: BASE.into(), inbox: inbox.0.clone(), item: Some(ITEM.into()), poll_interval: Duration::ZERO, ..Default::default() }
}

#[test]
fn a_bank_payload_becomes_statement_lines() {
    let content = fixture("pluggy_bank.json");
    assert_eq!(imports::detect_source(&format!("pluggy-20260918T091230412Z-{BANK_ACCOUNT}.json"), &content), "pluggy_json");
    let out = imports::parse("pluggy_json", &content, None).unwrap();
    assert_eq!(out.detected_source, "pluggy_json");
    assert_eq!(out.currency, "BRL");
    assert_eq!(out.account_ref, BANK_ACCOUNT);
    assert_eq!(out.lines.len(), 3, "the fourth transaction is not settled");
    assert_eq!(out.skipped_records, 1);

    let first = &out.lines[0];
    assert_eq!(first.reference, "1f6b9a20-2222-4000-8000-000000000101");
    assert_eq!(first.date, Some(date("2026-09-14")));
    assert_eq!(first.amount, Some(d("-34.04")));
    assert_eq!(first.currency, "BRL");
    assert_eq!(first.description, "Pix enviado: Padaria do Bairro — PADARIA DO BAIRRO LTDA");
    assert_eq!(first.balance_after, Some(d("2381.55")));
    assert_eq!(first.raw, fixture_json("pluggy_bank.json")["transactions"][0], "the transaction is kept verbatim");
    assert!(first.skip.is_none());

    let empty_description = &out.lines[1];
    assert_eq!(empty_description.description, "PIX RECEBIDO ACME COMERCIO LTDA", "an empty description falls back to descriptionRaw");
    assert_eq!(empty_description.amount, Some(d("1200.00")));

    assert_eq!(out.period_from, Some(date("2026-09-14")));
    assert_eq!(out.period_to, Some(date("2026-09-16")));
    assert_eq!(out.closing_balance, Some(d("3431.55")));
    assert_eq!(out.closing_date, Some(date("2026-09-16")));
}

#[test]
fn the_sign_follows_the_account_type() {
    let bank_lines = imports::parse("pluggy_json", &fixture("pluggy_bank.json"), None).unwrap().lines;
    assert_eq!(bank_lines[0].amount, Some(d("-34.04")), "a debit on a BANK account stays negative");
    assert_eq!(bank_lines[1].amount, Some(d("1200.00")), "a credit on a BANK account stays positive");

    let card = imports::parse("pluggy_json", &fixture("pluggy_credit.json"), None).unwrap();
    assert_eq!(card.lines.len(), 2);
    assert_eq!(card.lines[0].description, "PAGAMENTO FATURA");
    assert_eq!(card.lines[0].amount, Some(d("1240.55")), "a payment of the card is money into the card account");
    assert_eq!(card.lines[1].description, "RESTAURANTE SUSHI YAMA");
    assert_eq!(card.lines[1].amount, Some(d("-89.90")), "a new charge is money out of the card account");
    assert_eq!(card.lines[0].balance_after, None, "the payment carries no balance");
    assert_eq!(card.lines[1].balance_after, Some(d("-1330.45")), "the running balance of a card is inverted with its amounts");
    assert_eq!(card.closing_balance, Some(d("-1330.45")));
}

#[test]
fn a_transaction_that_is_not_settled_makes_no_line() {
    let bank = bank();
    let content = fixture("pluggy_bank.json");
    let out = {
        let mut conn = bank.db.conn();
        imports::run_import(&mut conn, imports::ImportRequest::new("auto", Some(bank.account.id), &format!("pluggy-20260918T091230412Z-{BANK_ACCOUNT}.json"), &content)).unwrap()
    };
    assert_eq!(out.import.source, "pluggy_json");
    assert_eq!(out.import.lines_count, 3);
    assert_eq!(out.import.skipped_count, 1, "the stats report the transaction that is not settled");
    assert!(out.lines.iter().all(|l| l.reference != "1f6b9a20-2222-4000-8000-000000000104"), "a PENDING transaction has no statement line");
    assert_eq!(out.import.created_count, 3);
}

#[test]
fn a_missing_credential_names_the_variable_to_set() {
    std::env::remove_var("PLUGGY_CLIENT_ID");
    std::env::set_var("PLUGGY_CLIENT_SECRET", "not-read-here");
    let message = Credentials::from_env().unwrap_err().to_string();
    assert!(message.contains("PLUGGY_CLIENT_ID"), "{message}");
    std::env::set_var("PLUGGY_CLIENT_ID", "an-id");
    std::env::remove_var("PLUGGY_CLIENT_SECRET");
    let message = Credentials::from_env().unwrap_err().to_string();
    assert!(message.contains("PLUGGY_CLIENT_SECRET"), "{message}");
    std::env::remove_var("PLUGGY_CLIENT_ID");
}

#[test]
fn a_refused_authentication_repeats_no_part_of_the_secret() {
    let inbox = Scratch::new("auth");
    let bank = bank();
    let echoed = json!({"code": 403, "message": format!("clientSecret {} is not valid", credentials().client_secret)}).to_string();
    let stub = Stub { auth: Some((403, echoed)), ..Stub::new() };
    let message = pull(&bank.db, &stub, &options(&inbox)).unwrap_err().to_string();
    assert!(!message.contains(&credentials().client_secret), "{message}");
    assert!(!message.contains("9f2c1e"), "{message}");
    assert!(message.contains("PLUGGY_CLIENT_SECRET"), "{message}");
    assert!(inbox.files().is_empty());
}

#[test]
fn the_pull_writes_one_file_per_account_and_follows_the_cursor() {
    let inbox = Scratch::new("pull");
    let bank = bank();
    let stub = Stub::new();
    let report = pull(&bank.db, &stub, &options(&inbox)).unwrap();

    assert_eq!(report.items.len(), 1);
    assert_eq!(report.items[0].status, "UPDATED");
    assert_eq!(report.accounts.len(), 2);
    assert_eq!(report.accounts[0].provider_account_id, BANK_ACCOUNT);
    assert_eq!(report.accounts[0].kind, "BANK");
    assert_eq!(report.accounts[0].transactions, 4);
    assert_eq!(report.accounts[0].pages, 2, "four transactions over pages of two");
    assert_eq!(report.accounts[1].provider_account_id, CREDIT_ACCOUNT);
    assert_eq!(report.accounts[1].pages, 1);

    let calls = stub.calls();
    assert!(calls.iter().any(|c| c.starts_with(&format!("PATCH {BASE}/items/{ITEM}"))), "the item is asked for an update");
    assert_eq!(calls.iter().filter(|c| c.starts_with(&format!("GET {BASE}/items/{ITEM}"))).count(), 2, "the connector is read before refresh and UPDATED is checked afterward");
    assert!(calls.iter().any(|c| c.contains("/v2/transactions?accountId=") && c.contains("after=cursor-2")), "the second page followed the cursor: {calls:?}");
    assert!(calls.iter().filter(|c| !c.contains("/auth")).all(|c| c.ends_with(&format!("key={API_KEY}"))), "every other call carries the api key");

    let files = inbox.files();
    assert_eq!(files.len(), 2);
    assert!(files.iter().all(|f| f.starts_with("pluggy-") && f.ends_with(".json")), "{files:?}");
    let bank_file = files.iter().find(|f| f.ends_with(&format!("{BANK_ACCOUNT}.json"))).expect("a file for the bank account");
    let written = inbox.read(bank_file);
    let recorded = fixture_json("pluggy_bank.json");
    assert_eq!(written["channel"], "pluggy");
    assert_eq!(written["itemId"], ITEM);
    assert_eq!(written["account"], recorded["account"]);
    assert_eq!(written["transactions"], recorded["transactions"], "the file holds what Pluggy answered, verbatim");
    assert!(written["pulledAt"].as_str().unwrap().len() > 10);

    let conn = bank.db.conn();
    let connections = pluggy::connections(&conn).unwrap();
    assert_eq!(connections.len(), 2);
    assert_eq!(connections[0].item_id, ITEM);
    assert_eq!(connections[0].provider_account_id, BANK_ACCOUNT);
    assert_eq!(connections[0].currency, "BRL");
    assert_eq!(connections[0].account_id, None, "a Pluggy account nobody has placed is not guessed");
    assert!(!connections[0].cursor.is_empty(), "the cursor the next pull sends");
    assert_eq!(connections[0].cursor, connections[0].last_pull_at);
}

#[test]
fn a_dry_run_writes_nothing_and_asks_for_no_update() {
    let inbox = Scratch::new("dry");
    let bank = bank();
    let stub = Stub::new();
    let report = pull(&bank.db, &stub, &PullOptions { dry_run: true, ..options(&inbox) }).unwrap();
    assert!(report.dry_run);
    assert_eq!(report.accounts.len(), 2);
    assert_eq!(report.accounts[0].transactions, 4, "it reports what it would fetch");
    assert!(report.accounts.iter().all(|a| a.file.is_empty()));
    assert!(inbox.files().is_empty());
    assert!(!stub.calls().iter().any(|c| c.starts_with("PATCH")), "a dry run refreshes no item");
    let conn = bank.db.conn();
    assert!(pluggy::connections(&conn).unwrap().is_empty(), "a dry run moves no cursor");
}

#[test]
fn an_unplaced_pluggy_account_waits_as_a_pending_import() {
    let inbox = Scratch::new("pending");
    let bank = bank();
    pull(&bank.db, &Stub::new(), &options(&inbox)).unwrap();
    let outcomes = {
        let mut conn = bank.db.conn();
        inbox::scan(&mut conn, &inbox.0).unwrap()
    };
    assert_eq!(outcomes.len(), 2);
    assert!(outcomes.iter().all(|o| o.action == "pending"), "{outcomes:?}");
    assert_eq!(postings(&bank.db), 0, "nothing is posted until the account is chosen");
}

#[test]
fn a_second_pull_of_the_same_window_adds_no_posting() {
    let inbox = Scratch::new("twice");
    let bank = bank();
    let since = Some(date("2026-09-01"));
    pull(&bank.db, &Stub::new(), &PullOptions { since, ..options(&inbox) }).unwrap();
    let pending = {
        let mut conn = bank.db.conn();
        inbox::scan(&mut conn, &inbox.0).unwrap()
    };
    let bank_import = pending.iter().find(|o| o.file.ends_with(&format!("{BANK_ACCOUNT}.json"))).unwrap().import_id.unwrap();
    let first = {
        let mut conn = bank.db.conn();
        inbox::complete_import(&mut conn, bank_import, bank.account.id).unwrap()
    };
    assert_eq!(first.import.created_count, 3);
    let after_first = postings(&bank.db);
    assert!(after_first > 0);

    // Two pulls inside one millisecond would write the same bytes, and the checksum alone would
    // refuse the second file; the ledger work above takes longer than that, and this makes it sure.
    std::thread::sleep(Duration::from_millis(2));
    pull(&bank.db, &Stub::new(), &PullOptions { since, ..options(&inbox) }).unwrap();
    let again = {
        let mut conn = bank.db.conn();
        inbox::scan(&mut conn, &inbox.0).unwrap()
    };
    let repeated = again.iter().find(|o| o.file.ends_with(&format!("{BANK_ACCOUNT}.json"))).unwrap();
    assert_eq!(repeated.action, "imported", "the Pluggy account is now known, so the file imports itself");
    let import = {
        let conn = bank.db.conn();
        imports::get_import(&conn, repeated.import_id.unwrap()).unwrap().0
    };
    assert_eq!(import.duplicate_count, 3, "every line repeats one already imported");
    assert_eq!(import.created_count, 0);
    assert_eq!(postings(&bank.db), after_first, "the second pull creates no posting");
}

/// Pull once, let the inbox take the files, and give the bank account's pending import its account.
fn place_the_bank_account(bank: &Bank, inbox: &Scratch) -> String {
    let outcomes = {
        let mut conn = bank.db.conn();
        inbox::scan(&mut conn, &inbox.0).unwrap()
    };
    let pending = outcomes.into_iter().find(|o| o.file.ends_with(&format!("{BANK_ACCOUNT}.json"))).expect("a pending import for the bank account");
    let mut conn = bank.db.conn();
    inbox::complete_import(&mut conn, pending.import_id.unwrap(), bank.account.id).unwrap();
    pending.file
}

#[test]
fn a_merged_account_takes_the_connection_with_it() {
    let inbox = Scratch::new("merge");
    let bank = bank();
    let target = {
        let conn = bank.db.conn();
        accounts::ensure_account(&conn, bank.account.entity_id, AccountType::Asset, &["Bank", "Inter (new)"], "bank", "BRL").unwrap()
    };
    pull(&bank.db, &Stub::new(), &options(&inbox)).unwrap();
    let filename = place_the_bank_account(&bank, &inbox);

    let report = {
        let mut conn = bank.db.conn();
        accounts::merge_accounts(&mut conn, bank.account.id, target.id).unwrap()
    };
    assert!(report.source_deleted, "the merged account is gone, and the connection row may not hold it");
    let conn = bank.db.conn();
    let connection = pluggy::connections(&conn).unwrap().into_iter().find(|c| c.provider_account_id == BANK_ACCOUNT).unwrap();
    assert_eq!(connection.account_id, Some(target.id), "the connection follows the account it pointed at");
    assert_eq!(inbox::resolve_account(&conn, "pluggy_json", &filename).unwrap(), Some(target.id));
}

#[test]
fn a_closed_account_sends_the_next_file_back_to_pending() {
    let inbox = Scratch::new("closed");
    let bank = bank();
    pull(&bank.db, &Stub::new(), &options(&inbox)).unwrap();
    place_the_bank_account(&bank, &inbox);
    let posted = postings(&bank.db);
    assert!(posted > 0);
    {
        let conn = bank.db.conn();
        accounts::close_account(&conn, bank.account.id, false).unwrap();
    }

    pull(&bank.db, &Stub::new(), &options(&inbox)).unwrap();
    let outcomes = {
        let mut conn = bank.db.conn();
        inbox::scan(&mut conn, &inbox.0).unwrap()
    };
    let again = outcomes.iter().find(|o| o.file.ends_with(&format!("{BANK_ACCOUNT}.json"))).unwrap();
    assert_eq!(again.action, "pending", "a closed account cannot take a statement");
    assert_eq!(postings(&bank.db), posted, "and nothing is posted into it");
}

#[test]
fn a_json_file_that_is_not_a_pluggy_payload_is_left_alone() {
    let inbox = Scratch::new("otherjson");
    let bank = bank();
    std::fs::write(inbox.0.join("index.json"), br#"{"name": "a file that is not a statement"}"#).unwrap();
    let outcomes = {
        let mut conn = bank.db.conn();
        inbox::scan(&mut conn, &inbox.0).unwrap()
    };
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].action, "ignored", "{outcomes:?}");
    assert!(inbox.files().contains(&"index.json".to_string()), "the file stays where it lies");
    let conn = bank.db.conn();
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM imports", [], |r| r.get::<_, i64>(0)).unwrap(), 0, "and no import row holds it");
}

#[test]
fn the_api_base_must_be_a_host_the_credentials_may_reach() {
    let inbox = Scratch::new("api");
    let bank = bank();
    // A host that merely begins with 127. is a name like any other: it resolves where its owner says.
    let refused = ["api.pluggy.ai", "ftp://api.pluggy.ai", "http://api.pluggy.ai", "http://198.51.100.7:8080", "http://127.0.0.1.tunnel.example", "http://127.example.com:8080", "https://"];
    for bad in refused {
        let stub = Stub::new();
        let message = pull(&bank.db, &stub, &PullOptions { base_url: bad.into(), ..options(&inbox) }).unwrap_err().to_string();
        assert!(message.contains("--api"), "{bad}: {message}");
        assert!(stub.calls().is_empty(), "{bad}: nothing was sent to it");
    }
    // Plain http to a loopback host is how a test and a local proxy reach the channel.
    let stub = Stub::new();
    let report = pull(&bank.db, &stub, &PullOptions { base_url: "http://127.0.0.1:7770".into(), ..options(&inbox) }).unwrap();
    assert_eq!(report.accounts.len(), 2);
}

#[test]
fn a_rate_limit_is_reported_and_not_worked_around() {
    let inbox = Scratch::new("limit");
    let bank = bank();
    let stub = Stub { rate_limited: true, ..Stub::new() };
    let message = pull(&bank.db, &stub, &options(&inbox)).unwrap_err().to_string();
    assert!(message.contains("429"), "{message}");
    assert!(message.contains("limits"), "{message}");
    assert_eq!(stub.calls().iter().filter(|c| c.contains("/v2/transactions")).count(), 1, "it asks once and stops");
    assert!(inbox.files().is_empty());
}

#[test]
fn an_item_that_needs_a_person_stops_the_pull() {
    for state in ["LOGIN_ERROR", "WAITING_USER_INPUT", "OUTDATED"] {
        let inbox = Scratch::new(state);
        let bank = bank();
        let stub = Stub { statuses: Mutex::new(vec![state.into()]), ..Stub::new() };
        let message = pull(&bank.db, &stub, &options(&inbox)).unwrap_err().to_string();
        assert!(message.contains(state), "{message}");
        assert!(message.contains("meu.pluggy.ai"), "{message}");
        assert!(inbox.files().is_empty());
    }
}

#[test]
fn an_item_that_never_finishes_updating_gives_up() {
    let inbox = Scratch::new("updating");
    let bank = bank();
    let stub = Stub { statuses: Mutex::new(vec!["UPDATING".into()]), ..Stub::new() };
    let message = pull(&bank.db, &stub, &options(&inbox)).unwrap_err().to_string();
    assert!(message.contains("still UPDATING"), "{message}");
    // One connector read, then UPDATE_ATTEMPTS status reads, and no transaction asked for.
    assert_eq!(stub.calls().iter().filter(|c| c.starts_with(&format!("GET {BASE}/items/"))).count(), 101);
    assert!(!stub.calls().iter().any(|c| c.contains("/v2/transactions")));
    assert!(inbox.files().is_empty());
}

#[test]
fn a_server_that_never_stops_paging_is_refused() {
    let inbox = Scratch::new("endless");
    let bank = bank();
    let stub = Stub { endless: true, ..Stub::new() };
    let message = pull(&bank.db, &stub, &options(&inbox)).unwrap_err().to_string();
    assert!(message.contains("another page"), "{message}");
    assert!(inbox.files().is_empty(), "a truncated account writes no file");
}

#[test]
fn meupluggy_uses_current_data_without_refreshing() {
    for statuses in [vec!["UPDATED"], vec!["UPDATING", "UPDATED"]] {
        let db = Db::open_memory().unwrap();
        let inbox = Scratch::new("meupluggy");
        let stub = Stub { connector_id: 200, statuses: Mutex::new(statuses.iter().map(|s| s.to_string()).collect()), ..Stub::new() };
        let report = pull(&db, &stub, &options(&inbox)).unwrap();
        assert_eq!(report.items[0].status, "UPDATED");
        assert_eq!(inbox.files().len(), 2);
        assert!(!stub.calls().iter().any(|c| c.starts_with("PATCH")));
        assert_eq!(stub.calls().iter().filter(|c| c.starts_with(&format!("GET {BASE}/items/"))).count(), statuses.len());
        assert_eq!(pluggy::connections(&db.conn()).unwrap().len(), 2);
    }
}

#[test]
fn meupluggy_still_requires_updated_before_fetching() {
    for status in ["LOGIN_ERROR", "WAITING_USER_INPUT", "OUTDATED", "UPDATING", "UNKNOWN"] {
        let db = Db::open_memory().unwrap();
        let inbox = Scratch::new("meupluggy-status");
        let stub = Stub { connector_id: 200, statuses: Mutex::new(vec![status.into()]), ..Stub::new() };
        let message = pull(&db, &stub, &options(&inbox)).unwrap_err().to_string();
        assert!(message.contains(status), "{message}");
        assert!(!stub.calls().iter().any(|c| c.starts_with("PATCH") || c.contains("/accounts") || c.contains("/transactions")));
        assert!(inbox.files().is_empty());
        assert!(pluggy::connections(&db.conn()).unwrap().is_empty());
    }
}

#[test]
fn json_money_preserves_digits_beyond_floating_point_precision() {
    // These are JSON tokens, not Rust float literals: the original decimal digits matter.
    for (token, expected) in [
        ("90071992547409.93", "90071992547409.93"),
        ("123456789.12345678", "123456789.12345678"),
        ("1.2345678912345678e8", "123456789.12345678"),
        ("-123456789.12345678", "-123456789.12345678"),
        ("\"123456789.12345678\"", "123456789.12345678"),
    ] {
        for kind in ["BANK", "CREDIT"] {
            let payload = format!(
                r#"{{"channel":"pluggy","account":{{"id":"exact","type":"{kind}","currencyCode":"BRL"}},"transactions":[{{"id":"exact-txn","status":"POSTED","date":"2026-09-19","amount":{token},"balance":{token}}}]}}"#
            );
            let parsed = pluggy::parse(payload.as_bytes()).unwrap();
            let signed = if kind == "CREDIT" { -d(expected) } else { d(expected) };
            assert_eq!(parsed.lines[0].amount, Some(signed), "{kind} amount {token}");
            assert_eq!(parsed.lines[0].balance_after, Some(signed), "{kind} balance {token}");
            assert_eq!(parsed.closing_balance, Some(signed));
            assert!(parsed.lines[0].skip.is_none());
        }
    }
}

#[test]
fn json_money_stays_exact_through_pull_inbox_and_ledger() {
    let inbox = Scratch::new("exact-json-money");
    let bank = bank();
    let mut file = fixture_json("pluggy_bank.json");
    file["transactions"] = serde_json::from_str(
        r#"[{
        "id":"exact-transaction", "status":"POSTED", "date":"2026-09-19T00:00:00.000Z",
        "description":"Exact decimal", "currencyCode":"BRL",
        "amount":90071992547409.93, "balance":90071992547409.94
    }]"#,
    )
    .unwrap();
    let stub = Stub { files: vec![file], ..Stub::new() };
    pull(&bank.db, &stub, &options(&inbox)).unwrap();
    let filename = inbox.files().pop().unwrap();
    let written = inbox.read(&filename);
    assert_eq!(written["transactions"][0]["amount"].to_string(), "90071992547409.93");
    assert_eq!(written["transactions"][0]["balance"].to_string(), "90071992547409.94");
    place_the_bank_account(&bank, &inbox);
    let conn = bank.db.conn();
    let line = imports::list_lines(&conn, Some(bank.account.id), "created", None, 10).unwrap().pop().unwrap();
    assert_eq!(line.amount.map(|a| a.major()), Some(d("90071992547409.93")));
    assert_eq!(line.balance_after.map(|a| a.major()), Some(d("90071992547409.94")));
    let quantity: i64 = conn.query_row("SELECT quantity FROM postings WHERE id = ?1", [line.posting_id.unwrap()], |row| row.get(0)).unwrap();
    assert_eq!(quantity, 9_007_199_254_740_993, "all original cents reach the ledger");
}

struct Pagination {
    stub: Stub,
    next: String,
    urls: Mutex<Vec<String>>,
}

impl Transport for Pagination {
    fn send(&self, request: Request<'_>) -> Result<Response> {
        if request.url.contains("/v2/transactions") {
            let mut urls = self.urls.lock().unwrap();
            urls.push(request.url.into());
            return ok(json!({"results": [], "next": if urls.len() == 1 { Some(&self.next) } else { None }}));
        }
        self.stub.send(request)
    }
}

#[test]
fn pagination_cannot_send_credentials_to_another_origin() {
    let bank = bank();
    let inbox = Scratch::new("pagination-origins");
    for next in [
        "https://untrusted.test/steal",
        "http://stub.pluggy.test/v2/transactions",
        "https://stub.pluggy.test:444/v2/transactions",
        "//untrusted.test/steal",
        "https://stub.pluggy.test@untrusted.test/steal",
        "https://user@stub.pluggy.test/v2/transactions",
        "ftp://stub.pluggy.test/steal",
        "\\\\untrusted.test/steal",
    ] {
        let transport = Pagination { stub: Stub::new(), next: next.into(), urls: Mutex::new(vec![]) };
        let mut conn = bank.db.conn();
        let error = pluggy::pull(&mut conn, &transport, &credentials(), &PullOptions { dry_run: true, ..options(&inbox) }).unwrap_err();
        assert!(error.to_string().contains("origin"), "{next}: {error}");
        assert_eq!(transport.urls.lock().unwrap().len(), 1, "no second request for {next}");
        assert!(pluggy::connections(&conn).unwrap().is_empty());
    }
}

#[test]
fn pagination_accepts_same_origin_urls_and_encodes_opaque_cursors() {
    let bank = bank();
    let inbox = Scratch::new("pagination-cursors");
    for next in ["https://stub.pluggy.test/v2/transactions?after=two", "/v2/transactions?after=two", "cursor+with/&accountId=other#fragment"] {
        let mut stub = Stub::new();
        stub.files.truncate(1);
        let transport = Pagination { stub, next: next.into(), urls: Mutex::new(vec![]) };
        let mut conn = bank.db.conn();
        pluggy::pull(&mut conn, &transport, &credentials(), &PullOptions { dry_run: true, ..options(&inbox) }).unwrap();
        let urls = transport.urls.lock().unwrap();
        assert_eq!(urls.len(), 2);
        let second = url::Url::parse(&urls[1]).unwrap();
        assert_eq!(second.origin(), url::Url::parse(BASE).unwrap().origin());
        if next.starts_with("cursor") {
            let pairs: Vec<_> = second.query_pairs().collect();
            assert_eq!(pairs.iter().filter(|(k, _)| k == "accountId").count(), 1);
            assert_eq!(pairs.iter().find(|(k, _)| k == "after").unwrap().1, next);
            assert!(second.fragment().is_none());
        }
    }
}

#[test]
fn payment_counterparties_enrich_descriptions_without_changing_evidence_or_identity() {
    for (kind, amount, payment, expected) in [
        ("BANK", "2750", json!({"payer":"EXAMPLE SOFTWARE", "receiver":"Own name"}), "Pix — EXAMPLE SOFTWARE"),
        ("BANK", "-1234.56", json!({"payer":"Own name", "receiver":"EXAMPLE HEALTH"}), "Pix — EXAMPLE HEALTH"),
        ("BANK", "-1234.56", json!({"receiver":{"name":" EXAMPLE HEALTH "}}), "Pix — EXAMPLE HEALTH"),
        ("CREDIT", "1234.56", json!({"receiver":{"name":"Shop"},"payer":{"name":"Own name"}}), "Pix — Shop"),
        ("CREDIT", "-100", json!({"payer":{"name":"Payment sender"}}), "Pix — Payment sender"),
        ("BANK", "-10", json!({"payer":"Own name", "receiver":null}), "Pix"),
        ("BANK", "0", json!({"payer":"Own name", "receiver":"Other"}), "Pix"),
    ] {
        let tx = json!({"id":"stable-id", "date":"2026-08-20", "amount":amount, "description":"Pix", "paymentData":payment});
        let file = json!({"channel":"pluggy", "account":{"type":kind,"currencyCode":"BRL"}, "transactions":[tx.clone()]});
        let parsed = pluggy::parse(&serde_json::to_vec(&file).unwrap()).unwrap();
        assert_eq!(parsed.lines[0].description, expected);
        assert_eq!(parsed.lines[0].raw, tx);
        assert_eq!(parsed.lines[0].reference, "stable-id");
    }
    let file = json!({"channel":"pluggy", "account":{"type":"BANK","currencyCode":"BRL"}, "transactions":[
        {"id":"1", "date":"2026-08-20", "amount":-1, "description":"Payment EXAMPLE HEALTH", "paymentData":{"receiver":"example health"}},
        {"id":"2", "date":"2026-08-20", "amount":1, "description":"", "descriptionRaw":"Raw", "paymentData":{"payer":"Sender"}}
    ]});
    let parsed = pluggy::parse(&serde_json::to_vec(&file).unwrap()).unwrap();
    assert_eq!(parsed.lines[0].description, "Payment EXAMPLE HEALTH");
    assert_eq!(parsed.lines[1].description, "Raw — Sender");
}

#[test]
fn booking_cutoff_is_inclusive_account_scoped_and_does_not_consume_the_recording_cursor() {
    let db = Db::open_memory().unwrap();
    let inbox = Scratch::new("booked-from");
    let initial = Stub::new();
    pull(&db, &initial, &options(&inbox)).unwrap();
    let before = pluggy::connections(&db.conn()).unwrap();
    let stub = Stub::new();
    let opts = PullOptions { booked_from: Some(date("2026-09-15")), account: Some(BANK_ACCOUNT.into()), ..options(&inbox) };
    let report = pull(&db, &stub, &opts).unwrap();
    assert_eq!(report.accounts.len(), 1);
    assert!(report.accounts[0].excluded_before_booking_date > 0);
    assert_eq!(report.accounts[0].created_at_from, "");
    let content: Value = serde_json::from_slice(&std::fs::read(inbox.0.join(&report.accounts[0].file)).unwrap()).unwrap();
    assert_eq!(content["bookedFrom"], "2026-09-15");
    assert!(content["transactions"].as_array().unwrap().iter().all(|tx| tx["date"].as_str().unwrap()[..10] >= *"2026-09-15"));
    assert!(content["transactions"].as_array().unwrap().iter().any(|tx| tx["date"].as_str().unwrap().starts_with("2026-09-15")));
    for old in before {
        let after = pluggy::connections(&db.conn()).unwrap();
        assert_eq!(old.cursor, after.iter().find(|c| c.provider_account_id == old.provider_account_id).unwrap().cursor);
    }
    let calls = stub.calls.lock().unwrap();
    assert!(!calls.iter().any(|c| c.contains("createdAtFrom=")));
    assert!(!calls.iter().any(|c| c.contains(&format!("accountId={CREDIT_ACCOUNT}"))));
    drop(calls);
    let files = inbox.files();
    pull(&db, &Stub::new(), &PullOptions { dry_run: true, ..opts }).unwrap();
    assert_eq!(files, inbox.files());
}

#[test]
fn booking_cutoff_refuses_undated_transactions_before_writing_files_or_cursors() {
    let db = Db::open_memory().unwrap();
    let inbox = Scratch::new("booked-invalid");
    let mut stub = Stub::new();
    stub.files[0]["transactions"][0]["date"] = json!("invalid");
    let error = pull(&db, &stub, &PullOptions { booked_from: Some(date("2026-09-15")), account: Some(BANK_ACCOUNT.into()), ..options(&inbox) }).unwrap_err();
    assert!(error.to_string().contains("booking date"));
    assert!(inbox.files().is_empty());
    assert!(pluggy::connections(&db.conn()).unwrap().is_empty());
}

#[test]
fn enriched_counterparty_fires_rules_and_does_not_duplicate_an_older_description() {
    let b = bank();
    let mut conn = b.db.conn();
    let health = accounts::ensure_account(&conn, b.account.entity_id, AccountType::Expense, &["Health"], "expense", "BRL").unwrap();
    means_core::rules::save_rule(
        &conn,
        &Rule {
            id: 0,
            entity_id: b.account.entity_id,
            name: "EXAMPLE HEALTH".into(),
            position: 0,
            enabled: true,
            conditions: vec![RuleCondition { field: "description".into(), op: "contains".into(), value: "EXAMPLE HEALTH".into() }],
            account_id: Some(health.id),
            template_id: None,
            payee: String::new(),
            hits_count: 0,
            created_at: String::new(),
            tags: String::new(),
        },
    )
    .unwrap();
    let mut file = json!({"channel":"pluggy","account":{"type":"BANK","currencyCode":"BRL"},"transactions":[{"id":"original-id","date":"2026-08-20","amount":-10,"description":"Bankslip","paymentData":{"receiver":"EXAMPLE HEALTH"}}]});
    let first = imports::run_import(&mut conn, imports::ImportRequest::new("pluggy_json", Some(b.account.id), "one.json", &serde_json::to_vec(&file).unwrap())).unwrap();
    let entry = means_core::journal::get_entry(&conn, first.lines[0].journal_entry_id.unwrap()).unwrap();
    assert_eq!(entry.status, EntryStatus::Posted);
    assert!(entry.postings.iter().any(|p| p.account_id == health.id));
    file["transactions"][0]["paymentData"] = Value::Null;
    let second = imports::run_import(&mut conn, imports::ImportRequest::new("pluggy_json", Some(b.account.id), "two.json", &serde_json::to_vec(&file).unwrap())).unwrap();
    assert_eq!(second.import.duplicate_count, 1);
    assert_eq!(second.import.created_count, 0);
}

#[test]
fn discovery_reads_accounts_only_and_saved_cutoffs_apply_to_cli_pulls() {
    let db = Db::open_memory().unwrap();
    let stub = Stub::new();
    let scratch = Scratch::new("discover-cutoff");
    let (status, found) = pluggy::discover(&stub, &credentials(), BASE, ITEM).unwrap();
    assert!(!status.is_empty());
    assert_eq!(found.len(), 2);
    assert!(!stub.calls.lock().unwrap().iter().any(|c| c.contains("PATCH") || c.contains("transactions")));
    {
        let mut c = db.conn();
        means_core::connections::discover(&c, "pluggy", ITEM, &found).unwrap();
        let id = means_core::connections::list(&c).unwrap().into_iter().find(|a| a.provider_account_id == BANK_ACCOUNT).unwrap().id;
        means_core::connections::configure(&mut c, id, None, "2026-09-15").unwrap();
    }
    let report = pull(&db, &stub, &PullOptions { account: Some(BANK_ACCOUNT.into()), ..options(&scratch) }).unwrap();
    assert_eq!(report.accounts[0].booked_from, "2026-09-15");
    assert!(report.accounts[0].excluded_before_booking_date > 0);
    assert!(report.accounts[0].created_at_from.is_empty());
    let report = pull(&db, &stub, &PullOptions { account: Some(BANK_ACCOUNT.into()), booked_from: Some(date("2026-09-01")), ..options(&scratch) }).unwrap();
    assert_eq!(report.accounts[0].booked_from, "2026-09-01");
}
