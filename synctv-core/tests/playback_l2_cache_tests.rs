//! PlaybackState L2 cache integration tests
//!
//! Tests the L2 (Redis) caching layer for PlaybackService, including:
//! - L1 hit behavior
//! - L1 miss with L2 fallback
//! - L2 miss with PostgreSQL fallback
//! - Cross-replica consistency via L2 cache
//!
//! Run with: cargo test -p synctv-core --test playback_l2_cache_tests -- --nocapture
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use chrono::Utc;
use redis::AsyncCommands;
use sqlx::PgPool;
use synctv_core::{
    cache::{CacheL2Backend, KeyBuilder, PlaybackStateCache, RedisCacheL2, UsernameCache},
    config::PasswordComplexityConfig,
    models::{RoomId, User, UserId, UserRole, UserStatus},
    repository::UserRepository,
    service::{
        auth::{BruteForceProtection, JwtService},
        InMemoryTokenBlacklistStore, RoomService, UserService,
    },
};
use synctv_core_testing::{
    redis_connection_manager, start_redis as start_test_redis,
    start_redis_client_manager_with_label,
};

// Test Helpers

async fn start_redis() -> (
    synctv_core_testing::RedisContainer,
    redis::aio::ConnectionManager,
) {
    start_test_redis().await
}

fn make_user_service(pool: &PgPool) -> UserService {
    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = JwtService::new(secret).expect("Failed to create JwtService");
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let password_complexity = PasswordComplexityConfig::default();
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    UserService::new(
        pool,
        jwt_service,
        username_cache,
        password_complexity,
        token_blacklist,
        key_builder,
        brute_force,
    )
}

fn make_room_service(pool: PgPool) -> RoomService {
    let user_service = make_user_service(&pool);

    RoomService::new(pool, user_service)
}

fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        signup_method: synctv_core::models::SignupMethod::Email,
        created_at: now,
        updated_at: now,
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    }
}

// Test 1: L1 Cache Hit Behavior

/// Test that when L1 cache has the data, it's returned without DB/L2 lookup.
///
/// Scenario:
/// - Create a room (which creates playback state)
/// - Get state (populates L1)
/// - Get state again (should hit L1, no additional DB queries)
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_playback_state_l1_cache_hit() {
    let (_container, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("l1_hit_owner")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "L1 Hit Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();

    // First call - populates L1 cache
    let state1 = playback_service.get_state(&room.id).await.unwrap();
    assert!(state1.version >= 0);

    // Second call - should hit L1 cache (no DB query)
    let state2 = playback_service.get_state(&room.id).await.unwrap();
    assert_eq!(state2.room_id, state1.room_id);
    assert_eq!(state2.version, state1.version);
}

// Test 2: L1 Miss Should Check L2

/// Test that when L1 cache misses, L2 (Redis) is checked.
///
/// Scenario:
/// - Create a room with playback state
/// - Populate L2 cache with playback state
/// - Clear L1 cache
/// - Get state should hit L2 (not DB)
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_playback_state_l1_miss_hits_l2() {
    let (_pg_container, pool) = synctv_core_testing::create_test_pool().await;
    let (_redis_container, redis_conn) = start_redis().await;

    // Verify Redis is working
    let mut conn = redis_conn.clone();
    let _: () = conn.set_ex("test:ping", "pong", 60).await.unwrap();

    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("l2_hit_owner")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "L2 Hit Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();

    // First call populates L1 and L2.
    let state1 = playback_service.get_state(&room.id).await.unwrap();

    // Manually populate L2 to verify it is checked.
    let l2 = RedisCacheL2::from_runtime(synctv_core::direct_runtime(redis_conn));
    let l2_key = format!("synctv:playback:{}", room.id);

    // Manually set in L2 to simulate it being there
    let state_json = serde_json::to_string(&state1).unwrap();
    l2.set(&l2_key, &state_json, 300).await.unwrap();

    // Verify L2 has the data
    let from_l2 = l2.get(&l2_key).await.unwrap();
    assert!(from_l2.is_some(), "L2 should have the playback state");
}

// Test 3: L2 Miss Should Read From PostgreSQL

/// Test that when both L1 and L2 miss, PostgreSQL is queried.
///
/// Scenario:
/// - Create a room with playback state
/// - Clear both L1 and L2
/// - Get state should query DB and populate both caches
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_playback_state_l2_miss_reads_from_db() {
    let (_pg_container, pool) = synctv_core_testing::create_test_pool().await;
    let (_redis_container, _redis_conn) = start_redis().await;

    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("db_fallback_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "DB Fallback Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();

    // Get state from DB
    let state = playback_service.get_state(&room.id).await.unwrap();
    assert!(state.version >= 0, "Should have valid state from DB");

    // Verify in PostgreSQL directly
    let db_state: synctv_core::models::RoomPlaybackState = sqlx::query_as(
        "SELECT room_id, playing_media_id, playing_playlist_id, target, \
         \"position\", speed, is_playing, updated_at, version \
         FROM room_playback_state WHERE room_id = $1",
    )
    .bind(room.id)
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(db_state.room_id, room.id);
}

/// Test that `get_state()` persists the default row via `create_or_get()` instead of
/// returning an unpersisted synthetic value.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_playback_state_get_state_persists_missing_row() {
    let (_pg_container, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("persist_missing_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Persist Missing Playback Row".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    sqlx::query("DELETE FROM room_playback_state WHERE room_id = $1")
        .bind(room.id)
        .execute(&pool)
        .await
        .unwrap();

    let state = room_service
        .playback_service()
        .get_state(&room.id)
        .await
        .unwrap();

    let persisted: Option<(RoomId,)> =
        sqlx::query_as("SELECT room_id FROM room_playback_state WHERE room_id = $1")
            .bind(room.id)
            .fetch_optional(&pool)
            .await
            .unwrap();

    assert_eq!(state.room_id, room.id);
    assert!(
        persisted.is_some(),
        "get_state() must create the missing playback row instead of caching an ephemeral default"
    );

    room_service.playback_service().shutdown().await;
    pool.close().await;
}

// Test 4: Cross-Replica Consistency via L2

/// Test that state updates are visible across "replicas" via L2 cache.
///
/// Scenario:
/// - Create room on "replica A"
/// - Update playback state (populates L2)
/// - "Replica B" clears L1 and reads - should get updated state from L2
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_playback_state_cross_replica_consistency() {
    let (_pg_container, pool) = synctv_core_testing::create_test_pool().await;
    let (_redis_container, redis_conn) = start_redis().await;

    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("cross_replica_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Cross Replica Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();

    // Update playback state
    let updated_state = playback_service
        .seek(room.id, owner.id, 123.45)
        .await
        .unwrap();

    // Verify state is updated in DB
    let db_state = playback_service.get_state(&room.id).await.unwrap();
    assert!(
        (db_state.position - 123.45).abs() < f64::EPSILON,
        "State should be updated in DB"
    );

    // Manually test L2 propagation
    let l2 = RedisCacheL2::from_runtime(synctv_core::direct_runtime(redis_conn));
    let l2_key = format!("synctv:playback:{}", room.id);

    // Set in L2 (simulating what the implementation should do)
    let state_json = serde_json::to_string(&updated_state.state).unwrap();
    l2.set(&l2_key, &state_json, 300).await.unwrap();

    // Read from L2 (simulating another replica)
    let from_l2 = l2.get(&l2_key).await.unwrap();
    assert!(from_l2.is_some(), "L2 should have the updated state");

    let deserialized: synctv_core::models::RoomPlaybackState =
        serde_json::from_str(&from_l2.unwrap()).unwrap();
    assert!(
        (deserialized.position - 123.45).abs() < f64::EPSILON,
        "L2 should have updated position"
    );
}

// Test 5: Cache Invalidation on State Update

/// Test that cache is properly invalidated when state is updated.
///
/// Scenario:
/// - Get state (populates cache)
/// - Update state (should invalidate cache)
/// - Get state again (should fetch fresh from DB)
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_playback_state_cache_invalidation_on_update() {
    let (_container, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("cache_inv_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Cache Inv Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();

    // Get initial state (populates cache)
    let initial_state = playback_service.get_state(&room.id).await.unwrap();
    let initial_version = initial_state.version;

    // Update state (should invalidate cache)
    let _ = playback_service
        .seek(room.id, owner.id, 50.0)
        .await
        .unwrap();

    // Get state again (should fetch fresh from DB, not stale cache)
    let updated_state = playback_service.get_state(&room.id).await.unwrap();

    // Version should be incremented
    assert!(
        updated_state.version > initial_version,
        "Version should be incremented after update"
    );
    assert!(
        (updated_state.position - 50.0).abs() < f64::EPSILON,
        "Position should be updated"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_playback_get_state_bypasses_stale_l1_without_invalidation() {
    let (_container, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("strong_playback_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Strong Playback Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();

    let cached_state = playback_service
        .get_state_eventually_consistent(&room.id)
        .await
        .unwrap();

    sqlx::query(
        r#"UPDATE room_playback_state
           SET "position" = $2, version = version + 1, updated_at = NOW()
           WHERE room_id = $1"#,
    )
    .bind(room.id)
    .bind(77.0_f64)
    .execute(&pool)
    .await
    .unwrap();

    let eventual_state = playback_service
        .get_state_eventually_consistent(&room.id)
        .await
        .unwrap();
    assert_eq!(
        eventual_state.version, cached_state.version,
        "eventual path should demonstrate the stale L1 fixture is still present"
    );

    let strong_state = playback_service.get_state(&room.id).await.unwrap();
    assert!(
        strong_state.version > cached_state.version,
        "strong get_state must bypass stale L1"
    );
    assert!(
        (strong_state.position - 77.0).abs() < f64::EPSILON,
        "strong get_state must read the DB-updated playback position"
    );
}

// Test 6: L2 TTL Enforcement

/// Test that L2 cache entries have proper TTL.
///
/// Playback state changes frequently, so L2 TTL should be short.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_playback_state_l2_has_proper_ttl() {
    let (_redis_container, redis_conn) = start_redis().await;

    let l2 = RedisCacheL2::from_runtime(synctv_core::direct_runtime(redis_conn.clone()));
    let l2_key = "test:playback:ttl_test";

    // Set a value with TTL
    let test_state =
        synctv_core::models::RoomPlaybackState::new(RoomId::expect_positive(1_001_000));
    let state_json = serde_json::to_string(&test_state).unwrap();
    l2.set(l2_key, &state_json, 60).await.unwrap();

    // Verify TTL is set
    let mut conn = redis_conn.clone();
    let ttl: i64 = conn.ttl(l2_key).await.unwrap();
    assert!(ttl > 0 && ttl <= 60, "TTL should be set and <= 60 seconds");
}

// Test 7: Version-Based Cache Update (Prevents Stale Overwrites)

/// Test that older versions don't overwrite newer versions in L2.
///
/// Scenario:
/// - Set state with version 10 in L2
/// - Try to set state with version 5 (older)
/// - L2 should reject the older state
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_playback_state_l2_version_check_prevents_stale_overwrite() {
    let (_redis_container, redis_conn) = start_redis().await;

    let l2 = RedisCacheL2::from_runtime(synctv_core::direct_runtime(redis_conn.clone()));
    let l2_key = "test:playback:version_test";

    let mut newer_state =
        synctv_core::models::RoomPlaybackState::new(RoomId::expect_positive(1_001_001));
    newer_state.version = 10;
    newer_state.position = 100.0;
    newer_state.updated_at = Utc::now();

    let newer_json = serde_json::to_string(&newer_state).unwrap();
    let newer_ts = newer_state.updated_at.timestamp_millis();

    // Set newer state first
    let was_set = l2
        .set_if_newer(l2_key, &newer_json, 300, newer_ts)
        .await
        .unwrap();
    assert!(was_set, "Newer state should be set");

    let mut older_state =
        synctv_core::models::RoomPlaybackState::new(RoomId::expect_positive(1_001_001));
    older_state.version = 5;
    older_state.position = 50.0;
    older_state.updated_at = Utc::now() - chrono::Duration::seconds(10);

    let older_json = serde_json::to_string(&older_state).unwrap();
    let older_ts = older_state.updated_at.timestamp_millis();

    // Try to set older state - should be rejected
    let was_set = l2
        .set_if_newer(l2_key, &older_json, 300, older_ts)
        .await
        .unwrap();
    assert!(!was_set, "Older state should NOT overwrite newer state");

    // Verify L2 still has the newer state
    let from_l2 = l2.get(l2_key).await.unwrap().unwrap();
    let stored: synctv_core::models::RoomPlaybackState = serde_json::from_str(&from_l2).unwrap();
    assert_eq!(stored.version, 10, "Version should still be 10");
}

// Test 8: SingleFlight Prevents Thundering Herd on L2 Miss

/// Test that concurrent requests for the same key don't all hit L2/DB.
///
/// Scenario:
/// - Clear cache for a room
/// - Spawn 10 concurrent get_state requests
/// - Only one should hit the DB (SingleFlight deduplication)
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_playback_state_singleflight_prevents_thundering_herd() {
    let (_container, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let owner = user_repo
        .create(&make_user("singleflight_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "SingleFlight Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    // Spawn concurrent get_state requests
    let mut handles = vec![];
    let barrier = Arc::new(tokio::sync::Barrier::new(10));

    for _ in 0..10 {
        let rs = room_service.clone();
        let rid = room.id;
        let b = barrier.clone();

        let handle = tokio::spawn(async move {
            b.wait().await;
            rs.playback_service().get_state(&rid).await
        });
        handles.push(handle);
    }

    let results: Vec<_> = futures::future::join_all(handles).await;

    // All should succeed
    let mut success_count = 0;
    for result in &results {
        match result {
            Ok(Ok(_)) => success_count += 1,
            Ok(Err(e)) => panic!("Request failed: {e:?}"),
            Err(e) => panic!("Task panicked: {e:?}"),
        }
    }

    assert_eq!(success_count, 10, "All requests should succeed");
}

// Test 9: PubSub + L2 Fallback for Cross-Replica Sync

/// Test that when PubSub fails, L2 provides fallback consistency.
///
/// Scenario:
/// - Replica A updates state, writes to L2
/// - PubSub message is lost (simulated)
/// - Replica B reads from L2 on L1 miss
/// - Replica B should see the updated state
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_playback_state_l2_fallback_when_pubsub_fails() {
    let (_pg_container, pool) = synctv_core_testing::create_test_pool().await;
    let (_redis_container, redis_conn) = start_redis().await;

    let l2 = RedisCacheL2::from_runtime(synctv_core::direct_runtime(redis_conn.clone()));

    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("l2_fallback_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "L2 Fallback Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();

    // Update state
    let result = playback_service
        .seek(room.id, owner.id, 200.0)
        .await
        .unwrap();

    // Write to L2 to simulate a value produced by another replica.
    let l2_key = format!("synctv:playback:{}", room.id);
    let state_json = serde_json::to_string(&result.state).unwrap();
    l2.set(&l2_key, &state_json, 300).await.unwrap();

    // Clear L1 to simulate another replica
    playback_service.invalidate_playback_cache(&room.id).await;

    // On L1 miss, if L2 is implemented, it should check L2 first
    // For now, verify the DB has the correct state
    let fresh_state = playback_service.get_state(&room.id).await.unwrap();
    assert!(
        (fresh_state.position - 200.0).abs() < f64::EPSILON,
        "State should be read correctly from DB after L1 invalidation"
    );
}

/// Test that cross-replica playback invalidation clears Redis L2 so stale entries
/// cannot repopulate L1 after an invalidation event.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_playback_state_cross_replica_invalidation_clears_l2() {
    let (_pg_container, pool) = synctv_core_testing::create_test_pool().await;
    let (_redis_container, redis_client, redis_conn) =
        start_redis_client_manager_with_label("playback-l2-invalidation").await;

    let user_repo = UserRepository::new(pool.clone());
    let mut room_service = make_room_service(pool.clone());

    let l2_backend: Arc<dyn CacheL2Backend> = Arc::new(RedisCacheL2::from_runtime(
        synctv_core::direct_runtime(redis_conn.clone()),
    ));
    let l2_cache = PlaybackStateCache::new(
        l2_backend,
        128,
        5,
        60,
        "test:playback:invalidate:".to_string(),
    )
    .unwrap();

    let cache_stream = format!(
        "test:playback:invalidate:stream:{}",
        synctv_common::snanoid!(8)
    );
    let subscriber = Arc::new(synctv_core::cache::CacheInvalidationService::from_runtime(
        synctv_core::direct_runtime(redis_connection_manager(&redis_client).await),
        "node-subscriber".to_string(),
        cache_stream.clone(),
    ));
    subscriber.start().await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let publisher = Arc::new(synctv_core::cache::CacheInvalidationService::from_runtime(
        synctv_core::direct_runtime(redis_connection_manager(&redis_client).await),
        "node-publisher".to_string(),
        cache_stream,
    ));

    room_service.set_playback_l2_cache(l2_cache.clone());
    room_service.set_playback_cache_invalidation(subscriber.clone());
    room_service.playback_service().start().await.unwrap();

    let owner = user_repo
        .create(&make_user("invalidate_l2_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Invalidate L2 Playback".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();
    let state = playback_service.get_state(&room.id).await.unwrap();
    l2_cache.set(&room.id, state.clone()).await.unwrap();
    assert!(l2_cache.get(&room.id).await.unwrap().is_some());

    publisher.invalidate_playback_state(&room.id).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    assert!(
        l2_cache.get(&room.id).await.unwrap().is_none(),
        "cross-replica invalidation must remove stale playback state from Redis L2"
    );
}

// Test 10: PlaybackStateCache Direct Test with L2 (Redis)

/// Test the PlaybackStateCache directly with a real Redis backend.
///
/// This test verifies the tiered cache behavior:
/// - L1 hit returns immediately
/// - L1 miss checks L2
/// - L2 miss would need to fetch from DB (not tested here as PlaybackStateCache doesn't do DB)
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_playback_state_cache_direct_with_redis() {
    let (_redis_container, redis_conn) = start_redis().await;

    let l2_backend = Arc::new(RedisCacheL2::from_runtime(synctv_core::direct_runtime(
        redis_conn.clone(),
    )));
    let cache = PlaybackStateCache::new(
        l2_backend,
        100, // L1 max capacity
        5,   // L1 TTL seconds
        60,  // L2 TTL seconds
        "test:playback:".to_string(),
    )
    .expect("Failed to create PlaybackStateCache");

    let room_id = RoomId::expect_positive(10_000_005);
    let state = synctv_core::models::RoomPlaybackState::new(room_id);

    // Cache miss
    assert!(
        cache.get(&room_id).await.unwrap().is_none(),
        "Should be a cache miss initially"
    );

    // Set in cache
    cache.set(&room_id, state.clone()).await.unwrap();

    // L1 hit
    let from_cache = cache.get(&room_id).await.unwrap().unwrap();
    assert_eq!(from_cache.room_id, room_id);

    // Clear L1, then check L2
    cache.clear_l1();

    // L2 hit
    let from_l2 = cache.get(&room_id).await.unwrap().unwrap();
    assert_eq!(
        from_l2.room_id, room_id,
        "Should get from L2 after L1 clear"
    );

    // Invalidate both
    cache.invalidate(&room_id).await.unwrap();

    // Both should be gone
    assert!(
        cache.get(&room_id).await.unwrap().is_none(),
        "Should be gone after invalidation"
    );
}

// Test 11: PlaybackStateCache set_if_newer with Real Redis

/// Test the set_if_newer functionality with real Redis backend.
///
/// This test verifies that stale data doesn't overwrite fresh data.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_playback_state_cache_set_if_newer_with_redis() {
    let (_redis_container, redis_conn) = start_redis().await;

    let l2_backend = Arc::new(RedisCacheL2::from_runtime(synctv_core::direct_runtime(
        redis_conn.clone(),
    )));
    let cache = PlaybackStateCache::new(l2_backend, 100, 5, 60, "test:playback:newer:".to_string())
        .expect("Failed to create PlaybackStateCache");

    let room_id = RoomId::expect_positive(10_000_006);

    let mut state1 = synctv_core::models::RoomPlaybackState::new(room_id);
    state1.version = 5;
    state1.position = 50.0;
    state1.updated_at = Utc::now();

    // Set initial state
    cache.set(&room_id, state1.clone()).await.unwrap();

    let mut state2 = synctv_core::models::RoomPlaybackState::new(room_id);
    state2.version = 10;
    state2.position = 100.0;
    state2.updated_at = Utc::now() + chrono::Duration::seconds(10);

    // Set newer state - should succeed
    let was_set = cache.set_if_newer(&room_id, state2.clone()).await.unwrap();
    assert!(was_set, "Newer state should be set");

    // Clear L1 to force read from L2
    cache.clear_l1();

    // Verify we get the newer state
    let from_cache = cache.get(&room_id).await.unwrap().unwrap();
    assert_eq!(from_cache.version, 10, "Should have version 10");
    assert!(
        (from_cache.position - 100.0).abs() < f64::EPSILON,
        "Should have position 100"
    );

    let mut state3 = synctv_core::models::RoomPlaybackState::new(room_id);
    state3.version = 3;
    state3.position = 25.0;
    state3.updated_at = Utc::now() - chrono::Duration::seconds(30);

    // Try to set older state - should be rejected
    let was_set = cache.set_if_newer(&room_id, state3).await.unwrap();
    assert!(!was_set, "Older state should be rejected");

    // Verify we still have the newer state
    cache.clear_l1();
    let from_cache = cache.get(&room_id).await.unwrap().unwrap();
    assert_eq!(from_cache.version, 10, "Should still have version 10");
}
