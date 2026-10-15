-- The party at the other side of an account, for example the shareholder of an
-- "Amount due to related parties" account. A payee of the entity of the account, or NULL.
-- The close check uses it to tell a related party outside the group from an entity of the group.
ALTER TABLE accounts ADD COLUMN counterparty_payee_id INTEGER REFERENCES payees(id);

CREATE TRIGGER account_counterparty_entity_insert BEFORE INSERT ON accounts
WHEN NEW.counterparty_payee_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM payees WHERE id = NEW.counterparty_payee_id AND entity_id = NEW.entity_id)
BEGIN SELECT RAISE(ABORT, 'the counterparty payee belongs to another entity'); END;
CREATE TRIGGER account_counterparty_entity_update BEFORE UPDATE OF counterparty_payee_id, entity_id ON accounts
WHEN NEW.counterparty_payee_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM payees WHERE id = NEW.counterparty_payee_id AND entity_id = NEW.entity_id)
BEGIN SELECT RAISE(ABORT, 'the counterparty payee belongs to another entity'); END;
