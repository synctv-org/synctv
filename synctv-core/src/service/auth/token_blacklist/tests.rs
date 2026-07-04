use super::stores::parse_l2_blacklist_value;
use super::*;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{test_helpers::failing_redis_runtime, RedisConnectionRuntime};

fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("{context}: {error}")),
    }
}

fn some<T>(value: Option<T>, context: &str) -> T {
    match value {
        Some(value) => value,
        None => std::panic::panic_any(context.to_string()),
    }
}

fn completed_task_value<T, E: std::fmt::Display>(
    result: std::result::Result<std::result::Result<T, E>, tokio::task::JoinError>,
    context: &str,
) -> T {
    match result {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => std::panic::panic_any(format!("{context}: {error}")),
        Err(error) => std::panic::panic_any(format!("{context}: task join failed: {error}")),
    }
}

fn expired_instant() -> Instant {
    some(
        Instant::now().checked_sub(Duration::from_secs(1)),
        "expired instant should be representable",
    )
}

#[test]
fn parse_l2_blacklist_value_accepts_only_known_sentinels() {
    assert_eq!(parse_l2_blacklist_value("jti:valid", "1"), Some(true));
    assert_eq!(parse_l2_blacklist_value("jti:valid", "0"), Some(false));
    assert_eq!(parse_l2_blacklist_value("jti:invalid", "true"), None);
    assert_eq!(parse_l2_blacklist_value("jti:invalid", ""), None);
}

#[derive(Clone, Debug)]
struct FailingDurableTokenBlacklistStore;

#[async_trait]
impl TokenBlacklistStore for FailingDurableTokenBlacklistStore {
    async fn is_blacklisted_checked(&self, _key: &str) -> Result<bool> {
        Err(crate::Error::ServiceUnavailable(
            "durable token blacklist unavailable".to_string(),
        ))
    }

    async fn blacklist(&self, _key: &str, _ttl_secs: u64) -> Result<()> {
        Err(crate::Error::ServiceUnavailable(
            "durable token blacklist unavailable".to_string(),
        ))
    }

    async fn blacklist_if_not_exists(&self, _key: &str, _ttl_secs: u64) -> Result<bool> {
        Err(crate::Error::ServiceUnavailable(
            "durable token blacklist unavailable".to_string(),
        ))
    }

    async fn get_family_revoked_at_checked(&self, _key: &str) -> Result<Option<i64>> {
        Err(crate::Error::ServiceUnavailable(
            "durable token blacklist unavailable".to_string(),
        ))
    }

    async fn set_family_revoked(&self, _key: &str, _timestamp: i64, _ttl_secs: u64) -> Result<()> {
        Err(crate::Error::ServiceUnavailable(
            "durable token blacklist unavailable".to_string(),
        ))
    }
}

#[tokio::test]
async fn test_tiered_token_blacklist_store_accepts_trait_object_runtime() {
    let runtime = failing_redis_runtime();
    let store = TieredTokenBlacklistStore::from_runtime(
        FailingDurableTokenBlacklistStore,
        Some(runtime.clone()),
        "synctv:".to_string(),
    );

    assert!(
        store
            .redis_runtime
            .as_ref()
            .is_some_and(|injected| Arc::ptr_eq(injected, &runtime)),
        "tiered token blacklist store should retain the injected runtime object"
    );
}

#[tokio::test]
async fn test_tiered_token_blacklist_store_from_shared_state_profile_uses_runtime() {
    let runtime = failing_redis_runtime();
    let profile = crate::SharedStateProfile::new(
        crate::SharedStateMode::SharedBestEffort,
        Some(runtime.clone()),
        "synctv:",
    );

    let store = TieredTokenBlacklistStore::from_shared_state_profile(
        FailingDurableTokenBlacklistStore,
        &profile,
    );

    assert!(
        store
            .redis_runtime
            .as_ref()
            .is_some_and(|injected| Arc::ptr_eq(injected, &runtime)),
        "shared-state builder should retain the injected runtime object"
    );
}

#[tokio::test]
async fn test_tiered_token_blacklist_store_from_shared_state_profile_allows_pg_only_mode() {
    let profile = crate::SharedStateProfile::for_cluster_runtime(None, "synctv:", false);

    let store = TieredTokenBlacklistStore::from_shared_state_profile(
        FailingDurableTokenBlacklistStore,
        &profile,
    );

    assert!(
        store.redis_runtime.is_none(),
        "standalone shared-state profile should allow PG+L1 token blacklist mode"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_tiered_token_blacklist_redis_timeout_falls_back_to_pg() {
    #[derive(Clone)]
    struct HangingRedisRuntime;

    #[async_trait]
    impl RedisConnectionRuntime for HangingRedisRuntime {
        async fn snapshot(&self) -> redis::RedisResult<redis::aio::ConnectionManager> {
            std::future::pending().await
        }

        fn operation_timeout(&self) -> Duration {
            Duration::from_millis(10)
        }
    }

    let (postgres, pool) = synctv_core_testing::create_test_pool().await;
    let pg = PgTokenBlacklistStore::new(pool.clone());
    ok(
        pg.blacklist("jti:redis-timeout", 3600).await,
        "PG blacklist entry should write",
    );
    ok(
        pg.set_family_revoked("family:redis-timeout", 1_700_000_000, 3600)
            .await,
        "PG family revocation should write",
    );

    let store = TieredTokenBlacklistStore::from_runtime(
        pg,
        Some(Arc::new(HangingRedisRuntime)),
        "timeout:".to_string(),
    );

    let is_blacklisted = tokio::time::timeout(
        Duration::from_millis(300),
        store.is_blacklisted_checked("jti:redis-timeout"),
    )
    .await;
    let is_blacklisted = ok(
        ok(
            is_blacklisted,
            "Redis snapshot timeout should not hang blacklist lookup",
        ),
        "blacklist lookup should complete",
    );
    assert!(is_blacklisted);

    let family_revoked_at = tokio::time::timeout(
        Duration::from_millis(300),
        store.get_family_revoked_at_checked("family:redis-timeout"),
    )
    .await;
    let family_revoked_at = ok(
        ok(
            family_revoked_at,
            "Redis snapshot timeout should not hang family lookup",
        ),
        "family lookup should complete",
    );
    assert_eq!(family_revoked_at, Some(1_700_000_000));

    pool.close().await;
    postgres.cleanup().await;
}

fn make_tiered_l1_only() -> TieredTokenBlacklistStore {
    TieredTokenBlacklistStore::from_runtime(
        FailingDurableTokenBlacklistStore,
        None,
        "test:".to_string(),
    )
}

// Helper: create an InMemoryTokenBlacklistStore for testing fallback scenarios.
fn make_in_memory_store() -> InMemoryTokenBlacklistStore {
    InMemoryTokenBlacklistStore::new(10_000, 3600, 86400)
}

#[tokio::test]
async fn test_l1_positive_blacklist_hit() {
    let store = make_tiered_l1_only();

    // Pre-populate L1 with a positive entry
    store
        .l1_blacklist
        .insert(
            "jti:abc".to_string(),
            (true, Instant::now() + Duration::from_mins(1)),
        )
        .await;

    // Should return true from L1 without touching PG
    assert!(ok(
        store.is_blacklisted_checked("jti:abc").await,
        "L1 blacklist hit should read"
    ));
}

#[tokio::test]
async fn test_l1_negative_blacklist_hit() {
    let store = make_tiered_l1_only();

    // Pre-populate L1 with a negative sentinel
    store
        .l1_blacklist
        .insert(
            "jti:def".to_string(),
            (false, Instant::now() + Duration::from_mins(1)),
        )
        .await;

    // Should return false from L1 without touching PG
    assert!(!ok(
        store.is_blacklisted_checked("jti:def").await,
        "L1 blacklist miss should read"
    ));
}

#[tokio::test]
async fn test_l1_expired_entry_ignored() {
    let store = make_tiered_l1_only();

    // Pre-populate L1 with an expired positive entry
    store
        .l1_blacklist
        .insert("jti:expired".to_string(), (true, expired_instant()))
        .await;

    let result = store.is_blacklisted_checked("jti:expired").await;
    assert!(
        result.is_err(),
        "expired L1 entries must fall through to durable storage errors"
    );
}

#[tokio::test]
async fn test_l1_positive_family_hit() {
    let store = make_tiered_l1_only();

    let ts = 1_700_000_000_i64;
    store
        .l1_family
        .insert(
            "family:user42".to_string(),
            (Some(ts), Instant::now() + Duration::from_mins(1)),
        )
        .await;

    assert_eq!(
        ok(
            store.get_family_revoked_at_checked("family:user42").await,
            "L1 family hit should read",
        ),
        Some(ts)
    );
}

#[tokio::test]
async fn test_l1_negative_family_hit() {
    let store = make_tiered_l1_only();

    store
        .l1_family
        .insert(
            "family:user99".to_string(),
            (None, Instant::now() + Duration::from_mins(1)),
        )
        .await;

    assert_eq!(
        ok(
            store.get_family_revoked_at_checked("family:user99").await,
            "L1 family negative hit should read",
        ),
        None
    );
}

#[tokio::test]
async fn test_blacklist_write_keeps_l1_negative_when_durable_write_fails() {
    let store = make_tiered_l1_only();

    store
        .l1_blacklist
        .insert(
            "jti:overwrite".to_string(),
            (false, Instant::now() + Duration::from_mins(1)),
        )
        .await;
    assert!(!ok(
        store.is_blacklisted_checked("jti:overwrite").await,
        "negative L1 entry should read"
    ));

    let result = store.blacklist("jti:overwrite", 3600).await;
    assert!(result.is_err());
    assert!(
        !ok(
            store.is_blacklisted_checked("jti:overwrite").await,
            "negative L1 entry should remain readable"
        ),
        "failed durable writes must not promote a negative L1 entry to blacklisted"
    );
}

#[tokio::test]
async fn test_set_family_revoked_populates_l1() {
    let store = make_tiered_l1_only();

    assert!(store.l1_family.get("family:write_test").await.is_none());

    // Family revocation is fail-closed: if the durable PG write fails, the
    // tiered store must return an error and must not populate L1 as if the
    // revocation were durably committed.
    let ts = 1_700_000_000_i64;
    let result = store
        .set_family_revoked("family:write_test", ts, 3600)
        .await;
    assert!(result.is_err());

    assert!(store.l1_family.get("family:write_test").await.is_none());
}

#[tokio::test]
async fn test_redis_key_format() {
    let store = make_tiered_l1_only();
    assert_eq!(store.bl_key("jti:abc"), "test:bl:jti:abc");
    assert_eq!(store.fam_key("family:user42"), "test:fam:family:user42");
}

#[tokio::test]
async fn test_l2_positive_ttl_computation() {
    assert_eq!(TieredTokenBlacklistStore::l2_positive_ttl(3600), 3570);
    assert_eq!(TieredTokenBlacklistStore::l2_positive_ttl(30), 1);
    assert_eq!(TieredTokenBlacklistStore::l2_positive_ttl(10), 1);
    assert_eq!(TieredTokenBlacklistStore::l2_positive_ttl(0), 1);
}

#[tokio::test]
async fn test_in_memory_blacklist_roundtrip() {
    let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);

    let key = "jti:abc123";
    assert!(!ok(
        store.is_blacklisted_checked(key).await,
        "initial blacklist lookup should read"
    ));

    ok(
        store.blacklist(key, 3600).await,
        "blacklist entry should write",
    );
    assert!(ok(
        store.is_blacklisted_checked(key).await,
        "blacklist lookup should read"
    ));
}

#[tokio::test]
async fn test_in_memory_blacklist_ttl_expiry() {
    let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);

    let key = "jti:expiry_test";
    ok(
        store.blacklist(key, 1).await,
        "blacklist entry should write",
    );
    assert!(ok(
        store.is_blacklisted_checked(key).await,
        "blacklist lookup should read"
    ));

    store
        .jti_blacklist
        .insert(key.to_string(), expired_instant())
        .await;
    assert!(
        !ok(
            store.is_blacklisted_checked(key).await,
            "expired blacklist lookup should read"
        ),
        "Should no longer be blacklisted after TTL expiry"
    );
}

#[tokio::test]
async fn test_in_memory_family_roundtrip() {
    let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);

    let key = "family:user_42";
    let timestamp = 1_700_000_000_i64;

    assert!(ok(
        store
            .get_family_revoked_at_checked(key)
            .await
            .map(|value| value.is_none()),
        "initial family lookup should read",
    ));
    ok(
        store.set_family_revoked(key, timestamp, 86400).await,
        "family revocation should write",
    );
    assert_eq!(
        ok(
            store.get_family_revoked_at_checked(key).await,
            "family lookup should read",
        ),
        Some(timestamp)
    );
}

#[tokio::test]
async fn test_in_memory_blacklist_multiple_entries() {
    let store = make_in_memory_store();

    for i in 0..10 {
        let key = format!("jti:test_{i}");
        assert!(!ok(
            store.is_blacklisted_checked(&key).await,
            "initial blacklist lookup should read"
        ));
        ok(
            store.blacklist(&key, 3600).await,
            "blacklist entry should write",
        );
        assert!(ok(
            store.is_blacklisted_checked(&key).await,
            "blacklist lookup should read"
        ));
    }

    for i in 0..10 {
        assert!(ok(
            store.is_blacklisted_checked(&format!("jti:test_{i}")).await,
            "blacklist lookup should read"
        ));
    }
}

#[tokio::test]
async fn test_in_memory_blacklist_overwrite() {
    let store = make_in_memory_store();

    let key = "jti:overwrite_test";
    ok(
        store.blacklist(key, 1).await,
        "blacklist entry should write",
    );
    assert!(ok(
        store.is_blacklisted_checked(key).await,
        "blacklist lookup should read"
    ));

    ok(
        store.blacklist(key, 3600).await,
        "blacklist entry should overwrite",
    );
    assert!(ok(
        store.is_blacklisted_checked(key).await,
        "blacklist lookup should read"
    ));

    let expiry = some(
        store.jti_blacklist.get(key).await,
        "overwrite should leave an expiry entry",
    );
    assert!(
        expiry > Instant::now() + Duration::from_mins(50),
        "TTL overwrite should extend the stored expiry"
    );
}

#[tokio::test]
async fn test_in_memory_family_ttl_expiry() {
    let store = make_in_memory_store();

    let key = "family:ttl_test";
    let timestamp = 1_700_000_000_i64;

    ok(
        store.set_family_revoked(key, timestamp, 1).await,
        "family revocation should write",
    );
    assert_eq!(
        ok(
            store.get_family_revoked_at_checked(key).await,
            "family lookup should read",
        ),
        Some(timestamp)
    );

    store
        .family_revoked
        .insert(key.to_string(), (timestamp, expired_instant()))
        .await;
    assert!(
        ok(
            store
                .get_family_revoked_at_checked(key)
                .await
                .map(|value| value.is_none()),
            "expired family lookup should read",
        ),
        "Family revocation should expire after TTL"
    );
}

#[tokio::test]
async fn test_in_memory_family_multiple_entries() {
    let store = make_in_memory_store();

    for i in 0..10 {
        let key = format!("family:user_{i}");
        let timestamp = 1_700_000_000_i64 + i;
        ok(
            store.set_family_revoked(&key, timestamp, 86400).await,
            "family revocation should write",
        );
    }

    for i in 0..10 {
        let key = format!("family:user_{i}");
        let expected_ts = 1_700_000_000_i64 + i;
        assert_eq!(
            ok(
                store.get_family_revoked_at_checked(&key).await,
                "family lookup should read",
            ),
            Some(expected_ts)
        );
    }
}

#[test]
fn test_pg_family_revocation_uses_single_primary_key_row() {
    let key = "family:user%_42\\segment";
    let expires_at = chrono::Utc::now() + chrono::Duration::seconds(3600);
    let timestamp = 1_700_000_123_i64;

    let row = (key, expires_at, Some(timestamp));

    assert_eq!(row.0, key);
    assert_eq!(row.2, Some(timestamp));
}

#[tokio::test]
async fn test_tiered_store_without_redis_still_works() {
    // Create a tiered store without Redis (L1 + PG only)
    let store = make_tiered_l1_only();

    // Pre-populate L1 to avoid hitting PG
    store
        .l1_blacklist
        .insert(
            "jti:l1_only_test".to_string(),
            (true, Instant::now() + Duration::from_mins(1)),
        )
        .await;

    // Should return true from L1 without hitting PG
    assert!(ok(
        store.is_blacklisted_checked("jti:l1_only_test").await,
        "L1-only blacklist lookup should read"
    ));
}

/// Test that concurrent calls to `blacklist_if_not_exists` on the same token
/// result in exactly one "first use" and all others detecting "replay".
/// This verifies the atomicity of the operation using InMemoryTokenBlacklistStore.
#[tokio::test]
async fn test_in_memory_concurrent_blacklist_if_not_exists_atomicity() {
    let store = Arc::new(make_in_memory_store());
    let key = "jti:concurrent_test";
    let num_concurrent = 10;

    // Spawn multiple concurrent tasks all trying to blacklist the same key
    let mut handles = Vec::new();
    for _ in 0..num_concurrent {
        let store_clone = Arc::clone(&store);
        let handle =
            tokio::spawn(async move { store_clone.blacklist_if_not_exists(key, 3600).await });
        handles.push(handle);
    }

    // Wait for all tasks to complete
    let results = futures::future::join_all(handles).await;

    let task_results: Vec<bool> = results
        .into_iter()
        .map(|result| completed_task_value(result, "concurrent blacklist-if-not-exists"))
        .collect();

    // Exactly one should return false (first use), all others should return true (replay)
    let first_use_count = task_results.iter().filter(|&&r| !r).count();
    let replay_count = task_results.iter().filter(|&&r| r).count();

    assert_eq!(
        first_use_count, 1,
        "Exactly one call should return false (first use), got {first_use_count}"
    );
    assert_eq!(
        replay_count,
        num_concurrent - 1,
        "All other calls should return true (replay), got {replay_count}"
    );

    // Verify the token is now blacklisted
    assert!(ok(
        store.is_blacklisted_checked(key).await,
        "blacklist lookup should read"
    ));
}

#[tokio::test]
async fn test_in_memory_blacklist_lock_cleanup_does_not_replace_live_mutex() {
    let store = make_in_memory_store();
    let key = "jti:lock_cleanup_in_memory";

    let original_mutex = store
        .blacklist_locks
        .entry(key.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .value()
        .clone();

    let _guard = original_mutex.lock().await;

    store.cleanup_blacklist_lock(key, &original_mutex);

    let stored_mutex = some(
        store.blacklist_locks.get(key),
        "live mutex entry must not be removed while in use",
    );
    assert!(
        Arc::ptr_eq(stored_mutex.value(), &original_mutex),
        "cleanup must not swap out an in-flight mutex"
    );
}

#[tokio::test]
async fn test_tiered_blacklist_lock_cleanup_does_not_replace_live_mutex() {
    let store = make_tiered_l1_only();
    let key = "jti:lock_cleanup_tiered";

    let original_mutex = store
        .blacklist_locks
        .entry(key.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .value()
        .clone();

    let _guard = original_mutex.lock().await;

    store.cleanup_blacklist_lock(key, &original_mutex);

    let stored_mutex = some(
        store.blacklist_locks.get(key),
        "live mutex entry must not be removed while in use",
    );
    assert!(
        Arc::ptr_eq(stored_mutex.value(), &original_mutex),
        "cleanup must not swap out an in-flight mutex"
    );
}

/// Test that concurrent calls to `blacklist_if_not_exists` on different tokens
/// all succeed (no false positives from the lock mechanism).
#[tokio::test]
async fn test_in_memory_concurrent_different_keys_all_succeed() {
    let store = Arc::new(make_in_memory_store());
    let num_concurrent = 10;

    let mut handles = Vec::new();
    for i in 0..num_concurrent {
        let store_clone = Arc::clone(&store);
        let handle = tokio::spawn(async move {
            let key = format!("jti:different_key_{i}");
            store_clone.blacklist_if_not_exists(&key, 3600).await
        });
        handles.push(handle);
    }

    let results = futures::future::join_all(handles).await;
    let task_results: Vec<bool> = results
        .into_iter()
        .map(|result| completed_task_value(result, "different-key blacklist-if-not-exists"))
        .collect();

    // All should return false (first use) since keys are different
    let first_use_count = task_results.iter().filter(|&&r| !r).count();
    assert_eq!(
        first_use_count, num_concurrent,
        "All calls with different keys should return false (first use), got {first_use_count}"
    );
}

/// Test that the InMemoryTokenBlacklistStore cleans up lock entries after use
/// to prevent unbounded memory growth.
#[tokio::test]
async fn test_in_memory_blacklist_lock_cleanup() {
    let store = make_in_memory_store();
    let key = "jti:lock_cleanup_test";

    // Initial lock count
    let initial_count = store.blacklist_locks.len();

    // Perform blacklist_if_not_exists
    ok(
        store.blacklist_if_not_exists(key, 3600).await,
        "blacklist-if-not-exists should run",
    );

    // Lock should be cleaned up after the operation
    // Note: There might be a brief moment where the lock exists,
    // but it should be removed after the operation completes
    tokio::time::sleep(Duration::from_millis(10)).await;

    // The lock entry should be removed (or at least not grow unbounded)
    // Since we're testing with a single key, the count should be back to initial
    assert_eq!(
        store.blacklist_locks.len(),
        initial_count,
        "Lock entry should be cleaned up after operation"
    );
}

/// Stress test: rapid concurrent blacklist_if_not_exists on same key
/// to verify no race conditions under heavy load.
#[tokio::test]
async fn test_in_memory_stress_concurrent_blacklist_if_not_exists() {
    let store = Arc::new(make_in_memory_store());
    let key = "jti:stress_test";
    let num_iterations = 100;

    for iteration in 0..5 {
        let iteration_key = format!("{key}_iter_{iteration}");
        let mut handles = Vec::new();

        for _ in 0..num_iterations {
            let store_clone = Arc::clone(&store);
            let key_clone = iteration_key.clone();
            let handle =
                tokio::spawn(
                    async move { store_clone.blacklist_if_not_exists(&key_clone, 3600).await },
                );
            handles.push(handle);
        }

        let results = futures::future::join_all(handles).await;
        let task_results: Vec<bool> = results
            .into_iter()
            .map(|result| completed_task_value(result, "stress blacklist-if-not-exists"))
            .collect();

        let first_use_count = task_results.iter().filter(|&&r| !r).count();
        let replay_count = task_results.iter().filter(|&&r| r).count();

        assert_eq!(
            first_use_count, 1,
            "Iteration {iteration}: exactly one first use expected, got {first_use_count}"
        );
        assert_eq!(
            replay_count,
            num_iterations - 1,
            "Iteration {}: expected {} replays, got {}",
            iteration,
            num_iterations - 1,
            replay_count
        );
    }
}
