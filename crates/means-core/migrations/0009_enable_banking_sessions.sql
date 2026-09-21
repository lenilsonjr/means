-- Consent identifiers require the application's external RSA key to access data.
-- Private keys, JWTs and one-time authorization codes never enter the ledger.
CREATE TABLE enable_banking_sessions (
  id TEXT PRIMARY KEY,
  bank TEXT NOT NULL,
  country TEXT NOT NULL,
  valid_until TEXT NOT NULL,
  created_at TEXT NOT NULL
);
