-- Migrate legacy index_hook and script columns into the trigger-keyed
-- scripts table, then drop the old columns.

-- 1. index_hook -> record.index:<lexicon_id>
INSERT INTO scripts (id, body, script_type, created_at, updated_at)
SELECT 'record.index:' || id, index_hook, 'lua', NOW(), NOW()
  FROM lexicons
 WHERE index_hook IS NOT NULL
ON CONFLICT (id) DO NOTHING;

-- 2. script -> xrpc.query:<id> or xrpc.procedure:<id>
INSERT INTO scripts (id, body, script_type, created_at, updated_at)
SELECT 'xrpc.' ||
       CASE (lexicon_json::jsonb)->'defs'->'main'->>'type'
           WHEN 'query' THEN 'query'
           WHEN 'procedure' THEN 'procedure'
       END || ':' || id,
       script, 'lua', NOW(), NOW()
  FROM lexicons
 WHERE script IS NOT NULL
   AND (lexicon_json::jsonb)->'defs'->'main'->>'type' IN ('query', 'procedure')
ON CONFLICT (id) DO NOTHING;

-- 3. Drop legacy columns.
ALTER TABLE lexicons DROP COLUMN index_hook;
ALTER TABLE lexicons DROP COLUMN script;
