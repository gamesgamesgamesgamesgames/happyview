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
-- Cascade rule (record events ONLY): the dispatcher tries
-- `record.<action>:<nsid>` first, falls back to `record.index:<nsid>` if no
-- specific row exists. No cascade for XRPC or labeler triggers — those
-- resolve directly. Operators express per-action surgical control by
-- creating action-specific rows; the wildcard `record.index:<nsid>` covers
-- "one body for everything" with branching on `event.action`.
CREATE TABLE scripts (
    id          TEXT PRIMARY KEY,
    body        TEXT NOT NULL,
    description TEXT,
    script_type TEXT NOT NULL DEFAULT 'lua',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Permanently-failed runs from the firehose-driven runners (record / label
-- events). XRPC scripts fail-closed and never land here.
CREATE TABLE dead_letter_scripts (
    id          BIGSERIAL PRIMARY KEY,
    script_ref  TEXT NOT NULL,                 -- = the trigger id whose script failed
    host_kind   TEXT NOT NULL,                 -- 'record' | 'label'
    host_id     TEXT NOT NULL,                 -- e.g. '<nsid>:<action>' for record, '<labeler-did>' for label
    payload     JSONB NOT NULL,                -- event payload for re-run
    error       TEXT NOT NULL,
    attempts    INT NOT NULL DEFAULT 0,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    resolved_at TIMESTAMPTZ
);

CREATE INDEX idx_dead_letter_scripts_host
    ON dead_letter_scripts (host_kind, host_id, created_at DESC);
CREATE INDEX idx_dead_letter_scripts_resolved_at
    ON dead_letter_scripts (resolved_at);

-- Permissions: management for users who can manage lexicons; read for those who can read.
INSERT INTO user_permissions (user_id, permission)
SELECT user_id, 'scripts:manage'
  FROM user_permissions
 WHERE permission = 'lexicons:create'
ON CONFLICT (user_id, permission) DO NOTHING;

INSERT INTO user_permissions (user_id, permission)
SELECT user_id, 'scripts:read'
  FROM user_permissions
 WHERE permission = 'lexicons:read'
ON CONFLICT (user_id, permission) DO NOTHING;
