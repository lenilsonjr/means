-- Learned defaults for the inbox: which account a bank file lands in, by source and filename shape.
-- filename_glob '' is the source-level default; a non-empty glob wins over it for matching filenames.
CREATE TABLE import_profiles (
  id            INTEGER PRIMARY KEY,
  source        TEXT NOT NULL,
  filename_glob TEXT NOT NULL DEFAULT '',
  account_id    INTEGER NOT NULL REFERENCES accounts(id),
  hits_count    INTEGER NOT NULL DEFAULT 0,
  updated_at    TEXT NOT NULL,
  UNIQUE (source, filename_glob)
);
