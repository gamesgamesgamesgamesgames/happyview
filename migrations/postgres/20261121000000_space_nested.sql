-- Spaces nested in a memberspace: every member of the nested space is a
-- member of this one, with at most the read and write access the reference
-- grants. Resolved when access is checked, never copied into
-- happyview_space_members. target_space_uri is the nested space's
-- at://<did>/space/<type>/<skey> address. The index serves the reverse
-- lookup: which spaces reach a space that changed.
CREATE TABLE IF NOT EXISTS happyview_space_nested (
    id TEXT PRIMARY KEY,
    space_id TEXT NOT NULL REFERENCES happyview_spaces(id) ON DELETE CASCADE,
    target_space_uri TEXT NOT NULL,
    can_read INTEGER NOT NULL DEFAULT 1,
    can_write INTEGER NOT NULL DEFAULT 0,
    added_by TEXT,
    created_at TEXT NOT NULL,
    UNIQUE(space_id, target_space_uri)
);

CREATE INDEX IF NOT EXISTS idx_happyview_space_nested_target
    ON happyview_space_nested(target_space_uri);
