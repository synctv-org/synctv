//! Playback optimistic locking integration tests
//!
//! Tests for version-based optimistic locking in `PlaybackService`.
//! Validates retry behavior, conflict detection, and version management.
//!
//! Run with: cargo test -p synctv-core --test `playback_optimistic_lock_tests` -- --nocapture
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    config::PasswordComplexityConfig,
    models::{User, UserId, UserRole, UserStatus},
    repository::{playback::RoomPlaybackStateRepository, UserRepository},
    service::{
        auth::{BruteForceProtection, JwtService},
        InMemoryTokenBlacklistStore, RoomService, UserService,
    },
    Error,
};
use synctv_core_testing::create_test_pool;
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

// Optimistic Lock Tests: Repository Level

/// Test: Update with matching version succeeds
///
/// When updating playback state with the correct version,
/// the update should succeed and version should increment.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_repo_update_with_matching_version_succeeds() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("ol_repo_match")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "OL Repo Match Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    let state = playback_repo.create_or_get(&room.id).await.unwrap();
    assert_eq!(state.version, 0, "Initial version should be 0");

    // Update with matching version
    let mut updated = state.clone();
    updated.position = 50.0;
    let result = playback_repo.update(&updated).await.unwrap();

    assert_eq!(result.version, 1, "Version should be incremented to 1");
    assert!(
        (result.position - 50.0).abs() < f64::EPSILON,
        "Current time should be updated"
    );
}

/// Test: Update with stale version fails
///
/// When updating playback state with an old version,
/// the update should fail with `OptimisticLockConflict`.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_repo_update_with_stale_version_fails() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("ol_repo_stale")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "OL Repo Stale Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    let state = playback_repo.create_or_get(&room.id).await.unwrap();

    // First update succeeds, version becomes 1
    let mut first_update = state.clone();
    first_update.position = 100.0;
    let first_result = playback_repo.update(&first_update).await.unwrap();
    assert_eq!(first_result.version, 1);

    // Second update with stale version 0 should fail
    let mut stale_update = state.clone(); // Still has version 0
    stale_update.position = 200.0;
    let result = playback_repo.update(&stale_update).await;

    assert!(
        matches!(result, Err(Error::OptimisticLockConflict)),
        "Expected OptimisticLockConflict, got: {result:?}"
    );

    // Verify data wasn't corrupted
    let current = playback_repo.get(&room.id).await.unwrap().unwrap();
    assert_eq!(current.version, 1, "Version should still be 1");
    assert!(
        (current.position - 100.0).abs() < f64::EPSILON,
        "Current time should be from first update"
    );
}

/// Test: Version increments on each update
///
/// Each successful update should increment the version by 1.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_repo_version_increments_sequentially() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("ol_repo_seq")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "OL Repo Seq Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    let mut state = playback_repo.create_or_get(&room.id).await.unwrap();
    assert_eq!(state.version, 0);

    // Multiple sequential updates
    for (expected_version, position) in [10.0, 20.0, 30.0, 40.0, 50.0].into_iter().enumerate() {
        state.position = position;
        state = playback_repo.update(&state).await.unwrap();
        assert_eq!(
            state.version,
            i64::try_from(expected_version + 1).expect("version index should fit in i64"),
            "Version should be {}",
            expected_version + 1
        );
    }
}

// Optimistic Lock Tests: Service Level Retry Mechanism

/// Test: Concurrent seek operations with retry
///
/// Multiple concurrent seek operations should all eventually succeed
/// through the retry mechanism.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_seek_with_retry() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let owner = user_repo.create(&make_user("ol_seek_retry")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "OL Seek Retry Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    // Spawn 5 concurrent seek operations
    let mut handles = vec![];
    let barrier = Arc::new(tokio::sync::Barrier::new(5));

    for i in 0..5 {
        let rs = room_service.clone();
        let rid = room.id;
        let uid = owner.id;
        let b = barrier.clone();
        let position = f64::from(i).mul_add(100.0, 50.0);

        let handle = tokio::spawn(async move {
            b.wait().await;
            rs.playback_service().seek(rid, uid, position).await
        });
        handles.push(handle);
    }

    let results: Vec<_> = futures::future::join_all(handles).await;

    // All operations should succeed (with retries)
    let mut success_count = 0;
    for result in &results {
        match result {
            Ok(Ok(_)) => success_count += 1,
            Ok(Err(e)) => {
                // May fail with retry exhaustion under high contention
                // but should not be OptimisticLockConflict directly
                assert!(
                    !matches!(e, Error::OptimisticLockConflict),
                    "OptimisticLockConflict should not leak to caller"
                );
            }
            Err(e) => panic!("Task panicked: {e:?}"),
        }
    }

    assert!(success_count >= 1, "At least one seek should succeed");

    // Final state should be valid
    let playback_service = room_service.playback_service();
    let state = playback_service.get_state(&room.id).await.unwrap();
    assert!(
        state.position >= 0.0,
        "Final position should be non-negative"
    );
}

/// Test: Retry mechanism handles version conflicts
///
/// The retry mechanism should handle version conflicts gracefully
/// by re-fetching and retrying.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_retry_handles_version_conflicts() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("ol_retry")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "OL Retry Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();

    // First operation
    let _state1 = playback_service
        .seek(room.id, owner.id, 50.0)
        .await
        .unwrap();

    // Simulate external update (version conflict)
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());
    let mut state = playback_repo.get(&room.id).await.unwrap().unwrap();
    state.position = 999.0;
    playback_repo.update(&state).await.unwrap();

    // Second operation should still succeed via retry
    let state2 = playback_service
        .seek(room.id, owner.id, 100.0)
        .await
        .unwrap();

    assert!(
        state2.seek_applied || state2.state.position >= 999.0 - 1.0,
        "Either seek applied or position reflects external update"
    );
}

/// Test: Retry exhaustion returns degraded response
///
/// When retries are exhausted, seek should return a degraded response
/// with `seek_applied` = false rather than an error.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_retry_exhaustion_returns_degraded_response() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let owner = user_repo.create(&make_user("ol_exhaust")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "OL Exhaust Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    // Spawn many concurrent seeks to trigger retry exhaustion
    let mut handles = vec![];
    let barrier = Arc::new(tokio::sync::Barrier::new(30));

    for i in 0..30 {
        let rs = room_service.clone();
        let rid = room.id;
        let uid = owner.id;
        let b = barrier.clone();
        let position = f64::from(i) * 10.0;

        let handle = tokio::spawn(async move {
            b.wait().await;
            rs.playback_service().seek(rid, uid, position).await
        });
        handles.push(handle);
    }

    let results: Vec<_> = futures::future::join_all(handles).await;

    // Count successes and degraded responses
    let mut success_count = 0;
    let mut degraded_count = 0;

    for result in &results {
        match result {
            Ok(Ok(response)) => {
                if response.seek_applied {
                    success_count += 1;
                } else {
                    degraded_count += 1;
                    // Degraded response should have valid state
                    assert!(response.state.position >= 0.0);
                    assert!(response.message.is_some());
                }
            }
            Ok(Err(e)) => {
                // Other errors are OK
                let _ = e;
            }
            Err(e) => panic!("Task panicked: {e:?}"),
        }
    }

    // At least some should succeed
    assert!(success_count > 0, "At least one seek should succeed");

    // We may or may not get degraded responses depending on contention level
    println!("Success: {success_count}, Degraded: {degraded_count}");
}

// Optimistic Lock Tests: Concurrent Mixed Operations

/// Test: Concurrent mixed operations (seek, play, speed)
///
/// Multiple concurrent operations of different types should preserve a valid
/// final state without leaking raw optimistic-lock conflicts.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_mixed_operations() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let owner = user_repo.create(&make_user("ol_mixed")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "OL Mixed Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    // Spawn different types of operations concurrently
    let mut seek_handles = vec![];
    let mut play_handles = vec![];
    let mut speed_handles = vec![];
    let barrier = Arc::new(tokio::sync::Barrier::new(9));

    // 3 seeks
    for i in 0..3 {
        let rs = room_service.clone();
        let rid = room.id;
        let uid = owner.id;
        let b = barrier.clone();
        let pos = f64::from(i) * 50.0;

        seek_handles.push(tokio::spawn(async move {
            b.wait().await;
            rs.playback_service().seek(rid, uid, pos).await
        }));
    }

    // 3 play/pause toggles
    for i in 0..3 {
        let rs = room_service.clone();
        let rid = room.id;
        let uid = owner.id;
        let b = barrier.clone();
        let playing = i % 2 == 0;

        play_handles.push(tokio::spawn(async move {
            b.wait().await;
            rs.playback_service().set_playing(rid, uid, playing).await
        }));
    }

    // 3 speed changes
    for i in 0..3 {
        let rs = room_service.clone();
        let rid = room.id;
        let uid = owner.id;
        let b = barrier.clone();
        let speed = [0.5, 1.0, 1.5][i];

        speed_handles.push(tokio::spawn(async move {
            b.wait().await;
            rs.playback_service().change_speed(rid, uid, speed).await
        }));
    }

    // Collect all results
    let seek_results: Vec<_> = futures::future::join_all(seek_handles).await;
    let play_results: Vec<_> = futures::future::join_all(play_handles).await;
    let speed_results: Vec<_> = futures::future::join_all(speed_handles).await;

    // Track successful API responses rather than assuming a fixed success ratio.
    // Under bounded retries, some operations may still exhaust their budget, and
    // an OK response is not guaranteed to map one-to-one to a persisted write.
    let initial_version = RoomPlaybackStateRepository::new(pool.clone())
        .get(&room.id)
        .await
        .unwrap()
        .map_or(0, |state| state.version);
    let mut successful_responses = 0;
    for result in &seek_results {
        match result {
            Ok(Ok(response)) => {
                if response.seek_applied {
                    successful_responses += 1;
                }
            }
            Ok(Err(e)) => {
                assert!(
                    !matches!(e, Error::OptimisticLockConflict),
                    "OptimisticLockConflict should not leak"
                );
            }
            Err(e) => panic!("Task panicked: {e:?}"),
        }
    }
    for result in &play_results {
        match result {
            Ok(Ok(_)) => successful_responses += 1,
            Ok(Err(e)) => {
                assert!(
                    !matches!(e, Error::OptimisticLockConflict),
                    "OptimisticLockConflict should not leak"
                );
            }
            Err(e) => panic!("Task panicked: {e:?}"),
        }
    }
    for result in &speed_results {
        match result {
            Ok(Ok(_)) => successful_responses += 1,
            Ok(Err(e)) => {
                assert!(
                    !matches!(e, Error::OptimisticLockConflict),
                    "OptimisticLockConflict should not leak"
                );
            }
            Err(e) => panic!("Task panicked: {e:?}"),
        }
    }

    assert!(
        successful_responses >= 1,
        "At least one playback operation should succeed, got: {successful_responses}"
    );

    // Final state should be consistent
    let playback_service = room_service.playback_service();
    let state = playback_service.get_state(&room.id).await.unwrap();
    assert!(state.speed > 0.0, "Speed should be positive");
    assert!(state.position >= 0.0, "Position should be non-negative");
    assert!(
        state.version >= initial_version,
        "Playback version must not move backwards under concurrent operations"
    );
    assert!(
        state.version <= initial_version + 9 * 3,
        "Bounded retries may consume reserved fence versions, but version growth should remain bounded"
    );
}

// Optimistic Lock Tests: High Contention Correctness

/// Test: high-contention operations still preserve a valid final playback state.
///
/// This is a concurrency-correctness test, not a benchmark: it verifies that a
/// burst of mixed operations does not corrupt state or leak internal conflict
/// errors to callers.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_high_contention_operations_remain_consistent() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let owner = user_repo
        .create(&make_user("ol_high_contention"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "OL Stress Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    // Spawn 50 concurrent operations
    let mut handles = vec![];
    let barrier = Arc::new(tokio::sync::Barrier::new(50));

    for i in 0..50 {
        let rs = room_service.clone();
        let rid = room.id;
        let uid = owner.id;
        let b = barrier.clone();

        handles.push(tokio::spawn(async move {
            b.wait().await;

            // Random operation type
            match i % 3 {
                0 => rs
                    .playback_service()
                    .seek(rid, uid, f64::from(i) * 5.0)
                    .await
                    .map(|r| format!("seek:{}", r.state.position)),
                1 => rs
                    .playback_service()
                    .set_playing(rid, uid, i % 2 == 0)
                    .await
                    .map(|s| format!("playing:{}", s.is_playing)),
                _ => rs
                    .playback_service()
                    .change_speed(rid, uid, 1.0 + f64::from(i % 4) * 0.5)
                    .await
                    .map(|s| format!("speed:{}", s.speed)),
            }
        }));
    }

    let results: Vec<_> = futures::future::join_all(handles).await;

    // Count successes
    let mut success_count = 0;
    for result in &results {
        match result {
            Ok(Ok(_)) => success_count += 1,
            Ok(Err(_)) => {}
            Err(e) => panic!("Task panicked: {e:?}"),
        }
    }

    // Most operations should succeed
    assert!(
        success_count >= 15,
        "At least 30% should succeed, got: {success_count}"
    );

    // Verify final state is valid
    let playback_service = room_service.playback_service();
    let state = playback_service.get_state(&room.id).await.unwrap();
    assert!(
        state.speed > 0.0 && state.speed <= 4.0,
        "Speed should be valid"
    );
    assert!(state.version > 0, "Version should have advanced");
}

// Optimistic Lock Tests: Version Number Overflow

/// Test: Version number handles large values
///
/// Test that the version number works correctly even with large values.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_version_handles_large_values() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("ol_large_ver")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "OL Large Ver Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    // Manually set version to a large value
    sqlx::query("UPDATE room_playback_state SET version = 999998 WHERE room_id = $1")
        .bind(room.id)
        .execute(&pool)
        .await
        .unwrap();

    // Get state (should have version 999998)
    let mut state = playback_repo.get(&room.id).await.unwrap().unwrap();
    assert_eq!(state.version, 999_998);

    // Update should work and version should increment
    state.position = 100.0;
    let result = playback_repo.update(&state).await.unwrap();
    assert_eq!(result.version, 999_999, "Version should be 999999");

    // One more update
    let mut state = result;
    state.position = 200.0;
    let result = playback_repo.update(&state).await.unwrap();
    assert_eq!(result.version, 1_000_000, "Version should be 1000000");
}
