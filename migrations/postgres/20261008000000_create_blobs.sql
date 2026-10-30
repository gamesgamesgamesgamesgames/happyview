-- Bytes a caller handed us, addressed by the CID of their content.
--
-- Nothing in HappyView stored bytes before this: `uploadBlob` is a proxy, and
-- the space blob route resolves a blob's author and fetches from that
-- account's PDS. A registry cannot work that way, because its obligations are
-- to hold the artifact before listing a release and to keep serving a
-- withdrawn version by exact version -- a publisher deleting a release is the
-- failure a registry exists to absorb.
--
-- The CID is the primary key rather than a surrogate id, so the checksum and
-- the identity are the same fact. "Never serve bytes that do not match the
-- release's checksum" then stops being a check that code has to remember and
-- becomes unrepresentable: bytes cannot be filed under a CID they do not hash
-- to. Identical content published twice occupies one row.
--
-- `size` duplicates what the bytes already say, deliberately: answering a
-- stat, or filling in a blob ref's `size`, should not have to read a megabyte
-- back, and the two dialects spell that length function differently.
--
-- `created_at` is TEXT written by `now_rfc3339()` rather than a SQL clock, so
-- it carries a `+00:00` offset and is safe to order or compare as text.
--
-- There is no owner column. Only the registry writes blobs today, and an
-- ownership model invented before a second writer exists would be a guess; it
-- is a nullable column away whenever a private-blob story needs one. Nothing
-- deletes rows either: content addressing means one row may have many
-- referents, so a delete on one referent's behalf is wrong without
-- refcounting, and on SQLite never deleting is what keeps the file from
-- needing a VACUUM to give pages back.
CREATE TABLE happyview_blobs (
    cid TEXT PRIMARY KEY,
    bytes BYTEA NOT NULL,
    mime_type TEXT NOT NULL,
    size BIGINT NOT NULL,
    created_at TEXT NOT NULL
);
