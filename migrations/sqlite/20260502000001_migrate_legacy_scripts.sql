-- Migrate legacy index_hook and script columns into the trigger-keyed
-- scripts table, then drop the old columns.

-- 1. index_hook -> record.index:<lexicon_id>
INSERT INTO scripts (id, body, script_type, created_at, updated_at)
SELECT 'record.index:' || id, index_hook, 'lua', datetime('now'), datetime('now')
  FROM lexicons
 WHERE index_hook IS NOT NULL
ON CONFLICT (id) DO NOTHING;

-- 2. script -> xrpc.query:<id> or xrpc.procedure:<id>
INSERT INTO scripts (id, body, script_type, created_at, updated_at)
SELECT 'xrpc.' ||
       CASE json_extract(lexicon_json, '$.defs.main.type')
           WHEN 'query' THEN 'query'
           WHEN 'procedure' THEN 'procedure'
       END || ':' || id,
       script, 'lua', datetime('now'), datetime('now')
  FROM lexicons
 WHERE script IS NOT NULL
   AND json_extract(lexicon_json, '$.defs.main.type') IN ('query', 'procedure')
ON CONFLICT (id) DO NOTHING;

-- 3. Drop legacy columns.
ALTER TABLE lexicons DROP COLUMN index_hook;
ALTER TABLE lexicons DROP COLUMN script;
