//! Application state and key handling. Rendering lives in `ui.rs`.

use crate::sort::{Order, Scope};

use std::collections::HashMap;

use chrono::Datelike;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use means_proto::v1 as pb;

use crate::client::Client;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Overview,
    Accounts,
    Journal,
    Review,
    Capture,
    Imports,
    Reports,
    Connections,
    Payees,
    Sharing,
}

impl Screen {
    pub const ALL: [Screen; 10] =
        [Screen::Overview, Screen::Accounts, Screen::Journal, Screen::Review, Screen::Capture, Screen::Imports, Screen::Reports, Screen::Connections, Screen::Payees, Screen::Sharing];

    pub fn title(self) -> &'static str {
        match self {
            Screen::Overview => "Overview",
            Screen::Accounts => "Accounts",
            Screen::Journal => "Journal",
            Screen::Review => "Review",
            Screen::Capture => "Capture",
            Screen::Imports => "Imports",
            Screen::Reports => "Reports",
            Screen::Connections => "Connections",
            Screen::Payees => "Payees",
            Screen::Sharing => "Sharing",
        }
    }

    pub fn index(self) -> usize {
        Screen::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportKind {
    TrialBalance,
    IncomeStatement,
    BalanceSheet,
    ExpenseClass,
    ExpenseTag,
    ExpensePayee,
    Budgets,
}

impl ReportKind {
    pub fn title(self) -> &'static str {
        match self {
            ReportKind::TrialBalance => "Trial balance",
            ReportKind::IncomeStatement => "Income statement",
            ReportKind::BalanceSheet => "Balance sheet",
            ReportKind::ExpenseClass => "Expenses by class",
            ReportKind::ExpenseTag => "Expenses by tag (overlapping)",
            ReportKind::ExpensePayee => "Expenses by canonical payee / unresolved booked text",
            ReportKind::Budgets => "Budgets",
        }
    }
}

/// A filterable list of (id, label, hint).
#[derive(Debug, Clone, Default)]
pub struct Picker {
    pub title: String,
    pub items: Vec<(i64, String, String)>,
    pub filter: String,
    pub selected: usize,
}

impl Picker {
    pub fn new(title: &str, items: Vec<(i64, String, String)>) -> Picker {
        Picker { title: title.to_string(), items, filter: String::new(), selected: 0 }
    }

    pub fn visible(&self) -> Vec<&(i64, String, String)> {
        let f = self.filter.to_lowercase();
        let words: Vec<&str> = f.split_whitespace().collect();
        self.items.iter().filter(|(_, label, hint)| words.iter().all(|w| label.to_lowercase().contains(w) || hint.to_lowercase().contains(w))).collect()
    }

    pub fn current(&self) -> Option<(i64, String)> {
        self.visible().get(self.selected).map(|(id, label, _)| (*id, label.clone()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerTarget {
    Sort(Scope),
    ConnectionAccount(i64),
    RefundOriginal(i64),
    ReviewContra,
    CaptureAccount,
    CaptureContra,
    ImportAccount,
    /// Assign the account of a pending inbox import (the import's id).
    CompletePending(i64),
    /// New parent for the account being moved (its id); item id 0 means the top level.
    MoveAccount(i64),
    /// Merge this account (id) into the picked one.
    MergeAccount(i64),
    /// New category for a two-leg entry seen from a ledger (the entry's id).
    Recategorize(i64),
    SplitCategory {
        source: i64,
        query: String,
    },
    /// Post every listed draft (same payee) to the picked account.
    ReviewBatch(Vec<i64>),
    /// Create a rule "description contains token -> account" and run it.
    ReviewRule {
        token: String,
        entity_id: i64,
    },
}

/// The words that identify a payee across noisy bank strings: lowercase, digits and
/// symbols dropped, stopwords out, first three words.
pub fn payee_key(payee: &str, description: &str) -> String {
    let text = if payee.trim().is_empty() { description } else { payee };
    let cleaned: String = text.to_lowercase().chars().map(|c| if c.is_alphabetic() || c.is_whitespace() { c } else { ' ' }).collect();
    cleaned.split_whitespace().filter(|w| w.len() > 1 && !["the", "und", "and", "de", "da", "do", "gmbh", "ltd", "llc", "sa", "lda"].contains(w)).take(3).collect::<Vec<_>>().join(" ")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputTarget {
    ConnectionCutoff(i64),
    ReportFrom,
    ReportTo,
    ReportTag,
    RefundOriginal(i64),
    JournalQuery,
    CategoryFilter(i64),
    ImportPath,
    /// Rename the account (id).
    AccountRename(i64),
    /// Set the account's code (id).
    AccountCode(i64),
    /// Set the account's class (id).
    AccountClass(i64),
    /// Path of a new account (an index into ATYPES gives the type).
    AccountNew {
        type_idx: usize,
    },
    /// Replace the tags of an entry (its id).
    Tags(i64),
}

pub const ATYPES: [&str; 5] = ["asset", "liability", "equity", "income", "expense"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmAction {
    PullConnection(i64),
    UnmapConnection(i64),
    DeleteBudget(i64),
    LinkRefund { line: i64, original: i64 },
    DeleteDraft(i64),
    MergeAccounts { source: i64, target: i64 },
    SplitCategory { source: i64, target: i64, query: String },
}

#[derive(Debug, Clone)]
pub struct PostingPreview {
    pub request: pb::ReviewPostingRequest,
    pub entry: pb::JournalEntry,
    pub back: Box<Modal>,
    pub remaining: Vec<pb::ReviewPostingRequest>,
    pub scroll: u16,
    pub error: String,
}

#[derive(Debug, Clone)]
pub enum Modal {
    None,
    Split(Box<crate::split::Editor>),
    PostingPreview(Box<PostingPreview>),
    Help,
    Picker(Picker, PickerTarget),
    Input { title: String, value: String, target: InputTarget },
    Confirm { text: String, action: ConfirmAction },
    EntryDetail(Box<pb::JournalEntry>),
    Import(Box<crate::import::ImportForm>),
    Budget(Box<crate::budget::Form>),
    Payee(Box<crate::payees::Form>),
    Sharing(Box<crate::sharing::Form>),
    Connection(Box<crate::connections::Form>),
    ConnectionStatus(String),
}

#[derive(Debug, Clone)]
pub struct LedgerView {
    pub account: pb::Account,
    pub rows: Vec<pb::LedgerRow>,
    pub opening: String,
    pub closing: String,
    pub commodity: String,
    pub selected: usize,
}

#[derive(Debug, Clone)]
pub struct CaptureForm {
    pub kind: usize, // 0 expense, 1 income, 2 transfer
    pub account: Option<(i64, String)>,
    pub amount: String,
    pub contra: Option<(i64, String)>,
    pub payee: String,
    pub date: String,
    pub notes: String,
    pub field: usize,
    pub result: String,
    /// Split lines: (category id, its path, amount as typed; "" = the rest).
    pub splits: Vec<crate::split::Row>,
    /// Space-separated key:value tags for the posted entry.
    pub tags: String,
}

pub const KINDS: [&str; 3] = ["expense", "income", "transfer"];
pub const CAPTURE_FIELDS: [&str; 8] = ["Kind", "Account", "Amount", "Category / To", "Payee", "Date", "Notes", "Tags"];

impl CaptureForm {
    pub fn new(today: &str) -> CaptureForm {
        CaptureForm {
            kind: 0,
            account: None,
            amount: String::new(),
            contra: None,
            payee: String::new(),
            date: today.to_string(),
            notes: String::new(),
            field: 0,
            result: String::new(),
            splits: Vec::new(),
            tags: String::new(),
        }
    }
}

pub struct App {
    pub client: Client,
    pub screen: Screen,
    pub status: String,
    pub quit: bool,
    /// A screen switch asked for data; the loop draws once, then loads.
    pub pending_load: bool,
    pub modal: Modal,
    pub today: String,
    // shared
    pub entities: Vec<pb::Entity>,
    pub entity_idx: usize,
    pub precisions: HashMap<String, usize>,
    pub info: Option<pb::GetStatusResponse>,
    pub accounts: Vec<pb::Account>,
    // overview
    pub net_worth: Option<pb::NetWorthResponse>,
    pub recent: Vec<pb::JournalEntry>,
    // accounts
    pub accounts_sel: usize,
    pub ledger: Option<LedgerView>,
    pub entry_scroll: u16,
    pub account_sort: Order,
    pub ledger_sort: Order,
    pub journal_sort: Order,
    pub review_sort: Order,
    pub expense_sort: Order,
    // journal
    pub entries: Vec<pb::JournalEntry>,
    pub entries_total: i32,
    pub entries_sel: usize,
    pub journal_status: String,
    pub journal_query: String,
    // review
    pub drafts: Vec<pb::JournalEntry>,
    pub unreviewed: Vec<pb::JournalEntry>,
    pub unmatched: Vec<pb::StatementLine>,
    pub review_sel: usize,
    // capture
    pub capture: CaptureForm,
    // imports
    pub imports: Vec<pb::Import>,
    pub imports_sel: usize,
    pub import_path: String,
    // reports
    pub report_kind: ReportKind,
    pub report: Option<pb::ReportResponse>,
    pub report_sel: usize,
    pub report_from: String,
    pub report_to: String,
    pub report_tag: String,
    pub sharing: pb::SharingResponse,
    pub sharing_sel: usize,
    pub sharing_scroll: usize,
    pub replica_return: Option<String>,
    pub payees: Vec<pb::Payee>,
    pub payee_sel: usize,
    pub budgets: Vec<pb::BudgetProgress>,
    pub budgets_by_tag: bool,
    pub connections: pb::ListConnectionsResponse,
    pub connections_sel: usize,
    connection_poll: Option<tokio::task::JoinHandle<anyhow::Result<pb::ListConnectionsResponse>>>,
    connection_polled: std::time::Instant,
}

impl App {
    pub fn new(server: &str) -> App {
        let today = chrono::Local::now().date_naive().to_string();
        App {
            client: Client::new(server),
            screen: Screen::Overview,
            pending_load: false,
            status: String::new(),
            quit: false,
            modal: Modal::None,
            capture: CaptureForm::new(&today),
            today,
            entities: Vec::new(),
            entity_idx: 0,
            precisions: HashMap::new(),
            info: None,
            accounts: Vec::new(),
            net_worth: None,
            recent: Vec::new(),
            accounts_sel: 0,
            ledger: None,
            entry_scroll: 0,
            account_sort: Order::NameAsc,
            ledger_sort: Order::Newest,
            journal_sort: Order::Newest,
            review_sort: Order::Newest,
            expense_sort: Order::NameAsc,
            entries: Vec::new(),
            entries_total: 0,
            entries_sel: 0,
            journal_status: String::new(),
            journal_query: String::new(),
            drafts: Vec::new(),
            unreviewed: Vec::new(),
            unmatched: Vec::new(),
            review_sel: 0,
            imports: Vec::new(),
            imports_sel: 0,
            import_path: String::new(),
            report_kind: ReportKind::TrialBalance,
            report: None,
            report_sel: 0,
            report_from: chrono::Local::now().date_naive().with_day(1).unwrap().to_string(),
            report_to: chrono::Local::now().date_naive().to_string(),
            report_tag: String::new(),
            sharing: Default::default(),
            sharing_sel: 0,
            sharing_scroll: 0,
            replica_return: None,
            payees: Vec::new(),
            payee_sel: 0,
            budgets: Vec::new(),
            budgets_by_tag: false,
            connections: Default::default(),
            connections_sel: 0,
            connection_poll: None,
            connection_polled: std::time::Instant::now(),
        }
    }

    pub fn entity(&self) -> Option<&pb::Entity> {
        self.entities.get(self.entity_idx)
    }

    pub fn entity_id(&self) -> i64 {
        self.entity().map(|e| e.id).unwrap_or(0)
    }

    pub fn precision(&self, commodity: &str) -> usize {
        self.precisions.get(commodity).copied().unwrap_or(2)
    }

    pub fn functional_precision(&self) -> usize {
        self.entity().map(|e| self.precision(&e.currency)).unwrap_or(2)
    }

    fn err(&mut self, e: anyhow::Error) {
        self.status = format!("error: {e}");
    }

    // ------------------------------------------------------------------
    // Loading
    // ------------------------------------------------------------------

    pub async fn load_common(&mut self) {
        match self.client.list_entities(pb::ListEntitiesRequest { include_archived: false }).await {
            Ok(r) => {
                self.entities = r.entities;
                if self.entity_idx >= self.entities.len() {
                    self.entity_idx = 0;
                }
            }
            Err(e) => self.err(e),
        }
        if let Ok(r) = self.client.list_commodities(pb::ListCommoditiesRequest {}).await {
            self.precisions = r.commodities.iter().map(|c| (c.code.clone(), c.precision.max(0) as usize)).collect();
        }
        match self.client.get_status(pb::GetStatusRequest {}).await {
            Ok(r) => self.info = Some(r),
            Err(e) => self.err(e),
        }
    }

    pub async fn load_accounts(&mut self) {
        let eid = self.entity_id();
        match self.client.list_accounts(pb::ListAccountsRequest { entity_id: eid, include_closed: false, as_of: String::new(), r#type: String::new() }).await {
            Ok(r) => {
                self.accounts = r.accounts;
                self.apply_sort(Scope::Accounts);
                if self.accounts_sel >= self.accounts.len() {
                    self.accounts_sel = 0;
                }
            }
            Err(e) => self.err(e),
        }
    }

    pub async fn load_screen(&mut self) {
        self.status.clear();
        self.load_common().await;
        match self.screen {
            Screen::Overview => {
                self.net_worth = None;
                self.recent.clear();
                let entity_id = self.entity_id();
                if entity_id == 0 {
                    return;
                }
                match self.client.net_worth(pb::NetWorthRequest { entity_id, currency: String::new(), as_of: String::new() }).await {
                    Ok(r) => self.net_worth = Some(r),
                    Err(e) => self.err(e),
                }
                match self
                    .client
                    .list_journal_entries(pb::ListJournalEntriesRequest {
                        entity_id,
                        account_id: 0,
                        status: String::new(),
                        from: String::new(),
                        to: String::new(),
                        query: String::new(),
                        limit: 12,
                        offset: 0,
                        origin: String::new(),
                        only_unreviewed: false,
                    })
                    .await
                {
                    Ok(r) => self.recent = r.entries,
                    Err(e) => self.err(e),
                }
            }
            Screen::Accounts => self.load_accounts().await,
            Screen::Journal => self.load_journal().await,
            Screen::Review => {
                self.load_accounts().await;
                match self
                    .client
                    .list_journal_entries(pb::ListJournalEntriesRequest {
                        entity_id: 0,
                        account_id: 0,
                        status: "draft".into(),
                        from: String::new(),
                        to: String::new(),
                        query: String::new(),
                        limit: 500,
                        offset: 0,
                        origin: String::new(),
                        only_unreviewed: false,
                    })
                    .await
                {
                    Ok(r) => self.drafts = r.entries,
                    Err(e) => self.err(e),
                }
                match self.client.list_journal_entries(pb::ListJournalEntriesRequest { status: "posted".into(), only_unreviewed: true, limit: 500, ..Default::default() }).await {
                    Ok(r) => self.unreviewed = r.entries,
                    Err(e) => self.err(e),
                }
                match self.client.list_statement_lines(pb::ListStatementLinesRequest { account_id: 0, status: "unmatched".into(), limit: 500, import_id: 0 }).await {
                    Ok(r) => self.unmatched = r.lines,
                    Err(e) => self.err(e),
                }
                self.apply_sort(Scope::Review);
                let n = self.review_len();
                if n == 0 {
                    self.review_sel = 0;
                } else if self.review_sel >= n {
                    self.review_sel = n - 1;
                }
            }
            Screen::Capture => self.load_accounts().await,
            Screen::Imports => {
                self.load_accounts().await;
                match self.client.list_imports(pb::ListImportsRequest { account_id: 0, limit: 200 }).await {
                    Ok(r) => self.imports = r.imports,
                    Err(e) => self.err(e),
                }
            }
            Screen::Reports => self.load_report().await,
            Screen::Connections => self.load_connections().await,
            Screen::Sharing => {
                if self.info.as_ref().is_some_and(|i| !i.replica_status.is_empty()) {
                    self.sharing = pb::SharingResponse { lines: vec!["Received vault: read-only. Esc returns to your books.".into()], ..Default::default() };
                } else {
                    match self.client.sharing(pb::SharingRequest { operation: "list".into(), ..Default::default() }).await {
                        Ok(r) => self.sharing = r,
                        Err(e) => self.status = format!("error: {e}"),
                    }
                }
            }
            Screen::Payees => match self.client.list_payees(pb::ListPayeesRequest { entity_id: self.entity_id() }).await {
                Ok(r) => {
                    self.payees = r.payees;
                    self.payee_sel = self.payee_sel.min(self.payees.len().saturating_sub(1));
                }
                Err(e) => self.err(e),
            },
        }
    }

    async fn load_journal(&mut self) {
        let eid = self.entity_id();
        match self
            .client
            .list_journal_entries(pb::ListJournalEntriesRequest {
                entity_id: eid,
                account_id: 0,
                status: self.journal_status.clone(),
                from: String::new(),
                to: String::new(),
                query: self.journal_query.clone(),
                limit: 300,
                offset: 0,
                origin: String::new(),
                only_unreviewed: false,
            })
            .await
        {
            Ok(r) => {
                self.entries = r.entries;
                self.apply_sort(Scope::Journal);
                self.entries_total = r.total;
                if self.entries_sel >= self.entries.len() {
                    self.entries_sel = self.entries.len().saturating_sub(1);
                }
            }
            Err(e) => self.err(e),
        }
    }

    async fn load_connections(&mut self) {
        match self.client.list_connections(pb::ListConnectionsRequest {}).await {
            Ok(r) => self.accept_connections(r),
            Err(e) => self.err(e),
        }
    }
    fn accept_connections(&mut self, r: pb::ListConnectionsResponse) {
        if let Modal::Connection(form) = &mut self.modal {
            if let Some(job) = r.jobs.iter().find(|j| j.operation == "banks" && !j.banks.is_empty()) {
                form.banks = job.banks.clone();
            }
            if let Some(job) = r.jobs.iter().find(|j| j.provider == crate::connections::PROVIDERS[form.provider]) {
                let version = format!("{}:{}:{}", job.id, job.state, job.authorization_url);
                if version != form.last_job_version {
                    form.last_job_version = version;
                    form.message = format!("{}: {}", job.state, job.message);
                    if !job.authorization_url.is_empty() {
                        form.message = format!("Authorize in your browser: {}", job.authorization_url);
                    }
                }
            }
        }
        self.connections = r;
        self.connections_sel = self.connections_sel.min((self.connections.providers.len() + self.connections.accounts.len()).saturating_sub(1));
    }
    /// Poll on a separate task so a busy importer cannot block terminal input.
    pub async fn tick_connections(&mut self) {
        if self.connection_poll.as_ref().is_some_and(|h| h.is_finished()) {
            if let Some(task) = self.connection_poll.take() {
                match task.await {
                    Ok(Ok(r)) => self.accept_connections(r),
                    Ok(Err(e)) => self.err(e),
                    Err(_) => self.status = "Connection status unavailable".into(),
                }
            }
        }
        let needed = self.screen == Screen::Connections || matches!(self.modal, Modal::Connection(_)) || self.connections.jobs.iter().any(|j| j.state == "running");
        if needed && self.connection_poll.is_none() && self.connection_polled.elapsed() >= std::time::Duration::from_secs(1) {
            self.connection_polled = std::time::Instant::now();
            let mut client = self.client.clone();
            self.connection_poll = Some(tokio::spawn(async move { client.list_connections(pb::ListConnectionsRequest {}).await }));
        }
    }
    pub fn selected_connection(&self) -> Option<&pb::BankConnection> {
        self.connections_sel.checked_sub(self.connections.providers.len()).and_then(|i| self.connections.accounts.get(i))
    }
    pub fn connection_job(&self) -> Option<&pb::ConnectionJob> {
        self.connections
            .jobs
            .iter()
            .find(|j| j.state == "running")
            .or_else(|| self.selected_connection().and_then(|a| self.connections.jobs.iter().find(|j| j.target == format!("account:{}", a.id) || (!a.item_id.is_empty() && j.target == a.item_id))))
            .or_else(|| self.connections.jobs.first())
    }
    fn setup_connection(&mut self) {
        let provider =
            self.selected_connection().map(crate::connections::provider).or_else(|| self.connections.providers.get(self.connections_sel).map(|p| p.id.clone())).unwrap_or_else(|| "pluggy".into());
        let mut form = crate::connections::Form::new(&provider);
        if let Some(a) = self.selected_connection() {
            form.item = a.item_id.clone();
        }
        if let Some(job) = self.connections.jobs.iter().find(|j| j.operation == "banks" && !j.banks.is_empty()) {
            form.banks = job.banks.clone();
        }
        self.modal = Modal::Connection(Box::new(form));
    }
    async fn map_connection(&mut self) {
        let Some(a) = self.selected_connection().cloned() else {
            self.setup_connection();
            return;
        };
        match self.client.list_accounts(pb::ListAccountsRequest { include_closed: false, ..Default::default() }).await {
            Ok(r) => {
                let items = r
                    .accounts
                    .into_iter()
                    .filter(|t| {
                        !t.placeholder
                            && t.commodity == a.currency
                            && matches!(t.r#type.as_str(), "asset" | "liability")
                            && (!a.provider_type.eq_ignore_ascii_case("credit") || t.r#type == "liability")
                    })
                    .map(|t| {
                        let entity = self.entities.iter().find(|e| e.id == t.entity_id).map(|e| e.name.as_str()).unwrap_or("");
                        (t.id, format!("{entity} / {}", t.path), t.commodity)
                    })
                    .collect();
                self.modal = Modal::Picker(Picker::new("Destination for future imports (create accounts in Accounts)", items), PickerTarget::ConnectionAccount(a.id));
            }
            Err(e) => self.err(e),
        }
    }
    async fn save_connection(&mut self, id: i64, account: i64, cutoff: String) {
        match self.client.configure_connection(pb::ConfigureConnectionRequest { id, account_id: account, booked_from: cutoff }).await {
            Ok(_) => {
                self.load_connections().await;
                self.status = "Connection preferences saved; existing imports are unchanged".into();
            }
            Err(e) => self.err(e),
        }
    }

    async fn load_report(&mut self) {
        let eid = self.entity_id();
        self.report = None;
        self.budgets.clear();
        self.report_sel = 0;
        if eid == 0 {
            return;
        }
        if self.report_kind == ReportKind::Budgets {
            match self.client.list_budgets(pb::ListBudgetsRequest { entity_id: eid, on: String::new(), tag: self.report_tag.clone() }).await {
                Ok(r) => {
                    self.budgets = r.rows;
                    self.sort_budgets();
                }
                Err(e) => self.err(e),
            }
            return;
        }
        let r = match self.report_kind {
            ReportKind::TrialBalance => self.client.trial_balance(pb::TrialBalanceRequest { entity_id: eid, as_of: String::new() }).await,
            ReportKind::IncomeStatement => self.client.income_statement(pb::IncomeStatementRequest { entity_id: eid, from: self.report_from.clone(), to: self.report_to.clone() }).await,
            ReportKind::BalanceSheet => self.client.balance_sheet(pb::BalanceSheetRequest { entity_id: eid, as_of: String::new(), currency: String::new() }).await,
            ReportKind::ExpenseClass => {
                self.client.expenses_by_class(pb::ExpensesByClassRequest { entity_id: eid, from: self.report_from.clone(), to: self.report_to.clone(), tag: self.report_tag.clone() }).await.map(|r| {
                    pb::ReportResponse {
                        rows: r.rows.into_iter().map(|r| pb::ReportRow { name: if r.class.is_empty() { "Unclassified".into() } else { r.class }, amount: r.amount, ..Default::default() }).collect(),
                        net: r.total,
                        currency: r.currency,
                        ..Default::default()
                    }
                })
            }
            ReportKind::ExpensePayee => {
                self.client.payee_expenses(pb::PayeeExpensesRequest { entity_id: eid, from: self.report_from.clone(), to: self.report_to.clone(), tag: self.report_tag.clone() }).await.map(|r| {
                    pb::ReportResponse {
                        rows: r
                            .rows
                            .into_iter()
                            .map(|r| pb::ReportRow { name: format!("{}{}", if r.payee_id == 0 { "[unresolved] " } else { "" }, r.name), amount: r.amount, ..Default::default() })
                            .collect(),
                        net: r.total,
                        currency: r.currency,
                        ..Default::default()
                    }
                })
            }
            ReportKind::ExpenseTag => {
                self.client.expenses_by_tag(pb::ExpensesByTagRequest { entity_id: eid, from: self.report_from.clone(), to: self.report_to.clone(), tag: self.report_tag.clone() }).await.map(|r| {
                    pb::ReportResponse {
                        rows: r.rows.into_iter().map(|r| pb::ReportRow { name: r.tag.unwrap_or_else(|| "Untagged".into()), amount: r.amount, ..Default::default() }).collect(),
                        net: r.total,
                        currency: r.currency,
                        ..Default::default()
                    }
                })
            }
            ReportKind::Budgets => unreachable!(),
        };
        match r {
            Ok(rep) => {
                self.report = Some(rep);
                self.apply_sort(Scope::Expenses);
            }
            Err(e) => self.err(e),
        }
    }

    fn sort_budgets(&mut self) {
        let by_tag = self.budgets_by_tag;
        self.budgets.sort_by_key(|r| (if by_tag { r.budget.as_ref().map(|b| b.tag.clone()).unwrap_or_default() } else { r.group.clone() }, r.budget.as_ref().map(|b| (b.name.clone(), b.id))));
    }

    async fn edit_budget(&mut self, existing: Option<pb::Budget>) {
        let eid = self.entity_id();
        if eid == 0 {
            return;
        }
        match self.client.list_accounts(pb::ListAccountsRequest { entity_id: eid, include_closed: true, ..Default::default() }).await {
            Ok(r) => self.modal = Modal::Budget(Box::new(crate::budget::Form::new(eid, self.entity().map(|e| e.currency.clone()).unwrap_or_default(), r.accounts, existing))),
            Err(e) => self.err(e),
        }
    }

    fn sort_scope(&self) -> Option<Scope> {
        match self.screen {
            Screen::Accounts => Some(if self.ledger.is_some() { Scope::Ledger } else { Scope::Accounts }),
            Screen::Journal => Some(Scope::Journal),
            Screen::Review => Some(Scope::Review),
            Screen::Reports if matches!(self.report_kind, ReportKind::ExpenseClass | ReportKind::ExpenseTag | ReportKind::ExpensePayee) => Some(Scope::Expenses),
            _ => None,
        }
    }
    fn apply_sort(&mut self, scope: Scope) {
        let entry_key = |e: &pb::JournalEntry| (e.date.clone(), if e.display_payee.is_empty() { e.payee.clone() } else { e.display_payee.clone() }, e.amount_functional.clone(), e.id);
        match scope {
            Scope::Accounts => self.account_sort.apply(&mut self.accounts, |a| (String::new(), a.path.clone(), a.balance_functional.clone(), a.id)),
            Scope::Ledger => {
                if let Some(l) = &mut self.ledger {
                    self.ledger_sort.apply(&mut l.rows, |r| {
                        (
                            format!("{} {:020}", r.date, r.journal_entry_id),
                            if r.display_payee.is_empty() { r.payee.clone() } else { r.display_payee.clone() },
                            if crate::fmt::sign(&r.debit) == 0 { r.credit.clone() } else { r.debit.clone() },
                            r.posting_id,
                        )
                    });
                }
            }
            Scope::Journal => self.journal_sort.apply(&mut self.entries, entry_key),
            Scope::Review => {
                self.review_sort.apply(&mut self.drafts, entry_key);
                self.review_sort.apply(&mut self.unreviewed, entry_key);
                self.review_sort.apply(&mut self.unmatched, |r| (r.date.clone(), r.description.clone(), r.amount.clone(), r.id));
            }
            Scope::Expenses => {
                if matches!(self.report_kind, ReportKind::ExpenseClass | ReportKind::ExpenseTag | ReportKind::ExpensePayee) {
                    if let Some(r) = &mut self.report {
                        self.expense_sort.apply(&mut r.rows, |r| (String::new(), r.name.clone(), r.amount.clone(), r.account_id));
                    }
                }
            }
        }
    }
    fn choose_sort(&mut self, scope: Scope, order: Order) {
        match scope {
            Scope::Accounts => {
                self.account_sort = order;
                self.accounts_sel = 0;
            }
            Scope::Ledger => {
                self.ledger_sort = order;
                if let Some(l) = &mut self.ledger {
                    l.selected = 0;
                }
            }
            Scope::Journal => {
                self.journal_sort = order;
                self.entries_sel = 0;
            }
            Scope::Review => {
                self.review_sort = order;
                self.review_sel = 0;
            }
            Scope::Expenses => {
                self.expense_sort = order;
                self.report_sel = 0;
            }
        }
        self.apply_sort(scope);
        self.status = format!("Sorted by {}{}", order.label(), if scope == Scope::Review { " within drafts, unreviewed entries and unmatched lines" } else { "" });
    }

    async fn open_ledger(&mut self) {
        let Some(acc) = self.accounts.get(self.accounts_sel).cloned() else { return };
        if acc.placeholder {
            self.status = "placeholders have no ledger; pick a child account".into();
            return;
        }
        match self.client.general_ledger(pb::GeneralLedgerRequest { account_id: acc.id, from: String::new(), to: String::new(), limit: 2000, newest_first: true }).await {
            Ok(r) => {
                let selected = 0;
                self.ledger = Some(LedgerView { account: acc, rows: r.rows, opening: r.opening_balance, closing: r.closing_balance, commodity: r.commodity, selected });
                self.apply_sort(Scope::Ledger);
            }
            Err(e) => self.err(e),
        }
    }

    // ------------------------------------------------------------------
    // Actions
    // ------------------------------------------------------------------

    fn account_items(&self, filter: &dyn Fn(&pb::Account) -> bool) -> Vec<(i64, String, String)> {
        self.accounts.iter().filter(|a| !a.placeholder && filter(a)).map(|a| (a.id, a.path.clone(), format!("{} {}", a.commodity, a.subtype))).collect()
    }

    async fn preview_postings(&mut self, mut requests: Vec<pb::ReviewPostingRequest>, back: Modal) {
        if requests.is_empty() {
            return;
        }
        let request = requests.remove(0);
        match self.client.review_posting(request.clone()).await {
            Ok(r) => {
                self.modal = Modal::PostingPreview(Box::new(PostingPreview {
                    request: pb::ReviewPostingRequest { confirmation: r.confirmation, ..request },
                    entry: r.entry.unwrap_or_default(),
                    back: Box::new(back),
                    remaining: requests,
                    scroll: 0,
                    error: String::new(),
                }))
            }
            Err(e) => {
                self.err(e);
                self.modal = back;
            }
        }
    }
    async fn post_draft_with(&mut self, contra: i64) {
        let Some(entry) = self.review_draft() else { return };
        let request = pb::ReviewPostingRequest {
            categorize: Some(pb::PostDraftRequest { id: entry.id, splits: vec![pb::SplitInput { account_id: contra, ..Default::default() }], payee: None }),
            ..Default::default()
        };
        self.preview_postings(vec![request], Modal::None).await;
    }
    async fn open_entry_split(&mut self, entry: pb::JournalEntry) {
        if entry.status == "void" {
            self.status = "Void entries cannot be split".into();
            return;
        }
        self.align_entity(entry.entity_id).await;
        let accounts = match self.client.list_accounts(pb::ListAccountsRequest { entity_id: entry.entity_id, include_closed: true, ..Default::default() }).await {
            Ok(r) => r.accounts,
            Err(e) => {
                self.err(e);
                return;
            }
        };
        let banks: Vec<_> = entry.postings.iter().filter(|p| accounts.iter().any(|a| a.id == p.account_id && matches!(a.r#type.as_str(), "asset" | "liability"))).collect();
        if banks.len() != 1 {
            self.status = "Splitting needs one bank/card posting and income/expense categories".into();
            return;
        }
        let bank = banks[0];
        if entry.postings.iter().filter(|p| p.account_id != bank.account_id).any(|p| !accounts.iter().any(|a| a.id == p.account_id && matches!(a.r#type.as_str(), "income" | "expense"))) {
            self.status = "Only income/expense categories can be split here".into();
            return;
        }
        let items = accounts
            .iter()
            .filter(|a| !a.placeholder && a.closed_at.is_empty() && a.system_role != "suspense" && matches!(a.r#type.as_str(), "expense" | "income"))
            .map(|a| (a.id, a.path.clone(), a.commodity.clone()))
            .collect();
        let kind = if crate::fmt::sign(&bank.quantity) < 0 { "expense" } else { "income" };
        self.modal = Modal::Split(Box::new(crate::split::Editor::new(
            crate::split::Target::Existing(entry.id),
            bank.quantity.trim_start_matches('-').into(),
            bank.commodity.clone(),
            vec![],
            items,
            kind.into(),
        )));
    }

    async fn open_review_split(&mut self) {
        if let Some(entry) = self.review_unreviewed().cloned() {
            self.open_entry_split(entry).await;
            return;
        }
        let Some(entry) = self.review_draft().cloned() else {
            self.status = "Select a draft or posted entry to split; unmatched statement lines are not drafts".into();
            return;
        };
        self.align_entity(entry.entity_id).await;
        if self.entity_id() != entry.entity_id {
            self.status = format!("Cannot load the vault for draft #{}; refresh Review", entry.id);
            return;
        }
        if !self.accounts.iter().any(|a| a.entity_id == entry.entity_id && a.system_role == "suspense") {
            self.load_accounts().await;
        }
        let Some(suspense) = self.accounts.iter().find(|a| a.entity_id == entry.entity_id && a.system_role == "suspense") else {
            self.status = "Cannot find Uncategorized in this vault; refresh Review and check the server connection".into();
            return;
        };
        if entry.postings.len() != 2 || entry.postings.iter().filter(|p| p.account_id == suspense.id).count() != 1 {
            self.status = format!("Draft #{} needs one bank posting and one Uncategorized posting to split", entry.id);
            return;
        }
        // Closed bank accounts are absent from the category picker. Their original
        // posting still supplies the identity, amount and currency of the split.
        let bank = entry.postings.iter().find(|p| p.account_id != suspense.id).unwrap();
        if bank.commodity.is_empty() {
            self.status = format!("Draft #{} has no bank currency; refresh Review", entry.id);
            return;
        }
        let kind = if crate::fmt::sign(&bank.quantity) < 0 { "expense" } else { "income" };
        let items = self.account_items(&|a| a.entity_id == entry.entity_id && a.system_role != "suspense" && a.id != bank.account_id);
        self.modal =
            Modal::Split(Box::new(crate::split::Editor::new(crate::split::Target::Review(entry.id), bank.quantity.trim_start_matches('-').into(), bank.commodity.clone(), vec![], items, kind.into())));
    }

    /// Make `entity_id` the current entity and load its accounts (pickers list the current entity).
    async fn align_entity(&mut self, entity_id: i64) {
        if self.entity_id() != entity_id {
            if let Some(i) = self.entities.iter().position(|e| e.id == entity_id) {
                self.entity_idx = i;
                self.load_accounts().await;
            }
        }
    }

    async fn pick_refund(&mut self) {
        let line = self.review_line().map(|l| l.id).or_else(|| self.review_entry().map(|e| e.statement_line_id)).filter(|id| *id > 0);
        let Some(line) = line else {
            self.status = "Select a bank credit or its imported draft first".into();
            return;
        };
        match self.client.refund_candidates(pb::RefundCandidatesRequest { line_id: line, window_days: 90 }).await {
            Ok(r) => {
                let mut items: Vec<_> = r.candidates.iter().map(|e| (e.id, format!("#{} {} {} {}", e.id, e.date, e.payee, e.amount_functional), e.description.clone())).collect();
                items.push((0, "Enter an original expense ID…".into(), "For an older purchase".into()));
                self.modal = Modal::Picker(Picker::new("This credit refunds which expense? (last 90 days)", items), PickerTarget::RefundOriginal(line));
            }
            Err(e) => self.err(e),
        }
    }

    async fn confirm_refund(&mut self, line: i64, original: i64) {
        match self.client.get_journal_entry(pb::GetJournalEntryRequest { id: original }).await {
            Ok(r) => {
                if let Some(e) = r.entry {
                    self.modal = Modal::Confirm {
                        text: format!(
                            "Post credit line #{line} as a full refund of #{} {} {}? Reverse original expense splits at book value; FX difference goes to FX gain/loss. (y/n)",
                            e.id, e.date, e.payee
                        ),
                        action: ConfirmAction::LinkRefund { line, original },
                    };
                }
            }
            Err(e) => self.err(e),
        }
    }

    pub fn review_draft(&self) -> Option<&pb::JournalEntry> {
        self.drafts.get(self.review_sel)
    }

    pub fn review_unreviewed(&self) -> Option<&pb::JournalEntry> {
        self.review_sel.checked_sub(self.drafts.len()).and_then(|i| self.unreviewed.get(i))
    }

    pub fn review_entry(&self) -> Option<&pb::JournalEntry> {
        self.review_draft().or_else(|| self.review_unreviewed())
    }

    pub fn review_line(&self) -> Option<&pb::StatementLine> {
        self.review_sel.checked_sub(self.drafts.len() + self.unreviewed.len()).and_then(|i| self.unmatched.get(i))
    }

    pub fn review_len(&self) -> usize {
        self.drafts.len() + self.unreviewed.len() + self.unmatched.len()
    }

    async fn confirm_selected(&mut self) {
        let Some(id) = self.review_unreviewed().map(|e| e.id) else { return };
        match self.client.confirm_entries(pb::ConfirmEntriesRequest { ids: vec![id] }).await {
            Ok(_) => {
                self.unreviewed.retain(|e| e.id != id);
                self.load_screen().await;
                if self.status.is_empty() {
                    self.status = format!("confirmed #{id}");
                }
            }
            Err(e) => self.err(e),
        }
    }

    async fn submit_capture(&mut self) {
        let f = &self.capture;
        let Some((account_id, _)) = f.account.clone() else {
            self.status = "choose an account".into();
            return;
        };
        let contra_id = match (f.contra.clone(), f.splits.is_empty()) {
            (Some((id, _)), _) => id,
            (None, false) => f.splits[0].0, // the engine ignores it when splits are present
            (None, true) => {
                self.status = "choose a category (or the destination account)".into();
                return;
            }
        };
        if f.amount.trim().is_empty() {
            self.status = "enter an amount".into();
            return;
        }
        let splits = f.splits.iter().map(|(id, _, amount)| pb::SplitInput { account_id: *id, quantity: amount.trim().replace(',', "."), memo: String::new() }).collect();
        let req = pb::CreateSimpleEntryRequest {
            entry: Some(pb::SimpleEntryInput {
                entity_id: self.entity_id(),
                date: f.date.trim().to_string(),
                kind: KINDS[f.kind].to_string(),
                account_id,
                contra_account_id: contra_id,
                quantity: f.amount.trim().replace(',', "."),
                contra_quantity: String::new(),
                payee: f.payee.trim().to_string(),
                notes: f.notes.trim().to_string(),
                splits,
                status: "posted".into(),
                fee: String::new(),
                fee_account_id: 0,
            }),
        };
        self.preview_postings(vec![pb::ReviewPostingRequest { capture: req.entry, tags: self.capture.tags.clone(), ..Default::default() }], Modal::None).await;
    }

    async fn upload_import(&mut self, path: String, account_id: i64) {
        let content = match std::fs::read(&path) {
            Ok(c) => c,
            Err(e) => {
                self.status = format!("cannot read {path}: {e}");
                return;
            }
        };
        let filename = std::path::Path::new(&path).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let account_label = self.accounts.iter().find(|a| a.id == account_id).map(|a| format!("{} ({})", a.path, a.commodity)).unwrap_or_default();
        let mut form = crate::import::ImportForm::new(filename, content, account_id, account_label);
        form.preview(&mut self.client).await;
        self.modal = Modal::Import(Box::new(form));
    }

    async fn begin_import(&mut self, path: String) {
        let content = match std::fs::read(&path) {
            Ok(content) => content,
            Err(e) => {
                self.status = format!("cannot read {path}: {e}");
                return;
            }
        };
        if path.to_lowercase().ends_with(".atb") || content.starts_with(b"bplist00") {
            let filename = std::path::Path::new(&path).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let mut form = crate::import::ImportForm::new(filename, content, 0, String::new());
            let entity_id = self.entity_id();
            match form.inspect_tracker(&mut self.client, entity_id).await {
                Ok(()) => self.modal = Modal::Import(Box::new(form)),
                Err(e) => self.err(e),
            }
        } else {
            let items = self.account_items(&|a| a.closed_at.is_empty() && matches!(a.r#type.as_str(), "asset" | "liability"));
            self.modal = Modal::Picker(Picker::new(&format!("Statement account for {}", crate::fmt::truncate(&path, 40)), items), PickerTarget::ImportAccount);
        }
    }

    // ------------------------------------------------------------------
    // Keys
    // ------------------------------------------------------------------

    pub async fn handle_key(&mut self, key: KeyEvent) {
        if !matches!(self.modal, Modal::None) {
            self.handle_modal_key(key).await;
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if key.code == KeyCode::Esc && self.replica_return.is_some() {
            let addr = self.replica_return.take().unwrap();
            let mut owned = Client::new(&addr);
            let _ = owned.sharing(pb::SharingRequest { operation: "close-view".into(), arguments: vec![self.client.addr().to_string()], ..Default::default() }).await;
            *self = App::new(&addr);
            self.screen = Screen::Sharing;
            self.pending_load = true;
            return;
        }
        // Capture owns most keys while editing a text field.
        if self.screen == Screen::Capture && self.handle_capture_key(key).await {
            return;
        }
        if key.code == KeyCode::Char('z') {
            if let Some(scope) = self.sort_scope() {
                let dates = matches!(scope, Scope::Ledger | Scope::Journal | Scope::Review);
                self.modal = Modal::Picker(Picker::new("Sort displayed rows", Order::items(dates)), PickerTarget::Sort(scope));
                return;
            }
        }
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.modal = Modal::Help,
            KeyCode::Char('0') => {
                self.screen = Screen::Sharing;
                self.pending_load = true;
            }
            KeyCode::Char(c @ '1'..='9') => {
                self.screen = Screen::ALL[(c as u8 - b'1') as usize];
                self.ledger = None;
                self.pending_load = true;
            }
            KeyCode::Tab => {
                self.screen = Screen::ALL[(self.screen.index() + 1) % Screen::ALL.len()];
                self.ledger = None;
                self.pending_load = true;
            }
            KeyCode::BackTab => {
                self.screen = Screen::ALL[(self.screen.index() + Screen::ALL.len() - 1) % Screen::ALL.len()];
                self.ledger = None;
                self.pending_load = true;
            }
            KeyCode::Char('r') => self.pending_load = true,
            KeyCode::Char('e') => {
                if !self.entities.is_empty() {
                    self.entity_idx = (self.entity_idx + 1) % self.entities.len();
                    self.net_worth = None;
                    self.recent.clear();
                    self.ledger = None;
                    self.pending_load = true;
                }
            }
            _ => self.handle_screen_key(key).await,
        }
    }

    async fn handle_screen_key(&mut self, key: KeyEvent) {
        match self.screen {
            Screen::Overview => {}
            Screen::Accounts => {
                if key.code == KeyCode::Char('S') || (key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::SHIFT)) {
                    if let Some(id) = self.ledger.as_ref().and_then(|l| l.rows.get(l.selected)).map(|r| r.journal_entry_id) {
                        match self.client.get_journal_entry(pb::GetJournalEntryRequest { id }).await {
                            Ok(r) => {
                                if let Some(entry) = r.entry {
                                    self.open_entry_split(entry).await;
                                }
                            }
                            Err(e) => self.err(e),
                        }
                    }
                    return;
                }
                if let Some(l) = self.ledger.as_mut() {
                    match key.code {
                        KeyCode::Esc | KeyCode::Backspace => self.ledger = None,
                        KeyCode::Up | KeyCode::Char('k') => l.selected = l.selected.saturating_sub(1),
                        KeyCode::Down | KeyCode::Char('j') => l.selected = (l.selected + 1).min(l.rows.len().saturating_sub(1)),
                        KeyCode::PageUp => l.selected = l.selected.saturating_sub(20),
                        KeyCode::PageDown => l.selected = (l.selected + 20).min(l.rows.len().saturating_sub(1)),
                        KeyCode::Home | KeyCode::Char('g') => l.selected = 0,
                        KeyCode::End | KeyCode::Char('G') => l.selected = l.rows.len().saturating_sub(1),
                        KeyCode::Char('s') => {
                            if matches!(l.account.r#type.as_str(), "income" | "expense") {
                                self.modal =
                                    Modal::Input { title: "Split category: text in payee, description or memo".into(), value: String::new(), target: InputTarget::CategoryFilter(l.account.id) };
                            } else {
                                self.status = "open an income or expense category to split it".into();
                            }
                        }
                        KeyCode::Char('m') => {
                            if let Some(r) = l.rows.get(l.selected) {
                                let entry_id = r.journal_entry_id;
                                let cur = r.contra_path.clone();
                                let items = self.account_items(&|a| a.system_role != "suspense");
                                self.modal = Modal::Picker(Picker::new(&format!("Re-categorize (now {})", crate::fmt::truncate(&cur, 32)), items), PickerTarget::Recategorize(entry_id));
                            }
                        }
                        KeyCode::Enter => {
                            if let Some(row) = l.rows.get(l.selected) {
                                let id = row.journal_entry_id;
                                if let Ok(r) = self.client.get_journal_entry(pb::GetJournalEntryRequest { id }).await {
                                    if let Some(e) = r.entry {
                                        self.entry_scroll = 0;
                                        self.modal = Modal::EntryDetail(Box::new(e));
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                    return;
                }
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => self.accounts_sel = self.accounts_sel.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => self.accounts_sel = (self.accounts_sel + 1).min(self.accounts.len().saturating_sub(1)),
                    KeyCode::Home | KeyCode::Char('g') => self.accounts_sel = 0,
                    KeyCode::End | KeyCode::Char('G') => self.accounts_sel = self.accounts.len().saturating_sub(1),
                    KeyCode::Enter => self.open_ledger().await,
                    KeyCode::Char('n') => {
                        if let Some(sel) = self.accounts.get(self.accounts_sel).cloned() {
                            let type_idx = ATYPES.iter().position(|t| *t == sel.r#type).unwrap_or(4);
                            // Start from the group under the cursor: the group itself, or a leaf's parent.
                            // Editable: extend it, or backspace toward the top level.
                            let group = if sel.placeholder { Some(sel.path.clone()) } else { self.accounts.iter().find(|a| a.id == sel.parent_id).map(|a| a.path.clone()) };
                            let strip_root = |p: &str| p.split_once(':').map(|(_, rest)| rest.to_string()).unwrap_or_default();
                            let value = match group {
                                Some(g) if !strip_root(&g).is_empty() => format!("{}:", strip_root(&g)),
                                _ => String::new(),
                            };
                            self.modal = Modal::Input { title: format!("New {} account — edit the path freely, \":\" nests", sel.r#type), value, target: InputTarget::AccountNew { type_idx } };
                        }
                    }
                    KeyCode::Char('R') => {
                        if let Some(sel) = self.accounts.get(self.accounts_sel) {
                            self.modal = Modal::Input { title: format!("Rename {}", sel.path), value: sel.name.clone(), target: InputTarget::AccountRename(sel.id) };
                        }
                    }
                    KeyCode::Char('c') => {
                        if let Some(sel) = self.accounts.get(self.accounts_sel) {
                            self.modal = Modal::Input { title: format!("Code for {} (empty clears)", sel.path), value: sel.code.clone(), target: InputTarget::AccountCode(sel.id) };
                        }
                    }
                    KeyCode::Char('C') => {
                        if let Some(sel) = self.accounts.get(self.accounts_sel) {
                            self.modal = Modal::Input {
                                title: format!("Class for {}: fixed, committed, discretionary, savings, not-spending, empty", sel.path),
                                value: sel.class.clone(),
                                target: InputTarget::AccountClass(sel.id),
                            };
                        }
                    }
                    KeyCode::Char('m') => {
                        if let Some(sel) = self.accounts.get(self.accounts_sel).cloned() {
                            if !sel.system_role.is_empty() {
                                self.status = format!("{} is a system account and stays put", sel.path);
                            } else {
                                let prefix = format!("{}:", sel.path);
                                let mut items: Vec<(i64, String, String)> = self
                                    .accounts
                                    .iter()
                                    .filter(|a| a.r#type == sel.r#type && a.id != sel.id && !a.path.starts_with(&prefix) && a.closed_at.is_empty())
                                    .map(|a| (a.id, a.path.clone(), if a.placeholder { "group".into() } else { format!("{} {}", a.commodity, a.subtype) }))
                                    .collect();
                                items.insert(0, (0, "(top level)".into(), String::new()));
                                self.modal = Modal::Picker(Picker::new(&format!("Move {} under", sel.path), items), PickerTarget::MoveAccount(sel.id));
                            }
                        }
                    }
                    KeyCode::Char('M') => {
                        if let Some(sel) = self.accounts.get(self.accounts_sel).cloned() {
                            let cat = |t: &str| t == "income" || t == "expense";
                            let items: Vec<(i64, String, String)> = self
                                .accounts
                                .iter()
                                .filter(|a| a.id != sel.id && !a.placeholder && a.closed_at.is_empty() && if cat(&sel.r#type) { cat(&a.r#type) } else { a.r#type == sel.r#type })
                                .map(|a| (a.id, a.path.clone(), format!("{} postings", a.postings_count)))
                                .collect();
                            self.modal = Modal::Picker(Picker::new(&format!("Merge {} ({} postings) into", sel.path, sel.postings_count), items), PickerTarget::MergeAccount(sel.id));
                        }
                    }
                    KeyCode::Char('x') => {
                        if let Some(sel) = self.accounts.get(self.accounts_sel).cloned() {
                            match self.client.close_account(pb::CloseAccountRequest { id: sel.id, reopen: false }).await {
                                Ok(_) => {
                                    self.status = format!("closed {}", sel.path);
                                    self.load_accounts().await;
                                }
                                Err(e) => self.err(e),
                            }
                        }
                    }
                    _ => {}
                }
            }
            Screen::Journal => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.entries_sel = self.entries_sel.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => self.entries_sel = (self.entries_sel + 1).min(self.entries.len().saturating_sub(1)),
                KeyCode::PageUp => self.entries_sel = self.entries_sel.saturating_sub(20),
                KeyCode::PageDown => self.entries_sel = (self.entries_sel + 20).min(self.entries.len().saturating_sub(1)),
                KeyCode::Home | KeyCode::Char('g') => self.entries_sel = 0,
                KeyCode::End | KeyCode::Char('G') => self.entries_sel = self.entries.len().saturating_sub(1),
                KeyCode::Char('a') => {
                    self.journal_status.clear();
                    self.load_journal().await;
                }
                KeyCode::Char('d') => {
                    self.journal_status = "draft".into();
                    self.load_journal().await;
                }
                KeyCode::Char('p') => {
                    self.journal_status = "posted".into();
                    self.load_journal().await;
                }
                KeyCode::Char('v') => {
                    self.journal_status = "void".into();
                    self.load_journal().await;
                }
                KeyCode::Char('/') => self.modal = Modal::Input { title: "Search payee, description, memo, tag".into(), value: self.journal_query.clone(), target: InputTarget::JournalQuery },
                KeyCode::Char('t') => {
                    if let Some(e) = self.entries.get(self.entries_sel) {
                        self.modal = Modal::Input { title: format!("Tags for #{} (key:value, space-separated; empty clears)", e.id), value: e.tags.join(" "), target: InputTarget::Tags(e.id) };
                    }
                }
                KeyCode::Enter => {
                    if let Some(e) = self.entries.get(self.entries_sel).cloned() {
                        self.entry_scroll = 0;
                        self.modal = Modal::EntryDetail(Box::new(e));
                    }
                }
                _ => {}
            },
            Screen::Review => {
                let n = self.review_len();
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => self.review_sel = self.review_sel.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => self.review_sel = (self.review_sel + 1).min(n.saturating_sub(1)),
                    KeyCode::Char('f') => self.pick_refund().await,
                    KeyCode::Home | KeyCode::Char('g') => self.review_sel = 0,
                    KeyCode::End | KeyCode::Char('G') => self.review_sel = n.saturating_sub(1),
                    KeyCode::Enter | KeyCode::Char('a') => {
                        if self.review_unreviewed().is_some() {
                            self.confirm_selected().await;
                        } else if let Some(d) = self.review_draft().cloned() {
                            // Accounts of the draft's entity: reload if the current entity differs.
                            if self.entity_id() != d.entity_id {
                                if let Some(i) = self.entities.iter().position(|e| e.id == d.entity_id) {
                                    self.entity_idx = i;
                                    self.load_accounts().await;
                                }
                            }
                            let items = self.account_items(&|a| a.system_role != "suspense" && matches!(a.r#type.as_str(), "expense" | "income" | "asset" | "liability" | "equity"));
                            self.modal = Modal::Picker(Picker::new(&format!("Post #{} {} {} → account", d.id, d.date, crate::fmt::truncate(&d.payee, 24)), items), PickerTarget::ReviewContra);
                        }
                    }
                    KeyCode::Char('S') => self.open_review_split().await,
                    KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::SHIFT) => self.open_review_split().await,
                    KeyCode::Char('x') | KeyCode::Delete => {
                        if let Some(d) = self.review_draft() {
                            self.modal = Modal::Confirm { text: format!("Delete draft #{} {} {}? (y/n)", d.id, d.date, d.payee), action: ConfirmAction::DeleteDraft(d.id) };
                        }
                    }
                    KeyCode::Char('s') => {
                        if let Some(l) = self.review_line().cloned() {
                            match self.client.skip_line(pb::SkipLineRequest { line_id: l.id, unskip: false }).await {
                                Ok(_) => {
                                    self.status = format!("skipped line #{}", l.id);
                                    self.load_screen().await;
                                }
                                Err(e) => self.err(e),
                            }
                        }
                    }
                    KeyCode::Char('o') => {
                        if let Some(d) = self.review_entry().cloned() {
                            self.entry_scroll = 0;
                            self.modal = Modal::EntryDetail(Box::new(d));
                        }
                    }
                    KeyCode::Char('t') => {
                        if let Some(d) = self.review_entry() {
                            self.modal = Modal::Input { title: format!("Tags for #{} (key:value, space-separated; empty clears)", d.id), value: d.tags.join(" "), target: InputTarget::Tags(d.id) };
                        }
                    }
                    KeyCode::Char('b') => {
                        if let Some(d) = self.review_draft().cloned() {
                            let key = payee_key(&d.payee, &d.description);
                            if key.is_empty() {
                                self.status = "this draft has no usable payee".into();
                                return;
                            }
                            let ids: Vec<i64> =
                                self.drafts.iter().filter(|x| x.entity_id == d.entity_id && x.postings.len() == 2 && payee_key(&x.payee, &x.description) == key).map(|x| x.id).collect();
                            if ids.len() < 2 {
                                self.status = format!("no other drafts look like \"{key}\"; Enter posts this one");
                                return;
                            }
                            self.align_entity(d.entity_id).await;
                            let items = self.account_items(&|a| a.system_role != "suspense" && matches!(a.r#type.as_str(), "expense" | "income" | "asset" | "liability" | "equity"));
                            self.modal = Modal::Picker(Picker::new(&format!("Post {} drafts like \"{key}\" → account", ids.len()), items), PickerTarget::ReviewBatch(ids));
                        }
                    }
                    KeyCode::Char('u') => {
                        if let Some(d) = self.review_draft().cloned() {
                            let token = payee_key(&d.payee, &d.description).split_whitespace().take(2).collect::<Vec<_>>().join(" ");
                            if token.len() < 3 {
                                self.status = "this payee is too short for a safe rule".into();
                                return;
                            }
                            self.align_entity(d.entity_id).await;
                            let items = self.account_items(&|a| a.system_role != "suspense" && matches!(a.r#type.as_str(), "expense" | "income"));
                            self.modal = Modal::Picker(Picker::new(&format!("Rule: lines containing \"{token}\" post to"), items), PickerTarget::ReviewRule { token, entity_id: d.entity_id });
                        }
                    }
                    _ => {}
                }
            }
            Screen::Capture => {}
            Screen::Imports => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.imports_sel = self.imports_sel.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => self.imports_sel = (self.imports_sel + 1).min(self.imports.len().saturating_sub(1)),
                KeyCode::Char('i') => {
                    self.modal = Modal::Input { title: "Path of the file to import (CSV, OFX, JSON or ATB)".into(), value: self.import_path.clone(), target: InputTarget::ImportPath }
                }
                KeyCode::Enter => {
                    let Some(i) = self.imports.get(self.imports_sel) else { return };
                    if i.status != "pending" {
                        self.status = format!("import #{} is {}; Enter assigns pending inbox files", i.id, i.status);
                        return;
                    }
                    let (id, fname) = (i.id, i.filename.clone());
                    // Bank accounts of every entity: a pending file can belong to any of them.
                    match self.client.list_accounts(pb::ListAccountsRequest { entity_id: 0, include_closed: false, as_of: String::new(), r#type: String::new() }).await {
                        Ok(r) => {
                            let items: Vec<(i64, String, String)> = r
                                .accounts
                                .iter()
                                .filter(|a| !a.placeholder && matches!(a.r#type.as_str(), "asset" | "liability"))
                                .map(|a| (a.id, a.path.clone(), format!("{} {}", a.commodity, a.subtype)))
                                .collect();
                            self.modal = Modal::Picker(Picker::new(&format!("Account for {}", crate::fmt::truncate(&fname, 40)), items), PickerTarget::CompletePending(id));
                        }
                        Err(e) => self.err(e),
                    }
                }
                _ => {}
            },
            Screen::Sharing => match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    self.sharing_sel = (self.sharing_sel + 1).min(self.sharing.actions.len().saturating_sub(1));
                    self.sharing_scroll = (self.sharing.lines.len() + 1 + self.sharing_sel).saturating_sub(8);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.sharing_sel = self.sharing_sel.saturating_sub(1);
                    self.sharing_scroll = (self.sharing.lines.len() + 1 + self.sharing_sel).saturating_sub(8);
                }
                KeyCode::PageDown => self.sharing_scroll = (self.sharing_scroll + 8).min(self.sharing.lines.len() + self.sharing.actions.len()),
                KeyCode::PageUp => self.sharing_scroll = self.sharing_scroll.saturating_sub(8),
                KeyCode::Enter => {
                    if let Some(a) = self.sharing.actions.get(self.sharing_sel) {
                        self.modal = Modal::Sharing(Box::new(crate::sharing::Form::new(a.clone())));
                    }
                }
                _ => {}
            },
            Screen::Payees => match key.code {
                KeyCode::Char('j') | KeyCode::Down => self.payee_sel = (self.payee_sel + 1).min(self.payees.len().saturating_sub(1)),
                KeyCode::Char('k') | KeyCode::Up => self.payee_sel = self.payee_sel.saturating_sub(1),
                KeyCode::Char('n') => {
                    self.modal = Modal::Payee(Box::new(crate::payees::Form::new(pb::Payee { entity_id: self.entity_id(), active: true, ..Default::default() }, crate::payees::Operation::Save)))
                }
                KeyCode::Char('h') => {
                    self.modal = Modal::Payee(Box::new(crate::payees::Form::new(pb::Payee { entity_id: self.entity_id(), ..Default::default() }, crate::payees::Operation::Backfill)))
                }
                KeyCode::Enter | KeyCode::Char('l') | KeyCode::Char('m') => {
                    if let Some(p) = self.payees.get(self.payee_sel) {
                        let op = match key.code {
                            KeyCode::Char('l') => crate::payees::Operation::Link,
                            KeyCode::Char('m') => crate::payees::Operation::Merge,
                            _ => crate::payees::Operation::Save,
                        };
                        self.modal = Modal::Payee(Box::new(crate::payees::Form::new(p.clone(), op)));
                    }
                }
                _ => {}
            },
            Screen::Connections => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.connections_sel = self.connections_sel.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => self.connections_sel = (self.connections_sel + 1).min((self.connections.providers.len() + self.connections.accounts.len()).saturating_sub(1)),
                KeyCode::Char('o') => {
                    if let Some(j) = self.connection_job() {
                        self.modal = Modal::ConnectionStatus(format!(
                            "{} / {}: {}\nStarted: {}\nFinished: {}\n\n{}\n{}",
                            j.provider, j.operation, j.state, j.started_at, j.finished_at, j.message, j.authorization_url
                        ));
                    }
                }
                KeyCode::Char('n') => self.setup_connection(),
                KeyCode::Enter => self.map_connection().await,
                KeyCode::Char('p') => {
                    if let Some(a) = self.selected_connection() {
                        let scope = "this account";
                        self.modal = Modal::Confirm {
                            text: format!("Pull {scope} for {}? Booking cutoff: {}. Unmapped files wait in Imports. (y/n)", a.name, if a.booked_from.is_empty() { "none" } else { &a.booked_from }),
                            action: ConfirmAction::PullConnection(a.id),
                        };
                    } else {
                        self.setup_connection();
                    }
                }
                KeyCode::Char('f') => {
                    if let Some(a) = self.selected_connection() {
                        if matches!(a.channel.as_str(), "pluggy" | "enable_banking" | "mercury" | "wise" | "inter_pj") {
                            self.modal = Modal::Input { title: "Booked from (inclusive YYYY-MM-DD; empty clears)".into(), value: a.booked_from.clone(), target: InputTarget::ConnectionCutoff(a.id) };
                        }
                    }
                }
                KeyCode::Char('x') => {
                    if let Some(a) = self.selected_connection() {
                        self.modal = Modal::Confirm { text: format!("Unmap {} for future imports? Existing entries stay unchanged. (y/n)", a.name), action: ConfirmAction::UnmapConnection(a.id) };
                    }
                }
                KeyCode::Char('v') => {
                    self.screen = Screen::Review;
                    self.pending_load = true;
                }
                KeyCode::Char('i') => {
                    self.screen = Screen::Imports;
                    self.pending_load = true;
                }
                _ => {}
            },
            Screen::Reports => match key.code {
                KeyCode::Char(c @ ('t' | 'i' | 'b' | 'c' | 'g' | 'p' | 'u')) => {
                    self.report_kind = match c {
                        't' => ReportKind::TrialBalance,
                        'i' => ReportKind::IncomeStatement,
                        'b' => ReportKind::BalanceSheet,
                        'c' => ReportKind::ExpenseClass,
                        'g' => ReportKind::ExpenseTag,
                        'p' => ReportKind::ExpensePayee,
                        _ => ReportKind::Budgets,
                    };
                    self.load_report().await;
                }
                KeyCode::Char('n') if self.report_kind == ReportKind::Budgets => self.edit_budget(None).await,
                KeyCode::Enter if self.report_kind == ReportKind::Budgets => {
                    if let Some(b) = self.budgets.get(self.report_sel).and_then(|r| r.budget.clone()) {
                        self.edit_budget(Some(b)).await;
                    }
                }
                KeyCode::Char('d') if self.report_kind == ReportKind::Budgets => {
                    if let Some(b) = self.budgets.get(self.report_sel).and_then(|r| r.budget.as_ref()) {
                        self.modal = Modal::Confirm { text: format!("Delete budget {}? Entries stay unchanged. (y/n)", b.name), action: ConfirmAction::DeleteBudget(b.id) };
                    }
                }
                KeyCode::Char('v') if self.report_kind == ReportKind::Budgets => {
                    self.budgets_by_tag = !self.budgets_by_tag;
                    self.sort_budgets();
                    self.report_sel = 0;
                }
                KeyCode::Char('f') if matches!(self.report_kind, ReportKind::IncomeStatement | ReportKind::ExpenseClass | ReportKind::ExpenseTag | ReportKind::ExpensePayee) => {
                    self.modal = Modal::Input { title: "Report from (YYYY-MM-DD; empty for all history)".into(), value: self.report_from.clone(), target: InputTarget::ReportFrom }
                }
                KeyCode::Char('o') if matches!(self.report_kind, ReportKind::IncomeStatement | ReportKind::ExpenseClass | ReportKind::ExpenseTag | ReportKind::ExpensePayee) => {
                    self.modal = Modal::Input { title: "Report through (YYYY-MM-DD; empty for no end)".into(), value: self.report_to.clone(), target: InputTarget::ReportTo }
                }
                KeyCode::Char('/') if matches!(self.report_kind, ReportKind::Budgets | ReportKind::ExpenseClass | ReportKind::ExpenseTag | ReportKind::ExpensePayee) => {
                    self.modal = Modal::Input { title: "Exact tag filter (empty clears)".into(), value: self.report_tag.clone(), target: InputTarget::ReportTag }
                }
                KeyCode::Up | KeyCode::Char('k') => self.report_sel = self.report_sel.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    let n = if self.report_kind == ReportKind::Budgets { self.budgets.len() } else { self.report.as_ref().map(|r| r.rows.len()).unwrap_or(0) };
                    self.report_sel = (self.report_sel + 1).min(n.saturating_sub(1));
                }
                _ => {}
            },
        }
    }

    /// Returns true when the key was consumed by the capture form.
    async fn handle_capture_key(&mut self, key: KeyEvent) -> bool {
        let field = self.capture.field;
        let text_field = matches!(field, 2 | 4 | 5 | 6 | 7);
        match key.code {
            KeyCode::Esc => {
                self.capture = CaptureForm::new(&self.today.clone());
                self.status = "capture cleared".into();
                true
            }
            KeyCode::Tab | KeyCode::Down => {
                self.capture.field = (field + 1) % CAPTURE_FIELDS.len();
                true
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.capture.field = (field + CAPTURE_FIELDS.len() - 1) % CAPTURE_FIELDS.len();
                true
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.submit_capture().await;
                true
            }
            KeyCode::Enter => {
                match field {
                    0 => self.capture.kind = (self.capture.kind + 1) % KINDS.len(),
                    1 => {
                        let items = self.account_items(&|a| matches!(a.r#type.as_str(), "asset" | "liability"));
                        self.modal = Modal::Picker(Picker::new("Account", items), PickerTarget::CaptureAccount);
                    }
                    3 => {
                        let kind = self.capture.kind;
                        let items = self.account_items(&|a| match kind {
                            0 => a.r#type == "expense",
                            1 => a.r#type == "income",
                            _ => matches!(a.r#type.as_str(), "asset" | "liability"),
                        });
                        let title = match kind {
                            0 => "Expense category",
                            1 => "Income category",
                            _ => "Destination account",
                        };
                        self.modal = Modal::Picker(Picker::new(title, items), PickerTarget::CaptureContra);
                    }
                    7 => self.submit_capture().await,
                    _ => self.capture.field = (field + 1) % CAPTURE_FIELDS.len(),
                }
                true
            }
            KeyCode::Left if field == 0 => {
                self.capture.kind = (self.capture.kind + KINDS.len() - 1) % KINDS.len();
                true
            }
            KeyCode::Right | KeyCode::Char(' ') if field == 0 => {
                self.capture.kind = (self.capture.kind + 1) % KINDS.len();
                true
            }
            KeyCode::Char('s') if !text_field && self.capture.kind != 2 => {
                let kind = if self.capture.kind == 1 { "income" } else { "expense" };
                let items = self.account_items(&|a| a.r#type == kind && a.system_role != "suspense");
                let currency = self.capture.account.as_ref().and_then(|(id, _)| self.accounts.iter().find(|a| a.id == *id)).map(|a| a.commodity.clone()).unwrap_or_default();
                self.modal = Modal::Split(Box::new(crate::split::Editor::new(crate::split::Target::Capture, self.capture.amount.clone(), currency, self.capture.splits.clone(), items, kind.into())));
                true
            }
            KeyCode::Char('S') if !text_field && !self.capture.splits.is_empty() => {
                self.capture.splits.clear();
                self.status = "splits cleared".into();
                true
            }
            KeyCode::Backspace if text_field => {
                self.capture_text_mut().pop();
                true
            }
            KeyCode::Char(c) if text_field && !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.capture_text_mut().push(c);
                true
            }
            _ => false,
        }
    }

    fn capture_text_mut(&mut self) -> &mut String {
        match self.capture.field {
            2 => &mut self.capture.amount,
            4 => &mut self.capture.payee,
            5 => &mut self.capture.date,
            6 => &mut self.capture.notes,
            _ => &mut self.capture.tags,
        }
    }

    async fn handle_modal_key(&mut self, key: KeyEvent) {
        let modal = std::mem::replace(&mut self.modal, Modal::None);
        match modal {
            Modal::None => {}
            Modal::Split(mut form) => match form.key(key) {
                crate::split::Action::Cancel => {}
                crate::split::Action::Continue => self.modal = Modal::Split(form),
                crate::split::Action::CreateCategory(spec) => {
                    if let Some((id, label)) = self.create_category_from(&spec, &form.category_type).await {
                        form.category_created(id, label);
                    }
                    self.modal = Modal::Split(form);
                }
                crate::split::Action::Apply => match form.target {
                    crate::split::Target::Capture => {
                        self.capture.splits = form.rows;
                        self.status = "Splits saved; Ctrl-S submits the capture".into();
                    }
                    crate::split::Target::Review(id) | crate::split::Target::Existing(id) => {
                        let request = pb::ReviewPostingRequest {
                            categorize: Some(pb::PostDraftRequest { id, splits: form.inputs().unwrap(), payee: None }),
                            existing: matches!(form.target, crate::split::Target::Existing(_)),
                            ..Default::default()
                        };
                        self.preview_postings(vec![request], Modal::Split(form)).await;
                    }
                },
            },
            Modal::PostingPreview(mut preview) => match key.code {
                KeyCode::Char('y') => match self.client.review_posting(preview.request.clone()).await {
                    Ok(r) => {
                        let entry = r.entry.unwrap_or_default();
                        if preview.request.capture.is_some() {
                            self.capture.result = format!("posted #{} on {}", entry.id, entry.date);
                            self.capture.tags.clear();
                            self.capture.amount.clear();
                            self.capture.payee.clear();
                            self.capture.notes.clear();
                            self.capture.splits.clear();
                            self.capture.field = 2;
                        }
                        self.load_screen().await;
                        if let Some(account) = self.ledger.as_ref().map(|l| l.account.id) {
                            if let Some(i) = self.accounts.iter().position(|a| a.id == account) {
                                self.accounts_sel = i;
                                self.open_ledger().await;
                            }
                        }
                        self.status = format!("Confirmed transaction #{}", entry.id);
                        if !preview.remaining.is_empty() {
                            self.preview_postings(preview.remaining, Modal::None).await;
                        }
                    }
                    Err(e) => {
                        preview.error = format!("{e} — n returns to editing; submit again for a fresh preview");
                        self.modal = Modal::PostingPreview(preview);
                    }
                },
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.modal = *preview.back;
                    self.status = "Not confirmed; no transaction changes saved".into();
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    preview.scroll = preview.scroll.saturating_add(1);
                    self.modal = Modal::PostingPreview(preview);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    preview.scroll = preview.scroll.saturating_sub(1);
                    self.modal = Modal::PostingPreview(preview);
                }
                _ => self.modal = Modal::PostingPreview(preview),
            },
            Modal::EntryDetail(entry) => match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    self.entry_scroll = self.entry_scroll.saturating_add(1);
                    self.modal = Modal::EntryDetail(entry);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.entry_scroll = self.entry_scroll.saturating_sub(1);
                    self.modal = Modal::EntryDetail(entry);
                }
                KeyCode::Char('s') | KeyCode::Char('S') => self.open_entry_split(*entry).await,
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {}
                _ => self.modal = Modal::EntryDetail(entry),
            },
            Modal::Connection(mut form) => {
                if !form.key(key, &mut self.client).await {
                    self.modal = Modal::Connection(form);
                } else {
                    self.load_connections().await;
                }
            }
            Modal::Sharing(mut form) => {
                let closed = form.key(key, &mut self.client).await;
                if !form.response.replica_server.is_empty() {
                    let owned = self.client.addr().to_string();
                    let url = form.response.replica_server.clone();
                    *self = App::new(&url);
                    self.replica_return = Some(owned);
                    self.pending_load = true;
                } else if closed {
                    self.pending_load = true;
                } else {
                    self.modal = Modal::Sharing(form);
                }
            }
            Modal::Payee(mut form) => {
                if form.key(key, &mut self.client).await {
                    self.load_screen().await;
                } else {
                    self.modal = Modal::Payee(form);
                }
            }
            Modal::Budget(mut form) => {
                if form.key(key, &mut self.client).await {
                    if form.saved {
                        self.load_report().await;
                        self.status = "Budget saved".into();
                    }
                } else {
                    self.modal = Modal::Budget(form);
                }
            }
            Modal::Import(mut form) => {
                if form.key(key, &mut self.client).await {
                    if form.finished {
                        self.load_screen().await;
                    }
                    self.status = form.message;
                } else {
                    self.modal = Modal::Import(form);
                }
            }
            Modal::Help | Modal::ConnectionStatus(_) => {
                if !matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('?')) {
                    self.modal = modal;
                }
            }
            Modal::Confirm { text, action } => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => match action {
                    ConfirmAction::SplitCategory { source, target, query } => {
                        match self.client.split_category(pb::SplitCategoryRequest { source_id: source, target_id: target, query, preview: false }).await {
                            Ok(r) => {
                                self.status = format!("moved {} entries ({} postings)", r.entries, r.postings);
                                self.load_accounts().await;
                                if let Some(index) = self.accounts.iter().position(|a| a.id == source) {
                                    self.accounts_sel = index;
                                }
                                self.open_ledger().await;
                            }
                            Err(e) => self.err(e),
                        }
                    }
                    ConfirmAction::LinkRefund { line, original } => match self.client.link_refund(pb::LinkRefundRequest { line_id: line, original_entry_id: original }).await {
                        Ok(r) => {
                            let id = r.entry.map(|e| e.id).unwrap_or(0);
                            self.load_screen().await;
                            self.status = format!("Posted refund #{id} of expense #{original}");
                        }
                        Err(e) => self.err(e),
                    },
                    ConfirmAction::PullConnection(id) => {
                        if let Some(a) = self.connections.accounts.iter().find(|a| a.id == id) {
                            let request = pb::StartConnectionJobRequest { provider: crate::connections::provider(a), operation: "pull".into(), connection_id: id, ..Default::default() };
                            match self.client.start_connection_job(request).await {
                                Ok(_) => {
                                    self.load_connections().await;
                                    self.status = "Pull started; keep working while the bank responds".into();
                                }
                                Err(e) => self.err(e),
                            }
                        }
                    }
                    ConfirmAction::UnmapConnection(id) => {
                        if let Some(a) = self.connections.accounts.iter().find(|a| a.id == id) {
                            self.save_connection(id, 0, a.booked_from.clone()).await;
                        }
                    }
                    ConfirmAction::DeleteBudget(id) => match self.client.delete_budget(pb::DeleteBudgetRequest { id }).await {
                        Ok(_) => {
                            self.load_report().await;
                            self.status = "Budget deleted".into();
                        }
                        Err(e) => self.err(e),
                    },
                    ConfirmAction::DeleteDraft(entry_id) => match self.client.delete_journal_entry(pb::DeleteJournalEntryRequest { id: entry_id }).await {
                        Ok(_) => {
                            self.status = format!("deleted draft #{entry_id}");
                            self.load_screen().await;
                        }
                        Err(e) => self.err(e),
                    },
                    ConfirmAction::MergeAccounts { source, target } => match self.client.merge_accounts(pb::MergeAccountsRequest { source_id: source, target_id: target }).await {
                        Ok(r) => {
                            self.status = format!("merged: {} postings, {} rules, {} template lines moved", r.moved_postings, r.moved_rules, r.moved_template_lines);
                            self.load_screen().await;
                        }
                        Err(e) => self.err(e),
                    },
                },
                KeyCode::Char('n') | KeyCode::Esc => {}
                _ => self.modal = Modal::Confirm { text, action },
            },
            Modal::Input { title, mut value, target } => match key.code {
                KeyCode::Esc => {}
                KeyCode::Enter => match target {
                    InputTarget::ConnectionCutoff(id) => {
                        if let Some(a) = self.connections.accounts.iter().find(|a| a.id == id) {
                            self.save_connection(id, a.account_id, value).await;
                        }
                    }
                    InputTarget::CategoryFilter(source) => {
                        let query = value.trim().to_string();
                        if query.is_empty() {
                            self.status = "enter a non-empty text filter".into();
                            return;
                        }
                        let Some(from) = self.accounts.iter().find(|a| a.id == source) else { return };
                        let items = self.account_items(&|a| a.id != source && a.r#type == from.r#type && a.commodity == from.commodity && a.system_role != "suspense");
                        self.modal = Modal::Picker(Picker::new(&format!("Move matches for {query:?} to"), items), PickerTarget::SplitCategory { source, query });
                    }
                    InputTarget::JournalQuery => {
                        self.journal_query = value.trim().to_string();
                        self.load_journal().await;
                    }
                    InputTarget::ImportPath => {
                        let path = value.trim().to_string();
                        if path.is_empty() {
                            return;
                        }
                        self.import_path = path.clone();
                        self.begin_import(path).await;
                    }
                    InputTarget::AccountRename(id) => {
                        let name = value.trim().to_string();
                        if name.is_empty() {
                            return;
                        }
                        if let Some(mut acc) = self.accounts.iter().find(|a| a.id == id).cloned() {
                            acc.name = name;
                            match self.client.update_account(pb::UpdateAccountRequest { account: Some(acc) }).await {
                                Ok(r) => {
                                    self.status = format!("renamed to {}", r.account.map(|x| x.path).unwrap_or_default());
                                    self.load_accounts().await;
                                }
                                Err(e) => self.err(e),
                            }
                        }
                    }
                    InputTarget::AccountCode(id) => {
                        if let Some(mut acc) = self.accounts.iter().find(|a| a.id == id).cloned() {
                            acc.code = value.trim().to_string();
                            match self.client.update_account(pb::UpdateAccountRequest { account: Some(acc) }).await {
                                Ok(r) => {
                                    let a = r.account.unwrap_or_default();
                                    self.status = format!("{}: code {}", a.path, if a.code.is_empty() { "(cleared)".into() } else { a.code });
                                    self.load_accounts().await;
                                }
                                Err(e) => self.err(e),
                            }
                        }
                    }
                    InputTarget::AccountClass(id) => {
                        if let Some(mut acc) = self.accounts.iter().find(|a| a.id == id).cloned() {
                            acc.class = value.trim().to_lowercase();
                            match self.client.update_account(pb::UpdateAccountRequest { account: Some(acc) }).await {
                                Ok(r) => {
                                    let a = r.account.unwrap_or_default();
                                    self.status = format!("{}: class {}", a.path, if a.class.is_empty() { "(none)".into() } else { a.class });
                                    self.load_accounts().await;
                                }
                                Err(e) => self.err(e),
                            }
                        }
                    }
                    InputTarget::AccountNew { type_idx } => {
                        let spec = value.trim().trim_matches(':').to_string();
                        if spec.is_empty() {
                            return;
                        }
                        let t = ATYPES[type_idx.min(4)];
                        self.create_category_from(&spec, t).await;
                    }
                    InputTarget::RefundOriginal(line) => match value.trim().parse::<i64>() {
                        Ok(id) if id > 0 => self.confirm_refund(line, id).await,
                        _ => {
                            self.status = "Enter a positive expense entry ID".into();
                            self.modal = Modal::Input { title, value, target };
                        }
                    },
                    InputTarget::ReportFrom | InputTarget::ReportTo | InputTarget::ReportTag => {
                        match target {
                            InputTarget::ReportFrom => self.report_from = value,
                            InputTarget::ReportTo => self.report_to = value,
                            _ => self.report_tag = value,
                        }
                        self.load_report().await;
                    }
                    InputTarget::Tags(entry_id) => {
                        let toks: Vec<String> = value.split_whitespace().map(|x| x.to_string()).collect();
                        match self.client.set_tags(pb::SetTagsRequest { entry_id, tags: toks }).await {
                            Ok(r) => {
                                let e = r.entry.unwrap_or_default();
                                self.status = if e.tags.is_empty() { format!("#{entry_id}: tags cleared") } else { format!("#{entry_id}: {}", e.tags.join(" ")) };
                                self.load_screen().await;
                            }
                            Err(err) => self.err(err),
                        }
                    }
                },
                KeyCode::Backspace => {
                    value.pop();
                    self.modal = Modal::Input { title, value, target };
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    value.push(c);
                    self.modal = Modal::Input { title, value, target };
                }
                _ => self.modal = Modal::Input { title, value, target },
            },
            Modal::Picker(mut picker, target) => match key.code {
                KeyCode::Esc => {}
                KeyCode::Up => {
                    picker.selected = picker.selected.saturating_sub(1);
                    self.modal = Modal::Picker(picker, target);
                }
                KeyCode::Down => {
                    let n = picker.visible().len();
                    picker.selected = (picker.selected + 1).min(n.saturating_sub(1));
                    self.modal = Modal::Picker(picker, target);
                }
                KeyCode::Backspace => {
                    picker.filter.pop();
                    picker.selected = 0;
                    self.modal = Modal::Picker(picker, target);
                }
                KeyCode::Enter => {
                    let Some((id, label)) = picker.current() else {
                        self.modal = Modal::Picker(picker, target);
                        return;
                    };
                    match target {
                        PickerTarget::ConnectionAccount(connection) => {
                            if let Some(a) = self.connections.accounts.iter().find(|a| a.id == connection) {
                                self.save_connection(connection, id, a.booked_from.clone()).await;
                            }
                        }
                        PickerTarget::RefundOriginal(line) => {
                            if id == 0 {
                                self.modal = Modal::Input { title: "Original expense entry ID".into(), value: String::new(), target: InputTarget::RefundOriginal(line) };
                            } else {
                                self.confirm_refund(line, id).await;
                            }
                        }
                        PickerTarget::Sort(scope) => {
                            if let Some(order) = Order::ALL.get(id as usize) {
                                self.choose_sort(scope, *order);
                            }
                        }
                        PickerTarget::ReviewContra => self.post_draft_with(id).await,
                        PickerTarget::CaptureAccount => {
                            self.capture.account = Some((id, label));
                            self.capture.field = 2;
                        }
                        PickerTarget::CaptureContra => {
                            self.capture.contra = Some((id, label));
                            self.capture.field = 4;
                        }
                        PickerTarget::ImportAccount => {
                            let path = self.import_path.clone();
                            self.upload_import(path, id).await;
                        }
                        PickerTarget::CompletePending(import_id) => match self.client.complete_import(pb::CompleteImportRequest { id: import_id, account_id: id }).await {
                            Ok(r) => {
                                let i = r.import.unwrap_or_default();
                                self.status = format!("import #{} → {}: {} created, {} matched, {} duplicates", i.id, label, i.created_count, i.matched_count, i.duplicate_count);
                                self.load_screen().await;
                            }
                            Err(e) => self.err(e),
                        },
                        PickerTarget::MoveAccount(src) => {
                            if let Some(mut acc) = self.accounts.iter().find(|x| x.id == src).cloned() {
                                acc.parent_id = id; // 0 means the top level
                                match self.client.update_account(pb::UpdateAccountRequest { account: Some(acc) }).await {
                                    Ok(r) => {
                                        self.status = format!("moved to {}", r.account.map(|x| x.path).unwrap_or_default());
                                        self.load_accounts().await;
                                    }
                                    Err(e) => self.err(e),
                                }
                            }
                        }
                        PickerTarget::MergeAccount(src) => {
                            let source_path = self.accounts.iter().find(|x| x.id == src).map(|x| format!("{} ({} postings)", x.path, x.postings_count)).unwrap_or_else(|| format!("#{src}"));
                            self.modal = Modal::Confirm {
                                text: format!("Merge {source_path} into {label}? Postings, rules and templates follow. (y/n)"),
                                action: ConfirmAction::MergeAccounts { source: src, target: id },
                            };
                        }

                        PickerTarget::SplitCategory { source, query } => {
                            match self.client.split_category(pb::SplitCategoryRequest { source_id: source, target_id: id, query: query.clone(), preview: true }).await {
                                Ok(r) if r.entries > 0 => {
                                    self.modal = Modal::Confirm {
                                        text: format!("Move all entries matching {query:?} to {label}? Currently {} entries, {} postings. (y/n)", r.entries, r.postings),
                                        action: ConfirmAction::SplitCategory { source, target: id, query },
                                    }
                                }
                                Ok(_) => self.status = "no entries match that text filter".into(),
                                Err(e) => self.err(e),
                            }
                        }
                        PickerTarget::Recategorize(entry_id) => self.recategorize(entry_id, id, &label).await,
                        PickerTarget::ReviewBatch(ids) => {
                            let requests = ids
                                .into_iter()
                                .map(|entry_id| pb::ReviewPostingRequest {
                                    categorize: Some(pb::PostDraftRequest { id: entry_id, splits: vec![pb::SplitInput { account_id: id, ..Default::default() }], payee: None }),
                                    ..Default::default()
                                })
                                .collect();
                            self.preview_postings(requests, Modal::None).await;
                        }
                        PickerTarget::ReviewRule { token, entity_id } => {
                            let rule = pb::Rule {
                                id: 0,
                                entity_id,
                                name: token.clone(),
                                position: 0,
                                enabled: true,
                                conditions: vec![pb::RuleCondition { field: "description".into(), op: "contains".into(), value: token.clone() }],
                                account_id: id,
                                template_id: 0,
                                payee: token.clone(),
                                hits_count: 0,
                                created_at: String::new(),
                                tags: String::new(),
                            };
                            match self.client.save_rule(pb::SaveRuleRequest { rule: Some(rule) }).await {
                                Ok(_) => match self.client.run_rules(pb::RunRulesRequest { entity_id, account_id: 0, include_drafts: true }).await {
                                    Ok(r) => {
                                        self.status = format!("rule \"{token}\" → {label}: {} posted now; future imports post themselves", r.drafted + r.redrafted);
                                        self.load_screen().await;
                                    }
                                    Err(e) => self.err(e),
                                },
                                Err(e) => self.err(e),
                            }
                        }
                    }
                }
                KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let spec = picker.filter.clone();
                    match target {
                        PickerTarget::ReviewContra => {
                            let t = if self.review_draft().and_then(|d| d.postings.first()).map(|p| crate::fmt::sign(&p.quantity) < 0).unwrap_or(true) { "expense" } else { "income" };
                            match self.create_category_from(&spec, t).await {
                                Some((id, _)) => self.post_draft_with(id).await,
                                None => self.modal = Modal::Picker(picker, target),
                            }
                        }
                        PickerTarget::CaptureContra => {
                            if self.capture.kind == 2 {
                                self.status = "transfers go to an existing account".into();
                                self.modal = Modal::Picker(picker, target);
                            } else {
                                let t = if self.capture.kind == 1 { "income" } else { "expense" };
                                match self.create_category_from(&spec, t).await {
                                    Some((id, new_label)) => {
                                        self.capture.contra = Some((id, new_label));
                                        self.capture.field = 4;
                                    }
                                    None => self.modal = Modal::Picker(picker, target),
                                }
                            }
                        }
                        _ => {
                            self.status = "Ctrl-N creates categories in the category pickers".into();
                            self.modal = Modal::Picker(picker, target);
                        }
                    }
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    picker.filter.push(c);
                    picker.selected = 0;
                    self.modal = Modal::Picker(picker, target);
                }
                _ => self.modal = Modal::Picker(picker, target),
            },
        }
    }

    /// Create (or find) an account by path, exactly like `means chart new`: "Health:Fitness:Martial arts"
    /// creates the whole chain, missing groups become placeholders, and existing groups are matched
    /// case-insensitively so "health:fitness:Boxing" lands under the real Health. Root name optional.
    async fn create_category_from(&mut self, spec: &str, t: &str) -> Option<(i64, String)> {
        let spec = spec.trim().trim_matches(':').to_string();
        if spec.is_empty() {
            self.status = "type the new category's path into the filter, then Ctrl-N".into();
            return None;
        }
        let root = match t {
            "asset" => "Assets",
            "liability" => "Liabilities",
            "equity" => "Equity",
            "income" => "Income",
            _ => "Expenses",
        };
        let mut segments: Vec<String> = spec.split(':').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
        if let Some(first) = segments.first() {
            if first.eq_ignore_ascii_case(root) {
                segments.remove(0);
            }
        }
        if segments.is_empty() {
            self.status = "the new category needs a name".into();
            return None;
        }
        // Reuse the exact casing of existing groups along the chain.
        let mut parent: i64 = 0;
        for seg in segments.iter_mut() {
            match self.accounts.iter().find(|a| a.r#type == t && a.parent_id == parent && a.name.eq_ignore_ascii_case(seg)) {
                Some(a) => {
                    *seg = a.name.clone();
                    parent = a.id;
                }
                None => break,
            }
        }
        let node = pb::ChartAccount { code: String::new(), path: segments.join(":"), r#type: t.to_string(), placeholder: false, description: String::new() };
        match self.client.apply_chart(pb::ApplyChartRequest { entity_id: self.entity_id(), accounts: vec![node] }).await {
            Ok(resp) => {
                self.load_accounts().await;
                let wanted = format!("{root}:{}", segments.join(":")).to_lowercase();
                match self.accounts.iter().find(|a| a.r#type == t && a.path.to_lowercase() == wanted) {
                    Some(a) => {
                        self.status = if resp.created > 0 { format!("created {}", a.path) } else { format!("{} already there", a.path) };
                        Some((a.id, a.path.clone()))
                    }
                    None => {
                        self.status = format!("made {}, but cannot see it in this entity's list", segments.join(":"));
                        None
                    }
                }
            }
            Err(e) => {
                self.err(e);
                None
            }
        }
    }

    /// Change the category leg of a two-leg entry seen from a ledger; the leg on the ledger's own
    /// account (possibly reconciled) stays untouched.
    async fn recategorize(&mut self, entry_id: i64, new_account: i64, label: &str) {
        let ledger_account = match self.ledger.as_ref() {
            Some(l) => l.account.id,
            None => return,
        };
        let entry = match self.client.get_journal_entry(pb::GetJournalEntryRequest { id: entry_id }).await {
            Ok(r) => match r.entry {
                Some(e) => e,
                None => return,
            },
            Err(e) => {
                self.err(e);
                return;
            }
        };
        if entry.postings.len() != 2 {
            self.status = format!("#{entry_id} has {} legs; edit it from the Journal", entry.postings.len());
            return;
        }
        if entry.postings.iter().all(|p| p.account_id == ledger_account) {
            self.status = "both legs are this account".into();
            return;
        }
        let postings: Vec<pb::PostingInput> = entry
            .postings
            .iter()
            .map(|p| pb::PostingInput {
                account_id: if p.account_id == ledger_account { p.account_id } else { new_account },
                quantity: p.quantity.clone(),
                amount: p.amount.clone(),
                memo: p.memo.clone(),
                metadata: p.metadata.clone(),
                external_id: p.external_id.clone(),
                fingerprint: p.fingerprint.clone(),
            })
            .collect();
        let input = pb::JournalEntryInput {
            entity_id: entry.entity_id,
            date: entry.date.clone(),
            payee: entry.payee.clone(),
            description: entry.description.clone(),
            notes: entry.notes.clone(),
            status: entry.status.clone(),
            postings,
            template_id: 0,
            origin: entry.origin.clone(),
        };
        match self.client.update_journal_entry(pb::UpdateJournalEntryRequest { id: entry_id, entry: Some(input) }).await {
            Ok(_) => {
                self.status = format!("#{entry_id} re-categorized to {label}");
                let keep = self.ledger.as_ref().map(|l| l.selected).unwrap_or(0);
                self.open_ledger().await;
                if let Some(l) = self.ledger.as_mut() {
                    l.selected = keep.min(l.rows.len().saturating_sub(1));
                }
            }
            Err(e) => self.err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sorting_picker_changes_view_and_keeps_running_balances_attached() {
        let mut app = App::new("http://127.0.0.1:1");
        app.screen = Screen::Accounts;
        app.ledger = Some(LedgerView {
            account: Default::default(),
            rows: vec![
                pb::LedgerRow { posting_id: 1, date: "2024-01-01".into(), debit: "100".into(), amount: "100".into(), running_balance: "100".into(), ..Default::default() },
                pb::LedgerRow { posting_id: 2, date: "2026-01-01".into(), debit: "20".into(), amount: "20".into(), running_balance: "120".into(), ..Default::default() },
            ],
            opening: "0".into(),
            closing: "120".into(),
            commodity: "USD".into(),
            selected: 1,
        });
        app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE)).await;
        assert!(matches!(app.modal, Modal::Picker(_, PickerTarget::Sort(Scope::Ledger))));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;
        let l = app.ledger.as_ref().unwrap();
        assert_eq!(l.selected, 0);
        assert_eq!(l.rows[0].posting_id, 2);
        assert_eq!(l.rows[0].running_balance, "120");
        app.choose_sort(Scope::Ledger, Order::AmountDesc);
        assert_eq!(app.ledger.as_ref().unwrap().rows[0].posting_id, 1);
        app.screen = Screen::Reports;
        app.report_kind = ReportKind::ExpenseClass;
        app.report = Some(pb::ReportResponse {
            rows: vec![pb::ReportRow { name: "Food".into(), amount: "9".into(), ..Default::default() }, pb::ReportRow { name: "Travel".into(), amount: "100".into(), ..Default::default() }],
            ..Default::default()
        });
        app.choose_sort(Scope::Expenses, Order::AmountDesc);
        assert_eq!(app.report.as_ref().unwrap().rows[0].name, "Travel");
    }

    #[tokio::test]
    async fn review_navigation_keeps_entry_types_and_actions_separate() {
        let mut app = App::new("http://127.0.0.1:1");
        app.screen = Screen::Review;
        app.drafts = vec![pb::JournalEntry { id: 1, status: "draft".into(), ..Default::default() }];
        app.unreviewed = vec![pb::JournalEntry { id: 2, status: "posted".into(), ..Default::default() }];
        app.unmatched = vec![pb::StatementLine { id: 3, ..Default::default() }];
        assert_eq!(app.review_len(), 3);
        assert_eq!(app.review_draft().unwrap().id, 1);
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)).await;
        assert_eq!(app.review_unreviewed().unwrap().id, 2);
        assert!(app.review_draft().is_none());
        assert!(app.review_line().is_none());
        for code in [KeyCode::Char('x'), KeyCode::Delete, KeyCode::Char('b'), KeyCode::Char('u'), KeyCode::Char('s')] {
            app.handle_key(KeyEvent::new(code, KeyModifiers::NONE)).await;
            assert!(matches!(app.modal, Modal::None));
            assert!(app.status.is_empty());
        }
        app.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE)).await;
        assert!(matches!(&app.modal, Modal::EntryDetail(e) if e.id == 2));
        app.modal = Modal::None;
        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)).await;
        assert_eq!(app.review_line().unwrap().id, 3);
        assert!(app.review_entry().is_none());
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)).await;
        assert_eq!(app.review_sel, 2);
        app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE)).await;
        assert_eq!(app.review_draft().unwrap().id, 1);
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;
        assert!(matches!(app.modal, Modal::Picker(_, PickerTarget::ReviewContra)));
    }

    #[tokio::test]
    async fn review_split_accepts_terminal_shift_encodings_and_closed_bank_accounts() {
        for key in [KeyEvent::new(KeyCode::Char('S'), KeyModifiers::NONE), KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT), KeyEvent::new(KeyCode::Char('s'), KeyModifiers::SHIFT)] {
            let mut app = App::new("http://127.0.0.1:1");
            app.screen = Screen::Review;
            app.entities = vec![pb::Entity { id: 1, ..Default::default() }];
            // The closed bank is deliberately absent from the open-account cache.
            app.accounts = vec![
                pb::Account { id: 20, entity_id: 1, system_role: "suspense".into(), ..Default::default() },
                pb::Account { id: 30, entity_id: 1, path: "Expenses:Food".into(), ..Default::default() },
            ];
            app.drafts = vec![pb::JournalEntry {
                id: 42,
                entity_id: 1,
                status: "draft".into(),
                postings: vec![
                    pb::Posting { account_id: 10, quantity: "-90.50".into(), commodity: "EUR".into(), ..Default::default() },
                    pb::Posting { account_id: 20, quantity: "108.60".into(), commodity: "USD".into(), ..Default::default() },
                ],
                ..Default::default()
            }];
            app.handle_key(key).await;
            let Modal::Split(form) = &app.modal else { panic!("{:?}: {}", key, app.status) };
            assert_eq!(form.total, "90.50");
            assert_eq!(form.currency, "EUR");
            assert!(matches!(form.target, crate::split::Target::Review(42)));
            app.modal = Modal::None;
            app.drafts[0].postings[1].account_id = 30;
            app.handle_key(key).await;
            assert!(matches!(app.modal, Modal::None));
            assert!(app.status.contains("Uncategorized"));
            app.drafts.clear();
            app.unreviewed = vec![pb::JournalEntry { id: 43, status: "posted".into(), ..Default::default() }];
            app.handle_key(key).await;
            assert!(app.status.starts_with("error"));
            app.unreviewed.clear();
            app.unmatched = vec![pb::StatementLine { id: 44, ..Default::default() }];
            app.handle_key(key).await;
            assert!(app.status.contains("unmatched statement lines"));
        }
    }

    #[tokio::test]
    async fn failed_confirmation_keeps_the_entry_in_review() {
        let mut app = App::new("http://127.0.0.1:1");
        app.screen = Screen::Review;
        app.unreviewed = vec![pb::JournalEntry { id: 2, status: "posted".into(), ..Default::default() }];
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).await;
        assert!(app.status.starts_with("error:"), "{}", app.status);
        assert_eq!(app.review_unreviewed().unwrap().id, 2);
        assert!(matches!(app.modal, Modal::None));
    }

    #[test]
    fn picker_filters_by_every_word() {
        let mut p = Picker::new("x", vec![(1, "Expenses:Food".into(), "EUR expense".into()), (2, "Assets:Bank:N26".into(), "EUR bank".into()), (3, "Expenses:Taxes:IOF".into(), "BRL expense".into())]);
        p.filter = "exp eur".into();
        assert_eq!(p.visible().len(), 1);
        assert_eq!(p.current().unwrap().0, 1);
        p.filter = "tax".into();
        assert_eq!(p.current().unwrap().0, 3);
    }
}
