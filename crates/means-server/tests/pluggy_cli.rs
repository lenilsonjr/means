//! `means pluggy pull` end to end: the real binary against a stub of the Pluggy API, serving the
//! recorded fixtures. Nothing here reaches the network.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

const ITEM: &str = "a1b2c3d4-0000-4000-8000-000000000001";
const BANK_ACCOUNT: &str = "b8e1c1a2-1111-4000-8000-000000000011";
const CREDIT_ACCOUNT: &str = "b8e1c1a2-1111-4000-8000-000000000012";
const API_KEY: &str = "api-key-from-the-stub";
const PAGE_SIZE: usize = 2;

fn fixture(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../means-core/tests/fixtures").join(name);
    serde_json::from_slice(&std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))).unwrap()
}

#[derive(Clone)]
struct Stub {
    files: Vec<Value>,
    base: Arc<Mutex<String>>,
    calls: Arc<Mutex<Vec<String>>>,
    /// The item answers UPDATING once, then UPDATED: the pull has to wait for it.
    reads: Arc<Mutex<usize>>,
}

async fn auth(State(stub): State<Stub>) -> Json<Value> {
    stub.calls.lock().unwrap().push("POST /auth".into());
    Json(json!({"apiKey": API_KEY}))
}

async fn patch_item(State(stub): State<Stub>, Path(id): Path<String>) -> Json<Value> {
    stub.calls.lock().unwrap().push(format!("PATCH /items/{id}"));
    Json(json!({"id": id, "status": "UPDATING"}))
}

async fn get_item(State(stub): State<Stub>, Path(id): Path<String>) -> Json<Value> {
    stub.calls.lock().unwrap().push(format!("GET /items/{id}"));
    let mut reads = stub.reads.lock().unwrap();
    *reads += 1;
    Json(json!({"id": id, "status": if *reads > 1 { "UPDATED" } else { "UPDATING" }}))
}

async fn accounts(State(stub): State<Stub>, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
    stub.calls.lock().unwrap().push(format!("GET /accounts?itemId={}", q.get("itemId").cloned().unwrap_or_default()));
    Json(json!({"results": stub.files.iter().map(|f| f["account"].clone()).collect::<Vec<Value>>(), "total": stub.files.len()}))
}

async fn transactions(State(stub): State<Stub>, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
    let account = q.get("accountId").cloned().unwrap_or_default();
    let after = q.get("after").cloned().unwrap_or_default();
    let created_at_from = q.get("createdAtFrom").cloned().unwrap_or_default();
    stub.calls.lock().unwrap().push(format!("GET /v2/transactions accountId={account} after={after} createdAtFrom={created_at_from}"));
    let all = stub.files.iter().find(|f| f["account"]["id"] == Value::String(account.clone())).map(|f| f["transactions"].as_array().cloned().unwrap_or_default()).unwrap_or_default();
    let from: usize = after.parse().unwrap_or(0);
    let to = (from + PAGE_SIZE).min(all.len());
    // Pluggy writes `next` as a whole URL; the pull follows it as it is.
    let next = if to < all.len() { json!(format!("{}/v2/transactions?accountId={account}&after={to}", stub.base.lock().unwrap())) } else { Value::Null };
    Json(json!({"results": all[from..to], "next": next, "total": all.len()}))
}

struct Server {
    url: String,
    calls: Arc<Mutex<Vec<String>>>,
}

async fn start() -> Server {
    let stub = Stub {
        files: vec![fixture("pluggy_bank.json"), fixture("pluggy_credit.json")],
        base: Arc::new(Mutex::new(String::new())),
        calls: Arc::new(Mutex::new(Vec::new())),
        reads: Arc::new(Mutex::new(0)),
    };
    let app = Router::new()
        .route("/auth", post(auth))
        .route("/items/{id}", get(get_item).patch(patch_item))
        .route("/accounts", get(accounts))
        .route("/v2/transactions", get(transactions))
        .with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    *stub.base.lock().unwrap() = url.clone();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Server { url, calls: stub.calls }
}

struct Ledger {
    db: PathBuf,
    inbox: PathBuf,
}

impl Ledger {
    fn new(name: &str) -> Ledger {
        let dir = std::env::temp_dir().join(format!("means-pluggy-cli-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Ledger { db: dir.join("ledger.db"), inbox: dir.join("inbox") }
    }

    fn files(&self) -> Vec<String> {
        let mut names: Vec<String> =
            std::fs::read_dir(&self.inbox).map(|d| d.flatten().filter(|e| e.path().is_file()).map(|e| e.file_name().to_string_lossy().to_string()).collect()).unwrap_or_default();
        names.sort();
        names
    }
}

impl Drop for Ledger {
    fn drop(&mut self) {
        if let Some(dir) = self.db.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

fn pull(ledger: &Ledger, api: &str, extra: &[&str]) -> (bool, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_means"));
    command
        .args(["--db", ledger.db.to_str().unwrap(), "pluggy", "pull", "--api", api, "--inbox", ledger.inbox.to_str().unwrap(), "--item", ITEM])
        .args(extra)
        .env("PLUGGY_CLIENT_ID", "client-id-of-the-application")
        .env("PLUGGY_CLIENT_SECRET", "pluggy-secret-9f2c1e");
    let out = command.output().expect("run means");
    (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

// The stub server and the binary run at the same time: the test blocks on the child process, so
// the runtime needs a thread left for the server to answer on.
#[tokio::test(flavor = "multi_thread")]
async fn pluggy_pull_writes_one_file_per_account_into_the_inbox() {
    let server = start().await;
    let ledger = Ledger::new("pull");

    let (ok, output) = pull(&ledger, &server.url, &["--dry-run"]);
    assert!(ok, "{output}");
    assert!(output.contains("nothing written (dry run)"), "{output}");
    assert!(ledger.files().is_empty(), "a dry run writes nothing");

    let (ok, output) = pull(&ledger, &server.url, &[]);
    assert!(ok, "{output}");
    let files = ledger.files();
    assert_eq!(files.len(), 2, "{files:?}");
    let bank_file = files.iter().find(|f| f.ends_with(&format!("{BANK_ACCOUNT}.json"))).expect("a file for the bank account");
    assert!(files.iter().any(|f| f.ends_with(&format!("{CREDIT_ACCOUNT}.json"))), "a file for the card");
    assert!(output.contains(bank_file), "{output}");

    let written: Value = serde_json::from_slice(&std::fs::read(ledger.inbox.join(bank_file)).unwrap()).unwrap();
    let recorded = fixture("pluggy_bank.json");
    assert_eq!(written["channel"], "pluggy");
    assert_eq!(written["itemId"], ITEM);
    assert_eq!(written["account"], recorded["account"]);
    assert_eq!(written["transactions"], recorded["transactions"], "the file holds what Pluggy answered, verbatim");

    let calls = server.calls.lock().unwrap().clone();
    assert!(calls.iter().any(|c| c == &format!("PATCH /items/{ITEM}")), "{calls:?}");
    assert!(calls.iter().filter(|c| c == &&format!("GET /items/{ITEM}")).count() >= 2, "the item state is read after the update is asked for: {calls:?}");
    assert!(calls.iter().any(|c| c.contains(&format!("accountId={BANK_ACCOUNT} after=2"))), "the second page followed the next cursor: {calls:?}");
    assert!(calls.iter().any(|c| c.contains(&format!("accountId={CREDIT_ACCOUNT} after="))), "{calls:?}");

    // The second pull sends the cursor the first one stored.
    let (ok, output) = pull(&ledger, &server.url, &[]);
    assert!(ok, "{output}");
    let calls = server.calls.lock().unwrap().clone();
    assert!(calls.iter().any(|c| c.contains(&format!("accountId={BANK_ACCOUNT} after= createdAtFrom=20"))), "{calls:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_credential_stops_the_pull() {
    let server = start().await;
    let ledger = Ledger::new("nocreds");
    let out = Command::new(env!("CARGO_BIN_EXE_means"))
        .args(["--db", ledger.db.to_str().unwrap(), "pluggy", "pull", "--api", &server.url, "--inbox", ledger.inbox.to_str().unwrap(), "--item", ITEM])
        .env_remove("PLUGGY_CLIENT_ID")
        .env_remove("PLUGGY_CLIENT_SECRET")
        .output()
        .expect("run means");
    let message = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(!out.status.success());
    assert!(message.contains("PLUGGY_CLIENT_ID"), "{message}");
    assert!(ledger.files().is_empty());
}

async fn serve_router(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, task)
}

#[tokio::test(flavor = "multi_thread")]
async fn redirects_and_pagination_never_reach_another_origin() {
    use axum::response::IntoResponse;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let received = Arc::new(AtomicUsize::new(0));
    let hits = received.clone();
    let (destination, sink) = serve_router(Router::new().fallback(move || {
        let hits = hits.clone();
        async move {
            hits.fetch_add(1, Ordering::SeqCst);
            Json(json!({"apiKey": API_KEY, "results": []}))
        }
    }))
    .await;
    for stage in ["auth", "item", "pagination"] {
        for code in [301, 302, 303, 307, 308] {
            let location = destination.clone();
            let app = Router::new().fallback(move |uri: axum::http::Uri| {
                let location = location.clone();
                async move {
                    let path = uri.path();
                    if (stage == "auth" && path == "/auth") || (stage == "item" && path.starts_with("/items/")) {
                        return (axum::http::StatusCode::from_u16(code).unwrap(), [("location", location)]).into_response();
                    }
                    let body = match path {
                        "/auth" => json!({"apiKey": API_KEY}),
                        "/accounts" => json!({"results": [{"id": BANK_ACCOUNT, "name": "Bank", "type": "BANK", "currencyCode": "BRL"}]}),
                        "/v2/transactions" => json!({"results": [], "next": location}),
                        _ => json!({"status": "UPDATED"}),
                    };
                    Json(body).into_response()
                }
            });
            let (base, source) = serve_router(app).await;
            let ledger = Ledger::new(&format!("redirect-{stage}-{code}"));
            let (success, output) = pull(&ledger, &base, &["--dry-run"]);
            assert!(!success, "{stage} {code}: {output}");
            assert!(!output.contains(API_KEY));
            assert!(!output.contains("pluggy-secret-9f2c1e"));
            assert_eq!(received.load(Ordering::SeqCst), 0, "no request may reach the other origin");
            assert!(ledger.files().is_empty());
            source.abort();
        }
    }
    sink.abort();
}
