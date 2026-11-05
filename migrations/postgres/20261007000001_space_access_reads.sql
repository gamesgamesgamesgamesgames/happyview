-- Which access grant each `space.moderator_read` event was made under, so a
-- grant's reads can be listed by index instead of searching event JSON. Rows
-- go when their event does, whether by retention or by purge.
CREATE TABLE happyview_space_access_reads (
    event_id TEXT PRIMARY KEY REFERENCES happyview_event_logs(id) ON DELETE CASCADE,
    grant_id TEXT NOT NULL
);
CREATE INDEX idx_space_access_reads_grant ON happyview_space_access_reads(grant_id);

INSERT INTO happyview_space_access_reads (event_id, grant_id)
SELECT id, detail::jsonb ->> 'grant_id'
FROM happyview_event_logs
WHERE event_type = 'space.moderator_read'
  AND detail::jsonb ->> 'grant_id' IS NOT NULL;
