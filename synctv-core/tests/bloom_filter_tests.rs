//! ProtectedCache bloom filter integration tests
//!
//! Tests rebuilding=true bypass behavior and grace period clear.
//!
//! Run with: cargo test --test bloom_filter_tests

use synctv_core::cache::ProtectedCache;

#[tokio::test]
async fn test_rebuilding_bypass_behavior() {
    // Create a ProtectedCache and manually simulate the rebuilding state.
    // When rebuilding=true, check_exists should bypass bloom filter (return None
    // instead of Some(false)) so all lookups go to the database.
    let cache = ProtectedCache::new(1000, 100);

    // Mark a key as existing
    cache.mark_exists("known_key").await;

    // A key not in the bloom filter should return Some(false) when not rebuilding
    let result = cache.check_exists("unknown_key").await;
    assert_eq!(
        result,
        Some(false),
        "Unknown key should be definitively absent when bloom filter is active"
    );

    // Now start periodic reset with a very short interval.
    // The reset will set rebuilding=true, clear the bloom filter, then after
    // the grace period (30s), set rebuilding=false.
    // We can't easily wait 30s, so instead we test by manually triggering
    // the same behavior: clear and check that bypass works.
    cache.clear().await;

    // After clearing, a known key is no longer in the bloom filter.
    // If we mark the cache as "rebuilding" (which periodic_reset does automatically),
    // the check_exists should return None (bypass) instead of Some(false).

    // Since we can't easily set rebuilding=true directly in an integration test
    // (it's an internal field), let's test via start_periodic_reset with a
    // tiny interval and tokio::time::pause.
    //
    // For a simpler test that doesn't require time manipulation, we'll verify
    // the basic bloom filter behavior and rely on the unit tests in the source
    // file for the rebuilding flag.

    // After clear, unknown keys return Some(false) when bloom is not rebuilding
    let result = cache.check_exists("some_new_key").await;
    assert_eq!(
        result,
        Some(false),
        "After clear, bloom should report unknown keys as absent"
    );
}

#[tokio::test(start_paused = true)]
async fn test_rebuilding_bypass_via_periodic_reset() {
    let cache = ProtectedCache::new(1000, 100);

    // Mark keys as existing
    cache.mark_exists("key_a").await;
    cache.mark_exists("key_b").await;

    // key_a should be "might exist" (None), key_c should be "definitely not" (Some(false))
    assert_eq!(cache.check_exists("key_a").await, None);
    assert_eq!(cache.check_exists("key_c").await, Some(false));

    // Start periodic reset with a 120-second interval (longer than the 30s grace period)
    // to ensure the second tick doesn't fire immediately after the grace period ends.
    cache
        .start_periodic_reset(std::time::Duration::from_secs(120))
        .await;

    // Advance time past the first interval to trigger reset.
    // Use tokio::time::sleep which auto-advances in paused mode.
    tokio::time::sleep(std::time::Duration::from_secs(121)).await;

    // During rebuilding=true, check_exists should bypass bloom filter.
    // Unknown keys should return None (uncertain) instead of Some(false).
    let result = cache.check_exists("key_c").await;
    assert_eq!(
        result, None,
        "During rebuilding, bloom filter should be bypassed (unknown key returns None)"
    );

    // Quick check should also bypass
    let quick = cache.check_exists_quick("key_c").await;
    assert!(
        quick,
        "During rebuilding, quick check should return true (bypass)"
    );

    // Advance time past the 30-second grace period.
    // The grace period sleep is 30s, so advance 31s more.
    tokio::time::sleep(std::time::Duration::from_secs(31)).await;

    // After grace period, rebuilding=false, bloom filter active again.
    // Unknown keys should return Some(false) again.
    let result = cache.check_exists("key_c").await;
    assert_eq!(
        result,
        Some(false),
        "After grace period, bloom filter should be active again"
    );

    cache.stop_periodic_reset().await;
}
