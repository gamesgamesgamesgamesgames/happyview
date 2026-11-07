-- The default records listing filters on collection and orders by
-- (created_at DESC, uri DESC). idx_records_created_at_uri had no collection
-- prefix, so that listing walked a collection index and sorted. This index
-- serves the filter and the order together, and it also covers
-- idx_records_collection.
CREATE INDEX IF NOT EXISTS idx_records_collection_created_at_uri
    ON happyview_records (collection, created_at DESC, uri DESC);
