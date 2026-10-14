-- When an operator explicitly unpublished a lexicon. Auto-publish skips a
-- withdrawn lexicon, so a reconcile does not put back a record someone chose
-- to delete; publishing it by hand clears the mark. NULL for every lexicon
-- that was never unpublished. Adding a nullable column without a default
-- rewrites nothing.
ALTER TABLE happyview_lexicon_publications ADD COLUMN IF NOT EXISTS withdrawn_at TEXT;
