use means_core::{accounts, entities, journal, reports, AccountType, Db, EntryInput, EntryStatus, PostingInput};
use rust_decimal::Decimal;
#[test]
fn recent_ledger_limits_after_ordering_and_preserves_chronological_balances() {
    let db = Db::open_memory().unwrap();
    let mut c = db.conn();
    let e = entities::create_entity(&mut c, "Personal", "person", "US", "USD").unwrap();
    let bank = accounts::ensure_account(&c, e.id, AccountType::Asset, &["Bank"], "bank", "USD").unwrap();
    let expense = accounts::ensure_account(&c, e.id, AccountType::Expense, &["Food"], "expense", "USD").unwrap();
    for (date, amount, status) in [("2024-01-01", 10, EntryStatus::Posted), ("2025-01-01", 20, EntryStatus::Posted), ("2026-01-01", 30, EntryStatus::Posted), ("2026-02-01", 99, EntryStatus::Draft)] {
        let mut input = EntryInput::new(e.id, date.parse().unwrap());
        input.status = status;
        input.postings = vec![PostingInput::new(bank.id, -Decimal::from(amount)), PostingInput::balancing(expense.id)];
        journal::create_entry(&mut c, input).unwrap();
    }
    for (account, sign) in [(bank.id, -1), (expense.id, 1)] {
        let recent = reports::general_ledger_ordered(&c, account, None, None, 2, false, true).unwrap();
        assert_eq!(recent.rows.iter().map(|r| r.date.to_string()).collect::<Vec<_>>(), ["2026-01-01", "2025-01-01"]);
        assert_eq!(recent.opening_balance.minor(), sign * 1000);
        assert_eq!(recent.closing_balance.minor(), sign * 6000);
        assert_eq!(recent.rows[0].running_balance.minor(), sign * 6000);
        assert_eq!(recent.rows[1].running_balance.minor(), sign * 3000);
        let with_draft = reports::general_ledger_ordered(&c, account, Some("2025-01-01".parse().unwrap()), None, 2, true, true).unwrap();
        assert_eq!(with_draft.rows[0].status, EntryStatus::Draft);
        assert_eq!(with_draft.opening_balance.minor(), sign * 3000);
        assert_eq!(with_draft.closing_balance.minor(), sign * 6000);
        assert_eq!(with_draft.rows[0].running_balance.minor(), sign * 6000);
        let oldest = reports::general_ledger(&c, account, None, None, 2, false).unwrap();
        assert_eq!(oldest.rows[0].date.to_string(), "2024-01-01");
        assert_eq!(oldest.opening_balance.minor(), 0);
        assert_eq!(oldest.closing_balance.minor(), sign * 3000);
    }
}
