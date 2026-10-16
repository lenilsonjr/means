# Beancount export

Export every active entity in one read transaction:

```sh
means export --format beancount --output books.beancount
bean-check books.beancount
fava books.beancount
```

Omit `--output` to write to stdout. An output path must be new; means refuses to overwrite existing files, including the ledger itself.

The export uses **book values**: every posting contains its stored amount in the entity’s functional currency. Each entity’s trial balance therefore matches means. Foreign quantities and currencies remain available as `means-quantity` and `means-commodity` posting metadata. Use native quantities as metadata when inspecting foreign movements.

Posted entries, voided originals, and their reversals are included. Drafts are excluded. Closed accounts remain included, and the export has no journal UI row limit. Archived entities are excluded by default, and the export names each one it leaves out on stderr. `--include-archived` includes them, and `--entity` includes an archived entity by name. It reads stored book values in one read transaction and writes nothing to the ledger.

Accounts are grouped by type and entity, with numeric entity/account IDs in their names to prevent collisions. Original entity names and full account paths are retained in metadata. Names are sanitized for Beancount; unsupported currency identifiers (and the reserved `MEANSX` prefix) use an unambiguous hexadecimal escape, with the original code in commodity metadata. Entry IDs, UIDs, status, reversal links, notes, tags, and posting IDs/memos are retained as metadata. Keep a separate ledger backup alongside this report.

`--entity` (repeatable) limits the export to some entities. `--as-of YYYY-MM-DD` leaves out the entries dated after that day.

## Snapshot export

A snapshot gives external tools the complete books in one directory:

```sh
means export --format snapshot --output DIR [--entity E]... [--include-archived] [--as-of YYYY-MM-DD]
means export verify DIR --json
```

The snapshot holds JSON files for entities, accounts, entries, drafts, parties, documents, statement lines, rates, tax codes, tax events, invoices, filings and inter-vault links. It also holds a copy of each document file and the Beancount export of the same scope. One read transaction gives all records. The manifest has the SHA-256 of every file, the record counts, the chain heads and the control totals of each entity. means writes the files into a temporary directory and then renames it to `DIR`. It refuses an existing `DIR`. `means export verify` hashes the files again and exits with code 1 on a mismatch. See the [snapshot format](snapshot-format.md) for the contract.

## Beancount syntax

The syntax follows the [Beancount language specification](https://beancount.github.io/docs/beancount_language_syntax/).

For integration verification, install Beancount in an isolated Python environment and run:

```sh
MEANS_BEAN_PYTHON=/path/to/venv/bin/python cargo test -p means-core --test export -- --include-ignored
MEANS_BEAN_CHECK=/path/to/venv/bin/bean-check cargo test -p means-server --test cli beancount_export
```

The first command compares Beancount’s parsed posting balances with a multi-entity golden fixture and means’ trial balances. The second checks the real CLI output with `bean-check`.
