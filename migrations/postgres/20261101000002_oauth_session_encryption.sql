-- Whether `session_data` holds ciphertext. Rows are encrypted at boot by
-- `auth::oauth_store::seal_stored_sessions`, since SQL has no key. INTEGER on
-- both backends, as `happyview_script_variables.encrypted` is.
ALTER TABLE happyview_oauth_sessions ADD COLUMN encrypted INTEGER NOT NULL DEFAULT 0;
ALTER TABLE happyview_linked_repo_sessions ADD COLUMN encrypted INTEGER NOT NULL DEFAULT 0;
