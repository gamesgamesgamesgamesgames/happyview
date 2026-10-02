CREATE TABLE happyview_backfill_errors (
    job_id     TEXT NOT NULL REFERENCES happyview_backfill_jobs(id) ON DELETE CASCADE,
    did        TEXT NOT NULL,
    collection TEXT,
    phase      TEXT NOT NULL,
    kind       TEXT NOT NULL,
    message    TEXT NOT NULL,
    attempts   INTEGER NOT NULL DEFAULT 1,
    last_at    TEXT NOT NULL,
    PRIMARY KEY (job_id, did, phase)
);

CREATE INDEX idx_happyview_backfill_errors_job_kind
    ON happyview_backfill_errors (job_id, kind);

ALTER TABLE happyview_backfill_jobs ADD COLUMN error_counts TEXT;
