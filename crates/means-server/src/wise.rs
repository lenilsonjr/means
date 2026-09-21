//! Wise business balance statements. Personal token stays at the HTTP boundary.
use crate::statement_http as http;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use clap::Subcommand;
use means_core::{connections::ConnectionAccount, imports::bank_api, Db};
use serde_json::{json, Value};
use std::path::PathBuf;
pub const API: &str = "https://api.wise.com";
const VERSION: &str = "2026Q3";
#[derive(Subcommand)]
pub enum Command {
    /// List business currency balances available to WISE_TOKEN
    Accounts {
        #[arg(long,default_value=API)]
        api: String,
    },
    /// Pull complete statements, or only bookings on/after a cutoff
    Pull {
        /// Stable profile:balance identity shown by accounts
        #[arg(long)]
        account: Option<String>,
        #[arg(long,value_parser=means_core::parse_date)]
        booked_from: Option<NaiveDate>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        inbox: Option<PathBuf>,
        #[arg(long,default_value=API)]
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
        if token.trim().is_empty() {
            bail!("WISE_TOKEN must not be empty")
        };
        Ok(Self { http: http::builder().build()?, base: http::origin(api)?, token })
    }
    fn env(api: &str) -> Result<Self> {
        Self::new(api, std::env::var("WISE_TOKEN").context("set WISE_TOKEN to your US Wise business API token")?)
    }
    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        http::response(self.http.get(self.base.join(&format!("/{VERSION}{path}"))?).query(query).bearer_auth(&self.token), "Wise").await
    }
    async fn accounts(&self) -> Result<Vec<Value>> {
        let profiles = self.get("/profiles", &[]).await?;
        let mut accounts = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for profile in profiles.as_array().context("Wise profiles response must be an array")? {
            if !profile["type"].as_str().is_some_and(|t| t.eq_ignore_ascii_case("business")) {
                continue;
            }
            let pid = positive(&profile["id"])?;
            let balances = self.get(&format!("/profiles/{pid}/balances"), &[("types", "STANDARD,SAVINGS".into())]).await?;
            for balance in balances.as_array().context("Wise balances response must be an array")? {
                if !matches!(balance["type"].as_str(), Some("STANDARD" | "SAVINGS")) {
                    bail!("unknown Wise balance type")
                }
                if balance["investmentState"].as_str().is_some_and(|s| s != "NOT_INVESTED") {
                    eprintln!("Skipping invested Wise balance; investment holdings need separate coverage");
                    continue;
                }
                let bid = positive(&balance["id"])?;
                let id = format!("{pid}:{bid}");
                if !seen.insert(id.clone()) {
                    bail!("Wise repeated a balance identity")
                }
                let currency = balance["currency"].as_str().context("Wise balance has no currency")?;
                accounts.push(json!({"id":id,"profile_id":pid,"balance_id":bid,"currency":currency,"name":format!("Wise {} {} {}",profile["businessName"].as_str().unwrap_or("Business"),currency,balance["name"].as_str().unwrap_or("")),"provider_account":balance}));
            }
        }
        Ok(accounts)
    }
    async fn statements(&self, account: &Value, from: NaiveDate, to: NaiveDate) -> Result<Vec<Value>> {
        let pid = positive(&account["profile_id"])?;
        let bid = positive(&account["balance_id"])?;
        let currency = account["currency"].as_str().context("missing currency")?;
        let mut rows = Vec::new();
        let mut refs = std::collections::HashSet::new();
        for (start, end) in http::ranges(from, to, 365)? {
            let statement = self
                .get(
                    &format!("/profiles/{pid}/balance-statements/{bid}/statement.json"),
                    &[
                        ("currency", currency.into()),
                        ("intervalStart", format!("{start}T00:00:00.000Z")),
                        ("intervalEnd", format!("{end}T23:59:59.999Z")),
                        ("type", "COMPACT".into()),
                        ("statementLocale", "en".into()),
                    ],
                )
                .await?;
            if statement["request"]["balanceId"].as_i64() != Some(bid)
                || statement["request"]["profileId"].as_i64() != Some(pid)
                || statement["request"]["currency"] != currency
                || statement["query"]["currency"] != currency
            {
                bail!("Wise returned a statement for another balance/currency")
            }
            for (key, expected) in [("intervalStart", format!("{start}T00:00:00.000Z")), ("intervalEnd", format!("{end}T23:59:59.999Z"))] {
                let returned = DateTime::parse_from_rfc3339(statement["request"][key].as_str().context("Wise omitted statement interval")?).context("invalid Wise statement interval")?;
                if returned != DateTime::parse_from_rfc3339(&expected)? {
                    bail!("Wise returned a different statement interval; account not published")
                }
            }
            for row in statement["transactions"].as_array().context("Wise statement has no transactions array")? {
                let date = DateTime::parse_from_rfc3339(row["date"].as_str().context("Wise transaction has no date")?).context("invalid Wise transaction date")?.with_timezone(&Utc).date_naive();
                if date < start || date > end {
                    bail!("Wise returned a transaction outside the requested interval")
                }
                let reference = row["referenceNumber"].as_str().filter(|s| !s.is_empty()).context("Wise transaction has no stable reference")?;
                if !refs.insert(reference.to_owned()) {
                    bail!("Wise repeated a transaction reference; account not published")
                }
                rows.push(row.clone());
            }
        }
        Ok(rows)
    }
}
fn positive(value: &Value) -> Result<i64> {
    value.as_i64().filter(|v| *v > 0).context("invalid Wise numeric ID")
}
fn connection(a: &Value) -> ConnectionAccount {
    ConnectionAccount {
        channel: "wise".into(),
        provider_account_id: a["id"].as_str().unwrap_or_default().into(),
        currency: a["currency"].as_str().unwrap_or_default().into(),
        name: a["name"].as_str().unwrap_or_default().into(),
        provider_type: "bank".into(),
        ..Default::default()
    }
}
pub async fn discover(api: &str) -> Result<Vec<ConnectionAccount>> {
    Ok(Client::env(api)?.accounts().await?.iter().map(connection).collect())
}
async fn pull(path: PathBuf, client: Client, only: Option<String>, cutoff: Option<NaiveDate>, dry: bool, inbox: Option<PathBuf>) -> Result<()> {
    if let Some(id) = &only {
        let parts = id.split(':').collect::<Vec<_>>();
        if parts.len() != 2 || parts.iter().any(|v| v.parse::<i64>().map_or(true, |n| n <= 0)) {
            bail!("--account must be profile:balance as shown by wise accounts")
        }
    }
    let mut accounts = client.accounts().await?;
    accounts.retain(|a| only.as_ref().is_none_or(|id| a["id"] == *id));
    if accounts.is_empty() {
        bail!("no matching Wise business balances; run means wise accounts")
    }
    let db = Db::open(&path)?;
    let saved = means_core::connections::list(&db.conn())?;
    let inbox = inbox.unwrap_or_else(|| path.parent().unwrap_or(std::path::Path::new(".")).join("inbox"));
    for account in accounts {
        let id = account["id"].as_str().context("missing Wise identity")?;
        let preference = saved.iter().find(|a| a.channel == "wise" && a.provider_account_id == id).map(|a| a.booked_from.as_str()).unwrap_or_default();
        let from = match cutoff.or(means_core::parse_opt_date(preference)?) {
            Some(d) => d,
            None => DateTime::parse_from_rfc3339(account["provider_account"]["creationTime"].as_str().context("Wise omitted balance creationTime; set --booked-from")?)
                .context("invalid Wise creationTime; set --booked-from")?
                .with_timezone(&Utc)
                .date_naive(),
        };
        let rows = client.statements(&account, from, Utc::now().date_naive()).await?;
        let envelope = json!({"channel":"wise","version":1,"account":account,"transactions":rows,"booked_from":from,"pulled_at":Utc::now().to_rfc3339(),"statement_type":"COMPACT"});
        let file = bank_api::publish(&mut db.conn(), &inbox, &envelope, dry)?;
        println!("{id}: {} records, booked from {from}{}", rows.len(), file.map(|p| format!(" -> {}", p.display())).unwrap_or_else(|| " (dry run)".into()));
    }
    Ok(())
}
pub async fn pull_connected(api: &str, path: PathBuf, account: String, inbox: PathBuf) -> Result<()> {
    pull(path, Client::env(api)?, Some(account), None, false, Some(inbox)).await
}
pub async fn run(path: PathBuf, command: Command) -> Result<()> {
    match command {
        Command::Accounts { api } => {
            for a in Client::env(&api)?.accounts().await? {
                println!("{}\t{}\t{}", a["id"].as_str().unwrap_or_default(), a["currency"].as_str().unwrap_or_default(), a["name"].as_str().unwrap_or_default());
            }
            Ok(())
        }
        Command::Pull { account, booked_from, dry_run, inbox, api } => pull(path, Client::env(&api)?, account, booked_from, dry_run, inbox).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::{Path, Query, State},
        http::{HeaderMap, StatusCode},
        response::{IntoResponse, Response},
        routing::get,
        Json, Router,
    };
    use std::{
        collections::HashMap,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };
    async fn statement(Path((profile, balance)): Path<(i64, i64)>, Query(q): Query<HashMap<String, String>>, State(mode): State<Arc<AtomicUsize>>, headers: HeaderMap) -> Response {
        assert_eq!(headers["authorization"], "Bearer synthetic-secret");
        assert_eq!((profile, balance), (1, 2));
        assert_eq!(q["type"], "COMPACT");
        assert_eq!(q["currency"], "USD");
        let start = DateTime::parse_from_rfc3339(&q["intervalStart"]).unwrap();
        let end = DateTime::parse_from_rfc3339(&q["intervalEnd"]).unwrap();
        assert!((end - start).num_days() < 365);
        if mode.load(Ordering::SeqCst) == 1 {
            return (StatusCode::FORBIDDEN, "synthetic-secret private data").into_response();
        }
        // Live statements identify the balance in request, not query.accountId.
        // Whole-second instants may omit the milliseconds sent by the client.
        let mut response = json!({
            "request": {"profileId": profile, "balanceId": balance, "currency": "USD",
                "intervalStart": start.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true), "intervalEnd": q["intervalEnd"]},
            "query": {"profileId": profile, "currency": "USD", "type": "COMPACT",
                "intervalStart": q["intervalStart"], "intervalEnd": q["intervalEnd"],
                "splitRefundFees": false, "addStamp": false, "timezone": "UTC"},
            "transactions": [{"referenceNumber": format!("TX-{}", start.date_naive()),
                "type": "DEBIT", "date": q["intervalStart"], "amount": {"value": -10.25, "currency": "USD"},
                "details": {"description": "Supplier"}, "totalFees": {"value": 0.25, "currency": "USD"}}]
        });
        match mode.load(Ordering::SeqCst) {
            2 => response["request"]["balanceId"] = json!(3),
            3 => response["request"]["currency"] = json!("EUR"),
            4 => response["query"]["currency"] = json!("EUR"),
            5 => response["request"]["profileId"] = json!(9),
            6 => response["request"]["intervalStart"] = json!("2000-01-01T00:00:00Z"),
            7 => {
                response["request"].as_object_mut().unwrap().remove("balanceId");
            }
            _ => {}
        }
        Json(response).into_response()
    }
    #[tokio::test]
    async fn wise_discovers_business_balances_chunks_history_and_publishes_only_complete_evidence() {
        let mode = Arc::new(AtomicUsize::new(0));
        let app=Router::new()
            .route("/2026Q3/profiles",get(||async{Json(json!([{"id":1,"type":"BUSINESS","businessName":"Company"},{"id":9,"type":"PERSONAL"}]))}))
            .route("/2026Q3/profiles/1/balances",get(|Query(q):Query<HashMap<String,String>>|async move{assert_eq!(q["types"],"STANDARD,SAVINGS");Json(json!([{"id":2,"type":"STANDARD","currency":"USD","investmentState":"NOT_INVESTED","creationTime":"2024-01-01T00:00:00Z"},{"id":3,"type":"SAVINGS","currency":"EUR","investmentState":"INVESTED","creationTime":"2024-01-01T00:00:00Z"}]))}))
            .route("/2026Q3/profiles/{profile}/balance-statements/{balance}/statement.json",get(statement)).with_state(mode.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = || Client::new(&api, "synthetic-secret".into()).unwrap();
        let accounts = client().accounts().await.unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0]["id"], "1:2");
        let rows = client().statements(&accounts[0], "2024-01-01".parse().unwrap(), "2025-01-02".parse().unwrap()).await.unwrap();
        assert_eq!(rows.len(), 2);
        let root = std::env::temp_dir().join(format!("means-wise-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("ledger.db");
        let cutoff = Some(Utc::now().date_naive());
        pull(path.clone(), client(), Some("1:2".into()), cutoff, true, None).await.unwrap();
        assert!(!root.join("inbox").exists());
        pull(path.clone(), client(), Some("1:2".into()), cutoff, false, None).await.unwrap();
        assert_eq!(std::fs::read_dir(root.join("inbox")).unwrap().count(), 1);
        mode.store(1, Ordering::SeqCst);
        let failed = root.join("failed");
        let error = pull(path.clone(), client(), Some("1:2".into()), cutoff, false, Some(failed.clone())).await.unwrap_err();
        assert!(error.to_string().contains("403"));
        assert!(!error.to_string().contains("synthetic-secret"));
        assert!(!failed.exists());
        for variant in 2..=7 {
            mode.store(variant, Ordering::SeqCst);
            let error = pull(path.clone(), client(), Some("1:2".into()), cutoff, false, Some(failed.clone())).await.unwrap_err();
            let expected = if variant == 6 { "different statement interval" } else { "another balance" };
            assert!(error.to_string().contains(expected), "variant {variant}: {error}");
            assert!(!failed.exists());
        }
        task.abort();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn wise_credentials_and_origins_are_validated() {
        assert!(Client::new(API, String::new()).is_err());
        for url in ["http://example.com", "https://secret@example.com", "https://api.wise.com/path", "https://api.wise.com?token=x"] {
            assert!(Client::new(url, "test".into()).is_err())
        }
    }
}
