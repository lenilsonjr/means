//! Terminal UI for means: an amber terminal over the engine's gRPC contract.

pub mod app;
pub mod budget;
pub mod client;
pub mod connections;
pub mod fmt;
pub mod import;
pub mod payees;
pub mod sharing;
pub mod sort;
pub mod split;
pub mod ui;

use std::io::Stdout;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::App;

/// Restores the terminal even when the app panics.
struct Guard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

/// Run the terminal UI against an engine at `server` (e.g. http://127.0.0.1:7770).
pub async fn run(server: &str) -> Result<()> {
    let mut app = App::new(server);
    app.load_screen().await;
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let mut guard = Guard { terminal };
    loop {
        guard.terminal.draw(|f| ui::draw(f, &mut app))?;
        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat => app.handle_key(key).await,
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
        app.tick_connections().await;
        if app.pending_load {
            // Show the new screen (and "loading") before waiting on the engine, so a slow call never freezes the tab switch.
            app.pending_load = false;
            app.status = "loading…".into();
            guard.terminal.draw(|f| ui::draw(f, &mut app))?;
            app.load_screen().await;
        }
        if app.quit {
            if let Some(owned) = &app.replica_return {
                let mut owner = client::Client::new(owned);
                let _ = owner.sharing(means_proto::v1::SharingRequest { operation: "close-view".into(), arguments: vec![app.client.addr().to_string()], ..Default::default() }).await;
            }
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use means_proto::v1 as pb;
    use ratatui::backend::TestBackend;

    #[test]
    fn received_vault_banner_survives_expiry_and_sharing_is_a_tenth_screen() {
        let mut app = App::new("http://127.0.0.1:7770");
        app.screen = app::Screen::Sharing;
        app.info = Some(pb::GetStatusResponse { replica_status: "expired — received copy remains readable".into(), ..Default::default() });
        let mut terminal = Terminal::new(TestBackend::new(160, 30)).unwrap();
        terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
        let text = screen_text(&terminal);
        assert!(text.contains("RECEIVED:"));
        assert!(text.contains("expired"));
        assert!(text.contains("0Sharing"));
    }
    fn screen_text(terminal: &Terminal<TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn demo_app() -> App {
        let mut app = App::new("http://127.0.0.1:1");
        app.entities = vec![
            pb::Entity { id: 1, name: "Personal".into(), kind: "person".into(), country: "PT".into(), currency: "EUR".into(), lock_date: String::new(), archived: false },
            pb::Entity { id: 2, name: "LLC".into(), kind: "company".into(), country: "US".into(), currency: "USD".into(), lock_date: String::new(), archived: false },
        ];
        app.precisions = [("EUR".to_string(), 2usize), ("USD".to_string(), 2usize), ("BTC".to_string(), 8usize)].into_iter().collect();
        app.info = Some(pb::GetStatusResponse {
            replica_status: String::new(),
            version: "0.1.0".into(),
            db_path: "/tmp/x.db".into(),
            entities: 2,
            accounts: 9,
            journal_entries: 42,
            drafts: 3,
            unmatched_lines: 0,
            prices: 1700,
            postings_without_rate: 0,
            onboarded: true,
            latest_price_date: "2026-08-28".into(),
            today: "2026-08-29".into(),
            pending_imports: 1,
            unreviewed: 2,
        });
        app.net_worth = Some(pb::NetWorthResponse {
            by_entity: vec![pb::ReportRow {
                account_id: 1,
                path: "Personal".into(),
                name: "Personal".into(),
                r#type: "equity".into(),
                depth: 0,
                placeholder: false,
                commodity: "EUR".into(),
                quantity: "12345.67".into(),
                amount: "12345.67".into(),
                debit: "15000".into(),
                credit: "2654.33".into(),
                market_value: String::new(),
            }],
            total: "12345.67".into(),
            currency: "EUR".into(),
            by_account: vec![],
        });
        let posting = |id: i64, path: &str, q: &str, amt: &str| pb::Posting {
            id,
            journal_entry_id: 1,
            account_id: id,
            account_path: path.into(),
            commodity: "EUR".into(),
            quantity: q.into(),
            amount: amt.into(),
            rate: String::new(),
            memo: String::new(),
            external_id: String::new(),
            fingerprint: String::new(),
            reconciled_at: String::new(),
            metadata: String::new(),
            position: 0,
        };
        let entry = |id: i64, date: &str, payee: &str, status: &str, kind: &str| pb::JournalEntry {
            id,
            entity_id: 1,
            date: date.into(),
            payee: payee.into(),
            payee_id: 0,
            display_payee: payee.into(),
            description: String::new(),
            notes: String::new(),
            status: status.into(),
            reverses_id: 0,
            reversed_by_id: 0,
            refund_of_id: 0,
            counterpart_id: 0,
            template_id: 0,
            origin: "capture".into(),
            posted_at: String::new(),
            created_at: String::new(),
            postings: vec![posting(1, "Assets:Bank:N26", "-3.2", "-3.2"), posting(2, "Expenses:Food", "3.2", "3.2")],
            kind: kind.into(),
            amount_functional: "3.2".into(),
            statement_line_id: 0,
            tags: vec![],
            reviewed_at: String::new(),
        };
        app.recent = vec![entry(1, "2026-08-29", "Cafe Central", "posted", "expense"), entry(2, "2026-08-28", "Example Client", "posted", "income")];
        app.entries = app.recent.clone();
        app.entries_total = 2;
        app.drafts = vec![entry(3, "2026-08-27", "LIDL SAGT DANKE", "draft", "expense")];
        app
    }

    #[test]
    fn import_coverage_warning_is_visible_for_the_selected_import() {
        let mut app = demo_app();
        app.screen = app::Screen::Imports;
        app.imports =
            vec![pb::Import { id: 27, status: "done".into(), options: r#"{"coverage_warning":"Possible coverage overlap: older split bookings need review."}"#.into(), ..Default::default() }];
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
        let text = screen_text(&terminal);
        assert!(text.contains("done !"), "{text}");
        assert!(text.contains("POSSIBLE IMPORT OVERLAP"), "{text}");
        assert!(text.contains("older split bookings need review"), "{text}");
    }

    #[tokio::test]
    async fn overview_clears_previous_vault_before_refresh_and_hides_mismatched_cache() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = demo_app();
        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE)).await;
        assert_eq!(app.entity_id(), 2);
        assert!(app.net_worth.is_none());
        assert!(app.recent.is_empty());
        app.net_worth = demo_app().net_worth;
        app.recent = demo_app().recent;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
        let text = screen_text(&terminal);
        assert!(!text.contains("12,345.67"), "{text}");
        assert!(!text.contains("Cafe Central"), "{text}");
        assert!(text.contains("Net worth unavailable"), "{text}");
    }

    #[test]
    fn overview_renders_at_80x24() {
        let mut app = demo_app();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
        let text = screen_text(&terminal);
        assert!(text.contains("MEANS"), "{text}");
        assert!(text.contains("SELECTED VAULT"), "{text}");
        assert!(text.contains("Net worth"), "{text}");
        assert!(text.contains("EUR"), "{text}");
        assert!(text.contains("Personal"), "{text}");
        assert!(text.contains("12,345.67"), "{text}");
        assert!(!text.contains("TOTAL"), "{text}");
        assert!(!text.contains("LLC"), "{text}");
        assert!(text.contains("RECENT ENTRIES"), "{text}");
        assert!(text.contains("Cafe Central"), "{text}");
        assert!(text.contains("drafts 3"), "{text}");
    }

    #[test]
    fn shared_split_editor_renders_total_remainder_and_explicit_action() {
        for (target, action) in [(split::Target::Capture, "use splits"), (split::Target::Review(42), "preview posting")] {
            for width in [80, 120] {
                let mut app = demo_app();
                app.modal = app::Modal::Split(Box::new(split::Editor::new(
                    target,
                    "90.50".into(),
                    "USD".into(),
                    vec![(1, "Groceries".into(), "72.40".into()), (2, "Household".into(), "".into())],
                    vec![],
                    "expense".into(),
                )));
                let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
                terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
                let text = screen_text(&terminal);
                for expected in ["SPLIT POSTING", "90.50 USD", "18.10 (remainder)", action, "Esc cancel"] {
                    assert!(text.contains(expected), "missing {expected}: {text}");
                }
            }
        }
    }

    #[test]
    fn transaction_modal_shows_full_accounts_and_only_real_links() {
        for width in [80, 120] {
            let mut app = demo_app();
            let mut entry = app.entries[0].clone();
            entry.postings = vec![pb::Posting {
                account_path: "Expenses:Shopping:Clothing, shoes and accessories".into(),
                commodity: "EUR".into(),
                quantity: "74.25".into(),
                amount: "74.25".into(),
                ..Default::default()
            }];
            app.modal = app::Modal::EntryDetail(Box::new(entry.clone()));
            let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
            terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
            let text = screen_text(&terminal);
            assert!(text.contains("Expenses:Shopping:Clothing, shoes and accessories"), "{text}");
            assert!(text.contains("Debit 74.25 EUR"), "{text}");
            assert!(!text.contains("#0"), "{text}");
            assert!(!text.contains("m re-categorize"), "{text}");
            app.modal =
                app::Modal::PostingPreview(Box::new(app::PostingPreview { request: Default::default(), entry, back: Box::new(app::Modal::None), remaining: vec![], scroll: 0, error: String::new() }));
            terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
            let text = screen_text(&terminal);
            assert!(text.contains("nothing saved"), "{text}");
            assert!(text.contains("y confirm"), "{text}");
        }
    }

    #[test]
    fn journal_and_review_render() {
        let mut app = demo_app();
        app.screen = app::Screen::Journal;
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
        let text = screen_text(&terminal);
        assert!(text.contains("JOURNAL"), "{text}");
        assert!(text.contains("Example Client"), "{text}");
        assert!(text.contains("Assets:Bank:N26"), "{text}");
        app.screen = app::Screen::Review;
        terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
        let text = screen_text(&terminal);
        assert!(text.contains("REVIEW"), "{text}");
        assert!(text.contains("LIDL SAGT DANKE"), "{text}");
        assert!(text.contains("Expenses:Food"), "{text}");
        // Entry detail popup.
        app.modal = app::Modal::EntryDetail(Box::new(app.entries[0].clone()));
        terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
        let text = screen_text(&terminal);
        assert!(text.contains("POSTINGS"), "{text}");
        assert!(text.contains("3.20"), "{text}");
    }

    #[test]
    fn unreviewed_entries_show_confirmation_at_wide_and_narrow_sizes() {
        let mut app = demo_app();
        app.screen = app::Screen::Review;
        app.unreviewed = vec![app.entries[0].clone()];
        app.review_sel = app.drafts.len();
        for width in [80, 120] {
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
            let text = screen_text(&terminal);
            assert!(text.contains("1 UNREVIEWED"), "{text}");
            assert!(text.contains("unreviewed"), "{text}");
            assert!(text.contains("Enter confirm"), "{text}");
            assert!(text.contains("Cafe Central"), "{text}");
            assert!(!text.contains("Enter post"), "{text}");
        }
    }

    #[test]
    fn capture_and_reports_render_narrow() {
        let mut app = demo_app();
        app.screen = app::Screen::Capture;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
        let text = screen_text(&terminal);
        assert!(text.contains("CAPTURE"), "{text}");
        assert!(text.contains("expense"), "{text}");
        app.screen = app::Screen::Reports;
        app.report = Some(pb::ReportResponse {
            rows: vec![pb::ReportRow {
                account_id: 1,
                path: "Assets:Bank:N26".into(),
                name: "N26".into(),
                r#type: "asset".into(),
                depth: 1,
                placeholder: false,
                commodity: "EUR".into(),
                quantity: "100".into(),
                amount: "100".into(),
                debit: "100".into(),
                credit: "0".into(),
                market_value: String::new(),
            }],
            total_debit: "100".into(),
            total_credit: "100".into(),
            net: "0".into(),
            currency: "EUR".into(),
            summary: vec![],
        });
        terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
        let text = screen_text(&terminal);
        assert!(text.contains("TRIAL BALANCE"), "{text}");
        assert!(text.contains("difference 0.00"), "{text}");
    }
    #[test]
    fn budget_and_overlapping_tag_reports_render_at_terminal_sizes() {
        let mut app = demo_app();
        app.screen = app::Screen::Reports;
        app.report_kind = app::ReportKind::Budgets;
        let budget = pb::Budget {
            id: 1,
            entity_id: 1,
            name: "Porto".into(),
            scope: "tag".into(),
            tag: "trip:porto".into(),
            amount: "600".into(),
            currency: "EUR".into(),
            starts_on: "2026-09-01".into(),
            ends_on: "2026-11-30".into(),
            ..Default::default()
        };
        app.budgets = vec![pb::BudgetProgress { budget: Some(budget.clone()), spent: "123.45".into(), remaining: "476.55".into(), group: "mixed".into(), target: "trip:porto".into() }];
        for width in [80, 120] {
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
            let text = screen_text(&terminal);
            assert!(text.contains("Porto"), "{text}");
            assert!(text.contains("476.55"), "{text}");
            assert!(text.contains("overlapping budgets"), "{text}");
            assert!(text.contains("2026-11-30"), "{text}");
            app.modal = app::Modal::Budget(Box::new(budget::Form::new(1, "EUR".into(), vec![], Some(budget.clone()))));
            terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
            let text = screen_text(&terminal);
            assert!(text.contains("Ctrl-S save"), "{text}");
            assert!(text.contains("trip:porto"), "{text}");
            app.modal = app::Modal::None;
        }
        app.report_kind = app::ReportKind::ExpenseTag;
        app.report = Some(pb::ReportResponse {
            currency: "EUR".into(),
            net: "10".into(),
            rows: vec![
                pb::ReportRow { name: "trip:porto".into(), amount: "10".into(), ..Default::default() },
                pb::ReportRow { name: "with:friends".into(), amount: "10".into(), ..Default::default() },
            ],
            ..Default::default()
        });
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
        let text = screen_text(&terminal);
        assert!(text.contains("OVERLAPPING"), "{text}");
        assert!(text.contains("each posting once) 10.00"), "{text}");
        assert!(text.contains("with:friends"), "{text}");
    }
    #[test]
    fn connections_setup_and_pull_confirmation_fit_narrow_terminals() {
        let mut app = demo_app();
        app.screen = app::Screen::Connections;
        app.connections = pb::ListConnectionsResponse {
            providers: vec![pb::ConnectionProvider { id: "pluggy".into(), name: "Pluggy / MeuPluggy".into(), configured: false, requirements: "PLUGGY_CLIENT_ID, PLUGGY_CLIENT_SECRET".into() }],
            accounts: vec![pb::BankConnection { id: 1, channel: "pluggy".into(), name: "Checking".into(), currency: "EUR".into(), booked_from: "2026-09-01".into(), ..Default::default() }],
            ..Default::default()
        };
        for width in [80, 120] {
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
            let text = screen_text(&terminal);
            assert!(text.contains("Connections"), "{text}");
            assert!(text.contains("PLUGGY_CLIENT_SECRET"), "{text}");
            app.connections_sel = 1;
            terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
            let text = screen_text(&terminal);
            assert!(text.contains("2026-09-01"), "{text}");
            assert!(text.contains("Unmapped"), "{text}");
            app.modal = app::Modal::Connection(Box::new(connections::Form::new("pluggy")));
            terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
            let text = screen_text(&terminal);
            assert!(text.contains("Ctrl-O"), "{text}");
            assert!(text.contains("Item ID"), "{text}");
            app.modal =
                app::Modal::Confirm { text: "Pull this account for Checking? Booking cutoff: 2026-09-01. Unmapped files wait in Imports. (y/n)".into(), action: app::ConfirmAction::PullConnection(1) };
            terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
            let text = screen_text(&terminal);
            assert!(text.contains("2026-09-01"), "{text}");
            assert!(text.contains("(y/n)"), "{text}");
            app.modal = app::Modal::None;
            app.connections_sel = 0;
        }
    }
    #[tokio::test]
    async fn bank_connections_offer_account_cutoff_and_account_only_confirmation() {
        for channel in ["enable_banking", "mercury", "wise", "inter_pj"] {
            use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
            let mut app = demo_app();
            app.screen = app::Screen::Connections;
            app.connections = pb::ListConnectionsResponse {
                accounts: vec![pb::BankConnection { id: 42, channel: channel.into(), name: "Bank main".into(), booked_from: "2026-08-30".into(), ..Default::default() }],
                ..Default::default()
            };
            app.connections_sel = 0;
            app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE)).await;
            let app::Modal::Input { value, target, .. } = &app.modal else { panic!("expected cutoff editor") };
            assert_eq!(value, "2026-08-30");
            assert_eq!(*target, app::InputTarget::ConnectionCutoff(42));
            app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)).await;
            app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE)).await;
            let app::Modal::Confirm { text, .. } = &app.modal else { panic!("expected confirmation") };
            assert!(text.contains("this account"));
            assert!(text.contains("2026-08-30"));
            assert!(!text.contains("all accounts"));
        }
    }
}
