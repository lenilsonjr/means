//! Payee editor and scrollable preview. Applying always requires a second, explicit key.
use crate::client::Client;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use means_proto::v1 as pb;

#[derive(Debug, Clone)]
pub enum Operation {
    Save,
    Backfill,
    Link,
    Merge,
}
#[derive(Debug, Clone)]
pub struct Form {
    pub payee: pb::Payee,
    pub aliases: String,
    pub operation: Operation,
    pub target: String,
    pub field: usize,
    pub preview: Option<pb::PayeePreview>,
    pub scroll: usize,
    pub message: String,
    pub saved: bool,
}
impl Form {
    pub fn new(payee: pb::Payee, operation: Operation) -> Self {
        Self { aliases: payee.aliases.join("; "), payee, operation, target: String::new(), field: 0, preview: None, scroll: 0, message: String::new(), saved: false }
    }
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec!["Tab fields · Ctrl-P preview · Ctrl-Y apply reviewed preview · Esc close".into()];
        if let Some(p) = &self.preview {
            lines.push("Up/down or PgUp/PgDn scroll the full preview".into());
            lines.extend(p.warnings.clone());
            lines.push(format!("{} affected/evaluated entries or statement lines", p.impacts.len()));
            for i in &p.impacts {
                lines.push(format!(
                    "Entry #{} / line #{} | booked: {} | evidence: {} | candidates {:?} | rule {} -> {}",
                    i.entry_id, i.line_id, i.booked, i.evidence, i.candidates, i.before_rule, i.after_rule
                ));
            }
        } else {
            match self.operation {
                Operation::Save => {
                    for (i, s) in [format!("Name: {}", self.payee.name), format!("Aliases (semicolon separated): {}", self.aliases), format!("Active: {} (space toggles)", self.payee.active)]
                        .into_iter()
                        .enumerate()
                    {
                        lines.push(format!("{} {}", if i == self.field { ">" } else { " " }, s));
                    }
                }
                Operation::Backfill => {
                    lines.push("Link unlinked history from statement evidence. Applying attests chart, classes, splits and rules are reviewed. A backup is saved beside the ledger.".into())
                }
                Operation::Link => lines.push(format!("Entry ID to link to {}: {}", self.payee.name, self.target)),
                Operation::Merge => lines.push(format!("Target payee ID for {}: {} (source aliases will transfer; conflicts are previewed)", self.payee.name, self.target)),
            }
        }
        lines.push(self.message.clone());
        lines
    }
    async fn request(&self, client: &mut Client, confirmation: String) -> anyhow::Result<pb::PayeePreview> {
        match self.operation {
            Operation::Save => {
                let mut p = self.payee.clone();
                p.aliases = if self.aliases.trim().is_empty() { vec![] } else { self.aliases.split(';').map(|s| s.trim().to_string()).collect() };
                client.save_payee(pb::SavePayeeRequest { payee: Some(p), confirmation }).await
            }
            Operation::Backfill => client.link_payees(pb::LinkPayeesRequest { entity_id: self.payee.entity_id, chart_reviewed: !confirmation.is_empty(), confirmation }).await,
            Operation::Link => client.reassign_payee(pb::ReassignPayeeRequest { entry_id: self.target.trim().parse()?, target_id: self.payee.id, confirmation, ..Default::default() }).await,
            Operation::Merge => client.reassign_payee(pb::ReassignPayeeRequest { source_id: self.payee.id, target_id: self.target.trim().parse()?, confirmation, ..Default::default() }).await,
        }
    }
    pub async fn key(&mut self, key: KeyEvent, client: &mut Client) -> bool {
        if key.code == KeyCode::Esc {
            return true;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            let confirmation = match key.code {
                KeyCode::Char('p') => Some(String::new()),
                KeyCode::Char('y') => self.preview.as_ref().map(|p| p.token.clone()),
                _ => None,
            };
            if let Some(token) = confirmation {
                match self.request(client, token).await {
                    Ok(p) => {
                        self.saved = p.applied;
                        self.scroll = 0;
                        if p.applied {
                            self.message = format!("Applied. Backup: {}", p.backup);
                            self.preview = None;
                        } else {
                            self.preview = Some(p);
                            self.message.clear();
                        }
                    }
                    Err(e) => {
                        self.message = e.to_string();
                        self.preview = None;
                    }
                }
            }
            return false;
        }
        if self.saved {
            return false;
        }
        if self.preview.is_some() {
            match key.code {
                KeyCode::Down => self.scroll += 1,
                KeyCode::PageDown => self.scroll += 8,
                KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(8),
                KeyCode::Backspace => {
                    self.preview = None;
                    self.scroll = 0
                }
                _ => {}
            }
            self.scroll = self.scroll.min(self.lines().iter().map(|l| l.chars().count() + 1).sum::<usize>());
            return false;
        }
        match key.code {
            KeyCode::Tab | KeyCode::Down => self.field = (self.field + 1) % 3,
            KeyCode::BackTab | KeyCode::Up => self.field = (self.field + 2) % 3,
            KeyCode::Char(' ') if matches!(self.operation, Operation::Save) && self.field == 2 => self.payee.active = !self.payee.active,
            KeyCode::Char(c) => {
                if let Some(s) = self.text() {
                    s.push(c)
                }
            }
            KeyCode::Backspace => {
                if let Some(s) = self.text() {
                    s.pop();
                }
            }
            _ => {}
        }
        false
    }
    fn text(&mut self) -> Option<&mut String> {
        match self.operation {
            Operation::Save => match self.field {
                0 => Some(&mut self.payee.name),
                1 => Some(&mut self.aliases),
                _ => None,
            },
            Operation::Link | Operation::Merge => Some(&mut self.target),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn long_conflict_previews_render_and_scroll_at_terminal_widths() {
        let mut app = crate::app::App::new("http://127.0.0.1:1");
        let mut form = Form::new(pb::Payee { entity_id: 1, name: "Cafe".into(), active: true, ..Default::default() }, Operation::Save);
        form.preview = Some(pb::PayeePreview {
            impacts: (1..40).map(|id| pb::PayeeImpact { entry_id: id, evidence: "Long statement text with counterparty details ".repeat(4), candidates: vec![1, 2], ..Default::default() }).collect(),
            ..Default::default()
        });
        form.scroll = 15;
        app.modal = crate::app::Modal::Payee(Box::new(form));
        for width in [80, 120] {
            let backend = ratatui::backend::TestBackend::new(width, 24);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            let text = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect::<String>();
            assert!(text.contains("candidates"));
        }
    }
}
