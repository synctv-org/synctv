//! Brute-force protection tests
//!
//! Tests the InMemoryAttemptTracker, BruteForceProtection logic, and
//! (with testcontainers) the RedisAttemptTracker.
//!
//! Run with: cargo test --test brute_force_tests -- --nocapture

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use synctv_core::service::{
    AttemptTracker, BruteForceProtection, InMemoryAttemptTracker, RedisAttemptTracker,
};

// ============================================================================
// InMemoryAttemptTracker tests
// ============================================================================

#[tokio::test]
async fn test_in_memory_tracker_record_and_get() {
    let tracker = InMemoryAttemptTracker::new(1000, 900);
    let key = "user:alice";
    let now = chrono::Utc::now().timestamp();

    // Initially no attempts
    let (count, _ts) = tracker.get_attempts(key).await;
    assert_eq!(count, 0);

    // Record failures
    tracker.record_failure(key, now, 900).await;
    tracker.record_failure(key, now + 1, 900).await;
    tracker.record_failure(key, now + 2, 900).await;

    let (count, last_ts) = tracker.get_attempts(key).await;
    assert_eq!(count, 3);
    assert_eq!(last_ts, now + 2);
}

#[tokio::test]
async fn test_in_memory_tracker_reset_clears() {
    let tracker = InMemoryAttemptTracker::new(1000, 900);
    let key = "user:bob";
    let now = chrono::Utc::now().timestamp();

    tracker.record_failure(key, now, 900).await;
    tracker.record_failure(key, now, 900).await;

    let (count, _) = tracker.get_attempts(key).await;
    assert_eq!(count, 2);

    tracker.reset(key).await;

    let (count, _) = tracker.get_attempts(key).await;
    assert_eq!(count, 0);
}

// ============================================================================
// BruteForceProtection tests
// ============================================================================

#[tokio::test]
async fn test_brute_force_below_threshold_allowed() {
    let protection = BruteForceProtection::in_memory("test".to_string());
    let ip = Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));

    // Record 4 failures (below tier1 threshold of 5)
    for _ in 0..4 {
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
    let ip = Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)));

    // Record exactly 5 failures (tier1 threshold)
    for _ in 0..5 {
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
        username_tracker.record_failure(key, past + i, 900).await;
    }

    // Lockout should have expired (60s window, 120s ago)
    let result = protection.check_allowed("charlie", None).await;
    assert!(result.is_ok(), "Tier1 lockout should have expired after 60s");
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

use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;

const REDIS_VERSION: &str = "7-alpine";

async fn start_redis() -> (testcontainers::ContainerAsync<Redis>, redis::aio::ConnectionManager) {
    let container = Redis::default()
        .with_tag(REDIS_VERSION)
        .start()
        .await
        .expect("Failed to start Redis");
    let port = container
        .get_host_port_ipv4(6379)
        .await
        .expect("Failed to get port");
    let redis_url = format!("redis://127.0.0.1:{}", port);
    let client = redis::Client::open(redis_url).expect("Failed to create Redis client");
    let conn = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Failed to create connection manager");
    (container, conn)
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_tracker_record_and_get() {
    let (_container, conn) = start_redis().await;
    let tracker = RedisAttemptTracker::new(conn, 50_000, 900);

    let key = "test:bf:redis_rg:alice";
    let now = chrono::Utc::now().timestamp();

    // Initially no attempts
    let (count, _) = tracker.get_attempts(key).await;
    assert_eq!(count, 0);

    // Record failures
    tracker.record_failure(key, now, 900).await;
    tracker.record_failure(key, now + 1, 900).await;

    let (count, last_ts) = tracker.get_attempts(key).await;
    assert_eq!(count, 2);
    assert_eq!(last_ts, now + 1);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_tracker_reset() {
    let (_container, conn) = start_redis().await;
    let tracker = RedisAttemptTracker::new(conn, 50_000, 900);

    let key = "test:bf:redis_reset:bob";
    let now = chrono::Utc::now().timestamp();

    tracker.record_failure(key, now, 900).await;
    tracker.record_failure(key, now, 900).await;
    tracker.record_failure(key, now, 900).await;

    let (count, _) = tracker.get_attempts(key).await;
    assert_eq!(count, 3);

    // Reset clears both Redis and fallback (verifies B5 fix: handles both Ok/Err)
    tracker.reset(key).await;

    let (count, _) = tracker.get_attempts(key).await;
    assert_eq!(count, 0, "Reset should clear the Redis key");
}

// ============================================================================
// BruteForceProtection::with_redis E2E tests
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_brute_force_with_redis_e2e_lockout_and_reset() {
    let (_container, conn) = start_redis().await;
    let protection = BruteForceProtection::with_redis(conn, "test_e2e:".to_string());
    let ip = Some(IpAddr::V4(Ipv4Addr::new(10, 1, 0, 1)));

    // Record 5 failures to trigger tier1 lockout
    for _ in 0..5 {
        protection.record_failure("redis_user", ip).await.unwrap();
    }

    // Should be locked out
    let result = protection.check_allowed("redis_user", ip).await;
    assert!(result.is_err(), "5 failures via Redis should trigger lockout");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Too many failed login attempts"),
        "Error should mention lockout: {err_msg}"
    );

    // Reset should unlock
    protection.reset("redis_user").await.unwrap();

    let result = protection.check_allowed("redis_user", ip).await;
    assert!(
        result.is_ok(),
        "Reset should unlock the account via Redis"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_brute_force_with_redis_ip_lockout_and_reset() {
    let (_container, conn) = start_redis().await;
    let protection = BruteForceProtection::with_redis(conn, "test_ip_e2e:".to_string());
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
    let result = protection.check_allowed("brand_new_ip_user", Some(ip)).await;
    assert!(
        result.is_err(),
        "IP with 20 failures should be locked out via Redis"
    );

    // Reset the IP
    protection.reset_ip(&ip).await.unwrap();

    // IP should be unlocked now
    let result = protection.check_allowed("brand_new_ip_user", Some(ip)).await;
    assert!(
        result.is_ok(),
        "reset_ip should unlock the IP via Redis"
    );
}
