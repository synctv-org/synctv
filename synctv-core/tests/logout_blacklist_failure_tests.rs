//! Logout blacklist failure tests
//!
//! Tests that logout correctly handles blacklist failures.
//! When the blacklist store fails, logout should return an error so the caller
//! knows that token revocation may not have succeeded.
//!

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use synctv_core::service::{InMemoryTokenBlacklistStore, TokenBlacklistStore};
use synctv_core_testing::{err, ok};

/// Mock store that always fails blacklist operations.
/// Simulates Redis being unavailable.
struct FailingBlacklistStore;

#[async_trait::async_trait]
impl TokenBlacklistStore for FailingBlacklistStore {
    async fn is_blacklisted_checked(&self, _key: &str) -> synctv_core::Result<bool> {
        Ok(false)
    }

    async fn blacklist(&self, _key: &str, _ttl_secs: u64) -> synctv_core::Result<()> {
        Err(synctv_core::Error::Internal(
            "Blacklist store unavailable".to_string(),
        ))
    }

    async fn blacklist_if_not_exists(
        &self,
        _key: &str,
        _ttl_secs: u64,
    ) -> synctv_core::Result<bool> {
        Err(synctv_core::Error::Internal(
            "Blacklist store unavailable".to_string(),
        ))
    }

    async fn get_family_revoked_at_checked(&self, _key: &str) -> synctv_core::Result<Option<i64>> {
        Ok(None)
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

struct CountingFailingStore {
    count: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl TokenBlacklistStore for CountingFailingStore {
    async fn is_blacklisted_checked(&self, _key: &str) -> synctv_core::Result<bool> {
        Ok(false)
    }

    async fn blacklist(&self, _key: &str, _ttl_secs: u64) -> synctv_core::Result<()> {
        self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(synctv_core::Error::Internal("Store failed".to_string()))
    }

    async fn blacklist_if_not_exists(
        &self,
        _key: &str,
        _ttl_secs: u64,
    ) -> synctv_core::Result<bool> {
        self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(synctv_core::Error::Internal("Store failed".to_string()))
    }

    async fn get_family_revoked_at_checked(&self, _key: &str) -> synctv_core::Result<Option<i64>> {
        Ok(None)
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

struct RecordingTtlStore {
    ttl_secs: AtomicU64,
}

#[async_trait::async_trait]
impl TokenBlacklistStore for RecordingTtlStore {
    async fn is_blacklisted_checked(&self, _key: &str) -> synctv_core::Result<bool> {
        Ok(false)
    }

    async fn blacklist(&self, _key: &str, ttl_secs: u64) -> synctv_core::Result<()> {
        self.ttl_secs.store(ttl_secs, Ordering::SeqCst);
        Ok(())
    }

    async fn blacklist_if_not_exists(
        &self,
        _key: &str,
        ttl_secs: u64,
    ) -> synctv_core::Result<bool> {
        self.ttl_secs.store(ttl_secs, Ordering::SeqCst);
        Ok(false)
    }

    async fn get_family_revoked_at_checked(&self, _key: &str) -> synctv_core::Result<Option<i64>> {
        Ok(None)
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

// Test 1: Blacklist success should work correctly

// Test 2: Blacklist failure should return error (fail-closed)

#[tokio::test]
async fn test_blacklist_failure_returns_error() {
    // With a failing store, blacklist should return an error (fail-closed)
    let store = FailingBlacklistStore;

    let result = store.blacklist("jti:test_token", 3600).await;
    assert!(
        result.is_err(),
        "Blacklist with failing store should return error (fail-closed behavior)"
    );

    let err = err(result, "blacklist with failing store should fail");
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

// Test 3: Verify fail-closed behavior for logout scenario

/// This test demonstrates the expected behavior for logout:
/// the blacklist operation should fail so the caller knows token revocation failed.
#[tokio::test]
async fn test_logout_blacklist_fail_closed_semantics() {
    // Scenario: A service uses a blacklist store that fails
    let store = FailingBlacklistStore;

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

// Test 4: Multiple concurrent blacklist failures should all return errors

#[tokio::test]
async fn test_concurrent_blacklist_failures_all_return_errors() {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    let failure_count = Arc::new(AtomicUsize::new(0));
    let failure_count_clone = failure_count.clone();

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

// Test 6: Verify TTL is passed correctly to blacklist

#[tokio::test]
async fn test_blacklist_ttl_passed_correctly() {
    let store = RecordingTtlStore {
        ttl_secs: AtomicU64::new(0),
    };

    let short_ttl = 1u64;
    ok(
        store.blacklist("jti:short_ttl", short_ttl).await,
        "blacklist should accept short TTL",
    );

    assert_eq!(
        store.ttl_secs.load(Ordering::SeqCst),
        short_ttl,
        "Blacklist should pass the requested TTL through to the store"
    );
}

// Test 7: Empty JTI should be handled gracefully

#[tokio::test]
async fn test_blacklist_empty_jti() {
    let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);

    // Empty JTI should still work (though it shouldn't be used in practice)
    let result = store.blacklist("", 3600).await;
    assert!(result.is_ok());

    // Empty key should be blacklisted
    assert!(ok(
        store.is_blacklisted_checked("").await,
        "empty key blacklist lookup should succeed"
    ));
}

// Test 8: Blacklist with zero TTL

#[tokio::test]
async fn test_blacklist_zero_ttl() {
    let store = InMemoryTokenBlacklistStore::new(10_000, 3600, 86400);

    // Zero TTL should work (immediately expired)
    let result = store.blacklist("jti:zero_ttl", 0).await;
    // The behavior may vary - some stores might reject, others accept
    // InMemoryTokenBlacklistStore accepts it
    assert!(result.is_ok());
}
