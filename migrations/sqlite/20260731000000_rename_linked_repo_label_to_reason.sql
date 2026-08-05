-- `label` was an admin's private note, but it is shown to whoever opens an
-- invite link as the stated reason for the request. Those were never two things
-- — the field only ever had one audience — so it is named for that audience.
ALTER TABLE happyview_linked_repos RENAME COLUMN label TO reason;
