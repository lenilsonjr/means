-- An entity name is unique among the active entities only. An archived set of books keeps its
-- name, and a new entity can take the same name.
-- SQLite cannot drop a UNIQUE column constraint, so this script rebuilds the table. The runner
-- turns foreign keys off for it and checks every foreign key before the commit.
CREATE TABLE entities_new (
  id              INTEGER PRIMARY KEY,
  uid             TEXT NOT NULL UNIQUE,
  name            TEXT NOT NULL,
  kind            TEXT NOT NULL DEFAULT 'person' CHECK (kind IN ('person','company')),
  country         TEXT NOT NULL DEFAULT '',
  currency        TEXT NOT NULL,
  lock_date       TEXT,
  archived_at     TEXT,
  created_at      TEXT NOT NULL,
  updated_at      TEXT NOT NULL,
  legal_form      TEXT NOT NULL DEFAULT '',
  fiscal_year_end TEXT NOT NULL DEFAULT '',
  registrations   TEXT NOT NULL DEFAULT '{}',
  incorporated_on TEXT,
  address         TEXT NOT NULL DEFAULT '{}',
  flags           TEXT NOT NULL DEFAULT '{}'
);
INSERT INTO entities_new (id, uid, name, kind, country, currency, lock_date, archived_at, created_at, updated_at,
                          legal_form, fiscal_year_end, registrations, incorporated_on, address, flags)
  SELECT id, uid, name, kind, country, currency, lock_date, archived_at, created_at, updated_at,
         legal_form, fiscal_year_end, registrations, incorporated_on, address, flags
  FROM entities;
DROP TABLE entities;
ALTER TABLE entities_new RENAME TO entities;
CREATE UNIQUE INDEX entities_active_name ON entities(name) WHERE archived_at IS NULL;
