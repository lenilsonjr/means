CREATE TABLE payees (
 id INTEGER PRIMARY KEY,
 uid TEXT NOT NULL UNIQUE,
 entity_id INTEGER NOT NULL REFERENCES entities(id),
 name TEXT NOT NULL CHECK(length(trim(name)) > 0),
 active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1)),
 UNIQUE(id, entity_id)
);
CREATE TABLE payee_aliases (
 payee_id INTEGER NOT NULL REFERENCES payees(id),
 text TEXT NOT NULL,
 normalized TEXT NOT NULL CHECK(length(normalized)>0),
 PRIMARY KEY(payee_id, normalized)
);
ALTER TABLE journal_entries ADD COLUMN payee_id INTEGER REFERENCES payees(id);
CREATE INDEX entries_payee ON journal_entries(payee_id);
CREATE TRIGGER payee_entity_insert BEFORE INSERT ON journal_entries
WHEN NEW.payee_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM payees WHERE id=NEW.payee_id AND entity_id=NEW.entity_id)
BEGIN SELECT RAISE(ABORT, 'payee belongs to another entity'); END;
CREATE TRIGGER payee_entity_update BEFORE UPDATE OF payee_id,entity_id ON journal_entries
WHEN NEW.payee_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM payees WHERE id=NEW.payee_id AND entity_id=NEW.entity_id)
BEGIN SELECT RAISE(ABORT, 'payee belongs to another entity'); END;
CREATE TRIGGER payee_entity_immutable BEFORE UPDATE OF entity_id ON payees
WHEN NEW.entity_id != OLD.entity_id
BEGIN SELECT RAISE(ABORT, 'payee cannot move between entities'); END;
