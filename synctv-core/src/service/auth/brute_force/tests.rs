use super::tracker::parse_redis_attempt_state;
use super::*;
use crate::test_helpers::failing_redis_runtime;

fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("{context}: {error}")),
    }
}

fn err<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> E {
    match result {
        Ok(_) => std::panic::panic_any(context.to_string()),
        Err(error) => error,
    }
}

fn runtime_without_snapshot() -> Arc<dyn crate::RedisConnectionRuntime> {
    failing_redis_runtime()
}

#[tokio::test]
async fn redis_attempt_tracker_accepts_trait_object_runtime() {
    let runtime = runtime_without_snapshot();
    let tracker = RedisAttemptTracker::from_runtime(runtime.clone(), 128, 60);

    assert!(
        Arc::ptr_eq(&tracker.conn, &runtime),
        "attempt tracker should retain the injected Redis runtime object"
    );
}

#[tokio::test]
async fn brute_force_protection_supports_service_trait_object() {
    let protection: Arc<dyn BruteForceProtectionService> =
        Arc::new(BruteForceProtection::in_memory("trait-test:".to_string()));

    ok(
        protection.record_failure("trait-user", None).await,
        "trait-object brute-force service should record failures",
    );
    ok(
        protection.check_allowed("trait-user", None).await,
        "single failure should stay below the default lockout threshold",
    );
}

#[tokio::test]
async fn brute_force_protection_shared_state_builder_returns_live_service() {
    let profile = SharedStateProfile::for_cluster_runtime(None, "trait-test:", false);
    let protection = ok(
        BruteForceProtection::from_shared_state_profile(&profile),
        "standalone mode should allow local brute-force protection",
    );

    ok(
        protection.check_allowed("trait-user", None).await,
        "shared-state builder should return a live service",
    );
}

#[test]
fn brute_force_protection_shared_state_builder_requires_shared_runtime_in_cluster_mode() {
    let profile = SharedStateProfile::for_cluster_runtime(None, "trait-test:", true);
    let Err(error) = BruteForceProtection::from_shared_state_profile(&profile) else {
        std::panic::panic_any("cluster runtime must reject local brute-force protection");
    };

    assert!(
        error
            .to_string()
            .contains("distributed runtime requires shared brute-force protection state"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn brute_force_protection_accepts_custom_redis_trackers() {
    let runtime = runtime_without_snapshot();
    let config = BruteForceConfig::default();
    let username_tracker: Arc<dyn AttemptTracker> = Arc::new(RedisAttemptTracker::from_runtime(
        runtime.clone(),
        50_000,
        config.attempts_ttl_secs,
    ));
    let ip_tracker: Arc<dyn AttemptTracker> = Arc::new(RedisAttemptTracker::from_runtime(
        runtime,
        100_000,
        config.ip_attempts_ttl_secs,
    ));

    let protection = BruteForceProtection::new_with_config(
        "test".to_string(),
        username_tracker,
        ip_tracker,
        config,
    );

    assert_eq!(protection.key_builder.prefix(), "test");
}

#[test]
fn lockout_duration_standard_thresholds() {
    let protection = BruteForceProtection::in_memory("test".to_string());
    assert_eq!(protection.lockout_duration_with_config(4), None);
    assert_eq!(
        protection.lockout_duration_with_config(5),
        Some(TIER1_LOCKOUT_SECS)
    );
    assert_eq!(
        protection.lockout_duration_with_config(9),
        Some(TIER1_LOCKOUT_SECS)
    );
    assert_eq!(
        protection.lockout_duration_with_config(10),
        Some(TIER2_LOCKOUT_SECS)
    );
    assert_eq!(
        protection.lockout_duration_with_config(14),
        Some(TIER2_LOCKOUT_SECS)
    );
    assert_eq!(
        protection.lockout_duration_with_config(15),
        Some(TIER3_LOCKOUT_SECS)
    );
    assert_eq!(
        protection.lockout_duration_with_config(100),
        Some(TIER3_LOCKOUT_SECS)
    );
}

#[test]
fn redis_tracker_initial_state_matches_failure_mode() {
    let fallback = RedisAttemptTracker::from_runtime(runtime_without_snapshot(), 128, 60);
    assert!(!fallback.is_degraded());
    assert_eq!(fallback.degraded_operation_count(), 0);
    assert!(!fallback.is_fail_closed());

    let fail_closed =
        RedisAttemptTracker::from_runtime_fail_closed(runtime_without_snapshot(), 128, 60);
    assert!(!fail_closed.is_degraded());
    assert_eq!(fail_closed.degraded_operation_count(), 0);
    assert!(fail_closed.is_fail_closed());
}

#[tokio::test]
async fn in_memory_tracker_records_and_resets_attempts() {
    let tracker = InMemoryAttemptTracker::new(1000, 900);
    let key = "test:user";
    let now = chrono::Utc::now().timestamp();

    ok(
        tracker.record_failure(key, now, 900).await,
        "first attempt should record",
    );
    ok(
        tracker.record_failure(key, now, 900).await,
        "second attempt should record",
    );

    let (count, _) = ok(tracker.get_attempts(key).await, "attempts should load");
    assert_eq!(count, 2);

    ok(tracker.reset(key).await, "attempts should reset");
    let (count, _) = ok(tracker.get_attempts(key).await, "attempts should reload");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn in_memory_tracker_operations_succeed() {
    let tracker = InMemoryAttemptTracker::new(1000, 900);
    let key = "test:user";

    assert!(tracker.get_attempts(key).await.is_ok());
    assert!(tracker
        .record_failure(key, chrono::Utc::now().timestamp(), 900)
        .await
        .is_ok());
    assert!(tracker.reset(key).await.is_ok());
}

#[test]
fn fail_closed_backend_error_is_service_unavailable() {
    let err = RedisAttemptTracker::fail_closed_backend_error("please try again later");
    match err {
        Error::ServiceUnavailable(message) => {
            assert!(
                message.contains("Brute-force protection temporarily unavailable"),
                "unexpected message: {message}"
            );
        }
        other => std::panic::panic_any(format!("expected ServiceUnavailable, got {other:?}")),
    }
}

#[test]
fn parse_redis_attempt_state_accepts_json_state() {
    let raw = r#"{"count":7,"last_failure_at":12345}"#;
    let parsed = ok(
        parse_redis_attempt_state("login:user", raw),
        "valid JSON state should parse",
    );

    assert_eq!(parsed, (7, 12345));
}

#[test]
fn parse_redis_attempt_state_rejects_corrupt_state() {
    let err = err(
        parse_redis_attempt_state("login:user", "{bad json"),
        "corrupt state should fail closed",
    );

    assert!(
        matches!(err, Error::ServiceUnavailable(ref message) if message.contains("state is invalid")),
        "unexpected error: {err}"
    );
}
