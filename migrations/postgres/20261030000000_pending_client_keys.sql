-- A rotated key is published as `pending` before it signs anything, so an
-- authorization server that cached the old key set has seen the new key by
-- the time it is used. `activates_at` is when it takes over as `current`.
ALTER TABLE happyview_oauth_client_keys ADD COLUMN activates_at TEXT;

-- At most one key waiting to take over per owner.
CREATE UNIQUE INDEX idx_happyview_oauth_client_keys_one_pending
    ON happyview_oauth_client_keys (owner) WHERE status = 'pending';
