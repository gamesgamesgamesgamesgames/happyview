-- Records deleted by a live or replayed delete, so an out-of-order create at
-- or before the delete's rev cannot bring them back. Swept after
-- RECORD_TOMBSTONE_RETENTION_HOURS.
CREATE TABLE IF NOT EXISTS happyview_record_tombstones (
    uri        TEXT PRIMARY KEY,
    rev        TEXT NOT NULL,
    deleted_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_record_tombstones_deleted_at
    ON happyview_record_tombstones (deleted_at);

-- Deleted accounts: one row per DID instead of one per purged record. rev is
-- a TID of the moment Jetstream witnessed the deletion; seq is the Jetstream
-- seq when the event came from v2.
CREATE TABLE IF NOT EXISTS happyview_did_tombstones (
    did        TEXT PRIMARY KEY,
    rev        TEXT NOT NULL,
    seq        INTEGER,
    deleted_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_did_tombstones_deleted_at
    ON happyview_did_tombstones (deleted_at);
