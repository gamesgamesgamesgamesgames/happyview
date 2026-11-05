-- no-transaction
-- The default records listing filters on collection and orders by
-- (created_at DESC, uri DESC). idx_records_created_at_uri had no collection
-- prefix, so that listing walked a collection index and sorted. This index
-- serves the filter and the order together, and it also covers
-- idx_records_collection.
--
-- CONCURRENTLY keeps writes flowing while it builds on a large table, and it
-- cannot run in a transaction, hence one statement in a no-transaction file.
-- A failed build leaves an INVALID index of this name behind, which IF NOT
-- EXISTS would skip; db::connect drops it before migrating, so the retry on
-- the next boot builds it again.
CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_records_collection_created_at_uri
    ON happyview_records (collection, created_at DESC, uri DESC);
