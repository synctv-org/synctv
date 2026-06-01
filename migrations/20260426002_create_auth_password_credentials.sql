CREATE TABLE IF NOT EXISTS auth_password_credentials (
    user_id BIGINT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    opaque_record BYTEA,
    opaque_credential_identifier BYTEA,
    opaque_ciphersuite VARCHAR(64),
    opaque_server_setup_version INTEGER,
    changed_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT auth_password_credentials_opaque_metadata_required
        CHECK (
            (
                opaque_record IS NULL
                AND opaque_credential_identifier IS NULL
                AND opaque_ciphersuite IS NULL
                AND opaque_server_setup_version IS NULL
            )
            OR (
                opaque_record IS NOT NULL
                AND opaque_credential_identifier IS NOT NULL
                AND opaque_ciphersuite IS NOT NULL
                AND opaque_server_setup_version IS NOT NULL
            )
        )
);

CREATE INDEX IF NOT EXISTS idx_auth_password_credentials_changed_at
    ON auth_password_credentials(changed_at);

CREATE TRIGGER update_auth_password_credentials_updated_at
    BEFORE UPDATE ON auth_password_credentials
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();
