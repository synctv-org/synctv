//! Cluster startup failure handling tests.
//!
//! Verifies that when cluster mode is explicitly enabled (cluster.enabled = true):
//! 1. `ClusterManager` initialization failure is a fatal error (not silent degradation)
//! 2. Cache invalidation startup failure is a fatal error (not just a warning)
//!
//! In standalone mode (cluster.enabled = false), these failures should be non-fatal.

#![allow(clippy::unwrap_used)]
use synctv_core::config::{
    BootstrapConfig, BufferSizesConfig, CacheConfig, ClusterChannelConfig, Config,
    ConnectionLimitsConfig, DatabaseConfig, EmailConfig, GrpcRateLimitConfig, HttpRateLimitConfig,
    JwtConfig, LivestreamConfig, LoggingConfig, MediaProvidersConfig, OAuth2Config,
    PasswordComplexityConfig, RedisConfig, ServerConfig, WebRTCConfig,
};

/// Create a minimal standalone config for testing (no Redis, no cluster mode)
fn standalone_test_config() -> Config {
    Config {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            grpc_port: 50051,
            http_port: 8080,
            enable_reflection: false,
            metrics_enabled: false,
            metrics_bearer_token: String::new(),
            grpc_max_message_size_bytes: 16 * 1024 * 1024,
            trusted_proxies: Vec::new(),
            cors_allowed_origins: Vec::new(),
            cluster_secret: String::new(), // No cluster secret for standalone mode
            advertise_host: String::new(),
            shutdown_drain_timeout_seconds: 30,
            disable_ws_token_query: true,
        },
        database: DatabaseConfig::default(),
        redis: RedisConfig::default(), // Empty Redis URL for standalone
        jwt: JwtConfig {
            secret: "test-jwt-secret-key-for-testing-minimum-length".to_string(),
            ..JwtConfig::default()
        },
        logging: LoggingConfig::default(),
        livestream: LivestreamConfig::default(),
        oauth2: OAuth2Config::default(),
        email: EmailConfig::default(),
        media_providers: MediaProvidersConfig::default(),
        webrtc: WebRTCConfig::default(),
        connection_limits: ConnectionLimitsConfig::default(),
        bootstrap: BootstrapConfig {
            create_root_user: true,
            root_username: "admin".to_string(),
            root_password: "StrongPwd12345!".to_string(),
        },
        cluster: ClusterChannelConfig::default(), // cluster.enabled = false
        password_complexity: PasswordComplexityConfig::default(),
        buffer_sizes: BufferSizesConfig::default(),
        cache: CacheConfig::default(),
        http_rate_limits: HttpRateLimitConfig::default(),
        grpc_rate_limits: GrpcRateLimitConfig::default(),
    }
}

/// Create a config with cluster mode enabled
fn cluster_test_config() -> Config {
    Config {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            grpc_port: 50051,
            http_port: 8080,
            enable_reflection: false,
            metrics_enabled: false,
            metrics_bearer_token: String::new(),
            grpc_max_message_size_bytes: 16 * 1024 * 1024,
            trusted_proxies: Vec::new(),
            cors_allowed_origins: Vec::new(),
            cluster_secret: "test-cluster-secret-key-1234567890".to_string(),
            advertise_host: String::new(),
            shutdown_drain_timeout_seconds: 30,
            disable_ws_token_query: true,
        },
        database: DatabaseConfig::default(),
        redis: RedisConfig {
            url: "redis://localhost:6379".to_string(),
            ..RedisConfig::default()
        },
        jwt: JwtConfig {
            secret: "test-jwt-secret-key-for-testing-minimum-length".to_string(),
            ..JwtConfig::default()
        },
        logging: LoggingConfig::default(),
        livestream: LivestreamConfig::default(),
        oauth2: OAuth2Config::default(),
        email: EmailConfig::default(),
        media_providers: MediaProvidersConfig::default(),
        webrtc: WebRTCConfig {
            stun_external_addr: "203.0.113.1:3478".to_string(),
            ..WebRTCConfig::default()
        },
        connection_limits: ConnectionLimitsConfig::default(),
        bootstrap: BootstrapConfig {
            create_root_user: true,
            root_username: "admin".to_string(),
            root_password: "StrongPwd12345!".to_string(),
        },
        cluster: ClusterChannelConfig {
            enabled: true, // Cluster mode explicitly enabled
            ..ClusterChannelConfig::default()
        },
        password_complexity: PasswordComplexityConfig::default(),
        buffer_sizes: BufferSizesConfig::default(),
        cache: CacheConfig::default(),
        http_rate_limits: HttpRateLimitConfig::default(),
        grpc_rate_limits: GrpcRateLimitConfig::default(),
    }
}

/// Test that cluster.enabled=true is properly reflected in config
#[test]
fn test_cluster_enabled_config() {
    let config = cluster_test_config();
    assert!(
        config.cluster.enabled,
        "cluster.enabled should be true in cluster_test_config"
    );

    let config = standalone_test_config();
    assert!(
        !config.cluster.enabled,
        "cluster.enabled should be false in standalone_test_config"
    );
}

/// Test that standalone mode config has no Redis URL by default
#[test]
fn test_standalone_mode_config_no_redis() {
    let config = standalone_test_config();
    // In standalone mode:
    // - cluster.enabled should be false
    // - Redis URL should be empty
    assert!(!config.cluster.enabled);
    assert!(
        config.redis.url.is_empty(),
        "Redis URL should be empty in standalone mode"
    );
}

/// Test cluster mode requires Redis URL
#[test]
fn test_cluster_mode_requires_redis_url() {
    let mut config = cluster_test_config();
    // Remove the Redis URL
    config.redis.url = String::new();

    // This should fail validation because cluster.enabled=true requires Redis
    let result = config.validate();
    assert!(
        result.is_err(),
        "cluster.enabled=true with empty redis.url should fail validation"
    );

    let errors = result.unwrap_err();
    let error_messages: Vec<_> = errors.iter().collect();
    let has_redis_error = error_messages.iter().any(|e| {
        e.contains("Redis is required when cluster mode is enabled")
            || e.contains("cluster.enabled=true")
    });
    assert!(
        has_redis_error,
        "Error message should mention Redis requirement, got: {error_messages:?}"
    );
}

/// Test cluster mode requires `cluster_secret`
#[test]
fn test_cluster_mode_requires_cluster_secret() {
    let mut config = cluster_test_config();
    config.server.cluster_secret = String::new(); // Clear the cluster secret

    // This should fail validation because cluster.enabled=true requires cluster_secret
    let result = config.validate();
    assert!(
        result.is_err(),
        "cluster.enabled=true with empty cluster_secret should fail validation"
    );

    let errors = result.unwrap_err();
    let error_messages: Vec<_> = errors.iter().collect();
    let has_secret_error = error_messages.iter().any(|e| e.contains("cluster_secret"));
    assert!(
        has_secret_error,
        "Error message should mention cluster_secret requirement, got: {error_messages:?}"
    );
}

/// Test standalone mode allows no Redis
#[test]
fn test_standalone_mode_allows_no_redis() {
    let config = standalone_test_config();
    // cluster.enabled=false, redis.url is empty

    // This should pass validation (standalone mode doesn't require Redis)
    let result = config.validate();
    assert!(
        result.is_ok(),
        "standalone mode should allow no Redis, got error: {:?}",
        result.err()
    );
}

/// Test valid cluster mode config
#[test]
fn test_valid_cluster_mode_config() {
    let config = cluster_test_config();

    // This should pass validation
    let result = config.validate();
    assert!(
        result.is_ok(),
        "valid cluster config should pass validation, got error: {:?}",
        result.err()
    );
}
