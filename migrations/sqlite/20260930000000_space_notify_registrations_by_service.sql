-- Notify registrations are space-wide: a syncer registered with a space host is
-- told about every writer. `author_did` held the subscriber's service DID, which
-- dispatch then matched against the writer, so a syncer only heard about its own
-- writes. The column is renamed to what it held, and a service holds one
-- registration per space.
ALTER TABLE happyview_space_notify_registrations RENAME COLUMN author_did TO service;

UPDATE happyview_space_notify_registrations SET service = registered_by WHERE service IS NULL;

DELETE FROM happyview_space_notify_registrations
WHERE id NOT IN (
    SELECT id FROM (
        SELECT id, ROW_NUMBER() OVER (
            PARTITION BY space_id, service ORDER BY created_at DESC
        ) AS rn
        FROM happyview_space_notify_registrations
    ) ranked
    WHERE rn = 1
);

DROP INDEX IF EXISTS idx_space_notify_repo;
CREATE UNIQUE INDEX idx_space_notify_service ON happyview_space_notify_registrations(space_id, service);
