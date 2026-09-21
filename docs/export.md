# Beancount export

Export every entity in one read snapshot:

```sh
means export --format beancount --output books.beancount
bean-check books.beancount
fava books.beancount
```

Omit `--output` to write to stdout. An output path must be new; means refuses to overwrite existing files, including the ledger itself.

The export uses **book values**: every posting contains its stored amount in the entity’s functional currency. Each entity’s trial balance therefore matches means. Foreign quantities and currencies remain available as `means-quantity` and `means-commodity` posting metadata. Use native quantities as metadata when inspecting foreign movements.

Posted entries, voided originals, and their reversals are included. Drafts are excluded. Archived entities and closed accounts remain included, and the export has no journal UI row limit. It reads stored book values with read-only database access.

Accounts are grouped by type and entity, with numeric entity/account IDs in their names to prevent collisions. Original entity names and full account paths are retained in metadata. Names are sanitized for Beancount; unsupported currency identifiers (and the reserved `MEANSX` prefix) use an unambiguous hexadecimal escape, with the original code in commodity metadata. Entry IDs, UIDs, status, reversal links, notes, tags, and posting IDs/memos are retained as metadata. Keep a separate ledger backup alongside this report.

The syntax follows the [Beancount language specification](https://beancount.github.io/docs/beancount_language_syntax/).

For integration verification, install Beancount in an isolated Python environment and run:

```sh
MEANS_BEAN_PYTHON=/path/to/venv/bin/python cargo test -p means-core --test export -- --include-ignored
MEANS_BEAN_CHECK=/path/to/venv/bin/bean-check cargo test -p means-server --test cli beancount_export
```

The first command compares Beancount’s parsed posting balances with a multi-entity golden fixture and means’ trial balances. The second checks the real CLI output with `bean-check`.
