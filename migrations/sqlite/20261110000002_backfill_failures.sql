-- Every repo a crawl job gave up on, or still has a fetch retry pending for,
-- one row per (did, collection), with no cap.
--
-- happyview_backfill_errors keeps message detail for the first 10,000
-- give-ups of a job and a DID once per phase, so past that cap a large crawl
-- kept no list of which repos failed. And a fetch deferred for retry lived
-- only in memory: a pause or restart dropped it with its unit already
-- completed. These rows are what retry-failed reads, and a resumed job
-- queues its pending rows again.
CREATE TABLE happyview_backfill_failures (
    job_id     TEXT NOT NULL REFERENCES happyview_backfill_jobs(id) ON DELETE CASCADE,
    did        TEXT NOT NULL,
    -- The collection that failed; '' for a resolve give-up of a unit that
    -- covers every collection its job targets.
    collection TEXT NOT NULL,
    -- 'resolve' or 'fetch'.
    phase      TEXT NOT NULL,
    -- A BackfillErrorKind, as in happyview_backfill_errors.
    kind       TEXT NOT NULL,
    -- 'pending': a fetch retry is due; 'failed': the job gave up.
    state      TEXT NOT NULL,
    attempts   INTEGER NOT NULL DEFAULT 0,
    last_at    TEXT NOT NULL,
    PRIMARY KEY (job_id, did, collection)
);

CREATE INDEX idx_backfill_failures_state
    ON happyview_backfill_failures (job_id, state, kind);
