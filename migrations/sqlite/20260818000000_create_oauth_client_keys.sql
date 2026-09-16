CREATE TABLE happyview_oauth_client_keys (
    kid         TEXT PRIMARY KEY,
    owner       TEXT NOT NULL,
    alg         TEXT NOT NULL DEFAULT 'ES256',
    private_jwk BLOB NOT NULL,
    public_jwk  TEXT NOT NULL,
    encrypted   INTEGER NOT NULL DEFAULT 0,
    status      TEXT NOT NULL DEFAULT 'current',
    created_at  TEXT NOT NULL,
    retired_at  TEXT
);

CREATE INDEX idx_happyview_oauth_client_keys_owner ON happyview_oauth_client_keys (owner, status);

-- At most one `current` key per owner. Without this, two processes booting
-- concurrently can both see zero rows via `ensure_instance_key` and both
-- insert, leaving atrium's `Keyset::find_key` (which selects by algorithm,
-- not `kid`) to pick between them arbitrarily.
CREATE UNIQUE INDEX idx_happyview_oauth_client_keys_one_current
    ON happyview_oauth_client_keys (owner) WHERE status = 'current';
