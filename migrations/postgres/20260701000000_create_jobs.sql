CREATE TABLE happyview_jobs (
    id            TEXT PRIMARY KEY,
    job_type      TEXT NOT NULL,
    status        TEXT NOT NULL DEFAULT 'pending',
    input         TEXT NOT NULL DEFAULT '{}',
    progress      TEXT NOT NULL DEFAULT '{}',
    result        TEXT,
    error         TEXT,
    created_by    TEXT NOT NULL,
    started_at    TEXT,
    completed_at  TEXT,
    created_at    TEXT NOT NULL
);

CREATE INDEX idx_happyview_jobs_status ON happyview_jobs (status);
CREATE INDEX idx_happyview_jobs_job_type ON happyview_jobs (job_type);
CREATE INDEX idx_happyview_jobs_created_by ON happyview_jobs (created_by);
