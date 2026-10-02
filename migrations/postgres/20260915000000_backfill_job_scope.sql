-- How a backfill job finds its repos.
--
--   network  discovered through the relay's listReposByCollection.
--   dids     supplied when the job was created; discovery is skipped.
ALTER TABLE happyview_backfill_jobs ADD COLUMN scope TEXT NOT NULL DEFAULT 'network';

UPDATE happyview_backfill_jobs SET scope = 'dids' WHERE did IS NOT NULL;
