CREATE TABLE IF NOT EXISTS service_entries (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    fragment_id  TEXT UNIQUE NOT NULL,
    service_type TEXT NOT NULL,
    access_mode  TEXT NOT NULL DEFAULT 'all',
    created_at   TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at   TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS service_entry_xrpcs (
    service_entry_id INTEGER NOT NULL REFERENCES service_entries(id) ON DELETE CASCADE,
    lexicon_id       TEXT NOT NULL,
    PRIMARY KEY (service_entry_id, lexicon_id)
);
