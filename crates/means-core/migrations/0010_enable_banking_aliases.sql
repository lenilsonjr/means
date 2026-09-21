-- Bank account UIDs change on reauthorization. Preserve routes using every stable
-- identification hash returned for an account, separately for each currency.
CREATE TABLE enable_banking_account_aliases (
  hash TEXT NOT NULL,
  currency TEXT NOT NULL,
  connection_id INTEGER NOT NULL REFERENCES channel_connections(id) ON DELETE CASCADE,
  PRIMARY KEY (hash, currency)
);
