-- The compact record format (Phase 3). Created empty beside the legacy
-- happyview_records/happyview_record_refs; the boot-time upgrade in
-- src/record_store/upgrade.rs copies the data, verifies it, replaces the
-- legacy tables with compatibility views of the same names, and drops them.

-- Interned identity. Rows are never deleted, so an id never changes meaning.
CREATE TABLE IF NOT EXISTS happyview_dids (
    id  INTEGER PRIMARY KEY,
    did TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS happyview_collections (
    id   INTEGER PRIMARY KEY,
    nsid TEXT NOT NULL UNIQUE
);

-- Per-collection zstd dictionaries. Never deleted while a row references one.
CREATE TABLE IF NOT EXISTS happyview_zstd_dicts (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    collection_id INTEGER NOT NULL REFERENCES happyview_collections(id),
    created_at    INTEGER NOT NULL,
    dict          BLOB NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_zstd_dicts_collection ON happyview_zstd_dicts (collection_id, id);

-- body is the record's JSON text exactly as received, zstd-compressed with
-- dictionary body_dict, or raw when body_dict is NULL (not yet considered for
-- compression) or 0 (considered and kept raw). cid is codec::encode_cid's
-- form. indexed_at and created_at are unix microseconds; a NULL indexed_at
-- still means "local write awaiting its Jetstream echo".
CREATE TABLE IF NOT EXISTS happyview_records_v2 (
    id            INTEGER PRIMARY KEY,
    did_id        INTEGER NOT NULL REFERENCES happyview_dids(id),
    collection_id INTEGER NOT NULL REFERENCES happyview_collections(id),
    rkey          TEXT NOT NULL,
    cid           BLOB NOT NULL,
    body          BLOB NOT NULL,
    body_dict     INTEGER,
    rev           TEXT,
    indexed_at    INTEGER,
    created_at    INTEGER NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_records_v2_identity
    ON happyview_records_v2 (did_id, collection_id, rkey);
CREATE INDEX IF NOT EXISTS idx_records_v2_collection_indexed_at
    ON happyview_records_v2 (collection_id, indexed_at);
CREATE INDEX IF NOT EXISTS idx_records_v2_collection_created_at_id
    ON happyview_records_v2 (collection_id, created_at DESC, id DESC);
-- What the background recompression walks.
CREATE INDEX IF NOT EXISTS idx_records_v2_uncompressed
    ON happyview_records_v2 (collection_id, id) WHERE body_dict IS NULL;

-- collection_id is the source record's collection, as the legacy collection
-- column was. A target that is not at://did/collection/rkey keeps its exact
-- text: target_collection_id 0 means 'at://' || the interned text, -1 the
-- interned text verbatim (src/record_store/codec.rs, RefTarget).
CREATE TABLE IF NOT EXISTS happyview_record_refs_v2 (
    source_id            INTEGER NOT NULL REFERENCES happyview_records_v2(id) ON DELETE CASCADE,
    target_did_id        INTEGER NOT NULL,
    target_collection_id INTEGER NOT NULL,
    target_rkey          TEXT NOT NULL,
    collection_id        INTEGER NOT NULL,
    PRIMARY KEY (source_id, target_did_id, target_collection_id, target_rkey)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS idx_record_refs_v2_target
    ON happyview_record_refs_v2 (target_did_id, target_collection_id, target_rkey, collection_id);
