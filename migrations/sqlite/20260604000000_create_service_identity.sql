CREATE TABLE IF NOT EXISTS service_identity (
    id                   INTEGER PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    mode                 TEXT NOT NULL,
    did                  TEXT,
    signing_key_enc      TEXT,
    rotation_key_enc     TEXT,
    attached_account_did TEXT,
    setup_complete       BOOLEAN NOT NULL DEFAULT FALSE,
    created_at           TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at           TEXT NOT NULL DEFAULT (datetime('now'))
);
