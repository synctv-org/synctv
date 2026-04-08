CREATE TABLE IF NOT EXISTS oauth2_clients (
    id CHAR(12) PRIMARY KEY,
    provider VARCHAR(50) NOT NULL,
    provider_user_id VARCHAR(255) NOT NULL,
    user_id CHAR(12) NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    username VARCHAR(255) NOT NULL,
    email VARCHAR(255),
    avatar_url TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(provider, provider_user_id)
);

CREATE INDEX idx_oauth2_clients_user_id ON oauth2_clients(user_id);
CREATE INDEX idx_oauth2_clients_provider ON oauth2_clients(provider);

CREATE TRIGGER update_oauth2_clients_updated_at
    BEFORE UPDATE ON oauth2_clients
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

ALTER TABLE oauth2_clients
    ADD CONSTRAINT valid_oauth2_provider CHECK (
        provider IN ('qq', 'github', 'google', 'microsoft', 'discord', 'casdoor', 'logto', 'oidc', 'feishu', 'gitee')
    );

COMMENT ON TABLE oauth2_clients IS 'OAuth2/OIDC provider mappings (NO TOKENS - only user identity info)';
COMMENT ON COLUMN oauth2_clients.id IS '12-character base62 ID';
COMMENT ON COLUMN oauth2_clients.provider IS 'OAuth2 provider (github, google, microsoft, discord, etc.)';
COMMENT ON COLUMN oauth2_clients.provider_user_id IS 'User ID from OAuth2 provider';
COMMENT ON COLUMN oauth2_clients.user_id IS 'Reference to local user';
COMMENT ON COLUMN oauth2_clients.username IS 'Username from OAuth2 provider';
COMMENT ON COLUMN oauth2_clients.email IS 'Email from OAuth2 provider (optional)';
COMMENT ON COLUMN oauth2_clients.avatar_url IS 'Avatar URL from OAuth2 provider (optional)';
