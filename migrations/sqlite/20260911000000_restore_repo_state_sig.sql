-- Add the commit signature column that 20260721000000_drop_repo_state_sig
-- removed. Proposal 0016 commits carry `sig`; src/spaces/commit.rs describes
-- what it covers and why `mac` alone does not authenticate a commit.
--
-- Existing rows are left NULL. src/spaces/rebuild.rs re-mints their commits
-- from the records.
ALTER TABLE happyview_space_repo_state ADD COLUMN sig BLOB;
