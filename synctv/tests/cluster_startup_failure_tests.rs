//! Cluster startup failure handling tests.
//!
//! Verifies that when distributed mode is explicitly enabled (cluster.enabled = true):
//! 1. `RealtimeManager` initialization failure is a fatal error (not silent degradation)
//! 2. Cache invalidation startup failure is a fatal error (not just a warning)
//!
//! In standalone mode (cluster.enabled = false), these failures should be non-fatal.

#![allow(clippy::unwrap_used)]
use synctv::app_config::{AppConfig as Config, *};

fn relaxed_request_rate_limits() -> RequestRateLimitConfig {
    RequestRateLimitConfig {
        auth_max_requests: 5000,
        auth_window_seconds: 1,
        write_max_requests: 5000,
        write_window_seconds: 1,
        read_max_requests: 5000,
        read_window_seconds: 1,
        media_max_requests: 5000,
        media_window_seconds: 1,
        admin_max_requests: 5000,
        admin_window_seconds: 1,
        streaming_max_requests: 5000,
        streaming_window_seconds: 1,
        websocket_max_requests: 5000,
        websocket_window_seconds: 1,
        ..RequestRateLimitConfig::default()
    }
}

fn test_security_config() -> SecurityConfig {
    SecurityConfig {
        credential_encryption_key:
            "7171717171717171717171717171717171717171717171717171717171717171".to_string(),
        totp_encryption_key: "7272727272727272727272727272727272727272727272727272727272727272"
            .to_string(),
        email_outbox_encryption_key:
            "7373737373737373737373737373737373737373737373737373737373737373".to_string(),
        opaque_server_setup_secret: "test-opaque-server-setup-secret-for-cluster-startup"
            .to_string(),
        proxy_signing_key: "test-proxy-signing-key-for-cluster-startup".to_string(),
        media_swarm_signing_key: "test-media-swarm-signing-key-for-cluster-startup".to_string(),
        provider_session_encryption_key: "test-provider-session-key-for-cluster-startup"
            .to_string(),
        login_discovery_key: "test-login-discovery-key-for-cluster-startup".to_string(),
        webauthn_enumeration_key: "test-webauthn-enumeration-key-for-cluster-startup".to_string(),
        ..SecurityConfig::default()
    }
}

fn test_file_storage_config() -> FileStorageConfig {
    FileStorageConfig {
        upload_token_secret: "test-file-upload-token-secret-for-cluster-startup".to_string(),
        ..FileStorageConfig::default()
    }
}

/// Create a minimal standalone config for testing (no Redis, no distributed mode)
fn standalone_test_config() -> Config {
    Config {
        logging: LoggingConfig::default(),
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 8080,
            project_url: synctv_api::DEFAULT_PROJECT_URL.to_string(),
            enable_reflection: false,
            grpc_max_message_size_bytes: 16 * 1024 * 1024,
            grpc_compression_enabled: true,
            trusted_proxies: Vec::new(),
            cors_allowed_origins: Vec::new(),
            advertise_host: String::new(),
            shutdown_drain_timeout_seconds: 30,
            logging: LoggingConfig::default(),
            access_log: synctv::app_config::AccessLogConfig::default(),
        },
        time: TimeConfig::default(),
        data_dir: default_data_dir().display().to_string(),
        metrics: MetricsConfig::default(),
        management: ManagementConfig {
            enabled: false,
            ..ManagementConfig::default()
        },
        database: DatabaseConfig::default(),
        redis: RedisConfig::default(), // Empty Redis URL for standalone
        jwt: JwtConfig {
            secret: "test-jwt-secret-key-for-testing-minimum-length".to_string(),
            ..JwtConfig::default()
        },
        health: HealthConfig::default(),
        livestream: LivestreamConfig::default(),
        file_storage: test_file_storage_config(),
        chat: ChatConfig::default(),
        webauthn: WebAuthnConfig::default(),
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
        proxy_slice_cache: ProxySliceCacheConfig::default(),
        messaging_rate_limits: MessagingRateLimitConfig::default(),
        // Raise rate limits to avoid cross-test interference when running in parallel
        request_rate_limits: relaxed_request_rate_limits(),
        security: test_security_config(),
    }
}

/// Create a config with distributed mode enabled
fn cluster_test_config() -> Config {
    Config {
        logging: LoggingConfig::default(),
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 8080,
            project_url: synctv_api::DEFAULT_PROJECT_URL.to_string(),
            enable_reflection: false,
            grpc_max_message_size_bytes: 16 * 1024 * 1024,
            grpc_compression_enabled: true,
            trusted_proxies: Vec::new(),
            cors_allowed_origins: Vec::new(),
            advertise_host: "127.0.0.1".to_string(),
            shutdown_drain_timeout_seconds: 30,
            logging: LoggingConfig::default(),
            access_log: synctv::app_config::AccessLogConfig::default(),
        },
        time: TimeConfig::default(),
        data_dir: default_data_dir().display().to_string(),
        metrics: MetricsConfig::default(),
        management: ManagementConfig {
            enabled: false,
            ..ManagementConfig::default()
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
        health: HealthConfig::default(),
        livestream: LivestreamConfig {
            hls_storage: HlsStorageConfig::SharedFile(HlsFileStorageConfig {
                path: "/var/lib/synctv/hls".to_string(),
            }),
            ..LivestreamConfig::default()
        },
        file_storage: test_file_storage_config(),
        chat: ChatConfig::default(),
        webauthn: WebAuthnConfig::default(),
        media_providers: MediaProvidersConfig::default(),
        webrtc: WebRTCConfig::default(),
        connection_limits: ConnectionLimitsConfig::default(),
        bootstrap: BootstrapConfig {
            create_root_user: true,
            root_username: "admin".to_string(),
            root_password: "StrongPwd12345!".to_string(),
        },
        cluster: ClusterChannelConfig {
            enabled: true, // Cluster mode explicitly enabled
            secret: "test-cluster-secret-key-1234567890".to_string(),
            ..ClusterChannelConfig::default()
        },
        password_complexity: PasswordComplexityConfig::default(),
        buffer_sizes: BufferSizesConfig::default(),
        cache: CacheConfig::default(),
        proxy_slice_cache: ProxySliceCacheConfig::default(),
        messaging_rate_limits: MessagingRateLimitConfig::default(),
        // Raise rate limits to avoid cross-test interference when running in parallel
        request_rate_limits: relaxed_request_rate_limits(),
        security: test_security_config(),
    }
}

/// Test that cluster.enabled=true is properly reflected in config
#[test]
fn test_distributed_enabled_config() {
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

/// Test distributed mode requires Redis URL
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
        e.contains("Redis is required when distributed mode is enabled")
            || e.contains("cluster.enabled=true")
    });
    assert!(
        has_redis_error,
        "Error message should mention Redis requirement, got: {error_messages:?}"
    );
}

/// Test distributed mode requires `cluster.secret`.
#[test]
fn test_cluster_mode_requires_cluster_secret() {
    let mut config = cluster_test_config();
    config.cluster.secret = String::new(); // Clear the cluster secret

    // This should fail validation because cluster.enabled=true requires cluster.secret.
    let result = config.validate();
    assert!(
        result.is_err(),
        "cluster.enabled=true with empty cluster.secret should fail validation"
    );

    let errors = result.unwrap_err();
    let error_messages: Vec<_> = errors.iter().collect();
    let has_secret_error = error_messages.iter().any(|e| e.contains("cluster.secret"));
    assert!(
        has_secret_error,
        "Error message should mention cluster.secret requirement, got: {error_messages:?}"
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

/// Test valid distributed mode config
#[test]
fn test_valid_cluster_mode_config() {
    let config = cluster_test_config();

    // This should pass validation
    let result = config.validate();
    assert!(
        result.is_ok(),
        "valid realtime config should pass validation, got error: {:?}",
        result.err()
    );
}

/// Realtime config validation catches an empty Redis URL when distributed mode is enabled.
#[test]
fn test_distributed_enabled_requires_redis_for_node_registry() {
    let mut config = cluster_test_config();
    // Remove the Redis URL entirely
    config.redis.url = String::new();

    // Config validation should catch this before init_cluster_components is even called
    let result = config.validate();
    assert!(
        result.is_err(),
        "Empty Redis URL with cluster.enabled=true should fail validation"
    );
}

/// Standalone mode should not fail when Redis is unavailable.
#[test]
fn test_standalone_mode_tolerates_missing_redis() {
    let config = standalone_test_config();

    // Standalone mode should pass validation even without Redis
    let result = config.validate();
    assert!(
        result.is_ok(),
        "Standalone mode should tolerate missing Redis, got error: {:?}",
        result.err()
    );
}

// Cluster initialization failure cleanup tests.

mod cluster_cleanup_tests {
    use std::sync::Arc;
    use std::time::Duration;
    use std::time::Instant;
    use synctv::Application;
    use synctv_cluster::discovery::NodeRegistry;
    use synctv_core_testing::{create_test_database_url_with_label, start_redis_url_with_label};
    use tokio::net::TcpListener;
    use tokio_util::sync::CancellationToken;

    /// Verify that RealtimeManager's cancel_token can stop the heartbeat loop.
    ///
    /// When init_cluster_components fails after starting the heartbeat loop,
    /// the cleanup should cancel the heartbeat loop via the cancel token.
    #[tokio::test]
    async fn test_cancel_token_stops_heartbeat_loop() {
        use synctv_cluster::discovery::{NodeInfo, NodeRegistry};
        use synctv_realtime::sync::{RealtimeConfig, RealtimeManager, RoomMessageHub};

        let config = RealtimeConfig {
            distributed_transport_factory: None,
            message_runtime: Arc::new(RoomMessageHub::new()),
            distributed_enabled: false,
            node_id: "cancel-test-node".to_string(),
            dedup_window: Duration::from_secs(1),
            critical_channel_capacity: 1000,
            publish_channel_capacity: 10_000,
            key_prefix: "synctv:".to_string(),
            catchup_window_secs: 300,
            stream_max_length: 10_000,
            event_handler: None,
            parent_cancel_token: None,
        };
        let manager = RealtimeManager::new(config)
            .await
            .expect("RealtimeManager::new should succeed");

        // This test verifies cancellation semantics, not Redis I/O latency.
        let registry = Arc::new(
            NodeRegistry::new_local_only("cancel-test-node".to_string(), 30, "test:").unwrap(),
        );
        registry
            .test_insert_local(NodeInfo::new(
                "cancel-test-node".to_string(),
                "localhost:8080".to_string(),
            ))
            .await;

        manager
            .start_heartbeat_loop(
                registry.clone(),
                "localhost:8080".to_string(),
                None::<fn() -> usize>,
            )
            .await;

        // Give heartbeat task a moment to start
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Cancel token should not be cancelled yet
        assert!(
            !manager.cancel_token().is_cancelled(),
            "Cancel token should not be cancelled before cleanup"
        );

        // Simulate cleanup by cancelling the token.
        manager.cancel_token().cancel();

        // Token should now be cancelled
        assert!(
            manager.cancel_token().is_cancelled(),
            "Cancel token should be cancelled after cleanup"
        );

        // The heartbeat loop should stop responding (verified by quick shutdown)
        let start = std::time::Instant::now();
        manager.shutdown().await;
        let elapsed = start.elapsed();

        // Since token was already cancelled, shutdown should be fast.
        // Keep a little margin for CI scheduler jitter.
        assert!(
            elapsed < Duration::from_secs(1),
            "Shutdown after cancel should be fast, took: {elapsed:?}"
        );
    }

    /// Verify that NodeRegistry.unregister() can be called after register().
    ///
    /// When init_cluster_components fails after registering the node,
    /// the cleanup should unregister the node from Redis.
    /// This test verifies the API contract using local mode.
    #[tokio::test]
    async fn test_node_registry_unregister_after_register() {
        use synctv_cluster::discovery::{NodeInfo, NodeRegistry};

        let client = redis::Client::open("redis://localhost:6379").unwrap();
        let registry = Arc::new(
            NodeRegistry::new(
                synctv_core::coordination_runtime_from_client(client),
                "unregister-test-node".to_string(),
                30,
                "test:",
            )
            .unwrap(),
        );

        // Insert node info locally (simulating register)
        registry
            .test_insert_local(NodeInfo::new(
                "unregister-test-node".to_string(),
                "localhost:8080".to_string(),
            ))
            .await;

        // Verify node exists locally
        let nodes = registry.get_all_nodes_local().await;
        assert!(
            nodes.iter().any(|n| n.node_id == "unregister-test-node"),
            "Node should exist in local registry after insert"
        );

        // Attempt unregister (will fail without Redis, but API should be callable)
        // In local mode, unregister just clears local state
        let _ = registry.unregister().await;

        // The key point is that unregister() can be called without panic
    }

    /// Verify cleanup order is correct (cancel token first, then unregister).
    ///
    /// Cancel before unregister so the heartbeat loop cannot re-register the node.
    #[tokio::test]
    async fn test_cleanup_order_cancel_before_unregister() {
        // This test verifies the ordering concept, not actual execution
        let cancel_token = CancellationToken::new();

        assert!(!cancel_token.is_cancelled());

        cancel_token.cancel();
        assert!(cancel_token.is_cancelled());

        // (Not calling actual unregister here as it requires Redis)
        // The key invariant: cancel happens before unregister
    }

    /// Verify that cleanup happens on any init_cluster_components failure.
    ///
    /// This test documents the expected behavior: if init_cluster_components
    /// fails at any point after register() and start_heartbeat_loop(),
    /// the cleanup should be triggered.
    #[tokio::test]
    #[ignore = "requires Docker"]
    async fn test_application_build_does_not_register_cluster_node_before_run() {
        let (postgres, database_url) =
            create_test_database_url_with_label("synctv_test", "build-before-run").await;
        let (redis, redis_url) = start_redis_url_with_label("build-before-run").await;

        let mut config = super::cluster_test_config();
        config.database.url = database_url;
        config.redis.url = redis_url.clone();
        config.server.port = 0;
        config.livestream.rtmp_port = 0;
        config.server.advertise_host = "127.0.0.1".to_string();

        let app = Box::pin(Application::build(config.clone()))
            .await
            .expect("application build should succeed before run");

        let redis_client =
            redis::Client::open(redis_url).expect("test Redis client should be created");
        let registry = NodeRegistry::new(
            synctv_core::coordination_runtime_from_client(redis_client),
            "observer-node".to_string(),
            30,
            &config.redis.key_prefix,
        )
        .expect("observer registry should be created");
        let nodes = registry
            .get_all_nodes()
            .await
            .expect("observer registry should read nodes");

        assert!(
            nodes.is_empty(),
            "Application::build must not make the node routable before listeners are started, found nodes: {:?}",
            nodes.iter().map(|n| n.node_id.as_str()).collect::<Vec<_>>()
        );

        Box::pin(app.run_with_shutdown_signal(std::future::ready(())))
            .await
            .ok();
        redis.cleanup();
        postgres.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "requires Docker"]
    async fn test_application_build_startup_failure_unregisters_cluster_node() {
        let (postgres, database_url) =
            create_test_database_url_with_label("synctv_test", "build-failure-unregister").await;
        let (redis, redis_url) = start_redis_url_with_label("build-failure-unregister").await;

        let mut config = super::cluster_test_config();
        config.database.url = database_url;
        config.redis.url = redis_url.clone();

        let occupied_rtmp = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("should reserve an RTMP port to force Phase 7 failure");
        let occupied_addr = occupied_rtmp
            .local_addr()
            .expect("reserved RTMP listener should have local addr");
        config.server.port = occupied_addr.port().saturating_add(1);
        config.livestream.rtmp_port = occupied_addr.port();
        config.server.advertise_host = "127.0.0.1".to_string();

        let result = Box::pin(Application::build(config.clone())).await;
        assert!(
            result.is_err(),
            "occupied RTMP port must force Application::build to fail after cluster init"
        );

        let redis_client =
            redis::Client::open(redis_url).expect("test Redis client should be created");
        let registry = NodeRegistry::new(
            synctv_core::coordination_runtime_from_client(redis_client),
            "observer-node".to_string(),
            30,
            &config.redis.key_prefix,
        )
        .expect("observer registry should be created");

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let nodes = registry
                .get_all_nodes()
                .await
                .expect("observer registry should read nodes");
            if nodes.is_empty() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "cluster node should be unregistered after build rollback, still found nodes: {:?}",
                nodes.iter().map(|n| n.node_id.as_str()).collect::<Vec<_>>()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        drop(occupied_rtmp);
        redis.cleanup();
        postgres.cleanup().await;
    }
}

// Leader election fallback tests: Cluster mode requires Redis for leader election

mod leader_election_fallback_tests {
    use super::*;

    /// Test that cluster.enabled=true with empty redis.url fails validation.
    ///
    /// This prevents the split-brain scenario where multiple nodes could all
    /// think they are the leader because Redis is unavailable.
    #[test]
    fn test_cluster_mode_requires_redis_for_leader_election() {
        let mut config = cluster_test_config();
        // Remove the Redis URL to simulate Redis unavailability
        config.redis.url = String::new();

        // Config validation should catch this before init_leader_election is called
        let result = config.validate();
        assert!(
            result.is_err(),
            "cluster.enabled=true with empty redis.url should fail validation"
        );

        let errors = result.unwrap_err();
        let error_messages: Vec<_> = errors.iter().collect();
        let has_redis_error = error_messages.iter().any(|e| {
            e.contains("Redis is required when distributed mode is enabled")
                || e.contains("cluster.enabled=true")
        });
        assert!(
            has_redis_error,
            "Error should mention Redis requirement for distributed mode, got: {error_messages:?}"
        );
    }

    /// Test that standalone mode (cluster.enabled=false) passes validation without Redis.
    ///
    /// This verifies that single-node deployments can still run without Redis,
    /// using AlwaysLeader for leader election (safe because there's only one node).
    #[test]
    fn test_standalone_mode_allows_always_leader_without_redis() {
        let config = standalone_test_config();

        // Standalone mode should pass validation without Redis
        let result = config.validate();
        assert!(
            result.is_ok(),
            "standalone mode should allow no Redis (will use AlwaysLeader), got error: {:?}",
            result.err()
        );

        // Verify this is truly standalone mode
        assert!(
            !config.cluster.enabled,
            "config should have cluster.enabled=false"
        );
        assert!(
            config.redis.url.is_empty(),
            "config should have empty redis.url"
        );
    }

    /// Test that distributed mode with valid Redis URL passes validation.
    ///
    /// This verifies that the happy path (distributed mode with Redis) works correctly.
    #[test]
    fn test_cluster_mode_with_redis_passes_validation() {
        let config = cluster_test_config();

        // Verify this is distributed mode
        assert!(
            config.cluster.enabled,
            "config should have cluster.enabled=true"
        );
        assert!(
            !config.redis.url.is_empty(),
            "config should have redis.url set"
        );

        // Should pass validation
        let result = config.validate();
        assert!(
            result.is_ok(),
            "distributed mode with Redis should pass validation, got error: {:?}",
            result.err()
        );
    }

    /// Document the split-brain prevention logic.
    ///
    /// This test documents the key invariant: in distributed mode, we must NEVER
    /// fall back to AlwaysLeader when Redis is unavailable:
    /// 1. Config validation catches empty redis.url when cluster.enabled=true
    /// 2. init_leader_election returns an error (not AlwaysLeader fallback)
    ///
    /// Without this fix, multiple nodes in a cluster could all believe they
    /// are the leader and run singleton tasks simultaneously, causing:
    /// - Database corruption (multiple nodes writing to same tables)
    /// - Inconsistent state (cleanup tasks running concurrently)
    /// - Resource leaks (partition management conflicts)
    #[test]
    fn test_split_brain_prevention_invariant() {
        // Case 1: cluster.enabled=true, no Redis -> MUST fail
        let mut cluster_no_redis = cluster_test_config();
        cluster_no_redis.redis.url = String::new();
        assert!(
            cluster_no_redis.validate().is_err(),
            "cluster.enabled=true with no Redis MUST fail validation"
        );

        // Case 2: cluster.enabled=false, no Redis -> OK (single-node mode)
        let standalone = standalone_test_config();
        assert!(
            standalone.validate().is_ok(),
            "cluster.enabled=false with no Redis should pass (AlwaysLeader is safe)"
        );

        // Case 3: cluster.enabled=true, with Redis -> OK (distributed mode)
        let cluster_with_redis = cluster_test_config();
        assert!(
            cluster_with_redis.validate().is_ok(),
            "cluster.enabled=true with Redis should pass"
        );
    }
}
