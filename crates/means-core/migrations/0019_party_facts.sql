-- Facts about a payee as a party, and its dated relationships to an entity.
-- A period starts on effective_from (inclusive) and ends on effective_to (exclusive).
-- A NULL effective_to keeps the period open.
-- ISO 3166-1 alpha-2, or '' when unknown.
ALTER TABLE payees ADD COLUMN country TEXT NOT NULL DEFAULT '';
-- JSON object of KEY: value, for example {"VAT": "..."}.
ALTER TABLE payees ADD COLUMN tax_ids TEXT NOT NULL DEFAULT '{}';
ALTER TABLE payees ADD COLUMN kind TEXT NOT NULL DEFAULT 'other' CHECK (kind IN ('person', 'company', 'authority', 'other'));

CREATE TABLE party_relationships (
    id INTEGER PRIMARY KEY,
    payee_id INTEGER NOT NULL REFERENCES payees(id),
    entity_id INTEGER NOT NULL REFERENCES entities(id),
    kind TEXT NOT NULL CHECK (kind IN ('owner', 'subsidiary', 'director', 'related_party', 'contractor', 'employee', 'customer', 'supplier', 'lender', 'borrower')),
    -- Basis points: 10000 is 100%. NULL when the relationship has no ownership.
    ownership_bp INTEGER CHECK (ownership_bp IS NULL OR ownership_bp BETWEEN 0 AND 10000),
    effective_from TEXT NOT NULL,
    effective_to TEXT CHECK (effective_to IS NULL OR effective_to > effective_from),
    basis TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    -- The relationship stays in the books of the payee's own entity.
    FOREIGN KEY (payee_id, entity_id) REFERENCES payees(id, entity_id)
);
CREATE INDEX party_relationships_entity ON party_relationships(entity_id, effective_from);
CREATE INDEX party_relationships_payee ON party_relationships(payee_id, kind, effective_from);
