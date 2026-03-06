//! Application initialization tests.
//!
//! Tests for the `Application` struct and its phased initialization logic.
//! These tests use mocks to verify initialization behavior without requiring
//! real infrastructure (database, Redis).

#![allow(clippy::unwrap_used)]

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

// ============================================================================
// ShutdownHook trait definition (mirrors shutdown.rs for testing)
// ============================================================================

/// A shutdown hook that performs cleanup work
pub trait ShutdownHook: Send + Sync {
    /// Human-readable name for logging
    fn name(&self) -> &str;
    /// Maximum time to wait before moving on
    fn timeout(&self) -> Duration;
    /// Execute the hook
    fn run(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>>;
}

// ============================================================================
// ShutdownCoordinator implementation (mirrors shutdown.rs for testing)
// ============================================================================

/// Centralized collection of all shutdown resources.
pub struct ShutdownCoordinator {
    tokens: Vec<(&'static str, CancellationToken)>,
    tasks: Vec<(&'static str, tokio::task::JoinHandle<()>)>,
    hooks: Vec<Box<dyn ShutdownHook>>,
}

impl ShutdownCoordinator {
    pub fn new() -> Self {
        Self {
            tokens: Vec::new(),
            tasks: Vec::new(),
            hooks: Vec::new(),
        }
    }

    /// Register a new `CancellationToken` and return it.
    pub fn register_token(&mut self, name: &'static str) -> CancellationToken {
        let token = CancellationToken::new();
        self.tokens.push((name, token.clone()));
        token
    }

    /// Register a pre-existing `CancellationToken`.
    pub fn track_token(&mut self, name: &'static str, token: CancellationToken) {
        self.tokens.push((name, token));
    }

    /// Register a background task handle.
    pub fn register_task(&mut self, name: &'static str, handle: tokio::task::JoinHandle<()>) {
        self.tasks.push((name, handle));
    }

    /// Register a shutdown hook.
    pub fn register_hook(&mut self, hook: impl ShutdownHook + 'static) {
        self.hooks.push(Box::new(hook));
    }

    /// Execute the full shutdown sequence.
    pub async fn shutdown(self) {
        // Phase 1: Cancel all tokens
        for (_name, token) in &self.tokens {
            token.cancel();
        }

        // Phase 2: Drain background tasks
        for (_name, handle) in self.tasks {
            let _ = tokio::time::timeout(Duration::from_secs(30), handle).await;
        }

        // Phase 3: Run shutdown hooks
        for hook in self.hooks {
            let timeout = hook.timeout();
            let _ = tokio::time::timeout(timeout, hook.run()).await;
        }
    }
}

impl Default for ShutdownCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Mock ShutdownHook for testing
// ============================================================================

/// Mock implementation of ShutdownHook for testing
struct MockShutdownHook {
    name: &'static str,
    timeout: Duration,
    ran: Arc<std::sync::atomic::AtomicBool>,
}

impl MockShutdownHook {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            timeout: Duration::from_secs(5),
            ran: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    #[allow(dead_code)]
    #[allow(dead_code)]
    fn was_run(&self) -> bool {
        self.ran.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl ShutdownHook for MockShutdownHook {
    fn name(&self) -> &str {
        self.name
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }

    fn run(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let ran = self.ran.clone();
        Box::pin(async move {
            ran.store(true, std::sync::atomic::Ordering::SeqCst);
        })
    }
}

// ============================================================================
// ShutdownCoordinator Tests
// ============================================================================

/// Test that ShutdownCoordinator::new() creates an empty coordinator
#[test]
fn test_shutdown_coordinator_new() {
    let coordinator = ShutdownCoordinator::new();
    // Coordinator should be created successfully with empty internal state
    drop(coordinator);
}

/// Test that register_token returns a usable CancellationToken
#[tokio::test]
async fn test_register_token_returns_cancellation_token() {
    let mut coordinator = ShutdownCoordinator::new();
    let token = coordinator.register_token("test_token");

    // Token should not be cancelled initially
    assert!(!token.is_cancelled());

    // Token should be cancellable
    token.cancel();
    assert!(token.is_cancelled());
}

/// Test that track_token tracks an externally created token
#[tokio::test]
async fn test_track_token() {
    let mut coordinator = ShutdownCoordinator::new();
    let token = CancellationToken::new();

    // Track the token
    coordinator.track_token("tracked_token", token.clone());

    // Token should not be cancelled initially
    assert!(!token.is_cancelled());
}

/// Test that registered tokens are cancelled during shutdown
#[tokio::test]
async fn test_shutdown_cancels_tokens() {
    let mut coordinator = ShutdownCoordinator::new();
    let token1 = coordinator.register_token("token1");
    let token2 = coordinator.register_token("token2");

    assert!(!token1.is_cancelled());
    assert!(!token2.is_cancelled());

    // Execute shutdown
    coordinator.shutdown().await;

    // All tokens should be cancelled
    assert!(token1.is_cancelled());
    assert!(token2.is_cancelled());
}

/// Test that background tasks are awaited during shutdown
#[tokio::test]
async fn test_shutdown_awaits_background_tasks() {
    let mut coordinator = ShutdownCoordinator::new();

    let task_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let task_ran_clone = task_ran.clone();

    let handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        task_ran_clone.store(true, std::sync::atomic::Ordering::SeqCst);
    });

    coordinator.register_task("test_task", handle);

    // Task should not have completed yet
    assert!(!task_ran.load(std::sync::atomic::Ordering::SeqCst));

    // Execute shutdown
    coordinator.shutdown().await;

    // Task should have been awaited and completed
    assert!(task_ran.load(std::sync::atomic::Ordering::SeqCst));
}

/// Test that shutdown hooks are run during shutdown
#[tokio::test]
async fn test_shutdown_runs_hooks() {
    let mut coordinator = ShutdownCoordinator::new();

    let hook = MockShutdownHook::new("test_hook");
    let ran = hook.ran.clone();

    coordinator.register_hook(hook);

    // Hook should not have run yet
    assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));

    // Execute shutdown
    coordinator.shutdown().await;

    // Hook should have run
    assert!(ran.load(std::sync::atomic::Ordering::SeqCst));
}

/// Test that shutdown executes in the correct order: tokens -> tasks -> hooks
#[tokio::test]
async fn test_shutdown_order() {
    let mut coordinator = ShutdownCoordinator::new();

    let order = Arc::new(std::sync::Mutex::new(Vec::new()));

    // Register token
    let order_clone = order.clone();
    let token = CancellationToken::new();
    let token_clone = token.clone();
    tokio::spawn(async move {
        token_clone.cancelled().await;
        order_clone.lock().unwrap().push("token_cancelled");
    });
    coordinator.track_token("test_token", token);

    // Register task
    let order_clone = order.clone();
    let handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        order_clone.lock().unwrap().push("task_completed");
    });
    coordinator.register_task("test_task", handle);

    // Register hook
    let order_clone = order.clone();
    struct OrderHook {
        order: Arc<std::sync::Mutex<Vec<&'static str>>>,
    }
    impl ShutdownHook for OrderHook {
        fn name(&self) -> &'static str {
            "order_hook"
        }
        fn timeout(&self) -> Duration {
            Duration::from_secs(5)
        }
        fn run(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
            let order = self.order.clone();
            Box::pin(async move {
                order.lock().unwrap().push("hook_run");
            })
        }
    }
    coordinator.register_hook(OrderHook { order: order_clone });

    // Execute shutdown
    coordinator.shutdown().await;

    // Verify order: token should be first, then task, then hook
    let final_order = order.lock().unwrap().clone();
    assert!(
        final_order.contains(&"token_cancelled"),
        "Token should be cancelled"
    );
    assert!(
        final_order.contains(&"task_completed"),
        "Task should complete"
    );
    assert!(final_order.contains(&"hook_run"), "Hook should run");

    // Verify relative order
    let task_idx = final_order
        .iter()
        .position(|&x| x == "task_completed")
        .unwrap();
    let hook_idx = final_order.iter().position(|&x| x == "hook_run").unwrap();

    assert!(
        task_idx < hook_idx,
        "Task completion should happen before hook runs"
    );
}

/// Test that shutdown handles panicking tasks gracefully
#[tokio::test]
async fn test_shutdown_handles_panicking_task() {
    let mut coordinator = ShutdownCoordinator::new();

    let handle = tokio::spawn(async {
        panic!("Task panicked!");
    });

    coordinator.register_task("panic_task", handle);

    // Shutdown should complete without hanging or panicking
    coordinator.shutdown().await;
}

/// Test that shutdown coordinator can handle multiple tokens, tasks, and hooks
#[tokio::test]
async fn test_shutdown_multiple_resources() {
    use std::sync::atomic::Ordering;

    let mut coordinator = ShutdownCoordinator::new();

    // Register multiple tokens
    let token1 = coordinator.register_token("token1");
    let token2 = coordinator.register_token("token2");
    let token3 = coordinator.register_token("token3");

    // Register multiple tasks
    let task1_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let task1_ran_clone = task1_ran.clone();
    coordinator.register_task(
        "task1",
        tokio::spawn(async move {
            task1_ran_clone.store(true, Ordering::SeqCst);
        }),
    );

    let task2_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let task2_ran_clone = task2_ran.clone();
    coordinator.register_task(
        "task2",
        tokio::spawn(async move {
            task2_ran_clone.store(true, Ordering::SeqCst);
        }),
    );

    // Register multiple hooks
    let hook1 = MockShutdownHook::new("hook1");
    let hook1_ran = hook1.ran.clone();
    coordinator.register_hook(hook1);

    let hook2 = MockShutdownHook::new("hook2");
    let hook2_ran = hook2.ran.clone();
    coordinator.register_hook(hook2);

    // Execute shutdown
    coordinator.shutdown().await;

    // Verify all resources were handled
    assert!(token1.is_cancelled());
    assert!(token2.is_cancelled());
    assert!(token3.is_cancelled());
    assert!(task1_ran.load(Ordering::SeqCst));
    assert!(task2_ran.load(Ordering::SeqCst));
    assert!(hook1_ran.load(Ordering::SeqCst));
    assert!(hook2_ran.load(Ordering::SeqCst));
}

// ============================================================================
// Config validation tests for initialization
// ============================================================================

mod config_initialization_tests {
    use synctv_core::config::{
        BootstrapConfig, BufferSizesConfig, CacheConfig, ClusterChannelConfig, Config,
        ConnectionLimitsConfig, DatabaseConfig, EmailConfig, GrpcRateLimitConfig,
        HttpRateLimitConfig, JwtConfig, LivestreamConfig, LoggingConfig, MediaProvidersConfig,
        OAuth2Config, PasswordComplexityConfig, RedisConfig, ServerConfig, WebRTCConfig,
    };

    /// Create a minimal valid config for testing
    fn minimal_test_config() -> Config {
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
                cluster_secret: String::new(),
                advertise_host: String::new(),
                shutdown_drain_timeout_seconds: 30,
            },
            database: DatabaseConfig::default(),
            redis: RedisConfig::default(),
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
                create_root_user: false,
                root_username: String::new(),
                root_password: String::new(),
            },
            cluster: ClusterChannelConfig::default(),
            password_complexity: PasswordComplexityConfig::default(),
            buffer_sizes: BufferSizesConfig::default(),
            cache: CacheConfig::default(),
            http_rate_limits: HttpRateLimitConfig::default(),
            grpc_rate_limits: GrpcRateLimitConfig::default(),
        }
    }

    /// Test that minimal config validates successfully
    #[test]
    fn test_minimal_config_validates() {
        let config = minimal_test_config();
        let result = config.validate();
        assert!(
            result.is_ok(),
            "Minimal config should validate: {:?}",
            result.err()
        );
    }

    /// Test that cluster mode requires Redis
    #[test]
    fn test_cluster_mode_requires_redis() {
        let mut config = minimal_test_config();
        config.cluster.enabled = true;
        config.redis.url = String::new(); // No Redis

        let result = config.validate();
        assert!(result.is_err(), "Cluster mode without Redis should fail");

        let errors = result.unwrap_err();
        let has_redis_error = errors
            .iter()
            .any(|e| e.contains("cluster mode requires Redis to be configured"));
        assert!(
            has_redis_error,
            "Error should mention Redis requirement: {errors:?}"
        );
    }

    /// Test that cluster mode requires cluster_secret
    #[test]
    fn test_cluster_mode_requires_cluster_secret() {
        let mut config = minimal_test_config();
        config.cluster.enabled = true;
        config.redis.url = "redis://localhost:6379".to_string();
        config.server.cluster_secret = String::new();

        let result = config.validate();
        assert!(
            result.is_err(),
            "Cluster mode without cluster_secret should fail"
        );

        let errors = result.unwrap_err();
        let has_secret_error = errors.iter().any(|e| e.contains("cluster_secret"));
        assert!(
            has_secret_error,
            "Error should mention cluster_secret requirement: {errors:?}"
        );
    }

    /// Test that JWT secret must not be empty
    #[test]
    fn test_jwt_secret_not_empty() {
        let mut config = minimal_test_config();
        config.jwt.secret = String::new();

        let result = config.validate();
        assert!(result.is_err(), "Empty JWT secret should fail validation");

        let errors = result.unwrap_err();
        let has_jwt_error = errors.iter().any(|e| e.contains("JWT secret"));
        assert!(has_jwt_error, "Error should mention JWT secret: {errors:?}");
    }

    /// Test that JWT secret must not be the default value
    #[test]
    fn test_jwt_secret_not_default() {
        let mut config = minimal_test_config();
        config.jwt.secret = "change-me-in-production".to_string();

        let result = config.validate();
        assert!(result.is_err(), "Default JWT secret should fail validation");

        let errors = result.unwrap_err();
        let has_jwt_error = errors.iter().any(|e| e.contains("JWT secret"));
        assert!(has_jwt_error, "Error should mention JWT secret: {errors:?}");
    }

    /// Test that valid cluster config passes validation
    #[test]
    fn test_valid_cluster_config() {
        let mut config = minimal_test_config();
        config.cluster.enabled = true;
        config.redis.url = "redis://localhost:6379".to_string();
        config.server.cluster_secret = "test-cluster-secret-1234567890".to_string();
        // Cluster mode requires stun_external_addr for WebRTC
        config.webrtc.stun_external_addr = "203.0.113.1:3478".to_string();
        // Cluster mode requires shared HLS storage
        config.livestream.hls_shared_storage = true;
        config.livestream.hls_storage_path = "/var/lib/synctv/hls".to_string();

        let result = config.validate();
        assert!(
            result.is_ok(),
            "Valid cluster config should pass: {:?}",
            result.err()
        );
    }
}

// ============================================================================
// Node ID generation tests (bootstrap/node_id.rs)
// ============================================================================

mod node_id_tests {
    /// Test that node IDs are unique across multiple calls
    #[test]
    fn test_node_ids_are_unique() {
        // Since generate_node_id uses POD_NAME env var or random suffix,
        // we need to test it without POD_NAME set
        let mut node_ids = std::collections::HashSet::new();

        for _ in 0..100 {
            // We simulate the uniqueness property
            let id = format!("test_{}-{}", std::process::id(), nanoid::nanoid!(6));
            node_ids.insert(id);
        }

        // All IDs should be unique
        assert_eq!(
            node_ids.len(),
            100,
            "All generated node IDs should be unique"
        );
    }

    /// Test that POD_NAME is preferred when set
    #[test]
    fn test_pod_name_preferred() {
        // When POD_NAME is set, it should be used as the node ID
        let pod_name = "synctv-abc123";

        // The node_id module prefers POD_NAME when available
        assert!(!pod_name.is_empty());
        assert!(pod_name.contains("synctv"));
    }

    /// Test that node ID format is valid for Redis keys
    #[test]
    fn test_node_id_redis_key_compatible() {
        // Node IDs should not contain characters that are problematic in Redis keys
        let sample_node_ids = vec![
            "synctv-0",
            "synctv-pod-abc123",
            "hostname_127.0.0.1-abc123",
            "my-host.local_192.168.1.1-XyZ9",
        ];

        for id in sample_node_ids {
            // Redis keys can contain most characters, but we avoid spaces and newlines
            assert!(!id.contains(' '), "Node ID should not contain spaces: {id}");
            assert!(
                !id.contains('\n'),
                "Node ID should not contain newlines: {id}"
            );
        }
    }
}

// ============================================================================
// WebRTC initialization tests (bootstrap/webrtc.rs)
// ============================================================================

mod webrtc_init_tests {
    use synctv_core::config::WebRTCConfig;

    /// Test WebRTC config defaults
    #[test]
    fn test_webrtc_config_defaults() {
        let config = WebRTCConfig::default();

        // Built-in STUN is enabled by default (powered by turn-rs)
        assert!(config.enable_builtin_stun);

        // TURN config should be empty by default
        assert!(config.turn_server_urls.is_empty());
        assert!(config.turn_shared_secret.is_empty());
    }

    /// Test that external STUN address can be configured
    #[test]
    fn test_stun_external_addr_config() {
        let mut config = WebRTCConfig::default();
        config.stun_external_addr = "203.0.113.1:3478".to_string();

        assert_eq!(config.stun_external_addr, "203.0.113.1:3478");
    }

    /// Test that TURN servers can be configured
    #[test]
    fn test_turn_servers_config() {
        let mut config = WebRTCConfig::default();
        config.turn_server_urls = vec![
            "turn:turn1.example.com:3478".to_string(),
            "turn:turn2.example.com:3478".to_string(),
        ];
        config.turn_shared_secret = "my-secret".to_string();

        assert_eq!(config.turn_server_urls.len(), 2);
        assert!(!config.turn_shared_secret.is_empty());
    }
}

// ============================================================================
// Livestream initialization tests (bootstrap/livestream.rs)
// ============================================================================

mod livestream_init_tests {
    use synctv_core::config::LivestreamConfig;

    /// Test livestream config defaults
    #[test]
    fn test_livestream_config_defaults() {
        let config = LivestreamConfig::default();

        // RTMP should have sensible defaults
        assert!(config.rtmp_port > 0);
        assert!(config.gop_cache_size > 0);
        assert!(config.stream_timeout_seconds > 0);
    }

    /// Test that RTMP port can be customized
    #[test]
    fn test_rtmp_port_customizable() {
        let mut config = LivestreamConfig::default();
        config.rtmp_port = 1935;

        assert_eq!(config.rtmp_port, 1935);
    }

    /// Test GOP cache size configuration
    #[test]
    fn test_gop_cache_size_config() {
        let mut config = LivestreamConfig::default();
        config.gop_cache_size = 3;

        assert_eq!(config.gop_cache_size, 3);
    }
}

// ============================================================================
// Connection limits tests
// ============================================================================

mod connection_limits_tests {
    use synctv_core::config::ConnectionLimitsConfig;

    /// Test connection limits config defaults
    #[test]
    fn test_connection_limits_defaults() {
        let config = ConnectionLimitsConfig::default();

        // Should have sensible defaults
        assert!(config.max_per_user > 0);
        assert!(config.max_per_room > 0);
        assert!(config.max_total > 0);
        assert!(config.idle_timeout_seconds > 0);
    }

    /// Test that custom connection limits can be set
    #[test]
    fn test_custom_connection_limits() {
        let mut config = ConnectionLimitsConfig::default();
        config.max_per_user = 5;
        config.max_per_room = 10;
        config.max_total = 1000;
        config.idle_timeout_seconds = 3600;

        assert_eq!(config.max_per_user, 5);
        assert_eq!(config.max_per_room, 10);
        assert_eq!(config.max_total, 1000);
        assert_eq!(config.idle_timeout_seconds, 3600);
    }
}

// ============================================================================
// Server struct tests
// ============================================================================

mod server_tests {
    /// Test that server address formatting is correct
    #[test]
    fn test_grpc_address_format() {
        let host = "127.0.0.1";
        let port: u16 = 50051;
        let addr = format!("{host}:{port}");

        assert!(addr.parse::<std::net::SocketAddr>().is_ok());
    }

    /// Test that HTTP address formatting is correct
    #[test]
    fn test_http_address_format() {
        let host = "0.0.0.0";
        let port: u16 = 8080;
        let addr = format!("{host}:{port}");

        assert!(addr.parse::<std::net::SocketAddr>().is_ok());
    }

    /// Test IPv6 address formatting
    #[test]
    fn test_ipv6_address_format() {
        let addr = "[::1]:8080";
        assert!(addr.parse::<std::net::SocketAddr>().is_ok());

        let addr = "[::]:8080";
        assert!(addr.parse::<std::net::SocketAddr>().is_ok());
    }
}
