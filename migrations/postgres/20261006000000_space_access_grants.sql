-- Time-limited access a moderator requested to read one space, or one
-- account's records across spaces. Rows are kept after they expire or are
-- revoked; they are the record of who could read what, and why.
CREATE TABLE happyview_space_access_grants (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    user_did TEXT NOT NULL,
    scope TEXT NOT NULL,
    target TEXT NOT NULL,
    reason TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    revoked_at TEXT,
    revoked_by TEXT
);
CREATE INDEX idx_space_access_grants_user ON happyview_space_access_grants(user_id, expires_at);
