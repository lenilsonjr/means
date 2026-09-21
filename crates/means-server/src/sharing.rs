//! Local file exchange and a separate read-only loopback viewer.
use anyhow::{ensure, Context, Result};
use clap::{Args, ValueEnum};
use means_core::Db;
use means_proto::v1 as pb;
use means_sharing::{
    crypto::{self, Public},
    store::{bounded_read, write_new, Store},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use zeroize::Zeroizing;

#[derive(Clone, Copy, ValueEnum)]
pub enum Operation {
    List,
    IdentityCreate,
    IdentityRestore,
    Public,
    Pin,
    Grant,
    Export,
    Import,
    Acknowledge,
    Revoke,
    Revocation,
    Rotate,
    AcceptRotation,
    View,
}
impl Operation {
    fn name(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::IdentityCreate => "identity-create",
            Self::IdentityRestore => "identity-restore",
            Self::Public => "public",
            Self::Pin => "pin",
            Self::Grant => "grant",
            Self::Export => "export",
            Self::Import => "import",
            Self::Acknowledge => "acknowledge",
            Self::Revoke => "revoke",
            Self::Revocation => "revocation",
            Self::Rotate => "rotate",
            Self::AcceptRotation => "accept-rotation",
            Self::View => "view",
        }
    }
}
#[derive(Args)]
pub struct Cli {
    #[arg(value_enum)]
    operation: Operation,
    /// Positional fields shown by `means share list`
    arguments: Vec<String>,
    /// Apply the exact grant/revocation preview token
    #[arg(long)]
    confirm: Option<String>,
    /// Private sharing directory (default: sharing/ beside the ledger)
    #[arg(long)]
    store: Option<PathBuf>,
}
pub fn root(db: &Path) -> PathBuf {
    db.parent().unwrap_or(Path::new(".")).join("sharing")
}
pub fn actions() -> Vec<pb::SharingAction> {
    [
        ("identity-create", "Create encrypted identity and verified manual backup", vec!["Identity file", "Backup file"], true, false),
        ("identity-restore", "Restore identity using independently verified fingerprint", vec!["Identity file", "Verified fingerprint"], true, false),
        ("public", "Save your public identity for a collaborator", vec!["Public identity output file"], false, false),
        ("pin", "Trust collaborator after independent fingerprint verification", vec!["Public identity file", "Verified fingerprint"], false, false),
        ("grant", "Preview and grant a read-only copy of one whole vault", vec!["Vault entity ID", "Recipient fingerprint", "Expiry (RFC3339)"], false, false),
        ("export", "Export or retry the pending encrypted snapshot", vec!["Grant ID", "New output file"], true, false),
        ("import", "Accept encrypted snapshot and write signed receipt", vec!["Encrypted file", "Owner fingerprint", "Receipt output file"], true, false),
        ("acknowledge", "Accept recipient receipt before the next snapshot", vec!["Grant ID", "Receipt file"], false, false),
        ("revoke", "Preview and revoke future sharing; received copies remain readable", vec!["Grant ID"], false, false),
        ("revocation", "Export a signed revocation notice", vec!["Grant ID", "New output file"], true, false),
        ("rotate", "Rotate owner keys with old-key proof and verified backup", vec!["New identity file", "New backup file", "Rotation proof output file"], true, true),
        ("accept-rotation", "Accept owner rotation after fingerprint verification", vec!["Owner fingerprint", "Rotation proof file", "Verified new fingerprint"], false, false),
        ("view", "Browse an isolated read-only received vault", vec!["Grant ID"], false, false),
    ]
    .into_iter()
    .map(|(name, description, fields, passphrase, new_passphrase)| pb::SharingAction {
        name: name.into(),
        description: description.into(),
        fields: fields.into_iter().map(String::from).collect(),
        passphrase,
        new_passphrase,
    })
    .collect()
}
pub async fn run(db_path: PathBuf, args: Cli) -> Result<()> {
    let operation = args.operation.name();
    let action = actions().into_iter().find(|a| a.name == operation);
    if let Some(a) = &action {
        ensure!(args.arguments.len() == a.fields.len(), "{} expects: {}", operation, a.fields.join(" · "));
    }
    let passphrase = Zeroizing::new(if action.as_ref().is_some_and(|a| a.passphrase) { rpassword::prompt_password("Sharing passphrase: ")? } else { String::new() });
    let new_passphrase = Zeroizing::new(if action.as_ref().is_some_and(|a| a.new_passphrase) { rpassword::prompt_password("New sharing passphrase: ")? } else { String::new() });
    let request = pb::SharingRequest {
        operation: operation.into(),
        arguments: args.arguments,
        passphrase: passphrase.to_string(),
        new_passphrase: new_passphrase.to_string(),
        confirmation: args.confirm.unwrap_or_default(),
    };
    let store = args.store.unwrap_or_else(|| root(&db_path));
    if operation == "view" {
        let (db, replica) = Store::open(&store)?.open_replica(&request.arguments[0])?;
        let (url, task) = viewer(db, replica).await?;
        let result = means_tui::run(&url).await;
        task.abort();
        return result;
    }
    let response = execute(&db_path, &store, request)?;
    for line in response.lines {
        println!("{line}");
    }
    for action in response.actions {
        println!("{} {} — {}", action.name, action.fields.iter().map(|s| format!("<{s}>")).collect::<Vec<_>>().join(" "), action.description);
    }
    if !response.confirmation.is_empty() {
        println!("Review the scope above, then repeat with --confirm {}", response.confirmation);
    }
    Ok(())
}
pub fn execute(db_path: &Path, root: &Path, mut request: pb::SharingRequest) -> Result<pb::SharingResponse> {
    let passphrase = Zeroizing::new(std::mem::take(&mut request.passphrase));
    let new_passphrase = Zeroizing::new(std::mem::take(&mut request.new_passphrase));
    let mut response = pb::SharingResponse::default();
    let op = request.operation.as_str();
    let args = &request.arguments;
    if op != "list" {
        let action = actions().into_iter().find(|a| a.name == op).context("unknown sharing operation")?;
        ensure!(args.len() == action.fields.len(), "expected fields: {}", action.fields.join(" · "));
    }
    let mut store = Store::open(root)?;
    match op {
        "list" => {
            let overview = store.overview()?;
            response.lines.push(format!("Identity: {}", overview.identity_root.unwrap_or_else(|| "not configured".into())));
            for (fp, pubkey) in overview.pins {
                response.lines.push(format!("Trusted {fp} · current {}", pubkey.fingerprint()?));
            }
            for g in overview.grants {
                response.lines.push(format!("Grant {} · vault {} · expires {}{}", g.id, g.vault, g.expires_at, if g.revoked { " · revoked" } else { "" }));
            }
            for r in overview.replicas {
                response.lines.push(format!("Received {} · vault {} · revision {} · {}", r.grant, r.vault, r.revision, r.status()?));
            }
            response.actions = actions();
        }
        "identity-create" => {
            let p = store.create_identity(Path::new(&args[0]), Path::new(&args[1]), &passphrase)?;
            response.lines.push(format!("Identity and backup verified. Fingerprint: {}", p.fingerprint()?));
        }
        "identity-restore" => {
            let p = store.restore_identity(Path::new(&args[0]), &args[1], &passphrase)?;
            response.lines.push(format!("Restored {}", p.fingerprint()?));
        }
        "public" => {
            let public = store.overview()?.identity.context("create or restore an identity first")?;
            write_new(Path::new(&args[0]), &crypto::canonical(&public)?)?;
            response.lines.push(format!("Public identity saved. Verify fingerprint independently: {}", public.fingerprint()?));
        }
        "pin" => {
            let public: Public = crypto::parse(&bounded_read(Path::new(&args[0]), crypto::MAX_IDENTITY)?)?;
            response.lines.push(format!("Trusted {}", store.pin(&public, &args[1])?));
        }
        "grant" => {
            let db = Db::open(db_path)?;
            let entity = args[0].parse()?;
            if request.confirmation.is_empty() {
                let scope = store.preview(&db, entity, &args[1], &args[2])?;
                response.lines.push(format!("Read-only entire vault: {} ({})", scope.name, scope.vault));
                response.lines.push(format!("Recipient: {} · expires {}", args[1], args[2]));
                for (name, count) in scope.counts {
                    response.lines.push(format!("{name}: {count}"));
                }
                response.confirmation = scope.token;
            } else {
                let grant = store.grant(&db, entity, &args[1], &args[2], &request.confirmation)?;
                response.lines.push(format!("Created grant {}", grant.id));
            }
        }
        "export" | "revocation" => {
            let db = Db::open(db_path)?;
            let manifest = store.export(&db, &args[0], &passphrase, Path::new(&args[1]), op == "revocation")?;
            response.lines.push(format!("Exported {} revision {}. Await recipient receipt before the next delivery.", manifest.kind, manifest.revision));
        }
        "import" => {
            let r = store.import(Path::new(&args[0]), &args[1], &passphrase, Path::new(&args[2]))?;
            response.lines.push(format!("Accepted {} revision {} · {}. Return the receipt to the owner.", r.grant, r.revision, r.status()?));
        }
        "acknowledge" => {
            let r = store.acknowledge(&args[0], Path::new(&args[1]))?;
            response.lines.push(format!("Acknowledged revision {}", r.revision));
        }
        "revoke" => {
            let grant = store.grant_by_id(&args[0])?;
            let token = crypto::digest(&crypto::canonical(&grant)?);
            if request.confirmation.is_empty() {
                response.lines.push(format!("Revoke grant {} for vault {}. Received copies stay readable; export and deliver a revocation notice to label them.", grant.id, grant.vault));
                response.confirmation = token;
            } else {
                ensure!(request.confirmation == token, "stale revocation preview");
                store.revoke(&args[0])?;
                response.lines.push("Grant revoked. Deliver a revocation notice to the recipient.".into());
            }
        }
        "rotate" => {
            let p = store.rotate(Path::new(&args[0]), Path::new(&args[1]), &passphrase, &new_passphrase, Path::new(&args[2]))?;
            response
                .lines
                .push(format!("Rotated. New fingerprint: {}. Deliver the proof and verify the new fingerprint independently. Recipient-key changes require replacement grants.", p.fingerprint()?));
        }
        "accept-rotation" => {
            let p = store.accept_rotation(&args[0], Path::new(&args[1]), &args[2])?;
            response.lines.push(format!("Accepted new owner key {}", p.fingerprint()?));
        }
        _ => anyhow::bail!("view must run through the local viewer"),
    }
    Ok(response)
}
pub async fn viewer(db: Db, replica: means_sharing::store::Replica) -> Result<(String, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let access = Arc::new(crate::local_api::LocalAccess::new(address));
    let service = crate::grpc::MeansService::replica(Arc::new(db), replica);
    let grpc = pb::means_server::MeansServer::new(service).max_decoding_message_size(128 * 1024 * 1024).max_encoding_message_size(128 * 1024 * 1024);
    let app =
        tonic::service::Routes::new(grpc).into_axum_router().fallback(|| async { axum::http::StatusCode::NOT_FOUND }).layer(axum::middleware::from_fn_with_state(access, crate::local_api::guard));
    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("replica viewer stopped: {e}");
        }
    });
    Ok((format!("http://{address}"), task))
}

/// Dropping the last owner service or closing a view shuts its listener down.
pub struct Viewer(tokio::task::JoinHandle<()>);
impl Viewer {
    pub fn new(task: tokio::task::JoinHandle<()>) -> Self {
        Self(task)
    }
    pub fn finished(&self) -> bool {
        self.0.is_finished()
    }
}
impl Drop for Viewer {
    fn drop(&mut self) {
        self.0.abort();
    }
}
