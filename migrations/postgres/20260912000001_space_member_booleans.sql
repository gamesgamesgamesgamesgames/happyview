-- Replace the ordinal access column with independent read and write booleans
-- (atproto spaces alpha, 2026-09-10).
--
-- read_self is kept as a HappyView-local column. The spec's member list has no
-- equivalent (there, read_self is an OAuth action, not a membership level), so
-- folding it into can_read would promote those members from own-records-only to
-- whole-space reads. It is enforced on the read path and never appears on the
-- wire.
--
-- INTEGER rather than BOOLEAN on both backends, matching is_delegation on this
-- same table.
ALTER TABLE happyview_space_members ADD COLUMN can_read INTEGER NOT NULL DEFAULT 1;
ALTER TABLE happyview_space_members ADD COLUMN can_write INTEGER NOT NULL DEFAULT 0;
ALTER TABLE happyview_space_members ADD COLUMN read_self INTEGER NOT NULL DEFAULT 0;

UPDATE happyview_space_members SET
    can_read  = 1,
    can_write = CASE WHEN access = 'write' THEN 1 ELSE 0 END,
    read_self = CASE WHEN access = 'read_self' THEN 1 ELSE 0 END;

ALTER TABLE happyview_space_members DROP COLUMN access;
