//! Enable Banking's HTTP and browser boundary. Keys and one-time codes stay here.

use anyhow::{bail, Context, Result};
use axum::{
    extract::{RawQuery, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use clap::Subcommand;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use means_core::{imports::enable_banking as core, Db};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::oneshot;

#[derive(Subcommand)]
pub enum Command {
    /// List banks supporting personal accounts (requires Enable Banking app credentials)
    Banks {
        #[arg(long)]
        country: Option<String>,
        #[arg(long, default_value = "https://api.enablebanking.com")]
        api: String,
    },
    /// Authorize or renew bank access in the browser, with a loopback callback
    Connect {
        #[arg(long)]
        bank: String,
        #[arg(long)]
        country: String,
        /// Local loopback callback port; proxy upstream when --redirect is set
        #[arg(long, default_value_t = 53682)]
        callback_port: u16,
        /// Advertised callback URL, e.g. https://means.example.ts.net/callback.
        /// Proxy this path to the loopback callback port; register the same URL with Enable Banking.
        #[arg(long, value_parser = callback_redirect)]
        redirect: Option<reqwest::Url>,
        /// Print the authorization URL without opening the browser
        #[arg(long)]
        no_open: bool,
        #[arg(long, default_value = "https://api.enablebanking.com")]
        api: String,
    },
    /// Fetch transaction history into the inbox (all available history unless a cutoff is set)
    Pull {
        /// Restrict the pull to one saved consent
        #[arg(long)]
        session: Option<String>,
        /// Select one provider account by session UID or stable identification_hash:currency
        #[arg(long)]
        account: Option<String>,
        /// Include bookings on or after this date; overrides the saved account cutoff
        #[arg(long, value_parser = means_core::parse_date)]
        booked_from: Option<chrono::NaiveDate>,
        /// Fetch and report, without publishing files or changing account mappings
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        inbox: Option<PathBuf>,
        #[arg(long, default_value = "https://api.enablebanking.com")]
        api: String,
    },
    /// List locally saved consents and their expiration dates
    Sessions,
}

struct Client {
    http: reqwest::Client,
    base: reqwest::Url,
    application_id: String,
    key: EncodingKey,
}

fn checked_url(raw: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw).context("invalid Enable Banking URL")?;
    let loopback = url.host_str().is_some_and(|host| host == "localhost" || host.trim_matches(['[', ']']).parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()));
    if !(url.scheme() == "https" || (url.scheme() == "http" && loopback)) || !url.username().is_empty() || url.password().is_some() {
        bail!("Enable Banking URLs require HTTPS (HTTP is allowed only on loopback), without embedded credentials");
    }
    Ok(url)
}

impl Client {
    fn from_env(api: &str) -> Result<Self> {
        let base = checked_url(api)?;
        if base.path() != "/" || base.query().is_some() || base.fragment().is_some() {
            bail!("Enable Banking API must be an origin without a path, query, or fragment");
        }
        let application_id = std::env::var("ENABLE_BANKING_APP_ID").context("set ENABLE_BANKING_APP_ID from your Enable Banking application")?;
        uuid::Uuid::parse_str(&application_id).context("ENABLE_BANKING_APP_ID must be a UUID")?;
        let path = std::env::var("ENABLE_BANKING_KEY_FILE").context("set ENABLE_BANKING_KEY_FILE to your application's RSA private key PEM file")?;
        let pem = std::fs::read(&path).context("read ENABLE_BANKING_KEY_FILE")?;
        let key = EncodingKey::from_rsa_pem(&pem).context("load RSA private key")?;
        let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(90)).user_agent(format!("means/{}", crate::VERSION)).build()?;
        Ok(Self { http, base, application_id, key })
    }

    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value> {
        let url = self.base.join(path)?;
        if url.origin() != self.base.origin() {
            bail!("Enable Banking request left the configured API origin");
        }
        let now = chrono::Utc::now().timestamp();
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(self.application_id.clone());
        let token = jsonwebtoken::encode(&header, &json!({"iss":"enablebanking.com","aud":"api.enablebanking.com","iat":now,"exp":now+3600}), &self.key)?;
        let mut request = self.http.request(method, url).bearer_auth(token).header("accept", "application/json");
        if let Some(body) = body {
            request = request.header("content-type", "application/json").body(serde_json::to_vec(&body)?);
        }
        let response = request.send().await.map_err(|error| error.without_url()).context("reach Enable Banking")?;
        let status = response.status();
        if !status.is_success() {
            // Do not echo arbitrary provider responses: they may include request credentials.
            bail!("Enable Banking returned HTTP {status} for {}", path.split('?').next().unwrap_or("request"));
        }
        serde_json::from_slice(&response.bytes().await?).context("invalid Enable Banking JSON response")
    }

    async fn banks(&self) -> Result<Vec<Value>> {
        let response = self.call(reqwest::Method::GET, "/aspsps", None).await?;
        let banks = response["aspsps"].as_array().context("Enable Banking returned no aspsps array")?;
        Ok(banks.iter().filter(|b| b["psu_types"].as_array().is_some_and(|types| types.iter().any(|v| v == "personal"))).cloned().collect())
    }
}

/// The continuation key is opaque data, never a URL. Even an empty page may
/// carry a key, and every page retains the same strategy and optional date cutoff.
async fn transactions(client: &Client, uid: &str, booked_from: Option<chrono::NaiveDate>) -> Result<Vec<Value>> {
    uuid::Uuid::parse_str(uid).context("invalid Enable Banking account UID")?;
    let mut all = Vec::new();
    let mut next: Option<String> = None;
    let mut seen = std::collections::HashSet::new();
    for _ in 0..1000 {
        let mut url = client.base.join(&format!("/accounts/{uid}/transactions"))?;
        if let Some(from) = booked_from {
            url.query_pairs_mut().append_pair("strategy", "default").append_pair("date_from", &from.to_string());
        } else {
            url.query_pairs_mut().append_pair("strategy", "longest");
        }
        if let Some(key) = &next {
            url.query_pairs_mut().append_pair("continuation_key", key);
        }
        let path = format!("{}?{}", url.path(), url.query().unwrap_or_default());
        let page = client.call(reqwest::Method::GET, &path, None).await?;
        let rows = page["transactions"].as_array().context("Enable Banking returned no transactions array; account not published")?;
        all.extend(rows.iter().cloned());
        match page.get("continuation_key") {
            None | Some(Value::Null) => return Ok(all),
            Some(Value::String(key)) if !key.is_empty() => {
                if !seen.insert(key.clone()) {
                    bail!("Enable Banking repeated a continuation key; account not published");
                }
                next = Some(key.clone());
            }
            _ => bail!("Enable Banking returned an invalid continuation key; account not published"),
        }
    }
    bail!("Enable Banking exceeded 1000 transaction pages; account not published")
}

#[derive(Default)]
struct PullScope {
    account: Option<String>,
    booked_from: Option<chrono::NaiveDate>,
}

fn filter_bookings(records: Vec<Value>, primary: &str, currency: Option<&str>, scope: &PullScope, saved: &HashMap<String, chrono::NaiveDate>) -> Result<Vec<Value>> {
    let mut kept = Vec::new();
    for record in records {
        let unit = record["transaction_amount"]["currency"].as_str().context("transaction has no currency; account not published")?;
        if currency.is_some_and(|selected| selected != unit) {
            continue;
        }
        let from = scope.booked_from.or_else(|| saved.get(&format!("{primary}:{unit}")).copied());
        if let Some(from) = from {
            // Pending rows are not bookable and may legitimately have no booking date.
            if record["status"] == "PDNG" {
                continue;
            }
            let date = record["booking_date"].as_str().context("transaction has no booking date; cannot apply cutoff; account not published")?;
            let date = means_core::parse_date(date).context("invalid booking date; cannot apply cutoff; account not published")?;
            if date < from {
                continue;
            }
        }
        kept.push(record);
    }
    Ok(kept)
}

async fn pull(db_path: PathBuf, client: Client, only: Option<String>, dry_run: bool, inbox: Option<PathBuf>, scope: PullScope) -> Result<()> {
    let db = Db::open(&db_path)?;
    let directory = inbox.unwrap_or_else(|| db_path.parent().unwrap_or_else(|| std::path::Path::new(".")).join("inbox"));
    let mut sessions = core::sessions(&db.conn())?;
    // Prefer the most recent authorization while retaining disjoint accounts from older consents.
    sessions.reverse();
    if let Some(id) = &only {
        sessions.retain(|s| &s.id == id);
    }
    if sessions.is_empty() {
        bail!("no saved Enable Banking consent; run `means enable-banking connect` first");
    }
    let saved: HashMap<String, chrono::NaiveDate> = means_core::connections::list(&db.conn())?
        .into_iter()
        .filter(|a| a.channel == core::CHANNEL && !a.booked_from.is_empty())
        .map(|a| Ok((a.provider_account_id, means_core::parse_date(&a.booked_from)?)))
        .collect::<means_core::Result<_>>()?;
    let mut selected = false;
    let mut active = false;
    let mut seen = std::collections::HashSet::new();
    let mut published = 0;
    for session in sessions {
        let expiry = chrono::DateTime::parse_from_rfc3339(&session.valid_until)?;
        if expiry <= chrono::Utc::now() {
            if only.is_some() {
                bail!("session {} expired; connect {} ({}) again", session.id, session.bank, session.country);
            }
            eprintln!("Skipping expired session {} for {} ({}); connect again to renew it.", session.id, session.bank, session.country);
            continue;
        }
        uuid::Uuid::parse_str(&session.id).context("invalid saved session ID")?;
        let details = client.call(reqwest::Method::GET, &format!("/sessions/{}", session.id), None).await?;
        let status = details["status"].as_str().context("Enable Banking returned no session status")?;
        let valid_until = details["access"]["valid_until"].as_str().context("Enable Banking returned no session expiry")?;
        if status != "AUTHORIZED" || chrono::DateTime::parse_from_rfc3339(valid_until)? <= chrono::Utc::now() {
            if only.is_some() {
                bail!("session {} is not currently authorized; connect again", session.id);
            }
            eprintln!("Skipping inactive session {} for {} ({}); connect again to renew it.", session.id, session.bank, session.country);
            continue;
        }
        active = true;
        let accounts = details["accounts_data"].as_array().context("Enable Banking returned no session accounts_data")?;
        for account in accounts {
            let uid = account["uid"].as_str().context("session account has no UID")?;
            uuid::Uuid::parse_str(uid).context("invalid session account UID")?;
            let primary = account["identification_hash"].as_str().filter(|h| !h.trim().is_empty()).context("account has no primary identification hash")?;
            let selected_currency = scope.account.as_deref().and_then(|id| id.strip_prefix(&format!("{primary}:")));
            if scope.account.as_deref().is_some_and(|id| id != uid && selected_currency.is_none()) {
                continue;
            }
            let mut hashes = vec![primary.to_owned()];
            if let Some(aliases) = account["identification_hashes"].as_array() {
                for alias in aliases {
                    let alias = alias.as_str().filter(|h| !h.is_empty()).context("invalid account identification hash")?;
                    if !hashes.iter().any(|h| h == alias) {
                        hashes.push(alias.into());
                    }
                }
            }
            let resource = client.call(reqwest::Method::GET, &format!("/accounts/{uid}/details"), None).await?;
            if !resource.is_object() {
                bail!("account {uid} details are not an object");
            }
            if resource.get("uid").is_some_and(|returned| returned.as_str() != Some(uid)) {
                bail!("Enable Banking returned details for a different account UID");
            }
            let currency = resource["currency"].as_str().unwrap_or("XXX").to_owned();
            if seen.contains(&(primary.to_owned(), currency.clone())) {
                continue;
            }
            if selected_currency.is_some_and(|unit| unit.is_empty() || (currency != "XXX" && unit != currency)) {
                continue;
            }
            selected = true;
            let from = scope.booked_from.or_else(|| saved.get(&format!("{primary}:{}", selected_currency.unwrap_or(&currency))).copied());
            let records = transactions(&client, uid, from).await?;
            let fetched = records.len();
            let records = filter_bookings(records, primary, selected_currency, &scope, &saved)?;
            if from.is_some() || fetched != records.len() {
                println!(
                    "{primary}:{}: booked from {}; {} returned records excluded by account/booking filter",
                    selected_currency.unwrap_or(&currency),
                    from.map(|d| d.to_string()).unwrap_or_else(|| "saved per-currency cutoffs".into()),
                    fetched - records.len()
                );
            }
            let files = core::publish_account(&mut db.conn(), &directory, &session.id, &resource, &hashes, records, dry_run)?;
            for file in files {
                println!("{} {}: {} records{}", file.account_ref, file.currency, file.records, file.path.as_ref().map(|p| format!(" -> {}", p.display())).unwrap_or_else(|| " (dry run)".into()));
                published += 1;
            }
            seen.insert((primary.to_owned(), currency));
        }
    }
    if !active {
        bail!("no active Enable Banking consent; connect your bank again");
    }
    if scope.account.is_some() && !selected {
        bail!("selected Enable Banking account was not found in an active consent; use its session UID or identification_hash:currency");
    }
    println!("{published} account/currency files {}", if dry_run { "would be published" } else { "published to the inbox" });
    Ok(())
}

#[derive(Clone)]
struct Callback {
    state: String,
    hosts: Vec<String>,
    result: Arc<Mutex<Option<oneshot::Sender<Result<String>>>>>,
}

enum CallbackOutcome {
    Authorized(String),
    Denied,
}

fn callback_code(query: Option<&str>, expected: &str) -> Result<CallbackOutcome> {
    let mut parsed = reqwest::Url::parse("http://localhost/")?;
    parsed.set_query(query);
    let mut fields = HashMap::new();
    for (name, value) in parsed.query_pairs() {
        if fields.insert(name.into_owned(), value.into_owned()).is_some() {
            bail!("duplicate callback parameter");
        }
    }
    if fields.get("state").map(String::as_str) != Some(expected) {
        bail!("callback state mismatch");
    }
    if fields.contains_key("error") {
        return Ok(CallbackOutcome::Denied);
    }
    fields.remove("code").filter(|s| !s.is_empty()).map(CallbackOutcome::Authorized).context("callback contains no authorization code")
}

async fn callback(State(callback): State<Callback>, headers: HeaderMap, RawQuery(query): RawQuery) -> Response {
    let host_ok =
        headers.get_all("host").iter().count() == 1 && headers.get("host").and_then(|h| h.to_str().ok()).is_some_and(|host| callback.hosts.iter().any(|allowed| allowed.eq_ignore_ascii_case(host)));
    if !host_ok {
        return (StatusCode::BAD_REQUEST, "Invalid callback host").into_response();
    }
    let result = callback_code(query.as_deref(), &callback.state);
    let (status, message) = match result {
        Ok(outcome) => {
            let (code, message) = match outcome {
                CallbackOutcome::Authorized(code) => (Ok(code), "Authorization received. You can close this tab and return to means."),
                CallbackOutcome::Denied => (Err(anyhow::anyhow!("bank authorization was declined or failed")), "Bank authorization failed. Return to means to connect again."),
            };
            let sender = callback.result.lock().expect("callback mutex").take();
            if let Some(sender) = sender {
                let _ = sender.send(code);
                (StatusCode::OK, message)
            } else {
                (StatusCode::GONE, "This authorization callback has already been used.")
            }
        }
        Err(_) => (StatusCode::BAD_REQUEST, "Authorization callback rejected. Return to means and try again."),
    };
    (status, [("cache-control", "no-store"), ("referrer-policy", "no-referrer"), ("content-security-policy", "default-src 'none'")], message).into_response()
}

fn callback_redirect(raw: &str) -> Result<reqwest::Url> {
    let url = checked_url(raw)?;
    if url.path() != "/callback" || url.query().is_some() || url.fragment().is_some() {
        bail!("redirect must end in /callback, without a query or fragment");
    }
    Ok(url)
}

struct CallbackOptions {
    port: u16,
    no_open: bool,
    redirect: Option<reqwest::Url>,
}

async fn connect(db_path: PathBuf, client: Client, bank: String, country: String, options: CallbackOptions) -> Result<()> {
    let saved = connect_flow(db_path, client, bank, country, options, |url| println!("Authorize in your browser:\n{url}")).await?;
    println!("Connected {} ({}), session {}, valid until {}", saved.bank, saved.country, saved.id, saved.valid_until);
    Ok(())
}

async fn connect_flow(db_path: PathBuf, client: Client, bank: String, country: String, options: CallbackOptions, progress: impl Fn(String)) -> Result<core::Session> {
    let db = Db::open(&db_path)?;
    let country = country.to_ascii_uppercase();
    let banks = client.banks().await?;
    let selected =
        banks.iter().find(|b| b["name"] == bank && b["country"] == country).context("bank not found for personal accounts; run `means enable-banking banks --country CODE` for current names")?;
    let validity = selected["maximum_consent_validity"].as_i64().filter(|v| *v > 0).context("bank returned no valid maximum consent lifetime")?;
    let until = chrono::Utc::now().checked_add_signed(chrono::Duration::try_seconds(validity).context("invalid bank consent lifetime")?).context("bank consent lifetime out of range")?;
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, options.port)).await.context("bind local authorization callback")?;
    let port = listener.local_addr()?.port();
    let redirect = options.redirect.unwrap_or(callback_redirect(&format!("http://127.0.0.1:{port}/callback"))?);
    // A proxy may preserve the advertised authority or use the local upstream
    // authority. Neither Forwarded nor X-Forwarded-Host expands this allowlist.
    let advertised_host = match redirect.port() {
        Some(port) => format!("{}:{port}", redirect.host().context("redirect requires a host")?),
        None => redirect.host().context("redirect requires a host")?.to_string(),
    };
    let hosts = vec![format!("127.0.0.1:{port}"), advertised_host];
    let state = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    let response = client.call(reqwest::Method::POST, "/auth", Some(json!({"access":{"valid_until":until.to_rfc3339(),"balances":true,"transactions":true},"aspsp":{"name":bank,"country":country},"state":state,"redirect_url":redirect.as_str(),"psu_type":"personal"}))).await?;
    let auth_url = checked_url(response["url"].as_str().context("Enable Banking returned no authorization URL")?)?;
    let (sender, receiver) = oneshot::channel();
    let callback_state = Callback { state, hosts, result: Arc::new(Mutex::new(Some(sender))) };
    let router = Router::new().route("/callback", get(callback)).with_state(callback_state);
    let (stop, stopped) = oneshot::channel::<()>();
    let mut server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
    });
    progress(auth_url.to_string());
    if !options.no_open {
        if let Err(error) = open::that(auth_url.as_str()) {
            eprintln!("Could not open the browser ({error}); open the URL above.");
        }
    }
    let result = tokio::time::timeout(Duration::from_secs(600), receiver).await;
    let _ = stop.send(());
    // A stalled local client must not prevent timeout or completion.
    match tokio::time::timeout(Duration::from_secs(2), &mut server).await {
        Ok(stopped) => {
            stopped.context("callback server task failed")?.context("callback server failed")?;
        }
        Err(_) => server.abort(),
    }
    let code = result.context("bank authorization timed out after ten minutes; connect again")?.context("authorization callback stopped")??;
    let session = client.call(reqwest::Method::POST, "/sessions", Some(json!({"code":code}))).await?;
    let saved = core::save_session(&db.conn(), &bank, &country, &session)?;
    Ok(saved)
}

pub async fn banks_connected(api: &str, country: &str) -> Result<Vec<(String, String)>> {
    Ok(Client::from_env(api)?
        .banks()
        .await?
        .into_iter()
        .filter(|b| country.is_empty() || b["country"].as_str().is_some_and(|c| c.eq_ignore_ascii_case(country)))
        .map(|b| (b["name"].as_str().unwrap_or_default().into(), b["country"].as_str().unwrap_or_default().into()))
        .collect())
}
pub async fn authorize_connected(api: &str, db_path: PathBuf, bank: String, country: String, port: u16, progress: impl Fn(String)) -> Result<core::Session> {
    connect_flow(db_path, Client::from_env(api)?, bank, country, CallbackOptions { port, no_open: false, redirect: None }, progress).await
}
pub async fn discover_connected(api: &str, session: &str) -> Result<Vec<means_core::connections::ConnectionAccount>> {
    uuid::Uuid::parse_str(session).context("invalid consent ID")?;
    let client = Client::from_env(api)?;
    let details = client.call(reqwest::Method::GET, &format!("/sessions/{session}"), None).await?;
    let until = details["access"]["valid_until"].as_str().context("consent has no expiry")?;
    if details["status"] != "AUTHORIZED" || chrono::DateTime::parse_from_rfc3339(until)? <= chrono::Utc::now() {
        bail!("Consent expired or inactive; authorize the bank again");
    }
    let mut out = Vec::new();
    for a in details["accounts_data"].as_array().context("consent has no accounts")? {
        let uid = a["uid"].as_str().context("account has no UID")?;
        uuid::Uuid::parse_str(uid).context("invalid account UID")?;
        let hash = a["identification_hash"].as_str().filter(|h| !h.is_empty()).context("account has no identification hash")?;
        let resource = client.call(reqwest::Method::GET, &format!("/accounts/{uid}/details"), None).await?;
        if resource.get("uid").is_some_and(|id| id.as_str() != Some(uid)) {
            bail!("account details returned another UID");
        }
        let currency = resource["currency"].as_str().filter(|s| !s.is_empty()).context("account has no currency")?;
        out.push(means_core::connections::ConnectionAccount {
            channel: core::CHANNEL.into(),
            item_id: session.into(),
            provider_account_id: format!("{hash}:{currency}"),
            provider_type: resource["cash_account_type"].as_str().unwrap_or_default().into(),
            name: resource["name"].as_str().unwrap_or_default().into(),
            currency: currency.into(),
            ..Default::default()
        });
    }
    Ok(out)
}
pub async fn pull_connected(api: &str, db_path: PathBuf, session: String, account: String, inbox: PathBuf) -> Result<()> {
    pull(db_path, Client::from_env(api)?, Some(session), false, Some(inbox), PullScope { account: Some(account), ..Default::default() }).await
}

pub async fn run(db_path: PathBuf, command: Command) -> Result<()> {
    match command {
        Command::Banks { country, api } => {
            let client = Client::from_env(&api)?;
            for bank in client.banks().await? {
                if country.as_ref().is_none_or(|c| bank["country"].as_str().is_some_and(|b| b.eq_ignore_ascii_case(c))) {
                    println!("{}\t{}", bank["country"].as_str().unwrap_or(""), bank["name"].as_str().unwrap_or(""));
                }
            }
            Ok(())
        }
        Command::Connect { bank, country, callback_port, redirect, no_open, api } => {
            connect(db_path, Client::from_env(&api)?, bank, country, CallbackOptions { port: callback_port, no_open, redirect }).await
        }
        Command::Pull { session, account, booked_from, dry_run, inbox, api } => pull(db_path, Client::from_env(&api)?, session, dry_run, inbox, PullScope { account, booked_from }).await,
        Command::Sessions => {
            let db = Db::open(&db_path)?;
            for session in core::sessions(&db.conn())? {
                println!("{}\t{}\t{}\t{}", session.id, session.country, session.bank, session.valid_until);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        extract::Request,
        middleware::{self, Next},
        routing::post,
        Json,
    };
    use jsonwebtoken::{DecodingKey, Validation};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;

    // Public, generated test-only key pair; never use these keys for an application.
    const PRIVATE: &[u8] = include_bytes!("../tests/fixtures/enable-banking-test-key.pem");
    const PUBLIC: &[u8] = include_bytes!("../tests/fixtures/enable-banking-test-public.pem");
    const APP_ID: &str = "c3779ad2-80b4-44c6-8899-f1bc89941166";
    const SESSION_ID: &str = "3d9bcdf6-6dde-40ac-811c-4f92a3650e09";

    #[test]
    fn callback_and_url_validation_rejects_ambiguous_or_unsafe_inputs() {
        for query in [None, Some("state=wrong&code=secret"), Some("state=expected"), Some("state=expected&state=wrong&code=secret"), Some("state=expected&code=a&code=b")] {
            assert!(callback_code(query, "expected").is_err());
        }
        assert!(matches!(callback_code(Some("state=expected&code=a%2Bb"), "expected").unwrap(), CallbackOutcome::Authorized(code) if code == "a+b"));
        assert!(matches!(callback_code(Some("state=expected&error=access_denied"), "expected").unwrap(), CallbackOutcome::Denied));
        for url in ["http://example.com", "file:///tmp/key", "https://user:password@example.com"] {
            assert!(checked_url(url).is_err());
        }
        for url in ["https://api.enablebanking.com", "http://127.0.0.1:1234", "http://[::1]:1234"] {
            assert!(checked_url(url).is_ok());
        }
        for url in [
            "http://means.example.ts.net/callback",
            "https://user:pass@means.example.ts.net/callback",
            "https://means.example.ts.net/",
            "https://means.example.ts.net/callback?state=x",
            "https://means.example.ts.net/callback#secret",
        ] {
            assert!(callback_redirect(url).is_err(), "{url}");
        }
        for url in ["https://means.example.ts.net/callback", "https://means.example.ts.net:8443/callback", "http://127.0.0.1:53682/callback"] {
            assert!(callback_redirect(url).is_ok());
        }
        use clap::Parser;
        assert!(crate::Cli::try_parse_from(["means", "enable-banking", "connect", "--bank", "Test Bank", "--country", "PT", "--redirect", "https://means.example.ts.net/callback"]).is_ok());
        assert!(crate::Cli::try_parse_from(["means", "enable-banking", "connect", "--bank", "Test Bank", "--country", "PT", "--redirect", "http://means.example.ts.net/callback"]).is_err());
    }

    #[tokio::test]
    async fn callback_checks_host_and_state_and_accepts_a_code_only_once() {
        let (tx, mut rx) = oneshot::channel();
        let state = Callback { state: "expected".into(), hosts: vec!["127.0.0.1:1234".into()], result: Arc::new(Mutex::new(Some(tx))) };
        let app = Router::new().route("/callback", get(callback)).with_state(state);
        for (host, query) in [("evil.test", "state=expected&code=secret"), ("127.0.0.1:1234", "state=wrong&code=secret")] {
            let request = Request::builder().uri(format!("/callback?{query}")).header("host", host).header("x-forwarded-host", "127.0.0.1:1234").body(Body::empty()).unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert!(matches!(rx.try_recv(), Err(oneshot::error::TryRecvError::Empty)));
        }
        let duplicate_host = Request::builder().uri("/callback?state=expected&code=secret").header("host", "127.0.0.1:1234").header("host", "evil.test").body(Body::empty()).unwrap();
        assert_eq!(app.clone().oneshot(duplicate_host).await.unwrap().status(), StatusCode::BAD_REQUEST);
        assert!(matches!(rx.try_recv(), Err(oneshot::error::TryRecvError::Empty)));
        let request = || Request::builder().uri("/callback?state=expected&code=secret").header("host", "127.0.0.1:1234").body(Body::empty()).unwrap();
        let response = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(rx.await.unwrap().unwrap(), "secret");
        assert_eq!(app.oneshot(request()).await.unwrap().status(), StatusCode::GONE);
    }

    #[tokio::test]
    async fn valid_bank_denial_ends_the_wait_without_a_code() {
        let (tx, rx) = oneshot::channel();
        let state = Callback { state: "expected".into(), hosts: vec!["127.0.0.1:1234".into()], result: Arc::new(Mutex::new(Some(tx))) };
        let request = Request::builder().uri("/callback?state=expected&error=access_denied").header("host", "127.0.0.1:1234").body(Body::empty()).unwrap();
        let response = Router::new().route("/callback", get(callback)).with_state(state).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(rx.await.unwrap().is_err());
    }

    async fn verify_jwt(request: Request, next: Next) -> Response {
        let token = request.headers()["authorization"].to_str().unwrap().strip_prefix("Bearer ").unwrap();
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&["api.enablebanking.com"]);
        validation.set_issuer(&["enablebanking.com"]);
        let decoded = jsonwebtoken::decode::<Value>(token, &DecodingKey::from_rsa_pem(PUBLIC).unwrap(), &validation).unwrap();
        assert_eq!(decoded.header.kid.as_deref(), Some(APP_ID));
        assert_eq!(decoded.claims["exp"].as_i64().unwrap() - decoded.claims["iat"].as_i64().unwrap(), 3600);
        next.run(request).await
    }

    #[tokio::test]
    async fn browser_flow_exchanges_validated_code_and_persists_only_consent_metadata() {
        browser_flow(None).await;
    }

    #[tokio::test]
    async fn https_proxy_redirect_works_with_preserved_or_rewritten_host() {
        browser_flow(Some(true)).await;
        browser_flow(Some(false)).await;
    }

    async fn browser_flow(proxy_preserves_host: Option<bool>) {
        // Reserve a test port for the simulated proxy's known upstream. The
        // default flow still exercises a dynamically assigned callback port.
        let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let callback_port = if proxy_preserves_host.is_some() { reserved.local_addr().unwrap().port() } else { 0 };
        drop(reserved);
        let calls = Arc::new(AtomicUsize::new(0));
        let session_calls = calls.clone();
        let app = Router::new()
            .route("/aspsps", get(|| async { Json(json!({"aspsps":[{"name":"Test Bank","country":"PT","psu_types":["personal"],"maximum_consent_validity":86400}]})) }))
            .route(
                "/auth",
                post(move |Json(body): Json<Value>| async move {
                    assert_eq!(body["psu_type"], "personal");
                    assert_eq!(body["aspsp"], json!({"name":"Test Bank","country":"PT"}));
                    assert_eq!(body["access"]["transactions"], true);
                    let until = chrono::DateTime::parse_from_rfc3339(body["access"]["valid_until"].as_str().unwrap()).unwrap();
                    assert!((until.timestamp() - chrono::Utc::now().timestamp() - 86400).abs() < 5);
                    let redirect = body["redirect_url"].as_str().unwrap().to_owned();
                    let state = body["state"].as_str().unwrap().to_owned();
                    assert_eq!(state.len(), 64);
                    let parsed = checked_url(&redirect).unwrap();
                    let (upstream, host) = if let Some(preserve) = proxy_preserves_host {
                        assert_eq!(redirect, "https://means.example.ts.net:8443/callback");
                        (format!("http://127.0.0.1:{callback_port}/callback"), if preserve { "means.example.ts.net:8443".to_owned() } else { format!("127.0.0.1:{callback_port}") })
                    } else {
                        assert_eq!(parsed.host_str(), Some("127.0.0.1"));
                        (redirect.clone(), format!("127.0.0.1:{}", parsed.port().unwrap()))
                    };
                    tokio::spawn(async move {
                        let http = reqwest::Client::new();
                        let wrong_host = http.get(format!("{upstream}?state={state}&code=evil")).header("host", "evil.test").header("x-forwarded-host", &host).send().await.unwrap();
                        assert_eq!(wrong_host.status(), StatusCode::BAD_REQUEST);
                        let invalid = http.get(format!("{upstream}?state=wrong&code=evil")).header("host", &host).send().await.unwrap();
                        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
                        let valid = http.get(format!("{upstream}?state={state}&code=one-time-code")).header("host", &host).send().await.unwrap();
                        assert_eq!(valid.status(), StatusCode::OK);
                    });
                    Json(json!({"url":"https://auth.enablebanking.com/test"}))
                }),
            )
            .route(
                "/sessions",
                post(move |Json(body): Json<Value>| {
                    let calls = session_calls.clone();
                    async move {
                        assert_eq!(body, json!({"code":"one-time-code"}));
                        calls.fetch_add(1, Ordering::SeqCst);
                        Json(json!({"session_id":SESSION_ID,"access":{"valid_until":"2030-01-01T00:00:00Z"},"accounts":[{"uid":"account-id"}]}))
                    }
                }),
            )
            .layer(middleware::from_fn(verify_jwt));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let api = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = Client {
            http: reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap(),
            base: checked_url(&base).unwrap(),
            application_id: APP_ID.into(),
            key: EncodingKey::from_rsa_pem(PRIVATE).unwrap(),
        };
        let dir = std::env::temp_dir().join(format!("means-enable-auth-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ledger.db");
        let options = CallbackOptions { port: callback_port, no_open: true, redirect: proxy_preserves_host.map(|_| callback_redirect("https://means.example.ts.net:8443/callback").unwrap()) };
        tokio::time::timeout(Duration::from_secs(15), connect(path.clone(), client, "Test Bank".into(), "pt".into(), options)).await.unwrap().unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        {
            let db = Db::open(&path).unwrap();
            let saved = core::sessions(&db.conn()).unwrap();
            assert_eq!(saved.len(), 1);
            assert_eq!(saved[0].id, SESSION_ID);
            assert_eq!(saved[0].bank, "Test Bank");
            assert_eq!(saved[0].valid_until, "2030-01-01T00:00:00Z");
        }
        api.abort();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn full_history_pull_handles_empty_pages_dry_run_and_mid_fetch_failure() {
        const ACCOUNT: &str = "9b630e9a-86ec-4b40-b0e9-b5d1f65c0f2a";
        const CURSOR: &str = "https://evil.invalid/path?token=opaque&x=1";
        let fail = Arc::new(AtomicUsize::new(0));
        let mode = fail.clone();
        let app = Router::new()
            .route("/sessions/{id}", get(|| async { Json(json!({"status":"AUTHORIZED","access":{"valid_until":"2030-01-01T00:00:00Z"},"accounts_data":[{"uid":ACCOUNT,"identification_hash":"hash","identification_hashes":["hash","alias"]}]})) }))
            .route("/accounts/{id}/details", get(|| async { Json(json!({"uid":ACCOUNT,"currency":"XXX","name":"Multi-currency"})) }))
            .route("/accounts/{id}/transactions", get(move |axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>| {
                let mode = mode.clone();
                async move {
                    assert_eq!(query.get("strategy").map(String::as_str), Some("longest"));
                    assert!(!query.contains_key("date_from"));
                    assert!(!query.contains_key("date_to"));
                    let key = query.get("continuation_key").map(String::as_str);
                    if key == Some(CURSOR) && mode.load(Ordering::SeqCst) == 1 {
                        return (StatusCode::TOO_MANY_REQUESTS, "sensitive provider response").into_response();
                    }
                    let record = |id: &str, currency: &str| json!({"entry_reference":id,"status":"BOOK","booking_date":"2020-01-01","credit_debit_indicator":"DBIT","transaction_amount":{"amount":"3.20","currency":currency}});
                    match key {
                        None => { assert_eq!(query.len(), 1); Json(json!({"transactions":[],"continuation_key":CURSOR})).into_response() }
                        Some(CURSOR) => { assert_eq!(query.len(), 2); Json(json!({"transactions":[record("eur", "EUR")],"continuation_key":"final"})).into_response() }
                        Some("final") => Json(json!({"transactions":[record("usd", "USD")],"continuation_key":null})).into_response(),
                        _ => panic!("unexpected continuation"),
                    }
                }
            }))
            .layer(middleware::from_fn(verify_jwt));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = || Client {
            http: reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap(),
            base: checked_url(&base).unwrap(),
            application_id: APP_ID.into(),
            key: EncodingKey::from_rsa_pem(PRIVATE).unwrap(),
        };
        let dir = std::env::temp_dir().join(format!("means-enable-pull-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ledger.db");
        {
            let db = Db::open(&path).unwrap();
            core::save_session(&db.conn(), "Bank", "PT", &json!({"session_id":SESSION_ID,"access":{"valid_until":"2030-01-01T00:00:00Z"},"accounts":[{"uid":ACCOUNT}]})).unwrap();
        }
        pull(path.clone(), client(), None, true, None, PullScope::default()).await.unwrap();
        assert!(!dir.join("inbox").exists());
        {
            let db = Db::open(&path).unwrap();
            let count: i64 = db.conn().query_row("SELECT COUNT(*) FROM channel_connections", [], |r| r.get(0)).unwrap();
            assert_eq!(count, 0);
        }
        pull(path.clone(), client(), None, false, None, PullScope::default()).await.unwrap();
        let files = std::fs::read_dir(dir.join("inbox")).unwrap().map(|e| e.unwrap().path()).collect::<Vec<_>>();
        assert_eq!(files.len(), 2);
        for file in files {
            let parsed = core::parse(&std::fs::read(file).unwrap()).unwrap();
            assert_eq!(parsed.lines.len(), 1);
            assert_eq!(parsed.lines[0].date.unwrap().to_string(), "2020-01-01", "old bookings remain in the full history");
        }
        fail.store(1, Ordering::SeqCst);
        let failed_dir = dir.join("failed-pull");
        let error = pull(path, client(), None, false, Some(failed_dir.clone()), PullScope::default()).await.unwrap_err();
        assert!(error.to_string().contains("429"));
        assert!(!error.to_string().contains("sensitive provider response"));
        assert!(!error.to_string().contains("opaque"));
        assert!(!failed_dir.exists(), "a failed page must not publish the partial account");
        server.abort();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn pull_uses_primary_hashes_and_rejects_secondary_only_accounts() {
        const FIRST: &str = "11111111-1111-4111-8111-111111111111";
        const SECOND: &str = "22222222-2222-4222-8222-222222222222";
        const REPEATED: &str = "33333333-3333-4333-8333-333333333333";
        let mode = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(AtomicUsize::new(0));
        let session_mode = mode.clone();
        let transaction_requests = requests.clone();
        let app = Router::new()
            .route(
                "/sessions/{id}",
                get(move || {
                    let mode = session_mode.clone();
                    async move {
                        let accounts = if mode.load(Ordering::SeqCst) == 0 {
                            json!([
                                {"uid":FIRST,"identification_hash":"first","identification_hashes":["shared"]},
                                {"uid":SECOND,"identification_hash":"second","identification_hashes":["shared"]},
                                {"uid":REPEATED,"identification_hash":"first","identification_hashes":["other"]}
                            ])
                        } else {
                            json!([{"uid":FIRST,"identification_hashes":["secondary-only"]}])
                        };
                        Json(json!({"status":"AUTHORIZED","access":{"valid_until":"2030-01-01T00:00:00Z"},"accounts_data":accounts}))
                    }
                }),
            )
            .route("/accounts/{id}/details", get(|axum::extract::Path(id): axum::extract::Path<String>| async move { Json(json!({"uid":id,"currency":"EUR"})) }))
            .route(
                "/accounts/{id}/transactions",
                get(move || {
                    let requests = transaction_requests.clone();
                    async move {
                        requests.fetch_add(1, Ordering::SeqCst);
                        Json(json!({"transactions":[]}))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = || Client { http: reqwest::Client::new(), base: checked_url(&base).unwrap(), application_id: APP_ID.into(), key: EncodingKey::from_rsa_pem(PRIVATE).unwrap() };
        let dir = std::env::temp_dir().join(format!("means-enable-identities-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ledger.db");
        {
            let db = Db::open(&path).unwrap();
            core::save_session(&db.conn(), "Bank", "PT", &json!({"session_id":SESSION_ID,"access":{"valid_until":"2030-01-01T00:00:00Z"},"accounts":[{"uid":FIRST}]})).unwrap();
        }
        pull(path.clone(), client(), None, false, None, PullScope::default()).await.unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 2, "shared secondary hashes must not skip distinct accounts");
        let mut identities = std::fs::read_dir(dir.join("inbox")).unwrap().map(|entry| core::parse(&std::fs::read(entry.unwrap().path()).unwrap()).unwrap().account_ref).collect::<Vec<_>>();
        identities.sort();
        assert_eq!(identities, ["first:EUR", "second:EUR"]);
        mode.store(1, Ordering::SeqCst);
        let rejected = dir.join("rejected");
        let error = pull(path, client(), None, false, Some(rejected.clone()), PullScope::default()).await.unwrap_err();
        assert!(error.to_string().contains("no primary identification hash"));
        assert!(!rejected.exists());
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        server.abort();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn repeated_pagination_cursor_is_an_error() {
        let app = Router::new().route("/accounts/{id}/transactions", get(|| async { Json(json!({"transactions":[],"continuation_key":"repeat"})) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = Client { http: reqwest::Client::new(), base: checked_url(&base).unwrap(), application_id: APP_ID.into(), key: EncodingKey::from_rsa_pem(PRIVATE).unwrap() };
        let error = transactions(&client, SESSION_ID, None).await.unwrap_err();
        assert!(error.to_string().contains("repeated a continuation key"));
        server.abort();
    }
    #[test]
    fn booking_filter_is_inclusive_uses_booking_not_value_date_and_fails_closed() {
        let row = |date: &str, unit: &str| json!({"status":"BOOK","booking_date":date,"value_date":"2026-08-01","transaction_amount":{"currency":unit}});
        let from = means_core::parse_date("2026-08-30").unwrap();
        let saved = HashMap::from([("hash:EUR".into(), from)]);
        let rows = vec![row("2026-08-29", "EUR"), row("2026-08-30", "EUR"), row("2026-08-31", "EUR"), row("2020-01-01", "USD")];
        let result = filter_bookings(rows.clone(), "hash", None, &PullScope::default(), &saved).unwrap();
        assert_eq!(result.len(), 3, "one account cutoff must not truncate another currency");
        let scope = PullScope { booked_from: Some(means_core::parse_date("2026-08-31").unwrap()), ..Default::default() };
        assert_eq!(filter_bookings(rows, "hash", Some("EUR"), &scope, &saved).unwrap(), vec![row("2026-08-31", "EUR")]);
        for date in ["", "2026-02-30", "2026-08-30T12:00:00Z"] {
            assert!(filter_bookings(vec![row(date, "EUR")], "hash", None, &scope, &saved).is_err());
        }
        let pending = json!({"status":"PDNG","transaction_amount":{"currency":"EUR"}});
        assert!(filter_bookings(vec![pending], "hash", None, &scope, &saved).unwrap().is_empty());
    }

    #[tokio::test]
    async fn account_cutoff_is_sent_on_every_page_and_filters_before_publication() {
        const ACCOUNT: &str = "9b630e9a-86ec-4b40-b0e9-b5d1f65c0f2a";
        const OTHER: &str = "8b630e9a-86ec-4b40-b0e9-b5d1f65c0f2a";
        let seen = Arc::new(AtomicUsize::new(0));
        let requests = seen.clone();
        let app = Router::new()
            .route("/sessions/{id}", get(|| async { Json(json!({"status":"AUTHORIZED","access":{"valid_until":"2030-01-01T00:00:00Z"},"accounts_data":[{"uid":OTHER,"identification_hash":"other"},{"uid":ACCOUNT,"identification_hash":"hash"}]})) }))
            .route("/accounts/{id}/details", get(|axum::extract::Path(id): axum::extract::Path<String>| async move {
                assert_eq!(id, ACCOUNT, "unselected account must not be fetched");
                Json(json!({"uid":ACCOUNT,"currency":"EUR"}))
            }))
            .route("/accounts/{id}/transactions", get(move |axum::extract::Path(id): axum::extract::Path<String>, axum::extract::Query(query): axum::extract::Query<HashMap<String,String>>| {
                let requests = requests.clone();
                async move {
                    assert_eq!(id, ACCOUNT);
                    assert_eq!(query["date_from"], "2026-08-30");
                    assert_eq!(query["strategy"], "default");
                    requests.fetch_add(1, Ordering::SeqCst);
                    let row = |date: &str| json!({"entry_reference":date,"status":"BOOK","booking_date":date,"value_date":"2026-08-29","credit_debit_indicator":"DBIT","transaction_amount":{"amount":"3.20","currency":"EUR"}});
                    if query.contains_key("continuation_key") {
                        Json(json!({"transactions":[row("2026-08-30"),row("2026-08-31")]}))
                    } else {
                        Json(json!({"transactions":[row("2026-08-29")],"continuation_key":"next"}))
                    }
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = || Client { http: reqwest::Client::new(), base: checked_url(&base).unwrap(), application_id: APP_ID.into(), key: EncodingKey::from_rsa_pem(PRIVATE).unwrap() };
        let dir = std::env::temp_dir().join(format!("means-enable-cutoff-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ledger.db");
        {
            let db = Db::open(&path).unwrap();
            let mut c = db.conn();
            core::save_session(&c, "Bank", "PT", &json!({"session_id":SESSION_ID,"access":{"valid_until":"2030-01-01T00:00:00Z"},"accounts":[{"uid":ACCOUNT},{"uid":OTHER}]})).unwrap();
            means_core::connections::discover(
                &c,
                core::CHANNEL,
                SESSION_ID,
                &[means_core::connections::ConnectionAccount { provider_account_id: "hash:EUR".into(), currency: "EUR".into(), ..Default::default() }],
            )
            .unwrap();
            let id = means_core::connections::list(&c).unwrap()[0].id;
            means_core::connections::configure(&mut c, id, None, "2026-08-30").unwrap();
        }
        pull(path.clone(), client(), None, true, None, PullScope { account: Some(ACCOUNT.into()), ..Default::default() }).await.unwrap();
        assert!(!dir.join("inbox").exists());
        pull(path.clone(), client(), None, false, None, PullScope { account: Some("hash:EUR".into()), ..Default::default() }).await.unwrap();
        assert_eq!(seen.load(Ordering::SeqCst), 4);
        let files = std::fs::read_dir(dir.join("inbox")).unwrap().map(|f| f.unwrap().path()).collect::<Vec<_>>();
        assert_eq!(files.len(), 1);
        let parsed = core::parse(&std::fs::read(&files[0]).unwrap()).unwrap();
        assert_eq!(parsed.lines.len(), 2);
        assert_eq!(parsed.lines[0].date.unwrap().to_string(), "2026-08-30");
        assert_eq!(parsed.lines[1].date.unwrap().to_string(), "2026-08-31");
        let error = pull(path, client(), None, false, None, PullScope { account: Some("missing:EUR".into()), ..Default::default() }).await.unwrap_err();
        assert!(error.to_string().contains("not found"));
        assert_eq!(seen.load(Ordering::SeqCst), 4);
        server.abort();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
