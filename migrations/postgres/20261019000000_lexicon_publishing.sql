CREATE TABLE IF NOT EXISTS happyview_lexicon_publish_rules (
    id TEXT PRIMARY KEY,
    pattern TEXT NOT NULL UNIQUE,
    target_kind TEXT NOT NULL CHECK (target_kind IN ('instance', 'linked_repo')),
    linked_repo_id TEXT REFERENCES happyview_linked_repos(id) ON DELETE SET NULL,
    auto_publish BOOLEAN NOT NULL DEFAULT FALSE,
    created_by TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS happyview_lexicon_publications (
    nsid TEXT PRIMARY KEY,
    did TEXT,
    uri TEXT,
    cid TEXT,
    published_at TEXT,
    last_error TEXT,
    last_attempt_at TEXT
);
