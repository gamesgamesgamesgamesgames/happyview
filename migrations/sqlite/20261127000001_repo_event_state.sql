-- What HappyView last heard of an indexed account's identity and status,
-- so identity:* and account:* scripts are told the values before an event.
-- INTEGER rather than BOOLEAN for `active`, on both backends.
CREATE TABLE IF NOT EXISTS happyview_repo_event_state (
    did TEXT PRIMARY KEY,
    handle TEXT,
    active INTEGER,
    status TEXT,
    updated_at TEXT NOT NULL
);
