-- Retention deletes ended grants by when they expired or were revoked, across
-- all users.
CREATE INDEX idx_space_access_grants_expires_at ON happyview_space_access_grants(expires_at);
CREATE INDEX idx_space_access_grants_revoked_at ON happyview_space_access_grants(revoked_at);
