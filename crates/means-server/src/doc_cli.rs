//! `means doc`: store documents as evidence and link them to entries and statement lines.
//!
//! With `--json`, every command prints one JSON object with `"schema_version": 1`.
//! A failure prints `{"schema_version":1,"ok":false,"error":{"code":..,"message":..}}` and exits with code 1.
//! No command deletes a document.

use std::path::PathBuf;

use clap::Subcommand;
use means_core::documents::{self, DocumentLink, Link};
use means_core::rusqlite::Connection;
use means_core::Db;
use serde_json::{json, Value};

const SCHEMA_VERSION: u32 = 1;

#[derive(Subcommand)]
pub enum Command {
    /// Store a file as evidence for an entity; the same bytes twice return the first record
    Add {
        file: PathBuf,
        /// Entity id, uid or name
        #[arg(long)]
        entity: String,
        /// email | upload | api | filing | import | other
        #[arg(long, default_value = "upload")]
        source: String,
        /// Date the document arrived (YYYY-MM-DD or RFC 3339; default: today)
        #[arg(long)]
        received: Option<String>,
        /// Link the document to this journal entry uid
        #[arg(long)]
        link_entry: Option<String>,
        /// Link the document to this statement line id
        #[arg(long)]
        link_line: Option<String>,
        /// Role of the links, for example invoice or receipt
        #[arg(long, default_value = "")]
        role: String,
        /// Metadata as KEY=VALUE; repeat for more keys
        #[arg(long = "meta")]
        meta: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// List the documents of an entity, newest received first
    List {
        /// Entity id, uid or name
        #[arg(long)]
        entity: String,
        /// Only documents without a link
        #[arg(long)]
        unlinked: bool,
        /// Only documents received on or after this date
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Link a document to a journal entry or a statement line
    Link {
        /// Document uid
        document: String,
        /// Journal entry uid
        #[arg(long, conflicts_with = "line", required_unless_present = "line")]
        entry: Option<String>,
        /// Statement line id
        #[arg(long)]
        line: Option<String>,
        #[arg(long, default_value = "")]
        role: String,
        #[arg(long)]
        json: bool,
    },
    /// Show a document, its links and the path of its stored file
    Show {
        /// Document uid
        document: String,
        #[arg(long)]
        json: bool,
    },
    /// Rehash every stored file and report missing files and mismatches
    Verify {
        #[arg(long)]
        json: bool,
    },
}

struct Failure {
    code: &'static str,
    message: String,
}

impl Failure {
    fn new(code: &'static str, message: impl Into<String>) -> Failure {
        Failure { code, message: message.into() }
    }
}

impl From<means_core::Error> for Failure {
    fn from(e: means_core::Error) -> Failure {
        Failure { code: documents::error_code(&e), message: e.to_string() }
    }
}

impl From<serde_json::Error> for Failure {
    fn from(e: serde_json::Error) -> Failure {
        Failure::new("io_error", e.to_string())
    }
}

/// What a command prints on success: a JSON body and a short text for people.
struct Output {
    body: Value,
    text: String,
    /// False when the command ran but found a problem (verify). The process then exits with code 1.
    ok: bool,
    error: Option<Failure>,
}

pub fn run(db_path: PathBuf, command: Command) -> anyhow::Result<()> {
    let json = match &command {
        Command::Add { json, .. } | Command::List { json, .. } | Command::Link { json, .. } | Command::Show { json, .. } | Command::Verify { json } => *json,
    };
    match execute(&db_path, command) {
        Ok(out) => {
            if json {
                let mut body = json!({ "schema_version": SCHEMA_VERSION, "ok": out.ok });
                if let (Value::Object(target), Value::Object(fields)) = (&mut body, out.body) {
                    target.extend(fields);
                }
                if let Some(error) = &out.error {
                    body["error"] = json!({ "code": means_core::proposal::normalize_code(error.code), "message": error.message });
                }
                println!("{}", serde_json::to_string_pretty(&body)?);
            } else {
                println!("{}", out.text);
            }
            if !out.ok {
                std::process::exit(1);
            }
            Ok(())
        }
        Err(failure) => {
            if json {
                println!("{}", json!({ "schema_version": SCHEMA_VERSION, "ok": false, "error": { "code": means_core::proposal::normalize_code(failure.code), "message": failure.message } }));
            } else {
                eprintln!("error ({}): {}", failure.code, failure.message);
            }
            std::process::exit(1);
        }
    }
}

fn execute(db_path: &std::path::Path, command: Command) -> Result<Output, Failure> {
    let db = Db::open(db_path)?;
    let mut conn = db.conn();
    match command {
        Command::Add { file, entity, source, received, link_entry, link_line, role, meta, json: _ } => {
            let entity = resolve_entity(&conn, &entity)?;
            let filename = file.file_name().and_then(|n| n.to_str()).ok_or_else(|| Failure::new("invalid_path", format!("{} has no usable file name", file.display())))?;
            documents::validate_filename(filename)?;
            let metadata = parse_meta(&meta)?;
            // Resolve the link targets before the file is stored, so that a typo stores nothing.
            let mut targets = Vec::new();
            if let Some(uid) = &link_entry {
                targets.push(Link::Entry(documents::entry_id_for(&conn, uid)?));
            }
            if let Some(line) = &link_line {
                targets.push(Link::StatementLine(parse_line(&conn, line)?));
            }
            let bytes = std::fs::read(&file).map_err(|e| {
                let code = if e.kind() == std::io::ErrorKind::NotFound { "not_found" } else { "io_error" };
                Failure::new(code, format!("read {}: {e}", file.display()))
            })?;
            let received = received.unwrap_or_else(|| means_core::today().to_string());
            let doc = documents::add_document(&mut conn, entity.id, &bytes, filename, &source, &received, metadata)?;
            let mut links = Vec::new();
            for target in targets {
                match documents::link_document(&mut conn, doc.id, target, &role) {
                    Ok(link) => links.push(link),
                    // A repeated add with the same link is idempotent.
                    Err(e) if documents::error_code(&e) == "duplicate" => links.extend(existing_link(&conn, doc.id, target, &role)?),
                    Err(e) => return Err(e.into()),
                }
            }
            let path = documents::absolute_path(&conn, &doc)?;
            let text = format!("{} {} ({}, {} bytes) -> {}", if doc.created { "Stored" } else { "Already stored" }, doc.uid, doc.filename, doc.size, path.display());
            Ok(done(json!({ "created": doc.created, "document": doc, "links": links, "stored_path": path }), text))
        }
        Command::List { entity, unlinked, since, json: _ } => {
            let entity = resolve_entity(&conn, &entity)?;
            if let Some(since) = &since {
                means_core::parse_date(since)?;
            }
            let docs = documents::list_documents(&conn, entity.id, unlinked, since.as_deref())?;
            let text = docs.iter().map(|d| format!("{}  {}  {}  {}  {}", d.uid, d.received_at, d.source, d.mime, d.filename)).collect::<Vec<_>>().join("\n");
            Ok(done(json!({ "entity_uid": entity.uid, "count": docs.len(), "documents": docs }), if text.is_empty() { "No documents.".into() } else { text }))
        }
        Command::Link { document, entry, line, role, json: _ } => {
            let doc = documents::get_document(&conn, &document)?;
            let target = match (entry, line) {
                (Some(uid), _) => Link::Entry(documents::entry_id_for(&conn, &uid)?),
                (None, Some(line)) => Link::StatementLine(parse_line(&conn, &line)?),
                (None, None) => return Err(Failure::new("invalid_input", "give --entry or --line")),
            };
            let link = documents::link_document(&mut conn, doc.id, target, &role)?;
            let text = format!("Linked {} as link #{}", doc.uid, link.id);
            Ok(done(json!({ "link": link }), text))
        }
        Command::Show { document, json: _ } => {
            let doc = documents::get_document(&conn, &document)?;
            let links = documents::links_for_document(&conn, doc.id)?;
            let path = documents::absolute_path(&conn, &doc)?;
            let text = format!("{}  {}  {}  {} bytes  sha256 {}\n  stored at {}\n  {} link(s)", doc.uid, doc.filename, doc.mime, doc.size, doc.sha256, path.display(), links.len());
            Ok(done(json!({ "document": doc, "links": links, "stored_path": path, "exists": path.is_file() }), text))
        }
        Command::Verify { json: _ } => {
            let report = documents::verify_all(&conn)?;
            let text = format!("{} documents checked: {} ok, {} missing, {} mismatched", report.checked, report.passed, report.missing.len(), report.mismatched.len());
            let error = if !report.mismatched.is_empty() {
                Some(Failure::new("hash_mismatch", format!("{} stored file(s) do not match their hash", report.mismatched.len())))
            } else if !report.missing.is_empty() {
                Some(Failure::new("not_found", format!("{} stored file(s) are missing", report.missing.len())))
            } else {
                None
            };
            Ok(Output { ok: error.is_none(), body: serde_json::to_value(&report)?, text, error })
        }
    }
}

fn done(body: Value, text: String) -> Output {
    Output { body, text, ok: true, error: None }
}

fn resolve_entity(conn: &Connection, key: &str) -> Result<means_core::model::Entity, Failure> {
    match means_core::entities::find_by_key(conn, key) {
        Err(means_core::Error::NotFound(_)) => Err(Failure::new("not_found", format!("no entity {:?}", key.trim()))),
        other => Ok(other?),
    }
}

fn parse_line(conn: &Connection, key: &str) -> Result<i64, Failure> {
    let id = key.trim().parse::<i64>().map_err(|_| Failure::new("invalid_input", format!("statement line must be a numeric id, got {key:?}")))?;
    means_core::imports::get_line(conn, id)?;
    Ok(id)
}

fn parse_meta(pairs: &[String]) -> Result<Value, Failure> {
    let mut map = serde_json::Map::new();
    for pair in pairs {
        let (key, value) = pair.split_once('=').ok_or_else(|| Failure::new("invalid_input", format!("metadata must be KEY=VALUE, got {pair:?}")))?;
        let key = key.trim();
        if key.is_empty() {
            return Err(Failure::new("invalid_input", format!("metadata key is empty in {pair:?}")));
        }
        map.insert(key.to_string(), Value::String(value.to_string()));
    }
    Ok(Value::Object(map))
}

fn existing_link(conn: &Connection, document_id: i64, target: Link, role: &str) -> Result<Option<DocumentLink>, Failure> {
    let links = documents::links_for_document(conn, document_id)?;
    Ok(links.into_iter().find(|l| {
        l.role == role.trim()
            && match target {
                Link::Entry(id) => l.entry_id == Some(id) && l.posting_id.is_none() && l.statement_line_id.is_none(),
                Link::Posting(id) => l.posting_id == Some(id) && l.entry_id.is_none() && l.statement_line_id.is_none(),
                Link::StatementLine(id) => l.statement_line_id == Some(id) && l.entry_id.is_none() && l.posting_id.is_none(),
            }
    }))
}
