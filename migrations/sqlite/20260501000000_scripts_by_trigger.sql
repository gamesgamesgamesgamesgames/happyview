-- Trigger-keyed scripts subsystem.
--
-- Each row's `id` IS the trigger string the dispatcher resolves on:
--   record.index:<nsid>           — wildcard for any record event
--   record.create:<nsid>          — specifically a create event (cascades to wildcard)
--   record.update:<nsid>          — specifically an update event
--   record.delete:<nsid>          — specifically a delete event
--   xrpc.query:<nsid>             — XRPC query handler
--   xrpc.procedure:<nsid>         — XRPC procedure handler
--   labeler.apply:<nsid>          — label arrives whose subject is at://<did>/<nsid>/<rkey>
--   labeler.apply:_actor          — label arrives whose subject is a bare DID
--
-- See migrations/postgres/20260501000000_scripts_by_trigger.sql for design notes.
-- SQLite mirror.
CREATE TABLE scripts (
    id          TEXT PRIMARY KEY,
    body        TEXT NOT NULL,
    description TEXT,
    script_type TEXT NOT NULL DEFAULT 'lua',
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE dead_letter_scripts (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    script_ref  TEXT NOT NULL,                 -- = the trigger id whose script failed
    host_kind   TEXT NOT NULL,                 -- 'record' | 'label' (xrpc fails-closed; never dead-letters)
    host_id     TEXT NOT NULL,                 -- e.g. '<nsid>:<action>' for record, '<labeler-did>' for label
    payload     TEXT NOT NULL,                 -- JSON-serialized event for re-run
    error       TEXT NOT NULL,
    attempts    INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    resolved_at TEXT
);

CREATE INDEX idx_dead_letter_scripts_host
    ON dead_letter_scripts (host_kind, host_id, created_at DESC);
CREATE INDEX idx_dead_letter_scripts_resolved_at
    ON dead_letter_scripts (resolved_at);

-- Permissions: management for users who can manage lexicons; read for those who can read.
INSERT INTO user_permissions (user_id, permission, granted_at)
SELECT user_id, 'scripts:manage', datetime('now')
  FROM user_permissions
 WHERE permission = 'lexicons:create'
ON CONFLICT (user_id, permission) DO NOTHING;

INSERT INTO user_permissions (user_id, permission, granted_at)
SELECT user_id, 'scripts:read', datetime('now')
  FROM user_permissions
 WHERE permission = 'lexicons:read'
ON CONFLICT (user_id, permission) DO NOTHING;
