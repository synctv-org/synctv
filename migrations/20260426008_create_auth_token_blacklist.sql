CREATE TABLE IF NOT EXISTS auth_token_blacklist (
    jti TEXT PRIMARY KEY,
    expires_at TIMESTAMPTZ NOT NULL,
    family_revoked_at BIGINT
);

CREATE INDEX IF NOT EXISTS idx_auth_token_blacklist_expires ON auth_token_blacklist(expires_at);

CREATE OR REPLACE FUNCTION cleanup_expired_auth_token_blacklist()
RETURNS void AS $$
BEGIN
    DELETE FROM auth_token_blacklist WHERE expires_at < CURRENT_TIMESTAMP;
END;
$$ LANGUAGE plpgsql;

COMMENT ON TABLE auth_token_blacklist IS 'Revoked token identifiers';
COMMENT ON FUNCTION cleanup_expired_auth_token_blacklist() IS 'Delete expired token blacklist records';
