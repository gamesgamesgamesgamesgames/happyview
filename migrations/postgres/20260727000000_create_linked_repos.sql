CREATE TABLE happyview_linked_repos (
    id                TEXT PRIMARY KEY,
    did               TEXT UNIQUE,
    handle            TEXT,
    label             TEXT,
    scopes            TEXT NOT NULL,
    status            TEXT NOT NULL DEFAULT 'pending',
    last_error        TEXT,
    last_refreshed_at TEXT,
    authorized_at     TEXT,
    created_by        TEXT NOT NULL,
    created_at        TEXT NOT NULL
);

CREATE INDEX idx_happyview_linked_repos_status ON happyview_linked_repos (status);

CREATE TABLE happyview_linked_repo_sessions (
    did          TEXT PRIMARY KEY,
    session_data TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);

CREATE TABLE happyview_linked_repo_auth_state (
    state      TEXT PRIMARY KEY,
    grant_id   TEXT NOT NULL,
    token_hash TEXT,
    expires_at TEXT NOT NULL
);

CREATE INDEX idx_happyview_linked_repo_auth_state_grant
    ON happyview_linked_repo_auth_state (grant_id);
