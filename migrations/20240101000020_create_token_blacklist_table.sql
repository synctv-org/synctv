CREATE TABLE IF NOT EXISTS token_blacklist (
    jti TEXT PRIMARY KEY,
    expires_at TIMESTAMPTZ NOT NULL,
    family_revoked_at BIGINT
);

CREATE INDEX idx_token_blacklist_expires ON token_blacklist(expires_at);

CREATE OR REPLACE FUNCTION cleanup_expired_token_blacklist()
RETURNS void AS $$
BEGIN
    DELETE FROM token_blacklist WHERE expires_at < CURRENT_TIMESTAMP;
END;
$$ LANGUAGE plpgsql;
