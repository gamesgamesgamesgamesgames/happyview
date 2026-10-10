-- Backfill work as a bounded queue of (did, collection) units.
--
-- happyview_backfill_repos kept every discovered DID for the life of a job
-- (6.86M rows, 1.55 GB on one network backfill) and is keyed (job_id, did),
-- so a DID found under two collections could not be two units at once.
-- Jobs created from here on queue their work in these tables instead; jobs
-- created before keep queue_version = 1 and finish on their old rows.

-- Pending units. A unit is deleted when it completes; collection '' means
-- every collection the job targets (account-targeted jobs).
CREATE TABLE happyview_backfill_queue (
    job_id       TEXT NOT NULL REFERENCES happyview_backfill_jobs(id) ON DELETE CASCADE,
    collection   TEXT NOT NULL,
    did          TEXT NOT NULL,
    pds_endpoint TEXT,
    PRIMARY KEY (job_id, collection, did)
);

-- The resolver rescans for unresolved units while discovery runs; this keeps
-- each rescan to the units still waiting rather than the whole window.
CREATE INDEX idx_backfill_queue_unresolved
    ON happyview_backfill_queue (job_id, collection, did)
    WHERE pds_endpoint IS NULL;

-- How far discovery got through each collection's relay listing, so a
-- paused or restarted job resumes from its last enqueued page.
CREATE TABLE happyview_backfill_cursors (
    job_id       TEXT NOT NULL REFERENCES happyview_backfill_jobs(id) ON DELETE CASCADE,
    collection   TEXT NOT NULL,
    relay_cursor TEXT,
    done         INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (job_id, collection)
);

-- Per-PDS progress, maintained as counters instead of a GROUP BY over every
-- unit. 64-bit: one large PDS can pass 2^31 records over a network backfill.
CREATE TABLE happyview_backfill_pds_stats (
    job_id          TEXT NOT NULL REFERENCES happyview_backfill_jobs(id) ON DELETE CASCADE,
    pds_endpoint    TEXT NOT NULL,
    repos           BIGINT NOT NULL DEFAULT 0,
    completed_repos BIGINT NOT NULL DEFAULT 0,
    records         BIGINT NOT NULL DEFAULT 0,
    errors          BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (job_id, pds_endpoint)
);

-- The most recent completed units per job (trimmed to 1,000), for the
-- dashboard's "fetched" list.
CREATE TABLE happyview_backfill_completions (
    id              BIGSERIAL PRIMARY KEY,
    job_id          TEXT NOT NULL REFERENCES happyview_backfill_jobs(id) ON DELETE CASCADE,
    did             TEXT NOT NULL,
    collection      TEXT NOT NULL,
    pds_endpoint    TEXT NOT NULL,
    records_fetched INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_backfill_completions_job ON happyview_backfill_completions (job_id, id);

-- 1 = repos in happyview_backfill_repos (jobs from before this migration);
-- 2 = units in happyview_backfill_queue.
ALTER TABLE happyview_backfill_jobs ADD COLUMN queue_version INTEGER NOT NULL DEFAULT 1;

-- 0 while a network job is still listing repos from the relay.
ALTER TABLE happyview_backfill_jobs ADD COLUMN discovery_complete INTEGER NOT NULL DEFAULT 1;
