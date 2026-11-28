-- Whether a call to this query or procedure lexicon pays its token cost.
-- Every existing lexicon stays rate limited; an operator turns it off per id.
ALTER TABLE happyview_lexicons ADD COLUMN rate_limited INTEGER NOT NULL DEFAULT 1;
