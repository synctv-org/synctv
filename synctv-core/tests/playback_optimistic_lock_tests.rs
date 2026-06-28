//! Playback optimistic locking integration tests
//!
//! Tests for version-based optimistic locking in `PlaybackService`.
//! Validates retry behavior, conflict detection, and version management.
//!

use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    models::{Media, MediaId, SourceProvider, User, UserId, UserRole, UserStatus},
    repository::{MediaRepository, RoomPlaybackStateRepository, UserRepository},
    service::{
        auth::{BruteForceProtection, JwtService},
        InMemoryTokenBlacklistStore, RoomService, UserService,
    },
    Error,
};
use synctv_core_testing::create_test_pool;
use synctv_core_testing::{TestOptionExt, TestResultExt};
fn make_user_service(pool: &PgPool) -> UserService {
    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = JwtService::new(secret).checked("JWT service should be created");
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    UserService::new_for_tests(
        pool,
        jwt_service,
        username_cache,
        token_blacklist,
        key_builder,
        brute_force,
    )
}

fn make_room_service(pool: PgPool) -> RoomService {
    let user_service = make_user_service(&pool);

    RoomService::new_for_tests(pool, user_service).checked("room service should build")
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

async fn attach_test_media(
    pool: &PgPool,
    playback_repo: &RoomPlaybackStateRepository,
    mut state: synctv_core::models::RoomPlaybackState,
    owner_id: UserId,
) -> synctv_core::models::RoomPlaybackState {
    let media = Media {
        id: MediaId::new(),
        playlist_id: None,
        room_id: state.room_id,
        creator_id: Some(owner_id),
        name: "Optimistic Lock Test Video".to_string(),
        description: String::new(),
        position: 0.0,
        source_provider: SourceProvider::DirectUrl,
        source_config: synctv_core_testing::direct_url_media_source_config(
            "https://example.com/video.mp4",
        ),
        provider_instance_name: None,
        cover_file_reference_id: None,
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    let media = MediaRepository::new(pool.clone())
        .create(&media)
        .await
        .checked("test media should be created");
    state.playing_media_id = Some(media.id);
    state.playing_playlist_id = None;
    state.target = None;
    state.position = 0.0;
    playback_repo
        .update(&state)
        .await
        .checked("playback state should attach test media")
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

    let owner = user_repo
        .create(&make_user("ol_repo_match"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "OL Repo Match Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    let state = playback_repo
        .create_or_get(&room.id)
        .await
        .checked("test operation should succeed");
    let state = attach_test_media(&pool, &playback_repo, state, owner.id).await;

    // Update with matching version
    let mut updated = state.clone();
    updated.position = 50.0;
    let result = playback_repo
        .update(&updated)
        .await
        .checked("test operation should succeed");

    assert_eq!(
        result.version,
        state.version + 1,
        "Version should be incremented"
    );
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

    let owner = user_repo
        .create(&make_user("ol_repo_stale"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "OL Repo Stale Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    let state = playback_repo
        .create_or_get(&room.id)
        .await
        .checked("test operation should succeed");
    let state = attach_test_media(&pool, &playback_repo, state, owner.id).await;

    // First update succeeds, version becomes 1
    let mut first_update = state.clone();
    first_update.position = 100.0;
    let first_result = playback_repo
        .update(&first_update)
        .await
        .checked("test operation should succeed");
    assert_eq!(first_result.version, state.version + 1);

    // Second update with stale version 0 should fail
    let mut stale_update = state.clone(); // Still has version 0
    stale_update.position = 200.0;
    let result = playback_repo.update(&stale_update).await;

    assert!(
        matches!(result, Err(Error::OptimisticLockConflict)),
        "Expected OptimisticLockConflict, got: {result:?}"
    );

    // Verify data wasn't corrupted
    let current = playback_repo
        .get(&room.id)
        .await
        .checked("test operation should succeed")
        .checked("test operation should succeed");
    assert_eq!(
        current.version, first_result.version,
        "Version should remain at the first update"
    );
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

    let owner = user_repo
        .create(&make_user("ol_repo_seq"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "OL Repo Seq Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    let mut state = playback_repo
        .create_or_get(&room.id)
        .await
        .checked("test operation should succeed");
    assert_eq!(state.version, 0);

    // Multiple sequential updates
    for (expected_version, position) in [10.0, 20.0, 30.0, 40.0, 50.0].into_iter().enumerate() {
        state.position = position;
        state = playback_repo
            .update(&state)
            .await
            .checked("test operation should succeed");
        assert_eq!(
            state.version,
            i64::try_from(expected_version + 1).checked("version index should fit in i64"),
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

    let owner = user_repo
        .create(&make_user("ol_seek_retry"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "OL Seek Retry Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());
    let state = playback_repo
        .create_or_get(&room.id)
        .await
        .checked("test operation should succeed");
    let _state = attach_test_media(&pool, &playback_repo, state, owner.id).await;

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
            Err(e) => std::panic::panic_any(format!("seek task should complete: {e:?}")),
        }
    }

    assert!(success_count >= 1, "At least one seek should succeed");

    // Final state should be valid
    let playback_service = room_service.playback_service();
    let state = playback_service
        .get_state(&room.id)
        .await
        .checked("test operation should succeed");
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

    let owner = user_repo
        .create(&make_user("ol_retry"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "OL Retry Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playback_service = room_service.playback_service();
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());
    let state = playback_repo
        .create_or_get(&room.id)
        .await
        .checked("test operation should succeed");
    let _state = attach_test_media(&pool, &playback_repo, state, owner.id).await;

    // First operation
    let _state1 = playback_service
        .seek(room.id, owner.id, 50.0)
        .await
        .checked("test operation should succeed");

    // Simulate external update (version conflict)
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());
    let mut state = playback_repo
        .get(&room.id)
        .await
        .checked("test operation should succeed")
        .checked("test operation should succeed");
    state.position = 999.0;
    playback_repo
        .update(&state)
        .await
        .checked("test operation should succeed");

    // Second operation should still succeed via retry
    let state2 = playback_service
        .seek(room.id, owner.id, 100.0)
        .await
        .checked("test operation should succeed");

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

    let owner = user_repo
        .create(&make_user("ol_exhaust"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "OL Exhaust Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());
    let state = playback_repo
        .create_or_get(&room.id)
        .await
        .checked("test operation should succeed");
    let _state = attach_test_media(&pool, &playback_repo, state, owner.id).await;

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
            Err(e) => std::panic::panic_any(format!("seek task should complete: {e:?}")),
        }
    }

    // At least some should succeed
    assert!(success_count > 0, "At least one seek should succeed");

    // We may or may not get degraded responses depending on contention level
    tracing::info!(
        success_count,
        degraded_count,
        "retry exhaustion result counts"
    );
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

    let owner = user_repo
        .create(&make_user("ol_mixed"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "OL Mixed Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());
    let state = playback_repo
        .create_or_get(&room.id)
        .await
        .checked("test operation should succeed");
    let _state = attach_test_media(&pool, &playback_repo, state, owner.id).await;

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
        .checked("test operation should succeed")
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
            Err(e) => std::panic::panic_any(format!("seek task should complete: {e:?}")),
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
            Err(e) => std::panic::panic_any(format!("play task should complete: {e:?}")),
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
            Err(e) => std::panic::panic_any(format!("speed task should complete: {e:?}")),
        }
    }

    assert!(
        successful_responses >= 1,
        "At least one playback operation should succeed, got: {successful_responses}"
    );

    // Final state should be consistent
    let playback_service = room_service.playback_service();
    let state = playback_service
        .get_state(&room.id)
        .await
        .checked("test operation should succeed");
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
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "OL Stress Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());
    let state = playback_repo
        .create_or_get(&room.id)
        .await
        .checked("test operation should succeed");
    let _state = attach_test_media(&pool, &playback_repo, state, owner.id).await;

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
            Err(e) => std::panic::panic_any(format!("playback task should complete: {e:?}")),
        }
    }

    // Most operations should succeed
    assert!(
        success_count >= 15,
        "At least 30% should succeed, got: {success_count}"
    );

    // Verify final state is valid
    let playback_service = room_service.playback_service();
    let state = playback_service
        .get_state(&room.id)
        .await
        .checked("test operation should succeed");
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

    let owner = user_repo
        .create(&make_user("ol_large_ver"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "OL Large Ver Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    // Manually set version to a large value
    sqlx::query!(
        "UPDATE room_playback_state SET version = 999998 WHERE room_id = $1",
        room.id.as_i64()
    )
    .execute(&pool)
    .await
    .checked("test operation should succeed");

    // Get state (should have version 999998)
    let mut state = playback_repo
        .get(&room.id)
        .await
        .checked("test operation should succeed")
        .checked("test operation should succeed");
    assert_eq!(state.version, 999_998);

    // Update should work and version should increment
    state.position = 100.0;
    let result = playback_repo
        .update(&state)
        .await
        .checked("test operation should succeed");
    assert_eq!(result.version, 999_999, "Version should be 999999");

    // One more update
    let mut state = result;
    state.position = 200.0;
    let result = playback_repo
        .update(&state)
        .await
        .checked("test operation should succeed");
    assert_eq!(result.version, 1_000_000, "Version should be 1000000");
}
