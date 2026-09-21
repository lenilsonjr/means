//! Banco Inter PJ current-account statements: read-only OAuth client credentials over mTLS.
use crate::statement_http as http;
use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use clap::Subcommand;
use means_core::{connections::ConnectionAccount, imports::bank_api, Db};
use serde_json::{json, Value};
use std::path::PathBuf;
pub const API: &str = "https://cdpj.partners.bancointer.com.br";
#[derive(Subcommand)]
pub enum Command {
    /// Verify credentials and list the configured BRL current account
    Accounts {
        #[arg(long,default_value=API)]
        api: String,
    },
    /// Fetch enriched BRL statements; first pull requires an explicit booking start
    Pull {
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
    client_id: String,
    secret: String,
    account: String,
}
impl Client {
    fn env(api: &str) -> Result<Self> {
        let cert = std::fs::read(std::env::var("INTER_CERT_FILE").context("set INTER_CERT_FILE")?).context("read Inter certificate file")?;
        let key = std::fs::read(std::env::var("INTER_KEY_FILE").context("set INTER_KEY_FILE")?).context("read Inter private key file")?;
        let pem = [cert, vec![b'\n'], key].concat();
        let identity = reqwest::Identity::from_pem(&pem).context("invalid Inter PEM certificate/private key")?;
        Self::new(
            api,
            std::env::var("INTER_CLIENT_ID").context("set INTER_CLIENT_ID")?,
            std::env::var("INTER_CLIENT_SECRET").context("set INTER_CLIENT_SECRET")?,
            std::env::var("INTER_ACCOUNT_NUMBER").context("set INTER_ACCOUNT_NUMBER (digits including check digit)")?,
            Some(identity),
        )
    }
    fn new(api: &str, client_id: String, secret: String, account: String, identity: Option<reqwest::Identity>) -> Result<Self> {
        let base = http::origin(api)?;
        if base.scheme() == "https" && identity.is_none() {
            bail!("Inter requires a client certificate")
        }
        if client_id.trim().is_empty() || secret.trim().is_empty() {
            bail!("Inter client credentials must not be empty")
        }
        if account.is_empty() || account.len() > 20 || !account.bytes().all(|b| b.is_ascii_digit()) {
            bail!("Inter account number must contain only digits, including its check digit")
        }
        let builder = http::builder();
        let builder = if let Some(identity) = identity { builder.identity(identity) } else { builder };
        Ok(Self { http: builder.build()?, base, client_id, secret, account })
    }
    async fn token(&self) -> Result<String> {
        let result = http::response(
            self.http.post(self.base.join("/oauth/v2/token")?).form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.secret.as_str()),
                ("grant_type", "client_credentials"),
                ("scope", "extrato.read"),
            ]),
            "Inter",
        )
        .await?;
        result["access_token"].as_str().filter(|s| !s.is_empty()).map(String::from).context("Inter returned no access token")
    }
    async fn get(&self, path: &str, query: &[(&str, String)], token: &str) -> Result<Value> {
        http::response(self.http.get(self.base.join(path)?).query(query).header("x-conta-corrente", &self.account).bearer_auth(token), "Inter").await
    }
    fn account(&self) -> Value {
        json!({"id":self.account,"currency":"BRL","name":format!("Inter PJ {}",self.account)})
    }
    async fn verify(&self) -> Result<()> {
        let token = self.token().await?;
        let value = self.get("/banking/v2/saldo", &[], &token).await?;
        if !value.is_object() {
            bail!("invalid Inter balance response")
        };
        Ok(())
    }
    async fn statements(&self, from: NaiveDate, to: NaiveDate) -> Result<Vec<Value>> {
        let mut rows = Vec::new();
        let mut ids = std::collections::HashSet::new();
        for (start, end) in http::ranges(from, to, 30)? {
            let token = self.token().await?;
            let mut expected = None;
            let mut fetched = 0;
            let mut finished = false;
            for page in 0..1000 {
                let result = self
                    .get("/banking/v2/extrato/completo", &[("dataInicio", start.to_string()), ("dataFim", end.to_string()), ("pagina", page.to_string()), ("tamanhoPagina", "50".into())], &token)
                    .await?;
                let total = result["totalPaginas"].as_u64().context("Inter omitted totalPaginas")?;
                let elements = result["totalElementos"].as_u64().context("Inter omitted totalElementos")?;
                if total > 1000 || (total == 0 && elements != 0) || expected.is_some_and(|e| e != (total, elements)) {
                    bail!("Inter pagination changed or exceeded its limit; retry the account")
                }
                expected = Some((total, elements));
                let batch = result["transacoes"].as_array().context("Inter omitted transacoes")?;
                fetched += batch.len() as u64;
                for row in batch {
                    let id = row["idTransacao"].as_str().filter(|s| !s.is_empty()).context("Inter omitted a transaction ID")?;
                    if !ids.insert(id.to_owned()) {
                        bail!("Inter repeated a transaction ID; account not published")
                    }
                    let date = means_core::parse_date(row["dataTransacao"].as_str().context("Inter omitted transaction date")?)?;
                    if date < start || date > end {
                        bail!("Inter returned a transaction outside the requested interval")
                    }
                    rows.push(row.clone());
                }
                if page + 1 >= total {
                    if fetched != elements {
                        bail!("Inter transaction count disagrees with pagination; account not published")
                    }
                    finished = true;
                    break;
                }
            }
            if !finished {
                bail!("Inter exceeded 1000 pages; account not published")
            }
        }
        Ok(rows)
    }
}
pub async fn discover(api: &str) -> Result<Vec<ConnectionAccount>> {
    let client = Client::env(api)?;
    client.verify().await?;
    Ok(vec![ConnectionAccount {
        channel: "inter_pj".into(),
        provider_account_id: client.account.clone(),
        name: format!("Inter PJ {}", client.account),
        currency: "BRL".into(),
        provider_type: "bank".into(),
        ..Default::default()
    }])
}
async fn pull(path: PathBuf, client: Client, account: Option<String>, cutoff: Option<NaiveDate>, dry: bool, inbox: Option<PathBuf>) -> Result<()> {
    if account.as_ref().is_some_and(|a| a != &client.account) {
        bail!("selected Inter account does not match INTER_ACCOUNT_NUMBER")
    }
    let db = Db::open(&path)?;
    let saved = means_core::connections::list(&db.conn())?;
    let saved = saved.iter().find(|a| a.channel == "inter_pj" && a.provider_account_id == client.account).map(|a| a.booked_from.as_str()).unwrap_or_default();
    let from = cutoff.or(means_core::parse_opt_date(saved)?).context("Inter requires a booking start: use --booked-from or save a cutoff in Connections")?;
    let to = chrono::Utc::now().with_timezone(&chrono_tz::America::Sao_Paulo).date_naive();
    let rows = client.statements(from, to).await?;
    let envelope = json!({"channel":"inter_pj","version":1,"account":client.account(),"transactions":rows,"booked_from":from,"pulled_at":chrono::Utc::now().to_rfc3339()});
    let inbox = inbox.unwrap_or_else(|| path.parent().unwrap_or(std::path::Path::new(".")).join("inbox"));
    let file = bank_api::publish(&mut db.conn(), &inbox, &envelope, dry)?;
    println!("{}: {} records, booked from {from}{}", client.account, rows.len(), file.map(|p| format!(" -> {}", p.display())).unwrap_or_else(|| " (dry run)".into()));
    Ok(())
}
pub async fn pull_connected(api: &str, path: PathBuf, account: String, inbox: PathBuf) -> Result<()> {
    pull(path, Client::env(api)?, Some(account), None, false, Some(inbox)).await
}
pub async fn run(path: PathBuf, command: Command) -> Result<()> {
    match command {
        Command::Accounts { api } => {
            for a in discover(&api).await? {
                println!("{}\t{}\t{}", a.provider_account_id, a.currency, a.name)
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
        extract::{Form, Query, State},
        http::{HeaderMap, StatusCode},
        response::{IntoResponse, Response},
        routing::{get, post},
        Json, Router,
    };
    use std::{
        collections::HashMap,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };
    async fn token(Form(f): Form<HashMap<String, String>>) -> Json<Value> {
        assert_eq!(f["scope"], "extrato.read");
        assert_eq!(f["grant_type"], "client_credentials");
        assert_eq!(f["client_id"], "test-client");
        assert_eq!(f["client_secret"], "test-secret");
        Json(json!({"access_token":"test-token"}))
    }
    async fn statement(Query(q): Query<HashMap<String, String>>, State(mode): State<Arc<AtomicUsize>>, headers: HeaderMap) -> Response {
        assert_eq!(headers["x-conta-corrente"], "0012345");
        assert_eq!(headers["authorization"], "Bearer test-token");
        assert_eq!(q["tamanhoPagina"], "50");
        let from = means_core::parse_date(&q["dataInicio"]).unwrap();
        let to = means_core::parse_date(&q["dataFim"]).unwrap();
        assert!((to - from).num_days() < 30);
        let page = q["pagina"].parse::<u64>().unwrap();
        if page == 1 && mode.load(Ordering::SeqCst) == 1 {
            return (StatusCode::TOO_MANY_REQUESTS, "test-secret private provider data").into_response();
        }
        Json(json!({"totalPaginas":2,"totalElementos":2,"transacoes":[{"idTransacao":format!("{from}-{page}"),"dataTransacao":from,"tipoOperacao":"D","valor":"10.25","titulo":"Pix","descricao":"Supplier"}]})).into_response()
    }
    #[tokio::test]
    async fn inter_requests_only_statement_scope_paginates_and_refuses_partial_publication() {
        let mode = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/oauth/v2/token", post(token))
            .route("/banking/v2/saldo", get(|| async { Json(json!({"disponivel":100})) }))
            .route("/banking/v2/extrato/completo", get(statement))
            .with_state(mode.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = || Client::new(&api, "test-client".into(), "test-secret".into(), "0012345".into(), None).unwrap();
        client().verify().await.unwrap();
        let rows = client().statements("2026-08-01".parse().unwrap(), "2026-09-01".parse().unwrap()).await.unwrap();
        assert_eq!(rows.len(), 4);
        let root = std::env::temp_dir().join(format!("means-inter-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("ledger.db");
        assert!(pull(path.clone(), client(), None, None, true, None).await.unwrap_err().to_string().contains("booking start"));
        let cutoff = Some(chrono::Utc::now().with_timezone(&chrono_tz::America::Sao_Paulo).date_naive());
        pull(path.clone(), client(), None, cutoff, true, None).await.unwrap();
        assert!(!root.join("inbox").exists());
        pull(path.clone(), client(), None, cutoff, false, None).await.unwrap();
        assert_eq!(std::fs::read_dir(root.join("inbox")).unwrap().count(), 1);
        mode.store(1, Ordering::SeqCst);
        let failed = root.join("failed");
        let error = pull(path, client(), None, cutoff, false, Some(failed.clone())).await.unwrap_err();
        assert!(error.to_string().contains("429"));
        assert!(!error.to_string().contains("test-secret"));
        assert!(!failed.exists());
        task.abort();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn production_requires_certificate_and_exact_account_digits() {
        assert!(Client::new(API, "a".into(), "b".into(), "0012345".into(), None).is_err());
        for account in ["", "12-34", "12\r\nheader"] {
            assert!(Client::new("http://127.0.0.1:1234", "a".into(), "b".into(), account.into(), None).is_err())
        }
    }
    #[tokio::test]
    async fn mutual_tls_presents_the_client_certificate_and_rejects_anonymous_clients() {
        use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        const CA: &[u8] = include_bytes!("../tests/fixtures/inter-test-ca.pem");
        const SERVER: &[u8] = include_bytes!("../tests/fixtures/inter-test-server.pem");
        const SERVER_KEY: &[u8] = include_bytes!("../tests/fixtures/inter-test-server-key.pem");
        const CLIENT: &[u8] = include_bytes!("../tests/fixtures/inter-test-client.pem");
        const CLIENT_KEY: &[u8] = include_bytes!("../tests/fixtures/inter-test-client-key.pem");
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from_pem_slice(CA).unwrap()).unwrap();
        let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider.clone()).build().unwrap();
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_client_cert_verifier(verifier)
            .with_single_cert(vec![CertificateDer::from_pem_slice(SERVER).unwrap()], PrivateKeyDer::from_pem_slice(SERVER_KEY).unwrap())
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("https://localhost:{}", listener.local_addr().unwrap().port());
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut stream = acceptor.accept(socket).await.unwrap();
            assert_eq!(stream.get_ref().1.peer_certificates().unwrap()[0], CertificateDer::from_pem_slice(CLIENT).unwrap());
            let mut bytes = vec![0; 8192];
            let n = stream.read(&mut bytes).await.unwrap();
            assert!(String::from_utf8_lossy(&bytes[..n]).starts_with("POST /oauth/v2/token"));
            let body = r#"{"access_token":"test-token"}"#;
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
            let (socket, _) = listener.accept().await.unwrap();
            assert!(acceptor.accept(socket).await.is_err());
        });
        let identity = reqwest::Identity::from_pem(&[CLIENT, b"\n", CLIENT_KEY].concat()).unwrap();
        let mut client = Client::new(&api, "test-client".into(), "test-secret".into(), "0012345".into(), Some(identity.clone())).unwrap();
        client.http = http::builder().add_root_certificate(reqwest::Certificate::from_pem(CA).unwrap()).identity(identity).build().unwrap();
        assert_eq!(client.token().await.unwrap(), "test-token");
        let anonymous = http::builder().add_root_certificate(reqwest::Certificate::from_pem(CA).unwrap()).build().unwrap();
        assert!(anonymous.post(format!("{api}/oauth/v2/token")).send().await.is_err());
        server.await.unwrap();
    }
}
