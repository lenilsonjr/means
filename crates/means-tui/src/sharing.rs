//! Sharing forms keep secrets masked; grants and revocations require a second action.
use crate::client::Client;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use means_proto::v1 as pb;
use zeroize::Zeroize;
#[derive(Clone)]
pub struct Form {
    pub action: pb::SharingAction,
    pub values: Vec<String>,
    pub field: usize,
    pub response: pb::SharingResponse,
    pub message: String,
    pub scroll: usize,
    pub done: bool,
}
impl std::fmt::Debug for Form {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharingForm").field("operation", &self.action.name).finish_non_exhaustive()
    }
}
impl Form {
    pub fn new(action: pb::SharingAction) -> Self {
        let count = action.fields.len() + usize::from(action.passphrase) + usize::from(action.new_passphrase);
        Self { action, values: vec![String::new(); count], field: 0, response: Default::default(), message: String::new(), scroll: 0, done: false }
    }
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![self.action.description.clone(), "Tab fields · Ctrl-P submit/preview · Ctrl-Y confirm preview · Esc close".into()];
        if self.response.confirmation.is_empty() && !self.done {
            let mut labels = self.action.fields.clone();
            if self.action.passphrase {
                labels.push("Passphrase".into())
            }
            if self.action.new_passphrase {
                labels.push("New passphrase".into())
            }
            for (i, label) in labels.iter().enumerate() {
                let value = if i >= self.action.fields.len() { "•".repeat(self.values[i].chars().count()) } else { self.values[i].clone() };
                lines.push(format!("{} {label}: {value}", if self.field == i { ">" } else { " " }));
            }
        }
        lines.extend(self.response.lines.clone());
        lines.push(self.message.clone());
        lines
    }
    pub async fn key(&mut self, key: KeyEvent, client: &mut Client) -> bool {
        if key.code == KeyCode::Esc {
            return true;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            let confirmation = match key.code {
                KeyCode::Char('p') if !self.done => Some(String::new()),
                KeyCode::Char('y') if !self.response.confirmation.is_empty() => Some(self.response.confirmation.clone()),
                _ => None,
            };
            if let Some(confirmation) = confirmation {
                let n = self.action.fields.len();
                let request = pb::SharingRequest {
                    operation: self.action.name.clone(),
                    arguments: self.values[..n].to_vec(),
                    passphrase: if self.action.passphrase { self.values[n].clone() } else { String::new() },
                    new_passphrase: if self.action.new_passphrase { self.values[n + 1].clone() } else { String::new() },
                    confirmation,
                };
                match client.sharing(request).await {
                    Ok(r) => {
                        self.done = r.confirmation.is_empty();
                        self.response = r;
                        self.message.clear();
                        self.scroll = 0;
                        if self.done {
                            for s in self.values.iter_mut().skip(n) {
                                s.zeroize();
                            }
                        }
                    }
                    Err(e) => {
                        self.message = e.to_string();
                        self.response = Default::default();
                    }
                }
            }
            return false;
        }
        if !self.response.confirmation.is_empty() || self.done {
            match key.code {
                KeyCode::Down => self.scroll += 1,
                KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::PageDown => self.scroll += 8,
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(8),
                KeyCode::Backspace if !self.done => self.response = Default::default(),
                _ => {}
            }
            return false;
        }
        if self.values.is_empty() {
            return false;
        }
        match key.code {
            KeyCode::Tab | KeyCode::Down => self.field = (self.field + 1) % self.values.len(),
            KeyCode::BackTab | KeyCode::Up => self.field = (self.field + self.values.len() - 1) % self.values.len(),
            KeyCode::Char(c) => self.values[self.field].push(c),
            KeyCode::Backspace => {
                self.values[self.field].pop();
            }
            _ => {}
        }
        false
    }
}

impl Drop for Form {
    fn drop(&mut self) {
        for s in &mut self.values {
            s.zeroize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn secrets_are_masked_in_rendering_and_debug() {
        let mut f = Form::new(pb::SharingAction { name: "rotate".into(), description: "Rotate".into(), fields: vec!["File".into()], passphrase: true, new_passphrase: true });
        f.values = vec!["safe-file".into(), "old secret passphrase".into(), "new secret passphrase".into()];
        let lines = f.lines().join("\n");
        assert!(lines.contains("safe-file"));
        assert!(!lines.contains("secret passphrase"));
        assert!(!format!("{f:?}").contains("secret"));
    }
    #[tokio::test]
    async fn confirmation_requires_preview_and_editing_invalidates_it() {
        let mut f = Form::new(pb::SharingAction { name: "grant".into(), fields: vec!["Vault".into()], ..Default::default() });
        let mut client = Client::new("http://127.0.0.1:1");
        f.key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL), &mut client).await;
        assert!(f.message.is_empty()); // No RPC without a reviewed preview.
        f.response.confirmation = "token".into();
        f.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE), &mut client).await;
        assert!(f.response.confirmation.is_empty());
    }
}
