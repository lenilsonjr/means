-- Period close, lock history and the filings register.
-- An audit event can name the person who acted and the reason for the change.
ALTER TABLE audit_log ADD COLUMN actor TEXT NOT NULL DEFAULT '';
ALTER TABLE audit_log ADD COLUMN reason TEXT NOT NULL DEFAULT '';

-- A filing records a return or a report that an entity prepares for an authority.
-- Dates are TEXT YYYY-MM-DD. The period includes period_start and period_end.
-- A filing in status submitted or accepted freezes its period: the engine refuses to unlock it
-- unless the caller amends it. An amendment is a new row with amends_id.
CREATE TABLE filings (
    id INTEGER PRIMARY KEY,
    uid TEXT NOT NULL UNIQUE,
    entity_id INTEGER NOT NULL REFERENCES entities(id),
    jurisdiction TEXT NOT NULL CHECK (length(trim(jurisdiction)) > 0),
    filing_code TEXT NOT NULL CHECK (length(trim(filing_code)) > 0),
    period_start TEXT NOT NULL,
    period_end TEXT NOT NULL CHECK (period_end >= period_start),
    status TEXT NOT NULL CHECK (status IN ('prepared','submitted','accepted','rejected','amended')),
    snapshot_digest TEXT NOT NULL DEFAULT '',
    chain_head_seq INTEGER,
    chain_head_hash TEXT,
    artifact_digest TEXT NOT NULL DEFAULT '',
    receipt_ref TEXT NOT NULL DEFAULT '',
    submitted_at TEXT,
    notes TEXT NOT NULL DEFAULT '',
    amends_id INTEGER REFERENCES filings(id),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX filings_entity ON filings (entity_id, jurisdiction, filing_code, period_start);
CREATE INDEX filings_amends ON filings (amends_id) WHERE amends_id IS NOT NULL;
