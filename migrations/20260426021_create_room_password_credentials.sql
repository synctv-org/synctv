CREATE TABLE IF NOT EXISTS room_password_credentials (
    room_id BIGINT PRIMARY KEY REFERENCES rooms(id) ON DELETE CASCADE,
    opaque_record BYTEA,
    opaque_credential_identifier BYTEA,
    opaque_ciphersuite VARCHAR(64),
    opaque_server_setup_version INTEGER,
    enabled BOOLEAN NOT NULL DEFAULT false,
    changed_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    version INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT room_password_credentials_opaque_metadata_required
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
        ),
    CONSTRAINT room_password_credentials_enabled_requires_opaque
        CHECK (
            enabled = false
            OR (
                opaque_record IS NOT NULL
                AND opaque_credential_identifier IS NOT NULL
                AND opaque_ciphersuite IS NOT NULL
                AND opaque_server_setup_version IS NOT NULL
            )
        ),
    CONSTRAINT room_password_credentials_opaque_record_not_empty
        CHECK (opaque_record IS NULL OR length(opaque_record) > 0),
    CONSTRAINT room_password_credentials_opaque_identifier_not_empty
        CHECK (opaque_credential_identifier IS NULL OR length(opaque_credential_identifier) > 0),
    CONSTRAINT room_password_credentials_opaque_ciphersuite_not_empty
        CHECK (opaque_ciphersuite IS NULL OR length(trim(opaque_ciphersuite)) > 0),
    CONSTRAINT room_password_credentials_opaque_setup_version_positive
        CHECK (
            opaque_server_setup_version IS NULL
            OR opaque_server_setup_version > 0
        ),
    CONSTRAINT room_password_credentials_version_nonnegative
        CHECK (version >= 0)
);

CREATE INDEX IF NOT EXISTS idx_room_password_credentials_changed_at
    ON room_password_credentials(changed_at);

CREATE TRIGGER update_room_password_credentials_updated_at
    BEFORE UPDATE ON room_password_credentials
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();
