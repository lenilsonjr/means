-- Typed links between two journal entries in different vaults.
-- A link records the native amounts of the intercompany side of each entry and the implied rate.
-- The engine keeps the entities of the two entries different.
-- status follows the pair: draft while either entry is a draft, posted when both are posted,
-- void when the engine voids the pair.
-- A deleted entry deletes its links.
CREATE TABLE intervault_links (
    id INTEGER PRIMARY KEY,
    uid TEXT NOT NULL UNIQUE,
    entry_a_id INTEGER NOT NULL REFERENCES journal_entries(id) ON DELETE CASCADE,
    entry_b_id INTEGER NOT NULL REFERENCES journal_entries(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('transfer','capital','loan','interest','dividend','service','recharge','settlement','other')),
    -- Minor units of commodity_a and commodity_b.
    amount_a INTEGER NOT NULL,
    commodity_a TEXT NOT NULL,
    amount_b INTEGER NOT NULL,
    commodity_b TEXT NOT NULL,
    -- Units of commodity_b per unit of commodity_a, as a decimal string.
    rate TEXT NOT NULL DEFAULT '',
    note TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft','posted','void')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (entry_a_id, entry_b_id),
    CHECK (entry_a_id <> entry_b_id)
);
CREATE INDEX intervault_links_entry_b ON intervault_links (entry_b_id);
CREATE INDEX intervault_links_status ON intervault_links (status);
