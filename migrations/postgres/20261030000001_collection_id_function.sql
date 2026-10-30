-- The id of collection $1, or NULL: what raw SQL filters
-- happyview_records_by_collection with, since raw SQL may not read
-- happyview_collections. STABLE, so a planner can use its value as an index
-- condition, evaluated once per scan.
CREATE OR REPLACE FUNCTION hv_collection_id(nsid text) RETURNS bigint
LANGUAGE sql STABLE STRICT PARALLEL SAFE AS $$
    SELECT c.id FROM happyview_collections c WHERE c.nsid = $1
$$;
