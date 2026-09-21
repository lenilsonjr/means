//! Reusable split editor for Capture and imported drafts. It never posts by itself.
use crate::app::Picker;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use means_proto::v1 as pb;
use rust_decimal::Decimal;
use std::str::FromStr;

pub type Row = (i64, String, String);
#[derive(Debug, Clone, Copy)]
pub enum Target {
    Capture,
    Review(i64),
    Existing(i64),
}
#[derive(Debug, Clone)]
enum Step {
    List,
    Category(Picker),
    Amount(i64, String, String),
}
#[derive(Debug, Clone)]
pub struct Editor {
    pub target: Target,
    pub total: String,
    pub currency: String,
    pub rows: Vec<Row>,
    pub selected: usize,
    pub error: String,
    pub category_type: String,
    accounts: Vec<(i64, String, String)>,
    step: Step,
    editing: Option<usize>,
}
pub enum Action {
    Continue,
    Cancel,
    Apply,
    CreateCategory(String),
}
impl Editor {
    pub fn new(target: Target, total: String, currency: String, rows: Vec<Row>, accounts: Vec<(i64, String, String)>, category_type: String) -> Self {
        Self { target, total, currency, rows, accounts, category_type, selected: 0, error: String::new(), step: Step::List, editing: None }
    }
    pub fn inputs(&self) -> Result<Vec<pb::SplitInput>, String> {
        let total = Decimal::from_str(&self.total.trim().replace(',', ".")).map_err(|_| "enter a valid total first")?;
        if total <= Decimal::ZERO || self.rows.is_empty() {
            return Err("add at least one split to a positive total".into());
        }
        let mut sum = Decimal::ZERO;
        let mut out = Vec::new();
        for (i, (id, _, text)) in self.rows.iter().enumerate() {
            let text = text.trim().replace(',', ".");
            let quantity = if text.is_empty() {
                if i + 1 != self.rows.len() {
                    return Err("only the last split can take the remainder".into());
                }
                total.checked_sub(sum).ok_or("split amount overflow")?
            } else {
                Decimal::from_str(&text).map_err(|_| "enter a valid split amount")?
            };
            if quantity.is_zero() || (text.is_empty() && quantity.is_sign_negative()) {
                return Err("split amounts must be nonzero; an omitted remainder must be positive".into());
            }
            sum = sum.checked_add(quantity).ok_or("split total overflow")?;
            // Keep the remainder symbolic; the server validates against the current draft.
            out.push(pb::SplitInput { account_id: *id, quantity: if text.is_empty() { String::new() } else { quantity.to_string() }, memo: String::new() });
        }
        if sum != total {
            return Err(format!("splits total {sum}; expected {total} {}", self.currency));
        }
        Ok(out)
    }
    pub fn category_created(&mut self, id: i64, label: String) {
        self.accounts.push((id, label.clone(), self.currency.clone()));
        self.step = Step::Amount(id, label, String::new());
    }
    pub fn key(&mut self, key: KeyEvent) -> Action {
        self.error.clear();
        match &mut self.step {
            Step::List => match key.code {
                KeyCode::Esc => return Action::Cancel,
                KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(self.rows.len().saturating_sub(1)),
                KeyCode::Char('a') => {
                    self.editing = None;
                    self.step = Step::Category(Picker::new("Category", self.accounts.clone()));
                }
                KeyCode::Char('e') => {
                    if let Some((id, label, amount)) = self.rows.get(self.selected).cloned() {
                        self.editing = Some(self.selected);
                        self.step = Step::Amount(id, label, amount);
                    }
                }
                KeyCode::Char('x') | KeyCode::Delete => {
                    if self.selected < self.rows.len() {
                        self.rows.remove(self.selected);
                        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
                    }
                }
                KeyCode::Enter => match self.inputs() {
                    Ok(_) => return Action::Apply,
                    Err(e) => self.error = e,
                },
                _ => {}
            },
            Step::Category(picker) => match key.code {
                KeyCode::Esc => self.step = Step::List,
                KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
                KeyCode::Down => picker.selected = (picker.selected + 1).min(picker.visible().len().saturating_sub(1)),
                KeyCode::Backspace => {
                    picker.filter.pop();
                    picker.selected = 0;
                }
                KeyCode::Enter => {
                    if let Some((id, label)) = picker.current() {
                        self.step = Step::Amount(id, label, String::new());
                    }
                }
                KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => return Action::CreateCategory(picker.filter.clone()),
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    picker.filter.push(c);
                    picker.selected = 0;
                }
                _ => {}
            },
            Step::Amount(id, label, value) => match key.code {
                KeyCode::Esc => self.step = Step::List,
                KeyCode::Backspace => {
                    value.pop();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => value.push(c),
                KeyCode::Enter => {
                    let text = value.trim().replace(',', ".");
                    if !text.is_empty() && Decimal::from_str(&text).map_or(true, |v| v.is_zero()) {
                        self.error = "enter a nonzero amount, or leave blank for the final remainder".into();
                    } else {
                        let row = (*id, label.clone(), text);
                        if let Some(i) = self.editing.take() {
                            self.rows[i] = row;
                        } else {
                            self.rows.push(row);
                            self.selected = self.rows.len() - 1;
                        }
                        self.step = Step::List;
                    }
                }
                _ => {}
            },
        }
        Action::Continue
    }
    pub fn lines(&self) -> Vec<String> {
        let mut out = vec![format!("Total: {} {} · amounts in this currency", self.total, self.currency)];
        match &self.step {
            Step::List => {
                out.push(format!(
                    "a add · e edit amount · x remove · Enter {} · Esc cancel",
                    match self.target {
                        Target::Capture => "use splits",
                        Target::Review(_) => "preview posting",
                        Target::Existing(_) => "preview changes",
                    }
                ));
                out.push("Leave only the final amount blank to use the remainder.".into());
                for (i, (_, label, amount)) in self.rows.iter().enumerate().skip(self.selected.saturating_sub(8)).take(14) {
                    let shown = if amount.is_empty() {
                        let remainder = Decimal::from_str(&self.total.trim().replace(',', "."))
                            .ok()
                            .and_then(|total| self.rows[..i].iter().try_fold(total, |remaining, (_, _, q)| remaining.checked_sub(Decimal::from_str(&q.trim().replace(',', ".")).ok()?)));
                        remainder.map(|v| format!("{v} (remainder)")).unwrap_or_else(|| "remainder".into())
                    } else {
                        amount.clone()
                    };
                    out.push(format!("{} {label}: {shown}", if i == self.selected { ">" } else { " " }));
                }
            }
            Step::Category(p) => {
                out.push(format!("Category: {} · type to filter · Enter select · Ctrl-N create · Esc back", p.filter));
                for (i, (_, label, _)) in p.visible().iter().enumerate().skip(p.selected.saturating_sub(8)).take(14) {
                    out.push(format!("{} {label}", if i == p.selected { ">" } else { " " }));
                }
            }
            Step::Amount(_, label, value) => {
                out.push(format!("Amount for {label}: {value}"));
                out.push("Enter save leg · blank = final remainder · Esc back".into());
            }
        }
        if !self.error.is_empty() {
            out.push(self.error.clone());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn press(editor: &mut Editor, code: KeyCode) -> Action {
        editor.key(KeyEvent::new(code, KeyModifiers::NONE))
    }
    #[test]
    fn capture_and_review_share_add_edit_delete_remainder_and_cancel() {
        for target in [Target::Capture, Target::Review(42)] {
            let mut form = Editor::new(target, "90.50".into(), "USD".into(), vec![], vec![(1, "Expenses:Food".into(), "USD".into())], "expense".into());
            press(&mut form, KeyCode::Char('a'));
            press(&mut form, KeyCode::Enter);
            for c in "72.40".chars() {
                press(&mut form, KeyCode::Char(c));
            }
            press(&mut form, KeyCode::Enter);
            assert!(form.inputs().is_err());
            press(&mut form, KeyCode::Char('a'));
            press(&mut form, KeyCode::Enter);
            press(&mut form, KeyCode::Enter);
            assert!(matches!(press(&mut form, KeyCode::Enter), Action::Apply));
            assert_eq!(form.inputs().unwrap()[0].quantity, "72.40");
            assert_eq!(form.inputs().unwrap()[1].quantity, "");
            press(&mut form, KeyCode::Char('e'));
            for c in "18.10".chars() {
                press(&mut form, KeyCode::Char(c));
            }
            press(&mut form, KeyCode::Enter);
            assert!(form.inputs().is_ok());
            press(&mut form, KeyCode::Char('x'));
            assert!(form.inputs().is_err());
            assert!(matches!(press(&mut form, KeyCode::Esc), Action::Cancel));
        }
    }
    #[test]
    fn split_editor_refuses_overallocation_incomplete_and_nonfinal_remainders() {
        for rows in [
            vec![(1, "A".into(), "".into()), (2, "B".into(), "1".into())],
            vec![(1, "A".into(), "11".into()), (2, "B".into(), "".into())],
            vec![(1, "A".into(), "-1".into()), (2, "B".into(), "10".into())],
        ] {
            let mut form = Editor::new(Target::Review(1), "10".into(), "USD".into(), rows, vec![], "expense".into());
            assert!(matches!(press(&mut form, KeyCode::Enter), Action::Continue));
            assert!(!form.error.is_empty());
        }
    }
}
