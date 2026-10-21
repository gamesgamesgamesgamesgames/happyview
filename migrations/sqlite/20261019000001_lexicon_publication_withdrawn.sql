-- When an operator explicitly unpublished a lexicon. Auto-publish skips a
-- withdrawn lexicon, so a reconcile does not put back a record someone chose
-- to delete; publishing it by hand clears the mark. NULL for every lexicon
-- that was never unpublished.
ALTER TABLE happyview_lexicon_publications ADD COLUMN withdrawn_at TEXT;
