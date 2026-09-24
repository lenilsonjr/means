-- Invoices received or issued elsewhere, recorded as facts and linked to the ledger.
-- Amounts are INTEGER minor units of the invoice commodity. Dates are TEXT YYYY-MM-DD.
-- The engine derives the status of an invoice from its settlements. The status column keeps
-- the last derived value; a cancelled invoice stays cancelled.
CREATE TABLE invoices (
    id INTEGER PRIMARY KEY,
    uid TEXT NOT NULL UNIQUE,
    entity_id INTEGER NOT NULL REFERENCES entities(id),
    direction TEXT NOT NULL CHECK (direction IN ('sales','purchase')),
    party_id INTEGER REFERENCES payees(id),
    number TEXT NOT NULL CHECK (length(trim(number)) > 0),
    series TEXT NOT NULL DEFAULT '',
    issue_date TEXT NOT NULL,
    tax_point TEXT NOT NULL,
    due_date TEXT,
    commodity TEXT NOT NULL,
    net INTEGER NOT NULL,
    tax INTEGER NOT NULL DEFAULT 0,
    gross INTEGER NOT NULL CHECK (gross = net + tax),
    withheld INTEGER NOT NULL DEFAULT 0 CHECK (withheld >= 0 AND withheld <= gross),
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open','partially_settled','settled','cancelled')),
    kind TEXT NOT NULL DEFAULT 'invoice' CHECK (kind IN ('invoice','credit_note')),
    credits_invoice_id INTEGER REFERENCES invoices(id),
    -- A deleted draft leaves the invoice without a recognition entry.
    recognition_entry_id INTEGER REFERENCES journal_entries(id) ON DELETE SET NULL,
    document_id INTEGER REFERENCES documents(id),
    external_ref TEXT NOT NULL DEFAULT '',
    metadata TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (entity_id, direction, series, number)
);
CREATE INDEX invoices_open ON invoices (entity_id, direction, status, due_date);
CREATE INDEX invoices_party ON invoices (party_id) WHERE party_id IS NOT NULL;
CREATE INDEX invoices_credits ON invoices (credits_invoice_id) WHERE credits_invoice_id IS NOT NULL;
CREATE INDEX invoices_recognition ON invoices (recognition_entry_id) WHERE recognition_entry_id IS NOT NULL;

CREATE TABLE invoice_lines (
    id INTEGER PRIMARY KEY,
    invoice_id INTEGER NOT NULL REFERENCES invoices(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    -- A decimal string, for example '1' or '2.5'.
    quantity TEXT NOT NULL DEFAULT '1',
    unit_price INTEGER NOT NULL,
    net INTEGER NOT NULL,
    tax_code TEXT NOT NULL DEFAULT '',
    tax INTEGER NOT NULL DEFAULT 0,
    account_id INTEGER REFERENCES accounts(id),
    UNIQUE (invoice_id, position)
);

-- A settlement reduces the open balance of an invoice. The amount is in the invoice commodity.
-- A positive amount reduces the open balance. A settlement goes away with its entry.
CREATE TABLE settlements (
    id INTEGER PRIMARY KEY,
    uid TEXT NOT NULL UNIQUE,
    invoice_id INTEGER NOT NULL REFERENCES invoices(id),
    entry_id INTEGER REFERENCES journal_entries(id) ON DELETE CASCADE,
    -- An entry edit replaces postings; the engine moves the link to the replacement posting.
    posting_id INTEGER REFERENCES postings(id) ON DELETE SET NULL,
    date TEXT NOT NULL,
    amount INTEGER NOT NULL CHECK (amount <> 0),
    kind TEXT NOT NULL CHECK (kind IN ('payment','credit_note','withholding','fee','fx_difference','write_off','refund')),
    fx_difference_entry_id INTEGER REFERENCES journal_entries(id) ON DELETE SET NULL,
    -- The credit note that this settlement applies, for kind credit_note.
    credit_note_id INTEGER REFERENCES invoices(id),
    created_at TEXT NOT NULL
);
CREATE INDEX settlements_invoice ON settlements (invoice_id, date);
CREATE INDEX settlements_entry ON settlements (entry_id) WHERE entry_id IS NOT NULL;
CREATE INDEX settlements_posting ON settlements (posting_id) WHERE posting_id IS NOT NULL;

-- A deleted settlement changes the status of its invoice. Void entries do not count.
CREATE TRIGGER settlements_delete_status AFTER DELETE ON settlements
BEGIN
  UPDATE invoices SET
    status = CASE
      WHEN (SELECT COALESCE(SUM(s.amount), 0) FROM settlements s LEFT JOIN journal_entries e ON e.id = s.entry_id
            WHERE s.invoice_id = OLD.invoice_id AND COALESCE(e.status, '') <> 'void') >= gross THEN 'settled'
      WHEN (SELECT COALESCE(SUM(s.amount), 0) FROM settlements s LEFT JOIN journal_entries e ON e.id = s.entry_id
            WHERE s.invoice_id = OLD.invoice_id AND COALESCE(e.status, '') <> 'void') > 0 THEN 'partially_settled'
      ELSE 'open' END,
    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
  WHERE id = OLD.invoice_id AND status <> 'cancelled';
END;
