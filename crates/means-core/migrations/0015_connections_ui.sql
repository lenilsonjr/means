-- UI preferences contain no provider credentials.
ALTER TABLE channel_connections ADD COLUMN booked_from TEXT NOT NULL DEFAULT '';
CREATE TABLE connection_jobs (
    id TEXT PRIMARY KEY,
    provider TEXT NOT NULL,
    operation TEXT NOT NULL,
    target TEXT NOT NULL,
    state TEXT NOT NULL,
    message TEXT NOT NULL DEFAULT '',
    started_at TEXT NOT NULL,
    finished_at TEXT NOT NULL DEFAULT ''
);
