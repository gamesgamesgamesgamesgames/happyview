-- The space host's record of the repos that have written to each space, with
-- each repo's latest reported state and the space revision that ordered it.
CREATE TABLE happyview_space_writers (
    space_id TEXT NOT NULL REFERENCES happyview_spaces(id) ON DELETE CASCADE,
    repo_did TEXT NOT NULL,
    rev TEXT NOT NULL,
    hash BYTEA NOT NULL,
    space_rev TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (space_id, repo_did)
);
CREATE INDEX idx_space_writers_space_rev ON happyview_space_writers(space_id, space_rev);

-- Repos committed before this table existed. Their repo revisions are TIDs, so
-- they stand in as space revisions and keep the order the writes happened in.
INSERT INTO happyview_space_writers (space_id, repo_did, rev, hash, space_rev, updated_at)
SELECT space_id, author_did, rev, hash, rev, updated_at
FROM happyview_space_repo_state
WHERE rev IS NOT NULL AND hash IS NOT NULL;
