//! `TieredCache` integration tests with Redis L2 backend
//!
//! Tests the L2 (Redis) caching layer, including set/get, invalidation,
//! and clear operations.
//!
//! Run with: cargo test --test `tiered_cache_tests` -- --nocapture
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use synctv_core::cache::{l2_backend::RedisCacheL2, user_cache::CachedUser, UserCache};
use synctv_core::models::{UserId, UserRole, UserStatus};
use synctv_core_testing::start_redis as start_test_redis;

fn make_cached_user(id: UserId, username: &str) -> CachedUser {
    CachedUser::with_updated_at(
        id,
        username.to_string(),
        UserRole::User,
        UserStatus::Active,
        chrono::Utc::now(),
        chrono::Utc::now(),
        false,
    )
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_tiered_cache_l2_set_and_get() {
    let (_container, conn) = start_test_redis().await;
    let l2 = Arc::new(RedisCacheL2::from_runtime(synctv_core::direct_runtime(
        conn,
    )));

    let cache = UserCache::new(l2, 100, 5, 300, "test:user:".to_string())
        .expect("Failed to create UserCache");

    let user_id = UserId::expect_positive(99_001);
    let user = make_cached_user(user_id, "alice");

    // Set in cache (populates both L1 and L2)
    cache.set(&user_id, user.clone()).await.unwrap();

    // Clear L1 so the next get must come from L2
    cache.clear_l1();

    // Get should hit L2
    let retrieved = cache.get(&user_id).await.unwrap();
    assert!(
        retrieved.is_some(),
        "Should retrieve from L2 after L1 clear"
    );
    let retrieved = retrieved.unwrap();
    assert_eq!(retrieved.status(), UserStatus::Active);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_tiered_cache_l2_invalidate_removes_from_redis() {
    let (_container, conn) = start_test_redis().await;
    let l2 = Arc::new(RedisCacheL2::from_runtime(synctv_core::direct_runtime(
        conn,
    )));

    let cache = UserCache::new(l2, 100, 5, 300, "test:user:inv:".to_string())
        .expect("Failed to create UserCache");

    let user_id = UserId::expect_positive(99_002);
    let user = make_cached_user(user_id, "bob");

    cache.set(&user_id, user).await.unwrap();

    // Verify it exists
    assert!(cache.get(&user_id).await.unwrap().is_some());

    // Invalidate removes from both L1 and L2
    cache.invalidate(&user_id).await.unwrap();

    // Should not be in L1
    assert!(cache.get(&user_id).await.unwrap().is_none());

    // Clear L1 again and try L2 -- should also be gone
    cache.clear_l1();
    assert!(
        cache.get(&user_id).await.unwrap().is_none(),
        "Should not be in L2 after invalidate"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_tiered_cache_clear_removes_all() {
    let (_container, conn) = start_test_redis().await;
    let l2 = Arc::new(RedisCacheL2::from_runtime(synctv_core::direct_runtime(
        conn,
    )));

    let cache = UserCache::new(l2, 100, 5, 300, "test:user:clr:".to_string())
        .expect("Failed to create UserCache");

    let user1 = UserId::expect_positive(99_003);
    let user2 = UserId::expect_positive(99_004);

    cache
        .set(&user1, make_cached_user(user1, "alice"))
        .await
        .unwrap();
    cache
        .set(&user2, make_cached_user(user2, "bob"))
        .await
        .unwrap();

    // Both should exist
    assert!(cache.get(&user1).await.unwrap().is_some());
    assert!(cache.get(&user2).await.unwrap().is_some());

    // Clear all (L1 + L2)
    cache.clear().await;

    // Neither should exist even after L1 clear
    assert!(cache.get(&user1).await.unwrap().is_none());
    assert!(cache.get(&user2).await.unwrap().is_none());
}
