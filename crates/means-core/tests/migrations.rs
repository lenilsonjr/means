//! The shipped migration files, the repair of migration 31 and the schema that an older SQLite
//! must read.

use means_core::{db, entities, Db};
use sha2::{Digest, Sha256};

/// The SHA-256 of each shipped migration file. A shipped migration never changes: a change in
/// place makes ledgers that ran the old file differ from new ledgers. Add a new migration to
/// repair an old one, and add its file here.
const PINS: &[(&str, &str)] = &[
    ("0001_init.sql", "0764e812a6b3e967b1a80bb5e15041834cd2ada725e8a72407c8340f28fde347"),
    ("0002_indexes.sql", "a6f3e82e3b1cb959c2a318da3e5bfb3ef6f28becb93345d6f66250b67820c220"),
    ("0003_import_profiles.sql", "0ca408c4b24957047bdf6ff180571cb73cc451b8f1390ec206d1bda28e8ca0c7"),
    ("0004_entry_tags.sql", "bc0110a6358e3d591aa379e255efe638557e3bddc4c41069a25e0e6841378cd8"),
    ("0005_account_class.sql", "bb39889144be3590149c1c9ff3c206e30556ed8c85cfae6ae85227a240e2f3a7"),
    ("0006_reviewed.sql", "a5b788baad09f9f632a52e5d9eff0926b02a1f7c2b994c3824dc1030ad527c11"),
    ("0008_channel_connections.sql", "58ece17af31f23cc3051f9c38c2ad36ddb5ec2a154c251e3b62d011adb3f4349"),
    ("0009_enable_banking_sessions.sql", "a3a14d280902e3a25684b479ead37c77406aa1597bf9e89a5534ad82def0e8f7"),
    ("0010_enable_banking_aliases.sql", "142ea8fe65e12ff0a8d1c5d3f2111a6db167653645e1821ae7948438fe1385d8"),
    ("0011_refunds.sql", "fcd71db2c2ee933a625b2c4925bc8c7ef46ab56610c39c531deb65682ad552fe"),
    ("0014_budgets.sql", "830722e7947b591709e02dd31839f951e96888cc38c5b42cac0a9901d8954f0c"),
    ("0015_connections_ui.sql", "b8e657be8ccfe53ee125370e6b3e84a5427d303413b7b977a2d470d75182d8ae"),
    ("0016_payees.sql", "4c17d3ac37cf2c2e0d9ccc2800a983a443646ae52f7d83dbb1e5dad4a75f3153"),
    ("0018_entity_facts.sql", "d5a8835537e0431e819504f70e14e5b912f8085f11d24903f05a4f08e2080765"),
    ("0019_party_facts.sql", "2e9c633b050167038f9871ff6b3d95f0922396e0eaa6817cfdcc5d40a74b0e78"),
    ("0020_documents.sql", "4e95718191daaae867bb170362eb0988425d45e9cc0cb5179a9091cba2ef2c3f"),
    ("0021_statutory_codes.sql", "8d913806b8927f5270ff1912267c94d4451db57e6a970568a826fbd2a63b2d39"),
    ("0022_tax_events.sql", "480a836d9d18cc9337e335a14ac2cd9faed935175cb9f511125ea5617ed18e2e"),
    ("0024_close_and_filings.sql", "084d57fa6ebc3227335e5452620f260e1d2ad0899da32812112872335d47c6bf"),
    ("0025_intervault_links.sql", "c05b725738a3935a82de49fc410cbee6c27bf6f6110b793c6a9dcb7ebb27e329"),
    ("0026_invoices.sql", "c245187c5323aeb7927c16c8dbd86f093f660892b0040d66354f3ebcff4d03f4"),
    ("0027_entity_flags.sql", "b46de5b60b7f4339b8406857adf68a930cbc4982428427d4a542689cf15287ae"),
    ("0028_entity_active_names.sql", "d7824c72a15d3185cb3267c6d7adcd1bb47fbc6075eb6edadfb724e811c89dde"),
    ("0029_archive_guards.sql", "0055b7eefbf8c02f38816e9341a86664eb1825f15379d848d13cd9b6c82ec336"),
    ("0030_account_counterparty.sql", "9577b5883a34123a37b9a0967fa68d5aef763a6a3a458534f45a5886d2bbb430"),
    ("0031_repair_names_and_guards.sql", "f06fc898db09f3a704189b469abc73004dfb6aeae7db18b8000a25429f2573ca"),
];

fn migrations_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations")
}

#[test]
fn no_shipped_migration_file_changes() {
    let mut files: Vec<String> = std::fs::read_dir(migrations_dir()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).filter(|n| n.ends_with(".sql")).collect();
    files.sort();
    let pinned: Vec<&str> = PINS.iter().map(|(f, _)| *f).collect();
    assert_eq!(files, pinned, "each migration file needs a pin, and a pin needs its file");
    for (file, pin) in PINS {
        let digest = hex::encode(Sha256::digest(std::fs::read(migrations_dir().join(file)).unwrap()));
        assert_eq!(&digest, pin, "{file} changed after it shipped; add a new migration instead");
    }
}

fn triggers(conn: &rusqlite::Connection) -> Vec<(String, String)> {
    let mut stmt = conn.prepare("SELECT name, sql FROM sqlite_master WHERE type = 'trigger' ORDER BY name").unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().collect::<Result<_, _>>().unwrap()
}

#[test]
fn no_trigger_builds_its_message_with_an_expression() {
    let db = Db::open_memory().unwrap();
    let c = db.conn();
    let all = triggers(&c);
    assert!(all.iter().filter(|(n, _)| n.starts_with("archive_guard_")).count() >= 70);
    for (name, sql) in all {
        // SQLite 3.43 refuses RAISE with an expression, and then cannot read the schema.
        assert!(!sql.contains("||"), "{name}: {sql}");
    }
}

#[test]
fn the_system_sqlite_reads_the_ledger_and_runs_the_migration_scripts() {
    let Ok(probe) = std::process::Command::new("python3").args(["-c", "import sqlite3"]).output() else {
        eprintln!("python3 is not available; the test checks nothing");
        return;
    };
    if !probe.status.success() {
        eprintln!("python3 has no sqlite3 module; the test checks nothing");
        return;
    }
    let dir = std::env::temp_dir().join(format!("means-migrations-{}", means_core::new_uid()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ledger.db");
    {
        let db = Db::open(&path).unwrap();
        let mut c = db.conn();
        let e = entities::create_entity(&mut c, "Example Archive Ltd", "company", "GB", "GBP").unwrap();
        entities::archive(&mut c, e.id, "Test User", "test").unwrap();
    }
    // The ledger file, and the scripts as scripts/gen_erd.py runs them.
    let script = r#"
import pathlib, sqlite3, sys
c = sqlite3.connect(sys.argv[1])
print(c.execute("SELECT count(*) FROM entities").fetchone()[0])
m = sqlite3.connect(":memory:")
for f in sorted(pathlib.Path(sys.argv[2]).glob("*.sql")):
    m.executescript(f.read_text())
print(m.execute("SELECT count(*) FROM sqlite_master WHERE type = 'trigger'").fetchone()[0])
print(sqlite3.sqlite_version)
"#;
    let out = std::process::Command::new("python3").args(["-c", script, path.to_str().unwrap(), migrations_dir().to_str().unwrap()]).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "1", "{text}");
    assert!(lines[1].parse::<i64>().unwrap() >= 70, "{text}");
}

#[test]
fn migration_31_repairs_a_ledger_with_the_old_index_and_triggers() {
    let raw = rusqlite::Connection::open_in_memory().unwrap();
    db::migrate_to(&raw, 30).unwrap();
    raw.execute_batch(
        "INSERT INTO commodities (code, kind, name, precision) VALUES ('GBP', 'currency', 'Pound sterling', 2);
         INSERT INTO entities (uid, name, currency, created_at, updated_at) VALUES ('e-1', 'Woodgrove Ltd', 'GBP', 'now', 'now');",
    )
    .unwrap();
    // The first version of migration 28 made a case-sensitive index.
    let twin = "INSERT INTO entities (uid, name, currency, created_at, updated_at) VALUES ('e-2', 'WOODGROVE LTD', 'GBP', 'now', 'now')";
    raw.execute(twin, []).unwrap();
    raw.execute("DELETE FROM entities WHERE uid = 'e-2'", []).unwrap();
    // Another version of migration 29 built its messages with an expression.
    raw.execute_batch(
        "DROP TRIGGER archive_guard_entities_update;
         CREATE TRIGGER archive_guard_entities_update BEFORE UPDATE ON entities
         WHEN OLD.archived_at IS NOT NULL AND NEW.archived_at IS NOT NULL
         BEGIN SELECT RAISE(ABORT, 'entity_archived: ' || OLD.name || ' is archived'); END;",
    )
    .unwrap();
    let guards_before = triggers(&raw).iter().filter(|(n, _)| n.starts_with("archive_guard_")).count();

    db::migrate_to(&raw, 31).unwrap();
    assert!(raw.execute(twin, []).is_err(), "the index of migration 31 ignores case");
    let after = triggers(&raw);
    assert_eq!(after.iter().filter(|(n, _)| n.starts_with("archive_guard_")).count(), guards_before);
    assert!(after.iter().all(|(_, sql)| !sql.contains("||")));
    // The repair can run again without harm.
    raw.execute_batch(include_str!("../migrations/0031_repair_names_and_guards.sql")).unwrap();
}

#[test]
fn migration_31_refuses_active_names_that_differ_only_in_case() {
    let raw = rusqlite::Connection::open_in_memory().unwrap();
    db::migrate_to(&raw, 30).unwrap();
    raw.execute_batch(
        "INSERT INTO commodities (code, kind, name, precision) VALUES ('GBP', 'currency', 'Pound sterling', 2);
         INSERT INTO entities (uid, name, currency, created_at, updated_at) VALUES ('e-1', 'Woodgrove Ltd', 'GBP', 'now', 'now');
         INSERT INTO entities (uid, name, currency, created_at, updated_at) VALUES ('e-2', 'WOODGROVE LTD', 'GBP', 'now', 'now');",
    )
    .unwrap();
    let err = db::migrate_to(&raw, 31).unwrap_err().to_string();
    assert!(err.contains("'Woodgrove Ltd' (#1)") && err.contains("'WOODGROVE LTD' (#2)") && err.contains("differ only in case"), "{err}");
    let version: i64 = raw.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    assert_eq!(version, 30);
    // A case twin that is archived does not block the migration.
    raw.execute("UPDATE entities SET archived_at = 'now' WHERE id = 2", []).unwrap();
    db::migrate_to(&raw, 31).unwrap();
}
