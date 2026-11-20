-- Whether a space-as-member entry (is_delegation = 1) was added by someone
-- who administers the space it names, and which space that was. Such an entry
-- brings that space's members in only while it still names the same space.
-- Entries already stored count as added that way when the one who added them
-- is the named space's creator or a super admin, or when both spaces have the
-- same creator.
ALTER TABLE happyview_space_members ADD COLUMN delegation_consented INTEGER NOT NULL DEFAULT 0;
ALTER TABLE happyview_space_members ADD COLUMN delegation_target_space_id TEXT;

-- The space each stored entry names today: by id, or by its at:// (or legacy
-- ats://) address, which may carry a record path after the space. Prefixes
-- are compared exactly: LIKE would read _ and % in an address as wildcards.
UPDATE happyview_space_members
SET delegation_target_space_id = (
    SELECT t.id
    FROM happyview_spaces t
    WHERE t.id = happyview_space_members.member_did
       OR happyview_space_members.member_did = 'at://' || t.did || '/space/' || t.type_nsid || '/' || t.skey
       OR substr(happyview_space_members.member_did, 1, length('at://' || t.did || '/space/' || t.type_nsid || '/' || t.skey) + 1)
          = 'at://' || t.did || '/space/' || t.type_nsid || '/' || t.skey || '/'
       OR happyview_space_members.member_did = 'ats://' || t.did || '/' || t.type_nsid || '/' || t.skey
       OR substr(happyview_space_members.member_did, 1, length('ats://' || t.did || '/' || t.type_nsid || '/' || t.skey) + 1)
          = 'ats://' || t.did || '/' || t.type_nsid || '/' || t.skey || '/'
    LIMIT 1
)
WHERE is_delegation = 1;

UPDATE happyview_space_members
SET delegation_consented = 1
WHERE is_delegation = 1
  AND delegation_target_space_id IS NOT NULL
  AND EXISTS (
    SELECT 1
    FROM happyview_spaces p, happyview_spaces t
    WHERE p.id = happyview_space_members.space_id
      AND t.id = happyview_space_members.delegation_target_space_id
      AND (
           (happyview_space_members.granted_by IS NOT NULL
            AND happyview_space_members.granted_by = t.creator_did)
        OR p.creator_did = t.creator_did
        OR EXISTS (
          SELECT 1 FROM happyview_users u
          WHERE u.did = happyview_space_members.granted_by AND u.is_super <> 0
        )
      )
  );

UPDATE happyview_space_members
SET delegation_target_space_id = NULL
WHERE delegation_consented = 0;
