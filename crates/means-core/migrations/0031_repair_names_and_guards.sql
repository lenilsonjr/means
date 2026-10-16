-- Repair of migrations 28 and 29. A build of the first round of the archive work changed both
-- scripts in place: one version of 28 made a case-sensitive index, and one version of 29 named
-- the entity in each trigger message with an expression. SQLite 3.43 cannot read a trigger with an
-- expression in RAISE. This script makes every ledger the same, whatever version of 28 and 29 it
-- ran. It can run again without harm.
--
-- An entity name is unique among the active entities, without case. Before this script, the
-- runner refuses the migration when two active entities have names that differ only in case.
DROP INDEX IF EXISTS entities_active_name;
CREATE UNIQUE INDEX entities_active_name ON entities(name COLLATE NOCASE) WHERE archived_at IS NULL;

-- The archive guards again, with literal messages. The engine names the entity in its own check.
DROP TRIGGER IF EXISTS archive_guard_entities_update;
DROP TRIGGER IF EXISTS archive_guard_accounts_insert;
DROP TRIGGER IF EXISTS archive_guard_accounts_update;
DROP TRIGGER IF EXISTS archive_guard_accounts_delete;
DROP TRIGGER IF EXISTS archive_guard_journal_entries_insert;
DROP TRIGGER IF EXISTS archive_guard_journal_entries_update;
DROP TRIGGER IF EXISTS archive_guard_journal_entries_delete;
DROP TRIGGER IF EXISTS archive_guard_payees_insert;
DROP TRIGGER IF EXISTS archive_guard_payees_update;
DROP TRIGGER IF EXISTS archive_guard_payees_delete;
DROP TRIGGER IF EXISTS archive_guard_entry_templates_insert;
DROP TRIGGER IF EXISTS archive_guard_entry_templates_update;
DROP TRIGGER IF EXISTS archive_guard_entry_templates_delete;
DROP TRIGGER IF EXISTS archive_guard_rules_insert;
DROP TRIGGER IF EXISTS archive_guard_rules_update;
DROP TRIGGER IF EXISTS archive_guard_rules_delete;
DROP TRIGGER IF EXISTS archive_guard_budgets_insert;
DROP TRIGGER IF EXISTS archive_guard_budgets_update;
DROP TRIGGER IF EXISTS archive_guard_budgets_delete;
DROP TRIGGER IF EXISTS archive_guard_documents_insert;
DROP TRIGGER IF EXISTS archive_guard_documents_update;
DROP TRIGGER IF EXISTS archive_guard_documents_delete;
DROP TRIGGER IF EXISTS archive_guard_tax_codes_insert;
DROP TRIGGER IF EXISTS archive_guard_tax_codes_update;
DROP TRIGGER IF EXISTS archive_guard_tax_codes_delete;
DROP TRIGGER IF EXISTS archive_guard_tax_events_insert;
DROP TRIGGER IF EXISTS archive_guard_tax_events_update;
DROP TRIGGER IF EXISTS archive_guard_tax_events_delete;
DROP TRIGGER IF EXISTS archive_guard_invoices_insert;
DROP TRIGGER IF EXISTS archive_guard_invoices_update;
DROP TRIGGER IF EXISTS archive_guard_invoices_delete;
DROP TRIGGER IF EXISTS archive_guard_filings_insert;
DROP TRIGGER IF EXISTS archive_guard_filings_update;
DROP TRIGGER IF EXISTS archive_guard_filings_delete;
DROP TRIGGER IF EXISTS archive_guard_party_relationships_insert;
DROP TRIGGER IF EXISTS archive_guard_party_relationships_update;
DROP TRIGGER IF EXISTS archive_guard_party_relationships_delete;
DROP TRIGGER IF EXISTS archive_guard_entity_classifications_insert;
DROP TRIGGER IF EXISTS archive_guard_entity_classifications_update;
DROP TRIGGER IF EXISTS archive_guard_entity_classifications_delete;
DROP TRIGGER IF EXISTS archive_guard_entity_flag_history_insert;
DROP TRIGGER IF EXISTS archive_guard_entity_flag_history_update;
DROP TRIGGER IF EXISTS archive_guard_entity_flag_history_delete;
DROP TRIGGER IF EXISTS archive_guard_entity_group_members_insert;
DROP TRIGGER IF EXISTS archive_guard_entity_group_members_update;
DROP TRIGGER IF EXISTS archive_guard_entity_group_members_delete;
DROP TRIGGER IF EXISTS archive_guard_postings_insert;
DROP TRIGGER IF EXISTS archive_guard_postings_update;
DROP TRIGGER IF EXISTS archive_guard_postings_delete;
DROP TRIGGER IF EXISTS archive_guard_entry_tags_insert;
DROP TRIGGER IF EXISTS archive_guard_entry_tags_update;
DROP TRIGGER IF EXISTS archive_guard_entry_tags_delete;
DROP TRIGGER IF EXISTS archive_guard_intervault_links_insert;
DROP TRIGGER IF EXISTS archive_guard_intervault_links_update;
DROP TRIGGER IF EXISTS archive_guard_intervault_links_delete;
DROP TRIGGER IF EXISTS archive_guard_imports_insert;
DROP TRIGGER IF EXISTS archive_guard_imports_update;
DROP TRIGGER IF EXISTS archive_guard_imports_delete;
DROP TRIGGER IF EXISTS archive_guard_statement_lines_insert;
DROP TRIGGER IF EXISTS archive_guard_statement_lines_update;
DROP TRIGGER IF EXISTS archive_guard_statement_lines_delete;
DROP TRIGGER IF EXISTS archive_guard_payee_aliases_insert;
DROP TRIGGER IF EXISTS archive_guard_payee_aliases_update;
DROP TRIGGER IF EXISTS archive_guard_payee_aliases_delete;
DROP TRIGGER IF EXISTS archive_guard_document_links_insert;
DROP TRIGGER IF EXISTS archive_guard_document_links_update;
DROP TRIGGER IF EXISTS archive_guard_document_links_delete;
DROP TRIGGER IF EXISTS archive_guard_invoice_lines_insert;
DROP TRIGGER IF EXISTS archive_guard_invoice_lines_update;
DROP TRIGGER IF EXISTS archive_guard_invoice_lines_delete;
DROP TRIGGER IF EXISTS archive_guard_settlements_insert;
DROP TRIGGER IF EXISTS archive_guard_settlements_update;
DROP TRIGGER IF EXISTS archive_guard_settlements_delete;

-- The entity row itself changes only through an archive or an unarchive.
CREATE TRIGGER archive_guard_entities_update BEFORE UPDATE ON entities
WHEN OLD.archived_at IS NOT NULL AND NEW.archived_at IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;

-- accounts
CREATE TRIGGER archive_guard_accounts_insert BEFORE INSERT ON accounts
WHEN EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_accounts_update BEFORE UPDATE ON accounts
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_accounts_delete BEFORE DELETE ON accounts
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;

-- journal_entries
CREATE TRIGGER archive_guard_journal_entries_insert BEFORE INSERT ON journal_entries
WHEN EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_journal_entries_update BEFORE UPDATE ON journal_entries
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_journal_entries_delete BEFORE DELETE ON journal_entries
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;

-- payees
CREATE TRIGGER archive_guard_payees_insert BEFORE INSERT ON payees
WHEN EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_payees_update BEFORE UPDATE ON payees
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_payees_delete BEFORE DELETE ON payees
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;

-- entry_templates
CREATE TRIGGER archive_guard_entry_templates_insert BEFORE INSERT ON entry_templates
WHEN EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_entry_templates_update BEFORE UPDATE ON entry_templates
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_entry_templates_delete BEFORE DELETE ON entry_templates
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;

-- rules
CREATE TRIGGER archive_guard_rules_insert BEFORE INSERT ON rules
WHEN EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_rules_update BEFORE UPDATE ON rules
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_rules_delete BEFORE DELETE ON rules
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;

-- budgets
CREATE TRIGGER archive_guard_budgets_insert BEFORE INSERT ON budgets
WHEN EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_budgets_update BEFORE UPDATE ON budgets
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_budgets_delete BEFORE DELETE ON budgets
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;

-- documents
CREATE TRIGGER archive_guard_documents_insert BEFORE INSERT ON documents
WHEN EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_documents_update BEFORE UPDATE ON documents
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_documents_delete BEFORE DELETE ON documents
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;

-- tax_codes
CREATE TRIGGER archive_guard_tax_codes_insert BEFORE INSERT ON tax_codes
WHEN EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_tax_codes_update BEFORE UPDATE ON tax_codes
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_tax_codes_delete BEFORE DELETE ON tax_codes
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;

-- tax_events
CREATE TRIGGER archive_guard_tax_events_insert BEFORE INSERT ON tax_events
WHEN EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_tax_events_update BEFORE UPDATE ON tax_events
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_tax_events_delete BEFORE DELETE ON tax_events
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;

-- invoices
CREATE TRIGGER archive_guard_invoices_insert BEFORE INSERT ON invoices
WHEN EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_invoices_update BEFORE UPDATE ON invoices
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_invoices_delete BEFORE DELETE ON invoices
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;

-- filings
CREATE TRIGGER archive_guard_filings_insert BEFORE INSERT ON filings
WHEN EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_filings_update BEFORE UPDATE ON filings
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_filings_delete BEFORE DELETE ON filings
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;

-- party_relationships
CREATE TRIGGER archive_guard_party_relationships_insert BEFORE INSERT ON party_relationships
WHEN EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_party_relationships_update BEFORE UPDATE ON party_relationships
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_party_relationships_delete BEFORE DELETE ON party_relationships
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;

-- entity_classifications
CREATE TRIGGER archive_guard_entity_classifications_insert BEFORE INSERT ON entity_classifications
WHEN EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_entity_classifications_update BEFORE UPDATE ON entity_classifications
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_entity_classifications_delete BEFORE DELETE ON entity_classifications
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;

-- entity_flag_history
CREATE TRIGGER archive_guard_entity_flag_history_insert BEFORE INSERT ON entity_flag_history
WHEN EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_entity_flag_history_update BEFORE UPDATE ON entity_flag_history
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_entity_flag_history_delete BEFORE DELETE ON entity_flag_history
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;

-- entity_group_members
CREATE TRIGGER archive_guard_entity_group_members_insert BEFORE INSERT ON entity_group_members
WHEN EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_entity_group_members_update BEFORE UPDATE ON entity_group_members
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM entities WHERE id = NEW.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_entity_group_members_delete BEFORE DELETE ON entity_group_members
WHEN EXISTS (SELECT 1 FROM entities WHERE id = OLD.entity_id AND archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;

-- postings
CREATE TRIGGER archive_guard_postings_insert BEFORE INSERT ON postings
WHEN EXISTS (SELECT 1 FROM journal_entries x JOIN entities e ON e.id = x.entity_id WHERE x.id = NEW.journal_entry_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_postings_update BEFORE UPDATE ON postings
WHEN EXISTS (SELECT 1 FROM journal_entries x JOIN entities e ON e.id = x.entity_id WHERE x.id = OLD.journal_entry_id AND e.archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM journal_entries x JOIN entities e ON e.id = x.entity_id WHERE x.id = NEW.journal_entry_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_postings_delete BEFORE DELETE ON postings
WHEN EXISTS (SELECT 1 FROM journal_entries x JOIN entities e ON e.id = x.entity_id WHERE x.id = OLD.journal_entry_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;

-- entry_tags
CREATE TRIGGER archive_guard_entry_tags_insert BEFORE INSERT ON entry_tags
WHEN EXISTS (SELECT 1 FROM journal_entries x JOIN entities e ON e.id = x.entity_id WHERE x.id = NEW.entry_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_entry_tags_update BEFORE UPDATE ON entry_tags
WHEN EXISTS (SELECT 1 FROM journal_entries x JOIN entities e ON e.id = x.entity_id WHERE x.id = OLD.entry_id AND e.archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM journal_entries x JOIN entities e ON e.id = x.entity_id WHERE x.id = NEW.entry_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_entry_tags_delete BEFORE DELETE ON entry_tags
WHEN EXISTS (SELECT 1 FROM journal_entries x JOIN entities e ON e.id = x.entity_id WHERE x.id = OLD.entry_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;

-- intervault_links
CREATE TRIGGER archive_guard_intervault_links_insert BEFORE INSERT ON intervault_links
WHEN EXISTS (SELECT 1 FROM journal_entries x JOIN entities e ON e.id = x.entity_id WHERE x.id = NEW.entry_a_id AND e.archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM journal_entries x JOIN entities e ON e.id = x.entity_id WHERE x.id = NEW.entry_b_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_intervault_links_update BEFORE UPDATE ON intervault_links
WHEN EXISTS (SELECT 1 FROM journal_entries x JOIN entities e ON e.id = x.entity_id WHERE x.id = OLD.entry_a_id AND e.archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM journal_entries x JOIN entities e ON e.id = x.entity_id WHERE x.id = OLD.entry_b_id AND e.archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM journal_entries x JOIN entities e ON e.id = x.entity_id WHERE x.id = NEW.entry_a_id AND e.archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM journal_entries x JOIN entities e ON e.id = x.entity_id WHERE x.id = NEW.entry_b_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_intervault_links_delete BEFORE DELETE ON intervault_links
WHEN EXISTS (SELECT 1 FROM journal_entries x JOIN entities e ON e.id = x.entity_id WHERE x.id = OLD.entry_a_id AND e.archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM journal_entries x JOIN entities e ON e.id = x.entity_id WHERE x.id = OLD.entry_b_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;

-- imports
CREATE TRIGGER archive_guard_imports_insert BEFORE INSERT ON imports
WHEN EXISTS (SELECT 1 FROM accounts x JOIN entities e ON e.id = x.entity_id WHERE x.id = NEW.account_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_imports_update BEFORE UPDATE ON imports
WHEN EXISTS (SELECT 1 FROM accounts x JOIN entities e ON e.id = x.entity_id WHERE x.id = OLD.account_id AND e.archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM accounts x JOIN entities e ON e.id = x.entity_id WHERE x.id = NEW.account_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_imports_delete BEFORE DELETE ON imports
WHEN EXISTS (SELECT 1 FROM accounts x JOIN entities e ON e.id = x.entity_id WHERE x.id = OLD.account_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;

-- statement_lines
CREATE TRIGGER archive_guard_statement_lines_insert BEFORE INSERT ON statement_lines
WHEN EXISTS (SELECT 1 FROM accounts x JOIN entities e ON e.id = x.entity_id WHERE x.id = NEW.account_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_statement_lines_update BEFORE UPDATE ON statement_lines
WHEN EXISTS (SELECT 1 FROM accounts x JOIN entities e ON e.id = x.entity_id WHERE x.id = OLD.account_id AND e.archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM accounts x JOIN entities e ON e.id = x.entity_id WHERE x.id = NEW.account_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_statement_lines_delete BEFORE DELETE ON statement_lines
WHEN EXISTS (SELECT 1 FROM accounts x JOIN entities e ON e.id = x.entity_id WHERE x.id = OLD.account_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;

-- payee_aliases
CREATE TRIGGER archive_guard_payee_aliases_insert BEFORE INSERT ON payee_aliases
WHEN EXISTS (SELECT 1 FROM payees x JOIN entities e ON e.id = x.entity_id WHERE x.id = NEW.payee_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_payee_aliases_update BEFORE UPDATE ON payee_aliases
WHEN EXISTS (SELECT 1 FROM payees x JOIN entities e ON e.id = x.entity_id WHERE x.id = OLD.payee_id AND e.archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM payees x JOIN entities e ON e.id = x.entity_id WHERE x.id = NEW.payee_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_payee_aliases_delete BEFORE DELETE ON payee_aliases
WHEN EXISTS (SELECT 1 FROM payees x JOIN entities e ON e.id = x.entity_id WHERE x.id = OLD.payee_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;

-- document_links
CREATE TRIGGER archive_guard_document_links_insert BEFORE INSERT ON document_links
WHEN EXISTS (SELECT 1 FROM documents x JOIN entities e ON e.id = x.entity_id WHERE x.id = NEW.document_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_document_links_update BEFORE UPDATE ON document_links
WHEN EXISTS (SELECT 1 FROM documents x JOIN entities e ON e.id = x.entity_id WHERE x.id = OLD.document_id AND e.archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM documents x JOIN entities e ON e.id = x.entity_id WHERE x.id = NEW.document_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_document_links_delete BEFORE DELETE ON document_links
WHEN EXISTS (SELECT 1 FROM documents x JOIN entities e ON e.id = x.entity_id WHERE x.id = OLD.document_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;

-- invoice_lines
CREATE TRIGGER archive_guard_invoice_lines_insert BEFORE INSERT ON invoice_lines
WHEN EXISTS (SELECT 1 FROM invoices x JOIN entities e ON e.id = x.entity_id WHERE x.id = NEW.invoice_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_invoice_lines_update BEFORE UPDATE ON invoice_lines
WHEN EXISTS (SELECT 1 FROM invoices x JOIN entities e ON e.id = x.entity_id WHERE x.id = OLD.invoice_id AND e.archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM invoices x JOIN entities e ON e.id = x.entity_id WHERE x.id = NEW.invoice_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_invoice_lines_delete BEFORE DELETE ON invoice_lines
WHEN EXISTS (SELECT 1 FROM invoices x JOIN entities e ON e.id = x.entity_id WHERE x.id = OLD.invoice_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;

-- settlements
CREATE TRIGGER archive_guard_settlements_insert BEFORE INSERT ON settlements
WHEN EXISTS (SELECT 1 FROM invoices x JOIN entities e ON e.id = x.entity_id WHERE x.id = NEW.invoice_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_settlements_update BEFORE UPDATE ON settlements
WHEN EXISTS (SELECT 1 FROM invoices x JOIN entities e ON e.id = x.entity_id WHERE x.id = OLD.invoice_id AND e.archived_at IS NOT NULL)
    OR EXISTS (SELECT 1 FROM invoices x JOIN entities e ON e.id = x.entity_id WHERE x.id = NEW.invoice_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
CREATE TRIGGER archive_guard_settlements_delete BEFORE DELETE ON settlements
WHEN EXISTS (SELECT 1 FROM invoices x JOIN entities e ON e.id = x.entity_id WHERE x.id = OLD.invoice_id AND e.archived_at IS NOT NULL)
BEGIN SELECT RAISE(ABORT, 'entity_archived: the entity is archived; run means entity unarchive before you change its books'); END;
