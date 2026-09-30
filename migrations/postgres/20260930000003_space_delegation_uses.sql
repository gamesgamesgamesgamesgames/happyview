-- Delegation tokens are single-use. Each exchanged token is held here until it
-- would have expired anyway, so a captured one cannot be exchanged again.
CREATE TABLE happyview_space_delegation_uses (
    issuer TEXT NOT NULL,
    jti TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    PRIMARY KEY (issuer, jti)
);
CREATE INDEX idx_space_delegation_uses_expires ON happyview_space_delegation_uses(expires_at);
