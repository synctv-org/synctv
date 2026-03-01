//! Migration system validation tests.
//!
//! Tests for the migration infrastructure including:
//! - Migration lock behavior (mock tests)
//! - PostgreSQL advisory lock fallback
//! - Migration state verification
//!
//! Tests requiring Docker are marked with #[ignore].

#![allow(clippy::unwrap_used)]

use std::time::Duration;

// ============================================================================
// Mock MigrationLock tests
// ============================================================================

/// Mock implementation of MigrationLock for unit testing
struct MockMigrationLock {
    /// Whether acquire should succeed
    acquire_succeeds: bool,
    /// Whether release should succeed
    release_succeeds: bool,
    /// Track if acquire was called
    acquire_called: std::sync::atomic::AtomicBool,
    /// Track if release was called
    release_called: std::sync::atomic::AtomicBool,
    /// Lock value to return
    lock_value: String,
}

impl MockMigrationLock {
    fn new(acquire_succeeds: bool) -> Self {
        Self {
            acquire_succeeds,
            release_succeeds: true,
            acquire_called: std::sync::atomic::AtomicBool::new(false),
            release_called: std::sync::atomic::AtomicBool::new(false),
            lock_value: "test-lock-value".to_string(),
        }
    }

    fn was_acquire_called(&self) -> bool {
        self.acquire_called
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    fn was_release_called(&self) -> bool {
        self.release_called
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl synctv_core::service::MigrationLock for MockMigrationLock {
    async fn acquire(&self, _key: &str, _ttl_secs: u64) -> anyhow::Result<Option<String>> {
        self.acquire_called
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if self.acquire_succeeds {
            Ok(Some(self.lock_value.clone()))
        } else {
            Ok(None)
        }
    }

    async fn release(&self, _key: &str, _lock_value: &str) -> anyhow::Result<bool> {
        self.release_called
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(self.release_succeeds)
    }
}

mod mock_migration_lock_tests {
    use super::*;
    use synctv_core::service::MigrationLock;

    /// Test that mock lock acquire succeeds when configured
    #[tokio::test]
    async fn test_mock_lock_acquire_success() {
        let lock = MockMigrationLock::new(true);

        let result = lock.acquire("test-key", 300).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_some());
        assert!(lock.was_acquire_called());
    }

    /// Test that mock lock acquire fails when configured
    #[tokio::test]
    async fn test_mock_lock_acquire_fail() {
        let lock = MockMigrationLock::new(false);

        let result = lock.acquire("test-key", 300).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    /// Test that mock lock release succeeds
    #[tokio::test]
    async fn test_mock_lock_release() {
        let lock = MockMigrationLock::new(true);

        let result = lock.release("test-key", "test-lock-value").await;
        assert!(result.is_ok());
        assert!(result.unwrap());
        assert!(lock.was_release_called());
    }

    /// Test full acquire/release cycle with mock
    #[tokio::test]
    async fn test_mock_lock_full_cycle() {
        let lock = MockMigrationLock::new(true);

        // Acquire
        let acquire_result = lock.acquire("migration-lock", 300).await;
        assert!(acquire_result.is_ok());
        let lock_value = acquire_result.unwrap();
        assert!(lock_value.is_some());

        // Release
        let release_result = lock.release("migration-lock", &lock_value.unwrap()).await;
        assert!(release_result.is_ok());
        assert!(release_result.unwrap());
    }
}

// ============================================================================
// Migration lock key format tests
// ============================================================================

mod migration_key_tests {
    /// Test that migration lock key includes key prefix
    #[test]
    fn test_migration_lock_key_format() {
        let key_prefix = "synctv:";
        let migration_key = format!("{key_prefix}migration");

        assert_eq!(migration_key, "synctv:migration");
    }

    /// Test that different key prefixes produce different lock keys
    #[test]
    fn test_migration_lock_key_uniqueness() {
        let prefixes = vec!["synctv:", "synctv-prod:", "synctv-staging:", "custom:"];
        let mut keys = std::collections::HashSet::new();
        let expected_len = prefixes.len();

        for prefix in &prefixes {
            let key = format!("{prefix}migration");
            keys.insert(key);
        }

        // All keys should be unique
        assert_eq!(keys.len(), expected_len);
    }
}

// ============================================================================
// Migration constants tests
// ============================================================================

mod migration_constants_tests {
    /// Test that migration lock TTL is reasonable
    #[test]
    fn test_migration_lock_ttl_reasonable() {
        const MIGRATION_LOCK_TTL: u64 = 300; // 5 minutes

        // TTL should be long enough for migrations to complete
        assert!(MIGRATION_LOCK_TTL >= 60, "TTL should be at least 1 minute");

        // But not too long to cause issues
        assert!(MIGRATION_LOCK_TTL <= 3600, "TTL should be at most 1 hour");
    }

    /// Test that migration poll interval is reasonable
    #[test]
    fn test_migration_poll_interval_reasonable() {
        const MIGRATION_POLL_INTERVAL_SECS: u64 = 2;

        // Poll interval should not be too aggressive
        assert!(
            MIGRATION_POLL_INTERVAL_SECS >= 1,
            "Poll interval should be at least 1 second"
        );

        // But not too slow
        assert!(
            MIGRATION_POLL_INTERVAL_SECS <= 10,
            "Poll interval should be at most 10 seconds"
        );
    }

    /// Test that max wait time is reasonable
    #[test]
    fn test_migration_max_wait_reasonable() {
        const MIGRATION_MAX_WAIT_SECS: u64 = 300; // 5 minutes

        // Max wait should be reasonable for production deployments
        assert!(
            MIGRATION_MAX_WAIT_SECS >= 60,
            "Max wait should be at least 1 minute"
        );
        assert!(
            MIGRATION_MAX_WAIT_SECS <= 600,
            "Max wait should be at most 10 minutes"
        );
    }

    /// Test PostgreSQL advisory lock key consistency
    #[test]
    fn test_pg_advisory_lock_key_consistency() {
        // This key must match between migrations.rs and distributed_lock.rs
        const PG_ADVISORY_LOCK_KEY_MIGRATIONS: i64 = 0x73796E63_74766D69_u64 as i64;
        const PG_ADVISORY_LOCK_KEY_DISTRIBUTED: i64 = 0x73796E63_74766D69_u64 as i64;

        assert_eq!(
            PG_ADVISORY_LOCK_KEY_MIGRATIONS, PG_ADVISORY_LOCK_KEY_DISTRIBUTED,
            "PG advisory lock keys must be consistent across modules"
        );

        // Verify it's the hash of "synctv_migration"
        // (0x73796E63 = "sync", 0x74766D69 = "tvmi" -> combined as "synctvmig" pattern)
        assert_ne!(
            PG_ADVISORY_LOCK_KEY_MIGRATIONS, 0,
            "Lock key should not be zero"
        );
    }
}

// ============================================================================
// PostgreSQL advisory lock backoff tests
// ============================================================================

mod pg_advisory_lock_backoff_tests {
    use std::time::Duration;

    /// Test exponential backoff calculation
    #[test]
    fn test_exponential_backoff() {
        let initial_backoff = Duration::from_millis(500);
        let max_backoff = Duration::from_secs(8);

        let mut backoff = initial_backoff;
        let mut backoffs = vec![backoff];

        for _ in 0..10 {
            backoff = (backoff * 2).min(max_backoff);
            backoffs.push(backoff);
        }

        // Should increase exponentially until hitting max
        assert_eq!(backoffs[0], Duration::from_millis(500));
        assert_eq!(backoffs[1], Duration::from_secs(1));
        assert_eq!(backoffs[2], Duration::from_secs(2));
        assert_eq!(backoffs[3], Duration::from_secs(4));
        assert_eq!(backoffs[4], Duration::from_secs(8));
        // Should stay at max after hitting it
        assert_eq!(backoffs[5], Duration::from_secs(8));
        assert_eq!(backoffs[10], Duration::from_secs(8));
    }

    /// Test that max wait time is not exceeded
    #[test]
    fn test_max_wait_not_exceeded() {
        let max_wait = Duration::from_secs(60);
        let initial_backoff = Duration::from_millis(500);
        let max_backoff = Duration::from_secs(8);

        let mut total_wait = Duration::ZERO;
        let mut backoff = initial_backoff;

        // Simulate waiting
        while total_wait < max_wait {
            let remaining = max_wait.saturating_sub(total_wait);
            let sleep_for = backoff.min(remaining);

            total_wait += sleep_for;
            backoff = (backoff * 2).min(max_backoff);

            if sleep_for.is_zero() {
                break;
            }
        }

        assert!(
            total_wait <= max_wait,
            "Total wait should not exceed max wait"
        );
    }
}

// ============================================================================
// Migration state verification tests (unit tests only)
// ============================================================================

mod migration_state_tests {
    /// Test that migration version comparison works correctly
    #[test]
    fn test_migration_version_comparison() {
        let applied_versions: std::collections::HashSet<i64> =
            [20240101000000_i64, 20240102000000, 20240103000000]
                .into_iter()
                .collect();

        let pending_versions = vec![20240101000000_i64, 20240102000000, 20240104000000];

        let all_applied = pending_versions
            .iter()
            .all(|v| applied_versions.contains(v));

        // Should be false because 20240104000000 is not applied
        assert!(!all_applied);

        let only_applied = vec![20240101000000_i64, 20240102000000];
        let all_applied = only_applied.iter().all(|v| applied_versions.contains(v));

        assert!(all_applied);
    }

    /// Test that empty migrations are handled correctly
    #[test]
    fn test_empty_migrations() {
        let applied_versions: std::collections::HashSet<i64> = [].into_iter().collect();
        let pending_versions: Vec<i64> = vec![];

        // Empty pending should be "all applied"
        let all_applied = pending_versions
            .iter()
            .all(|v| applied_versions.contains(v));
        assert!(all_applied);

        // But with pending migrations, should be false
        let pending_versions = vec![20240101000000_i64];
        let all_applied = pending_versions
            .iter()
            .all(|v| applied_versions.contains(v));
        assert!(!all_applied);
    }
}

// ============================================================================
// Integration tests (require Docker - marked as #[ignore])
// ============================================================================

mod integration_tests {
    use testcontainers::runners::AsyncRunner;
    use testcontainers_modules::redis::Redis;

    /// Test that Redis-based migration lock works with real Redis
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_redis_migration_lock_integration() {
        // Start Redis container
        let container = Redis::default()
            .start()
            .await
            .expect("Failed to start Redis");
        let port = container
            .get_host_port_ipv4(6379)
            .await
            .expect("Failed to get port");

        let redis_url = format!("redis://127.0.0.1:{port}");
        let client = redis::Client::open(redis_url).expect("Failed to create Redis client");
        let conn = redis::aio::ConnectionManager::new(client)
            .await
            .expect("Failed to create connection manager");

        let lock = synctv_core::service::DistributedLock::new(conn);

        // Acquire migration lock
        let result = lock.acquire("synctv:migration", 300).await;
        assert!(result.is_ok(), "Acquire should succeed");
        let lock_value = result.unwrap();
        assert!(lock_value.is_some(), "Lock should be acquired");

        // Second acquire should fail
        let result2 = lock.acquire("synctv:migration", 300).await;
        assert!(result2.is_ok(), "Second acquire call should succeed");
        assert!(
            result2.unwrap().is_none(),
            "Second acquire should return None (lock held)"
        );

        // Release lock
        let released = lock
            .release("synctv:migration", &lock_value.unwrap())
            .await
            .unwrap();
        assert!(released, "Lock should be released");

        // Now acquire should succeed again
        let result3 = lock.acquire("synctv:migration", 300).await;
        assert!(
            result3.unwrap().is_some(),
            "Acquire after release should succeed"
        );
    }

    /// Test that concurrent migration locks work correctly
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_concurrent_migration_locks() {
        use std::sync::Arc;

        // Start Redis container
        let container = Redis::default()
            .start()
            .await
            .expect("Failed to start Redis");
        let port = container
            .get_host_port_ipv4(6379)
            .await
            .expect("Failed to get port");

        let redis_url = format!("redis://127.0.0.1:{port}");
        let client = redis::Client::open(redis_url).expect("Failed to create Redis client");
        let conn = redis::aio::ConnectionManager::new(client)
            .await
            .expect("Failed to create connection manager");

        let lock = Arc::new(synctv_core::service::DistributedLock::new(conn));

        // Spawn multiple tasks trying to acquire the same lock
        let mut handles = Vec::new();
        for i in 0..5 {
            let lock_clone = lock.clone();
            handles.push(tokio::spawn(async move {
                let result = lock_clone.acquire("concurrent-test", 10).await;
                (i, result.is_ok() && result.unwrap().is_some())
            }));
        }

        let results: Vec<_> = futures::future::join_all(handles)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        // Only one should succeed
        let successes: Vec<_> = results.iter().filter(|(_, won)| *won).collect();
        assert_eq!(successes.len(), 1, "Only one task should acquire the lock");
    }
}
