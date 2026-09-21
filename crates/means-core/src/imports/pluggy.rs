//! The Pluggy channel: the Brazilian accounts pull themselves into the inbox.
//!
//! Two halves with a file between them. `pull` authenticates, asks each item for an update, lists
//! its accounts and fetches their transactions, and writes one canonical file per Pluggy account
//! into the inbox folder. `parse` reads such a file back as statement lines under the source
//! `pluggy_json`. Nothing here posts: the inbox, `run_import` and the rules do the rest.
//!
//! The network belongs to the caller. means-core opens no socket, so `pull` works over a
//! `Transport` the binary implements with its HTTP client and the tests implement with recorded
//! payloads. That trait, `Credentials::from_env` and the `channel_connections` table are the seam
//! the European counterpart (Enable Banking) reuses from its own module; nothing here is a plugin
//! framework for a channel that does not exist yet.
//!
//! Meu Pluggy is the free tier: it does not sync on its own (that needs production credentials),
//! there are no webhooks against a local engine, and its rate and item limits are undocumented.
//! A limit is reported as it arrives; the pull never works around one.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};
use rust_decimal::prelude::FromStr;
use rust_decimal::Decimal;
use serde_json::{json, Value};

use super::{ParseOutput, ParsedLine};
use crate::{now_ts, Error, Result};

/// The import source of a file this channel writes.
pub const SOURCE: &str = "pluggy_json";
/// The channel name in `channel_connections`.
pub const CHANNEL: &str = "pluggy";
pub const DEFAULT_API: &str = "https://api.pluggy.ai";

/// Pluggy settles a transaction under `POSTED`. A `PENDING` one may still change its id and its
/// amount before it settles, so it is not evidence and never becomes a statement line.
const SETTLED: &str = "POSTED";

/// A page carries 500 transactions by default. The stop is against a server that keeps answering
/// with a cursor: a silent truncation would lose money, so the pull says so instead.
const MAX_PAGES: usize = 1000;

/// How long an item may stay `UPDATING` before the pull gives up.
const UPDATE_ATTEMPTS: usize = 100;

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

/// The `client_id` and `client_secret` of a Pluggy development application.
#[derive(Clone)]
pub struct Credentials {
    pub client_id: String,
    pub client_secret: String,
}

impl std::fmt::Debug for Credentials {
    // Written by hand: a derived Debug would carry the secret into the first log line that prints it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials").field("client_id", &self.client_id).field("client_secret", &"***").finish()
    }
}

impl Credentials {
    /// From the environment, never from the ledger: a secret stored in the ledger would travel with
    /// the ledger file, its backups and its exports.
    pub fn from_env() -> Result<Credentials> {
        Ok(Credentials { client_id: env_var("PLUGGY_CLIENT_ID")?, client_secret: env_var("PLUGGY_CLIENT_SECRET")? })
    }
}

fn env_var(name: &str) -> Result<String> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Ok(v.trim().to_string()),
        _ => Err(Error::Invalid(format!("set {name}: the Pluggy Dashboard shows it on your development application (export {name}=...)"))),
    }
}

// ---------------------------------------------------------------------------
// The network seam
// ---------------------------------------------------------------------------

/// One HTTP exchange. `api_key` is empty on the call that asks for one, and goes out as the
/// `X-API-KEY` header on every other.
pub struct Request<'a> {
    pub method: &'a str,
    pub url: &'a str,
    pub api_key: &'a str,
    pub body: Option<String>,
}

pub struct Response {
    pub status: u16,
    pub body: String,
}

/// What the channel needs from the network, and all it needs.
pub trait Transport {
    fn send(&self, request: Request<'_>) -> Result<Response>;
}

fn path_of(url: &str) -> &str {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    after_scheme.find('/').map(|i| &after_scheme[i..]).unwrap_or(after_scheme)
}

/// Keep a credential out of a message that quotes what the server said.
fn redact(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        text.to_string()
    } else {
        text.replace(secret, "***")
    }
}

fn message_of(body: &str) -> String {
    let short: String = body.chars().take(300).collect();
    match serde_json::from_str::<Value>(body) {
        Ok(v) => match v.get("message").and_then(|m| m.as_str()) {
            Some(m) => m.to_string(),
            None => short,
        },
        Err(_) => short,
    }
}

fn call(transport: &dyn Transport, method: &str, url: &str, api_key: &str, body: Option<String>) -> Result<Value> {
    let r = transport.send(Request { method, url, api_key, body })?;
    if r.status == 429 {
        return Err(Error::Invalid(format!("pluggy answered HTTP 429 (too many requests) at {}: Meu Pluggy does not document its rate and item limits, so wait and run the pull again", path_of(url))));
    }
    if !(200..300).contains(&r.status) {
        return Err(Error::Invalid(format!("pluggy answered HTTP {} at {}: {}", r.status, path_of(url), redact(&message_of(&r.body), api_key))));
    }
    serde_json::from_str(&r.body).map_err(|e| Error::Parse(format!("pluggy answered {} with a body that is not JSON: {e}", path_of(url))))
}

/// `POST /auth` trades the two credentials for an API key valid two hours.
fn auth(transport: &dyn Transport, base: &str, cred: &Credentials) -> Result<String> {
    let url = format!("{base}/auth");
    let body = json!({"clientId": cred.client_id, "clientSecret": cred.client_secret}).to_string();
    let r = transport.send(Request { method: "POST", url: &url, api_key: "", body: Some(body) })?;
    if !(200..300).contains(&r.status) {
        // The body is dropped on purpose: a refusal is the one answer that may echo what was sent,
        // and no part of a secret may reach an error message or a log line.
        return Err(Error::Invalid(format!("pluggy refused the credentials (HTTP {}); check PLUGGY_CLIENT_ID and PLUGGY_CLIENT_SECRET", r.status)));
    }
    let v: Value = serde_json::from_str(&r.body).map_err(|e| Error::Parse(format!("pluggy answered /auth with a body that is not JSON: {e}")))?;
    let key = str_field(&v, "apiKey");
    if key.is_empty() {
        return Err(Error::Parse("pluggy answered /auth without an apiKey".into()));
    }
    Ok(key)
}

fn get_item(transport: &dyn Transport, base: &str, key: &str, item_id: &str) -> Result<Value> {
    let url = format!("{base}/items/{item_id}");
    call(transport, "GET", &url, key, None)
}

/// Ask a connector that supports refresh for fresh data.
fn request_update(transport: &dyn Transport, base: &str, key: &str, item_id: &str) -> Result<()> {
    let url = format!("{base}/items/{item_id}");
    call(transport, "PATCH", &url, key, Some("{}".into()))?;
    Ok(())
}

/// Wait for `UPDATED`. `UPDATING` is the only state worth waiting on: the other three need a person
/// at meu.pluggy.ai, so they are reported as they are.
fn wait_until_updated(transport: &dyn Transport, base: &str, key: &str, item_id: &str, interval: Duration, initial: Option<Value>) -> Result<String> {
    let mut initial = initial;
    for _ in 0..UPDATE_ATTEMPTS {
        let item = match initial.take() {
            Some(item) => item,
            None => get_item(transport, base, key, item_id)?,
        };
        let status = str_field(&item, "status").to_ascii_uppercase();
        match status.as_str() {
            "UPDATED" => return Ok(status),
            "UPDATING" => std::thread::sleep(interval),
            "LOGIN_ERROR" | "WAITING_USER_INPUT" | "OUTDATED" => {
                return Err(Error::Invalid(format!("pluggy item {item_id} is {status}: open meu.pluggy.ai and connect that bank again, then run the pull")))
            }
            other => return Err(Error::Invalid(format!("pluggy item {item_id} is {other}, which is not a state this pull knows"))),
        }
    }
    Err(Error::Invalid(format!("pluggy item {item_id} was still UPDATING after {UPDATE_ATTEMPTS} reads; run the pull again later")))
}

fn results_of(page: &Value) -> Vec<Value> {
    page.get("results").and_then(|r| r.as_array()).cloned().unwrap_or_default()
}

fn list_accounts(transport: &dyn Transport, base: &str, key: &str, item_id: &str) -> Result<Vec<Value>> {
    let url = format!("{base}/accounts?itemId={item_id}");
    Ok(results_of(&call(transport, "GET", &url, key, None)?))
}

fn parse_url(value: &str) -> Result<url::Url> {
    url::Url::parse(value).map_err(|_| Error::Invalid("invalid Pluggy URL".into()))
}

fn transactions_url(base: &str, account_id: &str, created_at_from: Option<&str>) -> Result<String> {
    let mut url = parse_url(&format!("{base}/v2/transactions"))?;
    url.query_pairs_mut().append_pair("accountId", account_id);
    if let Some(c) = created_at_from {
        url.query_pairs_mut().append_pair("createdAtFrom", c);
    }
    Ok(url.into())
}

/// URLs must retain the trusted origin; opaque cursors are encoded as one query value.
fn next_url(base: &str, page: &Value, account_id: &str, created_at_from: Option<&str>) -> Result<Option<String>> {
    let Some(next) = page.get("next").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) else { return Ok(None) };
    let trusted = parse_url(base)?;
    let url = if next.starts_with(['/', '\\']) || next.contains("://") || url::Url::parse(next).is_ok() {
        trusted.join(next).map_err(|_| Error::Invalid("invalid Pluggy pagination URL".into()))?
    } else {
        let mut url = parse_url(&transactions_url(base, account_id, created_at_from)?)?;
        url.query_pairs_mut().append_pair("after", next);
        url
    };
    if url.origin() != trusted.origin() || !url.username().is_empty() || url.password().is_some() {
        return Err(Error::Invalid("Pluggy pagination must stay on the configured API origin".into()));
    }
    Ok(Some(url.into()))
}

fn fetch_transactions(transport: &dyn Transport, base: &str, key: &str, account_id: &str, created_at_from: Option<&str>) -> Result<(Vec<Value>, usize)> {
    let mut url = transactions_url(base, account_id, created_at_from)?;
    let mut all = Vec::new();
    let mut pages = 0usize;
    loop {
        let page = call(transport, "GET", &url, key, None)?;
        all.extend(results_of(&page));
        pages += 1;
        match next_url(base, &page, account_id, created_at_from)? {
            Some(u) if pages < MAX_PAGES => url = u,
            Some(_) => return Err(Error::Invalid(format!("pluggy still offered another page of transactions after {MAX_PAGES} of them; narrow the window with --since"))),
            None => break,
        }
    }
    Ok((all, pages))
}

// ---------------------------------------------------------------------------
// The pull
// ---------------------------------------------------------------------------

pub struct PullOptions {
    pub base_url: String,
    pub inbox: PathBuf,
    pub item: Option<String>,
    /// Sent as `createdAtFrom`: when Pluggy recorded the transaction, not when the bank booked it.
    pub since: Option<NaiveDate>,
    /// Inclusive bank booking date; independent of Pluggy's recorded-at cursor.
    pub booked_from: Option<NaiveDate>,
    /// Only this provider account UUID, not a means ledger account ID.
    pub account: Option<String>,
    pub dry_run: bool,
    pub poll_interval: Duration,
}

impl Default for PullOptions {
    fn default() -> PullOptions {
        PullOptions { base_url: DEFAULT_API.into(), inbox: PathBuf::from("inbox"), item: None, since: None, booked_from: None, account: None, dry_run: false, poll_interval: Duration::from_secs(3) }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PulledItem {
    pub id: String,
    pub status: String,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PulledAccount {
    pub item_id: String,
    pub provider_account_id: String,
    pub name: String,
    /// BANK or CREDIT, as Pluggy types the account.
    pub kind: String,
    pub currency: String,
    pub transactions: usize,
    pub pages: usize,
    pub created_at_from: String,
    pub booked_from: String,
    pub excluded_before_booking_date: usize,
    /// Empty on a dry run.
    pub file: String,
    /// The means account this Pluggy account is already known to belong to, if any.
    pub account_id: Option<i64>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PullReport {
    pub items: Vec<PulledItem>,
    pub accounts: Vec<PulledAccount>,
    pub dry_run: bool,
}

/// The two credentials go to this host, so the value is checked before they leave: an absolute
/// `https` URL, or plain `http` only to a loopback host (the stub server of a test, a local proxy).
/// Anything else would carry a secret to an undeclared host, in cleartext or not at all.
fn checked_base(url: &str) -> Result<String> {
    let base = url.trim().trim_end_matches('/');
    let flag = "--api";
    let Some((scheme, rest)) = base.split_once("://") else {
        return Err(Error::Invalid(format!("{flag} must be an absolute http or https URL, not {base:?}")));
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = match authority.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => h,
        _ => authority,
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        return Err(Error::Invalid(format!("{flag} {base:?} names no host")));
    }
    // The address, not the text it is written in: "127.0.0.1.tunnel.example" begins with 127. and is
    // an ordinary DNS name that resolves wherever its owner publishes it.
    let loopback = host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false);
    match scheme.to_ascii_lowercase().as_str() {
        "https" => Ok(base.to_string()),
        "http" if loopback => Ok(base.to_string()),
        "http" => Err(Error::Invalid(format!("{flag} {base:?} is plain http to {host}: the credentials would travel in the clear. Use https, or http only to a loopback host"))),
        other => Err(Error::Invalid(format!("{flag} must be an absolute http or https URL, not a {other:?} one"))),
    }
}

/// Inspect an item and its accounts without refreshing or downloading transactions.
pub fn discover(transport: &dyn Transport, cred: &Credentials, base: &str, item: &str) -> Result<(String, Vec<crate::connections::ConnectionAccount>)> {
    uuid::Uuid::parse_str(item).map_err(|_| Error::Invalid("Pluggy item must be a UUID".into()))?;
    let base = checked_base(base)?;
    let key = auth(transport, &base, cred)?;
    let status = str_field(&get_item(transport, &base, &key, item)?, "status");
    let accounts = list_accounts(transport, &base, &key, item)?
        .into_iter()
        .map(|a| {
            let id = str_field(&a, "id");
            uuid::Uuid::parse_str(&id).map_err(|_| Error::Parse("Pluggy account must be a UUID".into()))?;
            Ok(crate::connections::ConnectionAccount {
                channel: CHANNEL.into(),
                item_id: item.into(),
                provider_account_id: id,
                provider_type: str_field(&a, "type"),
                name: str_field(&a, "name"),
                currency: str_field(&a, "currencyCode").to_uppercase(),
                ..Default::default()
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((status, accounts))
}

/// Authenticate, refresh supported items, and write one canonical file per Pluggy account into the inbox.
/// A dry run asks for nothing to change: it neither refreshes an item nor writes a file or a cursor.
pub fn pull(conn: &mut Connection, transport: &dyn Transport, cred: &Credentials, opts: &PullOptions) -> Result<PullReport> {
    let base = checked_base(&opts.base_url)?;
    let key = auth(transport, &base, cred)?;
    let items = match opts.item.as_deref().map(str::trim).filter(|i| !i.is_empty()) {
        Some(i) => vec![i.to_string()],
        None => known_items(conn)?,
    };
    if items.is_empty() {
        return Err(Error::Invalid("no Pluggy item is known yet: connect the bank at meu.pluggy.ai, then run `means pluggy pull --item <item id>` once; the ledger remembers it".into()));
    }
    // The cursor the next pull sends is the moment this one started. A transaction Pluggy records
    // while this pull runs is then fetched again next time, and the dedupe by `reference` drops it;
    // taking the end of the pull instead would lose it.
    let started = now_ts();
    let mut report = PullReport { dry_run: opts.dry_run, ..Default::default() };
    for item_id in items {
        let item = get_item(transport, &base, &key, &item_id)?;
        let status = if opts.dry_run {
            str_field(&item, "status").to_ascii_uppercase()
        } else {
            // MeuPluggy rejects PATCH; freshness comes from its own bank syncing.
            let initial = if item["connector"]["id"].as_u64() == Some(200) {
                Some(item)
            } else {
                request_update(transport, &base, &key, &item_id)?;
                None
            };
            wait_until_updated(transport, &base, &key, &item_id, opts.poll_interval, initial)?
        };
        report.items.push(PulledItem { id: item_id.clone(), status });
        for account in list_accounts(transport, &base, &key, &item_id)? {
            let provider_account_id = str_field(&account, "id");
            if provider_account_id.is_empty() {
                return Err(Error::Parse(format!("pluggy listed an account of item {item_id} without an id")));
            }
            if opts.account.as_ref().is_some_and(|id| id != &provider_account_id) {
                continue;
            }
            let saved: String =
                conn.query_row("SELECT booked_from FROM channel_connections WHERE channel='pluggy' AND provider_account_id=?1", [&provider_account_id], |r| r.get(0)).optional()?.unwrap_or_default();
            let booked_from = opts.booked_from.or(crate::parse_opt_date(&saved)?);
            let created_at_from = match opts.since {
                Some(d) => Some(format!("{d}T00:00:00.000Z")),
                None if booked_from.is_some() => None,
                None => cursor_for(conn, &provider_account_id)?,
            };
            let (mut transactions, pages) = fetch_transactions(transport, &base, &key, &provider_account_id, created_at_from.as_deref())?;
            let fetched = transactions.len();
            if let Some(from) = booked_from {
                // A missing date cannot safely be classified as before or after the cutoff.
                for transaction in &transactions {
                    if date_field(transaction, "date").is_none() {
                        return Err(Error::Parse(format!("Pluggy transaction {} has no valid booking date; cannot apply --booked-from", str_field(transaction, "id"))));
                    }
                }
                transactions.retain(|t| date_field(t, "date").is_some_and(|d| d >= from));
            }
            let mut pulled = PulledAccount {
                item_id: item_id.clone(),
                provider_account_id: provider_account_id.clone(),
                name: str_field(&account, "name"),
                kind: str_field(&account, "type").to_ascii_uppercase(),
                currency: str_field(&account, "currencyCode").to_ascii_uppercase(),
                transactions: transactions.len(),
                pages,
                created_at_from: created_at_from.clone().unwrap_or_default(),
                booked_from: booked_from.map(|d| d.to_string()).unwrap_or_default(),
                excluded_before_booking_date: fetched - transactions.len(),
                ..Default::default()
            };
            if !opts.dry_run {
                let content = canonical_file(&item_id, &account, &transactions, &started, created_at_from.as_deref(), booked_from)?;
                pulled.file = write_to_inbox(&opts.inbox, &provider_account_id, &started, &content)?;
                // A date-limited export must not consume the unrestricted stream's cursor.
                let cursor = if booked_from.is_some() { cursor_for(conn, &provider_account_id)?.unwrap_or_default() } else { started.clone() };
                record(conn, &item_id, &account, &cursor, &started)?;
            }
            pulled.account_id = account_for(conn, &provider_account_id)?;
            report.accounts.push(pulled);
        }
    }
    if opts.account.is_some() && report.accounts.is_empty() {
        return Err(Error::NotFound("the selected Pluggy account was not found in the selected items".into()));
    }
    Ok(report)
}

/// What the pull leaves in the inbox: the Pluggy account and its transactions verbatim, and what
/// the fetch asked for. `pulledAt` makes two pulls of one window two different files, so the second
/// is imported and its repeated lines are recorded as duplicates instead of the file being refused
/// whole by its checksum.
fn canonical_file(item_id: &str, account: &Value, transactions: &[Value], pulled_at: &str, created_at_from: Option<&str>, booked_from: Option<NaiveDate>) -> Result<Vec<u8>> {
    let file = json!({
        "channel": CHANNEL,
        "pulledAt": pulled_at,
        "itemId": item_id,
        "createdAtFrom": created_at_from,
        "bookedFrom": booked_from,
        "account": account,
        "transactions": transactions,
    });
    Ok(serde_json::to_vec_pretty(&file)?)
}

/// `pluggy-<when>-<pluggy account id>.json`: the timestamp carries no dash, so the account id is
/// everything after the second one, however the provider writes it. `nth` above 1 tells two pulls
/// of the same millisecond apart.
fn file_name(provider_account_id: &str, pulled_at: &str, nth: usize) -> String {
    let when: String = pulled_at.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    let when = if nth > 1 { format!("{when}_{nth}") } else { when };
    format!("pluggy-{when}-{provider_account_id}.json")
}

fn provider_account_of(filename: &str) -> Option<String> {
    let stem = filename.strip_prefix("pluggy-")?.strip_suffix(".json")?;
    let (_when, id) = stem.split_once('-')?;
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

fn write_to_inbox(dir: &Path, provider_account_id: &str, pulled_at: &str, content: &[u8]) -> Result<String> {
    std::fs::create_dir_all(dir).map_err(|e| Error::Invalid(format!("create {}: {e}", dir.display())))?;
    let mut nth = 1;
    let mut name = file_name(provider_account_id, pulled_at, nth);
    while dir.join(&name).exists() {
        nth += 1;
        name = file_name(provider_account_id, pulled_at, nth);
    }
    let target = dir.join(&name);
    // The inbox watcher may scan the folder while this is written: write beside the name and
    // rename, so a whole file appears at once (scan skips names that start with a dot).
    let part = dir.join(format!(".{name}.part"));
    std::fs::write(&part, content).map_err(|e| Error::Invalid(format!("write {}: {e}", part.display())))?;
    std::fs::rename(&part, &target).map_err(|e| Error::Invalid(format!("move {} to {}: {e}", part.display(), target.display())))?;
    Ok(name)
}

// ---------------------------------------------------------------------------
// The connection state: which Pluggy account is which means account, and where the pull reached
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ChannelConnection {
    pub id: i64,
    pub item_id: String,
    pub provider_account_id: String,
    pub provider_type: String,
    pub name: String,
    pub currency: String,
    pub account_id: Option<i64>,
    pub cursor: String,
    pub last_pull_at: String,
}

const CONNECTION_SELECT: &str = "SELECT id, item_id, provider_account_id, provider_type, name, currency, account_id, cursor, last_pull_at FROM channel_connections WHERE channel = 'pluggy'";

fn row_to_connection(r: &rusqlite::Row<'_>) -> rusqlite::Result<ChannelConnection> {
    Ok(ChannelConnection {
        id: r.get(0)?,
        item_id: r.get(1)?,
        provider_account_id: r.get(2)?,
        provider_type: r.get(3)?,
        name: r.get(4)?,
        currency: r.get(5)?,
        account_id: r.get(6)?,
        cursor: r.get(7)?,
        last_pull_at: r.get(8)?,
    })
}

pub fn connections(conn: &Connection) -> Result<Vec<ChannelConnection>> {
    let sql = format!("{CONNECTION_SELECT} ORDER BY item_id, provider_account_id");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_connection)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

/// The items this ledger has pulled before: a bare `means pluggy pull` fetches all of them.
/// Pluggy publishes no endpoint that lists the items of an application, so the ledger is the list.
pub fn known_items(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT item_id FROM channel_connections WHERE channel = 'pluggy' AND item_id <> '' ORDER BY item_id")?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn cursor_for(conn: &Connection, provider_account_id: &str) -> Result<Option<String>> {
    let cursor: Option<String> = conn.query_row("SELECT cursor FROM channel_connections WHERE channel = 'pluggy' AND provider_account_id = ?1", [provider_account_id], |r| r.get(0)).optional()?;
    Ok(cursor.filter(|c| !c.is_empty()))
}

fn account_for(conn: &Connection, provider_account_id: &str) -> Result<Option<i64>> {
    let id: Option<Option<i64>> = conn.query_row("SELECT account_id FROM channel_connections WHERE channel = 'pluggy' AND provider_account_id = ?1", [provider_account_id], |r| r.get(0)).optional()?;
    Ok(id.flatten())
}

/// Where the pull reached for one Pluggy account. The cursor and the moment of the pull are one
/// value, so the two columns take it twice. `account_id` is left alone: only a finished import says
/// which means account a Pluggy account belongs to.
fn record(conn: &Connection, item_id: &str, account: &Value, cursor: &str, pulled_at: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO channel_connections (channel, item_id, provider_account_id, provider_type, name, currency, cursor, last_pull_at, created_at)
         VALUES ('pluggy', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT (channel, provider_account_id) DO UPDATE SET item_id = excluded.item_id, provider_type = excluded.provider_type, name = excluded.name,
           currency = excluded.currency, cursor = excluded.cursor, last_pull_at = excluded.last_pull_at",
        params![
            item_id,
            str_field(account, "id"),
            str_field(account, "type").to_ascii_uppercase(),
            str_field(account, "name"),
            str_field(account, "currencyCode").to_ascii_uppercase(),
            cursor,
            pulled_at,
            now_ts()
        ],
    )?;
    Ok(())
}

/// The means account a Pluggy file belongs to, or None. A Pluggy account nobody has placed yet is
/// not guessed: its file waits as a pending import, the way an unrecognised bank file does.
pub fn account_for_file(conn: &Connection, filename: &str) -> Result<Option<i64>> {
    match provider_account_of(filename) {
        Some(id) => account_for(conn, &id),
        None => Ok(None),
    }
}

/// Remember that this Pluggy account's files belong to that means account. Called once the import
/// of such a file has finished, in place of the filename-shaped profile the other sources learn.
pub fn learn(conn: &Connection, filename: &str, account_id: i64) -> Result<()> {
    let Some(provider_account_id) = provider_account_of(filename) else { return Ok(()) };
    conn.execute(
        "INSERT INTO channel_connections (channel, item_id, provider_account_id, account_id, created_at) VALUES ('pluggy', '', ?1, ?2, ?3)
         ON CONFLICT (channel, provider_account_id) DO UPDATE SET account_id = excluded.account_id",
        params![provider_account_id, account_id, now_ts()],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The parser: a canonical file back into statement lines
// ---------------------------------------------------------------------------

fn str_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or_default().trim().to_string()
}

fn decimal_field(v: &Value, key: &str) -> Option<Decimal> {
    match v.get(key)? {
        // The workspace enables serde_json/arbitrary_precision: JSON numbers retain their
        // decimal text through the pull and inbox, without an intermediate f64 conversion.
        Value::Number(n) => Decimal::from_str(&n.to_string()).ok(),
        Value::String(s) => Decimal::from_str(s.trim()).ok(),
        _ => None,
    }
}

/// Pluggy dates are ISO 8601 timestamps ("2026-09-15T00:00:00.000Z"). The day is taken as Pluggy
/// wrote it, with no timezone shift: the bank's own booking day is what a statement says.
fn date_field(v: &Value, key: &str) -> Option<NaiveDate> {
    let s = v.get(key)?.as_str()?;
    NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok()
}

fn description_with_counterparty(tx: &Value, amount: Option<Decimal>) -> String {
    let mut description = str_field(tx, "description");
    if description.is_empty() {
        description = str_field(tx, "descriptionRaw");
    }
    let party = match amount {
        Some(a) if a > Decimal::ZERO => "payer",
        Some(a) if a < Decimal::ZERO => "receiver",
        _ => return description,
    };
    let participant = &tx["paymentData"][party];
    let name = participant.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).unwrap_or_else(|| str_field(participant, "name"));
    if !name.is_empty() && !description.to_lowercase().contains(&name.to_lowercase()) {
        if !description.is_empty() {
            description.push_str(" — ");
        }
        description.push_str(&name);
    }
    description
}

pub fn is_pluggy_file(content: &[u8]) -> bool {
    match serde_json::from_slice::<Value>(content) {
        Ok(v) => str_field(&v, "channel") == CHANNEL,
        Err(_) => false,
    }
}

pub fn parse(content: &[u8]) -> Result<ParseOutput> {
    let file: Value = serde_json::from_slice(content).map_err(|e| Error::Parse(format!("not a Pluggy file: {e}")))?;
    if str_field(&file, "channel") != CHANNEL {
        return Err(Error::Parse("not a Pluggy file: the channel is not \"pluggy\"".into()));
    }
    let account = file.get("account").cloned().unwrap_or(Value::Null);
    let kind = str_field(&account, "type").to_ascii_uppercase();
    let currency = str_field(&account, "currencyCode").to_ascii_uppercase();
    let mut out = ParseOutput { detected_source: SOURCE.into(), currency: currency.clone(), account_ref: str_field(&account, "id"), ..Default::default() };
    // The sign follows the account type. On a BANK account Pluggy signs an amount the way the bank
    // does, and a debit is already negative. On a CREDIT account a new charge is positive and a
    // payment negative, which is the other way round from "positive is money in".
    let invert = kind == "CREDIT";
    if kind != "BANK" && kind != "CREDIT" {
        out.warnings.push(format!("pluggy account type {kind:?} is neither BANK nor CREDIT: the amounts are taken as they are"));
    }
    for tx in file.get("transactions").and_then(|t| t.as_array()).cloned().unwrap_or_default() {
        let status = str_field(&tx, "status").to_ascii_uppercase();
        if !status.is_empty() && status != SETTLED {
            out.skipped_records += 1;
            continue;
        }
        let amount = decimal_field(&tx, "amount").map(|a| if invert { -a } else { a });
        let description = description_with_counterparty(&tx, amount);
        let line_currency = str_field(&tx, "currencyCode").to_ascii_uppercase();
        let mut line = ParsedLine {
            date: date_field(&tx, "date"),
            amount,
            currency: if line_currency.is_empty() { currency.clone() } else { line_currency },
            description,
            reference: str_field(&tx, "id"),
            balance_after: decimal_field(&tx, "balance").map(|b| if invert { -b } else { b }),
            raw: tx.clone(),
            ..Default::default()
        };
        if line.date.is_none() || line.amount.is_none() {
            line.skip = Some("a Pluggy transaction without a date or an amount".into());
        }
        out.lines.push(line);
    }
    if out.skipped_records > 0 {
        out.warnings.push(format!("{} pluggy transactions are not settled yet and were left out", out.skipped_records));
    }
    let mut dated: Vec<&ParsedLine> = out.lines.iter().filter(|l| l.skip.is_none() && l.date.is_some()).collect();
    dated.sort_by_key(|l| l.date);
    if let Some(last) = dated.iter().rev().find(|l| l.balance_after.is_some()) {
        out.closing_balance = last.balance_after;
        out.closing_date = last.date;
    }
    if let (Some(first), Some(last)) = (dated.first(), dated.last()) {
        out.period_from = first.date;
        out.period_to = last.date;
    }
    Ok(out)
}
