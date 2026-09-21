//! Rendering: an amber phosphor terminal. Everything is drawn from `&mut App`.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;

use crate::app::{App, Modal, ReportKind, Screen, CAPTURE_FIELDS, KINDS};
use crate::fmt::{indent, money, sign, truncate};

pub const AMBER: Color = Color::Rgb(245, 184, 46);
pub const DIM: Color = Color::Rgb(150, 116, 40);
pub const FAINT: Color = Color::Rgb(90, 72, 30);
pub const GREEN: Color = Color::Rgb(95, 242, 138);
pub const RED: Color = Color::Rgb(255, 92, 92);
pub const INK: Color = Color::Rgb(12, 11, 8);

pub fn amber() -> Style {
    Style::default().fg(AMBER)
}
pub fn dim() -> Style {
    Style::default().fg(DIM)
}
pub fn faint() -> Style {
    Style::default().fg(FAINT)
}
pub fn selected() -> Style {
    Style::default().fg(INK).bg(AMBER).add_modifier(Modifier::BOLD)
}
pub fn signed(s: &str) -> Style {
    match sign(s) {
        -1 => Style::default().fg(RED),
        1 => Style::default().fg(GREEN),
        _ => dim(),
    }
}

fn block(title: &str) -> Block<'static> {
    Block::bordered().border_type(BorderType::Plain).border_style(dim()).title(Line::from(vec![Span::styled(format!(" {} ", title.to_uppercase()), amber().add_modifier(Modifier::BOLD))]))
}

fn header(cells: &[&str]) -> Row<'static> {
    Row::new(cells.iter().map(|c| Cell::from(c.to_uppercase())).collect::<Vec<_>>()).style(dim())
}

fn right(s: String) -> Cell<'static> {
    Cell::from(Line::from(s).right_aligned())
}

fn centered(area: Rect, pct_x: u16, pct_y: u16) -> Rect {
    let w = area.width * pct_x / 100;
    let h = area.height * pct_y / 100;
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

fn fixed_centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(1), Constraint::Length(1)]).split(area);
    draw_top(f, app, chunks[0]);
    match app.screen {
        Screen::Overview => draw_overview(f, app, chunks[1]),
        Screen::Accounts => draw_accounts(f, app, chunks[1]),
        Screen::Journal => draw_journal(f, app, chunks[1]),
        Screen::Review => draw_review(f, app, chunks[1]),
        Screen::Capture => draw_capture(f, app, chunks[1]),
        Screen::Imports => draw_imports(f, app, chunks[1]),
        Screen::Reports => draw_reports(f, app, chunks[1]),
        Screen::Connections => draw_connections(f, app, chunks[1]),
        Screen::Sharing => {
            let mut lines = app.sharing.lines.clone();
            lines.push("Choose an action (j/k, Enter). Full fingerprints can be copied from the public identity file.".into());
            for (i, a) in app.sharing.actions.iter().enumerate() {
                lines.push(format!("{} {} · {}", if i == app.sharing_sel { ">" } else { " " }, a.name, a.description));
            }
            let offset = app.sharing_scroll;
            f.render_widget(Paragraph::new(lines.join("\n")).scroll((offset.min(u16::MAX as usize) as u16, 0)).block(block("Sharing · offline encrypted files")), chunks[1]);
        }
        Screen::Payees => {
            let rows = app
                .payees
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    Line::from(Span::styled(
                        format!("{} #{} {} [{}] · {}", if i == app.payee_sel { ">" } else { " " }, p.id, p.name, if p.active { "active" } else { "archived" }, p.aliases.join("; ")),
                        if i == app.payee_sel { selected() } else { amber() },
                    ))
                })
                .collect::<Vec<_>>();
            let offset = app.payee_sel.saturating_sub(chunks[1].height.saturating_sub(3) as usize);
            f.render_widget(Paragraph::new(rows).scroll((offset.min(u16::MAX as usize) as u16, 0)).block(block("Payees · current entity")), chunks[1]);
        }
    }
    draw_keys(f, app, chunks[2]);
    draw_status(f, app, chunks[3]);
    draw_modal(f, app, area);
}

fn draw_top(f: &mut Frame, app: &App, area: Rect) {
    let entity = app.entity().map(|e| format!("{} ({})", e.name, e.currency)).unwrap_or_else(|| "no entity".into());
    let mut spans = vec![Span::styled(" MEANS ", selected()), Span::styled(format!(" {entity} "), amber())];
    spans.push(Span::styled(format!("· {}{} ", (app.screen.index() + 1) % 10, app.screen.title()), amber()));
    if let Some(i) = &app.info {
        if !i.replica_status.is_empty() {
            spans.push(Span::styled(format!(" RECEIVED: {} · Esc back ", i.replica_status), selected()));
        }
    }

    if let Some(i) = &app.info {
        let inbox = if i.pending_imports > 0 { format!(" · inbox {}", i.pending_imports) } else { String::new() };
        spans.push(Span::styled(
            format!("│ drafts {} · unmatched {}{} · rates {} ", i.drafts, i.unmatched_lines, inbox, if i.latest_price_date.is_empty() { "none" } else { &i.latest_price_date }),
            dim(),
        ));
    }
    spans.push(Span::styled("│ ", faint()));
    for (i, s) in Screen::ALL.iter().enumerate().filter(|_| area.width >= 150) {
        let label = format!("{}{} ", (i + 1) % 10, s.title());
        if *s == app.screen {
            spans.push(Span::styled(label, amber().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)));
        } else {
            spans.push(Span::styled(label, dim()));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_keys(f: &mut Frame, app: &App, area: Rect) {
    let keys: &str = match &app.modal {
        Modal::EntryDetail(_) => "s split · Esc close · j/k scroll",
        Modal::PostingPreview(_) => "y confirm · n/Esc edit · j/k scroll",
        Modal::Split(_) => "Split editor · Esc back",
        _ => match app.screen {
            Screen::Overview => "1-9/0 screens · e entity · r refresh · ? help · q quit",
            Screen::Accounts => {
                if app.ledger.is_some() {
                    "z sort · j/k · Enter entry · m re-categorize · S split entry · s split by filter · Esc back"
                } else {
                    "z sort · j/k · Enter ledger · n new · R rename · c code · C class · m move · M merge · x close"
                }
            }
            Screen::Journal => "z sort · j/k · Enter detail · t tag · a/d/p/v filter · / search · e entity",
            Screen::Review if app.review_unreviewed().is_some() => "S split · z sort · j/k · Enter confirm · o open · t tag · f refund",
            Screen::Review => "S split · z sort · Enter preview · b batch payee · u rule · t tag · o open · f refund · x del · s skip",
            Screen::Capture => "Tab/↓ field · Enter pick/submit · ←/→ kind · s split · Ctrl-S submit · Esc clear",
            Screen::Sharing => "j/k action · PgUp/PgDn scroll · Enter open · Esc return · r refresh",
            Screen::Payees => "n new · Enter edit/archive · l link entry · m merge · h history preview",
            Screen::Connections => "n setup · Enter map · p pull · f cutoff · o status · v review · i imports",
            Screen::Imports => "j/k move · Enter account for a pending file · i import file · r refresh · e entity",
            Screen::Reports if app.report_kind == ReportKind::Budgets => "n new · Enter edit · d delete · v class/tag · / filter · c/g/p expenses",
            Screen::Reports if matches!(app.report_kind, ReportKind::ExpenseClass | ReportKind::ExpenseTag | ReportKind::ExpensePayee) => "z sort · c/g/p expenses · f/o dates · / tag filter",
            Screen::Reports => "t/i/b statements · c/g/p expenses · u budgets · f/o dates · / tag",
        },
    };
    f.render_widget(Paragraph::new(Line::from(Span::styled(format!(" {keys}"), dim()))), area);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let style = if app.status.starts_with("error") { Style::default().fg(RED) } else { amber() };
    let text = if app.status.is_empty() { format!(" {}", app.client.addr()) } else { format!(" {}", app.status) };
    f.render_widget(Paragraph::new(Line::from(Span::styled(truncate(&text, area.width as usize), style))), area);
}

fn draw_overview(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::vertical([Constraint::Length(8), Constraint::Min(3)]).split(area);
    let top = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).split(rows[0]);
    let vault = app.entity().map(|e| e.name.as_str()).unwrap_or("No vault selected");
    let mut worth = vec![Line::from(Span::styled(vault, amber().add_modifier(Modifier::BOLD)))];
    if let Some(n) = app.net_worth.as_ref().filter(|n| n.by_entity.len() == 1 && n.by_entity[0].account_id == app.entity_id()) {
        let precision = app.precision(&n.currency);
        if let Some(row) = n.by_entity.first() {
            for (label, value) in [("Assets", row.debit.as_str()), ("Liabilities", row.credit.as_str()), ("Net worth", n.total.as_str())] {
                worth.push(Line::from(vec![Span::styled(format!("{label:<13}"), dim()), Span::styled(format!("{} {}", money(value, precision), n.currency), amber())]));
            }
        }
    } else {
        worth.push(Line::from(Span::styled("Net worth unavailable", dim())));
    }
    f.render_widget(Paragraph::new(worth).block(block("selected vault")), top[0]);
    // Status.
    let mut lines: Vec<Line> = Vec::new();
    if let Some(i) = &app.info {
        let kv = |k: &str, v: String| Line::from(vec![Span::styled(format!("{k:<22}"), dim()), Span::styled(v, amber())]);
        lines.push(kv("entities", i.entities.to_string()));
        lines.push(kv("accounts", i.accounts.to_string()));
        lines.push(kv("journal entries", i.journal_entries.to_string()));
        lines.push(kv("drafts to review", i.drafts.to_string()));
        lines.push(kv("posted to confirm", i.unreviewed.to_string()));
        lines.push(kv("unmatched lines", i.unmatched_lines.to_string()));
        lines.push(kv("prices (latest)", format!("{} ({})", i.prices, if i.latest_price_date.is_empty() { "none" } else { &i.latest_price_date })));
        if i.postings_without_rate > 0 {
            lines.push(Line::from(Span::styled(format!("{} postings booked without a rate", i.postings_without_rate), Style::default().fg(RED))));
        }
    } else {
        lines.push(Line::from(Span::styled("not connected", Style::default().fg(RED))));
    }
    f.render_widget(Paragraph::new(lines).block(block("all vaults · status")), top[1]);
    // Recent entries.
    let fp = app.functional_precision();
    let recent: Vec<Row> = app
        .recent
        .iter()
        .filter(|entry| entry.entity_id == app.entity_id())
        .map(|e| {
            let entity = app.entities.iter().find(|x| x.id == e.entity_id).map(|x| x.name.clone()).unwrap_or_default();
            Row::new(vec![
                Cell::from(e.date.clone()),
                Cell::from(truncate(&entity, 12)),
                Cell::from(e.kind.clone()).style(dim()),
                Cell::from(truncate(payee_display(&e.display_payee, &e.payee, &e.description), 40)),
                right(money(&e.amount_functional, fp)),
                Cell::from(e.status.clone()).style(status_style(&e.status)),
            ])
        })
        .collect();
    let table = Table::new(recent, [Constraint::Length(10), Constraint::Length(12), Constraint::Length(9), Constraint::Min(20), Constraint::Length(14), Constraint::Length(7)])
        .header(header(&["date", "entity", "kind", "payee", "amount", "status"]))
        .block(block("selected vault · recent entries"))
        .column_spacing(1);
    f.render_widget(table, rows[1]);
}

fn status_style(s: &str) -> Style {
    match s {
        "draft" => Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        "void" => faint(),
        _ => dim(),
    }
}

fn draw_accounts(f: &mut Frame, app: &mut App, area: Rect) {
    if let Some(l) = app.ledger.as_ref() {
        let prec = app.precision(&l.commodity);
        let rows: Vec<Row> = l
            .rows
            .iter()
            .map(|r| {
                Row::new(vec![
                    Cell::from(r.date.clone()),
                    Cell::from(truncate(payee_display(&r.display_payee, &r.payee, &r.description), 30)),
                    Cell::from(truncate(&r.contra_path, 28)).style(dim()),
                    right(money(&r.debit, prec)),
                    right(money(&r.credit, prec)),
                    right(money(&r.running_balance, prec)).style(signed(&r.running_balance)),
                    Cell::from(if r.reconciled {
                        "✓"
                    } else if r.status == "draft" {
                        "d"
                    } else {
                        " "
                    })
                    .style(dim()),
                ])
            })
            .collect();
        let title =
            format!("{} · {} · {} · latest {} rows · opening {} · closing {}", l.account.path, l.commodity, app.ledger_sort.label(), l.rows.len(), money(&l.opening, prec), money(&l.closing, prec));
        let table =
            Table::new(rows, [Constraint::Length(10), Constraint::Min(16), Constraint::Length(28), Constraint::Length(13), Constraint::Length(13), Constraint::Length(15), Constraint::Length(1)])
                .header(header(&["date", "payee", "contra", "debit", "credit", "balance", ""]))
                .block(block(&title))
                .row_highlight_style(selected())
                .column_spacing(1);
        let mut state = TableState::default().with_selected(Some(l.selected)).with_offset(l.selected.saturating_sub(area.height.saturating_sub(4) as usize / 2));
        f.render_stateful_widget(table, area, &mut state);
        return;
    }
    let fp = app.functional_precision();
    let functional = app.entity().map(|e| e.currency.clone()).unwrap_or_default();
    let rows: Vec<Row> = app
        .accounts
        .iter()
        .map(|a| {
            let prec = app.precision(&a.commodity);
            let name_style = if a.placeholder { dim().add_modifier(Modifier::BOLD) } else { amber() };
            Row::new(vec![
                Cell::from(if app.account_sort == crate::sort::Order::NameAsc { indent(a.depth, &a.name) } else { a.path.clone() }).style(name_style),
                Cell::from(a.r#type.clone()).style(faint()),
                Cell::from(a.class.clone()).style(dim()),
                Cell::from(a.commodity.clone()).style(dim()),
                right(money(&a.balance, prec)).style(signed(&a.balance)),
                right(money(&a.balance_functional, fp)).style(dim()),
                right(a.postings_count.to_string()).style(faint()),
            ])
        })
        .collect();
    let table = Table::new(rows, [Constraint::Min(22), Constraint::Length(9), Constraint::Length(13), Constraint::Length(5), Constraint::Length(16), Constraint::Length(16), Constraint::Length(6)])
        .header(header(&["account", "type", "class", "unit", "balance", &functional, "n"]))
        .block(block(&format!("chart of accounts · {} · {}", app.entity().map(|e| e.name.clone()).unwrap_or_default(), app.account_sort.label())))
        .row_highlight_style(selected())
        .column_spacing(1);
    let mut state = TableState::default().with_selected(Some(app.accounts_sel)).with_offset(app.accounts_sel.saturating_sub(area.height.saturating_sub(4) as usize / 2));
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_journal(f: &mut Frame, app: &mut App, area: Rect) {
    let fp = app.functional_precision();
    let rows: Vec<Row> = app
        .entries
        .iter()
        .map(|e| {
            let first = e.postings.first().map(|p| p.account_path.clone()).unwrap_or_default();
            Row::new(vec![
                Cell::from(e.date.clone()),
                Cell::from(e.kind.clone()).style(dim()),
                Cell::from(truncate(payee_display(&e.display_payee, &e.payee, &e.description), 32)),
                Cell::from(truncate(&first, 26)).style(dim()),
                right(money(&e.amount_functional, fp)),
                Cell::from(e.status.clone()).style(status_style(&e.status)),
                Cell::from(e.origin.clone()).style(faint()),
            ])
        })
        .collect();
    let filter = if app.journal_status.is_empty() { "all".to_string() } else { app.journal_status.clone() };
    let title = format!(
        "journal · {} · {} of {} · {}{}",
        app.entity().map(|e| e.name.clone()).unwrap_or_default(),
        app.entries.len(),
        app.entries_total,
        filter,
        if app.journal_query.is_empty() { String::new() } else { format!(" · /{}", app.journal_query) }
    );
    let title = format!("{title} · {}", app.journal_sort.label());
    let table = Table::new(rows, [Constraint::Length(10), Constraint::Length(9), Constraint::Min(18), Constraint::Length(26), Constraint::Length(14), Constraint::Length(7), Constraint::Length(9)])
        .header(header(&["date", "kind", "payee", "first account", "amount", "status", "origin"]))
        .block(block(&title))
        .row_highlight_style(selected())
        .column_spacing(1);
    let mut state = TableState::default().with_selected(Some(app.entries_sel)).with_offset(app.entries_sel.saturating_sub(area.height.saturating_sub(4) as usize / 2));
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_review(f: &mut Frame, app: &mut App, area: Rect) {
    let fp = app.functional_precision();
    let mut rows: Vec<Row> = Vec::new();
    for d in app.drafts.iter().chain(&app.unreviewed) {
        let label = if d.status == "posted" { "unreviewed" } else { "draft" };
        let entity = app.entities.iter().find(|x| x.id == d.entity_id).map(|x| x.name.clone()).unwrap_or_default();
        let bank = d.postings.first().map(|p| format!("{} {}", money(&p.quantity, app.precision(&p.commodity)), p.commodity)).unwrap_or_default();
        let contra = d.postings.last().map(|p| p.account_path.clone()).unwrap_or_default();
        rows.push(Row::new(vec![
            Cell::from(label).style(status_style(&d.status)),
            Cell::from(d.date.clone()),
            Cell::from(truncate(&entity, 10)).style(dim()),
            Cell::from(truncate(payee_display(&d.display_payee, &d.payee, &d.description), 34)),
            right(bank),
            Cell::from(truncate(&contra, 26)).style(dim()),
            right(money(&d.amount_functional, fp)),
        ]));
    }
    for l in &app.unmatched {
        rows.push(Row::new(vec![
            Cell::from("line").style(Style::default().fg(RED)),
            Cell::from(l.date.clone()),
            Cell::from(format!("#{}", l.import_id)).style(dim()),
            Cell::from(truncate(&l.description, 34)),
            right(format!("{} {}", money(&l.amount, app.precision(&l.currency)), l.currency)),
            Cell::from(truncate(&l.note, 26)).style(dim()),
            Cell::from(""),
        ]));
    }
    let title = format!("review · {} drafts · {} unreviewed · {} unmatched lines · {} within groups", app.drafts.len(), app.unreviewed.len(), app.unmatched.len(), app.review_sort.label());
    let table = Table::new(rows, [Constraint::Min(10), Constraint::Length(10), Constraint::Length(10), Constraint::Min(18), Constraint::Length(18), Constraint::Length(26), Constraint::Length(12)])
        .header(header(&["", "date", "entity", "payee", "bank posting", "currently", "amount"]))
        .block(block(&title))
        .row_highlight_style(selected())
        .column_spacing(1);
    let mut state = TableState::default().with_selected(Some(app.review_sel)).with_offset(app.review_sel.saturating_sub(area.height.saturating_sub(4) as usize / 2));
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_capture(f: &mut Frame, app: &mut App, area: Rect) {
    let cols = Layout::horizontal([Constraint::Length(56), Constraint::Min(20)]).split(area);
    let c = &app.capture;
    let mut lines: Vec<Line> = Vec::new();
    let value = |i: usize| -> String {
        match i {
            0 => KINDS[c.kind].to_string(),
            1 => c.account.as_ref().map(|(_, l)| l.clone()).unwrap_or_else(|| "(Enter to pick)".into()),
            2 => c.amount.clone(),
            3 => {
                if c.splits.is_empty() {
                    c.contra.as_ref().map(|(_, l)| l.clone()).unwrap_or_else(|| "(Enter to pick)".into())
                } else {
                    format!("{} splits (s edits, S clears)", c.splits.len())
                }
            }
            4 => c.payee.clone(),
            5 => c.date.clone(),
            _ => c.notes.clone(),
        }
    };
    for (i, name) in CAPTURE_FIELDS.iter().enumerate() {
        let label = if i == 3 {
            if c.kind == 2 {
                "To account"
            } else {
                "Category"
            }
        } else {
            name
        };
        let focused = i == c.field;
        let cursor = if focused && matches!(i, 2 | 4 | 5 | 6 | 7) { "▌" } else { "" };
        let style = if focused { selected() } else { amber() };
        lines.push(Line::from(vec![
            Span::styled(format!(" {label:<12} "), if focused { amber().add_modifier(Modifier::BOLD) } else { dim() }),
            Span::styled(format!(" {}{} ", value(i), cursor), style),
        ]));
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(" Enter on the last field or Ctrl-S posts the entry.", dim())));
    let entity = app.entity().map(|e| format!("{} ({})", e.name, e.currency)).unwrap_or_default();
    f.render_widget(Paragraph::new(lines).block(block(&format!("capture · {entity}"))), cols[0]);
    let mut side: Vec<Line> = vec![
        Line::from(Span::styled("How it posts", amber().add_modifier(Modifier::BOLD))),
        Line::from(""),
        Line::from(Span::styled(
            match c.kind {
                0 => "expense: Dr category / Cr account",
                1 => "income:  Dr account / Cr category",
                _ => "transfer: Dr to account / Cr from account",
            },
            dim(),
        )),
        Line::from(""),
    ];
    if !c.splits.is_empty() {
        side.push(Line::from(Span::styled("Splits", amber().add_modifier(Modifier::BOLD))));
        for (_, label, amt) in &c.splits {
            side.push(Line::from(Span::styled(format!("{} — {}", label, if amt.is_empty() { "the rest" } else { amt }), dim())));
        }
        side.push(Line::from(""));
    }
    if !c.result.is_empty() {
        side.push(Line::from(Span::styled("Last entry", amber().add_modifier(Modifier::BOLD))));
        side.push(Line::from(Span::styled(c.result.clone(), amber())));
    }
    f.render_widget(Paragraph::new(side).wrap(Wrap { trim: false }).block(block("notes")), cols[1]);
}

fn draw_imports(f: &mut Frame, app: &mut App, area: Rect) {
    let warning = app.imports.get(app.imports_sel).and_then(crate::import::coverage_warning);
    let chunks = Layout::vertical([Constraint::Min(3), Constraint::Length(if warning.is_some() { 8 } else { 0 })]).split(area);
    let area = chunks[0];
    if let Some(warning) = warning {
        f.render_widget(Paragraph::new(warning).style(amber()).wrap(Wrap { trim: false }).block(block("Possible import overlap")), chunks[1]);
    }
    let rows: Vec<Row> = app
        .imports
        .iter()
        .map(|i| {
            let account = app.accounts.iter().find(|a| a.id == i.account_id).map(|a| a.path.clone()).unwrap_or_else(|| if i.account_id == 0 { "(all)".into() } else { format!("#{}", i.account_id) });
            Row::new(vec![
                Cell::from(format!("#{}", i.id)).style(dim()),
                Cell::from(if crate::import::coverage_warning(i).is_some() { format!("{} !", i.status) } else { i.status.clone() }).style(match i.status.as_str() {
                    "pending" => amber().add_modifier(Modifier::BOLD),
                    "failed" => Style::default().fg(RED),
                    _ => faint(),
                }),
                Cell::from(i.created_at.chars().take(10).collect::<String>()),
                Cell::from(i.source.clone()),
                Cell::from(truncate(&account, 26)).style(dim()),
                Cell::from(truncate(&i.filename, 24)),
                right(i.lines_count.to_string()),
                right(i.created_count.to_string()).style(Style::default().fg(GREEN)),
                right(i.matched_count.to_string()),
                right(i.duplicate_count.to_string()).style(dim()),
                right(i.error_count.to_string()).style(if i.error_count > 0 { Style::default().fg(RED) } else { faint() }),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(6),
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Length(16),
            Constraint::Length(26),
            Constraint::Min(12),
            Constraint::Length(6),
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Length(5),
            Constraint::Length(5),
        ],
    )
    .header(header(&["id", "status", "date", "source", "account", "file", "lines", "created", "matched", "dup", "err"]))
    .block(block("imports · drop files in the inbox folder · Enter assigns a pending file"))
    .row_highlight_style(selected())
    .column_spacing(1);
    let mut state = TableState::default().with_selected(Some(app.imports_sel));
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_reports(f: &mut Frame, app: &mut App, area: Rect) {
    let entity = app.entity().map(|e| e.name.clone()).unwrap_or_default();
    if app.report_kind == ReportKind::Budgets {
        draw_budgets(f, app, area);
        return;
    }
    let Some(r) = app.report.as_ref() else {
        f.render_widget(Paragraph::new(Line::from(Span::styled(" no report (t/i/b statements, c/g/p expenses, u budgets, e entity)", dim()))).block(block(&format!("reports · {entity}"))), area);
        return;
    };
    let fp = app.precision(&r.currency);
    let rows: Vec<Row> = match app.report_kind {
        ReportKind::TrialBalance => r
            .rows
            .iter()
            .map(|row| {
                Row::new(vec![
                    Cell::from(truncate(&row.path, 44)),
                    Cell::from(row.commodity.clone()).style(dim()),
                    right(money(&row.quantity, app.precision(&row.commodity))).style(dim()),
                    right(money(&row.debit, fp)),
                    right(money(&row.credit, fp)),
                ])
            })
            .collect(),
        _ => r
            .rows
            .iter()
            .map(|row| {
                let style = if row.placeholder || row.depth == 0 { amber().add_modifier(Modifier::BOLD) } else { amber() };
                Row::new(vec![
                    Cell::from(indent(row.depth, &row.name)).style(style),
                    Cell::from(row.r#type.clone()).style(faint()),
                    right(money(&row.amount, fp)).style(signed(&row.amount)),
                    right(if row.market_value.is_empty() { String::new() } else { money(&row.market_value, fp) }).style(dim()),
                ])
            })
            .collect(),
    };
    let footer = match app.report_kind {
        ReportKind::TrialBalance => format!("debits {} · credits {} · difference {}", money(&r.total_debit, fp), money(&r.total_credit, fp), money(&r.net, fp)),
        ReportKind::IncomeStatement => format!("income {} · expenses {} · net {}", money(&r.total_credit, fp), money(&r.total_debit, fp), money(&r.net, fp)),
        ReportKind::ExpenseClass | ReportKind::ExpenseTag | ReportKind::ExpensePayee => {
            format!(
                "total (each posting once) {}\n{}..{} · tag {}",
                money(&r.net, fp),
                if app.report_from.is_empty() { "beginning" } else { &app.report_from },
                if app.report_to.is_empty() { "no end" } else { &app.report_to },
                if app.report_tag.is_empty() { "all" } else { &app.report_tag }
            )
        }
        ReportKind::Budgets => String::new(),
        ReportKind::BalanceSheet => format!("assets {} · liabilities {} · net worth {}", money(&r.total_debit, fp), money(&r.total_credit, fp), money(&r.net, fp)),
    };
    let footer_height = if matches!(app.report_kind, ReportKind::ExpenseClass | ReportKind::ExpenseTag | ReportKind::ExpensePayee) { 3 } else { 1 };
    let chunks = Layout::vertical([Constraint::Min(3), Constraint::Length(footer_height)]).split(area);
    let title = format!(
        "{} · {} · {}{}",
        app.report_kind.title(),
        entity,
        r.currency,
        if matches!(app.report_kind, ReportKind::ExpenseClass | ReportKind::ExpenseTag | ReportKind::ExpensePayee) { format!(" · {}", app.expense_sort.label()) } else { String::new() }
    );
    let table = match app.report_kind {
        ReportKind::TrialBalance => Table::new(rows, [Constraint::Min(24), Constraint::Length(5), Constraint::Length(16), Constraint::Length(16), Constraint::Length(16)])
            .header(header(&["account", "unit", "balance", "debit", "credit"])),
        ReportKind::ExpenseClass | ReportKind::ExpenseTag | ReportKind::ExpensePayee => {
            Table::new(r.rows.iter().map(|row| Row::new(vec![Cell::from(row.name.clone()), right(money(&row.amount, fp))])), [Constraint::Min(24), Constraint::Length(18)])
                .header(header(&["group", "amount"]))
        }
        _ => Table::new(rows, [Constraint::Min(24), Constraint::Length(9), Constraint::Length(18), Constraint::Length(18)]).header(header(&["account", "type", "amount", "market"])),
    }
    .block(block(&title))
    .row_highlight_style(selected())
    .column_spacing(1);
    let mut state = TableState::default().with_selected(Some(app.report_sel)).with_offset(app.report_sel.saturating_sub(area.height.saturating_sub(5) as usize / 2));
    f.render_stateful_widget(table, chunks[0], &mut state);
    f.render_widget(Paragraph::new(footer).style(amber().add_modifier(Modifier::BOLD)).wrap(Wrap { trim: false }), chunks[1]);
}

fn draw_connections(f: &mut Frame, app: &mut App, area: Rect) {
    let chunks = Layout::vertical([Constraint::Min(5), Constraint::Length(4), Constraint::Length(4)]).split(area);
    let mut rows: Vec<Row> = app
        .connections
        .providers
        .iter()
        .map(|p| Row::new(vec![Cell::from(p.name.clone()), Cell::from(if p.configured { "Ready for setup" } else { "Needs server environment" }), Cell::from("")]))
        .collect();
    rows.extend(app.connections.accounts.iter().map(|a| {
        Row::new(vec![Cell::from(format!("  {} / {}", a.channel, a.name)), Cell::from(if a.account_id == 0 { "Unmapped".into() } else { a.account_path.clone() }), Cell::from(a.currency.clone())])
    }));
    let table = Table::new(rows, [Constraint::Percentage(48), Constraint::Min(20), Constraint::Length(5)])
        .header(header(&["provider / account", "destination / setup", "unit"]))
        .block(block("Connections · all entities · environment credentials"))
        .row_highlight_style(selected());
    let mut state = TableState::default().with_selected(Some(app.connections_sel));
    f.render_stateful_widget(table, chunks[0], &mut state);
    let detail = if let Some(a) = app.selected_connection() {
        let consent = app.connections.consents.iter().find(|s| s.id == a.item_id).map(|s| format!(" · consent expires {}", s.valid_until)).unwrap_or_default();
        format!(
            "{} · entity {}\nLast fetched: {} · booked from: {}{consent}\n{}",
            a.name,
            a.entity_id,
            if a.last_pull_at.is_empty() { "never" } else { &a.last_pull_at },
            if a.booked_from.is_empty() { "all available" } else { &a.booked_from },
            if a.channel == "pluggy" {
                "MeuPluggy freshness comes from its own bank syncing."
            } else if a.channel == "enable_banking" {
                "Pull fetches this account using its booking cutoff."
            } else {
                "Pull fetches all available history for this account."
            }
        )
    } else if let Some(p) = app.connections.providers.get(app.connections_sel) {
        format!("Server environment: {}\nPress Enter or n for setup. Credentials stay on the server.", p.requirements)
    } else {
        "Loading connections…".into()
    };
    f.render_widget(Paragraph::new(detail).wrap(Wrap { trim: false }), chunks[1]);
    let job = app
        .connection_job()
        .map(|j| {
            if j.authorization_url.is_empty() {
                format!("{} / {}: {}\n{}", j.provider, j.operation, j.state, j.message)
            } else {
                format!("Authorize in your browser (operation running):\n{}", j.authorization_url)
            }
        })
        .unwrap_or_else(|| "No operations yet. Discover accounts, map them, then pull.".into());
    f.render_widget(Paragraph::new(job).wrap(Wrap { trim: false }).block(block("Latest operation · pulls continue when you change screens")), chunks[2]);
}

fn draw_budgets(f: &mut Frame, app: &mut App, area: Rect) {
    let currency = app.entity().map(|e| e.currency.as_str()).unwrap_or("");
    let fp = app.precision(currency);
    let chunks = Layout::vertical([Constraint::Min(3), Constraint::Length(3)]).split(area);
    let rows = app.budgets.iter().filter_map(|r| {
        r.budget.as_ref().map(|b| {
            let group = if app.budgets_by_tag {
                if b.tag.is_empty() {
                    "No tag"
                } else {
                    &b.tag
                }
            } else if r.group.is_empty() {
                "Unclassified"
            } else {
                &r.group
            };
            Row::new(vec![Cell::from(format!("{group} / {}", b.name)), right(money(&b.amount, fp)), right(money(&r.spent, fp)), right(money(&r.remaining, fp)).style(signed(&r.remaining))])
        })
    });
    let title = format!("Budgets · {} · {currency} · grouped by {}", app.entity().map(|e| e.name.as_str()).unwrap_or(""), if app.budgets_by_tag { "tag" } else { "class" });
    let table = Table::new(rows, [Constraint::Min(20), Constraint::Length(12), Constraint::Length(12), Constraint::Length(12)])
        .header(header(&["group / budget", "limit", "spent", "remaining"]))
        .block(block(&title))
        .row_highlight_style(selected());
    let mut state = TableState::default().with_selected(Some(app.report_sel));
    f.render_stateful_widget(table, chunks[0], &mut state);
    let detail = app
        .budgets
        .get(app.report_sel)
        .and_then(|r| r.budget.as_ref().map(|b| format!("{}..{} · {} · tag {}", b.starts_on, b.ends_on, r.target, if b.tag.is_empty() { "all" } else { &b.tag })))
        .unwrap_or_else(|| "No budgets. Press n to create one.".into());
    f.render_widget(Paragraph::new(format!("{detail}\nWhole-period limits · overlapping budgets; no combined total")).wrap(Wrap { trim: false }), chunks[1]);
}

fn draw_modal(f: &mut Frame, app: &mut App, area: Rect) {
    match &app.modal {
        Modal::None => {}
        Modal::Split(form) => {
            let r = fixed_centered(area, 100, 23);
            f.render_widget(Clear, r);
            f.render_widget(Paragraph::new(form.lines().join("\n")).wrap(Wrap { trim: false }).block(block("Split posting")), r);
        }
        Modal::ConnectionStatus(text) => {
            let r = fixed_centered(area, 90, area.height.saturating_sub(2));
            f.render_widget(Clear, r);
            f.render_widget(Paragraph::new(text.as_str()).wrap(Wrap { trim: false }).block(block("Connection status · Esc closes")), r);
        }
        Modal::Connection(form) => {
            let r = fixed_centered(area, 90, 20);
            f.render_widget(Clear, r);
            let parts = Layout::vertical([Constraint::Length(6), Constraint::Length(7), Constraint::Min(3)]).split(r);
            let help="Tab/up/down fields · left/right choices · Esc close\nCtrl-D discover · Ctrl-B list banks · Ctrl-A authorize Enable Banking\nCtrl-O open MeuPluggy · credentials come from the server environment";
            f.render_widget(Paragraph::new(help).wrap(Wrap { trim: false }).block(block("Connection setup")), parts[0]);
            f.render_widget(Paragraph::new(form.lines().join("\n")).wrap(Wrap { trim: false }), parts[1]);
            f.render_widget(Paragraph::new(form.message.clone()).wrap(Wrap { trim: false }).block(block("Status")), parts[2]);
        }
        Modal::Sharing(form) => {
            let rect = centered(area, 90, 85);
            f.render_widget(Clear, rect);
            f.render_widget(Paragraph::new(form.lines().join("\n")).wrap(Wrap { trim: false }).scroll((form.scroll.min(u16::MAX as usize) as u16, 0)).block(block("Sharing")), rect);
        }
        Modal::Payee(form) => {
            let r = centered(area, 96, 94);
            f.render_widget(Clear, r);
            let width = r.width.saturating_sub(2).max(1) as usize;
            let lines = form
                .lines()
                .into_iter()
                .flat_map(|line| {
                    let mut rows = Vec::new();
                    let mut row = String::new();
                    for c in line.chars() {
                        let next = format!("{row}{c}");
                        if !row.is_empty() && Line::from(next.as_str()).width() > width {
                            rows.push(std::mem::take(&mut row));
                        }
                        row.push(c);
                    }
                    rows.push(row);
                    rows
                })
                .collect::<Vec<_>>();
            let offset = form.scroll.min(lines.len().saturating_sub(r.height.saturating_sub(2) as usize));
            f.render_widget(Paragraph::new(lines.join("\n")).scroll((offset.min(u16::MAX as usize) as u16, 0)).block(block("Payee · preview before applying")), r);
        }
        Modal::Budget(form) => {
            let r = fixed_centered(area, 78, 18);
            f.render_widget(Clear, r);
            let mut lines = vec![
                Line::from("Tab/up/down fields · Ctrl-S save · Esc cancel"),
                Line::from("Target: type to filter categories; left/right to choose"),
                Line::from(format!("Category search: {}", form.query)),
            ];
            lines.extend(form.lines().into_iter().map(|line| {
                let style = if line.starts_with('>') { selected() } else { amber() };
                Line::from(Span::styled(line, style))
            }));
            lines.push(Line::from("One limit for the entire inclusive period; no rollover."));
            lines.push(Line::from(form.message.clone()));
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(block("Budget")), r);
        }
        Modal::Import(form) => {
            let r = centered(area, 96, 94);
            f.render_widget(Clear, r);
            let chunks = Layout::vertical([Constraint::Length(5), Constraint::Min(1), Constraint::Length(4)]).split(r);
            f.render_widget(Paragraph::new(form.help()).wrap(Wrap { trim: false }).block(block(&format!("Import: {}", form.filename))), chunks[0]);
            let lines = form.lines();
            let offset = form.offset(chunks[1].height as usize);
            let lines: Vec<Line> = lines
                .into_iter()
                .skip(offset)
                .map(|line| {
                    let style = if line.starts_with('>') { selected() } else { amber() };
                    Line::from(Span::styled(line, style))
                })
                .collect();
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), chunks[1]);
            f.render_widget(Paragraph::new(form.message.as_str()).wrap(Wrap { trim: false }).block(block("Status")), chunks[2]);
        }
        Modal::Help => {
            let r = fixed_centered(area, 74, 24);
            f.render_widget(Clear, r);
            let lines: Vec<Line> = [
                "1-9, 0 or Tab/Shift-Tab   switch screens",
                "e                      cycle entity",
                "r                      refresh",
                "j/k or arrows          move · g/G first/last",
                "Enter                  open · Esc back",
                "/                      search (journal)",
                "a d p v                journal filters",
                "Enter or a             review: post a draft or confirm an unreviewed entry",
                "b                      review: preview each draft with this payee",
                "u                      review: make a rule from this payee and run it",
                "t                      journal, review: tag the entry (city:lisbon trip:x)",
                "x                      review: delete draft (asks)",
                "s                      review: skip an unmatched line",
                "f                      review: link a full refund to its expense",
                "i                      imports: import a file",
                "Enter                  imports: account for a pending inbox file",
                "n R c m M x            accounts: new, rename, code, move, merge, close",
                "m                      ledger: re-categorize the entry",
                "s                      category ledger: split by text filter",
                "s                      capture: split across categories",
                "z                      sort accounts, ledgers, journal, review, expenses",
                "S                      review/account ledger: split transaction",
                "Ctrl-N                 category pickers: create from the typed path (Food:Ramen)",
                "t/i/b c/g u            reports: statements, expenses, budgets",
                "q or Ctrl-C            quit",
            ]
            .iter()
            .map(|s| Line::from(Span::styled(format!(" {s}"), amber())))
            .collect();
            f.render_widget(Paragraph::new(lines).block(block("keys")), r);
        }
        Modal::Confirm { text, .. } => {
            let width = area.width.min(90);
            let lines = (text.chars().count() as u16).div_ceil(width.saturating_sub(4).max(1));
            let r = fixed_centered(area, width, lines.saturating_add(3));
            f.render_widget(Clear, r);
            f.render_widget(Paragraph::new(text.as_str()).wrap(Wrap { trim: false }).style(amber().add_modifier(Modifier::BOLD)).block(block("confirm")), r);
        }
        Modal::Input { title, value, .. } => {
            let r = fixed_centered(area, 70, 3);
            f.render_widget(Clear, r);
            f.render_widget(Paragraph::new(Line::from(vec![Span::styled(format!(" {value}"), amber()), Span::styled("▌", amber())])).block(block(title)), r);
        }
        Modal::Picker(p, target) => {
            let r = centered(area, 70, 70);
            f.render_widget(Clear, r);
            let chunks = Layout::vertical([Constraint::Length(3), Constraint::Min(3)]).split(r);
            let can_create = matches!(target, crate::app::PickerTarget::ReviewContra | crate::app::PickerTarget::CaptureContra);
            let hint = if can_create { "  filter · Enter select · Ctrl-N new category from the text · Esc" } else { "  type to filter · Enter select · Esc cancel" };
            f.render_widget(
                Paragraph::new(Line::from(vec![Span::styled(format!(" {}", p.filter), amber()), Span::styled("▌", amber()), Span::styled(hint, faint())])).block(block(&p.title)),
                chunks[0],
            );
            let visible = p.visible();
            let rows: Vec<Row> =
                visible.iter().map(|(_, label, hint)| Row::new(vec![Cell::from(truncate(label, chunks[1].width.saturating_sub(24) as usize)), Cell::from(truncate(hint, 18)).style(dim())])).collect();
            let table = Table::new(rows, [Constraint::Min(10), Constraint::Length(18)]).block(Block::bordered().border_style(dim())).row_highlight_style(selected());
            let mut state = TableState::default().with_selected(Some(p.selected)).with_offset(p.selected.saturating_sub(chunks[1].height.saturating_sub(2) as usize / 2));
            f.render_stateful_widget(table, chunks[1], &mut state);
        }
        Modal::EntryDetail(e) => draw_transaction(f, app, area, e, false, app.entry_scroll, ""),
        Modal::PostingPreview(p) => draw_transaction(f, app, area, &p.entry, true, p.scroll, &p.error),
    }
}

fn draw_transaction(f: &mut Frame, app: &App, area: Rect, e: &means_proto::v1::JournalEntry, preview: bool, scroll: u16, error: &str) {
    let currency = app.entities.iter().find(|entity| entity.id == e.entity_id).map(|entity| entity.currency.as_str()).unwrap_or("");
    let mut lines = vec![
        Line::from(Span::styled(if preview { "PREVIEW — nothing saved".to_string() } else { format!("Entry #{} · {}", e.id, e.status) }, amber())),
        Line::from(format!("{} · {} · {}", e.date, e.kind, payee_display(&e.display_payee, &e.payee, &e.description))),
        Line::from(format!("Booked payee: {}", e.payee)),
    ];
    if !e.description.is_empty() && e.description != e.payee {
        lines.push(Line::from(e.description.clone()));
    }
    if !e.notes.is_empty() {
        lines.push(Line::from(e.notes.clone()));
    }
    if !e.tags.is_empty() {
        lines.push(Line::from(e.tags.join(" ")));
    }
    lines.push(Line::from(Span::styled("POSTINGS", dim())));
    for p in &e.postings {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(p.account_path.clone(), amber())));
        let direction = if sign(&p.quantity) < 0 { "Credit" } else { "Debit" };
        lines.push(Line::from(format!(
            "  {direction} {} {} · booked {} {currency}{}",
            money(p.quantity.trim_start_matches('-'), app.precision(&p.commodity)),
            p.commodity,
            money(&p.amount, app.precision(currency)),
            if p.reconciled_at.is_empty() { "" } else { " · reconciled" }
        )));
        if !p.rate.is_empty() {
            lines.push(Line::from(format!("  Rate: {}", p.rate)));
        }
        if !p.memo.is_empty() {
            lines.push(Line::from(format!("  {}", p.memo)));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from("Asset accounts: debit adds money; credit removes money."));
    for (label, id) in [("Counterpart", e.counterpart_id), ("Reverses", e.reverses_id), ("Reversed by", e.reversed_by_id), ("Refund of", e.refund_of_id), ("Statement line", e.statement_line_id)] {
        if id != 0 {
            lines.push(Line::from(format!("{label} #{id}")));
        }
    }
    if !error.is_empty() {
        lines.push(Line::from(Span::styled(error.to_owned(), Style::default().fg(RED))));
    }
    let width = area.width.saturating_sub(4).clamp(1, 110);
    let wrapped: usize = lines.iter().map(|l| l.width().max(1).div_ceil(width.saturating_sub(4).max(1) as usize)).sum();
    let height = (wrapped as u16).saturating_add(4).min(area.height.saturating_sub(2));
    let r = Rect::new(area.x + area.width.saturating_sub(width) / 2, area.y + area.height.saturating_sub(height) / 2, width, height);
    f.render_widget(Clear, r);
    f.render_widget(Block::default().style(Style::default().bg(INK)), r);
    let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(r);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll.min(wrapped.saturating_sub(chunks[0].height.saturating_sub(2) as usize) as u16), 0))
            .style(amber().bg(INK))
            .block(block(if preview { "confirm transaction" } else { "transaction" })),
        chunks[0],
    );
    f.render_widget(Paragraph::new(if preview { " y confirm · n/Esc edit · j/k scroll" } else { " s split · Esc close · j/k scroll" }).style(amber().bg(INK)), chunks[1]);
}

pub fn help_alignment() -> Alignment {
    Alignment::Left
}

fn payee_display<'a>(display: &'a str, booked: &'a str, description: &'a str) -> &'a str {
    if !display.is_empty() {
        display
    } else if !booked.is_empty() {
        booked
    } else {
        description
    }
}
