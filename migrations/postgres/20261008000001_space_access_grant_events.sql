-- Which access grant each `space.access_granted` and `space.access_revoked`
-- event records. Retention keeps a grant's events, and the reads made under it,
-- for as long as the grant row exists, without holding back unrelated events.
CREATE TABLE happyview_space_access_grant_events (
    event_id TEXT PRIMARY KEY REFERENCES happyview_event_logs(id) ON DELETE CASCADE,
    grant_id TEXT NOT NULL
);
CREATE INDEX idx_space_access_grant_events_grant ON happyview_space_access_grant_events(grant_id);

INSERT INTO happyview_space_access_grant_events (event_id, grant_id)
SELECT id, detail::jsonb ->> 'grant_id'
FROM happyview_event_logs
WHERE event_type IN ('space.access_granted', 'space.access_revoked')
  AND detail::jsonb ->> 'grant_id' IS NOT NULL;
