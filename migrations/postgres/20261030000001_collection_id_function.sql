-- The id of collection $1, or NULL: what raw SQL filters
-- happyview_records_by_collection with, since raw SQL may not read
-- happyview_collections. STABLE, so a planner can use its value as an index
-- condition, evaluated once per scan.
CREATE OR REPLACE FUNCTION hv_collection_id(nsid text) RETURNS bigint
LANGUAGE sql STABLE STRICT PARALLEL SAFE AS $$
    SELECT c.id FROM happyview_collections c WHERE c.nsid = $1
$$;

-- Read happyview_collections from the schema HappyView's tables are in,
-- whatever the caller's search path puts first: a role whose path leads
-- with another schema could otherwise point it at a table of its own.
DO $$
BEGIN
    EXECUTE format(
        'ALTER FUNCTION hv_collection_id(text) SET search_path = %I, pg_temp',
        current_schema()
    );
END $$;
