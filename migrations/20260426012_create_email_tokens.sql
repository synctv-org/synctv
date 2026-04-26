CREATE TABLE IF NOT EXISTS email_tokens (
    id BIGSERIAL PRIMARY KEY,
    token VARCHAR(255) UNIQUE NOT NULL,
    user_id CHAR(12) NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    token_type SMALLINT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    used_at TIMESTAMPTZ NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_email_tokens_user_id ON email_tokens(user_id);
CREATE INDEX IF NOT EXISTS idx_email_tokens_type_expires ON email_tokens(token_type, expires_at);
CREATE INDEX IF NOT EXISTS idx_email_tokens_expires_at ON email_tokens(expires_at);

CREATE OR REPLACE FUNCTION cleanup_expired_email_tokens()
RETURNS void AS $$
BEGIN
    DELETE FROM email_tokens WHERE expires_at < CURRENT_TIMESTAMP;
END;
$$ LANGUAGE plpgsql;

COMMENT ON TABLE email_tokens IS 'Email token records';
COMMENT ON COLUMN email_tokens.expires_at IS 'Token expiration timestamp';
COMMENT ON COLUMN email_tokens.used_at IS 'Timestamp when the token was used';
COMMENT ON FUNCTION cleanup_expired_email_tokens() IS 'Delete expired email tokens';
