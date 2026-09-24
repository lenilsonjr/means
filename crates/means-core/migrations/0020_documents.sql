-- Documents: invoices, receipts, contracts, statements and filed forms kept as evidence.
-- The file content lives in a content-addressed store next to the ledger file:
-- documents/<first two hex digits of sha256>/<sha256>. The row keeps the metadata.
CREATE TABLE documents (
  id          INTEGER PRIMARY KEY,
  uid         TEXT NOT NULL UNIQUE,
  entity_id   INTEGER NOT NULL REFERENCES entities(id),
  sha256      TEXT NOT NULL,
  filename    TEXT NOT NULL,
  mime        TEXT NOT NULL DEFAULT '',
  size        INTEGER NOT NULL,
  stored_path TEXT NOT NULL,
  source      TEXT NOT NULL CHECK (source IN ('email','upload','api','filing','import','other')),
  received_at TEXT NOT NULL,
  metadata    TEXT NOT NULL DEFAULT '{}',
  created_at  TEXT NOT NULL
);
CREATE UNIQUE INDEX documents_entity_sha256 ON documents (entity_id, sha256);
CREATE INDEX documents_received ON documents (entity_id, received_at);

-- A link attaches a document to an entry, a posting or a statement line.
-- SQLite treats NULL values as distinct in a unique index, so the index uses COALESCE.
-- A link goes away with a deleted entry or statement line. The document stays and
-- returns to the unlinked list. A deleted posting moves its links to the entry (trigger below).
CREATE TABLE document_links (
  id                INTEGER PRIMARY KEY,
  document_id       INTEGER NOT NULL REFERENCES documents(id),
  entry_id          INTEGER REFERENCES journal_entries(id) ON DELETE CASCADE,
  posting_id        INTEGER REFERENCES postings(id),
  statement_line_id INTEGER REFERENCES statement_lines(id) ON DELETE CASCADE,
  role              TEXT NOT NULL DEFAULT '',
  created_at        TEXT NOT NULL,
  CHECK (entry_id IS NOT NULL OR posting_id IS NOT NULL OR statement_line_id IS NOT NULL)
);
CREATE UNIQUE INDEX document_links_unique ON document_links (document_id, COALESCE(entry_id, 0), COALESCE(posting_id, 0), COALESCE(statement_line_id, 0), role);
CREATE INDEX document_links_entry ON document_links (entry_id) WHERE entry_id IS NOT NULL;
CREATE INDEX document_links_posting ON document_links (posting_id) WHERE posting_id IS NOT NULL;
CREATE INDEX document_links_line ON document_links (statement_line_id) WHERE statement_line_id IS NOT NULL;

-- Entry edits and splits replace postings. Keep the evidence on the entry when its posting goes.
-- When the whole entry goes, the entry link cascades, so no new link is made.
CREATE TRIGGER document_links_posting_delete BEFORE DELETE ON postings
BEGIN
  INSERT OR IGNORE INTO document_links (document_id, entry_id, role, created_at)
    SELECT document_id, OLD.journal_entry_id, role, created_at FROM document_links
    WHERE posting_id = OLD.id AND EXISTS (SELECT 1 FROM journal_entries WHERE id = OLD.journal_entry_id);
  DELETE FROM document_links WHERE posting_id = OLD.id;
END;
