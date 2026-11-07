-- The scope the authorization server reported in a token response HappyView
-- received itself, as opposed to `scopes`, which the app supplied when it
-- registered the session. The admin API reads only this one.
ALTER TABLE happyview_dpop_sessions ADD COLUMN granted_scope TEXT;
