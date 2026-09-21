//! Background bank operations for the local TUI. Secrets stay in the server environment.
use anyhow::{bail, Context, Result};
use means_core::{connections as core, Db};
use means_proto::v1 as pb;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Default)]
struct Live {
    active: Option<String>,
    authorization_url: String,
    banks: Vec<pb::BankChoice>,
    bank_job: String,
}
pub struct Manager {
    db: Arc<Db>,
    inbox: PathBuf,
    live: Mutex<Live>,
    pluggy_api: String,
    mercury_api: String,
    enable_api: String,
    wise_api: String,
    inter_api: String,
}
impl Manager {
    pub fn new(db: Arc<Db>, inbox: PathBuf) -> Result<Arc<Self>> {
        db.conn().execute(
            "UPDATE connection_jobs SET state='interrupted',message='Server stopped during this operation. Check Imports before retrying.',finished_at=?1 WHERE state='running'",
            [means_core::now_ts()],
        )?;
        Ok(Arc::new(Self {
            db,
            inbox,
            live: Mutex::new(Live::default()),
            pluggy_api: means_core::imports::pluggy::DEFAULT_API.into(),
            mercury_api: "https://api.mercury.com".into(),
            enable_api: "https://api.enablebanking.com".into(),
            wise_api: crate::wise::API.into(),
            inter_api: crate::inter_pj::API.into(),
        }))
    }
    pub fn list(&self) -> Result<pb::ListConnectionsResponse> {
        let c = self.db.conn();
        let live = self.live.lock().unwrap_or_else(|p| p.into_inner());
        let mut stmt = c.prepare("SELECT id,provider,operation,target,state,message,started_at,finished_at FROM connection_jobs ORDER BY started_at DESC,id DESC LIMIT 20")?;
        let jobs = stmt
            .query_map([], |r| {
                let id: String = r.get(0)?;
                Ok(pb::ConnectionJob {
                    authorization_url: if live.active.as_ref() == Some(&id) { live.authorization_url.clone() } else { String::new() },
                    banks: if live.bank_job == id { live.banks.clone() } else { vec![] },
                    id,
                    provider: r.get(1)?,
                    operation: r.get(2)?,
                    target: r.get(3)?,
                    state: r.get(4)?,
                    message: r.get(5)?,
                    started_at: r.get(6)?,
                    finished_at: r.get(7)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(pb::ListConnectionsResponse {
            providers: providers(),
            accounts: core::list(&c)?.into_iter().map(account_pb).collect(),
            jobs,
            consents: means_core::imports::enable_banking::sessions(&c)?.into_iter().map(|s| pb::BankConsent { id: s.id, bank: s.bank, country: s.country, valid_until: s.valid_until }).collect(),
        })
    }
    pub fn configure(&self, r: pb::ConfigureConnectionRequest) -> Result<pb::BankConnection> {
        // Same lock order as list/start. Mapping cannot change while a pull uses it.
        let mut c = self.db.conn();
        let live = self.live.lock().unwrap_or_else(|p| p.into_inner());
        if live.active.is_some() {
            bail!("Wait for the active connection operation before changing mappings");
        }
        Ok(account_pb(core::configure(&mut c, r.id, if r.account_id == 0 { None } else { Some(r.account_id) }, &r.booked_from)?))
    }
    pub fn start(self: &Arc<Self>, r: pb::StartConnectionJobRequest) -> Result<pb::ConnectionJob> {
        validate(&r)?;
        let providers = providers();
        let provider = providers.iter().find(|p| p.id == r.provider).context("unknown provider")?;
        if !provider.configured {
            bail!("Configure {} on the server, then restart it", provider.requirements);
        }
        let c = self.db.conn();
        let mut live = self.live.lock().unwrap_or_else(|p| p.into_inner());
        if live.active.is_some() {
            bail!("A connection operation is already running");
        }
        let target = if r.connection_id != 0 {
            let a = core::get(&c, r.connection_id)?;
            if provider_for(&a) != r.provider {
                bail!("connection does not belong to this provider");
            }
            if r.operation == "pull" && a.channel == "pluggy" {
                uuid::Uuid::parse_str(&a.item_id).context("Discover this Pluggy item before pulling")?;
                uuid::Uuid::parse_str(&a.provider_account_id).context("Invalid Pluggy account ID; discover this item again")?;
            }
            format!("account:{}", a.id)
        } else if !r.item_id.is_empty() {
            r.item_id.clone()
        } else {
            r.bank.clone()
        };
        let job = pb::ConnectionJob {
            id: means_core::new_uid(),
            provider: r.provider.clone(),
            operation: r.operation.clone(),
            target,
            state: "running".into(),
            message: "Starting…".into(),
            started_at: means_core::now_ts(),
            ..Default::default()
        };
        c.execute(
            "INSERT INTO connection_jobs(id,provider,operation,target,state,message,started_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            means_core::rusqlite::params![job.id, job.provider, job.operation, job.target, job.state, job.message, job.started_at],
        )?;
        live.active = Some(job.id.clone());
        live.authorization_url.clear();
        drop(live);
        drop(c);
        let this = self.clone();
        let id = job.id.clone();
        tokio::spawn(async move {
            let worker = this.clone();
            let worker_id = id.clone();
            // Catch a worker panic as a failed job and release the single-operation guard.
            let result = tokio::spawn(async move { worker.run(&worker_id, r).await }).await;
            let result = match result {
                Ok(r) => r,
                Err(_) => Err(anyhow::anyhow!("Bank operation stopped unexpectedly")),
            };
            let (state, message) = match result {
                Ok(m) => ("succeeded", m),
                Err(e) => ("failed", redacted(&format!("{e:#}"))),
            };
            let c = this.db.conn();
            if let Err(e) = c.execute("UPDATE connection_jobs SET state=?2,message=?3,finished_at=?4 WHERE id=?1", means_core::rusqlite::params![id, state, message, means_core::now_ts()]) {
                tracing::error!("could not save bank operation result: {e}");
            }
            let mut live = this.live.lock().unwrap_or_else(|p| p.into_inner());
            live.active = None;
            live.authorization_url.clear();
        });
        Ok(job)
    }
    fn discovered(&self, channel: &str, item: &str, accounts: Vec<core::ConnectionAccount>) -> Result<usize> {
        let n = accounts.len();
        let mut c = self.db.conn();
        let tx = c.transaction()?;
        core::discover(&tx, channel, item, &accounts)?;
        tx.commit()?;
        Ok(n)
    }
    async fn run(self: &Arc<Self>, job: &str, r: pb::StartConnectionJobRequest) -> Result<String> {
        let path = self.db.path().to_path_buf();
        match (r.provider.as_str(), r.operation.as_str()) {
            ("pluggy", "discover") => {
                let item = r.item_id;
                let api = self.pluggy_api.clone();
                let query_item = item.clone();
                let (status, accounts) = tokio::task::spawn_blocking(move || {
                    let transport = pluggy_transport()?;
                    Ok::<_, anyhow::Error>(means_core::imports::pluggy::discover(&transport, &means_core::imports::pluggy::Credentials::from_env()?, &api, &query_item)?)
                })
                .await??;
                let n = self.discovered("pluggy", &item, accounts)?;
                Ok(format!("Found {n} accounts. Item status: {status}. Map accounts and set booking cutoffs before pulling. MeuPluggy syncs at meu.pluggy.ai."))
            }
            ("mercury" | "mercury_credit", "discover") => {
                let accounts = crate::mercury::discover_connected(&self.mercury_api, r.provider == "mercury_credit").await?;
                let n = self.discovered("mercury", "", accounts)?;
                Ok(format!("Found {n} accounts. Select an account to map it, then pull."))
            }
            ("enable_banking", "banks") => {
                let banks = crate::enable_banking::banks_connected(&self.enable_api, &r.country).await?;
                let n = banks.len();
                let mut live = self.live.lock().unwrap_or_else(|p| p.into_inner());
                live.banks = banks.into_iter().map(|(name, country)| pb::BankChoice { name, country }).collect();
                live.bank_job = job.into();
                Ok(format!("Found {n} banks. Choose one in connection setup, then authorize."))
            }
            ("enable_banking", "authorize") => {
                let this = self.clone();
                let session = crate::enable_banking::authorize_connected(&self.enable_api, path, r.bank, r.country, r.callback_port as u16, move |url| {
                    this.live.lock().unwrap_or_else(|p| p.into_inner()).authorization_url = url;
                })
                .await?;
                let accounts = crate::enable_banking::discover_connected(&self.enable_api, &session.id).await?;
                let n = self.discovered("enable_banking", &session.id, accounts)?;
                Ok(format!("Connected {}. Consent valid until {}. Found {n} accounts; map them before pulling.", session.bank, session.valid_until))
            }
            ("enable_banking", "discover") => {
                let mut sessions = means_core::imports::enable_banking::sessions(&self.db.conn())?;
                sessions.reverse();
                let mut seen = std::collections::HashSet::new();
                let mut n = 0;
                let mut skipped = 0;
                let mut selected = 0;
                for session in sessions.into_iter().filter(|s| r.item_id.is_empty() || s.id == r.item_id) {
                    selected += 1;
                    if chrono::DateTime::parse_from_rfc3339(&session.valid_until)? <= chrono::Utc::now() {
                        if !r.item_id.is_empty() {
                            bail!("Consent expired; authorize the bank again");
                        }
                        skipped += 1;
                        continue;
                    }
                    let mut accounts = crate::enable_banking::discover_connected(&self.enable_api, &session.id).await?;
                    accounts.retain(|a| seen.insert(a.provider_account_id.clone()));
                    n += self.discovered("enable_banking", &session.id, accounts)?;
                }
                if selected == 0 || selected == skipped {
                    bail!("No active saved consent. Authorize a bank first");
                }
                Ok(format!("Found {n} accounts; skipped {skipped} expired consents. Map accounts before pulling."))
            }
            ("wise", "discover") => {
                let accounts = crate::wise::discover(&self.wise_api).await?;
                let n = self.discovered("wise", "", accounts)?;
                Ok(format!("Found {n} Wise balances; map them before pulling."))
            }
            ("inter_pj", "discover") => {
                let accounts = crate::inter_pj::discover(&self.inter_api).await?;
                let n = self.discovered("inter_pj", "", accounts)?;
                Ok(format!("Found {n} Inter accounts; map and set a booking cutoff before pulling."))
            }
            (_, "pull") => {
                let account = core::get(&self.db.conn(), r.connection_id)?;
                match r.provider.as_str() {
                    "pluggy" => {
                        let api = self.pluggy_api.clone();
                        let inbox = self.inbox.clone();
                        tokio::task::spawn_blocking(move || {
                            let db = Db::open(path)?;
                            let transport = pluggy_transport()?;
                            let opts = means_core::imports::pluggy::PullOptions {
                                base_url: api,
                                inbox,
                                item: Some(account.item_id),
                                account: Some(account.provider_account_id),
                                booked_from: means_core::parse_opt_date(&account.booked_from)?,
                                ..Default::default()
                            };
                            means_core::imports::pluggy::pull(&mut db.conn(), &transport, &means_core::imports::pluggy::Credentials::from_env()?, &opts)?;
                            Ok::<_, anyhow::Error>(())
                        })
                        .await??;
                    }
                    "mercury" | "mercury_credit" => {
                        crate::mercury::pull_connected(&self.mercury_api, r.provider == "mercury_credit", path, account.provider_account_id, self.inbox.clone()).await?;
                    }
                    "wise" => {
                        crate::wise::pull_connected(&self.wise_api, path, account.provider_account_id, self.inbox.clone()).await?;
                    }
                    "inter_pj" => {
                        crate::inter_pj::pull_connected(&self.inter_api, path, account.provider_account_id, self.inbox.clone()).await?;
                    }
                    "enable_banking" => {
                        crate::enable_banking::pull_connected(&self.enable_api, path, account.item_id, account.provider_account_id, self.inbox.clone()).await?;
                    }
                    _ => bail!("unknown provider"),
                }
                let db = self.db.clone();
                let inbox = self.inbox.clone();
                tokio::task::spawn_blocking(move || means_core::imports::inbox::scan(&mut db.conn(), &inbox)).await??;
                Ok("Pull finished. Open Imports for per-file results and pending account assignments; open Review for postings.".into())
            }
            _ => bail!("unsupported connection operation"),
        }
    }
}
fn pluggy_transport() -> Result<crate::PluggyHttp> {
    Ok(crate::PluggyHttp {
        client: reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(90)).user_agent(format!("means/{}", crate::VERSION)).build()?,
        rt: tokio::runtime::Builder::new_current_thread().enable_all().build()?,
    })
}
fn env_present(name: &str) -> bool {
    std::env::var(name).is_ok_and(|s| !s.trim().is_empty())
}
pub fn providers() -> Vec<pb::ConnectionProvider> {
    [
        ("pluggy", "Pluggy / MeuPluggy", vec!["PLUGGY_CLIENT_ID", "PLUGGY_CLIENT_SECRET"]),
        ("mercury", "Mercury checking / savings", vec!["MERCURY_TOKEN"]),
        ("mercury_credit", "Mercury IO credit", vec!["MERCURY_TOKEN"]),
        ("enable_banking", "Enable Banking", vec!["ENABLE_BANKING_APP_ID", "ENABLE_BANKING_KEY_FILE"]),
        ("wise", "Wise business", vec!["WISE_TOKEN"]),
        ("inter_pj", "Banco Inter PJ", vec!["INTER_CLIENT_ID", "INTER_CLIENT_SECRET", "INTER_CERT_FILE", "INTER_KEY_FILE", "INTER_ACCOUNT_NUMBER"]),
    ]
    .into_iter()
    .map(|(id, name, keys)| pb::ConnectionProvider { id: id.into(), name: name.into(), configured: keys.iter().all(|k| env_present(k)), requirements: keys.join(", ") })
    .collect()
}
fn redacted(message: &str) -> String {
    let mut text = message.to_string();
    for key in ["PLUGGY_CLIENT_ID", "PLUGGY_CLIENT_SECRET", "MERCURY_TOKEN", "ENABLE_BANKING_APP_ID", "ENABLE_BANKING_KEY_FILE"] {
        if let Ok(value) = std::env::var(key) {
            if !value.is_empty() {
                text = text.replace(&value, "[redacted]");
            }
        }
    }
    text.chars().filter(|c| !c.is_control() || *c == '\n').take(1200).collect()
}
fn validate(r: &pb::StartConnectionJobRequest) -> Result<()> {
    if !["pluggy", "mercury", "mercury_credit", "enable_banking", "wise", "inter_pj"].contains(&r.provider.as_str()) {
        bail!("unknown provider");
    }
    match r.operation.as_str() {
        "pull" if r.connection_id > 0 => {}
        "discover" => {
            if r.provider == "pluggy" {
                uuid::Uuid::parse_str(&r.item_id).context("Enter the Pluggy item UUID from MeuPluggy")?;
            } else if !r.item_id.is_empty() {
                uuid::Uuid::parse_str(&r.item_id).context("invalid consent ID")?;
            }
        }
        "banks" if r.provider == "enable_banking" => {}
        "authorize" if r.provider == "enable_banking" => {
            if r.bank.trim().is_empty() || r.country.len() != 2 || r.callback_port == 0 || r.callback_port > u16::MAX as u32 {
                bail!("Choose a bank, two-letter country and registered callback port");
            }
        }
        _ => bail!("unsupported operation or missing connection"),
    }
    Ok(())
}
pub fn provider_for(a: &core::ConnectionAccount) -> String {
    if a.channel == "mercury" && a.provider_type == "credit" {
        "mercury_credit".into()
    } else {
        a.channel.clone()
    }
}
pub fn account_pb(a: core::ConnectionAccount) -> pb::BankConnection {
    pb::BankConnection {
        id: a.id,
        channel: a.channel,
        item_id: a.item_id,
        provider_account_id: a.provider_account_id,
        provider_type: a.provider_type,
        name: a.name,
        currency: a.currency,
        account_id: a.account_id.unwrap_or(0),
        account_path: a.account_path,
        entity_id: a.entity_id.unwrap_or(0),
        last_pull_at: a.last_pull_at,
        booked_from: a.booked_from,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::State,
        http::{Method, StatusCode, Uri},
        response::{IntoResponse, Response},
        routing::any,
        Json, Router,
    };
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};
    const ITEM: &str = "11111111-1111-4111-8111-111111111111";
    const ACCOUNT: &str = "22222222-2222-4222-8222-222222222222";
    const MERCURY: &str = "33333333-3333-4333-8333-333333333333";
    const CREDIT: &str = "44444444-4444-4444-8444-444444444444";
    const SESSION: &str = "55555555-5555-4555-8555-555555555555";
    const UID: &str = "66666666-6666-4666-8666-666666666666";
    #[derive(Clone, Default)]
    struct Stub {
        seen: Arc<Mutex<Vec<String>>>,
        fail: Arc<AtomicBool>,
        hold: Arc<AtomicBool>,
    }
    async fn handler(State(s): State<Stub>, method: Method, uri: Uri) -> Response {
        s.seen.lock().unwrap().push(format!("{method} {uri}"));
        while s.hold.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        if s.fail.load(Ordering::SeqCst) {
            return (StatusCode::UNAUTHORIZED, "test-secret sensitive body").into_response();
        }
        let path = uri.path();
        let value = match path {
            "/auth" => json!({"apiKey":"test-api-key"}),
            "/accounts" => json!({"results":[{"id":ACCOUNT,"itemId":ITEM,"type":"BANK","name":"Euro bank","currencyCode":"EUR"}]}),
            "/v2/transactions" => {
                json!({"results":[{"id":"old","accountId":ACCOUNT,"amount":-7,"currencyCode":"EUR","date":"2026-08-01","description":"Old","status":"POSTED"},{"id":"new","accountId":ACCOUNT,"amount":-9,"currencyCode":"EUR","date":"2026-09-02","description":"New","status":"POSTED"}]})
            }
            "/api/v1/accounts" => json!({"accounts":[{"id":MERCURY,"type":"mercury","kind":"checking","name":"Mercury checking"}],"page":{}}),
            "/api/v1/credit" => json!({"accounts":[{"id":CREDIT,"name":"Mercury IO"}]}),
            "/api/v1/transactions" => {
                let id = if uri.query().unwrap_or_default().contains(CREDIT) { CREDIT } else { MERCURY };
                json!({"transactions":[{"id":"77777777-7777-4777-8777-777777777777","accountId":id,"amount":-3.21,"status":"sent","postedAt":"2026-09-02T00:00:00Z","counterpartyName":"Shop"}],"page":{}})
            }
            "/aspsps" => json!({"aspsps":[{"name":"Test Bank","country":"PT","psu_types":["personal"]}]}),
            _ if path == format!("/items/{ITEM}") => {
                assert_eq!(method, Method::GET, "MeuPluggy must never refresh");
                json!({"id":ITEM,"status":"UPDATED","connector":{"id":200}})
            }
            _ if path == format!("/sessions/{SESSION}") => json!({"status":"AUTHORIZED","access":{"valid_until":"2099-01-01T00:00:00Z"},"accounts_data":[{"uid":UID,"identification_hash":"stable"}]}),
            _ if path == format!("/accounts/{UID}/details") => json!({"uid":UID,"currency":"EUR","name":"Enable account","cash_account_type":"CACC"}),
            _ if path == format!("/accounts/{UID}/transactions") => {
                json!({"transactions":[{"entry_reference":"enable-ref","status":"BOOK","booking_date":"2026-09-03","credit_debit_indicator":"DBIT","transaction_amount":{"amount":"2.00","currency":"EUR"},"creditor":{"name":"Shop"}}]})
            }
            _ => return StatusCode::NOT_FOUND.into_response(),
        };
        Json(value).into_response()
    }
    struct Environment(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Environment {
        fn set(values: &[(&'static str, String)]) -> Self {
            Self(
                values
                    .iter()
                    .map(|(key, value)| {
                        let old = std::env::var_os(key);
                        std::env::set_var(key, value);
                        (*key, old)
                    })
                    .collect(),
            )
        }
    }
    impl Drop for Environment {
        fn drop(&mut self) {
            for (key, value) in &self.0 {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
    async fn finished(manager: &Manager, id: &str) -> pb::ConnectionJob {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let list = manager.list().unwrap();
                let job = list.jobs.iter().find(|j| j.id == id).unwrap();
                if job.state != "running" {
                    return job.clone();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap()
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn background_discovery_mapping_pull_errors_and_restart_use_the_real_pipeline() {
        let _env = Environment::set(&[
            ("PLUGGY_CLIENT_ID", "test-id".into()),
            ("PLUGGY_CLIENT_SECRET", "test-secret".into()),
            ("MERCURY_TOKEN", "test-secret".into()),
            ("ENABLE_BANKING_APP_ID", "c3779ad2-80b4-44c6-8899-f1bc89941166".into()),
            ("ENABLE_BANKING_KEY_FILE", format!("{}/tests/fixtures/enable-banking-test-key.pem", env!("CARGO_MANIFEST_DIR"))),
        ]);
        let stub = Stub::default();
        let router = Router::new().route("/{*path}", any(handler)).with_state(stub.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let dir = std::env::temp_dir().join(format!("means-connections-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(dir.join("ledger.db")).unwrap());
        let inbox = dir.join("custom-inbox");
        let (entity, eur, usd, card) = {
            let mut c = db.conn();
            let e = means_core::entities::create_entity(&mut c, "Personal", "person", "PT", "EUR").unwrap().id;
            means_core::rates::set_price(&c, "USD", "EUR", "2026-01-01".parse().unwrap(), "0.9".parse().unwrap(), "test").unwrap();
            let account = |name, kind, currency| means_core::accounts::ensure_account(&c, e, kind, &[name], "bank", currency).unwrap().id;
            (e, account("EUR", means_core::AccountType::Asset, "EUR"), account("USD", means_core::AccountType::Asset, "USD"), account("IO", means_core::AccountType::Liability, "USD"))
        };
        let mut manager = Manager::new(db.clone(), inbox.clone()).unwrap();
        let inner = Arc::get_mut(&mut manager).unwrap();
        inner.pluggy_api = url.clone();
        inner.mercury_api = url.clone();
        inner.enable_api = url.clone();
        stub.hold.store(true, Ordering::SeqCst);
        let request = pb::StartConnectionJobRequest { provider: "pluggy".into(), operation: "discover".into(), item_id: ITEM.into(), ..Default::default() };
        let worker = manager.clone();
        let r = request.clone();
        let job = tokio::task::spawn_blocking(move || worker.start(r)).await.unwrap().unwrap();
        assert!(manager.start(request).is_err());
        assert_eq!(manager.list().unwrap().jobs[0].state, "running");
        assert!(manager.configure(pb::ConfigureConnectionRequest { id: 1, account_id: eur, booked_from: String::new() }).is_err());
        assert_eq!(db.conn().query_row("SELECT COUNT(*) FROM journal_entries", [], |r| r.get::<_, i64>(0)).unwrap(), 0, "discovery must not post");
        stub.hold.store(false, Ordering::SeqCst);
        assert_eq!(finished(&manager, &job.id).await.state, "succeeded");
        let a = manager.list().unwrap().accounts[0].clone();
        assert!(a.last_pull_at.is_empty());
        assert!(!inbox.exists());
        manager.configure(pb::ConfigureConnectionRequest { id: a.id, account_id: eur, booked_from: "2026-09-01".into() }).unwrap();
        let job = manager.start(pb::StartConnectionJobRequest { provider: "pluggy".into(), operation: "pull".into(), connection_id: a.id, ..Default::default() }).unwrap();
        let result = finished(&manager, &job.id).await;
        assert_eq!(result.state, "succeeded", "{}", result.message);
        assert_eq!(db.conn().query_row("SELECT COUNT(*) FROM statement_lines", [], |r| r.get::<_, i64>(0)).unwrap(), 1, "saved booking cutoff excludes old history");
        assert!(!manager.list().unwrap().accounts[0].last_pull_at.is_empty());
        assert!(inbox.join("done").exists());
        assert!(!dir.join("inbox").exists());
        assert!(!stub.seen.lock().unwrap().iter().any(|s| s.starts_with("PATCH")));
        for (provider, destination) in [("mercury", usd), ("mercury_credit", card)] {
            let job = manager.start(pb::StartConnectionJobRequest { provider: provider.into(), operation: "discover".into(), ..Default::default() }).unwrap();
            assert_eq!(finished(&manager, &job.id).await.state, "succeeded");
            let a = manager.list().unwrap().accounts.into_iter().find(|a| crate::connections::provider_for(&core::get(&db.conn(), a.id).unwrap()) == provider).unwrap();
            manager.configure(pb::ConfigureConnectionRequest { id: a.id, account_id: destination, booked_from: String::new() }).unwrap();
            let job = manager.start(pb::StartConnectionJobRequest { provider: provider.into(), operation: "pull".into(), connection_id: a.id, ..Default::default() }).unwrap();
            let result = finished(&manager, &job.id).await;
            assert_eq!(result.state, "succeeded", "{}", result.message);
        }
        means_core::imports::enable_banking::save_session(&db.conn(), "Test Bank", "PT", &json!({"session_id":SESSION,"access":{"valid_until":"2099-01-01T00:00:00Z"},"accounts":[UID]})).unwrap();
        for operation in ["banks", "discover"] {
            let job = manager.start(pb::StartConnectionJobRequest { provider: "enable_banking".into(), operation: operation.into(), country: "PT".into(), ..Default::default() }).unwrap();
            let result = finished(&manager, &job.id).await;
            assert_eq!(result.state, "succeeded", "{}", result.message);
            if operation == "banks" {
                assert_eq!(result.banks[0].name, "Test Bank");
            }
        }
        let a = manager.list().unwrap().accounts.into_iter().find(|a| a.channel == "enable_banking").unwrap();
        manager.configure(pb::ConfigureConnectionRequest { id: a.id, account_id: eur, booked_from: String::new() }).unwrap();
        let job = manager.start(pb::StartConnectionJobRequest { provider: "enable_banking".into(), operation: "pull".into(), connection_id: a.id, ..Default::default() }).unwrap();
        let result = finished(&manager, &job.id).await;
        assert_eq!(result.state, "succeeded", "{}", result.message);
        assert_eq!(db.conn().query_row("SELECT COUNT(*) FROM journal_entries WHERE entity_id=?1 AND status='draft'", [entity], |r| r.get::<_, i64>(0)).unwrap(), 4);
        stub.fail.store(true, Ordering::SeqCst);
        let job = manager.start(pb::StartConnectionJobRequest { provider: "mercury".into(), operation: "discover".into(), ..Default::default() }).unwrap();
        let result = finished(&manager, &job.id).await;
        assert_eq!(result.state, "failed");
        assert!(!result.message.contains("test-secret") && !result.message.contains("sensitive body"));
        assert_eq!(redacted("test-secret and test-id"), "[redacted] and [redacted]");
        db.conn().execute("INSERT INTO connection_jobs(id,provider,operation,target,state,started_at) VALUES('interrupted','pluggy','pull','account:1','running','2099')", []).unwrap();
        let restarted = Manager::new(db.clone(), inbox).unwrap();
        assert_eq!(restarted.list().unwrap().jobs[0].state, "interrupted");
        server.abort();
        drop(restarted);
        drop(manager);
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
