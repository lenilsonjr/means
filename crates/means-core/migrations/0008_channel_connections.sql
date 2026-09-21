-- One linked account at a pull channel: Pluggy today, Enable Banking next. A row says which
-- provider account belongs to which means account, where the pull reached (`cursor`, the value
-- the next fetch sends as its "created after" filter), and when it last ran.
-- No credential is stored here: they are read from the environment, so the ledger file carries none.
CREATE TABLE channel_connections (
  id                  INTEGER PRIMARY KEY,
  channel             TEXT NOT NULL,
  item_id             TEXT NOT NULL,
  provider_account_id TEXT NOT NULL,
  provider_type       TEXT NOT NULL DEFAULT '',
  name                TEXT NOT NULL DEFAULT '',
  currency            TEXT NOT NULL DEFAULT '',
  account_id          INTEGER REFERENCES accounts(id),
  cursor              TEXT NOT NULL DEFAULT '',
  last_pull_at        TEXT NOT NULL DEFAULT '',
  created_at          TEXT NOT NULL,
  UNIQUE (channel, provider_account_id)
);
