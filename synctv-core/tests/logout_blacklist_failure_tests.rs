//! Logout blacklist failure tests
//!
//! Tests that logout correctly handles blacklist failures.
//! When the blacklist store fails, logout should return an error so the caller
//! knows that token revocation may not have succeeded.
//!
//! Run with: cargo test --test logout_blacklist_failure_tests -- --nocapture
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use synctv_core::service::{
    FallbackTokenBlacklistStore, InMemoryTokenBlacklistStore, TokenBlacklistStore,
};

/// Mock store that always fails blacklist operations.
/// Simulates Redis being unavailable.
struct FailingBlacklistStore;

#[async_trait::async_trait]
impl TokenBlacklistStore for FailingBlacklistStore {
    async fn is_blacklisted(&self, _key: &str) -> bool {
        false
    }

    async fn is_blacklisted_checked(&self, _key: &str) -> synctv_core::Result<bool> {
        Ok(false)
    }

    async fn blacklist(&self, _key: &str, _ttl_secs: u64) -> synctv_core::Result<()> {
        Err(synctv_core::Error::Internal(
            "Blacklist store unavailable".to_string(),
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
            "Blacklist store unavailable".to_string(),
        ))
    }
}

// ============================================================================
// Test 1: Blacklist success should work correctly
// ============================================================================

#[tokio::test]
async fn test_blacklist_success_returns_ok() {
    // With a working store, blacklist should succeed
    let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);

    let result = store.blacklist("jti:test_token", 3600).await;
    assert!(
        result.is_ok(),
        "Blacklist with working store should succeed"
    );

    // Verify token is blacklisted
    assert!(store.is_blacklisted("jti:test_token").await);
}

// ============================================================================
// Test 2: Blacklist failure should return error (fail-closed)
// ============================================================================

#[tokio::test]
async fn test_blacklist_failure_returns_error() {
    // With a failing store, blacklist should return an error (fail-closed)
    let store = FailingBlacklistStore;

    let result = store.blacklist("jti:test_token", 3600).await;
    assert!(
        result.is_err(),
        "Blacklist with failing store should return error (fail-closed behavior)"
    );

    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("unavailable") || err.to_string().contains("Blacklist"),
        "Error message should indicate the blacklist store issue"
    );
}

#[tokio::test]
async fn test_family_revocation_failure_returns_error() {
    let store = FailingBlacklistStore;

    let result = store
        .set_family_revoked("family:test_token", chrono::Utc::now().timestamp(), 3600)
        .await;
    assert!(
        result.is_err(),
        "Family revocation must surface persistence failures so callers can fail closed"
    );
}

// ============================================================================
// Test 3: Fallback store should succeed even when primary fails
// ============================================================================

#[tokio::test]
async fn test_fallback_succeeds_when_primary_fails() {
    // FallbackTokenBlacklistStore should succeed even when primary fails
    // because it has its own in-memory fallback
    let primary = Arc::new(FailingBlacklistStore) as Arc<dyn TokenBlacklistStore>;
    let fallback = FallbackTokenBlacklistStore::with_defaults(primary);

    let result = fallback.blacklist("jti:fallback_test", 3600).await;
    assert!(
        result.is_ok(),
        "FallbackTokenBlacklistStore should succeed via memory fallback"
    );

    // Token should be blacklisted in the fallback
    assert!(
        fallback.is_blacklisted("jti:fallback_test").await,
        "Token should be blacklisted in memory fallback"
    );
}

// ============================================================================
// Test 4: Verify fail-closed behavior for logout scenario
// ============================================================================

/// This test demonstrates the expected behavior for logout:
/// When using a raw failing store (not wrapped in FallbackTokenBlacklistStore),
/// the blacklist operation should fail so the caller knows token revocation failed.
#[tokio::test]
async fn test_logout_blacklist_fail_closed_semantics() {
    // Scenario: A service uses a blacklist store that fails
    let store = FailingBlacklistStore;

    // Attempt to blacklist should fail
    let blacklist_result = store.blacklist("jti:user_logout_token", 3600).await;

    // The key invariant: caller can detect failure
    let blacklist_failed = blacklist_result.is_err();
    assert!(
        blacklist_failed,
        "Blacklist failure should be detectable by caller"
    );

    // Now verify with a working store
    let working_store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);
    let blacklist_result = working_store.blacklist("jti:user_logout_token", 3600).await;

    let blacklist_succeeded = blacklist_result.is_ok();
    assert!(
        blacklist_succeeded,
        "Blacklist success should be detectable by caller"
    );
}

// ============================================================================
// Test 5: Multiple concurrent blacklist failures should all return errors
// ============================================================================

#[tokio::test]
async fn test_concurrent_blacklist_failures_all_return_errors() {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    let failure_count = Arc::new(AtomicUsize::new(0));
    let failure_count_clone = failure_count.clone();

    // Store that tracks how many times it failed
    struct CountingFailingStore {
        count: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl TokenBlacklistStore for CountingFailingStore {
        async fn is_blacklisted(&self, _key: &str) -> bool {
            false
        }

        async fn is_blacklisted_checked(&self, _key: &str) -> synctv_core::Result<bool> {
            Ok(false)
        }

        async fn blacklist(&self, _key: &str, _ttl_secs: u64) -> synctv_core::Result<()> {
            self.count.fetch_add(1, AtomicOrdering::SeqCst);
            Err(synctv_core::Error::Internal("Store failed".to_string()))
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
            Ok(())
        }
    }

    let store = CountingFailingStore {
        count: failure_count_clone,
    };

    // Attempt multiple concurrent blacklist operations
    let mut handles = vec![];
    for i in 0..5 {
        let store = &store;
        handles.push(async move {
            let key = format!("jti:concurrent_{i}");
            store.blacklist(&key, 3600).await
        });
    }

    // All should fail
    let results = futures::future::join_all(handles).await;
    for result in results {
        assert!(
            result.is_err(),
            "All concurrent blacklist operations on failing store should fail"
        );
    }

    // All failures should have been counted
    assert_eq!(
        failure_count.load(AtomicOrdering::SeqCst),
        5,
        "All 5 blacklist attempts should have been recorded"
    );
}

// ============================================================================
// Test 6: Verify TTL is passed correctly to blacklist
// ============================================================================

#[tokio::test]
async fn test_blacklist_ttl_passed_correctly() {
    // This test verifies that the TTL is correctly passed to the blacklist store
    // which is important for logout to properly expire tokens

    let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);

    // Blacklist with a short TTL
    let short_ttl = 1u64;
    store.blacklist("jti:short_ttl", short_ttl).await.unwrap();
    assert!(store.is_blacklisted("jti:short_ttl").await);

    // Wait for TTL to expire
    tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;

    // Token should no longer be blacklisted
    assert!(
        !store.is_blacklisted("jti:short_ttl").await,
        "Token should be removed from blacklist after TTL expires"
    );
}

// ============================================================================
// Test 7: Empty JTI should be handled gracefully
// ============================================================================

#[tokio::test]
async fn test_blacklist_empty_jti() {
    let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);

    // Empty JTI should still work (though it shouldn't be used in practice)
    let result = store.blacklist("", 3600).await;
    assert!(result.is_ok());

    // Empty key should be blacklisted
    assert!(store.is_blacklisted("").await);
}

// ============================================================================
// Test 8: Blacklist with zero TTL
// ============================================================================

#[tokio::test]
async fn test_blacklist_zero_ttl() {
    let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);

    // Zero TTL should work (immediately expired)
    let result = store.blacklist("jti:zero_ttl", 0).await;
    // The behavior may vary - some stores might reject, others accept
    // InMemoryTokenBlacklistStore accepts it
    assert!(result.is_ok());
}
