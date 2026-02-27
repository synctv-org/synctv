//! Brute-force protection tests for authentication module
//!
//! Tests for brute-force protection including:
//! - Login failure counting
//! - IP lockout
//! - User lockout
//! - Lockout expiration
//!
//! Run with: cargo test --test auth_brute_force_tests
//! With Docker: cargo test --test auth_brute_force_tests -- --ignored

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use synctv_core::service::{
    AttemptTracker, BruteForceProtection, InMemoryAttemptTracker,
};

// ============================================================================
// Login Failure Counting Tests
// ============================================================================

#[tokio::test]
async fn test_record_single_failure() {
    let tracker = InMemoryAttemptTracker::new(1000, 900);
    let key = "user:alice";
    let now = chrono::Utc::now().timestamp();

    // Initially no failures
    let (count, _ts) = tracker.get_attempts(key).await.unwrap();
    assert_eq!(count, 0);

    // Record one failure
    tracker.record_failure(key, now, 900).await.unwrap();

    let (count, last_ts) = tracker.get_attempts(key).await.unwrap();
    assert_eq!(count, 1);
    assert_eq!(last_ts, now);
}

#[tokio::test]
async fn test_record_multiple_failures() {
    let tracker = InMemoryAttemptTracker::new(1000, 900);
    let key = "user:bob";
    let now = chrono::Utc::now().timestamp();

    // Record multiple failures
    tracker.record_failure(key, now, 900).await.unwrap();
    tracker.record_failure(key, now + 1, 900).await.unwrap();
    tracker.record_failure(key, now + 2, 900).await.unwrap();

    let (count, last_ts) = tracker.get_attempts(key).await.unwrap();
    assert_eq!(count, 3);
    assert_eq!(last_ts, now + 2);
}

#[tokio::test]
async fn test_failure_count_increments_independently() {
    let tracker = InMemoryAttemptTracker::new(1000, 900);
    let now = chrono::Utc::now().timestamp();

    // Different users have independent counters
    tracker.record_failure("user:alice", now, 900).await.unwrap();
    tracker.record_failure("user:alice", now, 900).await.unwrap();

    tracker.record_failure("user:bob", now, 900).await.unwrap();
    tracker.record_failure("user:bob", now, 900).await.unwrap();
    tracker.record_failure("user:bob", now, 900).await.unwrap();

    let (alice_count, _) = tracker.get_attempts("user:alice").await.unwrap();
    let (bob_count, _) = tracker.get_attempts("user:bob").await.unwrap();

    assert_eq!(alice_count, 2);
    assert_eq!(bob_count, 3);
}

#[tokio::test]
async fn test_reset_clears_failure_count() {
    let tracker = InMemoryAttemptTracker::new(1000, 900);
    let key = "user:charlie";
    let now = chrono::Utc::now().timestamp();

    // Record failures
    tracker.record_failure(key, now, 900).await.unwrap();
    tracker.record_failure(key, now, 900).await.unwrap();

    let (count, _) = tracker.get_attempts(key).await.unwrap();
    assert_eq!(count, 2);

    // Reset
    tracker.reset(key).await.unwrap();

    let (count, _) = tracker.get_attempts(key).await.unwrap();
    assert_eq!(count, 0);
}

// ============================================================================
// User Lockout Tests
// ============================================================================

#[tokio::test]
async fn test_user_lockout_at_tier1_threshold() {
    let protection = BruteForceProtection::in_memory("test".to_string());
    let ip = Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));

    // Record exactly 5 failures (tier1 threshold)
    for _ in 0..5 {
        protection.record_failure("alice", ip).await.unwrap();
    }

    // Should be locked out
    let result = protection.check_allowed("alice", ip).await;
    assert!(result.is_err(), "5 failures should trigger tier1 lockout");

    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("Too many failed login attempts"),
        "Error should mention lockout: {msg}"
    );
}

#[tokio::test]
async fn test_user_lockout_below_threshold_not_locked() {
    let protection = BruteForceProtection::in_memory("test".to_string());
    let ip = Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)));

    // Record 4 failures (below tier1 threshold of 5)
    for _ in 0..4 {
        protection.record_failure("bob", ip).await.unwrap();
    }

    // Should still be allowed
    let result = protection.check_allowed("bob", ip).await;
    assert!(result.is_ok(), "4 failures should not lock out");
}

#[tokio::test]
async fn test_user_lockout_reset_unlocks() {
    let protection = BruteForceProtection::in_memory("test".to_string());

    // Record 5 failures to trigger lockout
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

#[tokio::test]
async fn test_user_lockout_different_ips_independent() {
    let protection = BruteForceProtection::in_memory("test".to_string());
    let ip1 = Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));
    let ip2 = Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)));

    // Record 3 failures from IP1 for user alice
    for _ in 0..3 {
        protection.record_failure("alice", ip1).await.unwrap();
    }

    // User should not be locked (only 3 failures)
    assert!(protection.check_allowed("alice", ip2).await.is_ok());

    // Record 2 more failures from IP2 (total 5)
    for _ in 0..2 {
        protection.record_failure("alice", ip2).await.unwrap();
    }

    // Now user should be locked regardless of IP
    assert!(protection.check_allowed("alice", ip1).await.is_err());
    assert!(protection.check_allowed("alice", ip2).await.is_err());
}

// ============================================================================
// IP Lockout Tests
// ============================================================================

#[tokio::test]
async fn test_ip_lockout_at_threshold() {
    let protection = BruteForceProtection::in_memory("test".to_string());
    let ip = Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)));

    // Record 20 failures from same IP (across different usernames)
    for i in 0..20 {
        let username = format!("user_{}", i);
        protection.record_failure(&username, ip).await.unwrap();
    }

    // IP should be locked out even for a brand-new username
    let result = protection.check_allowed("brand_new_user", ip).await;
    assert!(result.is_err(), "IP with 20 failures should be locked out");
}

#[tokio::test]
async fn test_ip_lockout_below_threshold_not_locked() {
    let protection = BruteForceProtection::in_memory("test".to_string());
    let ip = Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 101)));

    // Record 19 failures from same IP (below threshold of 20)
    for i in 0..19 {
        let username = format!("user_{}", i);
        protection.record_failure(&username, ip).await.unwrap();
    }

    // IP should NOT be locked
    let result = protection.check_allowed("brand_new_user", ip).await;
    assert!(result.is_ok(), "IP with 19 failures should not be locked");
}

#[tokio::test]
async fn test_ip_lockout_reset_unlocks() {
    let protection = BruteForceProtection::in_memory("test".to_string());
    let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 102));

    // Lock the IP
    for i in 0..20 {
        let username = format!("user_{}", i);
        protection.record_failure(&username, Some(ip)).await.unwrap();
    }

    assert!(protection.check_allowed("any_user", Some(ip)).await.is_err());

    // Reset IP
    protection.reset_ip(&ip).await.unwrap();

    // IP should be unlocked
    assert!(
        protection.check_allowed("any_user", Some(ip)).await.is_ok(),
        "reset_ip should unlock the IP"
    );
}

#[tokio::test]
async fn test_different_ips_independent() {
    let protection = BruteForceProtection::in_memory("test".to_string());
    let ip1 = Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10)));
    let ip2 = Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)));

    // Lock IP1
    for i in 0..20 {
        let username = format!("ip1_user_{}", i);
        protection.record_failure(&username, ip1).await.unwrap();
    }

    // IP1 should be locked
    assert!(protection.check_allowed("any_ip1_user", ip1).await.is_err());

    // IP2 should NOT be locked
    assert!(
        protection.check_allowed("any_ip2_user", ip2).await.is_ok(),
        "Different IP should not be affected"
    );
}

// ============================================================================
// Lockout Expiration Tests
// ============================================================================

#[tokio::test]
async fn test_tier1_lockout_expires_after_60_seconds() {
    // This test uses the InMemoryAttemptTracker directly to simulate
    // time passing (by setting last_failure_at far in the past).
    let username_tracker = Arc::new(InMemoryAttemptTracker::new(50_000, 900));
    let ip_tracker = Arc::new(InMemoryAttemptTracker::new(100_000, 600));

    let protection = BruteForceProtection::new(
        "test".to_string(),
        username_tracker.clone(),
        ip_tracker,
    );

    // Record 5 failures with a timestamp far enough in the past that
    // the 60-second tier1 lockout has expired.
    let past = chrono::Utc::now().timestamp() - 120; // 2 minutes ago
    let key = "test:auth:login_attempts:charlie";
    for i in 0..5 {
        username_tracker.record_failure(key, past + i, 900).await.unwrap();
    }

    // Lockout should have expired (60s window, 120s ago)
    let result = protection.check_allowed("charlie", None).await;
    assert!(result.is_ok(), "Tier1 lockout should have expired after 60s");
}

#[tokio::test]
async fn test_ip_lockout_expires_after_ttl() {
    let ip_tracker = Arc::new(InMemoryAttemptTracker::new(100_000, 600));
    let username_tracker = Arc::new(InMemoryAttemptTracker::new(50_000, 900));

    let protection = BruteForceProtection::new(
        "test".to_string(),
        username_tracker,
        ip_tracker.clone(),
    );

    let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 50, 1));

    // Record 20 IP failures with timestamp far in the past
    let past = chrono::Utc::now().timestamp() - 700; // Beyond 600s TTL
    let key = "test:auth:ip_login_attempts:192.168.50.1";
    for i in 0..20 {
        ip_tracker.record_failure(key, past + i, 600).await.unwrap();
    }

    // IP lockout should have expired
    let result = protection.check_allowed("any_user", Some(ip)).await;
    assert!(result.is_ok(), "IP lockout should have expired after TTL");
}

#[tokio::test]
async fn test_failure_count_decreases_with_ttl() {
    let tracker = InMemoryAttemptTracker::new(1000, 2); // 2 second TTL
    let key = "user:tiffany";
    let now = chrono::Utc::now().timestamp();

    // Record 3 failures
    tracker.record_failure(key, now, 2).await.unwrap();
    tracker.record_failure(key, now, 2).await.unwrap();
    tracker.record_failure(key, now, 2).await.unwrap();

    let (count, _) = tracker.get_attempts(key).await.unwrap();
    assert_eq!(count, 3);

    // Wait for TTL to expire
    tokio::time::sleep(tokio::time::Duration::from_millis(2100)).await;

    // Failures should have expired
    let (count, _) = tracker.get_attempts(key).await.unwrap();
    assert_eq!(count, 0, "Failures should expire after TTL");
}

#[tokio::test]
async fn test_tier2_lockout_duration() {
    // Tier 2: 10 failures -> 5 minute (300s) lockout
    let protection = BruteForceProtection::in_memory("test".to_string());

    // Record 10 failures
    for _ in 0..10 {
        protection.record_failure("tier2_user", None).await.unwrap();
    }

    // Should be locked
    assert!(protection.check_allowed("tier2_user", None).await.is_err());

    // Use the tracker directly to simulate time passing
    // (In real system, would need to wait or mock time)
}

#[tokio::test]
async fn test_tier3_lockout_duration() {
    // Tier 3: 15+ failures -> 15 minute (900s) lockout
    let protection = BruteForceProtection::in_memory("test".to_string());

    // Record 15 failures
    for _ in 0..15 {
        protection.record_failure("tier3_user", None).await.unwrap();
    }

    // Should be locked with longest duration
    assert!(protection.check_allowed("tier3_user", None).await.is_err());
}

// ============================================================================
// Combined User + IP Protection Tests
// ============================================================================

#[tokio::test]
async fn test_both_user_and_ip_locked() {
    let protection = BruteForceProtection::in_memory("test".to_string());
    let ip = Some(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1)));

    // Lock user (5 failures)
    for _ in 0..5 {
        protection.record_failure("locked_user", ip).await.unwrap();
    }

    // User should be locked from any IP
    assert!(protection.check_allowed("locked_user", None).await.is_err());

    // Lock IP (20 failures across different users)
    for i in 0..20 {
        let username = format!("other_user_{}", i);
        protection.record_failure(&username, ip).await.unwrap();
    }

    // IP should be locked for any user
    assert!(protection.check_allowed("any_other_user", ip).await.is_err());
}

#[tokio::test]
async fn test_ip_only_failure_tracking() {
    let protection = BruteForceProtection::in_memory("test".to_string());
    let ip = Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)));

    // Record IP-only failures (for non-existent user detection)
    for _ in 0..19 {
        protection.record_ip_failure(ip).await.unwrap();
    }

    // IP should not be locked yet (threshold is 20)
    assert!(protection.check_ip_allowed(ip).await.is_ok());

    // One more failure
    protection.record_ip_failure(ip).await.unwrap();

    // Now IP should be locked
    assert!(protection.check_ip_allowed(ip).await.is_err());
}

#[tokio::test]
async fn test_ip_only_failure_does_not_lock_username() {
    let protection = BruteForceProtection::in_memory("test".to_string());
    let ip = Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 50)));

    // Record many IP-only failures
    for _ in 0..25 {
        protection.record_ip_failure(ip).await.unwrap();
    }

    // IP should be locked
    assert!(protection.check_ip_allowed(ip).await.is_err());

    // But a legitimate user with a different IP should be able to log in
    let different_ip = Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 99)));
    assert!(protection.check_allowed("legitimate_user", different_ip).await.is_ok());
}

// ============================================================================
// Integration Tests (require Docker)
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_based_brute_force_protection() {
    // Full Redis integration tests are in brute_force_tests.rs:
    // - test_redis_tracker_record_and_get
    // - test_redis_tracker_reset
    // - test_brute_force_with_redis_e2e_lockout_and_reset
    // - test_brute_force_with_redis_ip_lockout_and_reset
    //
    // This placeholder documents that Redis tests require Docker.
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_degradation_tracking_with_redis() {
    // Redis degradation tracking tests are in brute_force_tests.rs:
    // - test_redis_tracker_degradation_tracking
    // - test_redis_tracker_counter_is_monotonically_increasing
    // - test_redis_tracker_success_clears_degraded_flag
    //
    // These tests verify that when Redis fails, the system falls back
    // to in-memory tracking and monitors the degradation state.
}
