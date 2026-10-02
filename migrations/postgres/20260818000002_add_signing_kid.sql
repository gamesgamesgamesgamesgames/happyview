ALTER TABLE happyview_oauth_sessions ADD COLUMN signing_kid TEXT;
ALTER TABLE happyview_linked_repo_sessions ADD COLUMN signing_kid TEXT;
ALTER TABLE happyview_dpop_sessions ADD COLUMN signing_kid TEXT;

-- happyview_linked_repo_sessions is backfilled to the current instance key.
-- Safe only because that table is written exclusively by the linked-repos
-- OAuth client, which is built once at startup from the instance key
-- (`client_for_grant` / `state.linked_repos_client`) and offers no way to
-- select a different one — before this migration only one key can ever have
-- existed there: `ensure_instance_key` is idempotent and a partial unique
-- index permits one `current` row per owner (see
-- happyview_oauth_client_keys.idx_happyview_oauth_client_keys_one_current).
-- Every live linked-repo session was therefore necessarily established with
-- that key. This reasoning stops being true the moment key rotation exists,
-- which is why the backfill happens now rather than later.
UPDATE happyview_linked_repo_sessions
   SET signing_kid = (SELECT kid FROM happyview_oauth_client_keys WHERE owner = 'instance' AND status = 'current' LIMIT 1);

-- happyview_oauth_sessions is deliberately left NULL, unlike the table
-- above. That table is not instance-only: `/auth/login` accepts a
-- caller-supplied `client_id` and resolves it via `get_or_default` to *any*
-- registered API client, not just the primary one, so a session there may
-- have been established with a confidential API client's own key
-- (`owner = <api client id>`) rather than the instance key. Nothing in the
-- row says which — stamping every row with the instance key would silently
-- mis-pin the ones signed by another client's key, and Stage 3 (key
-- rotation) turns a mis-pin into a permanently destroyed session on next
-- refresh. NULL is correct for both populations: the reader that consults
-- this column falls back to "the current key for this session's owner",
-- which reproduces the pre-rotation behavior for an instance-signed session
-- and looks up the right client's key for an API-client-signed one.
--
-- happyview_dpop_sessions is likewise left NULL: those rows belong to
-- third-party API clients, most of which never had a client authentication
-- key at all. NULL means "public, or established before pinning existed",
-- which is treated as "use the current key" by the code that reads it.
