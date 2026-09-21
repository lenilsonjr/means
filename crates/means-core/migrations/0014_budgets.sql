-- Plans over booked expenses; creating a budget never creates postings.
CREATE TABLE budgets (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    uid TEXT NOT NULL UNIQUE,
    entity_id INTEGER NOT NULL REFERENCES entities(id),
    name TEXT NOT NULL CHECK (length(trim(name)) > 0),
    scope TEXT NOT NULL CHECK (scope IN ('category', 'class', 'tag')),
    account_id INTEGER REFERENCES accounts(id),
    class TEXT,
    tag TEXT,
    starts_on TEXT NOT NULL,
    ends_on TEXT NOT NULL CHECK (ends_on >= starts_on),
    amount INTEGER NOT NULL CHECK (amount >= 0),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK ((scope = 'category' AND account_id IS NOT NULL AND class IS NULL)
        OR (scope = 'class' AND account_id IS NULL AND class IS NOT NULL)
        OR (scope = 'tag' AND account_id IS NULL AND class IS NULL AND tag IS NOT NULL))
);
CREATE INDEX budgets_entity_dates ON budgets(entity_id, starts_on, ends_on);
