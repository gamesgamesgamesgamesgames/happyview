-- The rev (TID) of the repo commit that produced each network record, so an
-- older delivery (a reconnect, a replay, a backfill racing live ingest) never
-- overwrites a newer one. NULL for local writes, listRecords rows and rows
-- written before this column existed, which keep today's behaviour. Adding a
-- nullable column without a default rewrites nothing.
ALTER TABLE happyview_records ADD COLUMN IF NOT EXISTS rev TEXT;
