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

-- The last event each script family ran for, per account, so an event
-- applied again (after a failed write) doesn't run its scripts twice.
CREATE TABLE IF NOT EXISTS happyview_repo_event_fired (
    did TEXT NOT NULL,
    family TEXT NOT NULL,
    event_key TEXT NOT NULL,
    fired_at TEXT NOT NULL,
    PRIMARY KEY (did, family)
);
