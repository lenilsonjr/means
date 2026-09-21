-- A refund is a new movement, not a void/reversal of the original bank evidence.
ALTER TABLE journal_entries ADD COLUMN refund_of_id INTEGER REFERENCES journal_entries(id);
CREATE UNIQUE INDEX journal_entries_active_refund ON journal_entries(refund_of_id)
  WHERE refund_of_id IS NOT NULL AND status <> 'void';
