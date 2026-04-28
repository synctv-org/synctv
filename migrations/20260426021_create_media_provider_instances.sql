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

COMMENT ON TABLE media_provider_instances IS 'Media provider gRPC instance configurations';
COMMENT ON COLUMN media_provider_instances.name IS 'Instance name';
COMMENT ON COLUMN media_provider_instances.endpoint IS 'gRPC service endpoint';
COMMENT ON COLUMN media_provider_instances.jwt_secret IS 'JWT secret for provider authentication';
COMMENT ON COLUMN media_provider_instances.custom_ca IS 'Custom CA certificate';
COMMENT ON COLUMN media_provider_instances.timeout IS 'Request timeout';
COMMENT ON COLUMN media_provider_instances.tls IS 'Enable TLS';
COMMENT ON COLUMN media_provider_instances.insecure_tls IS 'Skip TLS certificate verification';
COMMENT ON COLUMN media_provider_instances.providers IS 'Supported media provider types';
COMMENT ON CONSTRAINT media_provider_instances_name_not_empty ON media_provider_instances IS
    'Name must not be empty or whitespace';
COMMENT ON CONSTRAINT media_provider_instances_endpoint_not_empty ON media_provider_instances IS
    'Endpoint must not be empty or whitespace';
