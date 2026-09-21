-- Tags: key:value lenses on entries (city:lisbon, trip:alentejo-2026). Deliberately outside
-- the hash chain: retagging history must not rewrite it. Multiple values per key are allowed.
CREATE TABLE entry_tags (
  entry_id INTEGER NOT NULL REFERENCES journal_entries(id) ON DELETE CASCADE,
  key      TEXT NOT NULL,
  value    TEXT NOT NULL DEFAULT '',
  PRIMARY KEY (entry_id, key, value)
) WITHOUT ROWID;
CREATE INDEX entry_tags_kv ON entry_tags (key, value);
ALTER TABLE rules ADD COLUMN tags TEXT NOT NULL DEFAULT '';
