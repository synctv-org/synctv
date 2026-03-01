//! TieredCache integration tests with Redis L2 backend
//!
//! Tests the L2 (Redis) caching layer, including set/get, invalidation,
//! and clear operations.
//!
//! Run with: cargo test --test tiered_cache_tests -- --nocapture
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use synctv_core::cache::{
    l2_backend::RedisCacheL2,
    UserCache,
    user_cache::CachedUser,
};
use synctv_core::models::{UserId, UserRole, UserStatus};
use testcontainers_modules::redis::Redis;
use testcontainers::runners::AsyncRunner;

async fn start_redis() -> (testcontainers::ContainerAsync<Redis>, redis::aio::ConnectionManager) {
    let container = Redis::default()
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

fn make_cached_user(id: &str, username: &str) -> CachedUser {
    CachedUser::with_updated_at(
        id.to_string(),
        username.to_string(),
        UserRole::User,
        UserStatus::Active,
        chrono::Utc::now(),
        chrono::Utc::now(),
        0,
        false,
    )
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_tiered_cache_l2_set_and_get() {
    let (_container, conn) = start_redis().await;
    let l2 = Arc::new(RedisCacheL2::new(conn));

    let cache = UserCache::new(l2, 100, 5, 300, "test:user:".to_string())
        .expect("Failed to create UserCache");

    let user_id = UserId::from_string("user_l2_1".to_string());
    let user = make_cached_user("user_l2_1", "alice");

    // Set in cache (populates both L1 and L2)
    cache.set(&user_id, user.clone()).await.unwrap();

    // Clear L1 so the next get must come from L2
    cache.clear_l1().await;

    // Get should hit L2
    let retrieved = cache.get(&user_id).await.unwrap();
    assert!(retrieved.is_some(), "Should retrieve from L2 after L1 clear");
    let retrieved = retrieved.unwrap();
    assert_eq!(retrieved.password_version(), 0);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_tiered_cache_l2_invalidate_removes_from_redis() {
    let (_container, conn) = start_redis().await;
    let l2 = Arc::new(RedisCacheL2::new(conn));

    let cache = UserCache::new(l2, 100, 5, 300, "test:user:inv:".to_string())
        .expect("Failed to create UserCache");

    let user_id = UserId::from_string("user_inv_1".to_string());
    let user = make_cached_user("user_inv_1", "bob");

    cache.set(&user_id, user).await.unwrap();

    // Verify it exists
    assert!(cache.get(&user_id).await.unwrap().is_some());

    // Invalidate removes from both L1 and L2
    cache.invalidate(&user_id).await.unwrap();

    // Should not be in L1
    assert!(cache.get(&user_id).await.unwrap().is_none());

    // Clear L1 again and try L2 -- should also be gone
    cache.clear_l1().await;
    assert!(
        cache.get(&user_id).await.unwrap().is_none(),
        "Should not be in L2 after invalidate"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_tiered_cache_clear_removes_all() {
    let (_container, conn) = start_redis().await;
    let l2 = Arc::new(RedisCacheL2::new(conn));

    let cache = UserCache::new(l2, 100, 5, 300, "test:user:clr:".to_string())
        .expect("Failed to create UserCache");

    let user1 = UserId::from_string("user_clr_1".to_string());
    let user2 = UserId::from_string("user_clr_2".to_string());

    cache
        .set(&user1, make_cached_user("user_clr_1", "alice"))
        .await
        .unwrap();
    cache
        .set(&user2, make_cached_user("user_clr_2", "bob"))
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
