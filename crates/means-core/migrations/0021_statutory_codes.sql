-- Statutory mapping of an account.
-- statutory_code: a code from a national chart or a reporting taxonomy, free text ('' for none).
-- tax_treatment: a free-text tag, for example output_tax, input_tax, withholding_payable, owner_capital ('' for none).
-- intercompany: 1 when the account holds balances with a related party.
ALTER TABLE accounts ADD COLUMN statutory_code TEXT NOT NULL DEFAULT '';
ALTER TABLE accounts ADD COLUMN tax_treatment TEXT NOT NULL DEFAULT '';
ALTER TABLE accounts ADD COLUMN intercompany INTEGER NOT NULL DEFAULT 0;
