//! End-to-end: start the real binary on a temp ledger and drive it over native gRPC.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use means_proto::v1 as pb;
use means_proto::v1::means_client::MeansClient;

struct Server {
    child: Child,
    url: String,
    db: std::path::PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.db);
        let _ = std::fs::remove_file(format!("{}-wal", self.db.display()));
        let _ = std::fs::remove_file(format!("{}-shm", self.db.display()));
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn start() -> Server {
    start_with_env(&[])
}

fn start_with_env(env: &[(&str, &str)]) -> Server {
    let port = free_port();
    let db = std::env::temp_dir().join(format!("means-e2e-{}-{port}.db", std::process::id()));
    let child = Command::new(env!("CARGO_BIN_EXE_means"))
        .envs(env.iter().copied())
        .args(["--db", db.to_str().unwrap(), "serve", "--listen", &format!("127.0.0.1:{port}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start means");
    let url = format!("http://127.0.0.1:{port}");
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(30) {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Server { child, url, db }
}

#[tokio::test]
async fn onboarding_capture_import_and_reports_over_grpc() {
    let server = start();
    let mut client = MeansClient::connect(server.url.clone()).await.expect("connect");
    let status = client.get_status(pb::GetStatusRequest {}).await.unwrap().into_inner();
    assert!(!status.onboarded);
    assert_eq!(status.entities, 0);

    // Onboarding: entities.
    let personal =
        client.create_entity(pb::CreateEntityRequest { name: "Personal".into(), kind: "person".into(), country: "PT".into(), currency: "EUR".into() }).await.unwrap().into_inner().entity.unwrap();
    let llc = client.create_entity(pb::CreateEntityRequest { name: "LLC".into(), kind: "company".into(), country: "US".into(), currency: "USD".into() }).await.unwrap().into_inner().entity.unwrap();
    assert_eq!(personal.currency, "EUR");
    let dup = client.create_entity(pb::CreateEntityRequest { name: "Personal".into(), kind: "person".into(), country: "".into(), currency: "EUR".into() }).await;
    assert_eq!(dup.unwrap_err().code(), tonic::Code::AlreadyExists);

    // Accounts, with an opening balance.
    let n26 = client
        .create_account(pb::CreateAccountRequest {
            entity_id: personal.id,
            name: "N26".into(),
            r#type: "asset".into(),
            subtype: "bank".into(),
            commodity: "EUR".into(),
            opening_balance: "500".into(),
            opening_date: "2026-01-01".into(),
            in_net_worth: true,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner()
        .account
        .unwrap();
    assert_eq!(n26.balance, "500.00");
    let food =
        client.create_account(pb::CreateAccountRequest { entity_id: personal.id, name: "Food".into(), r#type: "expense".into(), ..Default::default() }).await.unwrap().into_inner().account.unwrap();
    let mercury = client
        .create_account(pb::CreateAccountRequest { entity_id: llc.id, name: "Mercury".into(), r#type: "asset".into(), subtype: "bank".into(), commodity: "USD".into(), ..Default::default() })
        .await
        .unwrap()
        .into_inner()
        .account
        .unwrap();
    client.set_price(pb::SetPriceRequest { commodity: "USD".into(), currency: "EUR".into(), on: "2026-01-01".into(), price: "0.88".into(), source: "manual".into() }).await.unwrap();

    // Capture.
    let e = client
        .create_simple_entry(pb::CreateSimpleEntryRequest {
            entry: Some(pb::SimpleEntryInput {
                entity_id: personal.id,
                date: "2026-02-01".into(),
                kind: "expense".into(),
                account_id: n26.id,
                contra_account_id: food.id,
                quantity: "3.20".into(),
                payee: "Cafe".into(),
                status: "posted".into(),
                ..Default::default()
            }),
        })
        .await
        .unwrap()
        .into_inner()
        .entry
        .unwrap();
    assert_eq!(e.postings.len(), 2);
    assert_eq!(e.kind, "expense");
    let unbalanced = client
        .create_journal_entry(pb::CreateJournalEntryRequest {
            entry: Some(pb::JournalEntryInput {
                entity_id: personal.id,
                date: "2026-02-02".into(),
                postings: vec![
                    pb::PostingInput { account_id: n26.id, quantity: "-10".into(), ..Default::default() },
                    pb::PostingInput { account_id: food.id, quantity: "9".into(), ..Default::default() },
                ],
                ..Default::default()
            }),
        })
        .await;
    assert_eq!(unbalanced.unwrap_err().code(), tonic::Code::FailedPrecondition);

    // Money between entities: two linked entries.
    let t = client
        .create_transfer(pb::CreateTransferRequest {
            date: "2026-02-03".into(),
            from_account_id: mercury.id,
            to_account_id: n26.id,
            from_quantity: "1000".into(),
            to_quantity: "870".into(),
            status: "posted".into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    assert!(t.counterpart.is_some());
    assert_eq!(t.entry.unwrap().entity_id, llc.id);

    // Account-free previews retain unrounded parser values without inventing a unit.
    let unplaced = client
        .upload_import(pb::UploadImportRequest {
            source: "generic_csv".into(),
            filename: "unplaced.csv".into(),
            content: b"Date,Description,Amount,Balance\n2026-02-01,Preview,-12.345,100.12345\n".to_vec(),
            preview: true,
            mapping: Some(pb::CsvMapping {
                date_column: "Date".into(),
                description_column: "Description".into(),
                amount_column: "Amount".into(),
                balance_column: "Balance".into(),
                decimal_separator: ".".into(),
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(unplaced.lines[0].amount, "-12.345");
    assert_eq!(unplaced.lines[0].balance_after, "100.12345");
    assert!(unplaced.lines[0].currency.is_empty());

    // Import a statement: the coffee is matched, the rest drafted.
    let csv = "\"Booking Date\",\"Value Date\",\"Partner Name\",\"Partner Iban\",\"Type\",\"Payment Reference\",\"Account Name\",\"Amount (EUR)\",\"Original Amount\",\"Original Currency\",\"Exchange Rate\"\n\"2026-02-01\",\"2026-02-01\",\"Cafe Central\",\"\",\"MasterCard Payment\",\"\",\"Main Account\",\"-3.20\",\"\",\"\",\"\"\n\"2026-02-05\",\"2026-02-05\",\"LIDL\",\"\",\"MasterCard Payment\",\"\",\"Main Account\",\"-41.00\",\"\",\"\",\"\"\n";
    let preview = client
        .upload_import(pb::UploadImportRequest { source: "auto".into(), account_id: n26.id, filename: "n26.csv".into(), content: csv.as_bytes().to_vec(), preview: true, ..Default::default() })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(preview.detected_source, "n26_csv");
    assert_eq!(preview.lines.len(), 2);
    let done = client
        .upload_import(pb::UploadImportRequest { source: "n26_csv".into(), account_id: n26.id, filename: "n26.csv".into(), content: csv.as_bytes().to_vec(), preview: false, ..Default::default() })
        .await
        .unwrap()
        .into_inner();
    let imp = done.import.unwrap();
    assert_eq!(imp.matched_count, 1);
    assert_eq!(imp.created_count, 1);
    let drafts = client.list_journal_entries(pb::ListJournalEntriesRequest { entity_id: personal.id, status: "draft".into(), ..Default::default() }).await.unwrap().into_inner();
    assert_eq!(drafts.total, 1);
    let draft = &drafts.entries[0];
    // Review: post the draft against Food (bank posting kept, contra balanced).
    let posted = client
        .update_journal_entry(pb::UpdateJournalEntryRequest {
            id: draft.id,
            entry: Some(pb::JournalEntryInput {
                entity_id: personal.id,
                date: draft.date.clone(),
                payee: "Lidl".into(),
                status: "posted".into(),
                postings: vec![pb::PostingInput { account_id: n26.id, quantity: "-41".into(), ..Default::default() }, pb::PostingInput { account_id: food.id, ..Default::default() }],
                ..Default::default()
            }),
        })
        .await
        .unwrap()
        .into_inner()
        .entry
        .unwrap();
    assert_eq!(posted.status, "posted");
    assert!(!posted.postings[0].reconciled_at.is_empty());

    // Reports.
    let tb = client.trial_balance(pb::TrialBalanceRequest { entity_id: personal.id, as_of: "".into() }).await.unwrap().into_inner();
    assert_eq!(tb.total_debit, tb.total_credit);
    let nw = client.net_worth(pb::NetWorthRequest { entity_id: personal.id, currency: "EUR".into(), as_of: "".into() }).await.unwrap().into_inner();
    assert_eq!(nw.by_entity.len(), 1);
    assert_eq!(nw.by_entity[0].account_id, personal.id);
    let gl = client.general_ledger(pb::GeneralLedgerRequest { account_id: n26.id, ..Default::default() }).await.unwrap().into_inner();
    assert_eq!(gl.closing_balance, "1325.8"); // 500 + 870 - 3.20 - 41
    let recent = client.general_ledger(pb::GeneralLedgerRequest { account_id: n26.id, limit: 1, newest_first: true, ..Default::default() }).await.unwrap().into_inner();
    assert_eq!(recent.rows.len(), 1);
    assert_eq!(&recent.rows[0], gl.rows.last().unwrap());
    assert_eq!(recent.closing_balance, gl.closing_balance);
    let mut app = means_tui::app::App::new(&server.url);
    app.screen = means_tui::app::Screen::Accounts;
    app.load_screen().await;
    app.accounts_sel = app.accounts.iter().position(|a| a.id == n26.id).unwrap();
    app.handle_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Enter, crossterm::event::KeyModifiers::NONE)).await;
    let ledger = app.ledger.as_ref().unwrap();
    assert_eq!(ledger.selected, 0);
    assert_eq!(ledger.rows[0].posting_id, gl.rows.last().unwrap().posting_id);
    assert_eq!(ledger.rows[0].running_balance, gl.closing_balance);

    let rec = client.reconciliation(pb::ReconciliationRequest { account_id: n26.id }).await.unwrap().into_inner();
    assert_eq!(rec.unreconciled_postings, 2, "opening balance and the transfer are not yet on a statement");
    let status = client.get_status(pb::GetStatusRequest {}).await.unwrap().into_inner();
    assert!(status.onboarded);
    assert_eq!(status.drafts, 0);
    drop(server);
}

/// Opt-in API migration check for an explicitly supplied Account Tracker backup.
#[tokio::test]
#[ignore = "requires an explicitly supplied MEANS_ATB backup; may print private data"]
async fn account_tracker_backup_over_grpc() {
    let path = std::env::var_os("MEANS_ATB").expect("set MEANS_ATB to an explicit backup path");
    let content = std::fs::read(path).expect("read explicitly selected backup");
    let server = start();
    let mut client = MeansClient::connect(server.url.clone()).await.expect("connect");
    let insp = client.inspect_account_tracker(pb::InspectAccountTrackerRequest { content: content.clone() }).await.unwrap().into_inner();
    assert!(!insp.accounts.is_empty());
    let personal = client
        .create_entity(pb::CreateEntityRequest { name: "Import check".into(), kind: "person".into(), country: String::new(), currency: insp.base_currency.clone() })
        .await
        .unwrap()
        .into_inner()
        .entity
        .unwrap();
    let mapping: Vec<pb::AccountTrackerMapping> =
        insp.accounts.iter().map(|a| pb::AccountTrackerMapping { external_id: a.external_id.clone(), entity_id: personal.id, r#type: String::new(), subtype: String::new(), skip: false }).collect();
    let started = Instant::now();
    let out = client
        .import_account_tracker(pb::ImportAccountTrackerRequest { content, filename: "backup.atb".into(), mapping, default_entity_id: personal.id, expand_recurring: true, schedules: true })
        .await
        .unwrap()
        .into_inner();
    eprintln!(
        "api migration: {} created in {:.1}s, {} warnings, checks ok {}/{}",
        out.import.as_ref().map(|i| i.created_count).unwrap_or(0),
        started.elapsed().as_secs_f64(),
        out.warnings.len(),
        out.checks.iter().filter(|c| c.ok).count(),
        out.checks.len()
    );
    assert!(
        out.checks.iter().all(|c| c.ok),
        "every Account Tracker balance reproduces: {:?}",
        out.checks.iter().filter(|c| !c.ok).map(|c| format!("{} {} vs {}", c.name, c.expected, c.actual)).collect::<Vec<_>>()
    );
    let status = client.get_status(pb::GetStatusRequest {}).await.unwrap().into_inner();
    assert!(status.journal_entries > 0);
    drop(server);
}

#[tokio::test]
async fn imported_evidence_survives_posting_and_editing_over_grpc() {
    let server = start();
    let mut client = MeansClient::connect(server.url.clone()).await.unwrap();
    let entity = client.create_entity(pb::CreateEntityRequest { name: "LLC".into(), kind: "company".into(), country: "US".into(), currency: "USD".into() }).await.unwrap().into_inner().entity.unwrap();
    let bank = client
        .create_account(pb::CreateAccountRequest { entity_id: entity.id, name: "Mercury".into(), r#type: "asset".into(), subtype: "bank".into(), commodity: "USD".into(), ..Default::default() })
        .await
        .unwrap()
        .into_inner()
        .account
        .unwrap();
    let expense =
        client.create_account(pb::CreateAccountRequest { entity_id: entity.id, name: "Domains".into(), r#type: "expense".into(), ..Default::default() }).await.unwrap().into_inner().account.unwrap();
    let csv = "\"Date (UTC)\",\"Description\",\"Amount\",\"Status\",\"Transaction ID\"\n\"03-15-2026\",\"NAMECHEAP.COM\",\"-12.50\",\"Sent\",\"txn_9f3a\"\n";
    let uploaded = client
        .upload_import(pb::UploadImportRequest { source: "mercury_csv".into(), account_id: bank.id, filename: "mercury.csv".into(), content: csv.as_bytes().to_vec(), ..Default::default() })
        .await
        .unwrap()
        .into_inner()
        .import
        .unwrap();
    let line = client.get_import(pb::GetImportRequest { id: uploaded.id }).await.unwrap().into_inner().lines.remove(0);
    let mut entry = client.get_journal_entry(pb::GetJournalEntryRequest { id: line.journal_entry_id }).await.unwrap().into_inner().entry.unwrap();
    let original = entry.postings.iter().find(|p| p.account_id == bank.id).unwrap().clone();
    assert_eq!(original.external_id, "txn_9f3a");
    assert_eq!(original.fingerprint, line.fingerprint);
    assert!(!original.fingerprint.is_empty());
    assert!(!original.reconciled_at.is_empty());

    // First post the imported draft, then rebuild it again by editing its payee.
    for payee in ["Namecheap", "Namecheap Domains"] {
        let postings = entry
            .postings
            .iter()
            .map(|p| pb::PostingInput {
                account_id: if p.account_id == bank.id { bank.id } else { expense.id },
                quantity: p.quantity.clone(),
                amount: p.amount.clone(),
                memo: p.memo.clone(),
                metadata: p.metadata.clone(),
                external_id: p.external_id.clone(),
                fingerprint: p.fingerprint.clone(),
            })
            .collect();
        entry = client
            .update_journal_entry(pb::UpdateJournalEntryRequest {
                id: entry.id,
                entry: Some(pb::JournalEntryInput {
                    entity_id: entity.id,
                    date: entry.date.clone(),
                    payee: payee.into(),
                    status: "posted".into(),
                    postings,
                    origin: entry.origin.clone(),
                    ..Default::default()
                }),
            })
            .await
            .unwrap()
            .into_inner()
            .entry
            .unwrap();
        let posting = entry.postings.iter().find(|p| p.account_id == bank.id).unwrap();
        assert_eq!(posting.external_id, original.external_id);
        assert_eq!(posting.fingerprint, original.fingerprint);
        assert_eq!(posting.reconciled_at, original.reconciled_at);
        let linked = client.get_import(pb::GetImportRequest { id: uploaded.id }).await.unwrap().into_inner().lines.remove(0);
        assert_eq!(linked.posting_id, posting.id);
        assert_eq!(linked.journal_entry_id, entry.id);
    }

    // A different file containing the same transaction must not create another entry.
    let overlap = format!("{csv}\n");
    let repeated = client
        .upload_import(pb::UploadImportRequest { source: "mercury_csv".into(), account_id: bank.id, filename: "overlap.csv".into(), content: overlap.into_bytes(), ..Default::default() })
        .await
        .unwrap()
        .into_inner()
        .import
        .unwrap();
    assert_eq!(repeated.duplicate_count, 1);
    assert_eq!(repeated.created_count, 0);
    let db = means_core::Db::open(&server.db).unwrap();
    let conn = db.conn();
    let persisted = means_core::journal::get_entry(&conn, entry.id).unwrap();
    let bank_posting = persisted.postings.iter().find(|p| p.account_id == bank.id).unwrap();
    assert_eq!(bank_posting.external_id.as_deref(), Some("txn_9f3a"));
    assert_eq!(bank_posting.fingerprint.as_deref(), Some(original.fingerprint.as_str()));
    assert!(means_core::hashchain::verify(&conn, entity.id).unwrap().first_bad_seq.is_none());
}

#[tokio::test]
async fn tui_confirms_rule_posted_entries_without_rebuilding_them() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use means_core::{accounts, entities, imports, journal, model::*, rules, Db};
    use means_tui::app::{App, Modal, Screen};

    let server = start();
    let (entity_id, original, draft_id) = {
        let db = Db::open(&server.db).unwrap();
        let mut conn = db.conn();
        let entity = entities::create_entity(&mut conn, "TUI Review", "person", "US", "USD").unwrap();
        let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank", "Mercury"], "bank", "USD").unwrap();
        let expense = accounts::ensure_account(&conn, entity.id, AccountType::Expense, &["Domains"], "expense", "USD").unwrap();
        rules::save_rule(
            &conn,
            &Rule {
                id: 0,
                entity_id: entity.id,
                name: "Domains".into(),
                position: 0,
                enabled: true,
                conditions: vec![RuleCondition { field: "description".into(), op: "contains".into(), value: "NAMECHEAP".into() }],
                account_id: Some(expense.id),
                template_id: None,
                payee: String::new(),
                hits_count: 0,
                created_at: String::new(),
                tags: String::new(),
            },
        )
        .unwrap();
        let csv = "\"Date (UTC)\",\"Description\",\"Amount\",\"Status\",\"Transaction ID\"\n\"03-15-2026\",\"NAMECHEAP.COM\",\"-12.50\",\"Sent\",\"review-rule\"\n\"03-16-2026\",\"Unknown shop\",\"-8.00\",\"Sent\",\"review-draft\"\n";
        let imported = imports::run_import(&mut conn, imports::ImportRequest::new("mercury_csv", Some(bank.id), "review.csv", csv.as_bytes())).unwrap();
        let entry = journal::get_entry(&conn, imported.lines[0].journal_entry_id.unwrap()).unwrap();
        assert_eq!(entry.status, EntryStatus::Posted);
        assert!(entry.reviewed_at.is_none());
        (entity.id, entry, imported.lines[1].journal_entry_id.unwrap())
    };
    let mut app = App::new(&server.url);
    app.screen = Screen::Review;
    app.load_screen().await;
    assert!(app.status.is_empty(), "{}", app.status);
    assert_eq!(app.drafts.len(), 1);
    assert_eq!(app.drafts[0].id, draft_id);
    assert_eq!(app.unreviewed.len(), 1);
    assert_eq!(app.unreviewed[0].id, original.id);
    assert_eq!(app.info.as_ref().unwrap().unreviewed, 1);
    app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)).await;
    assert_eq!(app.review_unreviewed().unwrap().id, original.id);
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;
    assert_eq!(app.status, format!("confirmed #{}", original.id));
    assert!(matches!(app.modal, Modal::None));
    assert!(app.unreviewed.is_empty());
    assert_eq!(app.review_sel, 0, "selection is clamped after the last unreviewed row disappears");
    assert_eq!(app.review_draft().unwrap().id, draft_id);
    assert_eq!(app.info.as_ref().unwrap().unreviewed, 0);
    app.load_screen().await;
    assert!(app.unreviewed.is_empty(), "confirmed entries stay out of the queue after refresh");
    let db = Db::open(&server.db).unwrap();
    let conn = db.conn();
    let after = journal::get_entry(&conn, original.id).unwrap();
    assert!(after.reviewed_at.is_some());
    assert_eq!(after.status, EntryStatus::Posted);
    assert_eq!(serde_json::to_value(&after.postings).unwrap(), serde_json::to_value(&original.postings).unwrap());
    assert!(journal::get_entry(&conn, draft_id).unwrap().reviewed_at.is_none());
    assert!(means_core::hashchain::verify(&conn, entity_id).unwrap().first_bad_seq.is_none());
}

#[tokio::test]
async fn grpc_rejects_conflicting_posting_valuations_without_mutation() {
    let server = start();
    let mut client = MeansClient::connect(server.url.clone()).await.unwrap();
    let entity =
        client.create_entity(pb::CreateEntityRequest { name: "Valuation".into(), kind: "person".into(), country: "PT".into(), currency: "EUR".into() }).await.unwrap().into_inner().entity.unwrap();
    let mut account_ids = Vec::new();
    for (name, kind, commodity) in [("Bank", "asset", "EUR"), ("Food", "expense", "EUR"), ("Foreign", "asset", "USD")] {
        account_ids.push(
            client
                .create_account(pb::CreateAccountRequest { entity_id: entity.id, name: name.into(), r#type: kind.into(), commodity: commodity.into(), ..Default::default() })
                .await
                .unwrap()
                .into_inner()
                .account
                .unwrap()
                .id,
        );
    }
    let mut input = pb::JournalEntryInput {
        entity_id: entity.id,
        date: "2026-02-01".into(),
        status: "posted".into(),
        postings: vec![
            pb::PostingInput { account_id: account_ids[0], quantity: "-100".into(), amount: "-100".into(), ..Default::default() },
            pb::PostingInput { account_id: account_ids[1], ..Default::default() },
        ],
        ..Default::default()
    };
    let original = client.create_journal_entry(pb::CreateJournalEntryRequest { entry: Some(input.clone()) }).await.unwrap().into_inner().entry.unwrap();
    for (account, amount) in [(account_ids[0], "-1"), (account_ids[2], "1")] {
        input.postings[0].account_id = account;
        input.postings[0].amount = amount.into();
        let error = client.create_journal_entry(pb::CreateJournalEntryRequest { entry: Some(input.clone()) }).await.unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        let error = client.update_journal_entry(pb::UpdateJournalEntryRequest { id: original.id, entry: Some(input.clone()) }).await.unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        let after = client.get_journal_entry(pb::GetJournalEntryRequest { id: original.id }).await.unwrap().into_inner().entry.unwrap();
        assert_eq!(after, original);
    }
    let db = means_core::Db::open(&server.db).unwrap();
    let conn = db.conn();
    assert_eq!(means_core::journal::count_by_status(&conn, "posted").unwrap(), 1);
    assert!(means_core::hashchain::verify(&conn, entity.id).unwrap().first_bad_seq.is_none());
}

#[tokio::test]
async fn local_api_accepts_native_clients_and_rejects_browser_requests() {
    use prost::Message;
    let server = start();
    let http = reqwest::Client::new();
    let rpc = format!("{}/means.v1.Means/CreateEntity", server.url);
    let message = pb::CreateEntityRequest { name: "Browser".into(), kind: "person".into(), country: "PT".into(), currency: "EUR".into() }.encode_to_vec();
    let mut frame = vec![0];
    frame.extend_from_slice(&(message.len() as u32).to_be_bytes());
    frame.extend_from_slice(&message);
    for origin in [
        server.url.as_str(),
        "http://localhost:5173",
        "http://127.0.0.1:5173",
        "http://[::1]:5173",
        "https://untrusted.example",
        "null",
        "http://localhost:9999",
        "http://127.0.0.1.untrusted.example:5173",
    ] {
        let response = http.post(&rpc).header("origin", origin).header("content-type", "application/grpc-web+proto").body(frame.clone()).send().await.unwrap();
        assert_eq!(response.status(), 403, "{origin}");
        assert!(!response.headers().contains_key("access-control-allow-origin"));
        let response = http.get(format!("{}/means.v1.Means/GetStatus", server.url)).header("origin", origin).send().await.unwrap();
        assert_eq!(response.status(), 403);
        let response = http.request(reqwest::Method::OPTIONS, &rpc).header("origin", origin).header("access-control-request-method", "POST").send().await.unwrap();
        assert_eq!(response.status(), 403);
    }
    for host in ["untrusted.example", "localhost:9999"] {
        let response = http.get(&server.url).header("host", host).send().await.unwrap();
        assert_eq!(response.status(), 403);
    }
    let response = http.get(&server.url).header("sec-fetch-site", "cross-site").send().await.unwrap();
    assert_eq!(response.status(), 403);
    let mut client = MeansClient::connect(server.url.clone()).await.unwrap();
    assert_eq!(client.get_status(pb::GetStatusRequest {}).await.unwrap().into_inner().entities, 0);
    for content_type in ["application/grpc-web", "application/grpc-web+proto", "application/grpc-web-text+proto", "Application/Grpc-Web+Proto; charset=utf-8"] {
        let response = http.post(&rpc).header("content-type", content_type).body(frame.clone()).send().await.unwrap();
        assert_eq!(response.status(), 415, "{content_type}");
        assert!(!response.headers().contains_key("access-control-allow-origin"));
    }
    let response = http.post(&rpc).header("content-type", "application/grpc").header("x-grpc-web", "1").body(frame).send().await.unwrap();
    assert_eq!(response.status(), 415);
    for path in ["/", "/index.html", "/assets/app.js"] {
        let response = http.get(format!("{}{path}", server.url)).send().await.unwrap();
        assert_eq!(response.status(), 404, "{path}");
        assert!(!response.headers().contains_key("access-control-allow-origin"));
    }
    assert_eq!(client.get_status(pb::GetStatusRequest {}).await.unwrap().into_inner().entities, 0);
    client.create_entity(pb::CreateEntityRequest { name: "Native client".into(), kind: "person".into(), country: "PT".into(), currency: "EUR".into() }).await.unwrap();
    assert_eq!(client.get_status(pb::GetStatusRequest {}).await.unwrap().into_inner().entities, 1);
}

#[test]
fn serving_non_loopback_is_refused_before_opening_a_ledger() {
    for address in ["0.0.0.0:0", "[::]:0", "192.0.2.1:0"] {
        let db = std::env::temp_dir().join(format!("means-remote-refused-{}-{}.db", std::process::id(), address.replace(':', "_")));
        let mut child = Command::new(env!("CARGO_BIN_EXE_means")).args(["--db", db.to_str().unwrap(), "serve", "--listen", address]).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("non-loopback server was not refused");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let output = child.wait_with_output().unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("loopback"));
        assert!(!db.exists());
    }
}

#[tokio::test]
async fn tui_splits_a_category_by_filter_with_preview_and_confirmation() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use means_core::{accounts, entities, journal, model::*, Db};
    use means_tui::app::{App, Modal, Screen};
    let server = start();
    let (entity, source, target, ids) = {
        let db = Db::open(&server.db).unwrap();
        let mut conn = db.conn();
        let entity = entities::create_entity(&mut conn, "Split", "person", "PT", "EUR").unwrap();
        let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
        let source = accounts::ensure_account(&conn, entity.id, AccountType::Expense, &["Food"], "expense", "EUR").unwrap();
        let target = accounts::ensure_account(&conn, entity.id, AccountType::Expense, &["Delivery"], "expense", "EUR").unwrap();
        let mut ids = Vec::new();
        for payee in ["Deliveroo", "Lidl", "Delivery dinner"] {
            let mut input = EntryInput::new(entity.id, means_core::parse_date("2026-02-01").unwrap());
            input.status = EntryStatus::Posted;
            input.payee = payee.into();
            input.postings = vec![PostingInput::new(bank.id, (-10).into()), PostingInput::new(source.id, 10.into())];
            ids.push(journal::create_entry(&mut conn, input).unwrap().id);
        }
        (entity.id, source.id, target.id, ids)
    };
    let mut app = App::new(&server.url);
    app.screen = Screen::Accounts;
    app.load_screen().await;
    app.accounts_sel = app.accounts.iter().position(|a| a.id == source).unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;
    // The server must cover the entire account, not merely the TUI's loaded rows.
    app.ledger.as_mut().unwrap().rows.truncate(1);
    for confirm in [false, true] {
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE)).await;
        assert!(matches!(app.modal, Modal::Input { .. }));
        for ch in "deliver".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)).await;
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;
        if let Modal::Picker(picker, _) = &mut app.modal {
            picker.selected = picker.items.iter().position(|(id, _, _)| *id == target).unwrap();
        } else {
            panic!("expected category picker: {:?}", app.modal);
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;
        assert!(matches!(&app.modal, Modal::Confirm { text, .. } if text.contains("2 entries")));
        let db = Db::open(&server.db).unwrap();
        assert_eq!(journal::get_entry(&db.conn(), ids[0]).unwrap().postings[1].account_id, source, "preview must not move postings");
        app.handle_key(KeyEvent::new(KeyCode::Char(if confirm { 'y' } else { 'n' }), KeyModifiers::NONE)).await;
    }
    assert!(app.status.contains("moved 2 entries"), "{}", app.status);
    assert_eq!(app.ledger.as_ref().unwrap().rows.len(), 1);
    let db = Db::open(&server.db).unwrap();
    let conn = db.conn();
    for (index, id) in ids.iter().enumerate() {
        assert_eq!(journal::get_entry(&conn, *id).unwrap().postings[1].account_id, if index == 1 { source } else { target });
    }
    assert!(means_core::hashchain::verify(&conn, entity).unwrap().first_bad_seq.is_none());
    let output = Command::new(env!("CARGO_BIN_EXE_means")).args(["--db", server.db.to_str().unwrap(), "verify"]).output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("3 entries verified"));
}

#[tokio::test]
async fn tui_and_cli_link_refund_candidates_without_voiding_original() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use means_core::{accounts, entities, imports, journal, model::*, Db};
    use means_tui::app::{App, Modal, Screen};
    let server = start();
    let (bank, original, line, draft) = {
        let db = Db::open(&server.db).unwrap();
        let mut conn = db.conn();
        let entity = entities::create_entity(&mut conn, "Refund", "person", "PT", "EUR").unwrap();
        let bank = accounts::ensure_account(&conn, entity.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
        let expense = accounts::ensure_account(&conn, entity.id, AccountType::Expense, &["Shopping"], "expense", "EUR").unwrap();
        let mut input = EntryInput::new(entity.id, means_core::parse_date("2026-01-01").unwrap());
        input.payee = "Shop".into();
        input.postings = vec![PostingInput::new(bank.id, (-50).into()), PostingInput::new(expense.id, 50.into())];
        let original = journal::create_entry(&mut conn, input).unwrap();
        let bytes = b"Date,Amount,Description\n2026-02-01,50,Shop refund\n";
        let mapping = imports::CsvMapping {
            date_column: "Date".into(),
            date_format: "%Y-%m-%d".into(),
            amount_column: "Amount".into(),
            description_column: "Description".into(),
            currency: "EUR".into(),
            ..Default::default()
        };
        let mut request = imports::ImportRequest::new("generic_csv", Some(bank.id), "credit.csv", bytes);
        request.mapping = Some(&mapping);
        let out = imports::run_import(&mut conn, request).unwrap();
        (bank.id, original.id, out.lines[0].id, out.lines[0].journal_entry_id.unwrap())
    };
    let output = Command::new(env!("CARGO_BIN_EXE_means")).args(["--db", server.db.to_str().unwrap(), "refund", "candidates", &line.to_string(), "--json"]).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let candidates: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(candidates[0]["id"], original);
    let mut rpc = MeansClient::connect(server.url.clone()).await.unwrap();
    let suggested = rpc.suggest_matches(pb::SuggestMatchesRequest { line_id: line }).await.unwrap().into_inner();
    assert_eq!(suggested.refund_candidates[0].id, original);
    let mut app = App::new(&server.url);
    app.screen = Screen::Review;
    app.load_screen().await;
    app.review_sel = app.drafts.iter().position(|e| e.id == draft).unwrap();
    for confirm in [false, true] {
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE)).await;
        if let Modal::Picker(picker, _) = &mut app.modal {
            picker.selected = picker.items.iter().position(|(id, _, _)| *id == original).unwrap();
        } else {
            panic!("expected refund candidates: {:?}", app.modal);
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;
        assert!(matches!(&app.modal, Modal::Confirm { text, .. } if text.contains("book value")));
        app.handle_key(KeyEvent::new(KeyCode::Char(if confirm { 'y' } else { 'n' }), KeyModifiers::NONE)).await;
        if !confirm {
            let db = Db::open(&server.db).unwrap();
            assert_eq!(journal::get_entry(&db.conn(), draft).unwrap().status, EntryStatus::Draft);
        }
    }
    assert!(app.status.contains("Posted refund"), "{}", app.status);
    let refund = {
        let db = Db::open(&server.db).unwrap();
        let conn = db.conn();
        let credit = imports::get_line(&conn, line).unwrap();
        let refund = journal::get_entry(&conn, credit.journal_entry_id.unwrap()).unwrap();
        assert_eq!(refund.refund_of_id, Some(original));
        assert_eq!(refund.postings.iter().find(|p| p.account_id == bank).unwrap().quantity.minor(), 5000);
        assert_eq!(journal::get_entry(&conn, original).unwrap().status, EntryStatus::Posted);
        refund
    };
    let response = rpc.get_journal_entry(pb::GetJournalEntryRequest { id: refund.id }).await.unwrap().into_inner().entry.unwrap();
    assert_eq!(response.refund_of_id, original);
    let retry = Command::new(env!("CARGO_BIN_EXE_means")).args(["--db", server.db.to_str().unwrap(), "refund", "link", &line.to_string(), "--original", &original.to_string()]).output().unwrap();
    assert!(!retry.status.success(), "a second link must not post another refund");
}

#[tokio::test]
async fn class_expense_report_filters_tags_over_rpc_and_cli() {
    use means_core::model::{AccountType, EntryInput, PostingInput};
    use means_core::{accounts, entities, journal, tags, Db};
    use rust_decimal::Decimal;
    let server = start();
    let db = Db::open(&server.db).unwrap();
    let e = {
        let mut conn = db.conn();
        let e = entities::create_entity(&mut conn, "Personal", "person", "PT", "EUR").unwrap();
        let bank = accounts::ensure_account(&conn, e.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
        let expense = accounts::ensure_account(&conn, e.id, AccountType::Expense, &["Food"], "expense", "EUR").unwrap();
        accounts::update_account(&conn, expense.id, accounts::AccountUpdate { class: Some("committed".into()), ..Default::default() }).unwrap();
        let mut input = EntryInput::new(e.id, "2026-09-01".parse().unwrap());
        input.postings = vec![PostingInput::new(bank.id, Decimal::new(-1234, 2)), PostingInput::new(expense.id, Decimal::new(1234, 2))];
        let entry = journal::create_entry(&mut conn, input).unwrap();
        tags::set_tags(&mut conn, entry.id, &tags::parse("trip:porto with:friends").unwrap()).unwrap();
        e
    };
    let mut client = MeansClient::connect(server.url.clone()).await.unwrap();
    let request = pb::ExpensesByClassRequest { entity_id: e.id, from: "2026-09-01".into(), to: "2026-09-01".into(), tag: "trip:porto".into() };
    let report = client.expenses_by_class(request.clone()).await.unwrap().into_inner();
    assert_eq!(report.total, "12.34");
    assert_eq!(report.currency, "EUR");
    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.rows[0].class, "committed");
    assert_eq!(report.rows[0].amount, "12.34");
    let empty = client.expenses_by_class(pb::ExpensesByClassRequest { tag: "trip:other".into(), ..request.clone() }).await.unwrap().into_inner();
    assert_eq!(empty.total, "0");
    assert!(empty.rows.is_empty());
    let invalid = client.expenses_by_class(pb::ExpensesByClassRequest { from: "2026-09-02".into(), ..request }).await.unwrap_err();
    assert_eq!(invalid.code(), tonic::Code::InvalidArgument);
    let output = Command::new(env!("CARGO_BIN_EXE_means"))
        .arg("--db")
        .arg(&server.db)
        .args(["report", "expenses", "--entity", &e.id.to_string(), "--from", "2026-09-01", "--to", "2026-09-01", "--tag", "trip:porto", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["total"], serde_json::json!({"minor":"1234", "commodity":"EUR", "precision":2}));
    assert_eq!(value["rows"][0]["class"], "committed");
    let output = Command::new(env!("CARGO_BIN_EXE_means")).arg("--db").arg(&server.db).args(["report", "expenses", "--entity", &e.id.to_string()]).output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout).unwrap().contains("committed\t12.34"));
}

#[tokio::test]
async fn posting_and_descriptive_edit_roundtrips_preserve_valuation_provenance() {
    use means_core::model::*;
    use means_core::{accounts, entities, journal, rates, Db};
    let server = start();
    let date = chrono::NaiveDate::from_ymd_opt(2026, 2, 1).unwrap();
    let db = Db::open(&server.db).unwrap();
    let (entity, bank, food, entry) = {
        let mut c = db.conn();
        let entity = entities::create_entity(&mut c, "FX", "person", "PT", "EUR").unwrap();
        let bank = accounts::ensure_account(&c, entity.id, AccountType::Asset, &["USD"], "bank", "USD").unwrap();
        let food = accounts::ensure_account(&c, entity.id, AccountType::Expense, &["Food"], "expense", "EUR").unwrap();
        let mut input = EntryInput::new(entity.id, date);
        input.status = EntryStatus::Draft;
        input.postings = vec![PostingInput::new(bank.id, "-10".parse().unwrap()).meta("original", serde_json::json!({"amount":"-10","currency":"USD"})), PostingInput::balancing(food.id)];
        let entry = journal::create_entry(&mut c, input).unwrap();
        (entity, bank, food, entry)
    };
    let mut client = MeansClient::connect(server.url.clone()).await.unwrap();
    let mut e = client.get_journal_entry(pb::GetJournalEntryRequest { id: entry.id }).await.unwrap().into_inner().entry.unwrap();
    let original_metadata = e.postings[0].metadata.clone();
    // Native clients resend the unchanged booked amounts when posting or editing.
    for description in ["Categorized before rates arrived", "Edited description"] {
        let postings = e
            .postings
            .iter()
            .map(|p| pb::PostingInput {
                account_id: p.account_id,
                quantity: p.quantity.clone(),
                amount: p.amount.clone(),
                memo: p.memo.clone(),
                metadata: p.metadata.clone(),
                external_id: p.external_id.clone(),
                fingerprint: p.fingerprint.clone(),
            })
            .collect();
        e = client
            .update_journal_entry(pb::UpdateJournalEntryRequest {
                id: e.id,
                entry: Some(pb::JournalEntryInput { entity_id: entity.id, date: e.date.clone(), status: "posted".into(), description: description.into(), postings, ..Default::default() }),
            })
            .await
            .unwrap()
            .into_inner()
            .entry
            .unwrap();
        assert_eq!(journal::get_entry(&db.conn(), e.id).unwrap().postings[0].rate_source, "missing");
        assert_eq!(e.postings[0].metadata, original_metadata);
    }
    {
        let mut c = db.conn();
        rates::set_price(&c, "USD", "EUR", date, "0.9".parse().unwrap(), "manual").unwrap();
        assert_eq!(rates::revalue_missing(&mut c).unwrap().fixed, 1);
    }
    let valued = client.get_journal_entry(pb::GetJournalEntryRequest { id: entry.id }).await.unwrap().into_inner().entry.unwrap();
    assert_eq!(valued.postings.iter().find(|p| p.account_id == bank.id).unwrap().amount, "-9");
    assert_eq!(valued.postings.iter().find(|p| p.account_id == food.id).unwrap().amount, "9");
}

#[tokio::test]
async fn tui_csv_mapping_previews_without_writes_and_imports_the_inspected_bytes() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use means_tui::app::{App, InputTarget, Modal, Screen};
    let server = start();
    let mut rpc = MeansClient::connect(server.url.clone()).await.unwrap();
    let entity = rpc.create_entity(pb::CreateEntityRequest { name: "Personal".into(), currency: "EUR".into(), ..Default::default() }).await.unwrap().into_inner().entity.unwrap();
    let bank = rpc
        .create_account(pb::CreateAccountRequest { entity_id: entity.id, name: "Bank".into(), r#type: "asset".into(), commodity: "EUR".into(), ..Default::default() })
        .await
        .unwrap()
        .into_inner()
        .account
        .unwrap();
    let file = server.db.with_extension("csv");
    std::fs::write(&file, "Booked;Out;In;Memo;Ref\n19/09/2026;12,34;;Coffee;one\n20/09/2026;;5,00;Refund;two\n").unwrap();
    let mut app = App::new(&server.url);
    app.screen = Screen::Imports;
    app.load_screen().await;
    app.accounts = vec![bank.clone()];
    app.modal = Modal::Input { title: "Import".into(), value: file.display().to_string(), target: InputTarget::ImportPath };
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    app.handle_key(enter).await;
    app.handle_key(enter).await;
    assert!(matches!(&app.modal, Modal::Import(f) if !f.reviewing));
    for (index, value) in [(0, ";"), (2, "Booked"), (3, "%d/%m/%Y"), (5, "Out"), (6, "In"), (7, "Memo"), (8, "missing"), (11, ",")] {
        if let Modal::Import(f) = &mut app.modal {
            f.selected = index;
        } else {
            panic!("missing import form");
        }
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)).await;
        for c in value.chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)).await;
        }
    }
    let preview = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
    app.handle_key(preview).await;
    assert!(matches!(&app.modal, Modal::Import(f) if !f.reviewing && f.message.contains("not found")));
    if let Modal::Import(f) = &mut app.modal {
        f.fields[8] = "Ref".into();
    }
    app.handle_key(preview).await;
    assert!(matches!(&app.modal, Modal::Import(f) if f.reviewing && f.lines().iter().any(|s| s.contains("-12.34"))));
    assert!(rpc.list_imports(pb::ListImportsRequest { account_id: 0, limit: 100 }).await.unwrap().into_inner().imports.is_empty());
    // Returning to the form invalidates the preview. Typing y in a field cannot commit.
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)).await;
    app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)).await;
    assert!(rpc.list_imports(pb::ListImportsRequest { account_id: 0, limit: 100 }).await.unwrap().into_inner().imports.is_empty());
    if let Modal::Import(f) = &mut app.modal {
        f.fields[11] = ",".into();
    }
    app.handle_key(preview).await;
    // A later filesystem change must not substitute unreviewed content.
    std::fs::write(&file, "not the inspected statement").unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)).await;
    assert!(matches!(&app.modal, Modal::Import(f) if f.finished), "{:?}", app.modal);
    app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)).await;
    let imports = rpc.list_imports(pb::ListImportsRequest { account_id: 0, limit: 100 }).await.unwrap().into_inner().imports;
    assert_eq!(imports.len(), 1);
    assert_eq!(imports[0].account_id, bank.id);
    let lines = rpc.get_import(pb::GetImportRequest { id: imports[0].id }).await.unwrap().into_inner().lines;
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].amount, "-12.34");
    assert_eq!(lines[1].amount, "5");
    assert_eq!(lines[0].reference, "one");
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)).await;
    assert_eq!(app.imports.len(), 1);
    std::fs::remove_file(file).unwrap();
}

#[tokio::test]
async fn tui_account_tracker_mapping_selects_entities_types_and_skipped_accounts() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use means_tui::app::{App, InputTarget, Modal, Screen};
    let server = start();
    let mut rpc = MeansClient::connect(server.url.clone()).await.unwrap();
    let mut entities = Vec::new();
    for name in ["Personal", "Business"] {
        entities.push(rpc.create_entity(pb::CreateEntityRequest { name: name.into(), currency: "EUR".into(), ..Default::default() }).await.unwrap().into_inner().entity.unwrap());
    }
    let file = server.db.with_extension("atb");
    let accounts: String = [(1, "Bank"), (2, "Card"), (3, "Skip me")]
        .iter()
        .map(|(id, name)| format!("<dict><key>id</key><integer>{id}</integer><key>name</key><string>{name}</string><key>code</key><string>EUR</string></dict>"))
        .collect();
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>$objects</key><array/><key>$top</key><dict><key>root</key><dict><key>code</key><string>EUR</string><key>accounts</key><array>{accounts}</array></dict></dict></dict></plist>"#
    );
    std::fs::write(&file, xml).unwrap();
    let mut app = App::new(&server.url);
    app.screen = Screen::Imports;
    app.entities = entities.clone();
    app.entity_idx = 1;
    app.modal = Modal::Input { title: "Import".into(), value: file.display().to_string(), target: InputTarget::ImportPath };
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    app.handle_key(key(KeyCode::Enter)).await;
    assert!(matches!(&app.modal, Modal::Import(f) if f.mappings.len() == 3), "{:?}", app.modal);
    app.handle_key(key(KeyCode::Down)).await;
    app.handle_key(key(KeyCode::Char('e'))).await;
    // Cycle the second account from automatic to liability/card.
    for _ in 0..6 {
        app.handle_key(key(KeyCode::Char('t'))).await;
    }
    app.handle_key(key(KeyCode::Down)).await;
    app.handle_key(key(KeyCode::Char('x'))).await;
    app.handle_key(key(KeyCode::Char('r'))).await;
    app.handle_key(key(KeyCode::Char('s'))).await;
    app.handle_key(key(KeyCode::Enter)).await;
    assert!(rpc.list_imports(pb::ListImportsRequest { account_id: 0, limit: 100 }).await.unwrap().into_inner().imports.is_empty());
    // Back preserves the mapping and options, without writing.
    app.handle_key(key(KeyCode::Esc)).await;
    assert!(matches!(&app.modal, Modal::Import(f) if !f.reviewing && !f.expand_recurring && !f.schedules && f.mappings[2].skip));
    app.handle_key(key(KeyCode::Enter)).await;
    app.handle_key(key(KeyCode::Char('y'))).await;
    assert!(matches!(&app.modal, Modal::Import(f) if f.finished), "{:?}", app.modal);
    let accounts = rpc.list_accounts(pb::ListAccountsRequest { entity_id: 0, include_closed: false, as_of: String::new(), r#type: String::new() }).await.unwrap().into_inner().accounts;
    assert_eq!(accounts.iter().find(|a| a.name == "Bank").unwrap().entity_id, entities[1].id);
    let card = accounts.iter().find(|a| a.name == "Card").unwrap();
    assert_eq!(card.entity_id, entities[0].id);
    assert_eq!(card.r#type, "liability");
    assert_eq!(card.subtype, "card");
    assert!(!accounts.iter().any(|a| a.name == "Skip me"));
    assert!(matches!(&app.modal, Modal::Import(f) if f.lines().iter().any(|s| s.starts_with("OK"))));
    let imports = rpc.list_imports(pb::ListImportsRequest { account_id: 0, limit: 100 }).await.unwrap().into_inner().imports;
    let options: serde_json::Value = serde_json::from_str(&imports[0].options).unwrap();
    assert_eq!(options["default_entity_id"], entities[1].id);
    std::fs::remove_file(file).unwrap();
}

#[tokio::test]
async fn tui_csv_preview_handles_presets_and_explicit_signed_amount_options() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use means_tui::{client::Client, import::ImportForm};
    let server = start();
    let mut rpc = MeansClient::connect(server.url.clone()).await.unwrap();
    let entity = rpc.create_entity(pb::CreateEntityRequest { name: "Personal".into(), currency: "EUR".into(), ..Default::default() }).await.unwrap().into_inner().entity.unwrap();
    let bank = rpc
        .create_account(pb::CreateAccountRequest { entity_id: entity.id, name: "Bank".into(), r#type: "asset".into(), commodity: "EUR".into(), ..Default::default() })
        .await
        .unwrap()
        .into_inner()
        .account
        .unwrap();
    let mut client = Client::new(&server.url);
    let mut preset = ImportForm::new("n26.csv".into(), include_bytes!("../../means-core/tests/fixtures/n26_booking_date.csv").to_vec(), bank.id, "Bank EUR".into());
    preset.preview(&mut client).await;
    assert!(preset.reviewing, "{}", preset.message);
    assert!(preset.mapping().unwrap().is_none());
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    preset.key(esc, &mut client).await;
    assert!(preset.key(esc, &mut client).await);
    assert!(rpc.list_imports(pb::ListImportsRequest { account_id: 0, limit: 100 }).await.unwrap().into_inner().imports.is_empty());

    let mut form = ImportForm::new("custom.csv".into(), b"Exported statement\nWhen,Value,Text,Other,Running\n2026-09-19,12.34,Coffee,Shop,87.66\n".to_vec(), bank.id, "Bank EUR".into());
    for (i, v) in [(0, ","), (1, "2"), (2, "When"), (3, "%Y-%m-%d"), (4, "Value"), (7, "Text"), (9, "Running"), (11, "."), (12, "true"), (13, "EUR"), (14, "Other"), (15, "true")] {
        form.fields[i] = v.into();
    }
    form.preview(&mut client).await;
    assert!(form.reviewing, "{}", form.message);
    form.key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE), &mut client).await;
    assert!(form.finished, "{}", form.message);
    let imports = rpc.list_imports(pb::ListImportsRequest { account_id: 0, limit: 100 }).await.unwrap().into_inner().imports;
    assert_eq!(imports.len(), 1);
    let lines = rpc.get_import(pb::GetImportRequest { id: imports[0].id }).await.unwrap().into_inner().lines;
    assert_eq!(lines[0].amount, "-12.34");
    assert_eq!(lines[0].balance_after, "87.66");
    assert!(lines[0].description.contains("Coffee"));
    assert!(lines[0].description.contains("Shop"));
}

#[tokio::test]
async fn budgets_and_tag_reports_work_through_cli_rpc_and_tui() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use means_core::{accounts, budgets, entities, journal, tags, AccountType, Db, EntryInput, PostingInput};
    use means_tui::app::{App, Modal, ReportKind, Screen};
    let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
    let server = start();
    let db = Db::open(&server.db).unwrap();
    let (entity, food) = {
        let mut c = db.conn();
        let e = entities::create_entity(&mut c, "Personal", "person", "PT", "EUR").unwrap();
        let bank = accounts::ensure_account(&c, e.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap();
        let food = accounts::ensure_account(&c, e.id, AccountType::Expense, &["Food"], "expense", "EUR").unwrap();
        accounts::update_account(&c, food.id, accounts::AccountUpdate { class: Some("committed".into()), ..Default::default() }).unwrap();
        let mut i = EntryInput::new(e.id, "2026-09-01".parse().unwrap());
        i.postings = vec![PostingInput::new(bank.id, "-12.34".parse().unwrap()), PostingInput::new(food.id, "12.34".parse().unwrap())];
        let entry = journal::create_entry(&mut c, i).unwrap();
        tags::set_tags(&mut c, entry.id, &tags::parse("trip:porto with:friends").unwrap()).unwrap();
        (e.id, food.id)
    };
    let output = Command::new(env!("CARGO_BIN_EXE_means"))
        .arg("--db")
        .arg(&server.db)
        .args(["budget", "create", "--entity", &entity.to_string(), "--name", "Trip", "--tag", "trip:porto", "--amount", "100", "--from", "2026-09-01", "--to", "2026-11-30"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let id = value["id"].as_i64().unwrap();
    assert_eq!(value["limit"]["minor"], "10000");
    let mut rpc = MeansClient::connect(server.url.clone()).await.unwrap();
    let rows = rpc.list_budgets(pb::ListBudgetsRequest { entity_id: entity, on: "2026-10-01".into(), tag: "#TRIP:PORTO".into() }).await.unwrap().into_inner().rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].spent, "12.34");
    assert_eq!(rows[0].remaining, "87.66");
    let report = rpc.expenses_by_tag(pb::ExpensesByTagRequest { entity_id: entity, from: "2026-09-01".into(), to: "2026-09-01".into(), tag: String::new() }).await.unwrap().into_inner();
    assert_eq!(report.total, "12.34");
    assert!(report.overlapping);
    assert_eq!(report.rows.len(), 2);
    assert!(report.rows.iter().all(|r| r.amount == "12.34"));
    let output = Command::new(env!("CARGO_BIN_EXE_means")).arg("--db").arg(&server.db).args(["report", "expenses", "--entity", &entity.to_string(), "--group-by", "tag", "--json"]).output().unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["total"]["minor"], "1234");
    assert_eq!(value["overlapping"], true);
    let mut app = App::new(&server.url);
    app.screen = Screen::Reports;
    app.load_screen().await;
    app.handle_key(key(KeyCode::Char('u'))).await;
    assert_eq!(app.budgets.len(), 1);
    app.handle_key(key(KeyCode::Enter)).await;
    let Modal::Budget(form) = &mut app.modal else { panic!("budget editor missing") };
    form.budget.amount = "50".into();
    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)).await;
    assert!(matches!(app.modal, Modal::None));
    assert_eq!(app.budgets[0].remaining, "37.66");
    app.handle_key(key(KeyCode::Char('n'))).await;
    let Modal::Budget(form) = &mut app.modal else { panic!("budget editor missing") };
    form.field = 2;
    form.query = "Food".into();
    form.choice = 0;
    form.budget.name = "Food limit".into();
    form.budget.amount = "20".into();
    form.budget.starts_on = "2026-09-01".into();
    form.budget.ends_on = "2026-09-30".into();
    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)).await;
    assert!(matches!(app.modal, Modal::None));
    assert_eq!(app.budgets.len(), 2);
    let created = app.budgets.iter().find_map(|r| r.budget.as_ref().filter(|b| b.name == "Food limit")).unwrap();
    assert_eq!(created.account_id, food);
    let created_id = created.id;
    app.handle_key(key(KeyCode::Char('v'))).await;
    assert!(app.budgets_by_tag);
    app.report_sel = app.budgets.iter().position(|r| r.budget.as_ref().unwrap().id == created_id).unwrap();
    app.handle_key(key(KeyCode::Char('d'))).await;
    app.handle_key(key(KeyCode::Char('n'))).await;
    assert_eq!(app.budgets.len(), 2);
    app.handle_key(key(KeyCode::Char('d'))).await;
    app.handle_key(key(KeyCode::Char('y'))).await;
    assert_eq!(app.budgets.len(), 1);
    app.report_from = "2026-09-01".into();
    app.report_to = "2026-09-01".into();
    app.handle_key(key(KeyCode::Char('g'))).await;
    assert_eq!(app.report_kind, ReportKind::ExpenseTag);
    assert_eq!(app.report.as_ref().unwrap().net, "12.34");
    app.handle_key(key(KeyCode::Char('c'))).await;
    assert_eq!(app.report.as_ref().unwrap().rows[0].name, "committed");
    app.report_to = "invalid".into();
    app.handle_key(key(KeyCode::Char('g'))).await;
    assert!(app.report.is_none(), "do not display stale totals after error");
    let mut b = rows[0].budget.clone().unwrap();
    b.currency = "USD".into();
    assert_eq!(rpc.save_budget(pb::SaveBudgetRequest { budget: Some(b) }).await.unwrap_err().code(), tonic::Code::InvalidArgument);
    let output = Command::new(env!("CARGO_BIN_EXE_means")).arg("--db").arg(&server.db).args(["budget", "list", "--entity", &entity.to_string(), "--on", "2026-10-01", "--json"]).output().unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value[0]["remaining"]["minor"], "3766");
    rpc.delete_budget(pb::DeleteBudgetRequest { id }).await.unwrap();
    assert!(budgets::list(&db.conn(), entity, None, None).unwrap().is_empty());
    assert_eq!(db.conn().query_row("SELECT COUNT(*) FROM journal_entries", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
}

#[tokio::test]
async fn connection_rpc_and_tui_map_accounts_cutoffs_and_expose_missing_environment() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use means_core::{accounts, connections, entities, AccountType, Db};
    use means_tui::app::{App, InputTarget, Modal, Screen};
    let server = start_with_env(&[("PLUGGY_CLIENT_ID", ""), ("PLUGGY_CLIENT_SECRET", ""), ("MERCURY_TOKEN", ""), ("ENABLE_BANKING_APP_ID", ""), ("ENABLE_BANKING_KEY_FILE", "")]);
    let db = Db::open(&server.db).unwrap();
    let (id, bank) = {
        let mut c = db.conn();
        let e = entities::create_entity(&mut c, "Personal", "person", "PT", "EUR").unwrap();
        let bank = accounts::ensure_account(&c, e.id, AccountType::Asset, &["Bank"], "bank", "EUR").unwrap().id;
        connections::discover(
            &c,
            "pluggy",
            "11111111-1111-4111-8111-111111111111",
            &[connections::ConnectionAccount {
                provider_account_id: "22222222-2222-4222-8222-222222222222".into(),
                provider_type: "BANK".into(),
                name: "Checking".into(),
                currency: "EUR".into(),
                ..Default::default()
            }],
        )
        .unwrap();
        (connections::list(&c).unwrap()[0].id, bank)
    };
    let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
    let mut app = App::new(&server.url);
    app.screen = Screen::Connections;
    app.load_screen().await;
    assert_eq!(app.connections.providers.len(), 6);
    assert!(app.connections.providers.iter().all(|p| !p.configured));
    assert_eq!(app.connections.accounts.len(), 1);
    app.handle_key(key(KeyCode::Enter)).await;
    assert!(matches!(app.modal, Modal::Connection(_)));
    app.handle_key(key(KeyCode::Esc)).await;
    app.connections_sel = app.connections.providers.len();
    app.handle_key(key(KeyCode::Enter)).await;
    let Modal::Picker(p, _) = &app.modal else { panic!("expected destination picker") };
    assert_eq!(p.current().unwrap().0, bank);
    app.handle_key(key(KeyCode::Enter)).await;
    assert_eq!(app.connections.accounts[0].account_id, bank);
    app.handle_key(key(KeyCode::Char('f'))).await;
    let Modal::Input { value, target, .. } = &mut app.modal else { panic!("expected cutoff editor") };
    assert_eq!(*target, InputTarget::ConnectionCutoff(id));
    *value = "2026-09-01".into();
    app.handle_key(key(KeyCode::Enter)).await;
    assert_eq!(app.connections.accounts[0].booked_from, "2026-09-01");
    app.handle_key(key(KeyCode::Char('p'))).await;
    assert!(matches!(app.modal, Modal::Confirm { .. }));
    app.handle_key(key(KeyCode::Char('n'))).await;
    assert!(app.connections.jobs.is_empty());
    app.handle_key(key(KeyCode::Char('p'))).await;
    app.handle_key(key(KeyCode::Char('y'))).await;
    assert!(app.status.contains("PLUGGY_CLIENT_ID"), "{}", app.status);
    assert!(app.connections.jobs.is_empty());
    let mut rpc = MeansClient::connect(server.url.clone()).await.unwrap();
    assert_eq!(
        rpc.start_connection_job(pb::StartConnectionJobRequest { provider: "unknown".into(), operation: "pull".into(), connection_id: id, ..Default::default() }).await.unwrap_err().code(),
        tonic::Code::FailedPrecondition
    );
    assert_eq!(rpc.configure_connection(pb::ConfigureConnectionRequest { id, account_id: bank, booked_from: "bad".into() }).await.unwrap_err().code(), tonic::Code::InvalidArgument);
    assert_eq!(connections::get(&db.conn(), id).unwrap().booked_from, "2026-09-01");
    app.handle_key(key(KeyCode::Char('x'))).await;
    app.handle_key(key(KeyCode::Char('y'))).await;
    assert_eq!(app.connections.accounts[0].account_id, 0);
    app.handle_key(key(KeyCode::Char('v'))).await;
    assert_eq!(app.screen, Screen::Review);
    app.screen = Screen::Connections;
    app.handle_key(key(KeyCode::Char('i'))).await;
    assert_eq!(app.screen, Screen::Imports);
}

#[tokio::test]
async fn payee_tui_preview_confirmation_and_reports_use_the_rpc_contract() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use means_tui::{
        app::{App, Modal, Screen},
        payees::{Form, Operation},
    };
    let server = start();
    let mut client = MeansClient::connect(server.url.clone()).await.unwrap();
    let entity =
        client.create_entity(pb::CreateEntityRequest { name: "Payees".into(), kind: "person".into(), country: "PT".into(), currency: "EUR".into() }).await.unwrap().into_inner().entity.unwrap();
    let mut app = App::new(&server.url);
    app.load_screen().await;
    let mut form = Form::new(pb::Payee { entity_id: entity.id, name: "Coffee".into(), active: true, aliases: vec!["cafe central".into()], ..Default::default() }, Operation::Save);
    let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
    // Apply is inert until a preview is obtained.
    form.key(key('y'), &mut app.client).await;
    assert!(!form.saved);
    form.key(key('p'), &mut app.client).await;
    assert!(form.preview.is_some(), "{}", form.message);
    assert!(!form.saved);
    assert!(client.list_payees(pb::ListPayeesRequest { entity_id: entity.id }).await.unwrap().into_inner().payees.is_empty());
    form.key(key('y'), &mut app.client).await;
    assert!(form.saved, "{}", form.message);
    app.screen = Screen::Payees;
    app.load_screen().await;
    assert_eq!(app.payees.len(), 1);
    let saved = app.payees[0].clone();
    let bank = client
        .create_account(pb::CreateAccountRequest { entity_id: entity.id, name: "Bank".into(), r#type: "asset".into(), commodity: "EUR".into(), ..Default::default() })
        .await
        .unwrap()
        .into_inner()
        .account
        .unwrap();
    let food = client
        .create_account(pb::CreateAccountRequest { entity_id: entity.id, name: "Food".into(), r#type: "expense".into(), commodity: "EUR".into(), ..Default::default() })
        .await
        .unwrap()
        .into_inner()
        .account
        .unwrap();
    let entry = client
        .create_simple_entry(pb::CreateSimpleEntryRequest {
            entry: Some(pb::SimpleEntryInput {
                entity_id: entity.id,
                date: "2026-02-01".into(),
                kind: "expense".into(),
                account_id: bank.id,
                contra_account_id: food.id,
                quantity: "10".into(),
                payee: "Coffee".into(),
                ..Default::default()
            }),
        })
        .await
        .unwrap()
        .into_inner()
        .entry
        .unwrap();
    assert_eq!(entry.payee_id, saved.id);
    assert_eq!(entry.payee, "Coffee");
    let mut rename = Form::new(pb::Payee { name: "Coffee renamed".into(), ..saved }, Operation::Save);
    rename.key(key('p'), &mut app.client).await;
    rename.key(key('y'), &mut app.client).await;
    assert!(rename.saved, "{}", rename.message);
    let fetched = client.get_journal_entry(pb::GetJournalEntryRequest { id: entry.id }).await.unwrap().into_inner().entry.unwrap();
    assert_eq!(fetched.payee, "Coffee");
    assert_eq!(fetched.display_payee, "Coffee renamed");
    app.screen = Screen::Reports;
    app.report_kind = means_tui::app::ReportKind::ExpensePayee;
    app.report_from.clear();
    app.report_to.clear();
    app.load_screen().await;
    assert_eq!(app.report.as_ref().unwrap().rows[0].name, "Coffee renamed");
    let mut history = Form::new(pb::Payee { entity_id: entity.id, ..Default::default() }, Operation::Backfill);
    history.key(key('p'), &mut app.client).await;
    assert!(history.preview.is_some());
    app.modal = Modal::Payee(Box::new(history));
    assert!(matches!(app.modal, Modal::Payee(_)));
}

#[tokio::test]
async fn review_split_editor_posts_existing_draft_and_capture_reuses_it() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use means_core::{accounts, entities, imports, AccountType, Db};
    use means_tui::app::{App, Modal, Screen};
    let s = start();
    let (entity, bank, food, household, entry_id, bank_id) = {
        let db = Db::open(&s.db).unwrap();
        let mut c = db.conn();
        let e = entities::create_entity(&mut c, "Personal", "person", "US", "USD").unwrap();
        let bank = accounts::ensure_account(&c, e.id, AccountType::Asset, &["Bank"], "bank", "USD").unwrap();
        let food = accounts::ensure_account(&c, e.id, AccountType::Expense, &["Groceries"], "expense", "USD").unwrap();
        let household = accounts::ensure_account(&c, e.id, AccountType::Expense, &["Household"], "expense", "USD").unwrap();
        let csv = b"\"Date (UTC)\",\"Description\",\"Amount\",\"Status\",\"Transaction ID\"\n\"09-19-2026\",\"Shop\",\"-90.50\",\"Sent\",\"test-split\"\n";
        imports::run_import(&mut c, imports::ImportRequest::new("mercury_csv", Some(bank.id), "test.csv", csv)).unwrap();
        let line = imports::list_lines(&c, Some(bank.id), "created", None, 10).unwrap().remove(0);
        accounts::close_account(&c, bank.id, false).unwrap();
        (e.id, bank.id, food.id, household.id, line.journal_entry_id.unwrap(), line.posting_id.unwrap())
    };
    let mut app = App::new(&s.url);
    app.screen = Screen::Review;
    app.load_screen().await;
    assert!(app.accounts.iter().all(|a| a.id != bank), "closed bank is not in the picker cache");
    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::SHIFT)).await;
    let Modal::Split(form) = &mut app.modal else { panic!("split editor not opened: {}", app.status) };
    assert!(matches!(form.target, means_tui::split::Target::Review(id) if id == entry_id));
    assert_eq!(form.currency, "USD");
    form.rows = vec![(food, "Groceries".into(), "72.40".into()), (household, "Household".into(), "".into())];
    // Explicit submission posts through the shared server operation.
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;
    assert!(matches!(app.modal, Modal::PostingPreview(_)), "{}", app.status);
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;
    assert!(matches!(app.modal, Modal::PostingPreview(_)));
    app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE)).await;
    assert!(matches!(app.modal, Modal::Split(_)));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;
    app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)).await;
    assert!(matches!(app.modal, Modal::None), "{}", app.status);
    let mut client = MeansClient::connect(s.url.clone()).await.unwrap();
    let posted = client.get_journal_entry(pb::GetJournalEntryRequest { id: entry_id }).await.unwrap().into_inner().entry.unwrap();
    assert_eq!(posted.status, "posted");
    assert_eq!(posted.postings.iter().find(|p| p.account_id == bank).unwrap().id, bank_id);
    assert_eq!(posted.postings.iter().find(|p| p.account_id == household).unwrap().quantity.parse::<rust_decimal::Decimal>().unwrap(), "18.10".parse::<rust_decimal::Decimal>().unwrap());
    // Already-posted imported transactions use the same editor and preserve bank evidence.
    app.modal = Modal::EntryDetail(Box::new(posted));
    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE)).await;
    let Modal::Split(form) = &mut app.modal else { panic!("{}", app.status) };
    form.rows = vec![(food, "Groceries".into(), "30".into()), (household, "Household".into(), "30".into()), (food, "Groceries".into(), "".into())];
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;
    assert!(matches!(app.modal, Modal::PostingPreview(_)), "{}", app.status);
    app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)).await;
    let split = client.get_journal_entry(pb::GetJournalEntryRequest { id: entry_id }).await.unwrap().into_inner().entry.unwrap();
    assert_eq!(split.postings.len(), 4);
    assert_eq!(split.postings.iter().find(|p| p.account_id == bank).unwrap().id, bank_id);
    // Capture uses the same editor, but Apply only saves the form until Ctrl-S.
    accounts::close_account(&Db::open(&s.db).unwrap().conn(), bank, true).unwrap();
    app.screen = Screen::Capture;
    app.load_screen().await;
    app.capture.account = Some((bank, "Bank".into()));
    app.capture.amount = "90.50".into();
    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE)).await;
    let Modal::Split(form) = &mut app.modal else { panic!("capture split editor not opened") };
    form.rows = vec![(food, "Groceries".into(), "72.40".into()), (household, "Household".into(), "".into())];
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;
    assert_eq!(app.capture.splits.len(), 2);
    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)).await;
    assert!(matches!(app.modal, Modal::PostingPreview(_)), "{}", app.status);
    app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)).await;
    assert!(app.capture.splits.is_empty(), "{}", app.status);
    assert_eq!(client.list_journal_entries(pb::ListJournalEntriesRequest { entity_id: entity, ..Default::default() }).await.unwrap().into_inner().entries.len(), 2);
}
