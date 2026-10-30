-- Phase 2b: backfill jobs can replay the Jetstream archive instead of
-- crawling PDSes. Existing jobs are crawls.
ALTER TABLE happyview_backfill_jobs ADD COLUMN source TEXT NOT NULL DEFAULT 'crawl';
-- A gap replay starts after this seq instead of at the start of the archive.
ALTER TABLE happyview_backfill_jobs ADD COLUMN replay_after_seq INTEGER;
-- Progress of the sweep running now, for the dashboard.
ALTER TABLE happyview_backfill_jobs ADD COLUMN replay_through_seq INTEGER;
ALTER TABLE happyview_backfill_jobs ADD COLUMN replay_tip_seq INTEGER;
ALTER TABLE happyview_backfill_jobs ADD COLUMN replay_bytes INTEGER NOT NULL DEFAULT 0;
ALTER TABLE happyview_backfill_jobs ADD COLUMN replay_blocks_failed INTEGER NOT NULL DEFAULT 0;

-- One planSnapshot sweep of a replay job. `kind` is 'commits' (up to 100
-- collections, plus the job's DIDs for an account job) or 'markers' (account
-- and sync events for up to 10,000 DIDs). `collections` and `dids` are JSON
-- arrays; `dids` is NULL for a network commit sweep. `cursor_seq` is the
-- afterSeq to resume from; `tip_seq` is pinned by the first page.
CREATE TABLE IF NOT EXISTS happyview_backfill_replay_sweeps (
    job_id      TEXT    NOT NULL REFERENCES happyview_backfill_jobs(id) ON DELETE CASCADE,
    sweep       INTEGER NOT NULL,
    kind        TEXT    NOT NULL,
    collections TEXT    NOT NULL,
    dids        TEXT,
    after_seq   INTEGER NOT NULL,
    cursor_seq  INTEGER NOT NULL,
    tip_seq     INTEGER,
    last_did_id INTEGER,
    done        INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (job_id, sweep)
);

-- Blocks a retry job refetches. block_index -1 is a whole segment.
CREATE TABLE IF NOT EXISTS happyview_backfill_replay_retries (
    job_id      TEXT    NOT NULL REFERENCES happyview_backfill_jobs(id) ON DELETE CASCADE,
    segment     TEXT    NOT NULL,
    block_index INTEGER NOT NULL,
    done        INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (job_id, segment, block_index)
);
