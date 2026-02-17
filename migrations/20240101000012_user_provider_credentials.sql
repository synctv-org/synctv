-- Migration: User Media Provider Credentials
-- Purpose: Store user credentials for media providers (Bilibili, Alist, Emby)

CREATE TABLE user_media_provider_credentials (
    -- Primary Key
    id CHAR(12) PRIMARY KEY,  -- nanoid(12)

    -- User and Provider
    user_id CHAR(12) NOT NULL,  -- nanoid(12)
    provider VARCHAR(32) NOT NULL,  -- bilibili, alist, emby

    -- Server Identifier (required, distinguishes different servers/accounts)
    server_id VARCHAR(64) NOT NULL,  -- Alist/Emby: MD5(host), Bilibili: "bilibili" or account id

    -- Associated Provider Instance (optional)
    provider_instance_name VARCHAR(64),

    -- Credential Data (JSONB, encrypted at rest via AES-256-GCM when encryption key is configured)
    -- Format: "enc:<base64(nonce+ciphertext)>" for encrypted, raw JSON for legacy plaintext
    credential_data JSONB NOT NULL,

    -- Expiration Time (optional, for tokens/cookies with TTL)
    expires_at TIMESTAMPTZ,

    -- Audit
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- Constraints
    CONSTRAINT fk_user FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
    CONSTRAINT fk_media_provider_instance FOREIGN KEY (provider_instance_name) REFERENCES media_provider_instances(name) ON DELETE SET NULL,
    CONSTRAINT unique_user_media_provider_server UNIQUE(user_id, provider, server_id),
    CONSTRAINT valid_server_id CHECK (length(trim(server_id)) > 0 AND length(server_id) <= 64)
);

-- Indexes
CREATE INDEX idx_user_media_provider_credentials_user ON user_media_provider_credentials(user_id);
CREATE INDEX idx_user_media_provider_credentials_provider ON user_media_provider_credentials(provider);
CREATE INDEX idx_user_media_provider_credentials_instance ON user_media_provider_credentials(provider_instance_name);
CREATE INDEX idx_user_media_provider_credentials_expires ON user_media_provider_credentials(expires_at) WHERE expires_at IS NOT NULL;

-- Updated At Trigger
CREATE TRIGGER update_user_media_provider_credentials_updated_at
    BEFORE UPDATE ON user_media_provider_credentials
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();

-- Comments
COMMENT ON TABLE user_media_provider_credentials IS 'User credentials for media providers. Credential data is encrypted at rest when encryption key is configured.';
COMMENT ON COLUMN user_media_provider_credentials.provider IS 'Media provider type (bilibili, alist, emby)';
COMMENT ON COLUMN user_media_provider_credentials.server_id IS 'Server identifier (required): Bilibili uses "bilibili" (one per user), Alist/Emby use MD5(host)';
COMMENT ON COLUMN user_media_provider_credentials.provider_instance_name IS 'Associated media provider instance name (optional, for specifying parsing instance)';
COMMENT ON COLUMN user_media_provider_credentials.credential_data IS 'Credential data (JSONB). Encrypted at rest via AES-256-GCM when SYNCTV_CREDENTIAL_ENCRYPTION_KEY is configured.';
COMMENT ON COLUMN user_media_provider_credentials.expires_at IS 'Credential expiration time (optional, for tokens/cookies with TTL)';
COMMENT ON CONSTRAINT valid_server_id ON user_media_provider_credentials IS 'server_id must not be empty or whitespace';
COMMENT ON CONSTRAINT unique_user_media_provider_server ON user_media_provider_credentials IS 'User can only have one credential per provider per server (Bilibili: one per user, Alist/Emby: multiple allowed)';

-- ============================================================================
-- Expired Credential Cleanup
-- ============================================================================

-- Function: Delete expired credentials
-- This function deletes credentials that have expired based on their expires_at timestamp
CREATE OR REPLACE FUNCTION cleanup_expired_credentials(
    buffer_hours INTEGER DEFAULT 1
) RETURNS JSON AS $$
DECLARE
    deleted_count BIGINT;
BEGIN
    -- Delete credentials that expired more than buffer_hours ago
    -- Buffer prevents race conditions where credentials expire during active requests
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
'Delete expired credentials with a safety buffer (default: 1 hour). Returns deleted count. Should be called periodically (e.g., hourly cron job).';
