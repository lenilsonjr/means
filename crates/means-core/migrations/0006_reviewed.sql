-- Branch C of the review decision: machine-posted entries (rules, schedules, imports) carry no
-- review mark until a human confirms or edits them. History predates the flag: marked reviewed.
ALTER TABLE journal_entries ADD COLUMN reviewed_at TEXT;
UPDATE journal_entries SET reviewed_at = created_at;
CREATE INDEX je_unreviewed ON journal_entries (entity_id) WHERE reviewed_at IS NULL AND status = 'posted';
