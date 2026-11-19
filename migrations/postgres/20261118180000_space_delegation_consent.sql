-- Whether a space-as-member entry (is_delegation = 1) was added by someone
-- who administers the space it names. Only such entries bring that space's
-- members in. Entries already stored count as added that way when the one
-- who added them is the named space's creator or a super admin, or when both
-- spaces have the same creator.
ALTER TABLE happyview_space_members ADD COLUMN delegation_consented INTEGER NOT NULL DEFAULT 0;

UPDATE happyview_space_members
SET delegation_consented = 1
WHERE is_delegation = 1
  AND EXISTS (
    SELECT 1
    FROM happyview_spaces p, happyview_spaces t
    WHERE p.id = happyview_space_members.space_id
      AND (
           t.id = happyview_space_members.member_did
        OR happyview_space_members.member_did = 'at://' || t.did || '/space/' || t.type_nsid || '/' || t.skey
        OR happyview_space_members.member_did LIKE 'at://' || t.did || '/space/' || t.type_nsid || '/' || t.skey || '/%'
        OR happyview_space_members.member_did = 'ats://' || t.did || '/' || t.type_nsid || '/' || t.skey
        OR happyview_space_members.member_did LIKE 'ats://' || t.did || '/' || t.type_nsid || '/' || t.skey || '/%'
      )
      AND (
           (happyview_space_members.granted_by IS NOT NULL AND t.creator_did IS NOT NULL
            AND happyview_space_members.granted_by = t.creator_did)
        OR (p.creator_did IS NOT NULL AND t.creator_did IS NOT NULL AND p.creator_did = t.creator_did)
        OR EXISTS (
          SELECT 1 FROM happyview_users u
          WHERE u.did = happyview_space_members.granted_by AND u.is_super <> 0
        )
      )
  );
