CREATE TABLE IF NOT EXISTS auth_token_blacklist (
    jti TEXT PRIMARY KEY,
    expires_at TIMESTAMPTZ NOT NULL,
    family_revoked_at BIGINT
);

CREATE INDEX IF NOT EXISTS idx_auth_token_blacklist_expires ON auth_token_blacklist(expires_at);
