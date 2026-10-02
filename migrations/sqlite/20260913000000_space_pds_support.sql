-- Cached answers to "does this PDS serve atproto spaces".
--
-- Keyed by endpoint, not DID: the answer is a property of the server, so one
-- probe covers every account hosted on it.
CREATE TABLE happyview_space_pds_support (
    pds_endpoint TEXT PRIMARY KEY,
    supported INTEGER NOT NULL,
    -- Which tier produced the answer, for operator diagnosis: a "no" from a
    -- descriptor is a server stating its own method list, while a "no" from a
    -- probe is an inference, and "unreachable" is not evidence about the server
    -- at all.
    tier TEXT NOT NULL,
    -- Required methods the server did not offer, as a JSON array.
    missing TEXT NOT NULL DEFAULT '[]',
    checked_at TEXT NOT NULL
);
