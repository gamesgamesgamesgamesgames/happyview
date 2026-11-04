-- The attestation signing key, in a table of its own rather than among the
-- instance settings. `plugin::attestation::load_or_generate` moves an existing
-- key here at boot and encrypts it, since SQL has no key. `encrypted` is an
-- INTEGER on both backends, as the space tables' flags are.
CREATE TABLE IF NOT EXISTS happyview_attestation_key (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    private_key TEXT NOT NULL,
    encrypted INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);
