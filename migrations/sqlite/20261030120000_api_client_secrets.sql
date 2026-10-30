-- A client may hold several secrets, each labelled and revocable on its own.
-- The client's own client_secret_hash is no longer read; it is left in place
-- so a rollback still authenticates the secret each client had.
CREATE TABLE happyview_api_client_secrets (
    id TEXT PRIMARY KEY,
    client_id TEXT NOT NULL REFERENCES happyview_api_clients(id) ON DELETE CASCADE,
    label TEXT NOT NULL,
    secret_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_used_at TEXT,
    revoked_at TEXT
);

CREATE INDEX idx_api_client_secrets_client_id ON happyview_api_client_secrets(client_id);

-- Each existing secret becomes the client's first credential, its hash
-- copied as it is: the plaintext was never stored, so it cannot be hashed
-- again.
INSERT INTO happyview_api_client_secrets (id, client_id, label, secret_hash, created_at)
SELECT 'original-' || id, id, 'Original', client_secret_hash, created_at
FROM happyview_api_clients
WHERE client_secret_hash <> '';
