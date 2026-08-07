CREATE TABLE IF NOT EXISTS auth_totp_credentials (
    user_id BIGINT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    encrypted_secret JSONB NOT NULL,
    setup_id VARCHAR(64),
    setup_expires_at TIMESTAMPTZ,
    confirmed_at TIMESTAMPTZ,
    last_used_step BIGINT,
    recovery_code_hashes BYTEA[] NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT auth_totp_credentials_encrypted_secret_string
        CHECK (jsonb_typeof(encrypted_secret) = 'string'),
    CONSTRAINT auth_totp_credentials_setup_state_complete CHECK (
        (confirmed_at IS NOT NULL AND setup_id IS NULL AND setup_expires_at IS NULL)
        OR
        (confirmed_at IS NULL AND setup_id IS NOT NULL AND setup_expires_at IS NOT NULL)
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_auth_totp_credentials_setup_id
    ON auth_totp_credentials(setup_id)
    WHERE setup_id IS NOT NULL;

CREATE TRIGGER update_auth_totp_credentials_updated_at
    BEFORE UPDATE ON auth_totp_credentials
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();
