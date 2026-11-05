-- When the session's pds_url and issuer were last checked against the user's
-- DID document. NULL for a session stored before registration checked them;
-- its first use checks them and sets this.
ALTER TABLE happyview_dpop_sessions ADD COLUMN verified_at TEXT;
