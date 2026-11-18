-- Limit the #atproto_space_host service entry HappyView creates with a space
-- to the methods other hosts call on a space host. Only an entry still on
-- 'all' changes; an entry already on 'specific', and every other entry, stay
-- as they are.
INSERT INTO happyview_service_entry_xrpcs (service_entry_id, lexicon_id)
SELECT e.id, m.lexicon_id
FROM happyview_service_entries e
CROSS JOIN (
    SELECT 'com.atproto.space.notifyWrite' AS lexicon_id
    UNION ALL SELECT 'com.atproto.space.notifySpaceDeleted'
    UNION ALL SELECT 'com.atproto.space.notifyCredentialRevoked'
) m
WHERE e.fragment_id = '#atproto_space_host'
  AND e.service_type = 'AtprotoSpaceHost'
  AND e.access_mode = 'all'
ON CONFLICT DO NOTHING;

UPDATE happyview_service_entries
SET access_mode = 'specific', updated_at = to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"+00:00"')
WHERE fragment_id = '#atproto_space_host'
  AND service_type = 'AtprotoSpaceHost'
  AND access_mode = 'all';
