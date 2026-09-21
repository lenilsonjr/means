//! Live smoke test against a running engine: seeds a small set of books and drives the app's
//! loaders and actions without a terminal. Skipped unless MEANS_TUI_SERVER is set
//! (e.g. MEANS_TUI_SERVER=http://127.0.0.1:7771).

use means_proto::v1 as pb;
use means_tui::app::{App, Screen};
use means_tui::client::Client;

#[tokio::test]
async fn loads_every_screen_against_a_live_engine() {
    let Ok(server) = std::env::var("MEANS_TUI_SERVER") else {
        eprintln!("MEANS_TUI_SERVER not set, skipping");
        return;
    };
    let mut client = Client::new(&server);
    // Seed: one entity with a bank account and a category, if the ledger is empty.
    let entities = client.list_entities(pb::ListEntitiesRequest { include_archived: false }).await.expect("engine reachable");
    let entity = match entities.entities.into_iter().find(|e| e.name == "TUI Demo") {
        Some(e) => e,
        None => client.create_entity(pb::CreateEntityRequest { name: "TUI Demo".into(), kind: "person".into(), country: "PT".into(), currency: "EUR".into() }).await.unwrap().entity.unwrap(),
    };
    let accounts = client.list_accounts(pb::ListAccountsRequest { entity_id: entity.id, include_closed: false, as_of: String::new(), r#type: String::new() }).await.unwrap().accounts;
    let bank = match accounts.iter().find(|a| a.name == "N26") {
        Some(a) => a.clone(),
        None => client
            .create_account(pb::CreateAccountRequest {
                entity_id: entity.id,
                parent_id: 0,
                code: String::new(),
                name: "N26".into(),
                r#type: "asset".into(),
                subtype: "bank".into(),
                commodity: "EUR".into(),
                placeholder: false,
                in_net_worth: true,
                credit_limit: String::new(),
                statement_day: 0,
                due_day: 0,
                opening_balance: "1000".into(),
                opening_date: "2026-01-01".into(),
                notes: String::new(),
            })
            .await
            .unwrap()
            .account
            .unwrap(),
    };
    let food = match accounts.iter().find(|a| a.name == "Food") {
        Some(a) => a.clone(),
        None => client
            .create_account(pb::CreateAccountRequest {
                entity_id: entity.id,
                parent_id: 0,
                code: String::new(),
                name: "Food".into(),
                r#type: "expense".into(),
                subtype: "expense".into(),
                commodity: "EUR".into(),
                placeholder: false,
                in_net_worth: true,
                credit_limit: String::new(),
                statement_day: 0,
                due_day: 0,
                opening_balance: String::new(),
                opening_date: String::new(),
                notes: String::new(),
            })
            .await
            .unwrap()
            .account
            .unwrap(),
    };
    client
        .create_simple_entry(pb::CreateSimpleEntryRequest {
            entry: Some(pb::SimpleEntryInput {
                entity_id: entity.id,
                date: "2026-08-29".into(),
                kind: "expense".into(),
                account_id: bank.id,
                contra_account_id: food.id,
                quantity: "3.20".into(),
                contra_quantity: String::new(),
                payee: "Cafe Central".into(),
                notes: String::new(),
                splits: vec![],
                status: "posted".into(),
                fee: String::new(),
                fee_account_id: 0,
            }),
        })
        .await
        .unwrap();
    // A draft to review.
    client
        .create_simple_entry(pb::CreateSimpleEntryRequest {
            entry: Some(pb::SimpleEntryInput {
                entity_id: entity.id,
                date: "2026-08-28".into(),
                kind: "expense".into(),
                account_id: bank.id,
                contra_account_id: food.id,
                quantity: "12.00".into(),
                contra_quantity: String::new(),
                payee: "Draft dinner".into(),
                notes: String::new(),
                splits: vec![],
                status: "draft".into(),
                fee: String::new(),
                fee_account_id: 0,
            }),
        })
        .await
        .unwrap();

    let mut app = App::new(&server);
    for screen in Screen::ALL {
        app.screen = screen;
        app.load_screen().await;
        assert!(!app.status.starts_with("error"), "{:?}: {}", screen, app.status);
    }
    assert!(!app.entities.is_empty());
    assert!(app.info.as_ref().map(|i| i.onboarded).unwrap_or(false));
    assert!(app.net_worth.is_some());
    assert!(!app.recent.is_empty());
    if let Some(i) = app.entities.iter().position(|e| e.id == entity.id) {
        app.entity_idx = i;
    }
    app.screen = Screen::Accounts;
    app.load_screen().await;
    assert!(app.accounts.iter().any(|a| a.name == "N26"));
    app.screen = Screen::Review;
    app.load_screen().await;
    assert!(app.drafts.iter().any(|d| d.payee == "Draft dinner"));
    app.screen = Screen::Reports;
    app.load_screen().await;
    let r = app.report.as_ref().expect("trial balance");
    assert_eq!(r.total_debit, r.total_credit);
}
