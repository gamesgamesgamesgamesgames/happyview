-- A running job is held by a lease its worker renews. Recovery requeues only
-- jobs whose lease ran out, so one instance's restart no longer requeues the
-- jobs another instance is running. `lease_until` is epoch milliseconds;
-- `attempt` counts the runs started, a run that lost its lease included.
ALTER TABLE happyview_jobs ADD COLUMN attempt INTEGER NOT NULL DEFAULT 0;
ALTER TABLE happyview_jobs ADD COLUMN claimed_by TEXT;
ALTER TABLE happyview_jobs ADD COLUMN lease_until BIGINT;
