//! Bank setup contains identifiers and preferences, never passwords or API keys.
use crate::client::Client;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use means_proto::v1 as pb;
pub const PROVIDERS: [&str; 6] = ["pluggy", "mercury", "mercury_credit", "enable_banking", "wise", "inter_pj"];
pub fn provider(a: &pb::BankConnection) -> String {
    if a.channel == "mercury" && a.provider_type == "credit" {
        "mercury_credit".into()
    } else {
        a.channel.clone()
    }
}
#[derive(Debug, Clone)]
pub struct Form {
    pub provider: usize,
    pub field: usize,
    pub item: String,
    pub country: String,
    pub bank: String,
    pub port: String,
    pub banks: Vec<pb::BankChoice>,
    pub choice: usize,
    pub message: String,
    pub last_job_version: String,
}
impl Form {
    pub fn new(provider: &str) -> Self {
        Self {
            provider: PROVIDERS.iter().position(|p| *p == provider).unwrap_or(0),
            field: 0,
            item: String::new(),
            country: "PT".into(),
            bank: String::new(),
            port: "53682".into(),
            banks: vec![],
            choice: 0,
            message: String::new(),
            last_job_version: String::new(),
        }
    }
    fn matches(&self) -> Vec<&pb::BankChoice> {
        let query = self.bank.to_lowercase();
        self.banks.iter().filter(|b| b.country.eq_ignore_ascii_case(&self.country) && b.name.to_lowercase().contains(&query)).collect()
    }
    fn chosen(&self) -> Option<&pb::BankChoice> {
        self.matches().get(self.choice).copied()
    }
    pub fn lines(&self) -> Vec<String> {
        let mut fields = vec![format!("Provider: {} (left/right)", PROVIDERS[self.provider])];
        match PROVIDERS[self.provider] {
            "pluggy" => fields.extend([
                format!("Item ID: {}", self.item),
                "Ctrl-O opens MeuPluggy. Approve one bank, then paste its item ID.".into(),
                "Ctrl-D discovers accounts without importing transactions.".into(),
            ]),
            "enable_banking" => fields.extend([
                format!("Country: {}", self.country),
                format!("Bank search: {} → {}", self.bank, self.chosen().map(|b| b.name.as_str()).unwrap_or("fetch banks with Ctrl-B")),
                format!("Callback port: {}", self.port),
            ]),
            "wise" => fields.extend([
                "Uses WISE_TOKEN from the server environment.".into(),
                "Ctrl-D discovers business balances; map each currency account.".into(),
                "US business token statements supported; no payment actions.".into(),
            ]),
            "inter_pj" => fields.extend([
                "Uses Inter client credentials and PEM certificate/key files.".into(),
                "INTER_ACCOUNT_NUMBER selects the BRL current account.".into(),
                "Ctrl-D verifies access. Map account and set cutoff with f.".into(),
            ]),
            _ => fields.extend([
                "Uses MERCURY_TOKEN from the server environment.".into(),
                "Ctrl-D discovers accounts. Map them before pulling.".into(),
                "Checking/savings and IO use separate provider choices.".into(),
            ]),
        }
        fields.into_iter().enumerate().map(|(i, s)| format!("{} {s}", if i == self.field { ">" } else { " " })).collect()
    }
    pub fn request(&self, operation: &str) -> anyhow::Result<pb::StartConnectionJobRequest> {
        let mut r = pb::StartConnectionJobRequest {
            provider: PROVIDERS[self.provider].into(),
            operation: operation.into(),
            item_id: if self.provider == 0 { self.item.trim().into() } else { String::new() },
            country: self.country.trim().to_uppercase(),
            ..Default::default()
        };
        if operation == "authorize" {
            let bank = self.chosen().ok_or_else(|| anyhow::anyhow!("Fetch banks with Ctrl-B, then choose one with left/right"))?;
            r.bank = bank.name.clone();
            r.country = bank.country.clone();
            r.callback_port = self.port.parse().map_err(|_| anyhow::anyhow!("Callback port must be a number"))?;
        }
        Ok(r)
    }
    pub async fn key(&mut self, key: KeyEvent, client: &mut Client) -> bool {
        if key.code == KeyCode::Esc {
            return true;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            let operation = match key.code {
                KeyCode::Char('d') => Some("discover"),
                KeyCode::Char('b') if self.provider == 3 => Some("banks"),
                KeyCode::Char('a') if self.provider == 3 => Some("authorize"),
                _ => None,
            };
            if let Some(op) = operation {
                match self.request(op) {
                    Ok(request) => match client.start_connection_job(request).await {
                        Ok(_) => self.message = "Operation started. You can close this form while it runs.".into(),
                        Err(e) => self.message = e.to_string(),
                    },
                    Err(e) => self.message = e.to_string(),
                }
                return false;
            }
            if key.code == KeyCode::Char('o') && self.provider == 0 {
                self.message = match open::that("https://meu.pluggy.ai") {
                    Ok(_) => "Approve a bank in MeuPluggy, then paste its item ID here.".into(),
                    Err(_) => "Open https://meu.pluggy.ai in your browser.".into(),
                };
            }
            return false;
        }
        match key.code {
            KeyCode::Tab | KeyCode::Down => self.field = (self.field + 1) % 4,
            KeyCode::BackTab | KeyCode::Up => self.field = (self.field + 3) % 4,
            KeyCode::Left | KeyCode::Right if self.field == 0 => {
                self.provider = (self.provider + if key.code == KeyCode::Right { 1 } else { PROVIDERS.len() - 1 }) % PROVIDERS.len();
                self.choice = 0;
            }
            KeyCode::Left | KeyCode::Right if self.provider == 3 && self.field == 2 => {
                let n = self.matches().len();
                if n > 0 {
                    self.choice = (self.choice + if key.code == KeyCode::Right { 1 } else { n - 1 }) % n;
                }
            }
            KeyCode::Char(c) => {
                if let Some(text) = self.text_mut() {
                    text.push(c);
                }
                self.choice = 0;
            }
            KeyCode::Backspace => {
                if let Some(text) = self.text_mut() {
                    text.pop();
                }
                self.choice = 0;
            }
            _ => {}
        }
        false
    }
    fn text_mut(&mut self) -> Option<&mut String> {
        match (self.provider, self.field) {
            (0, 1) => Some(&mut self.item),
            (3, 1) => Some(&mut self.country),
            (3, 2) => Some(&mut self.bank),
            (3, 3) => Some(&mut self.port),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn setup_selects_discovered_banks_and_sends_no_credentials() {
        let mut form = Form::new("enable_banking");
        form.banks = vec![pb::BankChoice { name: "Bank A".into(), country: "PT".into() }, pb::BankChoice { name: "Bank B".into(), country: "PT".into() }];
        form.field = 2;
        let mut client = Client::new("http://127.0.0.1:1");
        form.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), &mut client).await;
        let r = form.request("authorize").unwrap();
        assert_eq!(r.bank, "Bank B");
        assert_eq!(r.callback_port, 53682);
        assert_eq!(r.country, "PT");
        assert!(r.item_id.is_empty());
        form.bank = "A".into();
        form.choice = 0;
        assert_eq!(form.request("authorize").unwrap().bank, "Bank A");
        form.country = "DE".into();
        assert!(form.request("authorize").is_err());
        assert!(form.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut client).await);
    }
    #[tokio::test]
    async fn every_provider_is_reachable_and_discovery_carries_no_credentials() {
        let mut form = Form::new("pluggy");
        let mut client = Client::new("http://127.0.0.1:1");
        for expected in PROVIDERS.iter().skip(1).chain(std::iter::once(&PROVIDERS[0])) {
            form.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), &mut client).await;
            assert_eq!(PROVIDERS[form.provider], *expected);
            assert_eq!(form.request("discover").unwrap().provider, *expected);
        }
        for provider in ["wise", "inter_pj"] {
            let form = Form::new(provider);
            assert!(form.lines().join(" ").contains(if provider == "wise" { "WISE_TOKEN" } else { "INTER_ACCOUNT_NUMBER" }));
            assert!(form.request("discover").unwrap().item_id.is_empty());
        }
    }
}
