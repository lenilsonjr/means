#!/usr/bin/env python3
"""Generate docs/erd.md from the schema the migrations build. Run after adding a migration:

    python3 scripts/gen_erd.py

The output is a generated mirror. docs/data-model.md gives the meaning of each table.
"""
import pathlib
import sqlite3

root = pathlib.Path(__file__).resolve().parents[1]
db = sqlite3.connect(":memory:")
for f in sorted((root / "crates/means-core/migrations").glob("*.sql")):
    db.executescript(f.read_text())

tables = [r[0] for r in db.execute("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")]
# The order of docs/data-model.md: entities and facts, accounts, journal, evidence, tax, invoices,
# inter-vault links, close and filings, audit, then operational tables.
order = [
    "entities", "entity_classifications", "entity_groups", "entity_group_members",
    "commodities", "prices", "accounts",
    "journal_entries", "postings", "entry_tags", "entry_templates", "rules", "payees", "payee_aliases", "party_relationships", "budgets",
    "imports", "statement_lines", "import_profiles", "documents", "document_links",
    "tax_codes", "tax_events",
    "invoices", "invoice_lines", "settlements",
    "intervault_links",
    "filings",
    "audit_log", "settings",
    "channel_connections", "connection_jobs", "enable_banking_sessions", "enable_banking_account_aliases",
]
tables.sort(key=lambda t: (order.index(t) if t in order else 99, t))

lines = [
    "# Entity-relationship diagram",
    "",
    "Generated from `crates/means-core/migrations/` by `scripts/gen_erd.py`; regenerate after a migration, never edit.",
    "See [the accounting model](data-model.md) for the meaning of each table. Money columns are INTEGER counts of the minor unit of their commodity. Dates are TEXT `YYYY-MM-DD`.",
    "",
    "```mermaid",
    "erDiagram",
]
rels = []
for t in tables:
    fks = {}
    fk_notnull = {}
    for r in db.execute(f"PRAGMA foreign_key_list({t})"):
        fks[r[3]] = r[2]
    uniques = set()
    for seq, name, unique, origin, partial in db.execute(f"PRAGMA index_list({t})"):
        if unique:
            cols = [c[2] for c in db.execute(f"PRAGMA index_info({name})")]
            if len(cols) == 1 and cols[0]:
                uniques.add(cols[0])
    lines.append(f"  {t} {{")
    for cid, name, ctype, notnull, dflt, pk in db.execute(f"PRAGMA table_info({t})"):
        if name in fks:
            fk_notnull[name] = notnull
        marker = " PK" if pk else (" FK" if name in fks else (" UK" if name in uniques else ""))
        comment = f' "{fks[name]}"' if name in fks else ""
        lines.append(f"    {ctype or 'TEXT'} {name}{marker}{comment}")
    lines.append("  }")
    for col, parent in fks.items():
        left = "||" if fk_notnull.get(col) else "|o"
        rels.append(f'  {parent} {left}--o{{ {t} : "{col}"')
lines += sorted(set(rels))
lines += ["```", ""]
(root / "docs/erd.md").write_text("\n".join(lines))
print(f"docs/erd.md: {len(tables)} tables, {len(set(rels))} relationships")
