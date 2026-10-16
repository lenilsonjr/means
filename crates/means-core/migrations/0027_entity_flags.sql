-- Facts that external tools ask for: the address and the flags of an entity, the identifiers of
-- an account, and the residence and citizenship of a party. They never change postings.
-- JSON object with the keys street, street2, city, region, postal_code and country.
ALTER TABLE entities ADD COLUMN address TEXT NOT NULL DEFAULT '{}';
-- JSON object of flag keys and scalar values. It holds the undated flags and the current
-- values of the dated flags. A dated flag reads its values from entity_flag_history.
ALTER TABLE entities ADD COLUMN flags TEXT NOT NULL DEFAULT '{}';

-- The dated values of a flag. A period starts on effective_from (inclusive) and ends on
-- effective_to (exclusive). A NULL effective_to keeps the period open.
CREATE TABLE entity_flag_history (
    id INTEGER PRIMARY KEY,
    entity_id INTEGER NOT NULL REFERENCES entities(id),
    key TEXT NOT NULL CHECK (length(trim(key)) > 0),
    -- A JSON scalar: a string, a number or a boolean.
    value TEXT NOT NULL,
    effective_from TEXT NOT NULL,
    effective_to TEXT CHECK (effective_to IS NULL OR effective_to > effective_from),
    created_at TEXT NOT NULL
);
CREATE INDEX entity_flag_history_entity ON entity_flag_history(entity_id, key, effective_from);

-- ISO 3166-1 alpha-2, or '' when unknown.
ALTER TABLE accounts ADD COLUMN country TEXT NOT NULL DEFAULT '';
-- Without spaces, uppercase, or ''.
ALTER TABLE accounts ADD COLUMN iban TEXT NOT NULL DEFAULT '';
ALTER TABLE accounts ADD COLUMN bic TEXT NOT NULL DEFAULT '';
ALTER TABLE accounts ADD COLUMN institution TEXT NOT NULL DEFAULT '';
-- Free text, for example the state where the asset is.
ALTER TABLE accounts ADD COLUMN location TEXT NOT NULL DEFAULT '';

-- ISO 3166-1 alpha-2, or '' when unknown.
ALTER TABLE payees ADD COLUMN residence TEXT NOT NULL DEFAULT '';
ALTER TABLE payees ADD COLUMN citizenship TEXT NOT NULL DEFAULT '';
