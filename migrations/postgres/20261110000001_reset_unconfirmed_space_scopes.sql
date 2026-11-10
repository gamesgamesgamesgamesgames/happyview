-- Sessions registered before HappyView confirmed a session's scopes with its
-- authorization server may hold a space: scope the user never granted. HappyView
-- enforces space: scopes itself, so drop such sessions back to atproto; the
-- user's next sign-in registers the scopes again, checked. Other scopes are
-- enforced by the user's PDS and are left alone on sessions without space:.
UPDATE happyview_dpop_sessions SET scopes = 'atproto' WHERE scopes LIKE '%space:%';
