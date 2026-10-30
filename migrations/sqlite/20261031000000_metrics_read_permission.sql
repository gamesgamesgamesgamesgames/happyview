-- Script metrics are read with `metrics:read`; everyone who could already
-- read the event log can read them.
INSERT INTO happyview_user_permissions (user_id, permission, granted_at, granted_by)
SELECT user_id, 'metrics:read', granted_at, granted_by
  FROM happyview_user_permissions
 WHERE permission = 'events:read'
ON CONFLICT (user_id, permission) DO NOTHING;
