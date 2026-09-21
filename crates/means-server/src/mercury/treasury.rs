//! Treasury evidence capture. Accounting conversion waits for its approved model.
use super::Client;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

#[derive(Subcommand)]
pub enum Command {
    /// List Treasury account UUIDs and statuses
    Accounts {
        #[arg(long, default_value = "https://api.mercury.com")]
        api: String,
    },
    /// Save all available raw Treasury activity; no ledger entries or inbox imports
    Fetch {
        #[arg(long)]
        account: uuid::Uuid,
        /// New evidence file; refuses to overwrite an existing file
        #[arg(long, required_unless_present = "dry_run")]
        output: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long, default_value = "https://api.mercury.com")]
        api: String,
    },
}

/// Treasury uses integer cursors, not transaction UUID cursors.
async fn transactions(client: &Client, account: uuid::Uuid) -> Result<Vec<Value>> {
    let path = format!("/api/v1/treasury/{account}/transactions");
    let mut cursor: Option<u64> = None;
    let mut cursors = HashSet::new();
    let mut ids = HashSet::new();
    let mut transactions = Vec::new();
    for _ in 0..10_000 {
        let mut url = client.base.join(&path)?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("limit", "1000").append_pair("order", "asc");
            if let Some(value) = cursor {
                query.append_pair("cursor", &value.to_string());
            }
        }
        let response = client.http.get(url).bearer_auth(&client.token).header("accept", "application/json").send().await.map_err(|e| e.without_url()).context("reach Mercury Treasury")?;
        if !response.status().is_success() {
            bail!("Mercury Treasury transactions returned HTTP {}", response.status());
        }
        let bytes = response.bytes().await.map_err(|e| e.without_url()).context("read Mercury Treasury response")?;
        let response: Value = serde_json::from_slice(&bytes).context("invalid Mercury Treasury JSON")?;
        for record in response["transactions"].as_array().context("Mercury Treasury response is missing its transactions array")? {
            let id = record["id"].as_str().context("Treasury transaction has no ID")?;
            let id = uuid::Uuid::parse_str(id).context("Treasury transaction has an invalid UUID")?;
            if !ids.insert(id) {
                bail!("Treasury pagination repeated a transaction ID; no evidence file was saved");
            }
            let owner = record["accountId"].as_str().and_then(|s| uuid::Uuid::parse_str(s).ok());
            if owner != Some(account) {
                bail!("Treasury transaction belongs to a different account");
            }
            // Retain all event kinds, including valuations, failures and new provider types.
            transactions.push(record.clone());
        }
        match response.get("cursor") {
            None | Some(Value::Null) => return Ok(transactions),
            Some(value) => {
                let next = value.as_u64().context("Treasury cursor must be a nonnegative integer")?;
                if !cursors.insert(next) {
                    bail!("Treasury pagination repeated a cursor; no evidence file was saved");
                }
                cursor = Some(next);
            }
        }
    }
    bail!("Treasury pagination exceeded 10000 pages; no evidence file was saved")
}

fn publish(path: &Path, content: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    // The user chooses an existing directory. Keep temporary files beside the destination.
    let temporary = parent.join(format!(".means-treasury-{}.part", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).context("create Treasury evidence temporary file")?;
        file.write_all(content).context("write Treasury evidence")?;
        file.sync_all().context("flush Treasury evidence")?;
        std::fs::hard_link(&temporary, path).context("publish Treasury evidence without overwriting")?;
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    let _ = std::fs::remove_file(temporary);
    result
}

async fn fetch(client: &Client, account: uuid::Uuid, output: Option<&Path>, dry_run: bool) -> Result<usize> {
    if !dry_run && output.is_none() {
        bail!("Treasury fetch requires --output unless --dry-run is set");
    }
    let accounts = client.pages("treasury", None).await?;
    let account =
        accounts.into_iter().find(|a| a["id"].as_str().and_then(|s| uuid::Uuid::parse_str(s).ok()) == Some(account)).context("no matching Treasury account; run `means mercury treasury accounts`")?;
    let id = uuid::Uuid::parse_str(account["id"].as_str().context("Treasury account has no ID")?)?;
    let records = transactions(client, id).await?;
    let count = records.len();
    if !dry_run {
        let content = serde_json::to_vec_pretty(&json!({
            "channel":"mercury_treasury_evidence", "version":1, "fetched_at":means_core::now_ts(),
            "account":account, "transactions":records
        }))?;
        publish(output.context("Treasury output path is missing")?, &content)?;
    }
    Ok(count)
}

pub(super) async fn run(command: Command) -> Result<()> {
    match command {
        Command::Accounts { api } => {
            for account in Client::from_env(&api)?.pages("treasury", None).await? {
                println!("{}\t{}", account["id"].as_str().unwrap_or(""), account["status"].as_str().unwrap_or(""));
            }
        }
        Command::Fetch { account, output, dry_run, api } => {
            let count = fetch(&Client::from_env(&api)?, account, output.as_deref(), dry_run).await?;
            if dry_run {
                println!("{account}\t{count} records\tdry-run");
            } else {
                println!("{account}\t{count} records\t{}", output.context("Treasury output path is missing")?.display());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::Query,
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::get,
        Json, Router,
    };
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };
    const ACCOUNT: &str = "11111111-1111-4111-8111-111111111111";
    const NEXT: &str = "22222222-2222-4222-8222-222222222222";
    const LARGE: u64 = 9_007_199_254_740_993;
    fn account() -> Value {
        json!({"id":ACCOUNT,"status":"active","currentBalance":1234.56,"availableBalance":1200,"netReturns":[{"month":"2026-08-01","status":"charged","dividends":[],"treasuryFee":1,"netAmount":-1}]})
    }
    fn record(n: u32, kind: &str) -> Value {
        json!({"id":format!("00000000-0000-4000-8000-{n:012}"),"accountId":ACCOUNT,"type":kind,"canonicalDay":"2026-08-31","amount":1.23,"balance":1001.23,"description":"Raw evidence","security":"CUSIP text","additionalDetails":"provider detail"})
    }
    async fn serve(app: Router) -> (Client, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = Client::new(&format!("http://{}", listener.local_addr().unwrap()), "test-secret".into()).unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (client, task)
    }
    fn root() -> PathBuf {
        std::env::temp_dir().join(format!("means-treasury-{}", uuid::Uuid::new_v4()))
    }

    #[tokio::test]
    async fn evidence_fetch_uses_both_cursor_types_and_keeps_all_activity() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let calls = seen.clone();
        let app = Router::new()
            .route(
                "/api/v1/treasury",
                get(|headers: HeaderMap, Query(q): Query<HashMap<String, String>>| async move {
                    assert_eq!(headers["authorization"], "Bearer test-secret");
                    assert_eq!(q["limit"], "1000");
                    assert_eq!(q["order"], "asc");
                    if let Some(cursor) = q.get("start_after") {
                        assert_eq!(cursor, NEXT);
                        Json(json!({"accounts":[account()],"page":{}}))
                    } else {
                        Json(json!({"accounts":[],"page":{"nextPage":NEXT}}))
                    }
                }),
            )
            .route(
                &format!("/api/v1/treasury/{ACCOUNT}/transactions"),
                get(move |headers: HeaderMap, Query(q): Query<HashMap<String, String>>| {
                    let calls = calls.clone();
                    async move {
                        assert_eq!(headers["authorization"], "Bearer test-secret");
                        assert_eq!(q["order"], "asc");
                        assert_eq!(q["limit"], "1000");
                        assert!(!q.contains_key("start_after") && !q.contains_key("accountId") && !q.contains_key("start"));
                        calls.lock().unwrap().push(q.clone());
                        match q.get("cursor").map(String::as_str) {
                            None => Json(json!({"transactions":[],"cursor":0})),
                            Some("0") => Json(json!({"transactions":[record(1,"valuationChangePosted"),record(2,"depositFailed")],"cursor":LARGE})),
                            Some(s) => {
                                assert_eq!(s, LARGE.to_string());
                                Json(json!({"transactions":[record(3,"futureEventKind")],"cursor":null}))
                            }
                        }
                    }
                }),
            );
        let (client, task) = serve(app).await;
        let root = root();
        std::fs::create_dir(&root).unwrap();
        let file = root.join("evidence.json");
        assert_eq!(fetch(&client, ACCOUNT.parse().unwrap(), Some(&file), true).await.unwrap(), 3);
        assert!(!file.exists());
        assert_eq!(fetch(&client, ACCOUNT.parse().unwrap(), Some(&file), false).await.unwrap(), 3);
        let content = std::fs::read(&file).unwrap();
        let raw: Value = serde_json::from_slice(&content).unwrap();
        assert_eq!(raw["channel"], "mercury_treasury_evidence");
        assert_eq!(raw["account"], account());
        assert_eq!(raw["transactions"], json!([record(1, "valuationChangePosted"), record(2, "depositFailed"), record(3, "futureEventKind")]));
        assert!(!String::from_utf8(content.clone()).unwrap().contains("test-secret"));
        assert_eq!(seen.lock().unwrap().len(), 6);
        assert!(fetch(&client, ACCOUNT.parse().unwrap(), Some(&file), false).await.is_err());
        assert_eq!(std::fs::read(&file).unwrap(), content);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1, "temporary files are removed");
        // Evidence is not an importable bank envelope and cannot create postings.
        let db = means_core::Db::open_memory().unwrap();
        let result = means_core::imports::inbox::scan(&mut db.conn(), &root).unwrap();
        assert_eq!(result[0].action, "ignored");
        assert_eq!(db.conn().query_row("SELECT COUNT(*) FROM journal_entries", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        }
        task.abort();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn malformed_pages_and_account_mismatches_fail() {
        let mut wrong = record(1, "depositComplete");
        wrong["accountId"] = json!(NEXT);
        for response in [
            json!({}),
            json!({"transactions":[],"cursor":-1}),
            json!({"transactions":[],"cursor":1.5}),
            json!({"transactions":[],"cursor":"2"}),
            json!({"transactions":[],"cursor":0}),
            json!({"transactions":[record(1,"dividendPosted"),record(1,"dividendPosted")]}),
            json!({"transactions":[wrong]}),
            json!({"transactions":[{}]}),
        ] {
            let app = Router::new().route(
                &format!("/api/v1/treasury/{ACCOUNT}/transactions"),
                get(move || {
                    let response = response.clone();
                    async move { Json(response) }
                }),
            );
            let (client, task) = serve(app).await;
            assert!(transactions(&client, ACCOUNT.parse().unwrap()).await.is_err());
            task.abort();
        }
    }

    #[tokio::test]
    async fn failed_later_page_does_not_publish_or_expose_response() {
        let app = Router::new().route("/api/v1/treasury", get(|| async { Json(json!({"accounts":[account()],"page":{}})) })).route(
            &format!("/api/v1/treasury/{ACCOUNT}/transactions"),
            get(|Query(q): Query<HashMap<String, String>>| async move {
                if q.contains_key("cursor") {
                    (StatusCode::TOO_MANY_REQUESTS, "test-secret private provider body").into_response()
                } else {
                    Json(json!({"transactions":[record(1,"depositComplete")],"cursor":1})).into_response()
                }
            }),
        );
        let (client, task) = serve(app).await;
        let root = root();
        std::fs::create_dir(&root).unwrap();
        let file = root.join("evidence.json");
        let error = fetch(&client, ACCOUNT.parse().unwrap(), Some(&file), false).await.unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("429"));
        assert!(!message.contains("test-secret") && !message.contains("private provider"));
        assert!(!file.exists());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        task.abort();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fetch_requires_a_valid_account_and_explicit_destination() {
        use clap::Parser;
        assert!(crate::Cli::try_parse_from(["means", "mercury", "treasury", "accounts"]).is_ok());
        assert!(crate::Cli::try_parse_from(["means", "mercury", "treasury", "fetch", "--account", ACCOUNT]).is_err());
        assert!(crate::Cli::try_parse_from(["means", "mercury", "treasury", "fetch", "--account", ACCOUNT, "--dry-run"]).is_ok());
        assert!(crate::Cli::try_parse_from(["means", "mercury", "treasury", "fetch", "--account", ACCOUNT, "--output", "evidence.json"]).is_ok());
        assert!(crate::Cli::try_parse_from(["means", "mercury", "treasury", "fetch", "--account", "../../wrong", "--dry-run"]).is_err());
    }
}
