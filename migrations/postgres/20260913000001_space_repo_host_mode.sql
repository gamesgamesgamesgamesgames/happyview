-- Where a permissioned repo lives.
--
--   polyfill  HappyView is the repo host and the source of truth.
--   migrating records are being replayed into the user's PDS; HappyView is
--             still authoritative until the handoff verifies.
--   native    the user's PDS is the source of truth; HappyView indexes it.
--
-- Per (space, author) rather than per user; see src/spaces/host_mode.rs.
ALTER TABLE happyview_space_repo_state ADD COLUMN host_mode TEXT NOT NULL DEFAULT 'polyfill';

-- Last rev consumed from the PDS in native mode, for incremental listRepoOps.
ALTER TABLE happyview_space_repo_state ADD COLUMN sync_cursor TEXT;
