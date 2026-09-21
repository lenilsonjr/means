//! Read-only Mercury depository and IO credit pulls. Credentials stay at the HTTP boundary.
mod treasury;

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use means_core::{imports::mercury as core, Db};
use serde_json::Value;
use std::{collections::HashSet, path::PathBuf, time::Duration};

#[derive(Subcommand)]
pub enum Command {
    /// Inspect Treasury accounts and save raw evidence; does not import ledger entries
    Treasury {
        #[command(subcommand)]
        command: treasury::Command,
    },
    /// List Mercury accounts (checking/savings by default)
    Accounts {
        /// List IO credit accounts instead of checking/savings
        #[arg(long)]
        credit: bool,
        #[arg(long, default_value = "https://api.mercury.com")]
        api: String,
    },
    /// Fetch account history into the inbox (all available history unless a cutoff is set)
    Pull {
        /// Pull IO credit accounts instead of checking/savings
        #[arg(long)]
        credit: bool,
        /// Restrict the pull to one Mercury account UUID
        #[arg(long)]
        account: Option<String>,
        /// Include bookings on or after this UTC date; overrides the saved account cutoff
        #[arg(long, value_parser = means_core::parse_date)]
        booked_from: Option<chrono::NaiveDate>,
        /// Fetch and validate without publishing files or changing account mappings
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        inbox: Option<PathBuf>,
        #[arg(long, default_value = "https://api.mercury.com")]
        api: String,
    },
}

struct Client {
    http: reqwest::Client,
    base: reqwest::Url,
    token: String,
}

impl Client {
    fn new(api: &str, token: String) -> Result<Self> {
        let base = reqwest::Url::parse(api).context("invalid Mercury API URL")?;
        let loopback = base.host_str().is_some_and(|h| h == "localhost" || h.trim_matches(['[', ']']).parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()));
        if !(base.scheme() == "https" || base.scheme() == "http" && loopback)
            || !base.username().is_empty()
            || base.password().is_some()
            || base.path() != "/"
            || base.query().is_some()
            || base.fragment().is_some()
        {
            bail!("Mercury API must be an HTTPS origin without credentials, path, query or fragment; HTTP is allowed only on loopback");
        }
        if token.trim().is_empty() {
            bail!("MERCURY_TOKEN must not be empty");
        }
        let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(90)).user_agent(format!("means/{}", crate::VERSION)).build()?;
        Ok(Self { http, base, token })
    }

    fn from_env(api: &str) -> Result<Self> {
        Self::new(api, std::env::var("MERCURY_TOKEN").context("set MERCURY_TOKEN to a read-only Mercury API token")?)
    }

    async fn pages(&self, resource: &str, account: Option<&str>) -> Result<Vec<Value>> {
        self.pages_from(resource, account, None).await
    }

    async fn pages_from(&self, resource: &str, account: Option<&str>, booked_from: Option<chrono::NaiveDate>) -> Result<Vec<Value>> {
        // Resource names come only from this module; provider cursors are query data.
        if !matches!(resource, "accounts" | "transactions" | "treasury") {
            bail!("unsupported Mercury resource");
        }
        let path = format!("/api/v1/{resource}");
        let mut cursor: Option<String> = None;
        let mut cursors = HashSet::new();
        let mut ids = HashSet::new();
        let mut records = Vec::new();
        for _ in 0..10_000 {
            let mut url = self.base.join(&path)?;
            {
                let mut query = url.query_pairs_mut();
                query.append_pair("limit", "1000").append_pair("order", "asc");
                if let Some(id) = account {
                    query.append_pair("accountId", id);
                }
                if let Some(from) = booked_from {
                    query.append_pair("postedStart", &format!("{from}T00:00:00Z"));
                }
                if let Some(key) = &cursor {
                    query.append_pair("start_after", key);
                }
            }
            let response = self.http.get(url).bearer_auth(&self.token).header("accept", "application/json").send().await.map_err(|e| e.without_url()).context("reach Mercury")?;
            if !response.status().is_success() {
                // Provider bodies and URLs can echo sensitive request data.
                bail!("Mercury GET {path} returned HTTP {}", response.status());
            }
            let bytes = response.bytes().await.map_err(|e| e.without_url()).context("read Mercury response")?;
            let response: Value = serde_json::from_slice(&bytes).context("invalid Mercury JSON response")?;
            let rows = response[if resource == "treasury" { "accounts" } else { resource }].as_array().context("Mercury response is missing its records array")?;
            for row in rows {
                let id = row["id"].as_str().context("Mercury record is missing its ID")?;
                let uuid = uuid::Uuid::parse_str(id).context("Mercury returned an invalid record UUID")?;
                if !ids.insert(uuid) {
                    bail!("Mercury pagination repeated a record ID; no incomplete account will be published");
                }
                records.push(row.clone());
            }
            let page = response["page"].as_object().context("Mercury response is missing its page object")?;
            match page.get("nextPage") {
                None | Some(Value::Null) => return Ok(records),
                Some(Value::String(key)) => {
                    let uuid = uuid::Uuid::parse_str(key).context("Mercury returned an invalid page cursor")?;
                    if !cursors.insert(uuid) {
                        bail!("Mercury pagination repeated a cursor; no incomplete account will be published");
                    }
                    cursor = Some(key.clone());
                }
                _ => bail!("Mercury returned an invalid page cursor"),
            }
        }
        bail!("Mercury pagination exceeded 10000 pages; no incomplete account will be published")
    }

    async fn credit_accounts(&self) -> Result<Vec<Value>> {
        let path = "/api/v1/credit";
        let response =
            self.http.get(self.base.join(path)?).bearer_auth(&self.token).header("accept", "application/json").send().await.map_err(|e| e.without_url()).context("reach Mercury credit accounts")?;
        if !response.status().is_success() {
            bail!("Mercury GET {path} returned HTTP {}", response.status());
        }
        let bytes = response.bytes().await.map_err(|e| e.without_url()).context("read Mercury credit accounts")?;
        let response: Value = serde_json::from_slice(&bytes).context("invalid Mercury credit JSON response")?;
        let accounts = response["accounts"].as_array().context("Mercury credit response is missing its accounts array")?;
        // /credit is a complete list. Refuse an unexpected cursor rather than truncate it.
        if response.get("page").is_some_and(|p| !p["nextPage"].is_null()) {
            bail!("Mercury credit discovery returned unsupported pagination");
        }
        let mut ids = HashSet::new();
        for account in accounts {
            let id = account["id"].as_str().context("Mercury credit account is missing its ID")?;
            if !ids.insert(uuid::Uuid::parse_str(id).context("Mercury returned an invalid credit account UUID")?) {
                bail!("Mercury credit discovery repeated an account ID");
            }
        }
        Ok(accounts.clone())
    }

    async fn accounts(&self) -> Result<Vec<Value>> {
        let all = self.pages("accounts", None).await?;
        let mut accounts = Vec::new();
        let mut skipped = 0;
        for account in all {
            if account["type"] == "mercury" && matches!(account["kind"].as_str(), Some("checking" | "savings")) {
                accounts.push(account);
            } else {
                skipped += 1;
            }
        }
        if skipped > 0 {
            eprintln!("Skipped {skipped} accounts outside Mercury checking/savings; use --credit for IO; Treasury requires a separate channel.");
        }
        Ok(accounts)
    }
}

fn filter_bookings(records: Vec<Value>, account: &str, from: Option<chrono::NaiveDate>) -> Result<Vec<Value>> {
    let Some(from) = from else { return Ok(records) };
    let mut kept = Vec::new();
    for record in records {
        if record["accountId"].as_str() != Some(account) {
            bail!("Mercury transaction accountId does not match selected account; account not published");
        }
        if record["status"] == "pending" {
            continue;
        }
        let posted = record["postedAt"].as_str().context("missing Mercury postedAt; cannot apply booking cutoff; account not published")?;
        let date = chrono::DateTime::parse_from_rfc3339(posted).context("invalid Mercury postedAt; cannot apply booking cutoff; account not published")?.with_timezone(&chrono::Utc).date_naive();
        if date >= from {
            kept.push(record);
        }
    }
    Ok(kept)
}

async fn pull_report(
    credit: bool,
    db_path: PathBuf,
    client: Client,
    only: Option<String>,
    dry_run: bool,
    inbox: Option<PathBuf>,
    booked_from: Option<chrono::NaiveDate>,
) -> Result<Vec<core::PullFile>> {
    let selected = only.as_deref().map(uuid::Uuid::parse_str).transpose().context("--account must be a Mercury account UUID")?;
    let mut accounts = if credit { client.credit_accounts().await? } else { client.accounts().await? };
    if let Some(id) = selected {
        accounts.retain(|a| a["id"].as_str().and_then(|v| uuid::Uuid::parse_str(v).ok()) == Some(id));
    }
    if accounts.is_empty() {
        bail!(
            "no matching Mercury {} accounts; run `means mercury accounts{}` to see supported accounts",
            if credit { "IO credit" } else { "checking/savings" },
            if credit { " --credit" } else { "" }
        );
    }
    let db = Db::open(&db_path)?;
    let directory = inbox.unwrap_or_else(|| db_path.parent().unwrap_or_else(|| std::path::Path::new(".")).join("inbox"));
    let saved = means_core::connections::list(&db.conn())?;
    let mut files = Vec::new();
    for account in accounts {
        let id = account["id"].as_str().context("Mercury account has no ID")?;
        // /transactions defaults to first available history. The account-specific
        // /account/{id}/transactions endpoint instead defaults to only 30 days.
        let saved_from = saved.iter().find(|a| a.channel == core::CHANNEL && a.provider_account_id == id).map(|a| a.booked_from.as_str()).unwrap_or_default();
        let from = booked_from.or(means_core::parse_opt_date(saved_from)?);
        let transactions = client.pages_from("transactions", Some(id), from).await?;
        let fetched = transactions.len();
        let transactions = filter_bookings(transactions, id, from)?;
        if let Some(from) = from {
            println!("{id}: booked from {from} (inclusive UTC); {} returned records excluded by booking filter", fetched - transactions.len());
        }
        let result = if credit {
            core::publish_credit_account(&mut db.conn(), &directory, &account, transactions, dry_run)?
        } else {
            core::publish_account(&mut db.conn(), &directory, &account, transactions, dry_run)?
        };
        files.push(result);
    }
    Ok(files)
}

async fn pull(credit: bool, db_path: PathBuf, client: Client, only: Option<String>, dry_run: bool, inbox: Option<PathBuf>, booked_from: Option<chrono::NaiveDate>) -> Result<()> {
    for result in pull_report(credit, db_path, client, only, dry_run, inbox, booked_from).await? {
        match result.path {
            Some(path) => println!("{}\t{} records\t{}", result.account_ref, result.records, path.display()),
            None => println!("{}\t{} records\tdry-run", result.account_ref, result.records),
        }
    }
    Ok(())
}

pub async fn discover_connected(api: &str, credit: bool) -> Result<Vec<means_core::connections::ConnectionAccount>> {
    let client = Client::from_env(api)?;
    let accounts = if credit { client.credit_accounts().await? } else { client.accounts().await? };
    Ok(accounts
        .into_iter()
        .map(|a| means_core::connections::ConnectionAccount {
            channel: core::CHANNEL.into(),
            provider_account_id: a["id"].as_str().unwrap_or_default().into(),
            provider_type: if credit { "credit" } else { a["kind"].as_str().unwrap_or_default() }.into(),
            name: a["name"].as_str().unwrap_or_default().into(),
            currency: "USD".into(),
            ..Default::default()
        })
        .collect())
}
pub async fn pull_connected(api: &str, credit: bool, db_path: PathBuf, account: String, inbox: PathBuf) -> Result<usize> {
    Ok(pull_report(credit, db_path, Client::from_env(api)?, Some(account), false, Some(inbox), None).await?.len())
}

pub async fn run(db_path: PathBuf, command: Command) -> Result<()> {
    match command {
        Command::Treasury { command } => treasury::run(command).await,
        Command::Accounts { api, credit } => {
            let client = Client::from_env(&api)?;
            for account in if credit { client.credit_accounts().await? } else { client.accounts().await? } {
                println!(
                    "{}\t{}\t{}\t{}",
                    account["id"].as_str().unwrap_or(""),
                    if credit { "credit" } else { account["kind"].as_str().unwrap_or("") },
                    account["status"].as_str().unwrap_or(""),
                    account["name"].as_str().unwrap_or("")
                );
            }
            Ok(())
        }
        Command::Pull { account, booked_from, dry_run, inbox, api, credit } => pull(credit, db_path, Client::from_env(&api)?, account, dry_run, inbox, booked_from).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::{Query, State},
        http::{HeaderMap, StatusCode},
        response::{IntoResponse, Response},
        routing::get,
        Json, Router,
    };
    use serde_json::json;
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    const CHECKING: &str = "11111111-1111-4111-8111-111111111111";
    const SAVINGS: &str = "22222222-2222-4222-8222-222222222222";
    const CREDIT: &str = "33333333-3333-4333-8333-333333333333";
    const CURSOR: &str = "44444444-4444-4444-8444-444444444444";
    const TX: &str = "55555555-5555-4555-8555-555555555555";
    type Seen = Arc<Mutex<Vec<HashMap<String, String>>>>;

    async fn accounts(headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
        assert_eq!(headers["authorization"], "Bearer test-secret");
        assert_eq!(q["order"], "asc");
        assert_eq!(q["limit"], "1000");
        if q.contains_key("start_after") {
            assert_eq!(q["start_after"], CURSOR);
            Json(json!({"accounts":[{"id":SAVINGS,"type":"mercury","kind":"savings","status":"archived","name":"Old savings"},{"id":CREDIT,"type":"mercury","kind":"credit"}],"page":{}}))
        } else {
            Json(json!({"accounts":[{"id":CHECKING,"type":"mercury","kind":"checking","status":"active","name":"Operating"}],"page":{"nextPage":CURSOR}}))
        }
    }
    async fn transactions(State((seen, fail)): State<(Seen, bool)>, Query(q): Query<HashMap<String, String>>, headers: HeaderMap) -> Response {
        assert_eq!(headers["authorization"], "Bearer test-secret");
        assert!(!q.contains_key("start") && !q.contains_key("postedStart") && !q.contains_key("status"));
        seen.lock().unwrap().push(q.clone());
        assert_ne!(q["accountId"], CREDIT);
        if q["accountId"] == SAVINGS {
            return Json(json!({"transactions":[],"page":{"nextPage":null}})).into_response();
        }
        if !q.contains_key("start_after") {
            return Json(json!({"transactions":[],"page":{"nextPage":CURSOR}})).into_response();
        }
        assert_eq!(q["start_after"], CURSOR);
        if fail {
            return (StatusCode::TOO_MANY_REQUESTS, "test-secret sensitive provider body").into_response();
        }
        Json(json!({"transactions":[{"id":TX,"accountId":CHECKING,"amount":-3.21,"status":"sent","postedAt":"2020-01-02T00:00:00Z","counterpartyName":"Shop"}],"page":{}})).into_response()
    }
    async fn server(fail: bool) -> (String, Seen, tokio::task::JoinHandle<()>) {
        let seen = Seen::default();
        let app = Router::new().route("/api/v1/accounts", get(accounts)).route("/api/v1/transactions", get(transactions)).with_state((seen.clone(), fail));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (url, seen, task)
    }

    #[test]
    fn rejects_unsafe_origins_and_empty_tokens() {
        for api in ["http://example.com", "https://user:password@example.com", "https://api.mercury.com/api", "https://api.mercury.com?key=x", "file:///tmp/x"] {
            assert!(Client::new(api, "test".into()).is_err());
        }
        assert!(Client::new("https://api.mercury.com", "".into()).is_err());
        for api in ["https://api.mercury.com", "http://127.0.0.1:1234", "http://[::1]:1234"] {
            assert!(Client::new(api, "test".into()).is_ok());
        }
    }

    #[tokio::test]
    async fn full_history_pull_paginates_both_resources_and_dry_run_does_not_publish() {
        let (url, seen, task) = server(false).await;
        let root = std::env::temp_dir().join(format!("means-mercury-http-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db_path = root.join("ledger.db");
        let inbox = root.join("inbox");
        pull(false, db_path.clone(), Client::new(&url, "test-secret".into()).unwrap(), None, true, None, None).await.unwrap();
        assert!(!inbox.exists());
        let db = Db::open(&db_path).unwrap();
        let count: i64 = db.conn().query_row("SELECT COUNT(*) FROM channel_connections", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
        pull(false, db_path.clone(), Client::new(&url, "test-secret".into()).unwrap(), None, false, None, None).await.unwrap();
        let files: Vec<_> = std::fs::read_dir(&inbox).unwrap().map(|e| e.unwrap().path()).collect();
        assert_eq!(files.len(), 2, "checking and archived savings, never credit");
        let parsed: Vec<_> = files.iter().map(|p| core::parse(&std::fs::read(p).unwrap()).unwrap()).collect();
        assert_eq!(parsed.iter().map(|p| p.lines.len()).sum::<usize>(), 1);
        assert_eq!(parsed.iter().find_map(|p| p.period_from).unwrap().to_string(), "2020-01-02");
        assert_eq!(seen.lock().unwrap().len(), 6, "empty first pages must not terminate pagination");
        let error = pull(false, db_path, Client::new(&url, "test-secret".into()).unwrap(), Some(CREDIT.into()), false, None, None).await.unwrap_err();
        assert!(error.to_string().contains("no matching"));
        task.abort();
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn failed_second_page_publishes_nothing_and_does_not_expose_provider_body() {
        let (url, _, task) = server(true).await;
        let root = std::env::temp_dir().join(format!("means-mercury-failure-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db_path = root.join("ledger.db");
        let error = pull(false, db_path.clone(), Client::new(&url, "test-secret".into()).unwrap(), Some(CHECKING.into()), false, None, None).await.unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("429"));
        assert!(!message.contains("test-secret") && !message.contains("sensitive"));
        assert!(!root.join("inbox").exists());
        let db = Db::open(db_path).unwrap();
        assert_eq!(db.conn().query_row("SELECT COUNT(*) FROM channel_connections", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        drop(db);
        task.abort();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn rejects_malformed_pagination_cycles_and_redirects() {
        for response in [
            json!({"transactions":[]}),
            json!({"transactions":[],"page":{"nextPage":"https://evil.example/steal"}}),
            json!({"transactions":[],"page":{"nextPage":7}}),
            json!({"transactions":[],"page":{"nextPage":CURSOR}}),
            json!({"transactions":[{"id":TX},{"id":TX}],"page":{}}),
        ] {
            let app = Router::new().route(
                "/api/v1/transactions",
                get(move || {
                    let response = response.clone();
                    async move { Json(response) }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let api = format!("http://{}", listener.local_addr().unwrap());
            let task = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            assert!(Client::new(&api, "test-secret".into()).unwrap().pages("transactions", Some(CHECKING)).await.is_err());
            task.abort();
        }
        let app = Router::new().route("/api/v1/accounts", get(|| async { (StatusCode::FOUND, [("location", "https://evil.example/steal")]) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let error = Client::new(&api, "test-secret".into()).unwrap().accounts().await.unwrap_err();
        assert!(error.to_string().contains("302"));
        task.abort();
    }

    #[tokio::test]
    async fn credit_discovery_and_full_history_pull_keep_raw_evidence() {
        for fail in [false, true] {
            let seen = Seen::default();
            let credit_account = json!({"id":CREDIT,"status":"active","createdAt":"2020-01-01T00:00:00Z","currentBalance":-3.21,"availableBalance":-3.21});
            let response = credit_account.clone();
            let log = seen.clone();
            let app = Router::new()
                .route(
                    "/api/v1/credit",
                    get(move |headers: HeaderMap, Query(q): Query<HashMap<String, String>>| {
                        let account = response.clone();
                        async move {
                            assert_eq!(headers["authorization"], "Bearer test-secret");
                            assert!(q.is_empty(), "credit discovery is not paginated");
                            Json(json!({"accounts":[account]}))
                        }
                    }),
                )
                .route(
                    "/api/v1/transactions",
                    get(move |headers: HeaderMap, Query(q): Query<HashMap<String, String>>| {
                        let log = log.clone();
                        async move {
                            assert_eq!(headers["authorization"], "Bearer test-secret");
                            assert_eq!(q["accountId"], CREDIT);
                            assert_eq!(q["order"], "asc");
                            assert_eq!(q["limit"], "1000");
                            assert!(!q.contains_key("start") && !q.contains_key("status") && !q.contains_key("postedStart"));
                            log.lock().unwrap().push(q.clone());
                            if !q.contains_key("start_after") {
                                return Json(json!({"transactions":[],"page":{"nextPage":CURSOR}})).into_response();
                            }
                            assert_eq!(q["start_after"], CURSOR);
                            if fail {
                                return (StatusCode::TOO_MANY_REQUESTS, "test-secret private response").into_response();
                            }
                            Json(json!({"transactions":[{"id":TX,"accountId":CREDIT,"amount":-3.21,"kind":"creditCardTransaction","status":"sent","postedAt":"2020-01-02T00:00:00Z"}],"page":{}}))
                                .into_response()
                        }
                    }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let task = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let root = std::env::temp_dir().join(format!("means-credit-http-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let db_path = root.join("ledger.db");
            if !fail {
                pull(true, db_path.clone(), Client::new(&url, "test-secret".into()).unwrap(), Some(CREDIT.into()), true, None, None).await.unwrap();
                assert!(!root.join("inbox").exists());
            }
            let result = pull(true, db_path.clone(), Client::new(&url, "test-secret".into()).unwrap(), None, false, None, None).await;
            if fail {
                let message = format!("{:#}", result.unwrap_err());
                assert!(message.contains("429"));
                assert!(!message.contains("test-secret") && !message.contains("private response"));
                assert!(!root.join("inbox").exists());
            } else {
                result.unwrap();
                let files: Vec<_> = std::fs::read_dir(root.join("inbox")).unwrap().collect();
                assert_eq!(files.len(), 1);
                let raw = std::fs::read(files[0].as_ref().unwrap().path()).unwrap();
                let value: Value = serde_json::from_slice(&raw).unwrap();
                assert_eq!(value["account"], credit_account);
                assert_eq!(value["account_type"], "credit");
                assert_eq!(core::parse(&raw).unwrap().lines[0].amount.unwrap().to_string(), "-3.21");
                assert_eq!(seen.lock().unwrap().len(), 4);
                assert!(pull(true, db_path.clone(), Client::new(&url, "test-secret".into()).unwrap(), Some(CHECKING.into()), false, None, None).await.is_err());
            }
            let db = Db::open(db_path).unwrap();
            assert_eq!(db.conn().query_row("SELECT COUNT(*) FROM channel_connections", [], |r| r.get::<_, i64>(0)).unwrap(), if fail { 0 } else { 1 });
            drop(db);
            task.abort();
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test]
    async fn credit_discovery_rejects_missing_ids_duplicates_and_new_pagination() {
        for response in [json!({}), json!({"accounts":[{}]}), json!({"accounts":[{"id":CREDIT},{"id":CREDIT}]}), json!({"accounts":[],"page":{"nextPage":CURSOR}})] {
            let app = Router::new().route(
                "/api/v1/credit",
                get(move || {
                    let response = response.clone();
                    async move { Json(response) }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let task = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            assert!(Client::new(&url, "test-secret".into()).unwrap().credit_accounts().await.is_err());
            task.abort();
        }
    }

    #[test]
    fn credit_is_opt_in_for_both_cli_commands() {
        use clap::Parser;
        for (args, expected) in [
            (vec!["means", "mercury", "accounts"], false),
            (vec!["means", "mercury", "accounts", "--credit"], true),
            (vec!["means", "mercury", "pull"], false),
            (vec!["means", "mercury", "pull", "--credit", "--dry-run"], true),
        ] {
            let cli = crate::Cli::try_parse_from(args).unwrap();
            let Some(crate::Command::Mercury { command }) = cli.command else { panic!("expected Mercury command") };
            let (Command::Accounts { credit, .. } | Command::Pull { credit, .. }) = command else { panic!("expected depository or credit command") };
            assert_eq!(credit, expected);
        }
    }
    #[test]
    fn booking_cutoff_uses_inclusive_utc_posting_dates_and_rejects_undated_bookings() {
        let from = means_core::parse_date("2026-09-19").unwrap();
        let row = |date: &str| json!({"accountId":CHECKING,"status":"sent","postedAt":date,"createdAt":"2026-05-01T00:00:00Z"});
        let keep = row("2026-09-18T23:30:00-01:00");
        let records = vec![row("2026-09-18T23:59:59Z"), row("2026-09-19T00:00:00Z"), keep.clone(), row("2026-09-19T00:30:00+01:00")];
        assert_eq!(filter_bookings(records.clone(), CHECKING, None).unwrap(), records);
        assert_eq!(filter_bookings(records, CHECKING, Some(from)).unwrap(), vec![row("2026-09-19T00:00:00Z"), keep]);
        for bad in ["", "2026-02-30T00:00:00Z", "2026-09-19"] {
            assert!(filter_bookings(vec![row(bad)], CHECKING, Some(from)).is_err());
        }
        assert!(filter_bookings(vec![json!({"accountId":CHECKING,"status":"sent"})], CHECKING, Some(from)).is_err());
        assert!(filter_bookings(vec![json!({"accountId":CHECKING,"status":"pending"})], CHECKING, Some(from)).unwrap().is_empty());
        assert!(filter_bookings(vec![json!({"accountId":SAVINGS,"status":"sent","postedAt":"2020-01-01T00:00:00Z"})], CHECKING, Some(from)).is_err());
    }

    #[tokio::test]
    async fn saved_and_explicit_cutoffs_filter_both_depository_and_credit_pulls() {
        for credit in [false, true] {
            let account_id = if credit { CREDIT } else { CHECKING };
            let seen: Seen = Arc::new(Mutex::new(Vec::new()));
            let log = seen.clone();
            let app = Router::new().route("/api/v1/accounts", get(accounts)).route("/api/v1/credit", get(|| async { Json(json!({"accounts":[{"id":CREDIT,"name":"IO"}]})) })).route(
                "/api/v1/transactions",
                get(move |Query(q): Query<HashMap<String, String>>| {
                    let log = log.clone();
                    async move {
                        assert_eq!(q["accountId"], account_id);
                        assert!(!q.contains_key("start"));
                        assert!(matches!(q["postedStart"].as_str(), "2026-09-19T00:00:00Z" | "2026-09-20T00:00:00Z"));
                        log.lock().unwrap().push(q.clone());
                        let row = |id: &str, date: &str| json!({"id":id,"accountId":account_id,"amount":-3.21,"status":"sent","postedAt":date,"createdAt":"2026-05-01T00:00:00Z"});
                        if q.contains_key("start_after") {
                            Json(json!({"transactions":[row(TX,"2026-09-19T00:00:00Z"),row(SAVINGS,"2026-09-20T00:00:00Z")],"page":{}}))
                        } else {
                            Json(json!({"transactions":[row(CURSOR,"2026-09-18T23:59:59Z")],"page":{"nextPage":CURSOR}}))
                        }
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let api = format!("http://{}", listener.local_addr().unwrap());
            let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let root = std::env::temp_dir().join(format!("means-mercury-cutoff-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let path = root.join("ledger.db");
            {
                let db = Db::open(&path).unwrap();
                let mut c = db.conn();
                means_core::connections::discover(
                    &c,
                    core::CHANNEL,
                    "",
                    &[means_core::connections::ConnectionAccount {
                        provider_account_id: account_id.into(),
                        currency: "USD".into(),
                        provider_type: if credit { "credit" } else { "checking" }.into(),
                        ..Default::default()
                    }],
                )
                .unwrap();
                let id = means_core::connections::list(&c).unwrap()[0].id;
                means_core::connections::configure(&mut c, id, None, "2026-09-19").unwrap();
            }
            let client = || Client::new(&api, "test-secret".into()).unwrap();
            let preview = pull_report(credit, path.clone(), client(), Some(account_id.into()), true, None, None).await.unwrap();
            assert_eq!(preview[0].records, 2);
            assert!(!root.join("inbox").exists());
            let result = pull_report(credit, path.clone(), client(), Some(account_id.into()), false, None, Some(means_core::parse_date("2026-09-20").unwrap())).await.unwrap();
            assert_eq!(result[0].records, 1);
            let raw: Value = serde_json::from_slice(&std::fs::read(result[0].path.as_ref().unwrap()).unwrap()).unwrap();
            assert_eq!(raw["transactions"][0]["postedAt"], "2026-09-20T00:00:00Z");
            assert_eq!(raw["transactions"][0]["createdAt"], "2026-05-01T00:00:00Z");
            let queries = seen.lock().unwrap();
            assert_eq!(queries.len(), 4);
            assert!(queries[..2].iter().all(|q| q["postedStart"] == "2026-09-19T00:00:00Z"));
            assert!(queries[2..].iter().all(|q| q["postedStart"] == "2026-09-20T00:00:00Z"));
            let db = Db::open(&path).unwrap();
            assert_eq!(means_core::connections::list(&db.conn()).unwrap()[0].booked_from, "2026-09-19");
            drop(db);
            task.abort();
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
