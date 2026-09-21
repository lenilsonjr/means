-- means: books that survive an audit. Schema v1.
-- Money columns are INTEGER. At this version they are scaled by 10^8; migration 0007 rescales
-- them to minor units of each commodity (D14, see money.rs). Dates are TEXT YYYY-MM-DD.

CREATE TABLE entities (
  id          INTEGER PRIMARY KEY,
  uid         TEXT NOT NULL UNIQUE,
  name        TEXT NOT NULL UNIQUE,
  kind        TEXT NOT NULL DEFAULT 'person' CHECK (kind IN ('person','company')),
  country     TEXT NOT NULL DEFAULT '',
  currency    TEXT NOT NULL,
  lock_date   TEXT,
  archived_at TEXT,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);

CREATE TABLE commodities (
  id        INTEGER PRIMARY KEY,
  code      TEXT NOT NULL UNIQUE,
  kind      TEXT NOT NULL DEFAULT 'currency' CHECK (kind IN ('currency','security','crypto')),
  name      TEXT NOT NULL DEFAULT '',
  precision INTEGER NOT NULL DEFAULT 2,
  isin      TEXT NOT NULL DEFAULT ''
);

CREATE TABLE prices (
  id           INTEGER PRIMARY KEY,
  commodity_id INTEGER NOT NULL REFERENCES commodities(id),
  currency_id  INTEGER NOT NULL REFERENCES commodities(id),
  on_date      TEXT NOT NULL,
  price        TEXT NOT NULL,
  source       TEXT NOT NULL DEFAULT 'manual',
  UNIQUE (commodity_id, currency_id, on_date)
);
CREATE INDEX prices_lookup ON prices (commodity_id, currency_id, on_date);

CREATE TABLE accounts (
  id            INTEGER PRIMARY KEY,
  uid           TEXT NOT NULL UNIQUE,
  entity_id     INTEGER NOT NULL REFERENCES entities(id),
  parent_id     INTEGER REFERENCES accounts(id),
  code          TEXT NOT NULL DEFAULT '',
  name          TEXT NOT NULL,
  type          TEXT NOT NULL CHECK (type IN ('asset','liability','equity','income','expense')),
  subtype       TEXT NOT NULL DEFAULT '',
  commodity_id  INTEGER NOT NULL REFERENCES commodities(id),
  system_role   TEXT NOT NULL DEFAULT '',
  placeholder   INTEGER NOT NULL DEFAULT 0,
  in_net_worth  INTEGER NOT NULL DEFAULT 1,
  credit_limit  TEXT,
  statement_day INTEGER,
  due_day       INTEGER,
  external_ids  TEXT NOT NULL DEFAULT '{}',
  notes         TEXT NOT NULL DEFAULT '',
  position      INTEGER NOT NULL DEFAULT 0,
  closed_at     TEXT,
  created_at    TEXT NOT NULL,
  updated_at    TEXT NOT NULL
);
CREATE UNIQUE INDEX accounts_unique_name ON accounts (entity_id, COALESCE(parent_id, 0), type, name);
CREATE INDEX accounts_entity ON accounts (entity_id);
CREATE INDEX accounts_role ON accounts (entity_id, system_role) WHERE system_role <> '';

CREATE TABLE entry_templates (
  id          INTEGER PRIMARY KEY,
  uid         TEXT NOT NULL UNIQUE,
  entity_id   INTEGER NOT NULL REFERENCES entities(id),
  name        TEXT NOT NULL,
  payee       TEXT NOT NULL DEFAULT '',
  description TEXT NOT NULL DEFAULT '',
  lines       TEXT NOT NULL DEFAULT '[]',
  rrule       TEXT NOT NULL DEFAULT '',
  starts_on   TEXT,
  next_on     TEXT,
  ends_on     TEXT,
  auto_post   INTEGER NOT NULL DEFAULT 0,
  lead_days   INTEGER NOT NULL DEFAULT 0,
  version     INTEGER NOT NULL DEFAULT 1,
  active      INTEGER NOT NULL DEFAULT 1,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);

CREATE TABLE journal_entries (
  id               INTEGER PRIMARY KEY,
  uid              TEXT NOT NULL UNIQUE,
  entity_id        INTEGER NOT NULL REFERENCES entities(id),
  date             TEXT NOT NULL,
  payee            TEXT NOT NULL DEFAULT '',
  description      TEXT NOT NULL DEFAULT '',
  notes            TEXT NOT NULL DEFAULT '',
  status           TEXT NOT NULL DEFAULT 'posted' CHECK (status IN ('draft','posted','void')),
  reverses_id      INTEGER REFERENCES journal_entries(id),
  counterpart_id   INTEGER REFERENCES journal_entries(id),
  template_id      INTEGER REFERENCES entry_templates(id),
  template_version INTEGER,
  origin           TEXT NOT NULL DEFAULT 'capture',
  posted_at        TEXT,
  seq              INTEGER,
  prev_hash        TEXT,
  hash             TEXT,
  created_at       TEXT NOT NULL,
  updated_at       TEXT NOT NULL
);
CREATE INDEX je_entity_date ON journal_entries (entity_id, date);
CREATE INDEX je_status ON journal_entries (status);
CREATE UNIQUE INDEX je_seq ON journal_entries (entity_id, seq) WHERE seq IS NOT NULL;

CREATE TABLE postings (
  id               INTEGER PRIMARY KEY,
  uid              TEXT NOT NULL UNIQUE,
  journal_entry_id INTEGER NOT NULL REFERENCES journal_entries(id) ON DELETE CASCADE,
  account_id       INTEGER NOT NULL REFERENCES accounts(id),
  quantity         INTEGER NOT NULL,
  amount           INTEGER NOT NULL,
  rate             TEXT,
  rate_source      TEXT NOT NULL DEFAULT '',
  memo             TEXT NOT NULL DEFAULT '',
  metadata         TEXT NOT NULL DEFAULT '{}',
  external_id      TEXT,
  fingerprint      TEXT,
  reconciled_at    TEXT,
  position         INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX postings_account ON postings (account_id);
CREATE INDEX postings_entry ON postings (journal_entry_id);
CREATE UNIQUE INDEX postings_external ON postings (account_id, external_id) WHERE external_id IS NOT NULL;
CREATE INDEX postings_fingerprint ON postings (fingerprint) WHERE fingerprint IS NOT NULL;

CREATE TABLE imports (
  id              INTEGER PRIMARY KEY,
  uid             TEXT NOT NULL UNIQUE,
  source          TEXT NOT NULL,
  account_id      INTEGER REFERENCES accounts(id),
  filename        TEXT NOT NULL DEFAULT '',
  checksum        TEXT NOT NULL,
  status          TEXT NOT NULL DEFAULT 'done',
  period_from     TEXT,
  period_to       TEXT,
  opening_balance TEXT,
  closing_balance TEXT,
  lines_count     INTEGER NOT NULL DEFAULT 0,
  created_count   INTEGER NOT NULL DEFAULT 0,
  matched_count   INTEGER NOT NULL DEFAULT 0,
  duplicate_count INTEGER NOT NULL DEFAULT 0,
  skipped_count   INTEGER NOT NULL DEFAULT 0,
  unmatched_count INTEGER NOT NULL DEFAULT 0,
  error_count     INTEGER NOT NULL DEFAULT 0,
  error           TEXT NOT NULL DEFAULT '',
  options         TEXT NOT NULL DEFAULT '{}',
  content         BLOB,
  created_at      TEXT NOT NULL
);
CREATE UNIQUE INDEX imports_checksum ON imports (source, COALESCE(account_id, 0), checksum);

CREATE TABLE statement_lines (
  id               INTEGER PRIMARY KEY,
  import_id        INTEGER NOT NULL REFERENCES imports(id) ON DELETE CASCADE,
  account_id       INTEGER REFERENCES accounts(id),
  position         INTEGER NOT NULL,
  raw              TEXT NOT NULL DEFAULT '{}',
  date             TEXT,
  amount           INTEGER,
  currency         TEXT NOT NULL DEFAULT '',
  description      TEXT NOT NULL DEFAULT '',
  reference        TEXT NOT NULL DEFAULT '',
  balance_after    INTEGER,
  fingerprint      TEXT NOT NULL DEFAULT '',
  posting_id       INTEGER REFERENCES postings(id) ON DELETE SET NULL,
  journal_entry_id INTEGER REFERENCES journal_entries(id) ON DELETE SET NULL,
  duplicate_of_id  INTEGER REFERENCES statement_lines(id),
  status           TEXT NOT NULL DEFAULT 'unmatched',
  note             TEXT NOT NULL DEFAULT ''
);
CREATE INDEX sl_account_status ON statement_lines (account_id, status);
CREATE INDEX sl_fingerprint ON statement_lines (fingerprint);
CREATE INDEX sl_reference ON statement_lines (account_id, reference);
CREATE INDEX sl_entry ON statement_lines (journal_entry_id);

CREATE TABLE rules (
  id          INTEGER PRIMARY KEY,
  entity_id   INTEGER NOT NULL REFERENCES entities(id),
  name        TEXT NOT NULL,
  position    INTEGER NOT NULL DEFAULT 0,
  enabled     INTEGER NOT NULL DEFAULT 1,
  conditions  TEXT NOT NULL DEFAULT '[]',
  account_id  INTEGER REFERENCES accounts(id),
  template_id INTEGER REFERENCES entry_templates(id),
  payee       TEXT NOT NULL DEFAULT '',
  hits_count  INTEGER NOT NULL DEFAULT 0,
  created_at  TEXT NOT NULL
);

CREATE TABLE audit_log (
  id         INTEGER PRIMARY KEY,
  at         TEXT NOT NULL,
  table_name TEXT NOT NULL,
  row_id     INTEGER NOT NULL,
  action     TEXT NOT NULL,
  before     TEXT,
  after      TEXT
);
CREATE INDEX audit_row ON audit_log (table_name, row_id);

CREATE TABLE settings (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
