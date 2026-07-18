CREATE TABLE happyview_job_logs (
    id         TEXT PRIMARY KEY,
    job_id     TEXT NOT NULL,
    level      TEXT NOT NULL DEFAULT 'info',
    message    TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX idx_happyview_job_logs_job_id ON happyview_job_logs (job_id);
CREATE INDEX idx_happyview_job_logs_job_id_created_at ON happyview_job_logs (job_id, created_at);
