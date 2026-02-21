-- Token blacklist table for durable JTI tracking when Redis is unavailable.
-- Used by PgTokenBlacklistStore as a fallback for standalone deployments.

CREATE TABLE IF NOT EXISTS token_blacklist (
    jti TEXT PRIMARY KEY,
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX idx_token_blacklist_expires ON token_blacklist(expires_at);
