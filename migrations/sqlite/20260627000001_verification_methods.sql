CREATE TABLE happyview_verification_methods (
    id TEXT PRIMARY KEY,
    fragment_id TEXT NOT NULL UNIQUE,
    key_type TEXT NOT NULL DEFAULT 'Multikey',
    public_key_multibase TEXT NOT NULL,
    private_key_enc BLOB NOT NULL,
    created_at TEXT NOT NULL
);
