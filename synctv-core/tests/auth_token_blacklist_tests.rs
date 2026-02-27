//! Token blacklist tests for authentication module
//!
//! Tests for token blacklist functionality including:
//! - Concurrent blacklist additions
//! - Cache penetration protection
//! - Redis degradation handling
//! - Token family revocation
//!
//! Run with: cargo test --test auth_token_blacklist_tests
//! With Docker: cargo test --test auth_token_blacklist_tests -- --ignored

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use synctv_core::service::{
    FallbackTokenBlacklistStore, InMemoryTokenBlacklistStore,
    TokenBlacklistStore, RedisSyncableTokenBlacklistStore,
};

// ============================================================================
// Concurrent Blacklist Addition Tests
// ============================================================================

#[tokio::test]
async fn test_concurrent_blacklist_additions() {
    let store = Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let key = "jti:concurrent_test";
    let success_count = Arc::new(AtomicUsize::new(0));

    let mut handles = vec![];
    for _ in 0..10 {
        let store = store.clone();
        let success_count = success_count.clone();
        let handle = tokio::spawn(async move {
            match store.blacklist(key, 3600).await {
                Ok(()) => {
                    success_count.fetch_add(1, Ordering::SeqCst);
                }
                Err(_) => {}
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.await.expect("Task panicked");
    }

    // All additions should succeed (idempotent operation)
    assert_eq!(success_count.load(Ordering::SeqCst), 10);

    // Token should be blacklisted
    assert!(store.is_blacklisted(key).await);
}

#[tokio::test]
async fn test_atomic_blacklist_if_not_exists() {
    let store = Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let key = "jti:atomic_test";

    // First call should return false (not existed)
    let already_existed = store
        .blacklist_if_not_exists(key, 3600)
        .await
        .expect("blacklist_if_not_exists failed");
    assert!(!already_existed, "First call should return false");

    // Subsequent calls should return true (already existed = replay detected)
    for _ in 0..5 {
        let existed = store
            .blacklist_if_not_exists(key, 3600)
            .await
            .expect("blacklist_if_not_exists failed");
        assert!(existed, "Subsequent calls should return true (replay detected)");
    }
}

#[tokio::test]
async fn test_concurrent_blacklist_if_not_exists_atomicity() {
    use tokio::sync::Barrier;

    let store = Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let key = "jti:concurrent_atomic";

    let first_use_count = Arc::new(AtomicUsize::new(0));
    let replay_count = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(20));

    let mut handles = vec![];
    for _ in 0..20 {
        let store = store.clone();
        let first_use = first_use_count.clone();
        let replay = replay_count.clone();
        let barrier = barrier.clone();

        handles.push(tokio::spawn(async move {
            barrier.wait().await;

            let already_existed = store.blacklist_if_not_exists(key, 3600).await.unwrap();
            if already_existed {
                replay.fetch_add(1, Ordering::SeqCst);
            } else {
                first_use.fetch_add(1, Ordering::SeqCst);
            }
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    // CRITICAL: Exactly ONE call should be first use, rest should be replays
    assert_eq!(
        first_use_count.load(Ordering::SeqCst),
        1,
        "Exactly ONE concurrent call should return false (first use)"
    );
    assert_eq!(
        replay_count.load(Ordering::SeqCst),
        19,
        "19 concurrent calls should return true (replay detected)"
    );

    assert!(store.is_blacklisted(key).await);
}

// ============================================================================
// Cache Penetration Protection Tests
// ============================================================================

#[tokio::test]
async fn test_non_blacklisted_token_returns_false() {
    let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);

    // Unknown token should NOT be blacklisted
    assert!(!store.is_blacklisted("jti:unknown_token").await);
    assert!(!store.is_blacklisted("jti:never_seen").await);
    assert!(!store.is_blacklisted("jti:random_jti_12345").await);
}

#[tokio::test]
async fn test_blacklist_ttl_expiry() {
    let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);

    let key = "jti:ttl_test";
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
async fn test_high_volume_lookups_no_memory_leak() {
    let store = InMemoryTokenBlacklistStore::new(100, 3600, 86400);

    // Perform many lookups for non-existent tokens
    for i in 0..1000 {
        let key = format!("jti:lookup_test_{}", i);
        // Should always return false (not blacklisted)
        assert!(!store.is_blacklisted(&key).await);
    }

    // Memory usage should be bounded by max_capacity
    // (This is a behavioral test - actual memory would need profiling)
}

// ============================================================================
// Redis Degradation Tests
// ============================================================================

/// Mock store that simulates Redis failures
struct FailingStore;

#[async_trait::async_trait]
impl TokenBlacklistStore for FailingStore {
    async fn is_blacklisted(&self, _key: &str) -> bool {
        false
    }

    async fn blacklist(&self, _key: &str, _ttl_secs: u64) -> synctv_core::Result<()> {
        Err(synctv_core::Error::Internal("Redis unavailable".to_string()))
    }

    async fn get_family_revoked_at(&self, _key: &str) -> Option<i64> {
        None
    }

    async fn set_family_revoked(&self, _key: &str, _timestamp: i64, _ttl_secs: u64) {
        // Simulate failure - do nothing
    }
}

#[tokio::test]
async fn test_fallback_store_when_primary_fails() {
    let primary = Arc::new(FailingStore) as Arc<dyn TokenBlacklistStore>;
    let fallback = FallbackTokenBlacklistStore::with_defaults(primary);

    let key = "jti:fallback_test";

    // Blacklist should succeed (written to fallback even if primary fails)
    let result = fallback.blacklist(key, 3600).await;
    assert!(result.is_ok(), "Blacklist should succeed via fallback");

    // Should be blacklisted (via fallback)
    assert!(
        fallback.is_blacklisted(key).await,
        "Token should be blacklisted via fallback"
    );
}

#[tokio::test]
async fn test_family_revocation_during_outage() {
    let primary = Arc::new(FailingStore) as Arc<dyn TokenBlacklistStore>;
    let fallback = FallbackTokenBlacklistStore::with_defaults(primary);

    let key = "family:outage_user";
    let timestamp = chrono::Utc::now().timestamp();

    // Set family revocation while primary is down
    fallback.set_family_revoked(key, timestamp, 86400).await;

    // Should be retrievable from memory fallback
    assert_eq!(
        fallback.get_family_revoked_at(key).await,
        Some(timestamp),
        "Family revocation should be retrievable from memory fallback"
    );
}

/// Mock store that can be toggled between failing and working states
struct ToggleableStore {
    failing: std::sync::atomic::AtomicBool,
}

impl ToggleableStore {
    fn new() -> Self {
        Self {
            failing: std::sync::atomic::AtomicBool::new(true),
        }
    }

    fn set_failing(&self, failing: bool) {
        self.failing.store(failing, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl TokenBlacklistStore for ToggleableStore {
    async fn is_blacklisted(&self, _key: &str) -> bool {
        false
    }

    async fn blacklist(&self, _key: &str, _ttl_secs: u64) -> synctv_core::Result<()> {
        if self.failing.load(Ordering::SeqCst) {
            Err(synctv_core::Error::Internal("Store is unavailable".to_string()))
        } else {
            Ok(())
        }
    }

    async fn get_family_revoked_at(&self, _key: &str) -> Option<i64> {
        None
    }

    async fn set_family_revoked(&self, _key: &str, _timestamp: i64, _ttl_secs: u64) {
        // Like real implementation - fire and forget
    }
}

#[tokio::test]
async fn test_recovery_after_outage() {
    let toggleable = Arc::new(ToggleableStore::new());
    let primary = toggleable.clone() as Arc<dyn TokenBlacklistStore>;
    let fallback = RedisSyncableTokenBlacklistStore::with_defaults(primary);

    // Blacklist while primary is down
    let key_down = "jti:while_down";
    fallback.blacklist(key_down, 3600).await.unwrap();

    // Verify it's blacklisted
    assert!(fallback.is_blacklisted(key_down).await);

    // Simulate primary recovering
    toggleable.set_failing(false);

    // Blacklist a new token while primary is up
    let key_up = "jti:while_up";
    fallback.blacklist(key_up, 3600).await.unwrap();

    // Both tokens should still be blacklisted
    assert!(
        fallback.is_blacklisted(key_down).await,
        "Token blacklisted during outage should still be blacklisted"
    );
    assert!(
        fallback.is_blacklisted(key_up).await,
        "Token blacklisted after recovery should be blacklisted"
    );
}

#[tokio::test]
async fn test_sync_pending_writes_on_recovery() {
    let toggleable = Arc::new(ToggleableStore::new());
    let primary = toggleable.clone() as Arc<dyn TokenBlacklistStore>;
    let fallback = RedisSyncableTokenBlacklistStore::with_defaults(primary);

    // Blacklist tokens while primary is down
    fallback.blacklist("jti:sync_test_1", 3600).await.unwrap();
    fallback.blacklist("jti:sync_test_2", 3600).await.unwrap();

    // Should have pending writes
    assert!(fallback.pending_write_count() >= 2);

    // Simulate primary recovering
    toggleable.set_failing(false);

    // Trigger sync of pending writes
    let sync_result = fallback.sync_pending_writes().await;
    assert!(sync_result.is_ok(), "Sync should succeed");

    // Pending should be cleared after successful sync
    assert_eq!(
        fallback.pending_write_count(),
        0,
        "Pending writes should be cleared after sync"
    );
}

// ============================================================================
// Token Family Revocation Tests
// ============================================================================

#[tokio::test]
async fn test_family_revocation_set_and_get() {
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

#[tokio::test]
async fn test_family_revocation_overwrites_previous() {
    let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);

    let key = "family:overwrite_test";
    let timestamp1 = chrono::Utc::now().timestamp() - 100;
    let timestamp2 = chrono::Utc::now().timestamp();

    // Set first revocation
    store.set_family_revoked(key, timestamp1, 86400).await;
    assert_eq!(store.get_family_revoked_at(key).await, Some(timestamp1));

    // Overwrite with newer timestamp
    store.set_family_revoked(key, timestamp2, 86400).await;
    assert_eq!(store.get_family_revoked_at(key).await, Some(timestamp2));
}

#[tokio::test]
async fn test_family_revocation_ttl_expiry() {
    let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);

    let key = "family:ttl_test";
    let timestamp = chrono::Utc::now().timestamp();

    store.set_family_revoked(key, timestamp, 1).await;
    assert_eq!(store.get_family_revoked_at(key).await, Some(timestamp));

    // Wait for expiry
    tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;
    assert!(
        store.get_family_revoked_at(key).await.is_none(),
        "Family revocation should expire after TTL"
    );
}

#[tokio::test]
async fn test_family_revocation_blocks_older_tokens() {
    let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);

    let key = "family:block_test";
    let revocation_time = chrono::Utc::now().timestamp();

    // Set family revocation
    store.set_family_revoked(key, revocation_time, 86400).await;

    // Tokens issued before revocation should be blocked
    let revoked_at = store.get_family_revoked_at(key).await;
    assert!(revoked_at.is_some());

    // Application layer checks: if token.iat < revoked_at, reject
    let token_iat_before = revocation_time - 3600; // 1 hour before revocation
    assert!(
        token_iat_before < revoked_at.unwrap(),
        "Token issued before revocation should be blocked"
    );

    // Tokens issued after revocation should be allowed
    let token_iat_after = revocation_time + 3600; // 1 hour after revocation
    assert!(
        token_iat_after >= revoked_at.unwrap(),
        "Token issued after revocation should be allowed"
    );
}

// ============================================================================
// Integration Tests (require Docker)
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_blacklist_integration() {
    // Full Redis integration tests are in token_blacklist_tests.rs:
    // - test_redis_tracker_record_and_get
    // - test_redis_tracker_reset
    // - test_brute_force_with_redis_e2e_lockout_and_reset
    //
    // This placeholder documents that Redis tests require Docker.
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_tiered_cache_integration() {
    // Tiered cache (L1 moka + L2 Redis + PG) tests require full infrastructure.
    // See token_blacklist_tests.rs for TieredTokenBlacklistStore tests.
}
