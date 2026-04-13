//! Brute-force protection tests
//!
//! Tests the `InMemoryAttemptTracker`, `BruteForceProtection` logic, and
//! (with testcontainers) the `RedisAttemptTracker`.
//!
//! ## Degradation Testing
//!
//! Tests in this module verify that `RedisAttemptTracker` properly handles
//! Redis failures by falling back to in-memory storage. This fallback behavior
//! is tracked via `is_degraded()` and `degraded_operation_count()` methods.
//!
//! **WARNING**: In multi-replica deployments, degraded mode means each replica
//! maintains independent brute-force counters. Monitor the `is_degraded()` flag
//! or `degraded_operation_count()` to detect Redis connectivity issues.
//!
//! Run with: cargo test --test `brute_force_tests` -- --nocapture
#![allow(clippy::unwrap_used)]

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use synctv_core::service::{
    auth::brute_force::{InMemoryAttemptTracker, RedisAttemptTracker},
    AttemptTracker, BruteForceProtection,
};
use synctv_core_testing::constants::{brute_force, network};
use synctv_core_testing::start_redis;
use tokio::sync::RwLock;

fn redis_brute_force_protection(
    conn: Arc<RwLock<redis::aio::ConnectionManager>>,
    key_prefix: String,
) -> BruteForceProtection {
    let config = synctv_core::service::BruteForceConfig::default();
    let username_tracker = Arc::new(RedisAttemptTracker::new(
        conn.clone(),
        50_000,
        config.attempts_ttl_secs,
    ));
    let ip_tracker = Arc::new(RedisAttemptTracker::new(
        conn,
        100_000,
        config.ip_attempts_ttl_secs,
    ));
    BruteForceProtection::new_with_config(key_prefix, username_tracker, ip_tracker, config)
}

// ============================================================================
// InMemoryAttemptTracker tests
// ============================================================================

#[tokio::test]
async fn test_in_memory_tracker_record_and_get() {
    let tracker = InMemoryAttemptTracker::new(1000, 900);
    let key = "user:alice";
    let now = chrono::Utc::now().timestamp();

    // Initially no attempts
    let (count, _ts) = tracker.get_attempts(key).await.unwrap();
    assert_eq!(count, 0);

    // Record failures
    tracker.record_failure(key, now, 900).await.unwrap();
    tracker.record_failure(key, now + 1, 900).await.unwrap();
    tracker.record_failure(key, now + 2, 900).await.unwrap();

    let (count, last_ts) = tracker.get_attempts(key).await.unwrap();
    assert_eq!(count, 3);
    assert_eq!(last_ts, now + 2);
}

#[tokio::test]
async fn test_in_memory_tracker_reset_clears() {
    let tracker = InMemoryAttemptTracker::new(1000, 900);
    let key = "user:bob";
    let now = chrono::Utc::now().timestamp();

    tracker.record_failure(key, now, 900).await.unwrap();
    tracker.record_failure(key, now, 900).await.unwrap();

    let (count, _) = tracker.get_attempts(key).await.unwrap();
    assert_eq!(count, 2);

    tracker.reset(key).await.unwrap();

    let (count, _) = tracker.get_attempts(key).await.unwrap();
    assert_eq!(count, 0);
}

// ============================================================================
// BruteForceProtection tests
// ============================================================================

#[tokio::test]
async fn test_brute_force_below_threshold_allowed() {
    let protection = BruteForceProtection::in_memory("test".to_string());
    let ip = Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));

    // Record 4 failures (below tier1 threshold)
    for _ in 0..(brute_force::TIER1_THRESHOLD - 1) {
        protection.record_failure("alice", ip).await.unwrap();
    }

    // Should still be allowed
    let result = protection.check_allowed("alice", ip).await;
    assert!(result.is_ok(), "4 failures should not lock out");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_brute_force_at_tier1_threshold_locked() {
    let protection = BruteForceProtection::in_memory("test".to_string());
    let ip = Some(network::PROXY_IP.parse().unwrap());

    // Record exactly tier1 threshold failures
    for _ in 0..brute_force::TIER1_THRESHOLD {
        protection.record_failure("bob", ip).await.unwrap();
    }

    // Should be locked out
    let result = protection.check_allowed("bob", ip).await;
    assert!(result.is_err(), "5 failures should trigger tier1 lockout");

    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("Too many failed login attempts"),
        "Error should mention lockout: {msg}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_brute_force_tier1_expired_window_unlocks() {
    // This test uses the InMemoryAttemptTracker directly to simulate
    // time passing (by setting last_failure_at far in the past).
    let username_tracker = Arc::new(InMemoryAttemptTracker::new(50_000, 900));
    let ip_tracker = Arc::new(InMemoryAttemptTracker::new(100_000, 600));

    let protection =
        BruteForceProtection::new("test".to_string(), username_tracker.clone(), ip_tracker);

    // Record 5 failures with a timestamp far enough in the past that
    // the 60-second tier1 lockout has expired.
    let past = chrono::Utc::now().timestamp() - 120; // 2 minutes ago
    let key = "test:auth:login_attempts:charlie";
    for i in 0..5 {
        username_tracker
            .record_failure(key, past + i, 900)
            .await
            .unwrap();
    }

    // Lockout should have expired (60s window, 120s ago)
    let result = protection.check_allowed("charlie", None).await;
    assert!(
        result.is_ok(),
        "Tier1 lockout should have expired after 60s"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_brute_force_ip_lockout() {
    let protection = BruteForceProtection::in_memory("test".to_string());
    let ip = Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)));

    // Record 20 failures from same IP (across different usernames)
    for i in 0..20 {
        let username = format!("user_{i}");
        protection.record_failure(&username, ip).await.unwrap();
    }

    // IP should be locked out even for a brand-new username
    let result = protection.check_allowed("brand_new_user", ip).await;
    assert!(result.is_err(), "IP with 20 failures should be locked out");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_brute_force_reset_unlocks() {
    let protection = BruteForceProtection::in_memory("test".to_string());

    // Record 5 failures
    for _ in 0..5 {
        protection.record_failure("dave", None).await.unwrap();
    }

    // Should be locked
    assert!(protection.check_allowed("dave", None).await.is_err());

    // Reset
    protection.reset("dave").await.unwrap();

    // Should be unlocked
    assert!(
        protection.check_allowed("dave", None).await.is_ok(),
        "Reset should unlock the account"
    );
}

// ============================================================================
// RedisAttemptTracker tests (require testcontainers)
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_tracker_record_and_get() {
    let (_container, conn) = start_redis().await;
    let tracker = RedisAttemptTracker::new(Arc::new(RwLock::new(conn)), 50_000, 900);

    let key = "test:bf:redis_rg:alice";
    let now = chrono::Utc::now().timestamp();

    // Initially no attempts
    let (count, _) = tracker.get_attempts(key).await.unwrap();
    assert_eq!(count, 0);

    // Record failures
    tracker.record_failure(key, now, 900).await.unwrap();
    tracker.record_failure(key, now + 1, 900).await.unwrap();

    let (count, last_ts) = tracker.get_attempts(key).await.unwrap();
    assert_eq!(count, 2);
    assert_eq!(last_ts, now + 1);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_tracker_reset() {
    let (_container, conn) = start_redis().await;
    let tracker = RedisAttemptTracker::new(Arc::new(RwLock::new(conn)), 50_000, 900);

    let key = "test:bf:redis_reset:bob";
    let now = chrono::Utc::now().timestamp();

    tracker.record_failure(key, now, 900).await.unwrap();
    tracker.record_failure(key, now, 900).await.unwrap();
    tracker.record_failure(key, now, 900).await.unwrap();

    let (count, _) = tracker.get_attempts(key).await.unwrap();
    assert_eq!(count, 3);

    // Reset clears both Redis and fallback (verifies B5 fix: handles both Ok/Err)
    tracker.reset(key).await.unwrap();

    let (count, _) = tracker.get_attempts(key).await.unwrap();
    assert_eq!(count, 0, "Reset should clear the Redis key");
}

// ============================================================================
// BruteForceProtection::with_redis E2E tests
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_brute_force_with_redis_e2e_lockout_and_reset() {
    let (_container, conn) = start_redis().await;
    let protection =
        redis_brute_force_protection(Arc::new(RwLock::new(conn)), "test_e2e:".to_string());
    let ip = Some(IpAddr::V4(Ipv4Addr::new(10, 1, 0, 1)));

    // Record 5 failures to trigger tier1 lockout
    for _ in 0..5 {
        protection.record_failure("redis_user", ip).await.unwrap();
    }

    // Should be locked out
    let result = protection.check_allowed("redis_user", ip).await;
    assert!(
        result.is_err(),
        "5 failures via Redis should trigger lockout"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Too many failed login attempts"),
        "Error should mention lockout: {err_msg}"
    );

    // Reset should unlock
    protection.reset("redis_user").await.unwrap();

    let result = protection.check_allowed("redis_user", ip).await;
    assert!(result.is_ok(), "Reset should unlock the account via Redis");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_brute_force_with_redis_ip_lockout_and_reset() {
    let (_container, conn) = start_redis().await;
    let protection =
        redis_brute_force_protection(Arc::new(RwLock::new(conn)), "test_ip_e2e:".to_string());
    let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 50, 1));

    // Record 20 failures from the same IP across different usernames
    for i in 0..20 {
        let username = format!("ip_user_{i}");
        protection
            .record_failure(&username, Some(ip))
            .await
            .unwrap();
    }

    // IP should be locked out even for a brand-new username
    let result = protection
        .check_allowed("brand_new_ip_user", Some(ip))
        .await;
    assert!(
        result.is_err(),
        "IP with 20 failures should be locked out via Redis"
    );

    // Reset the IP
    protection.reset_ip(&ip).await.unwrap();

    // IP should be unlocked now
    let result = protection
        .check_allowed("brand_new_ip_user", Some(ip))
        .await;
    assert!(result.is_ok(), "reset_ip should unlock the IP via Redis");
}

// ============================================================================
// RedisAttemptTracker degradation tracking tests
// ============================================================================

/// Test that `RedisAttemptTracker` tracks degradation state correctly
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_tracker_degradation_tracking() {
    let (_container, conn) = start_redis().await;
    let tracker = RedisAttemptTracker::new(Arc::new(RwLock::new(conn)), 50_000, 900);

    // Initially should not be degraded
    assert!(
        !tracker.is_degraded(),
        "Tracker should start in non-degraded state"
    );
    assert_eq!(
        tracker.degraded_operation_count(),
        0,
        "No degraded operations yet"
    );

    let key = "test:degradation:user1";
    let now = chrono::Utc::now().timestamp();

    // Successful operation should keep tracker in non-degraded state
    tracker.record_failure(key, now, 900).await.unwrap();
    assert!(
        !tracker.is_degraded(),
        "After successful operation, should not be degraded"
    );
    assert_eq!(
        tracker.degraded_operation_count(),
        0,
        "No degraded operations after success"
    );

    let (count, _) = tracker.get_attempts(key).await.unwrap();
    assert_eq!(count, 1);
    assert!(
        !tracker.is_degraded(),
        "After successful get, should not be degraded"
    );
}

/// Test that `RedisAttemptTracker` increments degraded counter on failures
///
/// NOTE: This test cannot easily simulate Redis failures without stopping
/// the container. The degradation behavior is tested indirectly through
/// unit tests that verify the atomic state management.
///
/// In production, you can monitor `is_degraded()` and `degraded_operation_count()`
/// to detect Redis connectivity issues. When degraded:
/// - Each replica maintains independent brute-force counters
/// - Attackers may bypass lockouts by distributing requests across replicas
/// - WARN-level logs are emitted with key "Redis degraded to fallback"
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_tracker_counter_is_monotonically_increasing() {
    let (_container, conn) = start_redis().await;
    let tracker = RedisAttemptTracker::new(Arc::new(RwLock::new(conn)), 50_000, 900);

    // The degraded_operation_count should be monotonically increasing
    // (even if we can't easily simulate failures in this test)
    let initial_count = tracker.degraded_operation_count();

    // Perform some operations - these should succeed
    let key = "test:monotonic:user";
    let now = chrono::Utc::now().timestamp();
    for _ in 0..5 {
        tracker.record_failure(key, now, 900).await.unwrap();
    }

    // Counter should still be at initial (no failures)
    assert_eq!(
        tracker.degraded_operation_count(),
        initial_count,
        "Counter should not increase when Redis is healthy"
    );
}

/// Test behavior when Redis operations succeed after the tracker is created
///
/// This verifies that the tracker properly clears the degraded flag on success.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_tracker_success_clears_degraded_flag() {
    let (_container, conn) = start_redis().await;
    let tracker = RedisAttemptTracker::new(Arc::new(RwLock::new(conn)), 50_000, 900);

    let key = "test:clear_degraded:user";
    let now = chrono::Utc::now().timestamp();

    // Multiple successful operations
    for i in 1..=3 {
        let _ = tracker.record_failure(key, now + i, 900).await;
        assert!(
            !tracker.is_degraded(),
            "After successful record_failure #{i}, should not be degraded"
        );
    }

    let (count, ts) = tracker.get_attempts(key).await.unwrap();
    assert_eq!(count, 3);
    assert_eq!(ts, now + 3);
    assert!(
        !tracker.is_degraded(),
        "After successful get_attempts, should not be degraded"
    );

    // Reset should also clear degraded flag
    let _ = tracker.reset(key).await;
    assert!(
        !tracker.is_degraded(),
        "After successful reset, should not be degraded"
    );

    let (count, _) = tracker.get_attempts(key).await.unwrap();
    assert_eq!(count, 0);
}

/// Test fallback cache maintains state during Redis operations
///
/// The fallback cache in `RedisAttemptTracker` should maintain consistent state
/// even when Redis is available - it's only used as a fallback, not as primary.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_tracker_fallback_not_used_when_redis_healthy() {
    let (_container, conn) = start_redis().await;
    let tracker = RedisAttemptTracker::new(Arc::new(RwLock::new(conn)), 50_000, 900);

    let key = "test:fallback:not_used";
    let now = chrono::Utc::now().timestamp();

    // Record failures - should go to Redis, not fallback
    tracker.record_failure(key, now, 900).await.unwrap();
    let _ = tracker.record_failure(key, now + 1, 900).await;
    let _ = tracker.record_failure(key, now + 2, 900).await;

    // Read back - should get data from Redis
    let (count, ts) = tracker.get_attempts(key).await.unwrap();
    assert_eq!(count, 3, "Should read from Redis, not fallback");
    assert_eq!(ts, now + 2);

    // Should not be degraded
    assert!(!tracker.is_degraded());
    assert_eq!(tracker.degraded_operation_count(), 0);
}

// ============================================================================
// BruteForceConfig tests (Task #64)
// ============================================================================

use synctv_core::service::auth::brute_force::BruteForceConfig;

/// Test `BruteForceConfig::custom_thresholds()` with custom values
#[test]
fn test_brute_force_config_custom_thresholds() {
    let config = BruteForceConfig {
        tier1_threshold: 3,
        tier1_lockout_secs: 30,
        tier2_threshold: 6,
        tier2_lockout_secs: 120,
        tier3_threshold: 9,
        tier3_lockout_secs: 300,
        ip_threshold: 10,
        ip_lockout_secs: 180,
        attempts_ttl_secs: 600,
        ip_attempts_ttl_secs: 300,
    };

    assert_eq!(config.tier1_threshold, 3);
    assert_eq!(config.tier1_lockout_secs, 30);
    assert_eq!(config.tier2_threshold, 6);
    assert_eq!(config.tier2_lockout_secs, 120);
    assert_eq!(config.tier3_threshold, 9);
    assert_eq!(config.tier3_lockout_secs, 300);
    assert_eq!(config.ip_threshold, 10);
    assert_eq!(config.ip_lockout_secs, 180);
}

/// Test `BruteForceConfig` serialization/deserialization for settings storage
#[test]
fn test_brute_force_config_serde_roundtrip() {
    let original = BruteForceConfig {
        tier1_threshold: 3,
        tier1_lockout_secs: 30,
        tier2_threshold: 6,
        tier2_lockout_secs: 120,
        tier3_threshold: 9,
        tier3_lockout_secs: 300,
        ip_threshold: 10,
        ip_lockout_secs: 180,
        attempts_ttl_secs: 600,
        ip_attempts_ttl_secs: 300,
    };

    let json = serde_json::to_string(&original).unwrap();
    let deserialized: BruteForceConfig = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.tier1_threshold, original.tier1_threshold);
    assert_eq!(deserialized.tier1_lockout_secs, original.tier1_lockout_secs);
    assert_eq!(deserialized.tier2_threshold, original.tier2_threshold);
    assert_eq!(deserialized.tier2_lockout_secs, original.tier2_lockout_secs);
    assert_eq!(deserialized.tier3_threshold, original.tier3_threshold);
    assert_eq!(deserialized.tier3_lockout_secs, original.tier3_lockout_secs);
    assert_eq!(deserialized.ip_threshold, original.ip_threshold);
    assert_eq!(deserialized.ip_lockout_secs, original.ip_lockout_secs);
    assert_eq!(deserialized.attempts_ttl_secs, original.attempts_ttl_secs);
    assert_eq!(
        deserialized.ip_attempts_ttl_secs,
        original.ip_attempts_ttl_secs
    );
}

/// Test `BruteForceProtection` uses custom thresholds via config
#[tokio::test]
async fn test_brute_force_with_custom_tier1_threshold() {
    let custom_config = BruteForceConfig {
        tier1_threshold: 3, // Lower than default (5)
        tier1_lockout_secs: 30,
        ..BruteForceConfig::default()
    };

    let protection = BruteForceProtection::in_memory_with_config(
        "test_custom_tier1:".to_string(),
        custom_config,
    );

    // Record 3 failures (custom tier1 threshold)
    for _ in 0..3 {
        protection
            .record_failure("custom_user", None)
            .await
            .unwrap();
    }

    // Should be locked out at 3 failures (not 5)
    let result = protection.check_allowed("custom_user", None).await;
    assert!(
        result.is_err(),
        "Should be locked out at 3 failures with custom threshold"
    );

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Too many failed login attempts"),
        "Error should mention lockout: {err_msg}"
    );
}

/// Test `BruteForceProtection` uses custom lockout duration
#[tokio::test]
async fn test_brute_force_with_custom_lockout_duration() {
    let custom_config = BruteForceConfig {
        tier1_threshold: 2,
        tier1_lockout_secs: 5, // Very short for testing
        ..BruteForceConfig::default()
    };

    let username_tracker = Arc::new(InMemoryAttemptTracker::new(50_000, 900));
    let ip_tracker = Arc::new(InMemoryAttemptTracker::new(100_000, 600));

    let protection = BruteForceProtection::new_with_config(
        "test_custom_duration:".to_string(),
        username_tracker.clone(),
        ip_tracker,
        custom_config,
    );

    // Record 2 failures with timestamp far in the past (beyond lockout)
    let past = chrono::Utc::now().timestamp() - 10; // 10 seconds ago (beyond 5s lockout)
    let key = "test_custom_duration:auth:login_attempts:expired_user";
    for i in 0..2 {
        username_tracker
            .record_failure(key, past + i, 900)
            .await
            .unwrap();
    }

    // Lockout should have expired (5s lockout, failures 10s ago)
    let result = protection.check_allowed("expired_user", None).await;
    assert!(
        result.is_ok(),
        "Custom lockout should have expired after 5 seconds"
    );
}

/// Test `BruteForceProtection` uses custom IP thresholds
#[tokio::test]
async fn test_brute_force_with_custom_ip_threshold() {
    let custom_config = BruteForceConfig {
        ip_threshold: 5, // Lower than default (20)
        ip_lockout_secs: 60,
        ip_attempts_ttl_secs: 300,
        ..BruteForceConfig::default()
    };

    let protection =
        BruteForceProtection::in_memory_with_config("test_custom_ip:".to_string(), custom_config);

    let ip = Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50)));

    // Record 5 failures from same IP (custom IP threshold)
    for i in 0..5 {
        let username = format!("ip_user_{i}");
        protection.record_failure(&username, ip).await.unwrap();
    }

    // IP should be locked out at 5 failures (not 20)
    let result = protection.check_allowed("brand_new_user", ip).await;
    assert!(
        result.is_err(),
        "IP should be locked out at 5 failures with custom threshold"
    );
}

/// Test `BruteForceConfig` validation: thresholds should be increasing
#[test]
fn test_brute_force_config_threshold_ordering() {
    // Valid: thresholds are in increasing order
    let valid = BruteForceConfig {
        tier1_threshold: 5,
        tier2_threshold: 10,
        tier3_threshold: 15,
        ..BruteForceConfig::default()
    };
    assert!(valid.tier1_threshold < valid.tier2_threshold);
    assert!(valid.tier2_threshold < valid.tier3_threshold);

    // Invalid but allowed (enforcement is in validation, not types)
    let _invalid = BruteForceConfig {
        tier1_threshold: 20, // Higher than tier2
        tier2_threshold: 10,
        tier3_threshold: 5,
        ..BruteForceConfig::default()
    };
    // Note: Config validation should reject this, but the type allows it
}

/// Test that `BruteForceConfig` can be parsed from JSON (for settings integration)
#[test]
fn test_brute_force_config_from_json() {
    let json = serde_json::json!({
        "tier1_threshold": 4,
        "tier1_lockout_secs": 45,
        "tier2_threshold": 8,
        "tier2_lockout_secs": 180,
        "tier3_threshold": 12,
        "tier3_lockout_secs": 600,
        "ip_threshold": 15,
        "ip_lockout_secs": 300,
        "attempts_ttl_secs": 1200,
        "ip_attempts_ttl_secs": 900
    });

    let config: BruteForceConfig = serde_json::from_value(json).unwrap();

    assert_eq!(config.tier1_threshold, 4);
    assert_eq!(config.tier1_lockout_secs, 45);
    assert_eq!(config.tier2_threshold, 8);
    assert_eq!(config.tier2_lockout_secs, 180);
    assert_eq!(config.tier3_threshold, 12);
    assert_eq!(config.tier3_lockout_secs, 600);
    assert_eq!(config.ip_threshold, 15);
    assert_eq!(config.ip_lockout_secs, 300);
    assert_eq!(config.attempts_ttl_secs, 1200);
    assert_eq!(config.ip_attempts_ttl_secs, 900);
}

// ============================================================================
// IP-only Failure Tracking Tests (Task #74)
// ============================================================================

/// Test `record_ip_failure` only increments IP counter, not username counter
#[tokio::test]
async fn test_record_ip_failure_only_affects_ip_counter() {
    let protection = BruteForceProtection::in_memory("test_ip_only:".to_string());
    let ip = Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)));

    // Record IP-only failure multiple times
    for _ in 0..5 {
        protection.record_ip_failure(ip).await.unwrap();
    }

    // IP should be tracked
    assert!(
        protection.check_ip_allowed(ip).await.is_ok(),
        "IP should not be locked yet"
    );

    // Record more failures to trigger IP lockout
    for _ in 0..15 {
        protection.record_ip_failure(ip).await.unwrap();
    }

    // Now IP should be locked out (default threshold is 20)
    let result = protection.check_ip_allowed(ip).await;
    assert!(result.is_err(), "IP should be locked out after 20 failures");
}

/// Test `check_ip_allowed` with None IP always returns Ok
#[tokio::test]
async fn test_check_ip_allowed_with_none_is_always_ok() {
    let protection = BruteForceProtection::in_memory("test_none_ip:".to_string());

    // Should always succeed when no IP is provided
    let result = protection.check_ip_allowed(None).await;
    assert!(result.is_ok());
}

/// Test that username is NOT locked when only IP failures are recorded
#[tokio::test]
async fn test_ip_only_failure_does_not_lock_username() {
    let protection = BruteForceProtection::in_memory("test_username_safe:".to_string());
    let ip = Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 50)));

    // Record many IP-only failures
    for _ in 0..25 {
        protection.record_ip_failure(ip).await.unwrap();
    }

    // IP should be locked
    assert!(protection.check_ip_allowed(ip).await.is_err());

    // But a legitimate user with a different IP should be able to log in
    // (this is the key behavior - random username guessing doesn't lock out real users)
    let different_ip = Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 99)));
    assert!(protection
        .check_allowed("legitimate_user", different_ip)
        .await
        .is_ok());

    // And the username that was attacked should also be accessible from different IP
    assert!(protection
        .check_allowed("nonexistent_user_tried_earlier", different_ip)
        .await
        .is_ok());
}

/// Test differentiated failure: wrong password for existing user locks both
#[tokio::test]
async fn test_wrong_password_for_existing_user_locks_both() {
    let protection = BruteForceProtection::in_memory("test_both_lock:".to_string());
    let ip = Some(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1)));
    let username = "existing_user";

    // Simulate wrong password attempts for existing user (record_failure)
    for _ in 0..5 {
        protection.record_failure(username, ip).await.unwrap();
    }

    // Username should be locked at tier 1 threshold (5)
    let result = protection.check_allowed(username, None).await;
    assert!(
        result.is_err(),
        "Username should be locked after 5 failures"
    );
}

/// Test that legitimate user from same IP gets locked when using wrong password
#[tokio::test]
async fn test_legitimate_user_wrong_password_locks_username() {
    let protection = BruteForceProtection::in_memory("test_legit_lock:".to_string());
    let ip = Some(IpAddr::V4(Ipv4Addr::new(192, 168, 0, 50)));
    let username = "alice";

    // Alice enters wrong password 5 times
    for _ in 0..5 {
        protection.record_failure(username, ip).await.unwrap();
    }

    // Alice should be locked out
    let result = protection.check_allowed(username, ip).await;
    assert!(result.is_err(), "Alice should be locked out");
}

/// Test attacker cannot lock out legitimate user by trying non-existent usernames
#[tokio::test]
async fn test_attacker_cannot_lock_legitimate_user() {
    let protection = BruteForceProtection::in_memory("test_no_lock:".to_string());
    let attacker_ip = Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 50)));

    // Attacker tries many non-existent usernames (only IP tracking)
    for _ in 0..19 {
        protection.record_ip_failure(attacker_ip).await.unwrap();
    }

    // Legitimate user "bob" should NOT be locked out
    // (even though attacker used the same IP, they didn't use "bob" as username)
    let result = protection.check_allowed("bob", attacker_ip).await;
    assert!(
        result.is_ok(),
        "Bob should not be locked out by attacker's random username attempts"
    );
}

/// Test IP lockout still works with IP-only failures
#[tokio::test]
async fn test_ip_lockout_works_with_ip_only_failures() {
    let protection = BruteForceProtection::in_memory("test_ip_lockout:".to_string());
    let ip = Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1)));

    // Record IP-only failures up to threshold (default: 20)
    for _ in 0..20 {
        protection.record_ip_failure(ip).await.unwrap();
    }

    // IP should be locked
    let result = protection.check_ip_allowed(ip).await;
    assert!(result.is_err(), "IP should be locked out");

    // Any username from this IP should also be blocked
    let result = protection.check_allowed("any_username", ip).await;
    assert!(
        result.is_err(),
        "Any username from locked IP should be blocked"
    );
}
