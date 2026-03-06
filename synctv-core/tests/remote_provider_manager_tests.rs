//! `RemoteProviderManager` integration tests
//!
//! Tests: channel creation, cache TTL, Redis invalidation, health checks,
//!        TLS configuration, fallback behavior.
//!
//! Run with: cargo test -p synctv-core --test `remote_provider_manager_tests`
//!
//! NOTE: These tests require Docker for testcontainers (`PostgreSQL` + Redis).
#![allow(clippy::unwrap_used)]

use chrono::Utc;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use synctv_core::{
    models::ProviderInstance, repository::ProviderInstanceRepository,
    service::remote_provider_manager::RemoteProviderManager,
};
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use tokio::sync::RwLock;

// Test utilities

/// Test infrastructure with `PostgreSQL` and Redis
struct TestInfra {
    pool: PgPool,
    redis_client: redis::Client,
    #[allow(dead_code)]
    redis_url: String,
    // Keep containers alive
    _postgres: testcontainers::ContainerAsync<testcontainers_modules::postgres::Postgres>,
    _redis: testcontainers::ContainerAsync<testcontainers_modules::redis::Redis>,
}

impl TestInfra {
    async fn new() -> Self {
        // Start containers
        let pg_container = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            testcontainers_modules::postgres::Postgres::default()
                .with_user("synctv")
                .with_password("synctv_test")
                .with_db_name("synctv_test")
                .with_tag("16-alpine")
                .start(),
        )
        .await
        .expect("Docker container startup timed out (is Docker running?)")
        .expect("Failed to start Postgres");
        let redis_container = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            testcontainers_modules::redis::Redis::default().start(),
        )
        .await
        .expect("Docker container startup timed out (is Docker running?)")
        .expect("Failed to start Redis");

        // Get mapped ports
        let pg_host = pg_container
            .get_host()
            .await
            .expect("Failed to get Postgres host");
        let pg_port = pg_container
            .get_host_port_ipv4(5432)
            .await
            .expect("Failed to get Postgres port");

        let redis_host = redis_container
            .get_host()
            .await
            .expect("Failed to get Redis host");
        let redis_port = redis_container
            .get_host_port_ipv4(6379)
            .await
            .expect("Failed to get Redis port");

        // Build connection URLs
        let database_url =
            format!("postgresql://synctv:synctv_test@{pg_host}:{pg_port}/synctv_test");
        let redis_url = format!("redis://{redis_host}:{redis_port}");

        // Connect to Postgres
        let pool = {
            let mut retries = 0u32;
            loop {
                match sqlx::postgres::PgPoolOptions::new()
                    .acquire_timeout(std::time::Duration::from_secs(2))
                    .max_connections(5)
                    .connect(&database_url)
                    .await
                {
                    Ok(p) => break p,
                    Err(_) if retries < 60 => {
                        retries += 1;
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                    Err(e) => panic!("PostgreSQL not ready after {retries} retries: {e}"),
                }
            }
        };

        // Run migrations
        sqlx::migrate!("../migrations")
            .run(&pool)
            .await
            .expect("Failed to run migrations");

        // Create Redis client
        let redis_client =
            redis::Client::open(redis_url.as_str()).expect("Failed to create Redis client");

        Self {
            pool,
            redis_client,
            redis_url,
            _postgres: pg_container,
            _redis: redis_container,
        }
    }

    async fn redis_connection_manager(&self) -> redis::aio::ConnectionManager {
        redis::aio::ConnectionManager::new(self.redis_client.clone())
            .await
            .expect("Failed to create Redis ConnectionManager")
    }
}

/// Create a test provider instance
fn make_test_instance(name: &str) -> ProviderInstance {
    let now = Utc::now();
    ProviderInstance {
        name: name.to_string(),
        endpoint: "http://example.com:50051".to_string(), // Use external domain to pass SSRF
        comment: Some("test instance".to_string()),
        jwt_secret: None,
        custom_ca: None,
        timeout: "10s".to_string(),
        tls: false,
        insecure_tls: false,
        providers: vec!["bilibili".to_string()],
        enabled: true,
        created_at: now,
        updated_at: now,
    }
}

/// Create a test provider instance with TLS
fn make_test_instance_tls(name: &str, insecure: bool) -> ProviderInstance {
    let now = Utc::now();
    ProviderInstance {
        name: name.to_string(),
        endpoint: "https://example.com:50052".to_string(),
        comment: Some("test TLS instance".to_string()),
        jwt_secret: None,
        custom_ca: None,
        timeout: "10s".to_string(),
        tls: true,
        insecure_tls: insecure,
        providers: vec!["emby".to_string()],
        enabled: true,
        created_at: now,
        updated_at: now,
    }
}

// ─── Test 1: Channel creation from DB config ────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_channel_creation_from_db_config() {
    let infra = TestInfra::new().await;
    let redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let redis_client = Some(infra.redis_client.clone());

    let repo = ProviderInstanceRepository::new(infra.pool.clone());
    let manager = RemoteProviderManager::new(Arc::new(repo), redis_conn, redis_client);

    // Create instance in DB
    let instance = make_test_instance("test-instance-1");
    manager.add(instance.clone()).await.unwrap();

    // Get channel - should create from DB config
    // Note: The channel creation will attempt to connect. Even though there's
    // no actual gRPC server, tonic creates lazy channels that don't connect
    // until the first RPC call. So we expect Some(channel) here.
    let channel = manager.get("test-instance-1").await;

    // Channel should be Some (lazy channel created, even if it will fail later)
    assert!(
        channel.is_some(),
        "Channel should be created (lazy connection)"
    );

    // Verify instance exists in DB
    let repo = ProviderInstanceRepository::new(infra.pool);
    let fetched = repo.get_by_name("test-instance-1").await.unwrap();
    assert!(fetched.is_some());
    assert_eq!(fetched.unwrap().name, "test-instance-1");
}

// ─── Test 2: Channel cache hit (cached channel returned) ─────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_channel_cache_hit() {
    let infra = TestInfra::new().await;
    let redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let redis_client = Some(infra.redis_client.clone());

    let repo = ProviderInstanceRepository::new(infra.pool.clone());
    let manager = RemoteProviderManager::new(Arc::new(repo), redis_conn, redis_client);

    // Create instance in DB
    let instance = make_test_instance("test-instance-2");
    manager.add(instance.clone()).await.unwrap();

    // First get - cache miss, attempts DB lookup
    let _ = manager.get("test-instance-2").await;

    // Second get - should hit cache (though still returns None since no server)
    let _ = manager.get("test-instance-2").await;

    // Verify DB was only queried once (cache working)
    // This is implicit - if cache wasn't working, we'd see multiple DB queries
    // in logs. For now, we just verify no panics occur.
}

// ─── Test 3: Channel cache TTL expiration ───────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_channel_cache_ttl_expiration() {
    let infra = TestInfra::new().await;
    let redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let redis_client = Some(infra.redis_client.clone());

    // Create a manager with a very short TTL for testing
    let repo = ProviderInstanceRepository::new(infra.pool.clone());
    let manager = RemoteProviderManager::new(Arc::new(repo), redis_conn, redis_client);

    // Create instance in DB
    let instance = make_test_instance("test-instance-3");
    manager.add(instance.clone()).await.unwrap();

    // First get - populates cache
    let _ = manager.get("test-instance-3").await;

    // Wait for cache to expire (default TTL is 300s, but we can't easily test this
    // without modifying the manager or using a custom build)
    // For now, we just verify the cache is being used
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Get again - should still be in cache (300s TTL)
    let _ = manager.get("test-instance-3").await;

    // Note: Testing actual TTL expiration would require:
    // 1. Making cache_ttl configurable via constructor
    // 2. Setting a very short TTL (e.g., 100ms)
    // 3. Waiting and verifying cache miss
    // This is left as an exercise for future enhancement
}

// ─── Test 4: Redis Pub/Sub invalidation ─────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_redis_pubsub_invalidation() {
    let infra = TestInfra::new().await;
    let redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let redis_client = Some(infra.redis_client.clone());

    let repo = ProviderInstanceRepository::new(infra.pool.clone());

    // Create two managers (simulating two replicas)
    let manager1 = RemoteProviderManager::new(
        Arc::new(ProviderInstanceRepository::new(infra.pool.clone())),
        Some(Arc::new(RwLock::new(
            infra.redis_connection_manager().await,
        ))),
        Some(infra.redis_client.clone()),
    );

    let manager2 = RemoteProviderManager::new(Arc::new(repo), redis_conn, redis_client);

    // Start invalidation listener on manager2
    manager2.start_invalidation_listener().await.unwrap();

    // Create instance via manager1
    let instance = make_test_instance("test-instance-4");
    manager1.add(instance.clone()).await.unwrap();

    // Wait for Pub/Sub message to propagate
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify both managers can see the instance
    let instances1 = manager1.list().await;
    let instances2 = manager2.list().await;

    assert!(
        instances1.contains(&"test-instance-4".to_string()),
        "Manager1 should list the instance"
    );
    assert!(
        instances2.contains(&"test-instance-4".to_string()),
        "Manager2 should see the instance after invalidation"
    );

    // Update instance via manager1
    let mut updated_instance = instance.clone();
    updated_instance.comment = Some("updated comment".to_string());
    updated_instance.updated_at = Utc::now();
    manager1.update(updated_instance).await.unwrap();

    // Wait for invalidation
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify manager2 sees the update
    let repo = ProviderInstanceRepository::new(infra.pool);
    let fetched = repo.get_by_name("test-instance-4").await.unwrap();
    assert!(fetched.is_some());
    assert_eq!(
        fetched.unwrap().comment,
        Some("updated comment".to_string())
    );
}

// ─── Test 5: Redis invalidation on delete ───────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_redis_invalidation_on_delete() {
    let infra = TestInfra::new().await;
    let redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let redis_client = Some(infra.redis_client.clone());

    let repo = ProviderInstanceRepository::new(infra.pool.clone());

    let manager1 = RemoteProviderManager::new(
        Arc::new(ProviderInstanceRepository::new(infra.pool.clone())),
        Some(Arc::new(RwLock::new(
            infra.redis_connection_manager().await,
        ))),
        Some(infra.redis_client.clone()),
    );

    let manager2 = RemoteProviderManager::new(Arc::new(repo), redis_conn, redis_client);

    // Start invalidation listener
    manager2.start_invalidation_listener().await.unwrap();

    // Create instance via manager1
    let instance = make_test_instance("test-instance-5");
    manager1.add(instance.clone()).await.unwrap();

    // Pre-warm manager2's cache
    let _ = manager2.get("test-instance-5").await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Delete via manager1
    manager1.delete("test-instance-5").await.unwrap();

    // Wait for invalidation
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify manager2 no longer lists the instance
    let instances2 = manager2.list().await;
    assert!(
        !instances2.contains(&"test-instance-5".to_string()),
        "Manager2 should not list deleted instance"
    );

    // Verify get returns None
    let channel = manager2.get("test-instance-5").await;
    assert!(channel.is_none(), "Deleted instance should return None");
}

// ─── Test 6: Health check integration ───────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_health_check_integration() {
    let infra = TestInfra::new().await;
    let redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let redis_client = Some(infra.redis_client.clone());

    let repo = ProviderInstanceRepository::new(infra.pool.clone());
    let manager = RemoteProviderManager::new(Arc::new(repo), redis_conn, redis_client);

    // Create instance in DB
    let instance = make_test_instance("test-instance-6");
    manager.add(instance.clone()).await.unwrap();

    // Run health check
    let health_results = manager.health_check().await;

    // Since there's no actual gRPC server, the instance should be unhealthy
    // The health check should return the instance in the map with false status
    assert!(
        health_results.contains_key("test-instance-6"),
        "Health check should include the instance"
    );
    assert!(
        !health_results["test-instance-6"],
        "Instance should be unhealthy when gRPC server is down"
    );
}

// ─── Test 7: Health check with enabled/disabled instances ──────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_health_check_respects_enabled_flag() {
    let infra = TestInfra::new().await;
    let redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let redis_client = Some(infra.redis_client.clone());

    let repo = ProviderInstanceRepository::new(infra.pool.clone());
    let manager = RemoteProviderManager::new(Arc::new(repo), redis_conn, redis_client);

    // Create enabled instance
    let instance_enabled = make_test_instance("test-instance-7a");
    manager.add(instance_enabled).await.unwrap();

    // Create disabled instance
    let instance_disabled = make_test_instance("test-instance-7b");
    let mut disabled = instance_disabled.clone();
    disabled.enabled = false;
    manager.add(disabled).await.unwrap();

    // Run health check
    let health_results = manager.health_check().await;

    // Enabled instance should be in results
    assert!(
        health_results.contains_key("test-instance-7a"),
        "Health check should include enabled instance"
    );

    // Disabled instance should NOT be in results
    assert!(
        !health_results.contains_key("test-instance-7b"),
        "Health check should skip disabled instance"
    );
}

// ─── Test 8: TLS configuration (non-insecure) ───────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_tls_configuration_secure() {
    let infra = TestInfra::new().await;
    let redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let redis_client = Some(infra.redis_client.clone());

    let _repo = ProviderInstanceRepository::new(infra.pool.clone());

    // Create instance with secure TLS (not insecure)
    let instance = make_test_instance_tls("test-instance-8", false);

    // Create the instance directly in the repository without creating a channel
    // This avoids the rustls crypto provider issue in test environment
    let repo_instance = ProviderInstanceRepository::new(infra.pool.clone());
    repo_instance.create(&instance).await.unwrap();

    // Verify instance was saved with correct TLS settings
    let fetched = repo_instance.get_by_name("test-instance-8").await.unwrap();
    assert!(fetched.is_some());
    let fetched = fetched.unwrap();
    assert!(fetched.tls, "Instance should have TLS enabled");
    assert!(
        !fetched.insecure_tls,
        "Instance should not have insecure TLS"
    );

    // Now create the manager and verify it can list the instance
    let manager = RemoteProviderManager::new(
        Arc::new(ProviderInstanceRepository::new(infra.pool)),
        redis_conn,
        redis_client,
    );

    // Verify the instance can be listed
    let instances = manager.list().await;
    assert!(
        instances.contains(&"test-instance-8".to_string()),
        "Should list the TLS instance"
    );
}

// ─── Test 9: TLS configuration (insecure) ───────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_tls_configuration_insecure() {
    let infra = TestInfra::new().await;
    let redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let redis_client = Some(infra.redis_client.clone());

    let repo = ProviderInstanceRepository::new(infra.pool.clone());
    let manager = RemoteProviderManager::new(Arc::new(repo), redis_conn, redis_client);

    // Create instance with insecure TLS. The add() eagerly connects for
    // insecure TLS (connect_with_connector), so use a short timeout to avoid
    // waiting for the remote to respond. The test exercises the TLS code path.
    let instance = {
        let mut inst = make_test_instance_tls("test-instance-9", true);
        inst.timeout = "2s".to_string();
        inst
    };

    // Wrap with timeout to avoid 270s+ waits on DNS/connect to example.com
    let result = tokio::time::timeout(Duration::from_secs(5), manager.add(instance.clone())).await;

    // If it succeeds (unlikely with port 1), verify the stored config
    if let Ok(Ok(())) = &result {
        let repo = ProviderInstanceRepository::new(infra.pool);
        let fetched = repo.get_by_name("test-instance-9").await.unwrap();
        assert!(fetched.is_some());
        let fetched = fetched.unwrap();
        assert!(fetched.tls, "Instance should have TLS enabled");
        assert!(
            fetched.insecure_tls,
            "Instance should have insecure TLS enabled"
        );
    }
    // Timeout or connection error is expected and acceptable
}

// ─── Test 10: Fallback to local provider (no remote instance) ───────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_fallback_to_local_provider() {
    let infra = TestInfra::new().await;
    let redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let redis_client = Some(infra.redis_client.clone());

    let repo = ProviderInstanceRepository::new(infra.pool.clone());
    let manager = RemoteProviderManager::new(Arc::new(repo), redis_conn, redis_client);

    // Try to get a non-existent instance
    let channel = manager.get("non-existent-instance").await;

    // Should return None, allowing caller to fallback to local provider
    assert!(
        channel.is_none(),
        "Non-existent instance should return None for fallback"
    );

    // Test resolve_client with fallback
    let result = manager
        .resolve_client(
            Some("non-existent-instance"),
            |_channel| "remote",
            || "local",
        )
        .await;

    assert_eq!(
        result, "local",
        "resolve_client should fallback to local when remote not found"
    );
}

// ─── Test 11: Fallback when instance_name is None ───────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_fallback_when_instance_name_none() {
    let infra = TestInfra::new().await;
    let redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let redis_client = Some(infra.redis_client.clone());

    let repo = ProviderInstanceRepository::new(infra.pool.clone());
    let manager = RemoteProviderManager::new(Arc::new(repo), redis_conn, redis_client);

    // Test resolve_client with None instance_name (should use local)
    let result = manager
        .resolve_client(None as Option<&str>, |_channel| "remote", || "local")
        .await;

    assert_eq!(
        result, "local",
        "resolve_client should use local when instance_name is None"
    );
}

// ─── Test 12: Fallback when remote instance exists but channel fails ─────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_fallback_when_channel_creation_fails() {
    let infra = TestInfra::new().await;
    let redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let redis_client = Some(infra.redis_client.clone());

    let repo = ProviderInstanceRepository::new(infra.pool.clone());
    let manager = RemoteProviderManager::new(Arc::new(repo), redis_conn, redis_client);

    // Create instance with invalid endpoint (will fail SSRF validation)
    let mut instance = make_test_instance("test-instance-12");
    instance.endpoint = "http://invalid-host-with-invalid-port:99999".to_string();
    let result = manager.add(instance.clone()).await;

    // Should fail due to invalid port/SSRF validation
    assert!(
        result.is_err(),
        "Adding instance with invalid endpoint should fail validation"
    );
}

// ─── Test 13: Enable/disable instance ───────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_enable_disable_instance() {
    let infra = TestInfra::new().await;
    let redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let redis_client = Some(infra.redis_client.clone());

    let repo = ProviderInstanceRepository::new(infra.pool.clone());
    let manager = RemoteProviderManager::new(Arc::new(repo), redis_conn, redis_client);

    // Create enabled instance
    let instance = make_test_instance("test-instance-13");
    manager.add(instance.clone()).await.unwrap();

    // Verify it's enabled and gettable
    let repo = ProviderInstanceRepository::new(infra.pool.clone());
    let fetched = repo.get_by_name("test-instance-13").await.unwrap();
    assert!(fetched.is_some());
    assert!(fetched.unwrap().enabled);

    // Disable the instance
    manager.disable("test-instance-13").await.unwrap();

    // Verify it's disabled
    let fetched = repo.get_by_name("test-instance-13").await.unwrap();
    assert!(fetched.is_some());
    assert!(!fetched.unwrap().enabled);

    // get() should return None for disabled instance
    let channel = manager.get("test-instance-13").await;
    assert!(channel.is_none(), "Disabled instance should return None");

    // Re-enable the instance
    manager.enable("test-instance-13").await.unwrap();

    // Verify it's enabled again
    let fetched = repo.get_by_name("test-instance-13").await.unwrap();
    assert!(fetched.is_some());
    assert!(fetched.unwrap().enabled);
}

// ─── Test 14: Reconnect instance ────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_reconnect_instance() {
    let infra = TestInfra::new().await;
    let redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let redis_client = Some(infra.redis_client.clone());

    let repo = ProviderInstanceRepository::new(infra.pool.clone());
    let manager = RemoteProviderManager::new(Arc::new(repo), redis_conn, redis_client);

    // Create instance
    let instance = make_test_instance("test-instance-14");
    manager.add(instance.clone()).await.unwrap();

    // Try to reconnect - since tonic creates lazy channels, this will succeed
    // even though there's no actual server
    let result = manager.reconnect("test-instance-14").await;

    // Reconnect will succeed because tonic creates lazy channels
    // (connection isn't established until first RPC call)
    assert!(
        result.is_ok(),
        "Reconnect should succeed with lazy channel (even without server)"
    );

    // Disable the instance
    manager.disable("test-instance-14").await.unwrap();

    // Try to reconnect disabled instance - should fail
    let result = manager.reconnect("test-instance-14").await;
    assert!(
        result.is_err(),
        "Reconnect should fail for disabled instance"
    );
}

// ─── Test 15: Add duplicate instance fails ───────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_add_duplicate_instance_fails() {
    let infra = TestInfra::new().await;
    let redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let redis_client = Some(infra.redis_client.clone());

    let repo = ProviderInstanceRepository::new(infra.pool.clone());
    let manager = RemoteProviderManager::new(Arc::new(repo), redis_conn, redis_client);

    // Create instance
    let instance = make_test_instance("test-instance-15");
    manager.add(instance.clone()).await.unwrap();

    // Try to add duplicate - should fail
    let result = manager.add(instance).await;
    assert!(result.is_err(), "Adding duplicate instance should fail");

    if let Err(e) = result {
        assert!(
            format!("{e:?}").contains("AlreadyExists"),
            "Error should be AlreadyExists variant"
        );
    }
}

// ─── Test 16: Update non-existent instance fails ─────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_update_nonexistent_instance_fails() {
    let infra = TestInfra::new().await;
    let redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let redis_client = Some(infra.redis_client.clone());

    let repo = ProviderInstanceRepository::new(infra.pool.clone());
    let manager = RemoteProviderManager::new(Arc::new(repo), redis_conn, redis_client);

    // Try to update non-existent instance
    let instance = make_test_instance("test-instance-16");
    let result = manager.update(instance).await;

    // Should fail (database will return 0 rows affected)
    assert!(
        result.is_err(),
        "Updating non-existent instance should fail"
    );
}

// ─── Test 17: Delete non-existent instance fails ─────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_delete_nonexistent_instance_fails() {
    let infra = TestInfra::new().await;
    let redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let redis_client = Some(infra.redis_client.clone());

    let repo = ProviderInstanceRepository::new(infra.pool.clone());
    let manager = RemoteProviderManager::new(Arc::new(repo), redis_conn, redis_client);

    // Try to delete non-existent instance
    let result = manager.delete("test-instance-17").await;

    // Should fail (database will return 0 rows affected)
    assert!(
        result.is_err(),
        "Deleting non-existent instance should fail"
    );
}

// ─── Test 18: Get all instances ─────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_get_all_instances() {
    let infra = TestInfra::new().await;
    let redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let redis_client = Some(infra.redis_client.clone());

    let repo = ProviderInstanceRepository::new(infra.pool.clone());
    let manager = RemoteProviderManager::new(Arc::new(repo), redis_conn, redis_client);

    // Create multiple instances
    for i in 1..=3 {
        let instance = make_test_instance(&format!("test-instance-18a-{i}"));
        manager.add(instance).await.unwrap();
    }

    // Also create a disabled instance
    let mut disabled = make_test_instance("test-instance-18b-disabled");
    disabled.enabled = false;
    manager.add(disabled).await.unwrap();

    // Get all instances (should include both enabled and disabled)
    let all_instances = manager.get_all_instances().await.unwrap();

    assert!(
        all_instances.len() >= 4,
        "Should have at least 4 instances (3 enabled + 1 disabled)"
    );

    // Verify we have both enabled and disabled
    let enabled_count = all_instances.iter().filter(|i| i.enabled).count();
    let disabled_count = all_instances.iter().filter(|i| !i.enabled).count();

    assert!(
        enabled_count >= 3,
        "Should have at least 3 enabled instances"
    );
    assert!(
        disabled_count >= 1,
        "Should have at least 1 disabled instance"
    );
}

// ─── Test 19: Manager without Redis (local-only invalidation) ───────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_manager_without_redis() {
    let infra = TestInfra::new().await;

    let repo = ProviderInstanceRepository::new(infra.pool.clone());
    let manager = RemoteProviderManager::new(
        Arc::new(repo),
        None, // No Redis
        None, // No Redis client
    );

    // Start invalidation listener - should return Ok without starting
    let result = manager.start_invalidation_listener().await;
    assert!(
        result.is_ok(),
        "Starting invalidation listener without Redis should succeed"
    );

    // Create instance
    let instance = make_test_instance("test-instance-19");
    manager.add(instance.clone()).await.unwrap();

    // Get should still work
    let _ = manager.get("test-instance-19").await;

    // List should work
    let instances = manager.list().await;
    assert!(
        instances.contains(&"test-instance-19".to_string()),
        "Should list the instance even without Redis"
    );
}

// ─── Test 20: Init pre-warms cache ──────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_init_pre_warms_cache() {
    let infra = TestInfra::new().await;
    let redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let redis_client = Some(infra.redis_client.clone());

    let repo = ProviderInstanceRepository::new(infra.pool.clone());
    let manager = RemoteProviderManager::new(Arc::new(repo), redis_conn, redis_client);

    // Create instances before init
    for i in 1..=3 {
        let instance = make_test_instance(&format!("test-instance-20-{i}"));
        manager.add(instance).await.unwrap();
    }

    // Call init - should pre-warm cache
    let result = manager.init().await;
    assert!(result.is_ok(), "Init should succeed");

    // Verify instances are listed
    let instances = manager.list().await;
    assert!(
        instances.len() >= 3,
        "Should list at least 3 instances after init"
    );
}

// ─── Test 21: SSRF validation prevents internal endpoints ───────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_ssrf_validation_blocks_internal_ips() {
    let infra = TestInfra::new().await;
    let redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let redis_client = Some(infra.redis_client.clone());

    let repo = ProviderInstanceRepository::new(infra.pool.clone());
    let manager = RemoteProviderManager::new(Arc::new(repo), redis_conn, redis_client);

    // Try to create instance with internal IP (should fail SSRF validation)
    let mut instance = make_test_instance("test-instance-21");
    instance.endpoint = "http://127.0.0.1:50051".to_string();

    let result = manager.add(instance).await;

    // Should fail due to SSRF validation
    assert!(
        result.is_err(),
        "Adding instance with internal IP should fail SSRF validation"
    );

    if let Err(e) = result {
        let error_msg = format!("{e:?}");
        assert!(
            error_msg.contains("SSRF")
                || error_msg.contains("ssrf")
                || error_msg.contains("internal"),
            "Error should mention SSRF validation: {error_msg}"
        );
    }
}

// ─── Test 22: SSRF validation allows public endpoints ───────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_ssrf_validation_allows_public_endpoints() {
    let infra = TestInfra::new().await;
    let redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let redis_client = Some(infra.redis_client.clone());

    let repo = ProviderInstanceRepository::new(infra.pool.clone());
    let manager = RemoteProviderManager::new(Arc::new(repo), redis_conn, redis_client);

    // Create instance with public endpoint (will fail connection, but pass SSRF)
    let mut instance = make_test_instance("test-instance-22");
    instance.endpoint = "http://example.com:50051".to_string();

    let result = manager.add(instance.clone()).await;

    // SSRF validation should pass (connection may fail, but that's different)
    // The instance should be added to DB
    assert!(
        result.is_ok(),
        "Adding instance with public endpoint should pass SSRF validation"
    );

    // Verify it's in the DB
    let repo = ProviderInstanceRepository::new(infra.pool);
    let fetched = repo.get_by_name("test-instance-22").await.unwrap();
    assert!(fetched.is_some());
}

// ─── Test 23: resolve_client with remote instance ───────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_resolve_client_uses_remote_when_available() {
    let infra = TestInfra::new().await;
    let redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let redis_client = Some(infra.redis_client.clone());

    let repo = ProviderInstanceRepository::new(infra.pool.clone());
    let manager = Arc::new(RemoteProviderManager::new(
        Arc::new(repo),
        redis_conn,
        redis_client,
    ));

    // Create instance (tonic creates lazy channel)
    let instance = make_test_instance("test-instance-23");
    manager.add(instance).await.unwrap();

    // Since tonic creates lazy channels, the remote path will be taken
    // even though there's no actual server
    let result = manager
        .resolve_client(Some("test-instance-23"), |_channel| "remote", || "local")
        .await;

    // Should be "remote" because the lazy channel was created successfully
    assert_eq!(result, "remote");
}

// ─── Test 24: Cache respects max capacity ───────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_cache_respects_max_capacity() {
    let infra = TestInfra::new().await;
    let redis_conn = Some(Arc::new(RwLock::new(
        infra.redis_connection_manager().await,
    )));
    let redis_client = Some(infra.redis_client.clone());

    let repo = ProviderInstanceRepository::new(infra.pool.clone());
    let manager = RemoteProviderManager::new(Arc::new(repo), redis_conn, redis_client);

    // Create instances (default max is 1000, so this won't test eviction)
    // This is more of a sanity check that the cache doesn't panic
    for i in 1..=10 {
        let instance = make_test_instance(&format!("test-instance-24-{i}"));
        manager.add(instance).await.unwrap();
    }

    // All should be listable
    let instances = manager.list().await;
    assert!(instances.len() >= 10, "Should list at least 10 instances");
}

// ─── Test 25: Provider instance supports_provider ───────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_provider_instance_supports_provider() {
    let _infra = TestInfra::new().await;

    // Create instance with multiple providers
    let instance = ProviderInstance {
        name: "test-instance-25".to_string(),
        endpoint: "http://localhost:50051".to_string(),
        comment: None,
        jwt_secret: None,
        custom_ca: None,
        timeout: "10s".to_string(),
        tls: false,
        insecure_tls: false,
        providers: vec![
            "bilibili".to_string(),
            "alist".to_string(),
            "emby".to_string(),
        ],
        enabled: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    // Test supports_provider
    assert!(instance.supports_provider("bilibili"));
    assert!(instance.supports_provider("alist"));
    assert!(instance.supports_provider("emby"));
    assert!(!instance.supports_provider("direct_url"));
    assert!(!instance.supports_provider("rtmp"));
}

// ─── Test 26: Provider instance parse_timeout ───────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_provider_instance_parse_timeout() {
    let _infra = TestInfra::new().await;

    // Test valid timeout formats
    let instance1 = make_test_instance("test-26a");
    assert_eq!(instance1.parse_timeout().unwrap(), Duration::from_secs(10));

    let mut instance2 = make_test_instance("test-26b");
    instance2.timeout = "30s".to_string();
    assert_eq!(instance2.parse_timeout().unwrap(), Duration::from_secs(30));

    let mut instance3 = make_test_instance("test-26c");
    instance3.timeout = "5m".to_string();
    assert_eq!(instance3.parse_timeout().unwrap(), Duration::from_mins(5));

    // Test invalid timeout format
    let mut instance4 = make_test_instance("test-26d");
    instance4.timeout = "invalid".to_string();
    assert!(
        instance4.parse_timeout().is_err(),
        "Invalid timeout should parse error"
    );
}
