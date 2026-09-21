#!/usr/bin/env python3
"""Generate docs/erd.md from the schema the migrations build. Run after adding a migration:

    python3 scripts/gen_erd.py

The output is a generated mirror; field semantics live in docs/data-model.md section 3.
"""
import pathlib
import sqlite3

root = pathlib.Path(__file__).resolve().parents[1]
db = sqlite3.connect(":memory:")
for f in sorted((root / "crates/means-core/migrations").glob("*.sql")):
    db.executescript(f.read_text())

tables = [r[0] for r in db.execute("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")]
order = ["entities", "commodities", "prices", "accounts", "journal_entries", "postings", "entry_templates", "rules", "imports", "statement_lines", "import_profiles", "audit_log", "settings"]
tables.sort(key=lambda t: (order.index(t) if t in order else 99, t))

lines = [
    "# Entity-relationship diagram",
    "",
    "Generated from `crates/means-core/migrations/` by `scripts/gen_erd.py`; regenerate after a migration, never edit.",
    "Field semantics live in `docs/data-model.md` section 3. Money columns are INTEGER counts of their commodity's minor unit (D14); dates are TEXT `YYYY-MM-DD`.",
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
