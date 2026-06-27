-- Proposal 0016: Permissioned Data alignment
-- Restructures spaces for the formal AT Protocol permissioned data spec.

-- 1. Rename owner_did → authority_did, add creator_did
ALTER TABLE happyview_spaces RENAME COLUMN owner_did TO authority_did;
ALTER TABLE happyview_spaces ADD COLUMN creator_did TEXT;
UPDATE happyview_spaces SET creator_did = authority_did WHERE creator_did IS NULL;
ALTER TABLE happyview_spaces ALTER COLUMN creator_did SET NOT NULL;

-- 2. Replace access_mode + allowlist/denylist with mint_policy + app_access
-- mint_policy: 'member-list' (default) | 'public' | 'managing-app'
ALTER TABLE happyview_spaces ADD COLUMN mint_policy TEXT NOT NULL DEFAULT 'member-list';
-- app_access: JSON open union, e.g. {"type": "open"} or {"type": "allowList", "allowed": [...]}
ALTER TABLE happyview_spaces ADD COLUMN app_access TEXT NOT NULL DEFAULT '{"type":"open"}';
-- Migrate existing data
UPDATE happyview_spaces
SET app_access = '{"type":"allowList","allowed":' || COALESCE(app_allowlist, '[]') || '}'
WHERE access_mode = 'default_deny' AND app_allowlist IS NOT NULL;
-- Drop old columns
ALTER TABLE happyview_spaces DROP COLUMN access_mode;
ALTER TABLE happyview_spaces DROP COLUMN app_allowlist;
ALTER TABLE happyview_spaces DROP COLUMN app_denylist;

-- 3. Per-user repo state: LtHash state + signed commit
CREATE TABLE happyview_space_repo_state (
    id TEXT PRIMARY KEY,
    space_id TEXT NOT NULL REFERENCES happyview_spaces(id) ON DELETE CASCADE,
    author_did TEXT NOT NULL,
    lthash_state BYTEA NOT NULL DEFAULT decode(repeat('00', 2048), 'hex'),
    rev TEXT,
    hash BYTEA,
    ikm BYTEA,
    sig BYTEA,
    mac BYTEA,
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
