-- Create email verification and password reset tokens table
CREATE TABLE IF NOT EXISTS email_tokens (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    token VARCHAR(255) UNIQUE NOT NULL,
    user_id CHAR(12) NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_type VARCHAR(20) NOT NULL, -- 'email_verification' or 'password_reset'
    expires_at TIMESTAMPTZ NOT NULL,
    used_at TIMESTAMPTZ NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT email_tokens_token_type_check
        CHECK (token_type IN ('email_verification', 'password_reset'))
);

-- Create indexes
-- Note: No separate index on token needed — the UNIQUE constraint already provides one
CREATE INDEX idx_email_tokens_user_id ON email_tokens(user_id);
CREATE INDEX idx_email_tokens_type_expires ON email_tokens(token_type, expires_at);
CREATE INDEX idx_email_tokens_expires_at ON email_tokens(expires_at);

-- Index for finding unused tokens
CREATE INDEX idx_email_tokens_unused ON email_tokens(user_id, token_type, expires_at)
    WHERE used_at IS NULL;

-- Cleanup expired tokens and used tokens (run via cron/job)
-- This prevents indefinite accumulation of used token rows
CREATE OR REPLACE FUNCTION cleanup_expired_email_tokens()
RETURNS void AS $$
BEGIN
    -- Delete tokens that are expired (regardless of used_at status)
    -- This includes both unused expired tokens and used tokens that have passed expiry
    DELETE FROM email_tokens WHERE expires_at < CURRENT_TIMESTAMP;
END;
$$ LANGUAGE plpgsql;

-- Comments
COMMENT ON TABLE email_tokens IS 'Email verification and password reset tokens';
COMMENT ON COLUMN email_tokens.token_type IS 'Type of token: email_verification or password_reset';
COMMENT ON COLUMN email_tokens.expires_at IS 'Token expiration timestamp';
COMMENT ON COLUMN email_tokens.used_at IS 'When the token was used (NULL = unused)';
COMMENT ON FUNCTION cleanup_expired_email_tokens() IS 'Delete expired email tokens (both used and unused) - run periodically';
