-- Indexes for the lookups that lists and matching run per row.
CREATE INDEX IF NOT EXISTS je_reverses ON journal_entries (reverses_id) WHERE reverses_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS je_counterpart ON journal_entries (counterpart_id) WHERE counterpart_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS je_date ON journal_entries (date, id);
CREATE INDEX IF NOT EXISTS sl_posting ON statement_lines (posting_id) WHERE posting_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS postings_rate_missing ON postings (rate_source) WHERE rate_source = 'missing';
