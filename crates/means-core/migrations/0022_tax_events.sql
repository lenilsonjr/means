-- Tax codes and tax events.
-- A tax code is a dated rate version. A period starts on effective_from (inclusive) and ends on
-- effective_to (exclusive). A NULL effective_to keeps the period open.
-- Rates are basis points: 10000 is 100%.
CREATE TABLE tax_codes (
    id INTEGER PRIMARY KEY,
    uid TEXT NOT NULL UNIQUE,
    entity_id INTEGER NOT NULL REFERENCES entities(id),
    code TEXT NOT NULL CHECK (length(trim(code)) > 0),
    name TEXT NOT NULL DEFAULT '',
    kind TEXT NOT NULL CHECK (kind IN ('output', 'input', 'reverse_charge', 'exempt', 'out_of_scope', 'withholding', 'zero_rated')),
    rate_bp INTEGER NOT NULL DEFAULT 0 CHECK (rate_bp >= 0),
    tax_account_id INTEGER REFERENCES accounts(id),
    recoverable_bp INTEGER NOT NULL DEFAULT 10000 CHECK (recoverable_bp BETWEEN 0 AND 10000),
    jurisdiction TEXT NOT NULL DEFAULT '',
    effective_from TEXT NOT NULL,
    effective_to TEXT CHECK (effective_to IS NULL OR effective_to > effective_from),
    active INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1)),
    created_at TEXT NOT NULL,
    UNIQUE (entity_id, code, effective_from)
);

-- One tax event per taxed posting and tax point. Amounts are minor units of the functional
-- currency (commodity). Signs follow the side of the taxed posting: debit positive.
-- The native fields keep the values in the commodity of the taxed posting when it differs.
-- A deleted draft deletes its events. A deleted posting leaves the event on the entry.
CREATE TABLE tax_events (
    id INTEGER PRIMARY KEY,
    uid TEXT NOT NULL UNIQUE,
    entity_id INTEGER NOT NULL REFERENCES entities(id),
    entry_id INTEGER NOT NULL REFERENCES journal_entries(id) ON DELETE CASCADE,
    posting_id INTEGER REFERENCES postings(id) ON DELETE SET NULL,
    tax_code_id INTEGER NOT NULL REFERENCES tax_codes(id),
    tax_point TEXT NOT NULL,
    base_amount INTEGER NOT NULL,
    tax_amount INTEGER NOT NULL,
    recoverable_amount INTEGER NOT NULL DEFAULT 0,
    withheld_amount INTEGER NOT NULL DEFAULT 0,
    commodity TEXT NOT NULL,
    native_base INTEGER,
    native_tax INTEGER,
    native_commodity TEXT,
    party_id INTEGER REFERENCES payees(id),
    document_ref TEXT NOT NULL DEFAULT '',
    rule_version TEXT NOT NULL DEFAULT '',
    corrects_event_id INTEGER REFERENCES tax_events(id),
    metadata TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL
);
CREATE INDEX tax_events_entity_point ON tax_events (entity_id, tax_point);
CREATE INDEX tax_events_entry ON tax_events (entry_id);
