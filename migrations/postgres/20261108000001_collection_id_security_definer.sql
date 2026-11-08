-- `hv_collection_id` is called by the record-by-collection read that plugin SQL
-- runs, but the least-privileged plugin read role has no grant on
-- happyview_collections. Make the function SECURITY DEFINER so it runs as its
-- owner (HappyView's role), which can read that table, rather than the caller.
-- Its search_path stays pinned to HappyView's schema with pg_temp last, so a
-- caller's search path cannot redirect it.
CREATE OR REPLACE FUNCTION hv_collection_id(nsid text) RETURNS bigint
LANGUAGE sql STABLE STRICT PARALLEL SAFE SECURITY DEFINER AS $$
    SELECT c.id FROM happyview_collections c WHERE c.nsid = $1
$$;

DO $$
BEGIN
    EXECUTE format(
        'ALTER FUNCTION hv_collection_id(text) SET search_path = %I, pg_temp',
        current_schema()
    );
END $$;
