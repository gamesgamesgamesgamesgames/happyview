-- Give invites the same independent read and write booleans the member list
-- already has, rather than the single access word.
--
-- `20260912000001_space_member_booleans.sql` split `happyview_space_members`
-- and left this table behind, so an invite still round-tripped through
-- `MemberAccess::as_wire_str`, which collapses any `write` to the word
-- "write", and back through `parse_wire`, which reads that word as read *and*
-- write. A write-only invite was therefore stored as read/write: the create
-- response reported what the caller asked for, because it returns the struct
-- it built, while every later read of the row — the invite list, and
-- redemption itself — granted read access the caller had withheld.
--
-- read_self comes across for the same reason it exists on the member list: it
-- is HappyView-local and never on the wire, but a script may mint an invite
-- with it through the Lua access word, and folding it into can_read would
-- promote the joiner from own-records-only to whole-space reads.
--
-- INTEGER rather than BOOLEAN on both backends, matching the member columns
-- and `revoked` on this same table.
ALTER TABLE happyview_space_invites ADD COLUMN can_read INTEGER NOT NULL DEFAULT 1;
ALTER TABLE happyview_space_invites ADD COLUMN can_write INTEGER NOT NULL DEFAULT 0;
ALTER TABLE happyview_space_invites ADD COLUMN read_self INTEGER NOT NULL DEFAULT 0;

-- Exactly what `parse_wire` would have made of each stored word, so no
-- outstanding invite changes meaning as it crosses this migration. Only
-- "none" withholds read, and an unrecognised word kept the column NOT NULL
-- and so cannot be present.
UPDATE happyview_space_invites SET
    can_read  = CASE WHEN access = 'none' THEN 0 ELSE 1 END,
    can_write = CASE WHEN access = 'write' THEN 1 ELSE 0 END,
    read_self = CASE WHEN access = 'read_self' THEN 1 ELSE 0 END;

ALTER TABLE happyview_space_invites DROP COLUMN access;
