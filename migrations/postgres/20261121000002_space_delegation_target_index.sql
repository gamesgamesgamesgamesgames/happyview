-- Finds the space-as-member entries that bring in a given space's members,
-- which a change to that space's members looks up.
CREATE INDEX IF NOT EXISTS idx_happyview_space_members_delegation_target
    ON happyview_space_members(delegation_target_space_id)
    WHERE delegation_target_space_id IS NOT NULL;
