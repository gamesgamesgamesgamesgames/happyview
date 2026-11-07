-- A space's own did:plc. HappyView holds its #atproto signing key, its
-- #atproto_kem key and one of its rotation keys, each sealed with
-- TOKEN_ENCRYPTION_KEY. A creator may hold a rotation key that outranks
-- HappyView's; only its public did:key is stored.
--
-- The row is written before the genesis operation is submitted, with
-- plc_state 'prepared'. The DID derives from the signed operation, so a
-- submission that failed is repeated with the stored operation unchanged. One
-- row per (created_by, space_type, skey): creating that space again, after a
-- failure or a deletion, reuses its DID, and a concurrent second create fails
-- on the constraint.
CREATE TABLE IF NOT EXISTS happyview_space_authorities (
    did TEXT PRIMARY KEY,
    signing_key_enc BLOB NOT NULL,
    kem_key_enc BLOB NOT NULL,
    rotation_key_enc BLOB NOT NULL,
    creator_rotation_key TEXT,
    genesis_op TEXT NOT NULL,
    plc_state TEXT NOT NULL,
    created_by TEXT NOT NULL,
    space_type TEXT NOT NULL,
    skey TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (created_by, space_type, skey)
);
