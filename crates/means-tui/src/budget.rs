//! Budget editor. Category choices are searchable and include parent categories.
use crate::client::Client;
use chrono::Datelike;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use means_proto::v1 as pb;

#[derive(Debug, Clone)]
pub struct Form {
    pub budget: pb::Budget,
    pub field: usize,
    pub query: String,
    pub choice: usize,
    pub accounts: Vec<pb::Account>,
    pub message: String,
    pub saved: bool,
}
const CLASSES: [&str; 6] = ["fixed", "committed", "discretionary", "savings", "not-spending", ""];
impl Form {
    pub fn new(entity_id: i64, currency: String, accounts: Vec<pb::Account>, existing: Option<pb::Budget>) -> Self {
        let day = chrono::Local::now().date_naive();
        let first = day.with_day(1).unwrap();
        let last = (28..=31).rev().find_map(|d| day.with_day(d)).unwrap();
        let budget = existing.unwrap_or(pb::Budget { entity_id, currency, scope: "category".into(), starts_on: first.to_string(), ends_on: last.to_string(), ..Default::default() });
        let accounts: Vec<_> = accounts.into_iter().filter(|a| a.entity_id == entity_id && a.r#type == "expense").collect();
        let choice =
            if budget.scope == "class" { CLASSES.iter().position(|c| Some(*c) == budget.class.as_deref()).unwrap_or(0) } else { accounts.iter().position(|a| a.id == budget.account_id).unwrap_or(0) };
        Self { budget, field: 0, query: String::new(), choice, accounts, message: String::new(), saved: false }
    }
    fn choices(&self) -> Vec<&pb::Account> {
        let q = self.query.to_lowercase();
        self.accounts.iter().filter(|a| a.path.to_lowercase().contains(&q)).collect()
    }
    fn target(&self) -> String {
        match self.budget.scope.as_str() {
            "category" => self.choices().get(self.choice).map(|a| a.path.clone()).unwrap_or_else(|| "No matching category".into()),
            "class" => {
                let c = CLASSES[self.choice % CLASSES.len()];
                if c.is_empty() {
                    "Unclassified".into()
                } else {
                    c.into()
                }
            }
            _ => "Use the tag field below".into(),
        }
    }
    pub fn lines(&self) -> Vec<String> {
        let b = &self.budget;
        [
            format!("Name: {}", b.name),
            format!("Scope: {} (left/right)", b.scope),
            format!("Target: {}", self.target()),
            format!("Tag: {}", b.tag),
            format!("Limit ({}): {}", b.currency, b.amount),
            format!("From: {}", b.starts_on),
            format!("Through: {}", b.ends_on),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, s)| format!("{} {}", if i == self.field { ">" } else { " " }, s))
        .collect()
    }
    pub fn request(&self) -> pb::SaveBudgetRequest {
        let mut b = self.budget.clone();
        b.account_id = if b.scope == "category" { self.choices().get(self.choice).map(|a| a.id).unwrap_or(0) } else { 0 };
        b.class = if b.scope == "class" { Some(CLASSES[self.choice % CLASSES.len()].into()) } else { None };
        pb::SaveBudgetRequest { budget: Some(b) }
    }
    pub async fn key(&mut self, key: KeyEvent, client: &mut Client) -> bool {
        if key.code == KeyCode::Esc {
            return true;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
            match client.save_budget(self.request()).await {
                Ok(_) => {
                    self.saved = true;
                    return true;
                }
                Err(e) => self.message = e.to_string(),
            }
            return false;
        }
        match key.code {
            KeyCode::Tab | KeyCode::Down | KeyCode::Enter => self.field = (self.field + 1) % 7,
            KeyCode::BackTab | KeyCode::Up => self.field = (self.field + 6) % 7,
            KeyCode::Left | KeyCode::Right if self.field == 1 => {
                let scopes = ["category", "class", "tag"];
                let i = scopes.iter().position(|s| *s == self.budget.scope).unwrap_or(0);
                self.budget.scope = scopes[(i + if key.code == KeyCode::Right { 1 } else { 2 }) % 3].into();
                self.choice = 0;
                self.query.clear();
            }
            KeyCode::Left | KeyCode::Right if self.field == 2 => {
                let n = if self.budget.scope == "class" { CLASSES.len() } else { self.choices().len() };
                if n > 0 {
                    self.choice = (self.choice + if key.code == KeyCode::Right { 1 } else { n - 1 }) % n;
                }
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(text) = self.text_mut() {
                    text.push(c);
                }
                if self.field == 2 {
                    self.choice = 0;
                }
            }
            KeyCode::Backspace => {
                if let Some(text) = self.text_mut() {
                    text.pop();
                }
                if self.field == 2 {
                    self.choice = 0;
                }
            }
            _ => {}
        }
        false
    }
    fn text_mut(&mut self) -> Option<&mut String> {
        match self.field {
            0 => Some(&mut self.budget.name),
            2 if self.budget.scope == "category" => Some(&mut self.query),
            3 => Some(&mut self.budget.tag),
            4 => Some(&mut self.budget.amount),
            5 => Some(&mut self.budget.starts_on),
            6 => Some(&mut self.budget.ends_on),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn category_search_scope_switching_and_cancel_preserve_intent() {
        let accounts = vec![
            pb::Account { id: 1, entity_id: 7, r#type: "expense".into(), path: "Expenses:Food".into(), ..Default::default() },
            pb::Account { id: 2, entity_id: 7, r#type: "expense".into(), path: "Expenses:Rent".into(), ..Default::default() },
        ];
        let existing = pb::Budget { id: 3, entity_id: 7, scope: "category".into(), account_id: 2, tag: "trip:porto".into(), ..Default::default() };
        let mut f = Form::new(7, "EUR".into(), accounts, Some(existing));
        let mut client = Client::new("http://127.0.0.1:1");
        assert_eq!(f.request().budget.unwrap().account_id, 2);
        f.field = 2;
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        for c in "food".chars() {
            f.key(key(KeyCode::Char(c)), &mut client).await;
        }
        assert_eq!(f.request().budget.unwrap().account_id, 1);
        f.field = 1;
        f.key(key(KeyCode::Right), &mut client).await;
        let b = f.request().budget.unwrap();
        assert_eq!(b.scope, "class");
        assert_eq!(b.account_id, 0);
        assert_eq!(b.class.as_deref(), Some("fixed"));
        f.key(key(KeyCode::Right), &mut client).await;
        let b = f.request().budget.unwrap();
        assert_eq!(b.scope, "tag");
        assert!(b.class.is_none());
        assert_eq!(b.tag, "trip:porto");
        assert!(f.key(key(KeyCode::Esc), &mut client).await);
        assert!(!f.saved);
    }
}
