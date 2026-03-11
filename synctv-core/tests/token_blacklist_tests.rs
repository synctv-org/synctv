//! Token blacklist tests
//!
//! Tests the `InMemoryTokenBlacklistStore` and `FallbackTokenBlacklistStore`.
//!
//! Run with: cargo test --test `token_blacklist_tests` -- --nocapture
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use synctv_core::service::{
    FallbackTokenBlacklistStore, InMemoryTokenBlacklistStore, PgTokenBlacklistStore,
    TokenBlacklistStore,
};
use synctv_core_testing::create_test_pool;

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

// ============================================================================
// Atomic blacklist_if_not_exists tests (Task #10)
// ============================================================================

/// Test that `blacklist_if_not_exists` returns false for first insert (first use)
#[tokio::test]
async fn test_in_memory_blacklist_if_not_exists_first_use() {
    let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);

    let key = "jti:first_use";

    // First call should return false (not existed, newly inserted)
    let already_existed = store.blacklist_if_not_exists(key, 3600).await.unwrap();
    assert!(
        !already_existed,
        "First use should return false (newly inserted)"
    );

    // Key should now be blacklisted
    assert!(store.is_blacklisted(key).await);
}

/// Test that `blacklist_if_not_exists` returns true for second insert (replay detected)
#[tokio::test]
async fn test_in_memory_blacklist_if_not_exists_replay_detected() {
    let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);

    let key = "jti:replay_test";

    // First call
    let first_result = store.blacklist_if_not_exists(key, 3600).await.unwrap();
    assert!(!first_result, "First use should return false");

    // Second call should return true (already existed = replay detected)
    let second_result = store.blacklist_if_not_exists(key, 3600).await.unwrap();
    assert!(
        second_result,
        "Second use should return true (replay detected)"
    );

    // Key should still be blacklisted
    assert!(store.is_blacklisted(key).await);
}

/// Test atomicity: concurrent calls to `blacklist_if_not_exists` should have exactly
/// one return false (first use) and all others return true (replay detected).
#[tokio::test]
async fn test_in_memory_blacklist_if_not_exists_concurrent_atomicity() {
    use std::sync::atomic::{AtomicUsize, Ordering};
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

    // Key should be blacklisted
    assert!(store.is_blacklisted(key).await);
}

#[tokio::test]
async fn test_in_memory_family_revoked_set_and_get() {
    let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);

    let key = "family:user_42";
    let timestamp = chrono::Utc::now().timestamp();

    // Initially no revocation
    assert!(store.get_family_revoked_at(key).await.is_none());

    // Set family revocation
    store
        .set_family_revoked(key, timestamp, 86400)
        .await
        .unwrap();

    // Should be retrievable
    let revoked_at = store.get_family_revoked_at(key).await;
    assert_eq!(revoked_at, Some(timestamp));
}

#[tokio::test]
async fn test_in_memory_family_ttl_expiry() {
    let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);

    let key = "family:ttl_test";
    let timestamp = chrono::Utc::now().timestamp();

    store.set_family_revoked(key, timestamp, 1).await.unwrap();
    assert_eq!(store.get_family_revoked_at(key).await, Some(timestamp));

    // Wait for expiry
    tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;
    assert!(
        store.get_family_revoked_at(key).await.is_none(),
        "Family revocation should expire after TTL"
    );
}

// ============================================================================
// FallbackTokenBlacklistStore tests
// ============================================================================

#[tokio::test]
async fn test_fallback_blacklist_roundtrip() {
    // Use InMemory as both primary and fallback (simulating working primary)
    let primary = std::sync::Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400))
        as std::sync::Arc<dyn TokenBlacklistStore>;
    let fallback = FallbackTokenBlacklistStore::with_defaults(primary);

    let key = "jti:fallback_test";
    assert!(!fallback.is_blacklisted(key).await);

    fallback.blacklist(key, 3600).await.unwrap();
    assert!(fallback.is_blacklisted(key).await);
}

#[tokio::test]
async fn test_fallback_family_roundtrip() {
    let primary = std::sync::Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400))
        as std::sync::Arc<dyn TokenBlacklistStore>;
    let fallback = FallbackTokenBlacklistStore::with_defaults(primary);

    let key = "family:fallback_test";
    let timestamp = chrono::Utc::now().timestamp();

    assert!(fallback.get_family_revoked_at(key).await.is_none());

    fallback
        .set_family_revoked(key, timestamp, 86400)
        .await
        .unwrap();
    assert_eq!(fallback.get_family_revoked_at(key).await, Some(timestamp));
}

#[tokio::test]
async fn test_fallback_ttl_expiry() {
    let primary = std::sync::Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400))
        as std::sync::Arc<dyn TokenBlacklistStore>;
    let fallback = FallbackTokenBlacklistStore::with_defaults(primary);

    let key = "jti:fallback_ttl_test";

    fallback.blacklist(key, 1).await.unwrap();
    assert!(fallback.is_blacklisted(key).await);

    // Wait for expiry
    tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;

    assert!(
        !fallback.is_blacklisted(key).await,
        "Token should no longer be blacklisted after TTL expiry"
    );
}

#[tokio::test]
async fn test_fallback_family_ttl_expiry() {
    let primary = std::sync::Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400))
        as std::sync::Arc<dyn TokenBlacklistStore>;
    let fallback = FallbackTokenBlacklistStore::with_defaults(primary);

    let key = "family:fallback_ttl_test";
    let timestamp = chrono::Utc::now().timestamp();

    fallback
        .set_family_revoked(key, timestamp, 1)
        .await
        .unwrap();
    assert_eq!(fallback.get_family_revoked_at(key).await, Some(timestamp));

    // Wait for expiry
    tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;

    assert!(
        fallback.get_family_revoked_at(key).await.is_none(),
        "Family revocation should expire after TTL"
    );
}

#[tokio::test]
async fn test_fallback_blacklist_if_not_exists_respects_authoritative_primary_replay() {
    let primary = std::sync::Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400))
        as std::sync::Arc<dyn TokenBlacklistStore>;
    let fallback = FallbackTokenBlacklistStore::with_defaults(primary.clone());

    let key = "jti:primary_replay_only";
    primary.blacklist(key, 3600).await.unwrap();

    let already_existed = fallback.blacklist_if_not_exists(key, 3600).await.unwrap();
    assert!(
        already_existed,
        "Primary already contained the JTI, so this must be treated as replay"
    );
    assert!(fallback.is_blacklisted(key).await);
}

#[tokio::test]
#[ignore = "requires Docker (PostgreSQL testcontainer)"]
async fn test_pg_family_revocation_survives_cleanup_until_marker_expires() {
    let (_container, pool) = create_test_pool().await;
    let store = PgTokenBlacklistStore::new(pool);
    let key = format!("family:pg_cleanup_guard:{}", nanoid::nanoid!(8));
    let timestamp = chrono::Utc::now().timestamp();

    store
        .set_family_revoked(&key, timestamp, 120)
        .await
        .unwrap();
    store.cleanup_expired().await.unwrap();

    assert_eq!(
        store.get_family_revoked_at(&key).await,
        Some(timestamp),
        "cleanup must not delete the family revocation timestamp while the marker is still alive"
    );
}

#[tokio::test]
#[ignore = "requires Docker (PostgreSQL testcontainer)"]
async fn test_pg_family_revocation_timestamp_is_stable_across_reads() {
    let (_container, pool) = create_test_pool().await;
    let store = PgTokenBlacklistStore::new(pool);
    let key = format!("family:pg_stable_ts:{}", nanoid::nanoid!(8));
    let timestamp = chrono::Utc::now().timestamp();

    store
        .set_family_revoked(&key, timestamp, 120)
        .await
        .unwrap();

    let first = store.get_family_revoked_at(&key).await;
    tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;
    let second = store.get_family_revoked_at(&key).await;

    assert_eq!(first, Some(timestamp));
    assert_eq!(
        second,
        Some(timestamp),
        "family revocation timestamp must remain stable instead of drifting with wall-clock time"
    );
}

#[tokio::test]
#[ignore = "requires Docker (PostgreSQL testcontainer)"]
async fn test_pg_family_revocation_is_atomic_when_timestamp_write_fails() {
    let (_container, pool) = create_test_pool().await;
    let store = PgTokenBlacklistStore::new(pool.clone());
    let key = format!("family:pg_atomicity_guard:{}", nanoid::nanoid!(8));
    let timestamp = chrono::Utc::now().timestamp();

    let trigger_fn_sql = r#"
        CREATE OR REPLACE FUNCTION fail_token_blacklist_family_insert()
        RETURNS trigger AS $$
        BEGIN
            IF NEW.jti = 'REPLACE_ME' THEN
                RAISE EXCEPTION 'forced family timestamp failure';
            END IF;
            RETURN NEW;
        END;
        $$ LANGUAGE plpgsql;
        "#
    .replace("REPLACE_ME", &key);

    sqlx::query(&trigger_fn_sql).execute(&pool).await.unwrap();

    sqlx::query("DROP TRIGGER IF EXISTS trg_fail_token_blacklist_family_insert ON token_blacklist")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"
        CREATE TRIGGER trg_fail_token_blacklist_family_insert
        BEFORE INSERT OR UPDATE ON token_blacklist
        FOR EACH ROW
        EXECUTE FUNCTION fail_token_blacklist_family_insert()
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let result = store.set_family_revoked(&key, timestamp, 120).await;
    assert!(
        result.is_err(),
        "forced family revoke write failure must bubble up as an error"
    );

    let marker_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM token_blacklist WHERE jti = $1)")
            .bind(&key)
            .fetch_one(&pool)
            .await
            .unwrap();

    assert!(
        !marker_exists,
        "family revoke must be atomic: no partial rows should remain after failure"
    );

    sqlx::query("DROP TRIGGER IF EXISTS trg_fail_token_blacklist_family_insert ON token_blacklist")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DROP FUNCTION IF EXISTS fail_token_blacklist_family_insert()")
        .execute(&pool)
        .await
        .unwrap();
}

/// Mock store that always fails - for testing fallback behavior
struct FailingStore;

#[async_trait::async_trait]
impl TokenBlacklistStore for FailingStore {
    async fn is_blacklisted(&self, _key: &str) -> bool {
        false
    }

    async fn is_blacklisted_checked(&self, _key: &str) -> synctv_core::Result<bool> {
        Ok(false)
    }

    async fn blacklist(&self, _key: &str, _ttl_secs: u64) -> synctv_core::Result<()> {
        Err(synctv_core::Error::Internal(
            "Primary store failed".to_string(),
        ))
    }

    async fn get_family_revoked_at(&self, _key: &str) -> Option<i64> {
        None
    }

    async fn set_family_revoked(
        &self,
        _key: &str,
        _timestamp: i64,
        _ttl_secs: u64,
    ) -> synctv_core::Result<()> {
        Err(synctv_core::Error::Internal(
            "Primary store failed".to_string(),
        ))
    }
}

#[tokio::test]
async fn test_fallback_with_failing_primary_still_tracks_blacklist() {
    // Use a failing store as primary
    let primary = std::sync::Arc::new(FailingStore) as std::sync::Arc<dyn TokenBlacklistStore>;
    let fallback = FallbackTokenBlacklistStore::with_defaults(primary);

    let key = "jti:failing_primary_test";

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
async fn test_fallback_with_failing_primary_still_tracks_family() {
    let primary = std::sync::Arc::new(FailingStore) as std::sync::Arc<dyn TokenBlacklistStore>;
    let fallback = FallbackTokenBlacklistStore::with_defaults(primary);

    let key = "family:failing_primary_test";
    let timestamp = chrono::Utc::now().timestamp();

    // Family revocation must fail closed when the primary store cannot persist
    // it, while still leaving degraded local state in the fallback store.
    let result = fallback.set_family_revoked(key, timestamp, 86400).await;
    assert!(result.is_err());

    // Should be retrievable (via fallback)
    assert_eq!(
        fallback.get_family_revoked_at(key).await,
        Some(timestamp),
        "Family revocation should be retrievable via fallback"
    );
}

// ============================================================================
// Memory Fallback with Sync Tests (Task #18)
// ============================================================================

/// Mock store that can be toggled between failing and working states.
/// This simulates Redis becoming unavailable and then recovering.
struct ToggleableStore {
    /// When true, all operations fail
    failing: std::sync::atomic::AtomicBool,
    /// Track blacklist calls
    blacklist_calls: std::sync::atomic::AtomicU64,
    /// Track `set_family_revoked` calls
    family_calls: std::sync::atomic::AtomicU64,
}

impl ToggleableStore {
    const fn new() -> Self {
        Self {
            failing: std::sync::atomic::AtomicBool::new(true),
            blacklist_calls: std::sync::atomic::AtomicU64::new(0),
            family_calls: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn set_failing(&self, failing: bool) {
        self.failing
            .store(failing, std::sync::atomic::Ordering::SeqCst);
    }

    fn blacklist_call_count(&self) -> u64 {
        self.blacklist_calls
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    fn family_call_count(&self) -> u64 {
        self.family_calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl TokenBlacklistStore for ToggleableStore {
    async fn is_blacklisted(&self, _key: &str) -> bool {
        false
    }

    async fn is_blacklisted_checked(&self, _key: &str) -> synctv_core::Result<bool> {
        Ok(false)
    }

    async fn blacklist(&self, _key: &str, _ttl_secs: u64) -> synctv_core::Result<()> {
        self.blacklist_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.failing.load(std::sync::atomic::Ordering::SeqCst) {
            Err(synctv_core::Error::Internal(
                "Store is unavailable".to_string(),
            ))
        } else {
            Ok(())
        }
    }

    async fn get_family_revoked_at(&self, _key: &str) -> Option<i64> {
        None
    }

    async fn set_family_revoked(
        &self,
        _key: &str,
        _timestamp: i64,
        _ttl_secs: u64,
    ) -> synctv_core::Result<()> {
        self.family_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.failing.load(std::sync::atomic::Ordering::SeqCst) {
            Err(synctv_core::Error::Internal(
                "Store is unavailable".to_string(),
            ))
        } else {
            Ok(())
        }
    }
}

/// Test 1: Memory fallback when primary (simulating Redis) is unavailable
#[tokio::test]
async fn test_memory_fallback_when_primary_unavailable() {
    // Create a failing primary store (simulating Redis being down)
    let primary = Arc::new(ToggleableStore::new()) as Arc<dyn TokenBlacklistStore>;
    let fallback = FallbackTokenBlacklistStore::with_defaults(primary.clone());

    let key = "jti:redis_down_test";

    // Blacklist should succeed even though primary fails
    let result = fallback.blacklist(key, 3600).await;
    assert!(
        result.is_ok(),
        "Blacklist should succeed via memory fallback"
    );

    // Token should be blacklisted (via fallback memory)
    assert!(
        fallback.is_blacklisted(key).await,
        "Token should be blacklisted in memory fallback"
    );
}

/// Test 2: Multiple blacklist operations accumulate in memory fallback during outage
#[tokio::test]
async fn test_multiple_blacklists_during_outage() {
    let primary = Arc::new(ToggleableStore::new()) as Arc<dyn TokenBlacklistStore>;
    let fallback = FallbackTokenBlacklistStore::with_defaults(primary.clone());

    // Blacklist multiple tokens while primary is down
    for i in 0..10 {
        let key = format!("jti:outage_{i}");
        let result = fallback.blacklist(&key, 3600).await;
        assert!(result.is_ok(), "Blacklist {i} should succeed via fallback");
    }

    // All tokens should be blacklisted in fallback
    for i in 0..10 {
        let key = format!("jti:outage_{i}");
        assert!(
            fallback.is_blacklisted(&key).await,
            "Token {i} should be blacklisted in memory fallback"
        );
    }
}

/// Test 3: Family revocations work during outage
#[tokio::test]
async fn test_family_revocation_during_outage() {
    let primary = Arc::new(ToggleableStore::new()) as Arc<dyn TokenBlacklistStore>;
    let fallback = FallbackTokenBlacklistStore::with_defaults(primary.clone());

    let key = "family:outage_user";
    let timestamp = chrono::Utc::now().timestamp();

    // Family revocation is fail-closed, so the call must error while still
    // leaving degraded fallback state available locally.
    let result = fallback.set_family_revoked(key, timestamp, 86400).await;
    assert!(result.is_err());

    // Should be retrievable from memory fallback
    assert_eq!(
        fallback.get_family_revoked_at(key).await,
        Some(timestamp),
        "Family revocation should be retrievable from memory fallback"
    );
}

/// Test 4: Verify that fallback persists data independently of primary state
#[tokio::test]
async fn test_fallback_persists_independently() {
    let toggleable = Arc::new(ToggleableStore::new());
    let primary = toggleable.clone() as Arc<dyn TokenBlacklistStore>;
    let fallback = FallbackTokenBlacklistStore::with_defaults(primary);

    // Blacklist while primary is down
    let key_down = "jti:while_down";
    fallback.blacklist(key_down, 3600).await.unwrap();

    // Verify it's blacklisted
    assert!(fallback.is_blacklisted(key_down).await);

    // Now simulate primary recovering
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

/// Test 5: Memory fallback with TTL expiry during extended outage
#[tokio::test]
async fn test_fallback_ttl_during_extended_outage() {
    let primary = Arc::new(ToggleableStore::new()) as Arc<dyn TokenBlacklistStore>;
    let fallback = FallbackTokenBlacklistStore::with_defaults(primary);

    let key_short = "jti:short_ttl_outage";
    let key_long = "jti:long_ttl_outage";

    // Blacklist with different TTLs
    fallback.blacklist(key_short, 1).await.unwrap(); // 1 second
    fallback.blacklist(key_long, 3600).await.unwrap(); // 1 hour

    // Both should be blacklisted initially
    assert!(fallback.is_blacklisted(key_short).await);
    assert!(fallback.is_blacklisted(key_long).await);

    // Wait for short TTL to expire
    tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;

    // Short TTL should be expired, long TTL should still be valid
    assert!(
        !fallback.is_blacklisted(key_short).await,
        "Short TTL token should be expired"
    );
    assert!(
        fallback.is_blacklisted(key_long).await,
        "Long TTL token should still be blacklisted"
    );
}

/// Test 6: Verify primary is called even when fallback holds the data
/// This tests that the system tries to maintain consistency with primary
#[tokio::test]
async fn test_primary_called_even_with_fallback() {
    let toggleable = Arc::new(ToggleableStore::new());
    let primary = toggleable.clone() as Arc<dyn TokenBlacklistStore>;
    let fallback = FallbackTokenBlacklistStore::with_defaults(primary);

    // Make primary work
    toggleable.set_failing(false);

    let key = "jti:primary_called";

    // Blacklist should write to both primary and fallback
    fallback.blacklist(key, 3600).await.unwrap();

    // Primary should have been called
    assert_eq!(
        toggleable.blacklist_call_count(),
        1,
        "Primary should have been called once"
    );

    // Now make primary fail
    toggleable.set_failing(true);

    // Blacklist another key - should succeed via fallback
    let key2 = "jti:primary_failing";
    let result = fallback.blacklist(key2, 3600).await;
    assert!(result.is_ok());

    // Primary was called (and failed)
    assert_eq!(
        toggleable.blacklist_call_count(),
        2,
        "Primary should have been attempted even though it failed"
    );
}

// ============================================================================
// Sync to Redis on Recovery Tests (Task #18)
// ============================================================================

use synctv_core::service::RedisSyncableTokenBlacklistStore;

/// Test 7: Pending writes are synced when Redis recovers
#[tokio::test]
async fn test_sync_pending_writes_on_recovery() {
    let toggleable = Arc::new(ToggleableStore::new());
    let primary = toggleable.clone() as Arc<dyn TokenBlacklistStore>;
    let fallback = RedisSyncableTokenBlacklistStore::with_defaults(primary);

    // Blacklist tokens while primary is down
    fallback.blacklist("jti:sync_test_1", 3600).await.unwrap();
    fallback.blacklist("jti:sync_test_2", 3600).await.unwrap();

    // Primary should have failed both times
    assert_eq!(toggleable.blacklist_call_count(), 2);

    // Now simulate primary recovering
    toggleable.set_failing(false);

    // Trigger sync of pending writes
    let sync_result = fallback.sync_pending_writes().await;
    assert!(sync_result.is_ok(), "Sync should succeed");

    // Primary should have been called for both pending writes
    assert_eq!(
        toggleable.blacklist_call_count(),
        4,
        "Primary should have been called 2 more times for sync"
    );
}

/// Test 8: Sync only attempts non-expired entries
#[tokio::test]
async fn test_sync_skips_expired_entries() {
    let toggleable = Arc::new(ToggleableStore::new());
    let primary = toggleable.clone() as Arc<dyn TokenBlacklistStore>;
    let fallback = RedisSyncableTokenBlacklistStore::with_defaults(primary);

    // Blacklist with very short TTL
    fallback.blacklist("jti:expired_sync", 1).await.unwrap();
    fallback.blacklist("jti:valid_sync", 3600).await.unwrap();

    // Wait for short TTL to expire
    tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;

    // Recover primary
    toggleable.set_failing(false);

    // Sync pending writes
    let sync_result = fallback.sync_pending_writes().await;
    assert!(sync_result.is_ok());

    // Only the valid entry should have been synced (2 initial + 1 sync)
    assert_eq!(
        toggleable.blacklist_call_count(),
        3,
        "Only valid (non-expired) entry should be synced"
    );
}

/// Test 9: Family revocations are also synced
#[tokio::test]
async fn test_sync_family_revocations() {
    let toggleable = Arc::new(ToggleableStore::new());
    let primary = toggleable.clone() as Arc<dyn TokenBlacklistStore>;
    let fallback = RedisSyncableTokenBlacklistStore::with_defaults(primary);

    let timestamp = chrono::Utc::now().timestamp();

    // Simulate primary outage for the initial write.
    toggleable.set_failing(true);

    // Set family revocation while primary is down
    let result = fallback
        .set_family_revoked("family:sync_test", timestamp, 86400)
        .await;
    assert!(
        result.is_err(),
        "family revocation must fail closed when primary persistence fails"
    );

    // Recover primary
    toggleable.set_failing(false);

    // Sync pending writes
    let sync_result = fallback.sync_pending_writes().await;
    assert!(sync_result.is_ok());

    // Family revocation should have been synced
    assert_eq!(
        toggleable.family_call_count(),
        2,
        "Family revocation should have been synced (initial + sync)"
    );
}

/// Test 10: Clear pending writes after successful sync
#[tokio::test]
async fn test_pending_cleared_after_sync() {
    let toggleable = Arc::new(ToggleableStore::new());
    let primary = toggleable.clone() as Arc<dyn TokenBlacklistStore>;
    let fallback = RedisSyncableTokenBlacklistStore::with_defaults(primary);

    // Blacklist while primary is down
    fallback.blacklist("jti:clear_test", 3600).await.unwrap();

    // Should have pending writes
    assert_eq!(fallback.pending_write_count(), 1);

    // Recover and sync
    toggleable.set_failing(false);
    fallback.sync_pending_writes().await.unwrap();

    // Pending should be cleared
    assert_eq!(
        fallback.pending_write_count(),
        0,
        "Pending writes should be cleared after sync"
    );
}

/// Test 11: Sync handles partial failures gracefully
#[tokio::test]
async fn test_sync_handles_partial_failures() {
    // This test simulates a scenario where some sync operations fail
    // The sync should continue and report which ones failed
    let toggleable = Arc::new(ToggleableStore::new());
    let primary = toggleable.clone() as Arc<dyn TokenBlacklistStore>;
    let fallback = RedisSyncableTokenBlacklistStore::with_defaults(primary);

    // Blacklist multiple tokens while primary is down
    fallback.blacklist("jti:partial_1", 3600).await.unwrap();
    fallback.blacklist("jti:partial_2", 3600).await.unwrap();
    fallback.blacklist("jti:partial_3", 3600).await.unwrap();

    // Keep primary failing - sync will fail
    let _sync_result = fallback.sync_pending_writes().await;
    // Sync should still return Ok but with 0 synced (all failed)
    // Pending count should remain the same
    assert_eq!(fallback.pending_write_count(), 3);
}

/// Test 12: Duplicate syncs are idempotent
#[tokio::test]
async fn test_sync_is_idempotent() {
    let toggleable = Arc::new(ToggleableStore::new());
    let primary = toggleable.clone() as Arc<dyn TokenBlacklistStore>;
    let fallback = RedisSyncableTokenBlacklistStore::with_defaults(primary);

    // Blacklist while primary is down
    fallback.blacklist("jti:idempotent", 3600).await.unwrap();

    // Recover primary
    toggleable.set_failing(false);

    // Sync multiple times
    fallback.sync_pending_writes().await.unwrap();
    let count_after_first = toggleable.blacklist_call_count();

    // Second sync should do nothing (pending already cleared)
    fallback.sync_pending_writes().await.unwrap();
    assert_eq!(
        toggleable.blacklist_call_count(),
        count_after_first,
        "Second sync should not call primary again"
    );
}

#[tokio::test]
async fn test_redis_syncable_blacklist_if_not_exists_respects_authoritative_primary_replay() {
    let primary = Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400))
        as Arc<dyn TokenBlacklistStore>;
    let fallback = RedisSyncableTokenBlacklistStore::with_defaults(primary.clone());

    let key = "jti:redis_syncable_primary_replay_only";
    primary.blacklist(key, 3600).await.unwrap();

    let already_existed = fallback.blacklist_if_not_exists(key, 3600).await.unwrap();
    assert!(
        already_existed,
        "Primary already contained the JTI, so redis-syncable wrapper must report replay"
    );
    assert!(fallback.is_blacklisted(key).await);
}
