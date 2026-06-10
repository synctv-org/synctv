//! `TieredCache` integration tests with Redis L2 backend
//!
//! Tests the L2 (Redis) caching layer, including set/get, invalidation,
//! and clear operations.
//!

use std::sync::Arc;
use synctv_core::cache::{
    l2_backend::RedisCacheL2,
    user_cache::{CachedUser, CachedUserSnapshot},
    UserCache,
};
use synctv_core::models::{UserId, UserRole, UserStatus};
use synctv_core_testing::{ok, some, start_redis as start_test_redis};

fn make_cached_user(id: UserId, username: &str) -> CachedUser {
    CachedUser::from_snapshot(CachedUserSnapshot {
        id,
        username: username.to_string(),
        role: UserRole::User,
        status: UserStatus::Active,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        is_banned: false,
        is_deleted: false,
    })
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_tiered_cache_l2_set_and_get() {
    let (_container, conn) = start_test_redis().await;
    let l2 = Arc::new(RedisCacheL2::from_runtime(synctv_core::direct_runtime(
        conn,
    )));

    let cache = UserCache::new(l2, 100, 5, 300, "test:user:".to_string());

    let user_id = UserId::expect_positive(99_001);
    let user = make_cached_user(user_id, "alice");

    // Set in cache (populates both L1 and L2)
    ok(
        cache.set(&user_id, user.clone()).await,
        "cached user should be stored in tiered cache",
    );

    // Clear L1 so the next get must come from L2
    cache.clear_l1();

    // Get should hit L2
    let retrieved = ok(
        cache.get(&user_id).await,
        "cached user should be read from tiered cache",
    );
    assert!(
        retrieved.is_some(),
        "Should retrieve from L2 after L1 clear"
    );
    let retrieved = some(retrieved, "cached user should exist after L1 clear");
    assert_eq!(retrieved.status(), UserStatus::Active);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_tiered_cache_l2_invalidate_removes_from_redis() {
    let (_container, conn) = start_test_redis().await;
    let l2 = Arc::new(RedisCacheL2::from_runtime(synctv_core::direct_runtime(
        conn,
    )));

    let cache = UserCache::new(l2, 100, 5, 300, "test:user:inv:".to_string());

    let user_id = UserId::expect_positive(99_002);
    let user = make_cached_user(user_id, "bob");

    ok(
        cache.set(&user_id, user).await,
        "cached user should be stored before invalidation",
    );

    // Verify it exists
    assert!(ok(
        cache.get(&user_id).await,
        "cached user should be read before invalidation"
    )
    .is_some());

    // Invalidate removes from both L1 and L2
    ok(
        cache.invalidate(&user_id).await,
        "cached user should be invalidated",
    );

    // Should not be in L1
    assert!(ok(
        cache.get(&user_id).await,
        "cached user should be read after invalidation"
    )
    .is_none());

    // Clear L1 again and try L2 -- should also be gone
    cache.clear_l1();
    assert!(
        ok(
            cache.get(&user_id).await,
            "cached user should be read from L2 after invalidation"
        )
        .is_none(),
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

    let cache = UserCache::new(l2, 100, 5, 300, "test:user:clr:".to_string());

    let user1 = UserId::expect_positive(99_003);
    let user2 = UserId::expect_positive(99_004);

    ok(
        cache.set(&user1, make_cached_user(user1, "alice")).await,
        "first cached user should be stored",
    );
    ok(
        cache.set(&user2, make_cached_user(user2, "bob")).await,
        "second cached user should be stored",
    );

    // Both should exist
    assert!(ok(
        cache.get(&user1).await,
        "first cached user should be read before clear"
    )
    .is_some());
    assert!(ok(
        cache.get(&user2).await,
        "second cached user should be read before clear"
    )
    .is_some());

    // Clear all (L1 + L2)
    cache.clear().await;

    // Neither should exist even after L1 clear
    assert!(ok(
        cache.get(&user1).await,
        "first cached user should be read after clear"
    )
    .is_none());
    assert!(ok(
        cache.get(&user2).await,
        "second cached user should be read after clear"
    )
    .is_none());
}
