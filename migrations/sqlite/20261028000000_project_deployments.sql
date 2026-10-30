-- Projects deployed through PUT /admin/project, newest last. The bundle is the
-- archive exactly as it arrived, so a boot can unpack and build what was
-- deployed without a volume to keep it on.
CREATE TABLE IF NOT EXISTS happyview_project_deployments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    sha256 TEXT NOT NULL,
    bundle BLOB NOT NULL,
    deployed_at TEXT NOT NULL,
    deployed_by TEXT NOT NULL
);
