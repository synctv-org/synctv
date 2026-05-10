CREATE TABLE IF NOT EXISTS auth_password_credentials (
    user_id BIGINT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    legacy_password_hash TEXT,
    legacy_password_algorithm VARCHAR(64),
    opaque_record BYTEA,
    opaque_credential_identifier BYTEA,
    opaque_ciphersuite VARCHAR(64),
    opaque_server_setup_version INTEGER,
    password_changed_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    password_version INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT auth_password_credentials_legacy_algorithm_required
        CHECK (
            (legacy_password_hash IS NULL AND legacy_password_algorithm IS NULL)
            OR (legacy_password_hash IS NOT NULL AND legacy_password_algorithm IS NOT NULL)
        ),
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

CREATE INDEX IF NOT EXISTS idx_auth_password_credentials_password_changed_at
    ON auth_password_credentials(password_changed_at);

CREATE TRIGGER update_auth_password_credentials_updated_at
    BEFORE UPDATE ON auth_password_credentials
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();
