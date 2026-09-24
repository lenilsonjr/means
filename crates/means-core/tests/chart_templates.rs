//! Chart templates in charts/: apply each one to a fresh ledger, check the statutory keys, and apply it again.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;

use chrono::NaiveDate;
use means_core::accounts::{self, AccountUpdate, ChartNode};
use means_core::model::*;
use means_core::rusqlite::Connection;
use means_core::{entities, journal, Db};
use rust_decimal::Decimal;

#[derive(serde::Deserialize)]
struct Template {
    accounts: Vec<ChartNode>,
}

fn load(id: &str) -> Vec<ChartNode> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../charts").join(format!("{id}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str::<Template>(&text).unwrap().accounts
}

fn account_count(conn: &Connection, entity_id: i64) -> usize {
    accounts::list_accounts(conn, Some(entity_id), true).unwrap().len()
}

fn balances(conn: &Connection, entity_id: i64) -> BTreeMap<String, Decimal> {
    accounts::list_accounts_with_balances(conn, Some(entity_id), true, None).unwrap().into_iter().map(|a| (a.path, a.balance)).collect()
}

fn account(conn: &Connection, entity_id: i64, path: &str) -> Account {
    accounts::find_by_path(conn, entity_id, path).unwrap().unwrap_or_else(|| panic!("no account {path}"))
}

/// Every node that gives a statutory key has that value on its account.
fn assert_keys_written(conn: &Connection, entity_id: i64, nodes: &[ChartNode]) {
    for n in nodes.iter().filter(|n| n.path.contains(':')) {
        let a = account(conn, entity_id, &n.path);
        if let Some(v) = &n.statutory_code {
            assert_eq!(&a.statutory_code, v, "statutory_code of {}", n.path);
        }
        if let Some(v) = &n.tax_treatment {
            assert_eq!(&a.tax_treatment, v, "tax_treatment of {}", n.path);
        }
        assert_eq!(a.intercompany, n.intercompany.unwrap_or(false), "intercompany of {}", n.path);
        assert_eq!(a.code, n.code, "code of {}", n.path);
    }
}

/// Post one expense from the first asset leaf to the first expense leaf of the chart.
fn post_sample(conn: &mut Connection, entity_id: i64, nodes: &[ChartNode]) {
    let leaf = |t: &str| nodes.iter().find(|n| n.r#type == t && !n.placeholder).map(|n| account(conn, entity_id, &n.path)).unwrap();
    let (asset, expense) = (leaf("asset"), leaf("expense"));
    journal::create_simple(
        conn,
        journal::SimpleEntry {
            entity_id,
            date: NaiveDate::from_ymd_opt(2026, 3, 14).unwrap(),
            kind: "expense".into(),
            account_id: asset.id,
            contra_account_id: None,
            quantity: Decimal::from_str("125.40").unwrap(),
            contra_quantity: None,
            payee: "Example Supplies Ltd".into(),
            notes: String::new(),
            splits: vec![(expense.id, Decimal::from_str("125.40").unwrap(), String::new())],
            status: EntryStatus::Posted,
            fee: None,
            fee_account_id: None,
            origin: String::new(),
        },
    )
    .unwrap();
}

struct Expect {
    id: &'static str,
    kind: &'static str,
    currency: &'static str,
    nodes: usize,
    /// Template paths that match a system account of the entity.
    system_overlap: usize,
}

/// Apply a template to a fresh entity, check it, then apply it again and check that nothing changed.
fn apply_and_reapply(e: &Expect) -> (Db, i64) {
    let nodes = load(e.id);
    assert_eq!(nodes.len(), e.nodes, "{} account count", e.id);
    assert!(nodes.iter().any(|n| n.tax_treatment.is_some()), "{} has tax treatments", e.id);
    assert!(nodes.iter().any(|n| n.statutory_code.is_some()), "{} has statutory codes", e.id);

    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let entity = entities::create_entity(&mut conn, "Example Holder", e.kind, "XX", e.currency).unwrap();
    let before = account_count(&conn, entity.id);
    // A root path such as "Assets" names the type. The engine keeps no account for it.
    let rows = nodes.iter().filter(|n| n.path.contains(':')).count();

    let (created, existing) = accounts::apply_chart(&mut conn, entity.id, &nodes).unwrap();
    assert_eq!((created, existing), (rows - e.system_overlap, e.system_overlap), "{} first apply", e.id);
    assert_eq!(account_count(&conn, entity.id), before + created);
    assert_keys_written(&conn, entity.id, &nodes);

    post_sample(&mut conn, entity.id, &nodes);
    let balances_before = balances(&conn, entity.id);
    assert!(balances_before.values().any(|b| !b.is_zero()));

    // A user edit to a statutory key goes back to the template value on the next apply.
    let tagged = nodes.iter().find(|n| n.tax_treatment.is_some()).unwrap();
    let edited = account(&conn, entity.id, &tagged.path);
    accounts::update_account(&conn, edited.id, AccountUpdate { tax_treatment: Some("stale".into()), statutory_code: Some("stale".into()), ..Default::default() }).unwrap();

    let (created, existing) = accounts::apply_chart(&mut conn, entity.id, &nodes).unwrap();
    assert_eq!((created, existing), (0, rows), "{} second apply", e.id);
    assert_eq!(account_count(&conn, entity.id), before + rows - e.system_overlap);
    assert_keys_written(&conn, entity.id, &nodes);
    assert_eq!(balances(&conn, entity.id), balances_before, "{} balances after re-apply", e.id);
    drop(conn);
    (db, entity.id)
}

#[test]
fn sg_pte_ltd_applies_with_sfrs_lines_gst_and_group_balances() {
    let (db, id) = apply_and_reapply(&Expect { id: "sg-pte-ltd", kind: "company", currency: "SGD", nodes: 77, system_overlap: 3 });
    let conn = db.conn();
    let output = account(&conn, id, "Liabilities:Current liabilities:GST output tax payable");
    assert_eq!((output.tax_treatment.as_str(), output.statutory_code.as_str()), ("output_tax", "sfrs:TradeAndOtherPayables"));
    assert_eq!(account(&conn, id, "Assets:Current assets:GST input tax receivable").tax_treatment, "input_tax");
    let fees = account(&conn, id, "Expenses:Related-party charges:Management fees to related parties");
    assert!(fees.intercompany);
    assert!(account(&conn, id, "Income:Investment income:Dividend income from subsidiary").intercompany);
    // The system FX account takes the template's code instead of a twin.
    let fx = account(&conn, id, "Expenses:FX gain/loss");
    assert_eq!((fx.system_role.as_str(), fx.code.as_str()), ("fx_gain_loss", "7800"));
}

#[test]
fn es_sl_pgc_pymes_applies_with_pgc_account_numbers() {
    let (db, id) = apply_and_reapply(&Expect { id: "es-sl-pgc-pymes", kind: "company", currency: "EUR", nodes: 81, system_overlap: 2 });
    let conn = db.conn();
    for (path, code, treatment) in [
        ("Assets:Tesorería:Bancos", "572", ""),
        ("Assets:Deudores:Hacienda Pública, IVA soportado", "472", "input_tax"),
        ("Liabilities:Administraciones públicas:Hacienda Pública, IVA repercutido", "477", "output_tax"),
        ("Liabilities:Administraciones públicas:Hacienda Pública, acreedora por retenciones practicadas", "4751", "withholding_payable"),
        ("Equity:Capital:Capital social", "100", "owner_capital"),
        ("Income:Ingresos de explotación:Prestaciones de servicios", "705", ""),
        ("Expenses:Gastos financieros:Diferencias negativas de cambio", "668", ""),
    ] {
        let a = account(&conn, id, path);
        assert_eq!((a.statutory_code.as_str(), a.tax_treatment.as_str()), (code, treatment), "{path}");
    }
    assert!(account(&conn, id, "Assets:Deudores:Cuenta corriente con socios").intercompany);
}

#[test]
fn pt_individual_applies_with_foreign_capital_income_tags() {
    let (db, id) = apply_and_reapply(&Expect { id: "pt-individual", kind: "person", currency: "EUR", nodes: 80, system_overlap: 1 });
    let conn = db.conn();
    let nodes = load("pt-individual");
    let capital: Vec<&ChartNode> = nodes.iter().filter(|n| !n.placeholder && (n.path.starts_with("Income:Dividends:") || n.path.starts_with("Income:Interest:Foreign"))).collect();
    assert_eq!(capital.len(), 4);
    for n in capital {
        let a = account(&conn, id, &n.path);
        assert_eq!((a.tax_treatment.as_str(), a.statutory_code.as_str()), ("foreign_capital_income", "irs:E"), "{}", n.path);
    }
    assert!(account(&conn, id, "Assets:Loans to companies:Loan to Company A").intercompany);
    assert_eq!(account(&conn, id, "Equity:Opening balances").system_role, "opening_balance");
}

#[test]
fn us_llc_single_member_applies_with_member_balances() {
    let (db, id) = apply_and_reapply(&Expect { id: "us-llc-single-member", kind: "company", currency: "USD", nodes: 82, system_overlap: 2 });
    let conn = db.conn();
    let due_to = account(&conn, id, "Liabilities:Due to member/parent");
    assert!(due_to.intercompany);
    assert_eq!(due_to.tax_treatment, "owner_capital");
    let due_from = account(&conn, id, "Assets:Due from member/parent");
    assert!(due_from.intercompany);
    assert_eq!(due_from.tax_treatment, "owner_distribution");
    let contributions = account(&conn, id, "Equity:Owner contributions");
    assert_eq!((contributions.system_role.as_str(), contributions.tax_treatment.as_str()), ("owner_contributions", "owner_capital"));
    assert_eq!(account(&conn, id, "Equity:Owner distributions").tax_treatment, "owner_distribution");
    // The existing income and expense accounts keep their paths and codes.
    assert_eq!(account(&conn, id, "Income:Revenue:Consulting and services").statutory_code, "schc:1");
    assert_eq!(account(&conn, id, "Expenses:Other expenses:Miscellaneous").code, "7230");
}

#[test]
fn node_without_statutory_keys_keeps_the_values_of_the_account() {
    let db = Db::open_memory().unwrap();
    let mut conn = db.conn();
    let entity = entities::create_entity(&mut conn, "Example Co", "company", "XX", "EUR").unwrap();
    let tagged: Vec<ChartNode> = serde_json::from_str(
        r#"[{"code": "2100", "path": "Liabilities:Tax payable", "type": "liability", "placeholder": false,
             "statutory_code": "demo:21", "tax_treatment": "output_tax", "intercompany": true}]"#,
    )
    .unwrap();
    // The old format: no statutory keys at all.
    let plain: Vec<ChartNode> = serde_json::from_str(r#"[{"code": "2100", "path": "Liabilities:Tax payable", "type": "liability", "placeholder": false}]"#).unwrap();
    assert!(plain[0].statutory_code.is_none() && plain[0].tax_treatment.is_none() && plain[0].intercompany.is_none());

    accounts::apply_chart(&mut conn, entity.id, &tagged).unwrap();
    accounts::apply_chart(&mut conn, entity.id, &plain).unwrap();
    let a = account(&conn, entity.id, "Liabilities:Tax payable");
    assert_eq!((a.statutory_code.as_str(), a.tax_treatment.as_str(), a.intercompany), ("demo:21", "output_tax", true));

    // An explicit empty value or false clears the key.
    let cleared: Vec<ChartNode> =
        serde_json::from_str(r#"[{"code": "2100", "path": "Liabilities:Tax payable", "type": "liability", "statutory_code": "", "tax_treatment": "", "intercompany": false}]"#).unwrap();
    accounts::apply_chart(&mut conn, entity.id, &cleared).unwrap();
    let a = account(&conn, entity.id, "Liabilities:Tax payable");
    assert_eq!((a.statutory_code.as_str(), a.tax_treatment.as_str(), a.intercompany), ("", "", false));
}

#[test]
fn migration_21_adds_empty_statutory_columns_to_existing_accounts() {
    let conn = Connection::open_in_memory().unwrap();
    means_core::db::migrate_to(&conn, 16).unwrap();
    conn.execute_batch(
        "INSERT INTO commodities (code, kind, name, precision) VALUES ('EUR', 'currency', 'Euro', 2);
         INSERT INTO entities (uid, name, kind, country, currency, created_at, updated_at) VALUES ('e1', 'Example', 'person', 'XX', 'EUR', '2026-01-01', '2026-01-01');
         INSERT INTO accounts (uid, entity_id, name, type, subtype, commodity_id, created_at, updated_at)
           VALUES ('a1', 1, 'Bank', 'asset', 'bank', 1, '2026-01-01', '2026-01-01');",
    )
    .unwrap();
    means_core::db::migrate_to(&conn, i64::MAX).unwrap();
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(version, 21);
    let a = accounts::get_account(&conn, 1).unwrap();
    assert_eq!((a.statutory_code.as_str(), a.tax_treatment.as_str(), a.intercompany), ("", "", false));
}
