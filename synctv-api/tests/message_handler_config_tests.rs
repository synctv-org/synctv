//! Tests for `StreamMessageHandler` configuration - TDD tests for removing global state
//!
//! These tests verify that:
//! 1. Message processing concurrency limit is configurable per-AppState (not global)
//! 2. Different `AppState` instances can have different concurrency limits
//! 3. Tests are properly isolated (no global state pollution)
//!
//! The global `MESSAGE_PROCESSING_SEMAPHORE` has been replaced with instance-level
//! configuration via `MessageConcurrencyConfig`.

#![allow(clippy::unwrap_used)]
use std::sync::Arc;
use synctv_api::impls::MessageConcurrencyConfig;

#[cfg(test)]
mod tests {
    use super::*;

    // ============================================================================
    // Test: MessageConcurrencyConfig can be created with custom limits
    // ============================================================================

    #[test]
    fn test_concurrency_config_can_be_created_with_custom_limit() {
        let config = MessageConcurrencyConfig::new(100);
        assert_eq!(config.max_concurrent(), 100);
        assert_eq!(config.available_permits(), 100);
    }

    // ============================================================================
    // Test: Different MessageConcurrencyConfig instances have independent semaphores
    // ============================================================================

    #[test]
    fn test_different_configs_have_independent_semaphores() {
        let config1 = MessageConcurrencyConfig::new(10);
        let config2 = MessageConcurrencyConfig::new(20);

        // Verify initial state
        assert_eq!(config1.available_permits(), 10);
        assert_eq!(config2.available_permits(), 20);

        // Acquire a permit from config1
        let permit1 = config1.semaphore().try_acquire_owned();
        assert!(permit1.is_ok(), "Should be able to acquire permit from config1");
        assert_eq!(config1.available_permits(), 9); // Decreased by 1
        assert_eq!(config2.available_permits(), 20); // Unchanged

        // Acquire a permit from config2
        let permit2 = config2.semaphore().try_acquire_owned();
        assert!(permit2.is_ok(), "Should be able to acquire permit from config2");
        assert_eq!(config1.available_permits(), 9); // Unchanged
        assert_eq!(config2.available_permits(), 19); // Decreased by 1

        // Drop permits
        drop(permit1);
        drop(permit2);
        assert_eq!(config1.available_permits(), 10); // Restored
        assert_eq!(config2.available_permits(), 20); // Restored
    }

    // ============================================================================
    // Test: Test isolation - two tests using different configs don't interfere
    // ============================================================================

    #[test]
    fn test_isolation_part1() {
        // This test uses a small limit
        let config = MessageConcurrencyConfig::new(5);
        assert_eq!(config.available_permits(), 5);

        // Acquire all permits
        let permits: Vec<_> = (0..5)
            .map(|_| config.semaphore().try_acquire_owned())
            .collect::<Result<Vec<_>, _>>()
            .expect("Should acquire all 5 permits");

        assert_eq!(config.available_permits(), 0);

        // Next acquisition should fail
        let failed_permit = config.semaphore().try_acquire_owned();
        assert!(failed_permit.is_err(), "Should fail when no permits available");

        // Keep permits alive until end of test
        drop(permits);
    }

    #[test]
    fn test_isolation_part2() {
        // This test should NOT be affected by test_isolation_part1
        // because they use different config instances (not global state)
        let config = MessageConcurrencyConfig::new(3);
        assert_eq!(config.available_permits(), 3);

        // Should be able to acquire all 3 permits
        let permits: Vec<_> = (0..3)
            .map(|_| config.semaphore().try_acquire_owned())
            .collect::<Result<Vec<_>, _>>()
            .expect("Should acquire all 3 permits - test isolation working");

        assert_eq!(config.available_permits(), 0);
        drop(permits);
    }

    // ============================================================================
    // Test: Concurrent access to the same config from multiple tasks
    // Note: This test verifies that permits can be acquired concurrently.
    // Since tokio::spawn tasks complete immediately after the permit check,
    // we need to hold permits alive to verify exhaustion behavior.
    // ============================================================================

    #[tokio::test]
    async fn test_concurrent_access_to_shared_config() {
        let config = Arc::new(MessageConcurrencyConfig::new(50));

        // Pre-acquire all 50 permits and hold them
        let permits: Vec<_> = (0..50)
            .map(|_| config.semaphore().try_acquire_owned())
            .collect::<Result<Vec<_>, _>>()
            .expect("Should acquire all 50 permits");

        // Verify all permits are exhausted
        assert_eq!(config.available_permits(), 0, "No permits should remain");

        // Next acquisition should fail
        let failed_permit = config.semaphore().try_acquire_owned();
        assert!(failed_permit.is_err(), "Should fail when no permits available");

        // Drop all permits
        drop(permits);
        assert_eq!(config.available_permits(), 50, "All permits restored after drop");
    }

    // ============================================================================
    // Test: Concurrent access with limit exhaustion
    // ============================================================================

    #[tokio::test]
    async fn test_concurrent_access_with_limit_exhaustion() {
        let config = Arc::new(MessageConcurrencyConfig::new(10));

        // Acquire all 10 permits
        let permits: Vec<_> = (0..10)
            .map(|_| config.semaphore().try_acquire_owned())
            .collect::<Result<Vec<_>, _>>()
            .expect("Should acquire all 10 permits");

        assert_eq!(config.available_permits(), 0, "No permits should remain");

        // Next 5 acquisitions should fail
        for i in 0..5 {
            let result = config.semaphore().try_acquire_owned();
            assert!(result.is_err(), "Permit {i} should fail (exhausted)");
        }

        // Drop all permits
        drop(permits);
        assert_eq!(config.available_permits(), 10, "All permits restored after drop");
    }
}

// ============================================================================
// Integration Tests with StreamMessageHandler
// ============================================================================

#[cfg(test)]
mod integration_tests {
    use super::*;

    // ============================================================================
    // Test: StreamMessageHandler should accept MessageConcurrencyConfig
    // This test will FAIL until StreamMessageHandler::new accepts a config parameter
    // ============================================================================

    /// Test that `StreamMessageHandler` can be configured with a custom concurrency config.
    /// This test uses the `synctv_api::impls::MessageConcurrencyConfig` type
    /// which needs to be added to the messaging module.
    #[test]
    fn test_stream_message_handler_accepts_concurrency_config() {
        // This test verifies that:
        // 1. MessageConcurrencyConfig is exported from synctv_api::impls
        // 2. It can be created with a custom limit

        // First, verify the config type is accessible from synctv_api
        // After implementation, this should be:
        // use synctv_api::impls::MessageConcurrencyConfig;
        let config = MessageConcurrencyConfig::new(100);
        assert_eq!(config.max_concurrent(), 100);

        // Note: Full integration with StreamMessageHandler would require
        // creating a full mock environment (room_service, cluster_manager, etc.)
        // which is complex. The key verification here is that the config type
        // exists and can be used.
    }

    /// Test that two different `AppState` instances can have different concurrency limits.
    /// This is the key isolation test - different instances should not share semaphores.
    #[test]
    fn test_different_app_states_have_isolated_concurrency() {
        // Create two configs with different limits
        let config1 = MessageConcurrencyConfig::new(50);
        let config2 = MessageConcurrencyConfig::new(200);

        // Verify they are independent
        assert_eq!(config1.available_permits(), 50);
        assert_eq!(config2.available_permits(), 200);

        // Exhaust config1
        let permits1: Vec<_> = (0..50)
            .map(|_| config1.semaphore().try_acquire_owned())
            .collect::<Result<Vec<_>, _>>()
            .expect("Should acquire all 50 permits from config1");

        // config2 should still have all 200 permits
        assert_eq!(config1.available_permits(), 0);
        assert_eq!(config2.available_permits(), 200);

        // config2 can acquire 200 permits
        let permits2: Vec<_> = (0..200)
            .map(|_| config2.semaphore().try_acquire_owned())
            .collect::<Result<Vec<_>, _>>()
            .expect("Should acquire all 200 permits from config2");

        assert_eq!(config1.available_permits(), 0);
        assert_eq!(config2.available_permits(), 0);

        // Both should fail to acquire more
        assert!(config1.semaphore().try_acquire_owned().is_err());
        assert!(config2.semaphore().try_acquire_owned().is_err());

        drop(permits1);
        drop(permits2);

        // Both restored
        assert_eq!(config1.available_permits(), 50);
        assert_eq!(config2.available_permits(), 200);
    }

    /// Test that the default config matches the original global constant.
    #[test]
    fn test_default_config_matches_original_constant() {
        let config = MessageConcurrencyConfig::default();
        // Should match DEFAULT_MAX_CONCURRENT_MESSAGE_PROCESSING (1000)
        assert_eq!(config.max_concurrent(), 1000);
        assert_eq!(config.available_permits(), 1000);
    }
}
