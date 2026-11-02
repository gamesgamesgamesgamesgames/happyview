-- Whether `state_data` holds ciphertext. Rows are short-lived, so they need no
-- boot step: one from before this reads as plaintext until it expires. INTEGER
-- on both backends, as the session tables are.
ALTER TABLE happyview_oauth_state ADD COLUMN encrypted INTEGER NOT NULL DEFAULT 0;
