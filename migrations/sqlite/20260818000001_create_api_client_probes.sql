CREATE TABLE happyview_api_client_probes (
    api_client_id TEXT PRIMARY KEY REFERENCES happyview_api_clients(id) ON DELETE CASCADE,
    confidential  INTEGER NOT NULL DEFAULT 0,
    reason        TEXT NOT NULL,
    checked_at    TEXT NOT NULL
);
