-- Legal and tax facts of an entity, its tax classifications and its groups.
-- These records describe the books. They never change postings.
-- A period starts on effective_from (inclusive) and ends on effective_to (exclusive).
-- A NULL effective_to keeps the period open.
ALTER TABLE entities ADD COLUMN legal_form TEXT NOT NULL DEFAULT '';
-- MM-DD, or '' when unknown.
ALTER TABLE entities ADD COLUMN fiscal_year_end TEXT NOT NULL DEFAULT '';
-- JSON object of KEY: value, for example registration and tax numbers.
ALTER TABLE entities ADD COLUMN registrations TEXT NOT NULL DEFAULT '{}';
ALTER TABLE entities ADD COLUMN incorporated_on TEXT;

CREATE TABLE entity_classifications (
    id INTEGER PRIMARY KEY,
    entity_id INTEGER NOT NULL REFERENCES entities(id),
    jurisdiction TEXT NOT NULL CHECK (length(trim(jurisdiction)) > 0),
    classification TEXT NOT NULL CHECK (length(trim(classification)) > 0),
    effective_from TEXT NOT NULL,
    effective_to TEXT CHECK (effective_to IS NULL OR effective_to > effective_from),
    note TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL
);
CREATE INDEX entity_classifications_entity ON entity_classifications(entity_id, jurisdiction, effective_from);

CREATE TABLE entity_groups (
    id INTEGER PRIMARY KEY,
    uid TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL UNIQUE CHECK (length(trim(name)) > 0),
    created_at TEXT NOT NULL
);

-- The code keeps each entity in one group at a time.
CREATE TABLE entity_group_members (
    id INTEGER PRIMARY KEY,
    group_id INTEGER NOT NULL REFERENCES entity_groups(id),
    entity_id INTEGER NOT NULL REFERENCES entities(id),
    parent_entity_id INTEGER REFERENCES entities(id),
    -- Basis points of the entity that the parent holds: 10000 is 100%.
    ownership_bp INTEGER NOT NULL DEFAULT 10000 CHECK (ownership_bp BETWEEN 0 AND 10000),
    control INTEGER NOT NULL DEFAULT 1 CHECK (control IN (0, 1)),
    effective_from TEXT NOT NULL,
    effective_to TEXT CHECK (effective_to IS NULL OR effective_to > effective_from),
    created_at TEXT NOT NULL,
    CHECK (parent_entity_id IS NULL OR parent_entity_id <> entity_id)
);
CREATE INDEX entity_group_members_group ON entity_group_members(group_id, effective_from);
CREATE INDEX entity_group_members_entity ON entity_group_members(entity_id, effective_from);
