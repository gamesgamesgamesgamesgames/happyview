-- Script and ingest metrics past what memory holds. A row is one key's
-- closed minute bucket, folded into an hour once it is two days old, and
-- kept as long as the event log is. `bucket_start` is RFC 3339 text with a
-- `+00:00` offset, so ranges compare as text. For a script `key` is its
-- trigger and `variant` its kind and layer, `<kind>/<layer>`; for ingest
-- `key` is the collection and `variant` the action. `counters` and
-- `sketches` are JSON objects whose fields may be missing. `instance` is
-- the process that wrote the row (`HAPPYVIEW_INSTANCE_ID`, else its
-- hostname, else a random id for one boot): instances sharing a database
-- each write, fold and read back only their own rows, and a stored window
-- sums every instance's.
CREATE TABLE IF NOT EXISTS happyview_script_metrics (
    kind TEXT NOT NULL,
    key TEXT NOT NULL,
    variant TEXT NOT NULL,
    resolution TEXT NOT NULL,
    bucket_start TEXT NOT NULL,
    counters TEXT NOT NULL,
    sketches TEXT NOT NULL,
    instance TEXT NOT NULL,
    PRIMARY KEY (kind, key, variant, resolution, bucket_start, instance)
);

CREATE INDEX IF NOT EXISTS happyview_script_metrics_bucket_start
    ON happyview_script_metrics (bucket_start);
