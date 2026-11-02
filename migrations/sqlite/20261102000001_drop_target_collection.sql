-- Scripts name the collection they read or write, so a query or procedure
-- lexicon no longer carries one, and with no script-less procedure left to
-- choose create, update or delete, `action` goes with it.
--
-- The v2 codemod still needs each lexicon's collection to write it into the
-- script that used `collection` or `ctx.collection`, and it runs after this
-- migration, so the pairs are kept here for it alone.
CREATE TABLE happyview_codemod_collections (
    lexicon_id TEXT PRIMARY KEY,
    collection TEXT NOT NULL
);

INSERT INTO happyview_codemod_collections (lexicon_id, collection)
SELECT id, target_collection FROM happyview_lexicons
WHERE target_collection IS NOT NULL AND target_collection <> '';

ALTER TABLE happyview_lexicons DROP COLUMN target_collection;
ALTER TABLE happyview_lexicons DROP COLUMN action;
