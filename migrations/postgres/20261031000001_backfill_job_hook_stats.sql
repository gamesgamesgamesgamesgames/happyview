-- What a backfill job's records cost in hooks, kept once the job ends.
ALTER TABLE happyview_backfill_jobs ADD COLUMN hook_stats TEXT;
