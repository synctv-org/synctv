-- Token blacklist table for durable JTI tracking when Redis is unavailable.
-- Used by PgTokenBlacklistStore as a fallback for standalone deployments.

CREATE TABLE IF NOT EXISTS token_blacklist (
    jti TEXT PRIMARY KEY,
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX idx_token_blacklist_expires ON token_blacklist(expires_at);

-- Cleanup function to remove expired token blacklist entries.
-- Call this periodically to prevent unbounded table growth.
CREATE OR REPLACE FUNCTION cleanup_expired_token_blacklist()
RETURNS void AS $$
BEGIN
    DELETE FROM token_blacklist WHERE expires_at < CURRENT_TIMESTAMP;
END;
$$ LANGUAGE plpgsql;
