-- A running job is held by a lease its worker renews. Recovery requeues only
-- jobs whose lease ran out, so one instance's restart no longer requeues the
-- jobs another instance is running. `lease_until` is epoch milliseconds;
-- `attempt` counts the claims, and fences a run's writes to its own claim;
-- `lost_leases` counts the runs whose worker stopped, which alone decides
-- when a job is failed rather than queued again.
ALTER TABLE happyview_jobs ADD COLUMN attempt INTEGER NOT NULL DEFAULT 0;
ALTER TABLE happyview_jobs ADD COLUMN claimed_by TEXT;
ALTER TABLE happyview_jobs ADD COLUMN lease_until BIGINT;
ALTER TABLE happyview_jobs ADD COLUMN lost_leases INTEGER NOT NULL DEFAULT 0;
