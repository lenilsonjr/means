-- Class: the budget nature of an expense account (see docs/expense-reports.md).
-- One of fixed | committed | discretionary | savings | not-spending, or '' for none.
ALTER TABLE accounts ADD COLUMN class TEXT NOT NULL DEFAULT '';
