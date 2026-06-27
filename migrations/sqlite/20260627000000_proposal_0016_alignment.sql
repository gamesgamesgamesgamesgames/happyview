-- Proposal 0016: Permissioned Data alignment

-- 1+2. Rebuild happyview_spaces with renamed/new columns
CREATE TABLE happyview_spaces_new (
    id TEXT PRIMARY KEY,
    did TEXT NOT NULL,
    authority_did TEXT NOT NULL,
    creator_did TEXT NOT NULL,
    type_nsid TEXT NOT NULL,
    skey TEXT NOT NULL,
    display_name TEXT,
    description TEXT,
    mint_policy TEXT NOT NULL DEFAULT 'member-list',
    app_access TEXT NOT NULL DEFAULT '{"type":"open"}',
    managing_app_did TEXT,
    config TEXT NOT NULL DEFAULT '{}',
    revision TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (did, type_nsid, skey)
);

INSERT INTO happyview_spaces_new (id, did, authority_did, creator_did, type_nsid, skey, display_name, description, mint_policy, app_access, managing_app_did, config, revision, created_at, updated_at)
SELECT id, did, owner_did, owner_did, type_nsid, skey, display_name, description,
    'member-list',
    CASE
        WHEN access_mode = 'default_deny' AND app_allowlist IS NOT NULL
        THEN '{"type":"allowList","allowed":' || app_allowlist || '}'
        ELSE '{"type":"open"}'
    END,
    managing_app_did, config, revision, created_at, updated_at
FROM happyview_spaces;

DROP TABLE happyview_spaces;
ALTER TABLE happyview_spaces_new RENAME TO happyview_spaces;

-- 3. Per-user repo state
CREATE TABLE happyview_space_repo_state (
    id TEXT PRIMARY KEY,
    space_id TEXT NOT NULL REFERENCES happyview_spaces(id) ON DELETE CASCADE,
    author_did TEXT NOT NULL,
    lthash_state BLOB NOT NULL DEFAULT (zeroblob(2048)),
    rev TEXT,
    hash BLOB,
    ikm BLOB,
    sig BLOB,
    mac BLOB,
    updated_at TEXT NOT NULL,
    UNIQUE (space_id, author_did)
);
CREATE INDEX idx_space_repo_state_space ON happyview_space_repo_state(space_id);

-- 4. Record operation log
CREATE TABLE happyview_space_record_oplog (
    id TEXT PRIMARY KEY,
    space_id TEXT NOT NULL REFERENCES happyview_spaces(id) ON DELETE CASCADE,
    author_did TEXT NOT NULL,
    rev TEXT NOT NULL,
    idx INTEGER NOT NULL DEFAULT 0,
    action TEXT NOT NULL CHECK (action IN ('create', 'update', 'delete')),
    collection TEXT NOT NULL,
    rkey TEXT NOT NULL,
    cid TEXT,
    prev TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_space_oplog_space_author ON happyview_space_record_oplog(space_id, author_did);
CREATE INDEX idx_space_oplog_rev ON happyview_space_record_oplog(space_id, author_did, rev);

-- 5. Write notification registrations
CREATE TABLE happyview_space_notify_registrations (
    id TEXT PRIMARY KEY,
    space_id TEXT NOT NULL REFERENCES happyview_spaces(id) ON DELETE CASCADE,
    author_did TEXT,
    endpoint TEXT NOT NULL,
    registered_by TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_space_notify_space ON happyview_space_notify_registrations(space_id);
CREATE INDEX idx_space_notify_repo ON happyview_space_notify_registrations(space_id, author_did);

-- 6. Drop old sync state table
DROP TABLE IF EXISTS happyview_space_sync_state;
