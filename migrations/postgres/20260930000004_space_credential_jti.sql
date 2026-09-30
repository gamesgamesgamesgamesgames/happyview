-- Credentials are revoked by `jti`, the identifier proposal 0016 revokes them by,
-- rather than by a hash of the token.
ALTER TABLE happyview_space_credentials ADD COLUMN jti TEXT;
CREATE INDEX idx_space_credentials_jti ON happyview_space_credentials(space_id, jti);
