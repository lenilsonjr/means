//! Create a synthetic ledger for public TUI recordings. Refuse an existing path.
use means_core::{accounts, entities, imports, journal, rates, tags, AccountType, Db, EntryInput, EntryStatus, PostingInput};
use rust_decimal::Decimal;

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).ok_or_else(|| anyhow::anyhow!("usage: website_demo NEW_DATABASE_PATH"))?;
    std::fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
    let db = Db::open(path)?;
    let mut c = db.conn();
    let today = means_core::today();
    let personal = entities::create_entity(&mut c, "Personal", "person", "US", "USD")?;
    let studio = entities::create_entity(&mut c, "Machine Studio", "company", "US", "USD")?;
    let bank = accounts::ensure_account(&c, personal.id, AccountType::Asset, &["Current"], "bank", "USD")?;
    let euro = accounts::ensure_account(&c, personal.id, AccountType::Asset, &["Europe"], "bank", "EUR")?;
    let savings = accounts::ensure_account(&c, personal.id, AccountType::Asset, &["Savings"], "bank", "USD")?;
    let opening = accounts::ensure_account(&c, personal.id, AccountType::Equity, &["Opening"], "equity", "USD")?;
    let income = accounts::ensure_account(&c, personal.id, AccountType::Income, &["Consulting"], "income", "USD")?;
    let mut expense = Vec::new();
    for (name, class) in [("Groceries", "committed"), ("Household", "committed"), ("Records", "discretionary"), ("Studio", "discretionary"), ("Rent", "fixed"), ("Transport", "committed")] {
        let account = accounts::ensure_account(&c, personal.id, AccountType::Expense, &[name], "expense", "USD")?;
        c.execute("UPDATE accounts SET class=?2 WHERE id=?1", means_core::rusqlite::params![account.id, class])?;
        expense.push(account.id);
    }
    rates::set_price(&c, "EUR", "USD", today - chrono::Duration::days(40), "1.08".parse()?, "synthetic demo")?;
    for (account, amount) in [(bank.id, "7800"), (savings.id, "18500"), (euro.id, "2100")] {
        let mut e = EntryInput::new(personal.id, today - chrono::Duration::days(35));
        e.payee = "Opening balance".into();
        e.origin = "synthetic-demo".into();
        e.postings = vec![PostingInput::new(account, amount.parse()?), PostingInput::balancing(opening.id)];
        let e = journal::create_entry(&mut c, e)?;
        journal::mark_reviewed(&c, &[e.id])?;
    }
    let transactions = [
        (28, "Signal Studio", income.id, "2400.00", "income"),
        (24, "Northside Apartments", expense[4], "1250.00", "expense"),
        (21, "Atom Heart Records", expense[2], "48.00", "expense"),
        (18, "Station Coffee", expense[0], "14.20", "expense"),
        (15, "Analog Supply", expense[3], "159.42", "expense"),
        (12, "Moonlight Market", expense[0], "63.80", "expense"),
        (9, "City Transit", expense[5], "28.50", "expense"),
        (6, "Signal Studio", income.id, "3200.00", "income"),
        (4, "Electric Workshop", expense[3], "86.00", "expense"),
        (2, "Quiet Corner Records", expense[2], "32.00", "expense"),
        (1, "Moonlight Market", expense[0], "45.70", "expense"),
    ];
    for (days, payee, target, amount, kind) in transactions {
        let e = journal::create_simple(
            &mut c,
            journal::SimpleEntry {
                entity_id: personal.id,
                date: today - chrono::Duration::days(days),
                kind: kind.into(),
                account_id: bank.id,
                contra_account_id: Some(target),
                quantity: amount.parse::<Decimal>()?,
                contra_quantity: None,
                payee: payee.into(),
                notes: "Fictional transaction for the means demo".into(),
                splits: vec![],
                status: EntryStatus::Posted,
                fee: None,
                fee_account_id: None,
                origin: "synthetic-demo".into(),
            },
        )?;
        if target == expense[3] {
            tags::set_tags(&mut c, e.id, &[("project".into(), "the-machine".into())])?;
        }
    }
    let studio_bank = accounts::ensure_account(&c, studio.id, AccountType::Asset, &["Operating"], "bank", "USD")?;
    let studio_income = accounts::ensure_account(&c, studio.id, AccountType::Income, &["Sessions"], "income", "USD")?;
    let mut e = EntryInput::new(studio.id, today - chrono::Duration::days(3));
    e.payee = "The Signal Sessions".into();
    e.origin = "synthetic-demo".into();
    e.postings = vec![PostingInput::new(studio_bank.id, "4500".parse()?), PostingInput::balancing(studio_income.id)];
    let e = journal::create_entry(&mut c, e)?;
    journal::mark_reviewed(&c, &[e.id])?;
    let csv = format!("\"Date (UTC)\",\"Description\",\"Amount\",\"Status\",\"Transaction ID\"\n\"{}\",\"Moonlight Market + Home\",\"-90.50\",\"Sent\",\"DEMO-SPLIT-001\"\n", today.format("%m-%d-%Y"));
    imports::run_import(&mut c, imports::ImportRequest::new("mercury_csv", Some(bank.id), "synthetic-statement.csv", csv.as_bytes()))?;
    println!("Created synthetic demo: two vaults, native USD/EUR accounts, one imported draft.");
    Ok(())
}
