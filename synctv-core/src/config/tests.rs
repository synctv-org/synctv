use super::*;
use crate::test_helpers::{TestOptionExt, TestResultExt};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use tempfile::tempdir;

fn env_map(entries: &[(&str, &str)]) -> HashMap<String, String> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

#[test]
fn test_large_public_service_capacity_defaults() {
    let connection_limits = ConnectionLimitsConfig::default();
    assert_eq!(connection_limits.max_per_user, 20);
    assert_eq!(connection_limits.max_per_room, 2000);
    assert_eq!(connection_limits.max_total, 100_000);
    assert_eq!(connection_limits.ws_message_rate_limit_per_second, 50);

    let cluster = ClusterChannelConfig::default();
    assert_eq!(cluster.critical_channel_capacity, 10_000);
    assert_eq!(cluster.publish_channel_capacity, 100_000);
    assert_eq!(cluster.stream_max_length, 100_000);

    let cache = CacheConfig::default();
    assert_eq!(cache.l1_capacity, 5000);
    assert_eq!(cache.username_cache_capacity, 10_000);
    assert_eq!(cache.permission_cache_capacity, 20_000);
}

#[test]
fn test_ssrf_guard_is_disabled_by_default() {
    let config = Config::default();
    let guard = config.security.ssrf_guard();

    assert!(!config.security.ssrf.enabled);
    assert!(guard.acl().is_none());
    assert!(!guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::LOCALHOST)));
}

#[test]
fn test_ssrf_enabled_env_builds_strict_guard() {
    let config =
        Config::load_with_env_map(None, &env_map(&[("SYNCTV_SECURITY_SSRF_ENABLED", "true")]))
            .checked("SSRF enabled env should load");
    let guard = config.security.ssrf_guard();

    assert!(config.security.ssrf.enabled);
    assert!(guard.acl().is_some());
    assert!(guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::LOCALHOST)));
    assert!(!guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
}

#[test]
fn test_ssrf_env_supports_enabled_and_allowlists() {
    let config = Config::load_with_env_map(
        None,
        &env_map(&[
            ("SYNCTV_SECURITY_SSRF_ENABLED", "true"),
            ("SYNCTV_SECURITY_SSRF_ALLOWED_HOSTS", "alist.internal"),
            (
                "SYNCTV_SECURITY_SSRF_ALLOWED_IP_RANGES",
                "127.0.0.1/32,10.0.8.0/24",
            ),
        ]),
    )
    .checked("SSRF allowlist env should load");
    let guard = config.security.ssrf_guard();

    assert!(config.security.ssrf.enabled);
    assert_eq!(
        config.security.ssrf.allowed_hosts,
        vec!["alist.internal".to_string()]
    );
    assert_eq!(
        config.security.ssrf.allowed_ip_ranges,
        vec!["127.0.0.1/32".to_string(), "10.0.8.0/24".to_string()]
    );
    assert!(!guard.is_host_blocked("alist.internal"));
    assert!(!guard.is_ip_blocked(&IpAddr::V4(Ipv4Addr::LOCALHOST)));
}

#[test]
fn test_grpc_message_size_validation() {
    let mut config = valid_prod_config();

    // Valid: within range
    config.server.grpc_max_message_size_bytes = 8 * 1024 * 1024; // 8 MB
    assert!(config.validate().is_ok());

    // Valid: minimum (1 MB)
    config.server.grpc_max_message_size_bytes = 1024 * 1024;
    assert!(config.validate().is_ok());

    // Valid: maximum (1 GB)
    config.server.grpc_max_message_size_bytes = 1024 * 1024 * 1024;
    assert!(config.validate().is_ok());

    // Invalid: below minimum
    config.server.grpc_max_message_size_bytes = 1024 * 1024 - 1; // Just under 1 MB
    let errors = config.validate().failed("operation should fail");
    assert!(errors
        .iter()
        .any(|e| e.contains("grpc_max_message_size_bytes") && e.contains("1 MB")));

    // Invalid: above maximum
    config.server.grpc_max_message_size_bytes = 1024 * 1024 * 1024 + 1; // Just over 1 GB
    let errors = config.validate().failed("operation should fail");
    assert!(errors
        .iter()
        .any(|e| e.contains("grpc_max_message_size_bytes") && e.contains("1 GB")));
}

#[test]
fn test_validate_rejects_cors_origin_with_path() {
    let mut config = valid_prod_config();
    config.server.cors_allowed_origins = vec!["https://app.example.com/foo".to_string()];

    let errors = config
        .validate()
        .failed("CORS origins with paths must be rejected during config validation");

    assert!(
        errors
            .iter()
            .any(|e| e.contains("cors origin") || e.contains("CORS origin")),
        "unexpected errors: {errors:?}"
    );
    assert!(
        errors.iter().any(|e| e.contains("must not include a path")),
        "unexpected errors: {errors:?}"
    );
}

#[test]
fn test_grpc_reflection_defaults_disabled() {
    let config = Config::default();
    assert!(!config.server.enable_reflection);
    assert!(!config.management.enable_reflection);
}

#[test]
fn test_api_address() {
    let config = Config {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 8080,
            enable_reflection: true,
            grpc_max_message_size_bytes: 16 * 1024 * 1024,
            grpc_compression_enabled: true,
            trusted_proxies: Vec::new(),
            cors_allowed_origins: Vec::new(),
            advertise_host: String::new(),
            shutdown_drain_timeout_seconds: 30,
        },
        time: TimeConfig::default(),
        public_ids: PublicIdsConfig::default(),
        security: SecurityConfig {
            opaque_server_setup_secret: "test-opaque-server-setup-secret-that-is-long-enough"
                .to_string(),
            ..SecurityConfig::default()
        },
        data_dir: default_data_dir().display().to_string(),
        metrics: MetricsConfig::default(),
        management: ManagementConfig::default(),
        database: DatabaseConfig::default(),
        redis: RedisConfig::default(),
        jwt: JwtConfig::default(),
        logging: LoggingConfig::default(),
        livestream: LivestreamConfig::default(),
        file_storage: FileStorageConfig::default(),
        chat: ChatConfig::default(),
        webauthn: WebAuthnConfig::default(),
        media_providers: MediaProvidersConfig::default(),
        webrtc: WebRTCConfig::default(),
        connection_limits: ConnectionLimitsConfig::default(),
        bootstrap: BootstrapConfig::default(),
        cluster: ClusterChannelConfig::default(),
        password_complexity: PasswordComplexityConfig::default(),
        buffer_sizes: BufferSizesConfig::default(),
        cache: CacheConfig::default(),
        proxy_slice_cache: ProxySliceCacheConfig::default(),
        messaging_rate_limits: MessagingRateLimitConfig::default(),
        request_rate_limits: RequestRateLimitConfig::default(),
    };

    assert_eq!(config.api_address(), "127.0.0.1:8080");
}

#[test]
fn test_metrics_address() {
    let config = Config {
        server: ServerConfig::default(),
        time: TimeConfig::default(),
        public_ids: PublicIdsConfig::default(),
        security: SecurityConfig::default(),
        data_dir: default_data_dir().display().to_string(),
        metrics: MetricsConfig {
            enabled: true,
            host: "127.0.0.1".to_string(),
            port: 9090,
            tls: MetricsTlsConfig::default(),
            auth: MetricsAuthConfig {
                bearer_token: "metrics-secret".to_string(),
                ..MetricsAuthConfig::default()
            },
        },
        management: ManagementConfig::default(),
        database: DatabaseConfig::default(),
        redis: RedisConfig::default(),
        jwt: JwtConfig::default(),
        logging: LoggingConfig::default(),
        livestream: LivestreamConfig::default(),
        file_storage: FileStorageConfig::default(),
        chat: ChatConfig::default(),
        webauthn: WebAuthnConfig::default(),
        media_providers: MediaProvidersConfig::default(),
        webrtc: WebRTCConfig::default(),
        connection_limits: ConnectionLimitsConfig::default(),
        bootstrap: BootstrapConfig::default(),
        cluster: ClusterChannelConfig::default(),
        password_complexity: PasswordComplexityConfig::default(),
        buffer_sizes: BufferSizesConfig::default(),
        cache: CacheConfig::default(),
        proxy_slice_cache: ProxySliceCacheConfig::default(),
        messaging_rate_limits: MessagingRateLimitConfig::default(),
        request_rate_limits: RequestRateLimitConfig::default(),
    };

    assert_eq!(config.metrics_address(), "127.0.0.1:9090");
}

#[test]
fn test_advertise_host_prefers_explicit_config_over_env() {
    let mut config = valid_prod_config();
    config.server.advertise_host = "10.1.2.3".to_string();

    assert_eq!(
        config.advertise_host_with_env_map(&env_map(&[("POD_IP", "10.0.0.99")])),
        "10.1.2.3"
    );
}

#[test]
fn test_advertise_host_uses_pod_ip_before_hostname() {
    let config = valid_prod_config();

    assert_eq!(
        config.advertise_host_with_env_map(&env_map(&[("POD_IP", "10.2.3.4")])),
        "10.2.3.4"
    );
}

#[test]
fn test_advertise_host_falls_back_to_hostname_without_pod_ip() {
    let config = valid_prod_config();
    let advertise_host = config.advertise_host_with_env_map(&HashMap::new());

    assert!(
        !advertise_host.is_empty(),
        "hostname fallback should produce a non-empty advertise host"
    );
    assert_ne!(advertise_host, "0.0.0.0");
}

#[test]
fn test_cluster_mode_rejects_unroutable_advertise_host() {
    let mut config = valid_prod_config();
    config.cluster.enabled = true;
    config.server.advertise_host = "0.0.0.0".to_string();

    let errors = config
        .validate_with_env_map(&HashMap::new())
        .failed("cluster mode must reject unroutable advertise_host");

    assert!(
        errors.iter().any(|e| e.contains("server.advertise_host")),
        "unexpected errors: {errors:?}"
    );
}

#[test]
fn test_cluster_mode_requires_explicit_routable_advertise_host_source() {
    let mut config = valid_prod_config();
    config.cluster.enabled = true;
    config.server.advertise_host.clear();

    let errors = config
        .validate_with_env_map(&HashMap::new())
        .failed("cluster mode must not fall back to implicit hostname advertise address");

    assert!(
        errors.iter().any(|e| {
            e.contains("server.advertise_host")
                && e.contains("SYNCTV_SERVER_ADVERTISE_HOST")
                && e.contains("POD_IP")
        }),
        "unexpected errors: {errors:?}"
    );
}

#[test]
fn test_cluster_mode_accepts_pod_ip_as_explicit_advertise_host_source() {
    let mut config = valid_prod_config();
    config.cluster.enabled = true;
    config.server.advertise_host.clear();

    config
        .validate_with_env_map(&env_map(&[("POD_IP", "10.2.3.4")]))
        .checked("cluster mode should accept POD_IP as the explicit advertise host source");
}

#[test]
fn test_from_env_rejects_invalid_numeric_override() {
    let error = Config::from_env_map(&env_map(&[("SYNCTV_SERVER_PORT", "not-a-port")]))
        .failed("invalid numeric override must fail closed");

    let message = error.to_string();
    assert!(message.contains("SYNCTV_SERVER_PORT"));
    assert!(message.contains("not-a-port"));
}

#[test]
fn test_load_with_explicit_missing_config_path_fails_closed() {
    let unique = format!(
        "synctv-missing-explicit-config-{}-{}.yaml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .checked("system time before unix epoch")
            .as_nanos()
    );
    let path = std::env::temp_dir().join(unique);

    let error = Config::load_with_env_map(Some(path.to_str().checked("utf-8 path")), &env_map(&[]))
        .failed("explicit missing config path must not fall back to defaults");

    let message = error.to_string();
    assert!(
        message.contains("config file not found"),
        "missing file error should report the missing config file: {message}"
    );
}

#[test]
fn test_from_env_rejects_invalid_boolean_override() {
    let error = Config::from_env_map(&env_map(&[("SYNCTV_METRICS_ENABLED", "maybe")]))
        .failed("invalid boolean override must fail closed");

    let message = error.to_string();
    assert!(message.contains("SYNCTV_METRICS_ENABLED"));
    assert!(message.contains("maybe"));
}

#[test]
fn test_from_env_rejects_invalid_redis_deployment_mode_override() {
    let error = Config::from_env_map(&env_map(&[("SYNCTV_REDIS_DEPLOYMENT_MODE", "sentinal")]))
        .failed("invalid redis deployment mode override must fail closed");

    let message = error.to_string();
    assert!(message.contains("SYNCTV_REDIS_DEPLOYMENT_MODE"));
    assert!(message.contains("sentinal"));
}

#[test]
fn test_from_env_rejects_unsupported_redis_cluster_mode_override() {
    let error = Config::from_env_map(&env_map(&[("SYNCTV_REDIS_DEPLOYMENT_MODE", "cluster")]))
        .failed("unsupported redis cluster mode override must fail closed");

    let message = error.to_string();
    assert!(message.contains("SYNCTV_REDIS_DEPLOYMENT_MODE"));
    assert!(message.contains("cluster"));
    assert!(message.contains("standalone"));
    assert!(message.contains("sentinel"));
}

#[test]
fn test_from_env_rejects_invalid_webrtc_mode_override() {
    let error = Config::from_env_map(&env_map(&[("SYNCTV_WEBRTC_MODE", "p2p")]))
        .failed("invalid webrtc mode override must fail closed");

    let message = error.to_string();
    assert!(message.contains("SYNCTV_WEBRTC_MODE"));
    assert!(message.contains("p2p"));
}

#[test]
fn test_from_env_rejects_invalid_hls_storage_backend_override() {
    let error = Config::from_env_map(&env_map(&[(
        "SYNCTV_LIVESTREAM_HLS_STORAGE_BACKEND",
        "nfs",
    )]))
    .failed("invalid HLS storage backend override must fail closed");

    let message = error.to_string();
    assert!(message.contains("SYNCTV_LIVESTREAM_HLS_STORAGE_BACKEND"));
    assert!(message.contains("memory"));
    assert!(message.contains("file"));
    assert!(message.contains("shared_file"));
    assert!(message.contains("oss"));
}

#[test]
fn test_from_env_rejects_unknown_server_port_env_vars() {
    let error = Config::from_env_map(&env_map(&[
        ("SYNCTV_SERVER_GRPC_PORT", "50051"),
        ("SYNCTV_SERVER_HTTP_PORT", "8080"),
        ("SYNCTV_SERVER_PORT", "18080"),
    ]))
    .failed("unknown split-port env vars must fail fast");

    let message = error.to_string();
    assert!(message.contains("SYNCTV_SERVER_GRPC_PORT"));
    assert!(message.contains("SYNCTV_SERVER_HTTP_PORT"));
}

#[test]
fn test_from_env_rejects_unknown_database_and_redis_keys() {
    let error = Config::from_env_map(&env_map(&[
        ("SYNCTV_DATABASE_UNKNOWN_KEY", "synctv"),
        ("SYNCTV_REDIS_UNKNOWN_KEY", "cache-user"),
    ]))
    .failed("unknown nested env keys must fail fast");

    let message = error.to_string();
    assert!(message.contains("SYNCTV_DATABASE_UNKNOWN_KEY"));
    assert!(message.contains("SYNCTV_REDIS_UNKNOWN_KEY"));
}

#[test]
fn test_collect_unknown_synctv_env_vars_only_returns_unhandled_synctv_keys() {
    let env = env_map(&[
        ("SYNCTV_SERVER_PORT", "18080"),
        ("SYNCTV_UNKNOWN_FLAG", "1"),
        ("SYNCTV_ANOTHER_UNKNOWN", "2"),
        ("SYNCTV_MANAGEMENT_ENDPOINT", "unix:///tmp/synctv.sock"),
        ("SYNCTV_TEST_DOCKER_STARTUP_TIMEOUT_SECS", "600"),
        ("PATH", "/usr/bin"),
    ]);
    let seen = std::collections::HashSet::from(["SYNCTV_SERVER_PORT".to_string()]);

    let unknown = Config::collect_unknown_synctv_env_vars(&env, &seen);

    assert_eq!(
        unknown,
        vec![
            "SYNCTV_ANOTHER_UNKNOWN".to_string(),
            "SYNCTV_UNKNOWN_FLAG".to_string()
        ]
    );
}

#[test]
fn test_inspect_unknowns_with_env_map_reports_file_and_env_unknowns() {
    let secret = "12345678901234567890123456789012";
    let unique = format!(
        "synctv-config-inspect-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .checked("system time before unix epoch")
            .as_nanos()
    );
    let path = std::env::temp_dir().join(unique);
    std::fs::write(
        &path,
        format!(r#"{{"jwt":{{"secret":"{secret}"}},"metrics":{{"obsolete_token":"ignored"}}}}"#),
    )
    .checked("write config fixture");

    let diagnostics = Config::inspect_unknowns_with_env_map(
        Some(path.to_str().checked("utf-8 path")),
        &env_map(&[("SYNCTV_UNKNOWN_FLAG", "1")]),
        None,
    )
    .checked("unknown inspection should still parse known config");
    let _ = std::fs::remove_file(&path);

    assert_eq!(
        diagnostics.config_keys,
        vec!["metrics.obsolete_token".to_string()]
    );
    assert_eq!(
        diagnostics.env_keys,
        vec!["SYNCTV_UNKNOWN_FLAG".to_string()]
    );
    assert!(diagnostics
        .strict_error_message()
        .contains("metrics.obsolete_token"));
}

#[test]
fn test_public_ids_default_to_prefixed_decimal_ids() {
    let config = Config::from_env_map(&HashMap::new()).checked("default config should load");
    let codec = crate::PublicIdCodec::from_config(&config.public_ids)
        .checked("default public IDs config should be valid");

    assert!(config.public_ids.sqids.is_none());
    assert_eq!(
        codec
            .encode_user_id(crate::models::UserId::expect_positive(1))
            .checked("user ID should encode"),
        "usr_1"
    );
}

#[test]
fn test_public_ids_sqids_env_enables_prefixed_sqids() {
    let config = Config::from_env_map(&env_map(&[("SYNCTV_PUBLIC_IDS_SQIDS_MIN_LENGTH", "8")]))
        .checked("sqids env config should load");
    let codec = crate::PublicIdCodec::from_config(&config.public_ids)
        .checked("sqids public IDs config should be valid");
    let encoded = codec
        .encode_user_id(crate::models::UserId::expect_positive(1))
        .checked("user ID should encode");

    assert_eq!(
        config
            .public_ids
            .sqids
            .as_ref()
            .checked("sqids should be enabled")
            .min_length,
        8
    );
    assert!(encoded.starts_with("usr_"));
    assert_ne!(encoded, "usr_1");
    assert_eq!(
        codec
            .decode_user_id(&encoded)
            .checked("user ID should decode"),
        crate::models::UserId::expect_positive(1)
    );
}

#[test]
fn test_checked_in_yaml_configs_deserialize() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .checked("synctv-core should be inside the workspace root");

    let config_file = "synctv.example.yaml";
    let path = workspace_root.join(config_file);
    let path_str = path
        .to_str()
        .checked("checked-in config path should be valid UTF-8");
    Config::load_config_file(path_str).map_or_else(
        |error| {
            std::panic::panic_any(format!("{config_file} should deserialize: {error}"));
        },
        |(config, _)| config,
    );
    let unknown_keys = Config::collect_unknown_config_file_keys(path_str).unwrap_or_else(|error| {
        std::panic::panic_any(format!("{config_file} unknown-key scan failed: {error}"));
    });
    assert!(
        unknown_keys.is_empty(),
        "{config_file} should not contain unsupported keys: {unknown_keys:?}"
    );
}

/// Helper to create a valid production config for validation tests
fn valid_prod_config() -> Config {
    Config {
        server: ServerConfig {
            host: "0.0.0.0".to_string(),
            port: 8080,
            grpc_max_message_size_bytes: 16 * 1024 * 1024,
            grpc_compression_enabled: true,
            enable_reflection: false,
            trusted_proxies: Vec::new(),
            cors_allowed_origins: Vec::new(),
            advertise_host: String::new(),
            shutdown_drain_timeout_seconds: 30,
        },
        time: TimeConfig::default(),
        public_ids: PublicIdsConfig::default(),
        security: SecurityConfig {
            opaque_server_setup_secret: "test-opaque-server-setup-secret-that-is-long-enough"
                .to_string(),
            ..SecurityConfig::default()
        },
        data_dir: default_data_dir().display().to_string(),
        metrics: MetricsConfig::default(),
        management: ManagementConfig {
            auth_token: "test-management-auth-token".to_string(),
            ..ManagementConfig::default()
        },
        database: DatabaseConfig::default(),
        redis: RedisConfig {
            url: "redis://localhost:6379".to_string(),
            ..RedisConfig::default()
        },
        jwt: JwtConfig {
            secret: "my-very-secret-production-key-that-is-long-enough".to_string(),
            ..JwtConfig::default()
        },
        logging: LoggingConfig::default(),
        livestream: LivestreamConfig {
            // Keep a valid shared file backend so cluster-mode tests can opt in by
            // toggling `cluster.enabled` without unrelated HLS path errors.
            hls_storage_backend: HlsStorageBackend::SharedFile,
            hls_storage_path: "/var/lib/synctv/hls".to_string(),
            ..LivestreamConfig::default()
        },
        file_storage: FileStorageConfig::default(),
        chat: ChatConfig::default(),
        webauthn: WebAuthnConfig::default(),
        media_providers: MediaProvidersConfig::default(),
        webrtc: WebRTCConfig {
            // Keep a valid external STUN address so cluster-mode tests can opt in by
            // toggling `cluster.enabled` without additional changes.
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
            secret: "test-cluster-secret-for-validation".to_string(),
            ..ClusterChannelConfig::default()
        },
        password_complexity: PasswordComplexityConfig::default(),
        buffer_sizes: BufferSizesConfig::default(),
        cache: CacheConfig::default(),
        proxy_slice_cache: ProxySliceCacheConfig::default(),
        messaging_rate_limits: MessagingRateLimitConfig::default(),
        request_rate_limits: RequestRateLimitConfig::default(),
    }
}

#[test]
fn test_validate_valid_production_config() {
    let config = valid_prod_config();
    assert!(config.validate().is_ok());
}

#[test]
fn test_validate_cluster_mode_allows_local_hls_storage() {
    // In distributed mode, local HLS backends are allowed because non-publisher
    // nodes proxy playlist/segment reads to the publisher node.
    let mut config = valid_prod_config();
    config.cluster.enabled = true;
    config.server.advertise_host = "10.0.0.12".to_string();
    config.livestream.hls_storage_backend = HlsStorageBackend::File;
    assert!(config.validate().is_ok());

    config.livestream.hls_storage_backend = HlsStorageBackend::Memory;
    config.livestream.hls_storage_path = String::new();
    assert!(config.validate().is_ok());
}

#[test]
fn test_validate_standalone_mode_allows_hls_local_storage() {
    // In standalone mode (no cluster.secret and cluster.enabled=false),
    // local file storage should be allowed.
    let mut config = valid_prod_config();
    // Disable cluster mode by clearing cluster.secret and ensuring cluster.enabled is false.
    config.cluster.secret = String::new();
    config.cluster.enabled = false;
    // Remove Redis to ensure cluster mode is fully disabled
    config.redis.url = String::new();
    // Also need to clear stun_external_addr since standalone mode no longer
    // requires an external STUN address.
    config.webrtc.stun_external_addr = String::new();
    config.livestream.hls_storage_backend = HlsStorageBackend::File;
    // This should pass validation (only a warning is logged)
    assert!(config.validate().is_ok());
}

#[test]
fn test_validate_single_api_port_is_allowed() {
    let mut config = valid_prod_config();
    config.server.port = 8080;
    config.validate().checked("single API port should be valid");
}

#[test]
fn test_validate_port_conflict_rtmp_http() {
    let mut config = valid_prod_config();
    config.livestream.rtmp_port = 8080;
    let errors = config.validate().failed("operation should fail");
    assert!(errors
        .iter()
        .any(|e| e.contains("rtmp_port") && e.contains("server.port")));
}

#[test]
fn test_validate_zero_port() {
    let mut config = valid_prod_config();
    config.server.port = 0;
    let errors = config.validate().failed("operation should fail");
    assert!(errors
        .iter()
        .any(|e| e.contains("server.port") && e.contains('0')));
}

#[test]
fn test_validate_default_jwt_secret_production() {
    let mut config = valid_prod_config();
    config.jwt.secret = "change-me-in-production".to_string();
    let errors = config.validate().failed("operation should fail");
    assert!(errors.iter().any(|e| e.contains("JWT secret")));
}

#[test]
fn test_validate_known_development_jwt_secret() {
    let mut config = valid_prod_config();
    for known_secret in KNOWN_DEV_JWT_SECRETS {
        config.jwt.secret = (*known_secret).to_string();
        let errors = config.validate().failed("operation should fail");
        assert!(
            errors
                .iter()
                .any(|e| e.contains("JWT secret") && e.contains("placeholder")),
            "unexpected errors for {known_secret:?}: {errors:?}"
        );
    }
}

#[test]
fn test_validate_empty_jwt_secret() {
    let mut config = valid_prod_config();
    config.jwt.secret = String::new();
    let errors = config.validate().failed("operation should fail");
    assert!(errors.iter().any(|e| e.contains("JWT secret is empty")));
}

#[test]
fn test_validate_known_development_security_secrets() {
    let mut config = valid_prod_config();
    config.security.credential_encryption_key = KNOWN_DEV_CREDENTIAL_ENCRYPTION_KEYS[0].to_string();
    config.security.opaque_server_setup_secret =
        KNOWN_DEV_OPAQUE_SERVER_SETUP_SECRETS[0].to_string();

    let errors = config.validate().failed("operation should fail");

    assert!(
        errors.iter().any(|e| {
            e.contains("security.credential_encryption_key")
                && e.contains("known development value")
        }),
        "unexpected errors: {errors:?}"
    );
    assert!(
        errors.iter().any(|e| {
            e.contains("security.opaque_server_setup_secret") && e.contains("placeholder")
        }),
        "unexpected errors: {errors:?}"
    );
}

#[test]
fn test_validate_historical_development_security_secrets() {
    let mut config = valid_prod_config();
    config.security.credential_encryption_key = KNOWN_DEV_CREDENTIAL_ENCRYPTION_KEYS[1].to_string();
    config.security.opaque_server_setup_secret =
        KNOWN_DEV_OPAQUE_SERVER_SETUP_SECRETS[1].to_string();
    config.cluster.secret = KNOWN_DEV_CLUSTER_SECRETS[0].to_string();

    let errors = config.validate().failed("operation should fail");

    assert!(
        errors.iter().any(|e| {
            e.contains("security.credential_encryption_key")
                && e.contains("known development value")
        }),
        "unexpected errors: {errors:?}"
    );
    assert!(
        errors.iter().any(|e| {
            e.contains("security.opaque_server_setup_secret") && e.contains("placeholder")
        }),
        "unexpected errors: {errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("cluster.secret") && e.contains("known development value")),
        "unexpected errors: {errors:?}"
    );
}

#[test]
fn test_validate_jwt_secret_too_short() {
    let mut config = valid_prod_config();
    // 31 characters - just under the 32 minimum
    config.jwt.secret = "a".repeat(31);
    let errors = config.validate().failed("operation should fail");
    assert!(errors
        .iter()
        .any(|e| e.contains("JWT secret") && e.contains("32") && e.contains("characters")));
}

#[test]
fn test_validate_jwt_secret_exactly_32_chars() {
    let mut config = valid_prod_config();
    // Exactly 32 characters - should pass
    config.jwt.secret = "a".repeat(32);
    assert!(config.validate().is_ok());
}

#[test]
fn test_from_file_merges_partial_nested_sections_with_defaults() {
    let unique = format!(
        "synctv-config-test-{}-{}.yaml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .checked("system time before unix epoch")
            .as_nanos()
    );
    let path = std::env::temp_dir().join(unique);
    std::fs::write(
        &path,
        r#"
server:
  port: 50051
database:
  url: "postgresql://user:pass@localhost/db"
jwt:
  secret: "12345678901234567890123456789012"
"#,
    )
    .checked("write config");

    let config =
        Config::load_with_env_map(Some(path.to_str().checked("utf-8 path")), &HashMap::new())
            .checked("partial config should merge with defaults");
    let _ = std::fs::remove_file(&path);

    assert_eq!(config.server.port, 50051);
    assert_eq!(config.jwt.secret, "12345678901234567890123456789012");
    assert_eq!(
        config.jwt.access_token_duration_hours,
        JwtConfig::default().access_token_duration_hours
    );
    assert_eq!(config.logging.level, LoggingConfig::default().level);
    assert_eq!(config.logging.filter, LoggingConfig::default().filter);
    assert_eq!(config.logging.backtrace, LoggingConfig::default().backtrace);
}

#[test]
fn test_from_file_parses_explicit_local_media_provider_config() {
    let unique = format!(
        "synctv-media-providers-config-test-{}-{}.yaml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .checked("system time before unix epoch")
            .as_nanos()
    );
    let path = std::env::temp_dir().join(unique);
    std::fs::write(
        &path,
        r"
media_providers:
  alist:
    request_timeout_seconds: 40
    connect_timeout_seconds: 8
  bilibili:
    request_timeout_seconds: 50
    connect_timeout_seconds: 9
  emby:
    request_timeout_seconds: 60
    connect_timeout_seconds: 10
",
    )
    .checked("write config");

    let config =
        Config::load_with_env_map(Some(path.to_str().checked("utf-8 path")), &HashMap::new())
            .checked("explicit local media provider config should load");
    let _ = std::fs::remove_file(&path);

    assert_eq!(config.media_providers.alist.request_timeout_seconds, 40);
    assert_eq!(config.media_providers.alist.connect_timeout_seconds, 8);
    assert_eq!(config.media_providers.bilibili.request_timeout_seconds, 50);
    assert_eq!(config.media_providers.bilibili.connect_timeout_seconds, 9);
    assert_eq!(config.media_providers.emby.request_timeout_seconds, 60);
    assert_eq!(config.media_providers.emby.connect_timeout_seconds, 10);
}

#[test]
fn test_from_file_resolves_typed_secret_file_references_relative_to_config_path() {
    let temp_dir = tempdir().checked("temp dir should be created");
    let config_dir = temp_dir.path().join("config");
    std::fs::create_dir_all(&config_dir).checked("config dir should be created");
    let data_dir = config_dir.join("state");
    std::fs::create_dir_all(&data_dir).checked("data dir should be created");

    std::fs::write(config_dir.join("jwt.secret"), "jwt-secret-from-file\n")
        .checked("jwt secret file should be written");
    std::fs::write(
        config_dir.join("cluster.secret"),
        "cluster-secret-from-file\n",
    )
    .checked("cluster secret file should be written");
    std::fs::write(
        config_dir.join("management.token"),
        "management-token-from-file\n",
    )
    .checked("management token file should be written");
    std::fs::write(
        config_dir.join("metrics.password"),
        "metrics-basic-password\n",
    )
    .checked("metrics password file should be written");
    std::fs::write(config_dir.join("metrics.bearer"), "metrics-bearer-token\n")
        .checked("metrics bearer token file should be written");
    std::fs::write(
        config_dir.join("database.url"),
        "postgresql://synctv:secret@db.example.com:5432/synctv\n",
    )
    .checked("database url file should be written");
    std::fs::write(config_dir.join("database.password"), "database-password\n")
        .checked("database password file should be written");
    std::fs::write(
        config_dir.join("redis.url"),
        "redis://:secret@redis.example.com:6379/0\n",
    )
    .checked("redis url file should be written");
    std::fs::write(config_dir.join("redis.password"), "redis-password\n")
        .checked("redis password file should be written");
    std::fs::write(config_dir.join("root.password"), "StrongPwd12345!\n")
        .checked("root password file should be written");
    std::fs::write(
        config_dir.join("credential.key"),
        "111102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f\n",
    )
    .checked("credential encryption key file should be written");
    std::fs::write(
        config_dir.join("opaque.secret"),
        "opaque-server-setup-secret-from-file\n",
    )
    .checked("opaque server setup secret file should be written");

    let config_path = config_dir.join("synctv.yaml");
    std::fs::write(
        &config_path,
        r#"
data_dir: "./state"
cluster:
  secret_file: "./cluster.secret"
management:
  transport: "unix"
  auth_token_file: "./management.token"
metrics:
  auth:
    mode: "basic"
    basic_username: "metrics"
    bearer_token_file: "./metrics.bearer"
    basic_password_file: "./metrics.password"
database:
  url_file: "./database.url"
  password_file: "./database.password"
redis:
  url_file: "./redis.url"
  password_file: "./redis.password"
jwt:
  secret_file: "./jwt.secret"
security:
  credential_encryption_key_file: "./credential.key"
  opaque_server_setup_secret_file: "./opaque.secret"
bootstrap:
  create_root_user: true
  root_username: "admin"
  root_password_file: "./root.password"
"#,
    )
    .checked("config file should be written");

    let unknown_keys =
        Config::collect_unknown_config_file_keys(config_path.to_str().checked("utf-8 path"))
            .checked("supported _file keys should not be reported as unknown");
    let config = Config::load_with_env_map(
        Some(config_path.to_str().checked("utf-8 path")),
        &HashMap::new(),
    )
    .checked("typed _file references should load");

    assert!(
        unknown_keys.is_empty(),
        "supported _file keys should not be treated as unknown: {unknown_keys:?}"
    );
    assert_eq!(config.jwt.secret, "jwt-secret-from-file");
    assert_eq!(config.cluster.secret, "cluster-secret-from-file");
    assert_eq!(config.management.auth_token, "management-token-from-file");
    assert_eq!(config.metrics.auth.basic_password, "metrics-basic-password");
    assert_eq!(config.metrics.auth.bearer_token, "metrics-bearer-token");
    assert_eq!(
        config.database.url,
        "postgresql://synctv:secret@db.example.com:5432/synctv"
    );
    assert_eq!(config.database.password, "database-password");
    assert_eq!(config.redis.url, "redis://:secret@redis.example.com:6379/0");
    assert_eq!(config.redis.password, "redis-password");
    assert_eq!(
        config.security.credential_encryption_key,
        "111102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
    );
    assert_eq!(
        config.security.opaque_server_setup_secret,
        "opaque-server-setup-secret-from-file"
    );
    assert_eq!(config.bootstrap.root_password, "StrongPwd12345!");
}

#[test]
fn test_from_file_builds_database_and_redis_urls_from_split_config() {
    let temp_dir = tempdir().checked("temp dir should be created");
    let config_dir = temp_dir.path().join("config");
    std::fs::create_dir_all(&config_dir).checked("config dir should be created");
    std::fs::write(config_dir.join("database.password"), "pg-password\n")
        .checked("database password file should be written");
    std::fs::write(config_dir.join("redis.password"), "redis-password\n")
        .checked("redis password file should be written");

    let config_path = config_dir.join("synctv.yaml");
    std::fs::write(
        &config_path,
        r#"
database:
  host: "db.example.com"
  port: 5433
  username: "synctv"
  password_file: "./database.password"
  name: "synctv_prod"
redis:
  host: "redis.example.com"
  port: 6380
  username: "cache-user"
  password_file: "./redis.password"
  database: 7
"#,
    )
    .checked("config file should be written");

    let config = Config::load_with_env_map(
        Some(config_path.to_str().checked("utf-8 path")),
        &HashMap::new(),
    )
    .checked("split database config should load");

    assert!(config.database.url.is_empty());
    assert_eq!(config.database.username, "synctv");
    assert_eq!(
        config.database_url(),
        "postgresql://synctv:pg-password@db.example.com:5433/synctv_prod"
    );
    assert_eq!(config.redis.username, "cache-user");
    assert_eq!(
        config.redis_url(),
        "redis://cache-user:redis-password@redis.example.com:6380/7"
    );
}

#[test]
fn test_split_database_and_redis_urls_escape_reserved_characters() {
    let mut config = Config::default();
    config.database.url.clear();
    config.database.host = "db.example.com".to_string();
    config.database.port = 5432;
    config.database.username = "sync@tv".to_string();
    config.database.password = "p@ss/word:with?symbols#frag".to_string();
    config.database.name = "sync/tv prod".to_string();
    config.redis.url.clear();
    config.redis.host = "redis.example.com".to_string();
    config.redis.port = 6379;
    config.redis.username = "cache:user".to_string();
    config.redis.password = "p@ss/word:with?symbols#frag".to_string();
    config.redis.database = 7;

    let database_url = config.database_url();
    assert_eq!(
        database_url,
        "postgresql://sync%40tv:p%40ss%2Fword%3Awith%3Fsymbols%23frag@db.example.com:5432/sync%2Ftv%20prod"
    );
    let parsed_database =
        url::Url::parse(&database_url).checked("escaped database URL should parse");
    assert_eq!(parsed_database.host_str(), Some("db.example.com"));
    assert_eq!(parsed_database.port(), Some(5432));

    let redis_url = config.redis_url();
    assert_eq!(
        redis_url,
        "redis://cache%3Auser:p%40ss%2Fword%3Awith%3Fsymbols%23frag@redis.example.com:6379/7"
    );
    let redis_client =
        redis::Client::open(redis_url.as_str()).checked("escaped Redis URL should parse");
    let parsed_redis = redis_client.get_connection_info();
    assert_eq!(
        parsed_redis.addr(),
        &redis::ConnectionAddr::Tcp("redis.example.com".to_string(), 6379)
    );
    assert_eq!(parsed_redis.redis_settings().db(), 7);
    assert_eq!(parsed_redis.redis_settings().username(), Some("cache:user"));
    assert_eq!(
        parsed_redis.redis_settings().password(),
        Some("p@ss/word:with?symbols#frag")
    );
}

#[test]
fn test_data_dir_override_does_not_rebase_typed_secret_file_references() {
    let temp_dir = tempdir().checked("temp dir should be created");
    let config_dir = temp_dir.path().join("config");
    let override_data_dir = temp_dir.path().join("override-state");
    std::fs::create_dir_all(&config_dir).checked("config dir should be created");
    std::fs::create_dir_all(&override_data_dir).checked("override data dir should be created");

    std::fs::write(
        config_dir.join("jwt.secret"),
        "jwt-secret-from-config-dir\n",
    )
    .checked("jwt secret file should be written");

    let config_path = config_dir.join("synctv.yaml");
    std::fs::write(
        &config_path,
        r#"
jwt:
  secret_file: "./jwt.secret"
"#,
    )
    .checked("config file should be written");

    let config = Config::load_with_env_map_and_data_dir_override(
        Some(config_path.to_str().checked("utf-8 path")),
        &HashMap::new(),
        Some(override_data_dir.to_str().checked("utf-8 path")),
    )
    .checked("data_dir override should not change secret file lookup");

    assert_eq!(config.jwt.secret, "jwt-secret-from-config-dir");
}

#[test]
fn test_from_file_resolves_owned_local_paths_relative_to_data_dir() {
    let temp_dir = tempdir().checked("temp dir should be created");
    let config_dir = temp_dir.path().join("config");
    std::fs::create_dir_all(&config_dir).checked("config dir should be created");
    let config_path = config_dir.join("synctv.yaml");
    std::fs::write(
        &config_path,
        r#"
data_dir: "./state"
management:
  transport: "unix"
  unix_socket_path: "sockets/admin.sock"
metrics:
  tls:
    cert_path: "tls/metrics.crt"
    key_path: "tls/metrics.key"
proxy_slice_cache:
  enabled: false
  slice_size_bytes: 4194304
  max_cache_size_bytes: 1073741824
  segment_ttl_seconds: 600
  stale_max_age_seconds: 120
  stale_while_revalidate: false
  file_backend_enabled: true
  file_cache_dir: "proxy-cache"
  eviction_interval_seconds: 30
  watermark_ratio: 0.75
logging:
  file_path: "logs/server.log"
livestream:
  hls_storage_path: "hls"
"#,
    )
    .checked("config file should be written");

    let config = Config::load_with_env_map(
        Some(config_path.to_str().checked("utf-8 path")),
        &HashMap::new(),
    )
    .checked("config file with data_dir should load");
    let expected_data_dir = config_dir.join("state");

    assert_eq!(Path::new(&config.data_dir), expected_data_dir);
    assert_eq!(
        Path::new(&config.management.unix_socket_path),
        expected_data_dir.join("sockets").join("admin.sock")
    );
    assert_eq!(
        config.logging.file_path.as_deref().map(Path::new),
        Some(expected_data_dir.join("logs").join("server.log").as_path())
    );
    assert_eq!(
        Path::new(&config.metrics.tls.cert_path),
        config_dir.join("tls").join("metrics.crt")
    );
    assert_eq!(
        Path::new(&config.metrics.tls.key_path),
        config_dir.join("tls").join("metrics.key")
    );
    assert!(!config.proxy_slice_cache.enabled);
    assert_eq!(config.proxy_slice_cache.slice_size_bytes, 4 * 1024 * 1024);
    assert_eq!(
        config.proxy_slice_cache.max_cache_size_bytes,
        1024 * 1024 * 1024
    );
    assert_eq!(config.proxy_slice_cache.segment_ttl_seconds, 600);
    assert_eq!(config.proxy_slice_cache.stale_max_age_seconds, 120);
    assert!(!config.proxy_slice_cache.stale_while_revalidate);
    assert!(config.proxy_slice_cache.file_backend_enabled);
    assert_eq!(
        Path::new(&config.proxy_slice_cache.file_cache_dir),
        expected_data_dir.join("proxy-cache")
    );
    assert_eq!(config.proxy_slice_cache.eviction_interval_seconds, 30);
    assert!((config.proxy_slice_cache.watermark_ratio - 0.75).abs() < f64::EPSILON);
    assert_eq!(
        Path::new(&config.livestream.hls_storage_path),
        expected_data_dir.join("hls")
    );
}

#[test]
fn test_collect_unknown_config_file_keys_ignores_top_level_data_dir() {
    let temp_dir = tempdir().checked("temp dir should be created");
    let config_path = temp_dir.path().join("synctv.yaml");
    std::fs::write(
        &config_path,
        r#"
data_dir: "./state"
management:
  transport: "unix"
"#,
    )
    .checked("config file should be written");

    let unknown_keys =
        Config::collect_unknown_config_file_keys(config_path.to_str().checked("utf-8 path"))
            .checked("top-level data_dir should deserialize cleanly");

    assert!(
        unknown_keys.is_empty(),
        "top-level data_dir should not be reported as unknown: {unknown_keys:?}"
    );
}

#[test]
fn test_from_env_map_resolves_relative_data_dir_from_current_dir() {
    let cwd = std::env::current_dir().checked("current dir should resolve");
    let env = HashMap::from([
        ("SYNCTV_DATA_DIR".to_string(), "var/synctv".to_string()),
        (
            "SYNCTV_MANAGEMENT_UNIX_SOCKET_PATH".to_string(),
            "ops/management.sock".to_string(),
        ),
        (
            "SYNCTV_LOGGING_FILE_PATH".to_string(),
            "logs/server.log".to_string(),
        ),
        (
            "SYNCTV_LIVESTREAM_HLS_STORAGE_PATH".to_string(),
            "livestream/hls".to_string(),
        ),
        (
            "SYNCTV_METRICS_TLS_CERT_PATH".to_string(),
            "tls/metrics.crt".to_string(),
        ),
        (
            "SYNCTV_METRICS_TLS_KEY_PATH".to_string(),
            "tls/metrics.key".to_string(),
        ),
    ]);

    let config = Config::from_env_map(&env).checked("env-backed config should load");
    let expected_data_dir = cwd.join("var").join("synctv");

    assert_eq!(Path::new(&config.data_dir), expected_data_dir);
    assert_eq!(
        Path::new(&config.management.unix_socket_path),
        expected_data_dir.join("ops").join("management.sock")
    );
    assert_eq!(
        config.logging.file_path.as_deref().map(Path::new),
        Some(expected_data_dir.join("logs").join("server.log").as_path())
    );
    assert_eq!(
        Path::new(&config.metrics.tls.cert_path),
        cwd.join("tls").join("metrics.crt")
    );
    assert_eq!(
        Path::new(&config.metrics.tls.key_path),
        cwd.join("tls").join("metrics.key")
    );
    assert_eq!(
        Path::new(&config.livestream.hls_storage_path),
        expected_data_dir.join("livestream").join("hls")
    );
}

#[test]
fn test_from_env_map_resolves_proxy_slice_cache_dir_relative_to_data_dir() {
    let cwd = std::env::current_dir().checked("current dir should resolve");
    let env = HashMap::from([
        ("SYNCTV_DATA_DIR".to_string(), "var/synctv".to_string()),
        (
            "SYNCTV_PROXY_SLICE_CACHE_FILE_BACKEND_ENABLED".to_string(),
            "true".to_string(),
        ),
        (
            "SYNCTV_PROXY_SLICE_CACHE_ENABLED".to_string(),
            "false".to_string(),
        ),
        (
            "SYNCTV_PROXY_SLICE_CACHE_SLICE_SIZE_BYTES".to_string(),
            "4194304".to_string(),
        ),
        (
            "SYNCTV_PROXY_SLICE_CACHE_MAX_CACHE_SIZE_BYTES".to_string(),
            "1073741824".to_string(),
        ),
        (
            "SYNCTV_PROXY_SLICE_CACHE_SEGMENT_TTL_SECONDS".to_string(),
            "600".to_string(),
        ),
        (
            "SYNCTV_PROXY_SLICE_CACHE_STALE_MAX_AGE_SECONDS".to_string(),
            "120".to_string(),
        ),
        (
            "SYNCTV_PROXY_SLICE_CACHE_STALE_WHILE_REVALIDATE".to_string(),
            "false".to_string(),
        ),
        (
            "SYNCTV_PROXY_SLICE_CACHE_EVICTION_INTERVAL_SECONDS".to_string(),
            "30".to_string(),
        ),
        (
            "SYNCTV_PROXY_SLICE_CACHE_WATERMARK_RATIO".to_string(),
            "0.75".to_string(),
        ),
    ]);

    let config = Config::from_env_map(&env).checked("env-backed config should load");
    let expected_data_dir = cwd.join("var").join("synctv");

    assert!(!config.proxy_slice_cache.enabled);
    assert_eq!(config.proxy_slice_cache.slice_size_bytes, 4 * 1024 * 1024);
    assert_eq!(
        config.proxy_slice_cache.max_cache_size_bytes,
        1024 * 1024 * 1024
    );
    assert_eq!(config.proxy_slice_cache.segment_ttl_seconds, 600);
    assert_eq!(config.proxy_slice_cache.stale_max_age_seconds, 120);
    assert!(!config.proxy_slice_cache.stale_while_revalidate);
    assert!(config.proxy_slice_cache.file_backend_enabled);
    assert_eq!(
        Path::new(&config.proxy_slice_cache.file_cache_dir),
        expected_data_dir.join("cache").join("proxy-slice")
    );
    assert_eq!(config.proxy_slice_cache.eviction_interval_seconds, 30);
    assert!((config.proxy_slice_cache.watermark_ratio - 0.75).abs() < f64::EPSILON);
}

#[test]
fn test_from_file_rejects_missing_secret_file_reference() {
    let temp_dir = tempdir().checked("temp dir should be created");
    let config_path = temp_dir.path().join("synctv.yaml");
    std::fs::write(
        &config_path,
        r#"
jwt:
  secret_file: "./missing.secret"
"#,
    )
    .checked("config file should be written");

    let error = Config::from_file(config_path.to_str().checked("utf-8 path"))
        .failed("missing _file target must fail closed");

    assert!(
        error.to_string().contains("jwt.secret_file"),
        "missing file error should mention the failing _file key: {error}"
    );
}

#[test]
fn test_from_file_rejects_missing_path() {
    let unique = format!(
        "synctv-missing-config-{}-{}.yaml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .checked("system time before unix epoch")
            .as_nanos()
    );
    let path = std::env::temp_dir().join(unique);

    let error = Config::from_file(path.to_str().checked("utf-8 path"))
        .failed("missing file must not fall back to defaults");

    assert!(
        error.to_string().contains("not found"),
        "missing file error should mention not found: {error}"
    );
}

#[test]
fn test_from_file_rejects_unknown_server_port_keys() {
    let unique = format!(
        "synctv-unknown-port-config-{}-{}.yaml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .checked("system time before unix epoch")
            .as_nanos()
    );
    let path = std::env::temp_dir().join(unique);
    std::fs::write(
        &path,
        r#"
server:
  host: "0.0.0.0"
  grpc_port: 50051
  http_port: 8080
jwt:
  secret: "12345678901234567890123456789012"
"#,
    )
    .checked("write config");

    let unknown_keys =
        Config::collect_unknown_config_file_keys(path.to_str().checked("utf-8 path"))
            .checked("unknown split-port keys should be collected");
    let error = Config::from_file(path.to_str().checked("utf-8 path"))
        .failed("unknown split-port file keys must fail fast");
    let _ = std::fs::remove_file(&path);

    assert!(
        unknown_keys.contains(&"server.grpc_port".to_string()),
        "server.grpc_port should be reported as unknown: {unknown_keys:?}"
    );
    assert!(
        unknown_keys.contains(&"server.http_port".to_string()),
        "server.http_port should be reported as unknown: {unknown_keys:?}"
    );
    let message = error.to_string();
    assert!(message.contains("server.grpc_port"));
    assert!(message.contains("server.http_port"));
}

#[test]
fn test_from_file_rejects_unknown_database_and_redis_keys() {
    let unique = format!(
        "synctv-unknown-nested-config-{}-{}.yaml",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .checked("system time before unix epoch")
            .as_nanos()
    );
    let path = std::env::temp_dir().join(unique);
    std::fs::write(
        &path,
        r#"
database:
  host: "db.example.com"
  port: 5432
  unknown_key: "synctv"
  password: "secret"
  name: "synctv"
redis:
  host: "redis.example.com"
  port: 6379
  unknown_key: "cache-user"
jwt:
  secret: "12345678901234567890123456789012"
"#,
    )
    .checked("write config");

    let error = Config::from_file(path.to_str().checked("utf-8 path"))
        .failed("unknown nested config keys must fail fast");
    let _ = std::fs::remove_file(&path);

    let message = error.to_string();
    assert!(message.contains("database.unknown_key"));
    assert!(message.contains("redis.unknown_key"));
}

#[test]
fn test_validate_allows_empty_root_password_until_bootstrap_creation() {
    let mut config = valid_prod_config();
    config.bootstrap.create_root_user = false;
    config.bootstrap.root_password.clear();
    config
        .validate()
        .checked("static config validation should not require a root password");
}

#[test]
fn test_validate_root_password_for_creation_rejects_default() {
    let mut config = valid_prod_config();
    config.bootstrap.root_password = "root".to_string();
    let errors = config.bootstrap.validate_root_password_for_creation();
    assert!(errors
        .iter()
        .any(|e| e.contains("Root password") && e.contains("default")));
}

#[test]
fn test_validate_root_password_for_creation_rejects_known_development_passwords() {
    let mut config = valid_prod_config();
    for known_password in KNOWN_DEV_ROOT_PASSWORDS {
        config.bootstrap.root_password = (*known_password).to_string();
        let errors = config.bootstrap.validate_root_password_for_creation();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("Root password") && e.contains("default")),
            "unexpected errors for {known_password:?}: {errors:?}"
        );
    }
}

#[test]
fn test_validate_root_password_for_creation_rejects_empty_password_once() {
    let mut config = valid_prod_config();
    config.bootstrap.root_password.clear();
    let errors = config.bootstrap.validate_root_password_for_creation();

    assert!(
        errors.iter().any(|e| e.contains("Root password is empty")),
        "empty password should still report the required-value error: {errors:?}"
    );
    assert!(
        !errors.iter().any(|e| e.contains("12 characters")),
        "empty password should not duplicate into length errors: {errors:?}"
    );
    assert!(
        !errors.iter().any(|e| e.contains("uppercase")),
        "empty password should not duplicate into uppercase errors: {errors:?}"
    );
    assert!(
        !errors.iter().any(|e| e.contains("lowercase")),
        "empty password should not duplicate into lowercase errors: {errors:?}"
    );
    assert!(
        !errors.iter().any(|e| e.contains("digit")),
        "empty password should not duplicate into digit errors: {errors:?}"
    );
}

#[test]
fn test_validate_root_password_for_creation_rejects_too_short() {
    let mut config = valid_prod_config();
    config.bootstrap.root_password = "Short1aA".to_string(); // 8 chars, < 12
    let errors = config.bootstrap.validate_root_password_for_creation();
    assert!(errors.iter().any(|e| e.contains("12 characters")));
}

#[test]
fn test_validate_root_password_for_creation_rejects_no_uppercase() {
    let mut config = valid_prod_config();
    config.bootstrap.root_password = "allowercase123".to_string();
    let errors = config.bootstrap.validate_root_password_for_creation();
    assert!(errors.iter().any(|e| e.contains("uppercase")));
}

#[test]
fn test_validate_root_password_for_creation_rejects_no_lowercase() {
    let mut config = valid_prod_config();
    config.bootstrap.root_password = "ALLUPPERCASE123".to_string();
    let errors = config.bootstrap.validate_root_password_for_creation();
    assert!(errors.iter().any(|e| e.contains("lowercase")));
}

#[test]
fn test_validate_root_password_for_creation_rejects_no_digit() {
    let mut config = valid_prod_config();
    config.bootstrap.root_password = "NoDigitsHereABC".to_string();
    let errors = config.bootstrap.validate_root_password_for_creation();
    assert!(errors.iter().any(|e| e.contains("digit")));
}

#[test]
fn test_validate_root_username_too_short() {
    let mut config = valid_prod_config();
    config.bootstrap.root_username = "ab".to_string();
    let errors = config.validate().failed("operation should fail");
    assert!(errors
        .iter()
        .any(|e| e.contains("Root username") && e.contains('3')));
}

#[test]
fn test_validate_db_pool_min_exceeds_max() {
    let mut config = valid_prod_config();
    config.database.min_connections = 30;
    config.database.max_connections = 10;
    let errors = config.validate().failed("operation should fail");
    assert!(errors
        .iter()
        .any(|e| e.contains("min_connections") && e.contains("max_connections")));
}

#[test]
fn test_validate_db_pool_max_zero() {
    let mut config = valid_prod_config();
    config.database.max_connections = 0;
    let errors = config.validate().failed("operation should fail");
    assert!(errors
        .iter()
        .any(|e| e.contains("max_connections") && e.contains("greater than 0")));
}

#[test]
fn test_validate_shutdown_drain_timeout_zero() {
    let mut config = valid_prod_config();
    config.server.shutdown_drain_timeout_seconds = 0;

    let errors = config.validate().failed("operation should fail");

    assert!(errors
        .iter()
        .any(|e| { e.contains("shutdown_drain_timeout_seconds") && e.contains("greater than 0") }));
}

#[test]
fn test_validate_database_timeouts_zero() {
    let mut config = valid_prod_config();
    config.database.connect_timeout_seconds = 0;
    config.database.idle_timeout_seconds = 0;
    config.database.max_lifetime_seconds = 0;

    let errors = config.validate().failed("operation should fail");

    assert!(errors
        .iter()
        .any(|e| e.contains("database.connect_timeout_seconds")));
    assert!(errors
        .iter()
        .any(|e| e.contains("database.idle_timeout_seconds")));
    assert!(errors
        .iter()
        .any(|e| e.contains("database.max_lifetime_seconds")));
}

#[test]
fn test_validate_connection_limits_zero() {
    let mut config = valid_prod_config();
    config.connection_limits.max_per_user = 0;
    let errors = config.validate().failed("operation should fail");
    assert!(errors.iter().any(|e| e.contains("max_per_user")));

    let mut config = valid_prod_config();
    config.connection_limits.max_per_room = 0;
    let errors = config.validate().failed("operation should fail");
    assert!(errors.iter().any(|e| e.contains("max_per_room")));

    let mut config = valid_prod_config();
    config.connection_limits.max_total = 0;
    let errors = config.validate().failed("operation should fail");
    assert!(errors.iter().any(|e| e.contains("max_total")));
}

#[test]
fn test_validate_messaging_rate_limits_zero() {
    let mut config = valid_prod_config();

    config.messaging_rate_limits.chat_per_second = 0;
    let errors = config
        .validate()
        .failed("chat rate limit must be validated");
    assert!(errors
        .iter()
        .any(|e| e.contains("messaging_rate_limits.chat_per_second")));

    config.messaging_rate_limits.chat_per_second = 1;
    config.messaging_rate_limits.window_seconds = 0;
    let errors = config.validate().failed("window must be validated");
    assert!(errors
        .iter()
        .any(|e| e.contains("messaging_rate_limits.window_seconds")));
}

#[test]
fn test_from_env_overrides_messaging_rate_limits() {
    let config = Config::from_env_map(&env_map(&[
        ("SYNCTV_MESSAGING_RATE_LIMITS_CHAT_PER_SECOND", "17"),
        ("SYNCTV_MESSAGING_RATE_LIMITS_WINDOW_SECONDS", "4"),
    ]))
    .checked("messaging rate env overrides should parse");

    assert_eq!(config.messaging_rate_limits.chat_per_second, 17);
    assert_eq!(config.messaging_rate_limits.window_seconds, 4);
}

#[test]
fn test_from_env_overrides_request_websocket_rate_limits() {
    let config = Config::from_env_map(&env_map(&[
        ("SYNCTV_REQUEST_RATE_LIMITS_WEBSOCKET_MAX_REQUESTS", "13"),
        ("SYNCTV_REQUEST_RATE_LIMITS_WEBSOCKET_WINDOW_SECONDS", "17"),
    ]))
    .checked("request websocket rate limit env overrides should parse");

    assert_eq!(config.request_rate_limits.websocket_max_requests, 13);
    assert_eq!(config.request_rate_limits.websocket_window_seconds, 17);
}

#[test]
fn test_from_env_overrides_request_rate_limit_scope_rules() {
    let config = Config::from_env_map(&env_map(&[(
        "SYNCTV_REQUEST_RATE_LIMITS_SCOPES",
        r#"{"room_members":{"max_requests":90,"window_seconds":15,"strategy":"fixed_window"}}"#,
    )]))
    .checked("request scope rate limit env override should parse");

    let rule = config
        .request_rate_limits
        .scopes
        .get("room_members")
        .checked("room_members scope should be configured");
    assert_eq!(rule.max_requests, Some(90));
    assert_eq!(rule.window_seconds, Some(15));
    assert_eq!(rule.strategy, RateLimitScopeStrategy::FixedWindow);
}

#[test]
fn test_validate_api_rate_limits_zero() {
    let mut config = valid_prod_config();
    config.request_rate_limits.read_max_requests = 0;
    config.request_rate_limits.websocket_window_seconds = 0;
    config.request_rate_limits.scopes.insert(
        "room_members".to_string(),
        RateLimitScopeRule {
            max_requests: Some(0),
            window_seconds: Some(30),
            strategy: RateLimitScopeStrategy::FixedWindow,
        },
    );

    let errors = config
        .validate()
        .failed("zero API rate limits should be rejected");
    assert!(errors
        .iter()
        .any(|e| e.contains("request_rate_limits.read.max_requests")));
    assert!(errors
        .iter()
        .any(|e| e.contains("request_rate_limits.websocket.window_seconds")));
    assert!(errors
        .iter()
        .any(|e| e.contains("request_rate_limits.scopes.room_members.max_requests")));
}

#[test]
fn test_validate_database_url_is_mutually_exclusive_with_split_database_fields() {
    let mut config = valid_prod_config();
    config.database.url = "postgresql://user:pass@db.example.com:5432/synctv".to_string();
    config.database.host = "db.example.com".to_string();

    let errors = config
        .validate()
        .failed("database URL and split fields must be exclusive");
    assert!(errors
        .iter()
        .any(|e| e.contains("database.url is mutually exclusive")));
}

#[test]
fn test_validate_redis_url_is_mutually_exclusive_with_split_redis_fields() {
    let mut config = valid_prod_config();
    config.redis.url = "redis://:secret@redis.example.com:6379/0".to_string();
    config.redis.host = "redis.example.com".to_string();

    let errors = config
        .validate()
        .failed("redis URL and split fields must be exclusive");
    assert!(errors
        .iter()
        .any(|e| e.contains("redis.url is mutually exclusive")));
}

#[test]
fn test_validate_split_database_config_requires_all_fields() {
    let mut config = valid_prod_config();
    config.database.url.clear();
    config.database.host = "db.example.com".to_string();
    config.database.port = 5432;
    config.database.username = "synctv".to_string();
    config.database.password.clear();
    config.database.name.clear();

    let errors = config
        .validate()
        .failed("incomplete split database config must fail");
    assert!(errors
        .iter()
        .any(|e| e.contains("database.password must be set")));
    assert!(errors
        .iter()
        .any(|e| e.contains("database.name must be set")));
}

#[test]
fn test_validate_split_redis_config_requires_host_and_port() {
    let mut config = valid_prod_config();
    config.redis.url.clear();
    config.redis.host = "redis.example.com".to_string();
    config.redis.port = 0;

    let errors = config
        .validate()
        .failed("incomplete split redis config must fail");
    assert!(errors
        .iter()
        .any(|e| e.contains("redis.port must be greater than 0")));
}

#[test]
fn test_from_env_overrides_logging_filter_and_backtrace() {
    let config = Config::from_env_map(&env_map(&[
        ("SYNCTV_LOGGING_FILTER", "info,synctv=debug"),
        ("SYNCTV_LOGGING_BACKTRACE", "true"),
    ]))
    .checked("logging env overrides should parse");

    assert_eq!(config.logging.filter.as_deref(), Some("info,synctv=debug"));
    assert!(config.logging.backtrace);
}

#[test]
fn test_from_env_resolves_timezone_from_synctv_env() {
    let config = Config::from_env_map(&env_map(&[("SYNCTV_TIME_TIMEZONE", "Asia/Shanghai")]))
        .checked("SYNCTV_TIME_TIMEZONE should resolve");

    assert_eq!(config.time.timezone, "Asia/Shanghai");
}

#[test]
fn test_from_env_resolves_timezone_from_tz_fallback() {
    let config = Config::from_env_map(&env_map(&[("TZ", "America/New_York")]))
        .checked("TZ fallback should resolve");

    assert_eq!(config.time.timezone, "America/New_York");
}

#[test]
fn test_management_tcp_endpoint_is_always_loopback() {
    let mut config = Config::default();
    config.management.transport = ManagementTransport::Tcp;
    config.management.port = 50099;

    assert_eq!(config.management_endpoint(), "http://127.0.0.1:50099");
    assert_eq!(config.management_bind_target(), "127.0.0.1:50099");
}

#[cfg(target_os = "macos")]
#[test]
fn test_default_management_unix_socket_path_uses_home_hidden_runtime_dir_on_macos() {
    let socket_path = default_management_unix_socket_path();
    let home = user_home_dir().checked("macOS test environment should expose HOME");
    assert_eq!(
        socket_path,
        home.join(".synctv").join("run").join("synctv.sock")
    );
}

#[cfg(target_os = "macos")]
#[test]
fn test_default_data_dir_uses_home_hidden_dir_on_macos() {
    let data_dir = default_data_dir();
    let home = user_home_dir().checked("macOS test environment should expose HOME");

    assert_eq!(data_dir, home.join(".synctv"));
}

#[cfg(target_os = "macos")]
#[test]
fn test_default_config_search_paths_use_home_hidden_config_dir_on_macos() {
    let home = user_home_dir().checked("macOS test environment should expose HOME");
    let expected = [
        home.join(".synctv").join("synctv.yaml"),
        home.join(".synctv").join("synctv.yml"),
        home.join(".synctv").join("synctv.json"),
        home.join(".synctv").join("synctv.toml"),
    ];
    let paths = default_config_search_paths();
    assert!(
        expected.iter().all(|path| paths.contains(path)),
        "macOS default config search paths should include ~/.synctv variants, got: {paths:?}"
    );
}

#[test]
fn test_validate_management_tcp_requires_auth_token() {
    let mut config = valid_prod_config();
    config.management.transport = ManagementTransport::Tcp;
    config.management.auth_token.clear();

    let errors = config
        .validate()
        .failed("management tcp transport must reject missing auth token");

    assert!(errors
        .iter()
        .any(|error| error.contains("management.auth_token")));
}

#[cfg(unix)]
#[test]
fn test_validate_management_unix_allows_empty_auth_token() {
    let mut config = valid_prod_config();
    config.management.transport = ManagementTransport::Unix;
    config.management.auth_token.clear();

    assert!(
        config.validate().is_ok(),
        "unix management transport may rely on owner-only socket permissions without a bearer token"
    );
}

#[test]
fn test_from_env_overrides_management_auth_token() {
    let config = Config::from_env_map(&env_map(&[("SYNCTV_MANAGEMENT_AUTH_TOKEN", "mgmt-secret")]))
        .checked("management auth token env override should parse");

    assert_eq!(config.management.auth_token, "mgmt-secret");
}

#[test]
fn test_from_env_loads_management_auth_token_file() {
    let temp_dir = tempdir().checked("temp dir should be created");
    let token_path = temp_dir.path().join("management.token");
    std::fs::write(&token_path, "mgmt-file-secret\n")
        .checked("management token file should be written");
    let config = Config::from_env_map(&env_map(&[(
        "SYNCTV_MANAGEMENT_AUTH_TOKEN_FILE",
        token_path.to_str().checked("token path should be utf-8"),
    )]))
    .checked("management auth token file env override should parse");

    assert_eq!(config.management.auth_token, "mgmt-file-secret");
}

#[test]
fn test_validate_webauthn_requires_rp_id_and_origin_when_enabled() {
    let mut config = valid_prod_config();
    config.webauthn.enabled = true;
    config.webauthn.rp_id.clear();
    config.webauthn.rp_origin.clear();

    let errors = config
        .validate()
        .failed("enabled WebAuthn must require relying-party identity");

    assert!(errors.iter().any(|error| error.contains("webauthn.rp_id")));
    assert!(errors
        .iter()
        .any(|error| error.contains("webauthn.rp_origin")));
}

#[test]
fn test_validate_webauthn_rejects_origin_with_path_query_or_fragment() {
    let mut config = valid_prod_config();
    config.webauthn.enabled = true;
    config.webauthn.rp_id = "app.example.com".to_string();
    config.webauthn.rp_origin = "https://app.example.com/login?next=/#section".to_string();

    let errors = config
        .validate()
        .failed("WebAuthn origins must be bare origins");

    assert!(errors
        .iter()
        .any(|error| error.contains("without path, query, or fragment")));
}

#[test]
fn test_validate_webauthn_accepts_minimal_valid_config() {
    let mut config = valid_prod_config();
    config.webauthn.enabled = true;
    config.webauthn.rp_id = "app.example.com".to_string();
    config.webauthn.rp_origin = "https://app.example.com".to_string();
    config.webauthn.rp_name = "SyncTV".to_string();

    assert!(config.validate().is_ok());
}

#[test]
fn test_validate_webauthn_requires_redis_in_cluster_mode() {
    let mut config = valid_prod_config();
    config.cluster.enabled = true;
    config.cluster.secret = "cluster-secret-long-enough".to_string();
    config.server.advertise_host = "10.0.0.12".to_string();
    config.redis.url.clear();
    config.webrtc.stun_external_addr = "203.0.113.1:3478".to_string();
    config.webauthn.enabled = true;
    config.webauthn.rp_id = "app.example.com".to_string();
    config.webauthn.rp_origin = "https://app.example.com".to_string();

    let errors = config
        .validate()
        .failed("clustered WebAuthn must use shared challenge storage");

    assert!(errors.iter().any(|error| {
        error.contains("WebAuthn/passkey requires Redis for challenge state in cluster mode")
    }));
}

#[test]
fn test_from_env_loads_top_level_secret_file_overrides() {
    let temp_dir = tempdir().checked("temp dir should be created");
    let write_secret = |name: &str, value: &str| -> std::path::PathBuf {
        let path = temp_dir.path().join(name);
        std::fs::write(&path, format!("{value}\n")).checked("secret file should be written");
        path
    };
    let jwt_secret = write_secret("jwt.secret", "jwt-secret-from-env-file");
    let cluster_secret = write_secret("cluster.secret", "cluster-secret-from-env-file");
    let credential_key = write_secret(
        "credential.key",
        "111102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
    );
    let opaque_secret = write_secret("opaque.secret", "opaque-server-setup-secret-from-env-file");
    let metrics_bearer = write_secret("metrics.bearer", "metrics-bearer-from-env-file");
    let metrics_password = write_secret("metrics.password", "metrics-password-from-env-file");
    let database_url = write_secret(
        "database.url",
        "postgresql://synctv:secret@db.example.com:5432/synctv",
    );
    let database_password = write_secret("database.password", "database-password-from-env-file");
    let redis_url = write_secret("redis.url", "redis://:secret@redis.example.com:6379/0");
    let redis_password = write_secret("redis.password", "redis-password-from-env-file");
    let chat_upload_token_secret = write_secret(
        "file-upload-token.secret",
        "chat-upload-token-secret-from-env-file",
    );
    let root_password = write_secret("root.password", "RootPassword12345");

    let config = Config::from_env_map(&env_map(&[
        (
            "SYNCTV_JWT_SECRET_FILE",
            jwt_secret.to_str().checked("utf-8 path"),
        ),
        (
            "SYNCTV_CLUSTER_SECRET_FILE",
            cluster_secret.to_str().checked("utf-8 path"),
        ),
        (
            "SYNCTV_SECURITY_CREDENTIAL_ENCRYPTION_KEY_FILE",
            credential_key.to_str().checked("utf-8 path"),
        ),
        (
            "SYNCTV_SECURITY_OPAQUE_SERVER_SETUP_SECRET_FILE",
            opaque_secret.to_str().checked("utf-8 path"),
        ),
        (
            "SYNCTV_METRICS_AUTH_BEARER_TOKEN_FILE",
            metrics_bearer.to_str().checked("utf-8 path"),
        ),
        (
            "SYNCTV_METRICS_AUTH_BASIC_PASSWORD_FILE",
            metrics_password.to_str().checked("utf-8 path"),
        ),
        (
            "SYNCTV_DATABASE_URL_FILE",
            database_url.to_str().checked("utf-8 path"),
        ),
        (
            "SYNCTV_DATABASE_PASSWORD_FILE",
            database_password.to_str().checked("utf-8 path"),
        ),
        (
            "SYNCTV_REDIS_URL_FILE",
            redis_url.to_str().checked("utf-8 path"),
        ),
        (
            "SYNCTV_REDIS_PASSWORD_FILE",
            redis_password.to_str().checked("utf-8 path"),
        ),
        (
            "SYNCTV_FILE_UPLOAD_TOKEN_SECRET_FILE",
            chat_upload_token_secret.to_str().checked("utf-8 path"),
        ),
        (
            "SYNCTV_BOOTSTRAP_ROOT_PASSWORD_FILE",
            root_password.to_str().checked("utf-8 path"),
        ),
    ]))
    .checked("secret file env overrides should parse");

    assert_eq!(config.jwt.secret, "jwt-secret-from-env-file");
    assert_eq!(config.cluster.secret, "cluster-secret-from-env-file");
    assert_eq!(
        config.security.credential_encryption_key,
        "111102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
    );
    assert_eq!(
        config.security.opaque_server_setup_secret,
        "opaque-server-setup-secret-from-env-file"
    );
    assert_eq!(
        config.metrics.auth.bearer_token,
        "metrics-bearer-from-env-file"
    );
    assert_eq!(
        config.metrics.auth.basic_password,
        "metrics-password-from-env-file"
    );
    assert_eq!(
        config.database.url,
        "postgresql://synctv:secret@db.example.com:5432/synctv"
    );
    assert_eq!(config.database.password, "database-password-from-env-file");
    assert_eq!(config.redis.url, "redis://:secret@redis.example.com:6379/0");
    assert_eq!(config.redis.password, "redis-password-from-env-file");
    assert_eq!(
        config.file_storage.upload_token_secret,
        "chat-upload-token-secret-from-env-file"
    );
    assert_eq!(config.bootstrap.root_password, "RootPassword12345");
}

#[test]
fn test_from_env_overrides_cluster_stream_max_length() {
    let config = Config::from_env_map(&env_map(&[("SYNCTV_CLUSTER_STREAM_MAX_LENGTH", "25000")]))
        .checked("cluster stream max length env override should parse");

    assert_eq!(config.cluster.stream_max_length, 25_000);
}

#[test]
fn test_from_env_builds_database_and_redis_urls_from_split_config() {
    let temp_dir = tempdir().checked("temp dir should be created");
    let database_password = temp_dir.path().join("database.password");
    let redis_password = temp_dir.path().join("redis.password");
    std::fs::write(&database_password, "pg-password\n")
        .checked("database password file should be written");
    std::fs::write(&redis_password, "redis-password\n")
        .checked("redis password file should be written");

    let config = Config::from_env_map(&env_map(&[
        ("SYNCTV_DATABASE_HOST", "db.example.com"),
        ("SYNCTV_DATABASE_PORT", "5433"),
        ("SYNCTV_DATABASE_USERNAME", "synctv"),
        (
            "SYNCTV_DATABASE_PASSWORD_FILE",
            database_password.to_str().checked("utf-8 path"),
        ),
        ("SYNCTV_DATABASE_NAME", "synctv_prod"),
        ("SYNCTV_REDIS_HOST", "redis.example.com"),
        ("SYNCTV_REDIS_PORT", "6380"),
        ("SYNCTV_REDIS_USERNAME", "cache-user"),
        (
            "SYNCTV_REDIS_PASSWORD_FILE",
            redis_password.to_str().checked("utf-8 path"),
        ),
        ("SYNCTV_REDIS_DATABASE", "7"),
    ]))
    .checked("split database env config should parse");

    assert!(config.database.url.is_empty());
    assert_eq!(config.database.username, "synctv");
    assert_eq!(
        config.database_url(),
        "postgresql://synctv:pg-password@db.example.com:5433/synctv_prod"
    );
    assert_eq!(config.redis.username, "cache-user");
    assert_eq!(
        config.redis_url(),
        "redis://cache-user:redis-password@redis.example.com:6380/7"
    );
}

#[test]
fn test_redis_split_env_overrides_file_url() {
    let temp_dir = tempdir().checked("temp dir should be created");
    let config_path = temp_dir.path().join("synctv.yaml");
    std::fs::write(
        &config_path,
        r#"
database:
  url: "postgresql://config-user:config-password@db.example.invalid:5432/config_db"
redis:
  url: "redis://localhost:6379"
jwt:
  secret: "12345678901234567890123456789012"
"#,
    )
    .checked("config file should be written");

    let config = Config::load_with_env_map(
        Some(config_path.to_str().checked("utf-8 path")),
        &env_map(&[
            ("SYNCTV_REDIS_HOST", "redis.example.com"),
            ("SYNCTV_REDIS_PORT", "6380"),
            ("SYNCTV_REDIS_PASSWORD", "redis-password"),
            ("SYNCTV_REDIS_DATABASE", "7"),
        ]),
    )
    .checked("split redis env config should replace file URL");

    assert_eq!(
        config.redis.url,
        "redis://:redis-password@redis.example.com:6380/7"
    );
    assert_eq!(
        config.redis_url(),
        "redis://:redis-password@redis.example.com:6380/7"
    );
}

#[test]
fn test_redis_url_partial_env_overrides_preserve_configured_endpoint() {
    let temp_dir = tempdir().checked("temp dir should be created");
    let config_path = temp_dir.path().join("synctv.yaml");
    let redis_password = temp_dir.path().join("redis.password");
    std::fs::write(&redis_password, "redis-password\n")
        .checked("redis password file should be written");
    std::fs::write(
        &config_path,
        r#"
database:
  url: "postgresql://config-user:config-password@db.example.invalid:5432/config_db"
redis:
  url: "redis://cache.example.com:6379/2"
jwt:
  secret: "12345678901234567890123456789012"
"#,
    )
    .checked("config file should be written");

    let config = Config::load_with_env_map(
        Some(config_path.to_str().checked("utf-8 path")),
        &env_map(&[
            (
                "SYNCTV_REDIS_PASSWORD_FILE",
                redis_password.to_str().checked("utf-8 path"),
            ),
            ("SYNCTV_REDIS_DATABASE", "7"),
        ]),
    )
    .checked("partial redis env config should update the configured URL");

    assert!(config.redis.host.is_empty());
    assert_eq!(
        config.redis_url(),
        "redis://:redis-password@cache.example.com:6379/7"
    );
}

#[cfg(not(unix))]
#[test]
fn test_validate_rejects_unix_management_transport_on_unsupported_platform() {
    let mut config = Config::default();
    config.jwt.secret = "12345678901234567890123456789012".to_string();
    config.management.transport = ManagementTransport::Unix;
    config.management.unix_socket_path = "C:/synctv/synctv.sock".to_string();

    let errors = config
        .validate()
        .failed("unix management transport must be rejected on unsupported platforms");

    assert!(errors.iter().any(|error| {
        error.contains("management.transport=unix")
            && error.contains("only supported on unix-like platforms")
    }));
}

#[test]
fn test_from_file_supports_yaml_yml_json_and_toml() {
    let secret = "12345678901234567890123456789012";
    let fixtures = [
        (
            "yaml",
            format!("jwt:\n  secret: \"{secret}\"\nserver:\n  port: 50051\n"),
        ),
        (
            "yml",
            format!("jwt:\n  secret: \"{secret}\"\nserver:\n  port: 50052\n"),
        ),
        (
            "json",
            format!(r#"{{"jwt":{{"secret":"{secret}"}},"server":{{"port":50053}}}}"#),
        ),
        (
            "toml",
            format!("server.port = 50054\njwt.secret = \"{secret}\"\n"),
        ),
    ];

    for (extension, contents) in fixtures {
        let unique = format!(
            "synctv-config-format-{}-{}-{}.{}",
            extension,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .checked("system time before unix epoch")
                .as_nanos(),
            extension
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::write(&path, contents).checked("write config fixture");

        let config =
            Config::load_with_env_map(Some(path.to_str().checked("utf-8 path")), &HashMap::new())
                .checked("supported config format should load");
        let _ = std::fs::remove_file(&path);

        assert_eq!(config.jwt.secret, secret);
        assert!(
            (50051..=50054).contains(&config.server.port),
            "unexpected port for extension {extension}: {}",
            config.server.port
        );
    }
}

#[test]
fn test_from_file_rejects_unknown_keys_for_json_and_toml() {
    let secret = "12345678901234567890123456789012";
    let fixtures = [
        (
            "json",
            format!(
                r#"{{"jwt":{{"secret":"{secret}"}},"metrics":{{"enabled":true,"obsolete_token":"ignored"}}}}"#
            ),
            "metrics.obsolete_token",
        ),
        (
            "toml",
            format!(
                "jwt.secret = \"{secret}\"\n[metrics]\nenabled = true\nobsolete_token = \"ignored\"\n"
            ),
            "metrics.obsolete_token",
        ),
    ];

    for (extension, contents, unknown_key) in fixtures {
        let unique = format!(
            "synctv-config-unknown-{}-{}-{}.{}",
            extension,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .checked("system time before unix epoch")
                .as_nanos(),
            extension
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::write(&path, contents).checked("write config fixture");

        let unknown_keys =
            Config::collect_unknown_config_file_keys(path.to_str().checked("utf-8 path"))
                .checked("unknown keys should be collected");
        let error = Config::from_file(path.to_str().checked("utf-8 path"))
            .failed("unknown config keys must fail fast");
        let _ = std::fs::remove_file(&path);

        assert!(
            unknown_keys.contains(&unknown_key.to_string()),
            "missing unknown key {unknown_key} for {extension}: {unknown_keys:?}"
        );
        assert!(
            error.to_string().contains(unknown_key),
            "unknown key should appear in strict error for {extension}: {error}"
        );
    }
}

#[test]
fn test_from_file_rejects_unsupported_extension() {
    let unique = format!(
        "synctv-config-unsupported-{}-{}.ini",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .checked("system time before unix epoch")
            .as_nanos()
    );
    let path = std::env::temp_dir().join(unique);
    std::fs::write(
        &path,
        "[jwt]\nsecret = \"12345678901234567890123456789012\"\n",
    )
    .checked("write config");

    let error = Config::from_file(path.to_str().checked("utf-8 path"))
        .failed("unsupported extension must fail");
    let _ = std::fs::remove_file(&path);

    assert!(
        error
            .to_string()
            .contains("unsupported config file extension"),
        "unexpected error: {error}"
    );
}

#[test]
fn test_validate_rejects_invalid_logging_filter() {
    let mut config = Config::default();
    config.jwt.secret = "12345678901234567890123456789012".to_string();
    config.logging.filter = Some("not a valid filter ==".to_string());

    let errors = config
        .validate()
        .failed("invalid logging.filter must fail validation");

    assert!(errors.iter().any(|error| error.contains("logging.filter")));
}

#[test]
fn test_from_env_overrides_livestream_extended_runtime_limits() {
    let config = Config::from_env_map(&env_map(&[
        ("SYNCTV_LIVESTREAM_HLS_MEMORY_MAX_MB", "768"),
        ("SYNCTV_LIVESTREAM_HLS_STORAGE_BACKEND", "oss"),
        (
            "SYNCTV_LIVESTREAM_HLS_OSS_ENDPOINT",
            "https://s3.example.com",
        ),
        ("SYNCTV_LIVESTREAM_HLS_OSS_BUCKET", "synctv-hls"),
        ("SYNCTV_LIVESTREAM_HLS_OSS_REGION", "auto"),
        ("SYNCTV_LIVESTREAM_HLS_OSS_BASE_PATH", "/synctv/hls"),
        ("SYNCTV_LIVESTREAM_HLS_OSS_ACCESS_KEY_ID", "access-key"),
        ("SYNCTV_LIVESTREAM_HLS_OSS_SECRET_ACCESS_KEY", "secret-key"),
        (
            "SYNCTV_LIVESTREAM_FLV_MAX_CONNECTION_DURATION_SECONDS",
            "7200",
        ),
        ("SYNCTV_LIVESTREAM_FLV_WRITE_TIMEOUT_SECONDS", "45"),
        ("SYNCTV_LIVESTREAM_PUBLIC_RTMP_HOST", "stream.example.com"),
    ]))
    .checked("livestream env overrides should parse");

    assert_eq!(config.livestream.hls_memory_max_mb, 768);
    assert_eq!(
        config.livestream.hls_storage_backend,
        HlsStorageBackend::Oss
    );
    assert_eq!(config.livestream.hls_oss.endpoint, "https://s3.example.com");
    assert_eq!(config.livestream.hls_oss.bucket, "synctv-hls");
    assert_eq!(config.livestream.hls_oss.region.as_deref(), Some("auto"));
    assert_eq!(config.livestream.hls_oss.base_path, "synctv/hls/");
    assert_eq!(config.livestream.hls_oss.access_key_id, "access-key");
    assert_eq!(config.livestream.hls_oss.secret_access_key, "secret-key");
    assert_eq!(config.livestream.flv_max_connection_duration_seconds, 7200);
    assert_eq!(config.livestream.flv_write_timeout_seconds, 45);
    assert_eq!(config.livestream.public_rtmp_host, "stream.example.com");
}

#[test]
fn test_from_env_overrides_file_s3_storage() {
    let config = Config::from_env_map(&env_map(&[
        ("SYNCTV_FILE_STORAGE_DEFAULT_BACKEND", "s3_public"),
        (
            "SYNCTV_FILE_STORAGE_CHAT_ATTACHMENTS_BACKEND",
            "s3_public",
        ),
        (
            "SYNCTV_FILE_STORAGE_UNREFERENCED_OBJECT_RETENTION_SECONDS",
            "7200",
        ),
        ("SYNCTV_FILE_UPLOAD_TOKEN_SECRET", "upload-token-secret"),
        (
            "SYNCTV_FILE_STORAGE_BACKENDS",
            r#"{"s3_public":{"type":"s3","s3":{"endpoint":"https://s3.example.com","bucket":"synctv-files","region":"auto","base_path":"/synctv/files","access_key_id":"access-key","secret_access_key":"secret-key","public_base_url":"https://cdn.example.com/files","upload_expires_seconds":600}},"database_files":{"type":"database","database":{"compression":"none"}}}"#,
        ),
    ]))
    .checked("file storage S3 env overrides should parse");

    assert_eq!(config.file_storage.default_backend, "s3_public");
    assert_eq!(
        config.file_storage.backend_for_chat_attachments(),
        "s3_public"
    );
    assert_eq!(
        config.file_storage.upload_token_secret,
        "upload-token-secret"
    );
    assert_eq!(
        config.file_storage.unreferenced_object_retention_seconds,
        7200
    );
    let s3 = &config
        .file_storage
        .backends
        .get("s3_public")
        .checked("s3 backend")
        .s3;
    assert_eq!(s3.endpoint, "https://s3.example.com");
    assert_eq!(s3.bucket, "synctv-files");
    assert_eq!(s3.region, "auto");
    assert_eq!(s3.base_path, "synctv/files/");
    assert_eq!(s3.access_key_id, "access-key");
    assert_eq!(s3.secret_access_key, "secret-key");
    assert_eq!(
        s3.public_base_url.as_deref(),
        Some("https://cdn.example.com/files")
    );
    assert_eq!(s3.upload_expires_seconds, 600);
    let database = &config
        .file_storage
        .backends
        .get("database_files")
        .checked("database backend")
        .database;
    assert_eq!(database.compression, FileStorageDatabaseCompression::None);
}

#[test]
fn test_file_storage_backend_accepts_disabled_database_and_s3() {
    assert_eq!(
        "disabled"
            .parse::<FileStorageBackendType>()
            .checked("operation should succeed"),
        FileStorageBackendType::Disabled
    );
    assert_eq!(
        "database"
            .parse::<FileStorageBackendType>()
            .checked("operation should succeed"),
        FileStorageBackendType::Database
    );
    assert_eq!(
        "s3".parse::<FileStorageBackendType>()
            .checked("operation should succeed"),
        FileStorageBackendType::S3
    );
    assert!("metadata".parse::<FileStorageBackendType>().is_err());
    assert_eq!(
        FileStorageConfig::default().backend_for_chat_attachments(),
        "disabled"
    );
    assert_eq!(
        FileStorageConfig::default().unreferenced_object_retention_seconds,
        86_400
    );
    assert_eq!(
        FileStorageBackendConfig::default().database.compression,
        FileStorageDatabaseCompression::Zstd
    );
    assert_eq!(
        "none"
            .parse::<FileStorageDatabaseCompression>()
            .checked("operation should succeed"),
        FileStorageDatabaseCompression::None
    );
    assert_eq!(
        "lz4"
            .parse::<FileStorageDatabaseCompression>()
            .checked("operation should succeed"),
        FileStorageDatabaseCompression::Lz4
    );
    assert!("gzip".parse::<FileStorageDatabaseCompression>().is_err());
}

#[test]
fn test_validate_file_storage_unreferenced_retention_floor() {
    let mut config = valid_prod_config();
    config.file_storage.unreferenced_object_retention_seconds = 3599;

    let errors = config.validate().failed("operation should fail");

    assert!(
        errors
            .iter()
            .any(|e| e.contains("file_storage.unreferenced_object_retention_seconds")),
        "Expected unreferenced file retention validation error, got: {errors:?}"
    );
}

#[test]
fn test_from_env_overrides_local_media_provider_timeouts() {
    let config = Config::from_env_map(&env_map(&[
        ("SYNCTV_MEDIA_PROVIDERS_ALIST_REQUEST_TIMEOUT_SECONDS", "40"),
        ("SYNCTV_MEDIA_PROVIDERS_ALIST_CONNECT_TIMEOUT_SECONDS", "8"),
        (
            "SYNCTV_MEDIA_PROVIDERS_BILIBILI_REQUEST_TIMEOUT_SECONDS",
            "50",
        ),
        (
            "SYNCTV_MEDIA_PROVIDERS_BILIBILI_CONNECT_TIMEOUT_SECONDS",
            "9",
        ),
        ("SYNCTV_MEDIA_PROVIDERS_EMBY_REQUEST_TIMEOUT_SECONDS", "60"),
        ("SYNCTV_MEDIA_PROVIDERS_EMBY_CONNECT_TIMEOUT_SECONDS", "10"),
    ]))
    .checked("local media provider env overrides should parse");

    assert_eq!(config.media_providers.alist.request_timeout_seconds, 40);
    assert_eq!(config.media_providers.alist.connect_timeout_seconds, 8);
    assert_eq!(config.media_providers.bilibili.request_timeout_seconds, 50);
    assert_eq!(config.media_providers.bilibili.connect_timeout_seconds, 9);
    assert_eq!(config.media_providers.emby.request_timeout_seconds, 60);
    assert_eq!(config.media_providers.emby.connect_timeout_seconds, 10);
}

#[test]
fn test_validate_local_media_provider_timeouts() {
    let mut config = valid_prod_config();
    config.media_providers.alist.request_timeout_seconds = 0;
    config.media_providers.bilibili.connect_timeout_seconds = 31;
    config.media_providers.bilibili.request_timeout_seconds = 30;
    config.media_providers.emby.request_timeout_seconds = 301;

    let errors = config
        .validate()
        .failed("invalid local provider timeout config must fail validation");

    assert!(errors
        .iter()
        .any(|error| error.contains("media_providers.alist.request_timeout_seconds")));
    assert!(errors
        .iter()
        .any(|error| error.contains("media_providers.bilibili.connect_timeout_seconds")));
    assert!(errors
        .iter()
        .any(|error| error.contains("media_providers.emby.request_timeout_seconds")));
}

#[test]
fn test_public_rtmp_host_prefers_explicit_override() {
    let mut config = Config::default();
    config.server.advertise_host = "10.0.0.12".to_string();
    config.livestream.public_rtmp_host = "stream.example.com".to_string();

    assert_eq!(config.public_rtmp_host(), "stream.example.com");
}

#[test]
fn test_public_rtmp_host_does_not_reuse_cluster_advertise_host() {
    let mut config = Config::default();
    config.server.host = "0.0.0.0".to_string();
    config.server.advertise_host = "10.0.0.12".to_string();
    config.livestream.public_rtmp_host.clear();

    assert_eq!(config.public_rtmp_host(), "127.0.0.1");
}

#[test]
fn test_public_rtmp_host_does_not_reuse_pod_ip() {
    let mut config = Config::default();
    config.server.host = "0.0.0.0".to_string();
    config.server.advertise_host.clear();
    config.livestream.public_rtmp_host.clear();

    assert_eq!(
        config.public_rtmp_host_without_internal_advertise_fallback(),
        "127.0.0.1"
    );
}

#[test]
fn test_public_rtmp_host_falls_back_to_local_loopback_for_wildcard_bind() {
    let mut config = Config::default();
    config.server.host = "0.0.0.0".to_string();
    config.server.advertise_host.clear();
    config.livestream.public_rtmp_host.clear();

    assert_eq!(config.public_rtmp_host(), "127.0.0.1");
}

#[test]
fn test_public_rtmp_host_falls_back_to_bound_host_when_specific() {
    let mut config = Config::default();
    config.server.host = "192.168.10.15".to_string();
    config.server.advertise_host.clear();
    config.livestream.public_rtmp_host.clear();

    assert_eq!(config.public_rtmp_host(), "192.168.10.15");
}

#[test]
fn test_public_rtmp_host_formats_ipv6_bind_host_for_urls() {
    let mut config = Config::default();
    config.server.host = "::".to_string();
    config.server.advertise_host.clear();
    config.livestream.public_rtmp_host.clear();

    assert_eq!(config.public_rtmp_host(), "[::1]");
}

#[test]
fn test_validate_livestream_zero_timeout() {
    let mut config = valid_prod_config();
    config.livestream.stream_timeout_seconds = 0;
    let errors = config.validate().failed("operation should fail");
    assert!(errors.iter().any(|e| e.contains("stream_timeout_seconds")));
}

#[test]
fn test_validate_webrtc_p2p_mode_allowed_in_cluster() {
    let mut config = valid_prod_config();
    config.cluster.enabled = true;
    config.cluster.secret = "shared-secret-123".to_string();
    config.server.advertise_host = "10.0.0.12".to_string();
    config.webrtc.mode = WebRTCMode::PeerToPeer;
    config.webrtc.stun_external_addr = "203.0.113.1:3478".to_string();
    assert!(config.validate().is_ok());
}

#[test]
fn test_validate_webrtc_signaling_only_mode_allowed_in_cluster() {
    let mut config = valid_prod_config();
    config.cluster.enabled = true;
    config.cluster.secret = "shared-secret-123".to_string();
    config.server.advertise_host = "10.0.0.12".to_string();
    config.webrtc.mode = WebRTCMode::SignalingOnly;
    config.webrtc.stun_external_addr = "203.0.113.1:3478".to_string();
    assert!(config.validate().is_ok());
}

#[test]
fn test_validate_redis_standalone_mode_allowed() {
    let config = valid_prod_config();
    // Default is Standalone, should pass
    assert!(config.validate().is_ok());
}

#[test]
fn test_validate_redis_sentinel_mode_allowed() {
    let mut config = valid_prod_config();
    config.redis.deployment_mode = RedisDeploymentMode::Sentinel;
    config.redis.sentinel_master_name = Some("mymaster".to_string());
    config.redis.sentinel_addresses = vec!["127.0.0.1:26379".to_string()];
    assert!(config.validate().is_ok());
}

#[test]
fn test_validate_cluster_enabled_requires_redis() {
    let mut config = valid_prod_config();
    config.cluster.enabled = true;
    config.redis.url = String::new();
    // cluster.enabled=true with no Redis URL must produce an error
    let errors = config.validate().failed("operation should fail");
    assert!(
        errors
            .iter()
            .any(|e| e.contains("distributed mode requires Redis to be configured")),
        "Expected cluster+no-redis error, got: {errors:?}"
    );
}

#[test]
fn test_validate_cluster_enabled_with_redis_ok() {
    let mut config = valid_prod_config();
    config.cluster.enabled = true;
    config.server.advertise_host = "10.0.0.12".to_string();
    // valid_prod_config() includes redis.url, so this should pass
    // (assuming webrtc.stun_external_addr is set for cluster mode)
    config.webrtc.stun_external_addr = "203.0.113.1:3478".to_string();
    assert!(config.validate().is_ok());
}

#[test]
fn test_validate_cluster_enabled_allows_builtin_stun_without_explicit_external_addr() {
    let mut config = valid_prod_config();
    config.cluster.enabled = true;
    config.server.advertise_host = "10.0.0.12".to_string();
    config.webrtc.enable_builtin_stun = true;
    config.webrtc.stun_external_addr.clear();

    assert!(
        config.validate().is_ok(),
        "cluster mode should not reject STUN auto-detection paths during config validation"
    );
}

#[test]
fn test_validate_cluster_enabled_with_sentinel_rejects_k8s_lease() {
    let mut config = valid_prod_config();
    config.cluster.enabled = true;
    config.redis.url = String::new();
    config.redis.deployment_mode = RedisDeploymentMode::Sentinel;
    config.redis.sentinel_master_name = Some("mymaster".to_string());
    config.redis.sentinel_addresses = vec!["127.0.0.1:26379".to_string()];
    config.cluster.leader_election_mode = ClusterLeaderElectionMode::K8sLease;
    config.webrtc.stun_external_addr = "203.0.113.1:3478".to_string();

    let errors = config
        .validate_with_env_map(&env_map(&[
            ("POD_NAME", "synctv-0"),
            ("POD_NAMESPACE", "default"),
        ]))
        .failed("operation should fail");

    assert!(
        errors
            .iter()
            .any(|e| e.contains("cluster.enabled=true is not supported with Redis Sentinel")),
        "expected Sentinel + k8s_lease rejection while Redis locks are still required, got: {errors:?}"
    );
}

#[test]
fn test_validate_cluster_enabled_with_sentinel_rejects_redis_leader_election() {
    let mut config = valid_prod_config();
    config.cluster.enabled = true;
    config.redis.url = String::new();
    config.redis.deployment_mode = RedisDeploymentMode::Sentinel;
    config.redis.sentinel_master_name = Some("mymaster".to_string());
    config.redis.sentinel_addresses = vec!["127.0.0.1:26379".to_string()];
    config.cluster.leader_election_mode = ClusterLeaderElectionMode::Redis;
    config.webrtc.stun_external_addr = "203.0.113.1:3478".to_string();

    let errors = config.validate().failed("operation should fail");
    assert!(
        errors
            .iter()
            .any(|e| e.contains("cluster.enabled=true is not supported with Redis Sentinel")),
        "expected Sentinel rejection in cluster mode, got: {errors:?}"
    );
}

#[test]
fn test_validate_cluster_secret_without_cluster_enabled_is_standalone() {
    let mut config = valid_prod_config();
    // cluster.secret alone must not implicitly enable cluster mode.
    config.cluster.secret = "shared-secret-long-enough".to_string();
    config.redis.url = String::new();
    config.livestream.hls_storage_backend = HlsStorageBackend::File;
    config.webrtc.stun_external_addr = String::new();
    assert!(
        config.validate().is_ok(),
        "cluster.secret alone should not require cluster runtime services"
    );
}

#[test]
fn test_validate_metrics_endpoint_requires_bearer_token_when_enabled() {
    let mut config = valid_prod_config();
    config.metrics.enabled = true;
    config.metrics.auth.mode = MetricsAuthMode::BearerToken;
    config.metrics.auth.bearer_token.clear();

    let errors = config
        .validate()
        .failed("metrics endpoint must fail closed when enabled without auth");

    assert!(
        errors.iter().any(|e| {
            e.contains("metrics.auth.bearer_token")
                && e.contains("metrics.enabled")
                && e.contains("must be set")
        }),
        "unexpected errors: {errors:?}"
    );
}

#[test]
fn test_validate_metrics_endpoint_accepts_bearer_token_when_enabled() {
    let mut config = valid_prod_config();
    config.metrics.enabled = true;
    config.metrics.auth.mode = MetricsAuthMode::BearerToken;
    config.metrics.auth.bearer_token = "metrics-secret".to_string();

    assert!(
        config.validate().is_ok(),
        "authenticated metrics endpoint should be allowed"
    );
}

#[test]
fn test_validate_metrics_endpoint_accepts_basic_auth_when_enabled() {
    let mut config = valid_prod_config();
    config.metrics.enabled = true;
    config.metrics.auth.mode = MetricsAuthMode::Basic;
    config.metrics.auth.basic_username = "metrics".to_string();
    config.metrics.auth.basic_password = "metrics-password".to_string();

    assert!(
        config.validate().is_ok(),
        "basic-authenticated metrics endpoint should be allowed"
    );
}

#[test]
fn test_validate_metrics_endpoint_requires_basic_password_when_basic_auth_enabled() {
    let mut config = valid_prod_config();
    config.metrics.enabled = true;
    config.metrics.auth.mode = MetricsAuthMode::Basic;
    config.metrics.auth.basic_username = "metrics".to_string();
    config.metrics.auth.basic_password.clear();

    let errors = config
        .validate()
        .failed("metrics basic auth must reject missing password");

    assert!(
        errors
            .iter()
            .any(|e| e.contains("metrics.auth.basic_password")),
        "unexpected errors: {errors:?}"
    );
}

#[test]
fn test_validate_metrics_tls_requires_cert_and_key_paths() {
    let mut config = valid_prod_config();
    config.metrics.enabled = true;
    config.metrics.auth.mode = MetricsAuthMode::BearerToken;
    config.metrics.auth.bearer_token = "metrics-secret".to_string();
    config.metrics.tls.enabled = true;
    config.metrics.tls.cert_path.clear();
    config.metrics.tls.key_path.clear();

    let errors = config
        .validate()
        .failed("metrics TLS must require cert and key paths");

    assert!(
        errors.iter().any(|e| e.contains("metrics.tls.cert_path")),
        "unexpected errors: {errors:?}"
    );
    assert!(
        errors.iter().any(|e| e.contains("metrics.tls.key_path")),
        "unexpected errors: {errors:?}"
    );
}

#[test]
fn test_validate_cluster_enabled_requires_cluster_secret() {
    // cluster.enabled=true + cluster.secret empty must be an error.
    let mut config = valid_prod_config();
    config.cluster.enabled = true;
    config.cluster.secret = String::new(); // clear the secret
    let errors = config.validate().failed("operation should fail");
    assert!(
        errors
            .iter()
            .any(|e| e.contains("cluster.secret must be set when distributed mode is enabled")),
        "Expected cluster.secret error, got: {errors:?}"
    );
}

#[test]
fn test_validate_standalone_redis_without_cluster_secret_ok() {
    // Standalone mode (cluster.enabled=false) with Redis but no cluster.secret is OK.
    let mut config = valid_prod_config();
    config.cluster.enabled = false;
    config.cluster.secret = String::new();
    assert!(
        config.validate().is_ok(),
        "Expected Ok in standalone mode without cluster.secret"
    );
}

#[test]
fn test_validate_cluster_enabled_with_cluster_secret_ok() {
    // cluster.enabled=true + cluster.secret set should pass.
    let mut config = valid_prod_config();
    config.cluster.enabled = true;
    config.server.advertise_host = "10.0.0.12".to_string();
    assert!(
        config.validate().is_ok(),
        "Expected Ok with cluster mode + cluster.secret set"
    );
}

#[test]
fn test_validate_cluster_secret_too_short_rejected() {
    // cluster.secret too short must be rejected.
    let mut config = valid_prod_config();
    config.cluster.enabled = true;
    config.cluster.secret = "short".to_string();
    let errors = config.validate().failed("operation should fail");
    assert!(
        errors
            .iter()
            .any(|e| e.contains("cluster.secret is too short")),
        "Expected short cluster.secret error, got: {errors:?}"
    );
}

#[test]
fn test_from_env_rejects_unknown_cluster_discovery_mode() {
    let error = Config::from_env_map(&env_map(&[("SYNCTV_CLUSTER_DISCOVERY_MODE", "mystery")]))
        .failed("invalid discovery mode override must fail closed");

    assert!(
        error.to_string().contains("SYNCTV_CLUSTER_DISCOVERY_MODE")
            && error
                .to_string()
                .contains(ClusterDiscoveryMode::ALLOWED_VALUES),
        "Expected discovery_mode parse error, got: {error}"
    );
}

#[test]
fn test_from_env_rejects_unknown_cluster_leader_election_mode() {
    let error = Config::from_env_map(&env_map(&[(
        "SYNCTV_CLUSTER_LEADER_ELECTION_MODE",
        "mystery",
    )]))
    .failed("invalid leader election mode override must fail closed");

    assert!(
        error
            .to_string()
            .contains("SYNCTV_CLUSTER_LEADER_ELECTION_MODE")
            && error
                .to_string()
                .contains(ClusterLeaderElectionMode::ALLOWED_VALUES),
        "Expected leader_election_mode parse error, got: {error}"
    );
}

#[test]
fn test_validate_k8s_dns_requires_env_vars_in_cluster_mode() {
    let mut config = valid_prod_config();
    config.cluster.enabled = true;
    config.cluster.discovery_mode = ClusterDiscoveryMode::K8sDns;

    let errors = config
        .validate_with_env_map(&HashMap::new())
        .failed("operation should fail");

    assert!(
        errors
            .iter()
            .any(|e| e.contains("HEADLESS_SERVICE_NAME") && e.contains("k8s_dns")),
        "Expected HEADLESS_SERVICE_NAME validation error, got: {errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("POD_NAMESPACE") && e.contains("k8s_dns")),
        "Expected POD_NAMESPACE validation error, got: {errors:?}"
    );
    assert!(
        errors
            .iter()
            .all(|e| !e.contains("POD_IP") || !e.contains("k8s_dns")),
        "Offline validation must not require runtime-only POD_IP, got: {errors:?}"
    );
}

#[test]
fn test_validate_k8s_lease_requires_env_vars_in_cluster_mode() {
    let mut config = valid_prod_config();
    config.cluster.enabled = true;
    config.cluster.leader_election_mode = ClusterLeaderElectionMode::K8sLease;

    let errors = config
        .validate_with_env_map(&HashMap::new())
        .failed("operation should fail");

    assert!(
        errors
            .iter()
            .any(|e| e.contains("POD_NAME") && e.contains("k8s_lease")),
        "Expected POD_NAME validation error, got: {errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("POD_NAMESPACE") && e.contains("k8s_lease")),
        "Expected POD_NAMESPACE validation error, got: {errors:?}"
    );

    if !cfg!(feature = "k8s") {
        assert!(
            errors
                .iter()
                .any(|e| e.contains("k8s_lease") && e.contains("requires the 'k8s' feature")),
            "Expected k8s feature validation error, got: {errors:?}"
        );
    }
}

#[test]
fn test_validate_k8s_dns_requires_compiled_k8s_support() {
    if cfg!(feature = "k8s") {
        return;
    }

    let mut config = valid_prod_config();
    config.cluster.enabled = true;
    config.cluster.discovery_mode = ClusterDiscoveryMode::K8sDns;

    let errors = config
        .validate_with_env_map(&env_map(&[
            ("HEADLESS_SERVICE_NAME", "synctv-headless"),
            ("POD_NAMESPACE", "default"),
        ]))
        .failed("operation should fail");

    assert!(
        errors
            .iter()
            .any(|e| e.contains("k8s_dns") && e.contains("requires the 'k8s' feature")),
        "Expected k8s feature validation error, got: {errors:?}"
    );
}

#[test]
fn test_validate_k8s_lease_requires_compiled_k8s_support() {
    if cfg!(feature = "k8s") {
        return;
    }

    let mut config = valid_prod_config();
    config.cluster.enabled = true;
    config.cluster.leader_election_mode = ClusterLeaderElectionMode::K8sLease;

    let errors = config
        .validate_with_env_map(&env_map(&[
            ("POD_NAME", "synctv-0"),
            ("POD_NAMESPACE", "default"),
        ]))
        .failed("operation should fail");

    assert!(
        errors
            .iter()
            .any(|e| e.contains("k8s_lease") && e.contains("requires the 'k8s' feature")),
        "Expected k8s feature validation error, got: {errors:?}"
    );
}

#[test]
fn test_validate_shared_file_hls_storage_requires_storage_path() {
    let mut config = valid_prod_config();
    config.livestream.hls_storage_backend = HlsStorageBackend::SharedFile;
    config.livestream.hls_storage_path = String::new();

    let errors = config.validate().failed("operation should fail");

    assert!(
        errors
            .iter()
            .any(|e| e.contains("hls_storage_path") && e.contains("must be set")),
        "Expected hls_storage_path validation error, got: {errors:?}"
    );
}

#[test]
fn test_validate_oss_hls_storage_requires_required_fields() {
    let mut config = valid_prod_config();
    config.cluster.enabled = false;
    config.cluster.secret.clear();
    config.livestream.hls_storage_backend = HlsStorageBackend::Oss;
    config.livestream.hls_oss = HlsOssConfig::default();

    let errors = config.validate().failed("operation should fail");

    assert!(
        errors.iter().any(|e| e.contains("hls_oss.endpoint")),
        "Expected hls_oss.endpoint validation error, got: {errors:?}"
    );
    assert!(
        errors.iter().any(|e| e.contains("hls_oss.bucket")),
        "Expected hls_oss.bucket validation error, got: {errors:?}"
    );
    assert!(
        errors.iter().any(|e| e.contains("hls_oss.access_key_id")),
        "Expected hls_oss.access_key_id validation error, got: {errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("hls_oss.secret_access_key")),
        "Expected hls_oss.secret_access_key validation error, got: {errors:?}"
    );
}

#[test]
fn test_validate_file_s3_storage_requires_required_fields() {
    let mut config = valid_prod_config();
    config.cluster.enabled = false;
    config.cluster.secret.clear();
    config.file_storage.default_backend = "broken_s3".to_string();
    config.file_storage.chat_attachments_backend = "broken_s3".to_string();
    config.file_storage.backends.insert(
        "broken_s3".to_string(),
        FileStorageBackendConfig {
            backend_type: FileStorageBackendType::S3,
            database: FileStorageDatabaseConfig::default(),
            s3: FileStorageS3Config {
                upload_expires_seconds: 0,
                ..FileStorageS3Config::default()
            },
        },
    );

    let errors = config.validate().failed("operation should fail");

    assert!(
        errors
            .iter()
            .any(|e| e.contains("file_storage.backends.broken_s3.s3.endpoint")),
        "Expected chat S3 endpoint validation error, got: {errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("file_storage.backends.broken_s3.s3.bucket")),
        "Expected chat S3 bucket validation error, got: {errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("file_storage.backends.broken_s3.s3.access_key_id")),
        "Expected chat S3 access_key_id validation error, got: {errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("file_storage.backends.broken_s3.s3.secret_access_key")),
        "Expected chat S3 secret_access_key validation error, got: {errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("file_storage.backends.broken_s3.s3.upload_expires_seconds")),
        "Expected chat S3 upload_expires_seconds validation error, got: {errors:?}"
    );
}
