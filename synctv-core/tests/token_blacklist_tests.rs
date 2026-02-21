//! Token blacklist tests
//!
//! Tests the InMemoryTokenBlacklistStore.
//!
//! Run with: cargo test --test token_blacklist_tests -- --nocapture

use synctv_core::service::{
    InMemoryTokenBlacklistStore, TokenBlacklistStore,
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
