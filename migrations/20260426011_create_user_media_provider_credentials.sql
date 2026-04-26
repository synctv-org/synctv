CREATE TABLE IF NOT EXISTS user_media_provider_credentials (
    id CHAR(12) PRIMARY KEY,

    user_id CHAR(12) NOT NULL,
    provider VARCHAR(32) NOT NULL,

    server_id VARCHAR(64) NOT NULL,

    provider_instance_name VARCHAR(64),

    credential_data JSONB NOT NULL,

    expires_at TIMESTAMPTZ,

    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    CONSTRAINT user_media_provider_credentials_user_fk
        FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE RESTRICT,
    CONSTRAINT user_media_provider_credentials_instance_fk
        FOREIGN KEY (provider_instance_name) REFERENCES media_provider_instances(name) ON DELETE SET NULL,
    CONSTRAINT user_media_provider_credentials_user_provider_server_unique
        UNIQUE(user_id, provider, server_id),
    CONSTRAINT user_media_provider_credentials_server_id_not_empty
        CHECK (length(trim(server_id)) > 0)
);

CREATE INDEX IF NOT EXISTS idx_user_media_provider_credentials_user
    ON user_media_provider_credentials(user_id);
CREATE INDEX IF NOT EXISTS idx_user_media_provider_credentials_provider
    ON user_media_provider_credentials(provider);
CREATE INDEX IF NOT EXISTS idx_user_media_provider_credentials_instance
    ON user_media_provider_credentials(provider_instance_name);
CREATE INDEX IF NOT EXISTS idx_user_media_provider_credentials_expires
    ON user_media_provider_credentials(expires_at)
    WHERE expires_at IS NOT NULL;

CREATE TRIGGER update_user_media_provider_credentials_updated_at
    BEFORE UPDATE ON user_media_provider_credentials
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();

COMMENT ON TABLE user_media_provider_credentials IS 'User credentials for media providers';
COMMENT ON COLUMN user_media_provider_credentials.provider IS 'Media provider type';
COMMENT ON COLUMN user_media_provider_credentials.server_id IS 'Provider-scoped server identifier';
COMMENT ON COLUMN user_media_provider_credentials.provider_instance_name IS 'Associated media provider instance name';
COMMENT ON COLUMN user_media_provider_credentials.credential_data IS 'Provider credential payload';
COMMENT ON COLUMN user_media_provider_credentials.expires_at IS 'Credential expiration timestamp';
COMMENT ON CONSTRAINT user_media_provider_credentials_server_id_not_empty ON user_media_provider_credentials IS
    'server_id must not be empty or whitespace';

CREATE OR REPLACE FUNCTION cleanup_expired_credentials(
    buffer_hours INTEGER DEFAULT 1
) RETURNS JSON AS $$
DECLARE
    deleted_count BIGINT;
BEGIN
    DELETE FROM user_media_provider_credentials
    WHERE expires_at IS NOT NULL
      AND expires_at < CURRENT_TIMESTAMP - (buffer_hours || ' hours')::INTERVAL;

    GET DIAGNOSTICS deleted_count = ROW_COUNT;

    IF deleted_count > 0 THEN
        RAISE NOTICE 'Deleted % expired credentials (buffer: % hours)', deleted_count, buffer_hours;
    END IF;

    RETURN json_build_object(
        'status', 'success',
        'deleted_count', deleted_count,
        'buffer_hours', buffer_hours,
        'cutoff_time', CURRENT_TIMESTAMP - (buffer_hours || ' hours')::INTERVAL
    );
END;
$$ LANGUAGE plpgsql;

COMMENT ON FUNCTION cleanup_expired_credentials(INTEGER) IS
    'Delete expired credentials with a safety buffer and return the deleted count';
