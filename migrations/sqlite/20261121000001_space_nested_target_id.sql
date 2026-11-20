-- The id of the space a nested reference names, as it was when the
-- reference was made. A reference brings in members only while its address
-- still names that space, so a later space at the same address doesn't take
-- its place. References whose address names no space here keep NULL.
ALTER TABLE happyview_space_nested ADD COLUMN target_space_id TEXT;

UPDATE happyview_space_nested
SET target_space_id = (
    SELECT s.id FROM happyview_spaces s
    WHERE 'at://' || s.did || '/space/' || s.type_nsid || '/' || s.skey = happyview_space_nested.target_space_uri
);

CREATE INDEX IF NOT EXISTS idx_happyview_space_nested_target_id
    ON happyview_space_nested(target_space_id);
