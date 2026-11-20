-- A crawl unit stays queued until it is fetched or given up, retrying in
-- place while its PDS is rate-limiting or failing. Before, a unit whose
-- fetch was deferred completed with no records and counted as processed.

-- Failed attempts at fetching the unit so far, kept across a pause or
-- restart so a resumed job does not start the count again.
ALTER TABLE happyview_backfill_queue ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;

-- When the unit's next attempt is due (Unix milliseconds), while it waits
-- out a backoff; NULL otherwise.
ALTER TABLE happyview_backfill_queue ADD COLUMN retry_at INTEGER;

-- 429 responses from each PDS, and across the job.
ALTER TABLE happyview_backfill_pds_stats ADD COLUMN rate_limited INTEGER NOT NULL DEFAULT 0;
ALTER TABLE happyview_backfill_jobs ADD COLUMN rate_limited INTEGER NOT NULL DEFAULT 0;
