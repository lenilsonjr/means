//! Import forms keep the inspected bytes until the user confirms the import.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use means_proto::v1 as pb;

use crate::client::Client;

const LABELS: [&str; 16] = [
    "Delimiter (empty=auto, tab=TAB)",
    "Header row (0=auto)",
    "Date column",
    "Date format (empty=auto)",
    "Signed amount column",
    "Debit column",
    "Credit column",
    "Description column",
    "Reference column",
    "Balance column",
    "Currency column",
    "Decimal separator (empty=auto)",
    "Invert sign (true/false)",
    "Fixed currency",
    "Extra description columns (one per |)",
    "Use mapping (true/false)",
];
const KINDS: [(&str, &str); 8] = [("", ""), ("asset", "bank"), ("asset", "savings"), ("asset", "cash"), ("asset", "receivable"), ("asset", "investment"), ("liability", "card"), ("liability", "loan")];

#[derive(Debug, Clone)]
pub struct ImportForm {
    pub filename: String,
    content: Vec<u8>,
    account_id: i64,
    account_label: String,
    pub fields: Vec<String>,
    pub selected: usize,
    pub scroll: usize,
    pub message: String,
    headers: Vec<String>,
    samples: Vec<String>,
    entities: Vec<pb::Entity>,
    default_entity_id: i64,
    inspection: Option<pb::InspectAccountTrackerResponse>,
    pub mappings: Vec<pb::AccountTrackerMapping>,
    pub expand_recurring: bool,
    pub schedules: bool,
    preview: Option<pb::UploadImportResponse>,
    pub reviewing: bool,
    pub finished: bool,
    result: Vec<String>,
}

impl ImportForm {
    pub fn new(filename: String, content: Vec<u8>, account_id: i64, account_label: String) -> Self {
        let mut fields = vec![String::new(); LABELS.len()];
        fields[1] = "0".into();
        fields[12] = "false".into();
        fields[15] = "false".into();
        Self {
            filename,
            content,
            account_id,
            account_label,
            fields,
            selected: 0,
            scroll: 0,
            message: String::new(),
            headers: vec![],
            samples: vec![],
            entities: vec![],
            default_entity_id: 0,
            inspection: None,
            mappings: vec![],
            expand_recurring: true,
            schedules: true,
            preview: None,
            reviewing: false,
            finished: false,
            result: vec![],
        }
    }

    pub async fn inspect_tracker(&mut self, client: &mut Client, default_entity: i64) -> anyhow::Result<()> {
        self.entities = client.list_entities(pb::ListEntitiesRequest { include_archived: false }).await?.entities;
        anyhow::ensure!(!self.entities.is_empty(), "create an entity before importing a backup");
        let entity_id = self.entities.iter().find(|e| e.id == default_entity).unwrap_or(&self.entities[0]).id;
        self.default_entity_id = entity_id;
        let inspection = client.inspect_account_tracker(pb::InspectAccountTrackerRequest { content: self.content.clone() }).await?;
        self.mappings =
            inspection.accounts.iter().map(|a| pb::AccountTrackerMapping { external_id: a.external_id.clone(), entity_id, r#type: String::new(), subtype: String::new(), skip: false }).collect();
        self.inspection = Some(inspection);
        Ok(())
    }

    pub fn mapping(&self) -> anyhow::Result<Option<pb::CsvMapping>> {
        let boolean = |i: usize| -> anyhow::Result<bool> {
            match self.fields[i].trim() {
                "true" => Ok(true),
                "false" => Ok(false),
                _ => anyhow::bail!("{} must be true or false", LABELS[i]),
            }
        };
        if !boolean(15)? {
            return Ok(None);
        }
        let value = |i: usize| self.fields[i].trim().to_string();
        let delimiter = if value(0) == "tab" { "\t".into() } else { value(0) };
        anyhow::ensure!(delimiter.is_empty() || [",", ";", "\t", "|"].contains(&delimiter.as_str()), "delimiter must be comma, semicolon, tab, or pipe");
        let header_row: i32 = value(1).parse().map_err(|_| anyhow::anyhow!("header row must be a nonnegative integer"))?;
        anyhow::ensure!(header_row >= 0, "header row must be nonnegative");
        anyhow::ensure!(["", ".", ","].contains(&value(11).as_str()), "decimal separator must be . or ,");
        anyhow::ensure!(!value(2).is_empty(), "choose a date column");
        anyhow::ensure!(!value(4).is_empty() || !value(5).is_empty() || !value(6).is_empty(), "choose a signed amount or debit/credit columns");
        anyhow::ensure!(value(4).is_empty() || (value(5).is_empty() && value(6).is_empty()), "use signed amount OR debit/credit columns");
        Ok(Some(pb::CsvMapping {
            delimiter,
            header_row,
            date_column: value(2),
            date_format: value(3),
            amount_column: value(4),
            debit_column: value(5),
            credit_column: value(6),
            description_column: value(7),
            reference_column: value(8),
            balance_column: value(9),
            currency_column: value(10),
            decimal_separator: value(11),
            invert_sign: boolean(12)?,
            currency: value(13),
            extra_description_columns: value(14).split('|').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect(),
        }))
    }

    fn request(&self, preview: bool) -> anyhow::Result<pb::UploadImportRequest> {
        Ok(pb::UploadImportRequest {
            source: "auto".into(),
            account_id: self.account_id,
            filename: self.filename.clone(),
            content: self.content.clone(),
            mapping: self.mapping()?,
            preview,
            options: String::new(),
        })
    }

    pub async fn preview(&mut self, client: &mut Client) {
        self.preview = None;
        self.reviewing = false;
        let response = match self.request(true) {
            Ok(request) => client.upload_import(request).await,
            Err(error) => Err(error),
        };
        match response {
            Ok(r) => {
                self.headers = r.headers.clone();
                self.samples = r.sample_rows.clone();
                if let Ok(Some(mapping)) = self.mapping() {
                    let columns = [
                        &mapping.date_column,
                        &mapping.amount_column,
                        &mapping.debit_column,
                        &mapping.credit_column,
                        &mapping.description_column,
                        &mapping.reference_column,
                        &mapping.balance_column,
                        &mapping.currency_column,
                    ];
                    if let Some(missing) =
                        columns.into_iter().chain(mapping.extra_description_columns.iter()).find(|c| !c.is_empty() && !r.headers.iter().any(|h| h.trim().eq_ignore_ascii_case(c.trim())))
                    {
                        self.message = format!("Column {missing:?} was not found. Check the headers and mapping.");
                        return;
                    }
                }
                if r.detected_source == "generic_csv" && self.fields[15] == "false" {
                    self.fields[15] = "true".into();
                    self.selected = 2;
                    self.message = "Map the columns, then Ctrl-P to preview. Left/Right selects a header.".into();
                } else if r.import.as_ref().is_none_or(|i| i.lines_count == 0) {
                    self.message = "No parsed lines. Check the mapping before importing.".into();
                } else {
                    self.preview = Some(r);
                    self.reviewing = true;
                    self.scroll = 0;
                    self.message.clear();
                }
            }
            Err(e) => self.message = e.to_string(),
        }
    }

    /// Returns true when the modal can close. Escape never imports anything.
    pub async fn key(&mut self, key: KeyEvent, client: &mut Client) -> bool {
        if key.code == KeyCode::Esc {
            if self.reviewing && !self.finished {
                self.reviewing = false;
                self.preview = None;
                self.scroll = 0;
                return false;
            }
            return true;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return true;
        }
        if self.finished || self.reviewing {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => self.scroll = (self.scroll + 1).min(self.lines().len().saturating_sub(1)),
                KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::Char('y') if self.reviewing && !self.finished => self.commit(client).await,
                _ => {}
            }
            return false;
        }
        if !matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
            self.scroll = 0;
        }
        if self.inspection.is_some() {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(self.mappings.len().saturating_sub(1)),
                KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
                KeyCode::Char('e') => {
                    if let Some(m) = self.mappings.get_mut(self.selected) {
                        let index = self.entities.iter().position(|e| e.id == m.entity_id).unwrap_or(0);
                        m.entity_id = self.entities[(index + 1) % self.entities.len()].id;
                    }
                }
                KeyCode::Char('t') => {
                    if let Some(m) = self.mappings.get_mut(self.selected) {
                        let index = KINDS.iter().position(|(t, s)| *t == m.r#type && *s == m.subtype).unwrap_or(0);
                        let (kind, subtype) = KINDS[(index + 1) % KINDS.len()];
                        m.r#type = kind.into();
                        m.subtype = subtype.into();
                    }
                }
                KeyCode::Char('x') => {
                    if let Some(m) = self.mappings.get_mut(self.selected) {
                        m.skip = !m.skip;
                    }
                }
                KeyCode::Char('r') => self.expand_recurring = !self.expand_recurring,
                KeyCode::Char('s') => self.schedules = !self.schedules,
                KeyCode::Enter => {
                    if self.mappings.iter().any(|m| !m.skip) {
                        self.reviewing = true;
                        self.scroll = 0;
                        self.message.clear();
                    } else {
                        self.message = "Select at least one account to import.".into();
                    }
                }
                _ => {}
            }
        } else {
            match key.code {
                KeyCode::PageDown => self.scroll = (self.scroll + 5).min(self.lines().len().saturating_sub(1)),
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(5),
                KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => self.preview(client).await,
                KeyCode::Down | KeyCode::Tab => self.selected = (self.selected + 1) % LABELS.len(),
                KeyCode::Up | KeyCode::BackTab => self.selected = (self.selected + LABELS.len() - 1) % LABELS.len(),
                KeyCode::Left | KeyCode::Right if [2, 4, 5, 6, 7, 8, 9, 10].contains(&self.selected) => {
                    let mut choices = vec![String::new()];
                    choices.extend(self.headers.clone());
                    let index = choices.iter().position(|s| *s == self.fields[self.selected]).unwrap_or(0);
                    let delta = if key.code == KeyCode::Right { 1 } else { choices.len() - 1 };
                    self.fields[self.selected] = choices[(index + delta) % choices.len()].clone();
                    self.fields[15] = "true".into();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.fields[self.selected].clear();
                    if self.selected != 15 {
                        self.fields[15] = "true".into();
                    }
                }
                KeyCode::Backspace => {
                    self.fields[self.selected].pop();
                    if self.selected != 15 {
                        self.fields[15] = "true".into();
                    }
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.fields[self.selected].push(c);
                    if self.selected != 15 {
                        self.fields[15] = "true".into();
                    }
                }
                _ => {}
            }
        }
        false
    }

    async fn commit(&mut self, client: &mut Client) {
        let result = if self.inspection.is_some() {
            let request = pb::ImportAccountTrackerRequest {
                filename: self.filename.clone(),
                content: self.content.clone(),
                mapping: self.mappings.clone(),
                default_entity_id: self.default_entity_id,
                expand_recurring: self.expand_recurring,
                schedules: self.schedules,
            };
            client.import_account_tracker(request).await.map(|r| {
                let mut lines = vec![summary(r.import.as_ref())];
                lines.extend(r.warnings.into_iter().map(|w| format!("Warning: {w}")));
                lines.extend(r.checks.into_iter().map(|c| format!("{} {}: expected {}, actual {}", if c.ok { "OK" } else { "MISMATCH" }, c.name, c.expected, c.actual)));
                lines
            })
        } else if self.preview.is_some() {
            match self.request(false) {
                Ok(request) => client.upload_import(request).await.map(|r| vec![summary(r.import.as_ref())]),
                Err(e) => Err(e),
            }
        } else {
            return;
        };
        match result {
            Ok(lines) => {
                self.message = lines.first().cloned().unwrap_or_default();
                self.result = lines;
                self.finished = true;
                self.scroll = 0;
            }
            Err(e) => {
                self.message = e.to_string();
                self.reviewing = false;
                self.preview = None;
            }
        }
    }

    pub fn help(&self) -> &'static str {
        if self.finished {
            "Up/Down scroll · Esc close"
        } else if self.reviewing {
            "Review all rows · Up/Down scroll · y import · Esc back"
        } else if self.inspection.is_some() {
            "Up/Down account · e entity · t type · x skip · r expand repeats · s schedules · Enter review · Esc cancel"
        } else {
            "Tab/Up/Down field · type to edit · Left/Right column · Ctrl-U clear · Ctrl-P preview · PgUp/Dn samples · Esc cancel"
        }
    }

    pub fn offset(&self, height: usize) -> usize {
        if self.reviewing || self.finished {
            self.scroll
        } else {
            (self.selected.saturating_sub(height.saturating_sub(5) / 4) + self.scroll).min(self.lines().len().saturating_sub(1))
        }
    }

    pub fn lines(&self) -> Vec<String> {
        if self.finished {
            return self.result.clone();
        }
        if let Some(inspect) = &self.inspection {
            let mut lines = vec![format!(
                "{} accounts; {} transactions, {} to {}; expand repeats: {}; schedules: {}",
                inspect.accounts.len(),
                inspect.transactions,
                inspect.first_date,
                inspect.last_date,
                self.expand_recurring,
                self.schedules
            )];
            for (i, (a, m)) in inspect.accounts.iter().zip(&self.mappings).enumerate() {
                let entity = self.entities.iter().find(|e| e.id == m.entity_id).map(|e| e.name.as_str()).unwrap_or("?");
                lines.push(format!(
                    "{} {} [{}] {} {} → {} / {} / {} {}",
                    if self.selected == i && !self.reviewing { ">" } else { " " },
                    a.name,
                    a.external_id,
                    a.currency,
                    a.balance,
                    entity,
                    if m.r#type.is_empty() { "auto" } else { &m.r#type },
                    if m.subtype.is_empty() { "auto" } else { &m.subtype },
                    if m.skip { "SKIP" } else { "IMPORT" }
                ));
            }
            lines.push("Skipped accounts and their transactions are excluded. Existing imported accounts retain their ledger mapping.".into());
            return lines;
        }
        if let Some(r) = &self.preview {
            let mut lines = vec![format!("Destination: {}", self.account_label), summary(r.import.as_ref()), "Parsed rows (up to 200); import processes the full file:".into()];
            lines.extend(r.lines.iter().map(|l| format!("{} {} {} | {} | {} {}", l.date, l.amount, l.currency, l.description, l.status, l.note)));
            return lines;
        }
        let mut lines: Vec<String> = LABELS.iter().enumerate().map(|(i, label)| format!("{} {label}: {}", if i == self.selected { ">" } else { " " }, self.fields[i])).collect();
        lines.push(format!("Destination: {}", self.account_label));
        lines.push(format!("Headers: {}", self.headers.join(" | ")));
        lines.extend(self.samples.iter().cloned());
        lines
    }
}

pub(crate) fn coverage_warning(import: &pb::Import) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(&import.options).ok()?.get("coverage_warning")?.as_str().map(str::to_owned)
}

fn summary(import: Option<&pb::Import>) -> String {
    import
        .map(|i| {
            format!(
                "{} #{} ({}): {} lines, {} created, {} matched, {} duplicates, {} skipped, {} errors {}{}",
                i.status,
                i.id,
                i.source,
                i.lines_count,
                i.created_count,
                i.matched_count,
                i.duplicate_count,
                i.skipped_count,
                i.error_count,
                i.error,
                coverage_warning(i).map(|w| format!("\nWarning: {w}")).unwrap_or_default()
            )
        })
        .unwrap_or_else(|| "No import summary returned".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_controls_and_confirmation_fit_a_standard_terminal() {
        use crate::app::{App, Modal, Screen};
        use ratatui::{backend::TestBackend, Terminal};
        for (width, height) in [(80, 24), (120, 40)] {
            let mut app = App::new("http://127.0.0.1:1");
            app.screen = Screen::Imports;
            let mut form = ImportForm::new("bank.csv".into(), vec![], 1, "Bank EUR".into());
            form.selected = 15;
            app.modal = Modal::Import(Box::new(form));
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            let rendered: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
            assert!(rendered.contains("Use mapping (true/false): false"));
            assert!(rendered.contains("Ctrl-P preview"));
            if let Modal::Import(form) = &mut app.modal {
                form.reviewing = true;
            }
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            let rendered: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
            assert!(rendered.contains("y import"));
            assert!(rendered.contains("Esc back"));
        }
    }

    #[test]
    fn csv_mapping_rejects_ambiguous_amounts_and_invalid_format_settings() {
        let mut form = ImportForm::new("bank.csv".into(), vec![], 1, "Bank".into());
        form.fields[15] = "true".into();
        form.fields[2] = "Date".into();
        form.fields[4] = "Amount".into();
        assert!(form.mapping().is_ok());
        for (index, value) in [(1, "-1"), (1, "1.5"), (0, "::"), (5, "Debit"), (11, "x"), (12, "yes")] {
            let mut invalid = form.clone();
            invalid.fields[index] = value.into();
            assert!(invalid.mapping().is_err(), "{index}: {value}");
        }
    }

    #[tokio::test]
    async fn leaving_preview_invalidates_confirmation_and_cancel_closes_without_a_server() {
        let mut client = Client::new("http://127.0.0.1:1");
        let mut form = ImportForm::new("bank.csv".into(), vec![], 1, "Bank".into());
        form.preview = Some(pb::UploadImportResponse::default());
        form.reviewing = true;
        let escape = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!form.key(escape, &mut client).await);
        assert!(!form.reviewing);
        assert!(form.preview.is_none());
        assert!(form.key(escape, &mut client).await);
        assert!(!form.finished);
    }
}
