//! Token blacklist tests
//!
//! Tests the InMemoryTokenBlacklistStore and RedisTokenBlacklistStore.
//!
//! Run with: cargo test --test token_blacklist_tests -- --nocapture

use synctv_core::service::{
    InMemoryTokenBlacklistStore, RedisTokenBlacklistStore, TokenBlacklistStore,
};

// ============================================================================
// InMemoryTokenBlacklistStore tests
// ============================================================================

#[tokio::test]
async fn test_in_memory_blacklist_insert_and_check() {
    let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);

    let key = "jti:abc123";
    assert!(!store.is_blacklisted(key).await);

    store.blacklist(key, 3600).await.unwrap();
    assert!(store.is_blacklisted(key).await);
}

#[tokio::test]
async fn test_in_memory_blacklist_not_blacklisted_unknown() {
    let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);

    assert!(!store.is_blacklisted("jti:unknown").await);
    assert!(!store.is_blacklisted("jti:never_seen").await);
}

#[tokio::test]
async fn test_in_memory_blacklist_ttl_expiry() {
    let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);

    let key = "jti:expiry_test";
    // Blacklist with 1 second TTL
    store.blacklist(key, 1).await.unwrap();
    assert!(store.is_blacklisted(key).await);

    // Wait for expiry
    tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;
    assert!(
        !store.is_blacklisted(key).await,
        "Should no longer be blacklisted after TTL expiry"
    );
}

#[tokio::test]
async fn test_in_memory_family_revoked_set_and_get() {
    let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);

    let key = "family:user_42";
    let timestamp = chrono::Utc::now().timestamp();

    // Initially no revocation
    assert!(store.get_family_revoked_at(key).await.is_none());

    // Set family revocation
    store.set_family_revoked(key, timestamp, 86400).await;

    // Should be retrievable
    let revoked_at = store.get_family_revoked_at(key).await;
    assert_eq!(revoked_at, Some(timestamp));
}

// ============================================================================
// RedisTokenBlacklistStore tests (require testcontainers)
// ============================================================================

use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;

const REDIS_VERSION: &str = "7-alpine";

async fn start_redis() -> (testcontainers::ContainerAsync<Redis>, redis::aio::ConnectionManager) {
    let container = Redis::default()
        .with_tag(REDIS_VERSION)
        .start()
        .await
        .expect("Failed to start Redis");
    let port = container
        .get_host_port_ipv4(6379)
        .await
        .expect("Failed to get port");
    let redis_url = format!("redis://127.0.0.1:{}", port);
    let client = redis::Client::open(redis_url).expect("Failed to create Redis client");
    let conn = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to create connection manager");
    (container, conn)
}

#[tokio::test]
async fn test_redis_blacklist_roundtrip() {
    let (_container, conn) = start_redis().await;
    let store = RedisTokenBlacklistStore::new(conn);

    let key = "test:bl:jti_roundtrip";
    assert!(!store.is_blacklisted(key).await);

    store.blacklist(key, 60).await.unwrap();
    assert!(store.is_blacklisted(key).await);
}

#[tokio::test]
async fn test_redis_blacklist_ttl_expiry() {
    let (_container, conn) = start_redis().await;
    let store = RedisTokenBlacklistStore::new(conn);

    let key = "test:bl:jti_ttl";
    store.blacklist(key, 1).await.unwrap();
    assert!(store.is_blacklisted(key).await);

    // Wait for Redis TTL expiry
    tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;
    assert!(
        !store.is_blacklisted(key).await,
        "Should expire after TTL in Redis"
    );
}

#[tokio::test]
async fn test_redis_family_revoked_roundtrip() {
    let (_container, conn) = start_redis().await;
    let store = RedisTokenBlacklistStore::new(conn);

    let key = "test:bl:family_user_99";
    let timestamp = chrono::Utc::now().timestamp();

    // Initially not set
    assert!(store.get_family_revoked_at(key).await.is_none());

    // Set and retrieve
    store.set_family_revoked(key, timestamp, 86400).await;
    let revoked_at = store.get_family_revoked_at(key).await;
    assert_eq!(revoked_at, Some(timestamp));
}
