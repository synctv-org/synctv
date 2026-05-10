CREATE TABLE IF NOT EXISTS media_provider_instances (
    name VARCHAR(64) PRIMARY KEY,

    endpoint VARCHAR(512) NOT NULL,
    comment TEXT,

    jwt_secret VARCHAR(256),
    custom_ca TEXT,
    timeout VARCHAR(32) NOT NULL DEFAULT '10s',
    tls BOOLEAN NOT NULL DEFAULT false,
    insecure_tls BOOLEAN NOT NULL DEFAULT false,

    providers TEXT[] NOT NULL DEFAULT '{}',

    enabled BOOLEAN NOT NULL DEFAULT true,

    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    CONSTRAINT media_provider_instances_name_not_empty
        CHECK (length(trim(name)) > 0),
    CONSTRAINT media_provider_instances_endpoint_not_empty
        CHECK (length(trim(endpoint)) > 0),
    CONSTRAINT media_provider_instances_valid_jwt_secret_format
        CHECK (jwt_secret IS NULL OR jwt_secret LIKE 'enc:%'),
    CONSTRAINT media_provider_instances_valid_custom_ca_format
        CHECK (custom_ca IS NULL OR custom_ca LIKE 'enc:%')
);

CREATE INDEX IF NOT EXISTS idx_media_provider_instances_enabled ON media_provider_instances(enabled);
CREATE INDEX IF NOT EXISTS idx_media_provider_instances_providers ON media_provider_instances USING gin(providers);
CREATE INDEX IF NOT EXISTS idx_media_provider_instances_endpoint ON media_provider_instances(endpoint);

CREATE TRIGGER update_media_provider_instances_updated_at
    BEFORE UPDATE ON media_provider_instances
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();
