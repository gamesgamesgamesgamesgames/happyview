-- Split mint_policy into the required readPolicy/writePolicy pair (atproto
-- spaces alpha, 2026-09-10) and move managingApp inside the policy value.
--
-- Read and write are independent permissions: read gates whether a space
-- credential is minted, write gates whether the authority tracks the writer in
-- listRepos and forwards their notifyWrite. mint_policy governed both, so both
-- new columns take its value, which keeps each space's existing behaviour.
ALTER TABLE happyview_spaces ADD COLUMN read_policy TEXT;
ALTER TABLE happyview_spaces ADD COLUMN write_policy TEXT;

UPDATE happyview_spaces
SET read_policy = CASE mint_policy
        WHEN 'public' THEN '{"$type":"com.atproto.simplespace.defs#publicPolicy"}'
        WHEN 'managing-app' THEN
            '{"$type":"com.atproto.simplespace.defs#managingAppPolicy","managingApp":"'
            || COALESCE(managing_app_did, '') || '"}'
        ELSE '{"$type":"com.atproto.simplespace.defs#memberListPolicy"}'
    END,
    write_policy = CASE mint_policy
        WHEN 'public' THEN '{"$type":"com.atproto.simplespace.defs#publicPolicy"}'
        WHEN 'managing-app' THEN
            '{"$type":"com.atproto.simplespace.defs#managingAppPolicy","managingApp":"'
            || COALESCE(managing_app_did, '') || '"}'
        ELSE '{"$type":"com.atproto.simplespace.defs#memberListPolicy"}'
    END;

-- Retag app_access from the old {"type":"open"} form to the lexicon's $type.
UPDATE happyview_spaces
SET app_access = REPLACE(
        REPLACE(app_access, '"type":"open"', '"$type":"com.atproto.simplespace.defs#open"'),
        '"type":"allowList"', '"$type":"com.atproto.simplespace.defs#allowList"');

ALTER TABLE happyview_spaces DROP COLUMN mint_policy;
ALTER TABLE happyview_spaces DROP COLUMN managing_app_did;
