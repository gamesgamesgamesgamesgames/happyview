-- A rotated key is published as `pending` before it signs anything, so an
-- authorization server that cached the old key set has seen the new key by
-- the time it is used. `activates_at` is when it takes over as `current`.
ALTER TABLE happyview_oauth_client_keys ADD COLUMN activates_at TEXT;

-- At most one key waiting to take over per owner.
CREATE UNIQUE INDEX idx_happyview_oauth_client_keys_one_pending
    ON happyview_oauth_client_keys (owner) WHERE status = 'pending';

-- The key an authorization was started with, so a promotion while the user
-- is at their PDS finishes it with the same key: the reference provider
-- refuses a code exchange signed by a different key than the request was.
ALTER TABLE happyview_auth_login_redirects ADD COLUMN kid TEXT;
ALTER TABLE happyview_linked_repo_auth_state ADD COLUMN kid TEXT;
-- The key HappyView signed a provision's client assertions with, for the
-- same reason, and so the session it becomes is pinned to that key.
ALTER TABLE happyview_dpop_keys ADD COLUMN assertion_kid TEXT;
