-- A space's own did:plc. HappyView holds its #atproto signing key, its
-- #atproto_kem key and one of its rotation keys, each sealed with
-- TOKEN_ENCRYPTION_KEY. A creator may hold a rotation key that outranks
-- HappyView's; only its public did:key is stored.
--
-- The row is written before the genesis operation is submitted, with
-- plc_state 'prepared'. The DID derives from the signed operation, so a
-- submission that failed can be repeated with the stored operation unchanged.
CREATE TABLE IF NOT EXISTS happyview_space_authorities (
    did TEXT PRIMARY KEY,
    signing_key_enc BYTEA NOT NULL,
    kem_key_enc BYTEA NOT NULL,
    rotation_key_enc BYTEA NOT NULL,
    creator_rotation_key TEXT,
    genesis_op TEXT NOT NULL,
    plc_state TEXT NOT NULL,
    created_by TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
