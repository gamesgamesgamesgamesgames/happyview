-- The ticket key a DPoP sign-in pinned when its session was registered: the
-- RFC 7638 thumbprint of a key the caller proved they hold. Caller assertions
-- for that sign-in name only this key. NULL: nothing pinned, so no assertion
-- names a proven key. The row, and the pin with it, goes when the session
-- ends.
ALTER TABLE happyview_dpop_keys ADD COLUMN ticket_jkt TEXT;
