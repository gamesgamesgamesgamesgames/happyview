-- A prefix of idx_records_collection_indexed_at and of
-- idx_records_collection_created_at_uri, so every query it served has an index.
DROP INDEX IF EXISTS idx_records_collection;
