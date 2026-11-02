-- HappyView's own key for signing space delegation tokens, sealed with
-- TOKEN_ENCRYPTION_KEY. `spaces::delegation_key` generates it on first use.
-- It is never stored unsealed, and its public key is published nowhere.
CREATE TABLE IF NOT EXISTS happyview_delegation_key (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    private_key TEXT NOT NULL,
    created_at TEXT NOT NULL
);
