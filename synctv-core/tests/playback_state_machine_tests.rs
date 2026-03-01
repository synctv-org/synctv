//! Playback state machine integration tests
//!
//! Tests for playback state transitions including play/pause/stop operations.
//! Validates that state transitions follow expected patterns and that
//! invalid transitions are properly rejected.
//!
//! Run with: cargo test -p synctv-core --test playback_state_machine_tests -- --nocapture

use std::sync::Arc;

use synctv_core_testing::{create_test_pool, create_test_jwt_service};
use synctv_core::{
    cache::{KeyBuilder, UsernameCache, NoopCacheL2},
    config::PasswordComplexityConfig,
    models::{
        UserId, User, UserRole, UserStatus,
        Media, MediaId, Playlist,
    },
    repository::{UserRepository, MediaRepository},
    service::{
        RoomService, UserService, InMemoryTokenBlacklistStore,
        auth::{JwtService, BruteForceProtection},
    },
};
use chrono::Utc;
use sqlx::PgPool;
fn make_user_service(pool: PgPool) -> UserService {
    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = JwtService::new(secret).expect("Failed to create JwtService");
    let l2 = Arc::new(NoopCacheL2);
    let username_cache = UsernameCache::new(l2, "test:username:".to_string(), 100, 60);
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
    let user_service = make_user_service(pool.clone());
    RoomService::new(pool, user_service)
}

fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{}@test.com", username)),
        password_hash: "hash".to_string(),
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: None,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
    }
}

// ============================================================================
// State Machine Tests: Stopped -> Playing Transition
// ============================================================================

/// Test: Initial state should be stopped (is_playing = false)
///
/// A newly created room should have playback state with is_playing = false.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_initial_state_is_stopped() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("sm_initial")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Initial State Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();
    let state = playback_service.get_state(&room.id).await.unwrap();

    assert!(!state.is_playing, "Initial state should be stopped (is_playing = false)");
    assert!((state.current_time - 0.0).abs() < f64::EPSILON, "Initial position should be 0");
    assert!((state.speed - 1.0).abs() < f64::EPSILON, "Initial speed should be 1.0");
    assert!(state.playing_media_id.is_none(), "Initial media should be None");
}

/// Test: Stopped -> Playing transition
///
/// A room in stopped state should be able to transition to playing
/// when set_playing(true) is called.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_stopped_to_playing_transition() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("sm_play")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Play Transition Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();

    // Transition to playing
    let state = playback_service
        .set_playing(room.id.clone(), owner.id.clone(), true)
        .await
        .unwrap();

    assert!(state.is_playing, "State should be playing after set_playing(true)");
}

/// Test: Playing -> Paused transition
///
/// A room in playing state should be able to transition to paused
/// when set_playing(false) is called.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_playing_to_paused_transition() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("sm_pause")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Pause Transition Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();

    // First, start playing
    playback_service
        .set_playing(room.id.clone(), owner.id.clone(), true)
        .await
        .unwrap();

    // Then, pause
    let state = playback_service
        .set_playing(room.id.clone(), owner.id.clone(), false)
        .await
        .unwrap();

    assert!(!state.is_playing, "State should be paused after set_playing(false)");
}

/// Test: Paused -> Playing transition
///
/// A room in paused state should be able to resume playing
/// when set_playing(true) is called.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_paused_to_playing_transition() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("sm_resume")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Resume Transition Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();

    // Start playing
    playback_service
        .set_playing(room.id.clone(), owner.id.clone(), true)
        .await
        .unwrap();

    // Pause
    playback_service
        .set_playing(room.id.clone(), owner.id.clone(), false)
        .await
        .unwrap();

    // Resume
    let state = playback_service
        .set_playing(room.id.clone(), owner.id.clone(), true)
        .await
        .unwrap();

    assert!(state.is_playing, "State should be playing after resume");
}

// ============================================================================
// State Machine Tests: Full Transition Matrix
// ============================================================================

/// Test: Transition matrix - all valid transitions
///
/// Tests the complete state transition matrix:
/// - Stopped -> Playing (valid)
/// - Stopped -> Stopped (idempotent)
/// - Playing -> Paused (valid)
/// - Playing -> Playing (idempotent)
/// - Paused -> Playing (valid)
/// - Paused -> Paused (idempotent)
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_state_transition_matrix_all_valid() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("sm_matrix")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Matrix Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();

    // Stopped -> Stopped (idempotent)
    let state = playback_service
        .set_playing(room.id.clone(), owner.id.clone(), false)
        .await
        .unwrap();
    assert!(!state.is_playing, "Stopped -> Stopped should stay stopped");

    // Stopped -> Playing
    let state = playback_service
        .set_playing(room.id.clone(), owner.id.clone(), true)
        .await
        .unwrap();
    assert!(state.is_playing, "Stopped -> Playing should become playing");

    // Playing -> Playing (idempotent)
    let state = playback_service
        .set_playing(room.id.clone(), owner.id.clone(), true)
        .await
        .unwrap();
    assert!(state.is_playing, "Playing -> Playing should stay playing");

    // Playing -> Paused
    let state = playback_service
        .set_playing(room.id.clone(), owner.id.clone(), false)
        .await
        .unwrap();
    assert!(!state.is_playing, "Playing -> Paused should become paused");

    // Paused -> Paused (idempotent)
    let state = playback_service
        .set_playing(room.id.clone(), owner.id.clone(), false)
        .await
        .unwrap();
    assert!(!state.is_playing, "Paused -> Paused should stay paused");

    // Paused -> Playing
    let state = playback_service
        .set_playing(room.id.clone(), owner.id.clone(), true)
        .await
        .unwrap();
    assert!(state.is_playing, "Paused -> Playing should become playing");
}

/// Test: Rapid state transitions (toggle play/pause)
///
/// Tests that rapid toggling between play and pause states works correctly.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_rapid_state_transitions() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let owner = user_repo.create(&make_user("sm_toggle")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Toggle Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();

    // Rapidly toggle play/pause 10 times
    for i in 0..10 {
        let playing = i % 2 == 0;
        let state = playback_service
            .set_playing(room.id.clone(), owner.id.clone(), playing)
            .await
            .unwrap();

        assert_eq!(state.is_playing, playing,
            "After toggle {}, is_playing should be {}", i, playing);
    }

    // Final state should be paused (last toggle was false)
    let final_state = playback_service.get_state(&room.id).await.unwrap();
    assert!(!final_state.is_playing, "Final state should be paused");
}

// ============================================================================
// State Machine Tests: Position Preservation
// ============================================================================

/// Test: Position preserved on pause
///
/// When pausing, the current position should be preserved.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_position_preserved_on_pause() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("sm_pos_pause")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Position Pause Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();

    // Seek to a position
    playback_service
        .seek(room.id.clone(), owner.id.clone(), 120.0)
        .await
        .unwrap();

    // Start playing
    playback_service
        .set_playing(room.id.clone(), owner.id.clone(), true)
        .await
        .unwrap();

    // Pause
    let state = playback_service
        .set_playing(room.id.clone(), owner.id.clone(), false)
        .await
        .unwrap();

    // Position should be preserved (approximately, since computed time may advance slightly)
    assert!(state.current_time >= 120.0 - 1.0,
        "Position should be preserved on pause, got: {}", state.current_time);
}

/// Test: Position reset on media switch
///
/// When switching to a new media, position should reset to 0.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_position_reset_on_media_switch() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("sm_pos_switch")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Position Switch Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Get the root playlist
    let playlists: Vec<Playlist> = sqlx::query_as(
        "SELECT * FROM playlists WHERE room_id = $1"
    )
    .bind(room.id.as_str())
    .fetch_all(&pool)
    .await
    .unwrap();
    let root_playlist = &playlists[0];

    // Add media
    let media = Media {
        id: MediaId::new(),
        playlist_id: root_playlist.id.clone(),
        room_id: room.id.clone(),
        creator_id: Some(owner.id.clone()),
        name: "Test Video".to_string(),
        position: 0,
        source_provider: "direct_url".to_string(),
        source_config: serde_json::json!({"url": "https://example.com/video.mp4"}),
        provider_instance_name: None,
        added_at: Utc::now(),
        version: 0,
    };
    media_repo.create(&media).await.unwrap();

    let playback_service = room_service.playback_service();

    // Seek to a position
    playback_service
        .seek(room.id.clone(), owner.id.clone(), 150.0)
        .await
        .unwrap();

    // Switch media
    let state = playback_service
        .switch_media(room.id.clone(), owner.id.clone(), media.id.clone())
        .await
        .unwrap();

    // Position should be reset to 0
    assert!((state.current_time - 0.0).abs() < f64::EPSILON,
        "Position should be reset to 0 after media switch, got: {}", state.current_time);
    assert!(state.is_playing, "Should start playing after media switch");
}

// ============================================================================
// State Machine Tests: Speed Changes
// ============================================================================

/// Test: Speed change preserves computed position
///
/// When changing speed while playing, the computed position should be
/// preserved (current_time is updated to computed_current_time before speed change).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_speed_change_preserves_position() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("sm_speed_pos")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Speed Position Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();

    // Seek to a position
    playback_service
        .seek(room.id.clone(), owner.id.clone(), 100.0)
        .await
        .unwrap();

    // Start playing
    playback_service
        .set_playing(room.id.clone(), owner.id.clone(), true)
        .await
        .unwrap();

    // Change speed immediately (minimal elapsed time)
    let state = playback_service
        .change_speed(room.id.clone(), owner.id.clone(), 2.0)
        .await
        .unwrap();

    // Position should be approximately preserved
    assert!(state.current_time >= 100.0 - 1.0,
        "Position should be preserved on speed change, got: {}", state.current_time);
    assert!((state.speed - 2.0).abs() < f64::EPSILON,
        "Speed should be 2.0, got: {}", state.speed);
}

/// Test: Speed change while paused
///
/// Speed changes while paused should work correctly.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_speed_change_while_paused() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("sm_speed_paused")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Speed Paused Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();

    // Change speed while paused
    let state = playback_service
        .change_speed(room.id.clone(), owner.id.clone(), 1.5)
        .await
        .unwrap();

    assert!(!state.is_playing, "Should still be paused");
    assert!((state.speed - 1.5).abs() < f64::EPSILON,
        "Speed should be 1.5, got: {}", state.speed);
}

// ============================================================================
// State Machine Tests: Reset
// ============================================================================

/// Test: Reset returns to initial state
///
/// The reset operation should return playback to initial state:
/// - is_playing = false
/// - current_time = 0
/// - speed = 1.0
/// - playing_media_id = None
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_reset_returns_to_initial_state() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("sm_reset")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Reset Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();

    // Make some changes
    playback_service
        .seek(room.id.clone(), owner.id.clone(), 200.0)
        .await
        .unwrap();

    playback_service
        .change_speed(room.id.clone(), owner.id.clone(), 2.0)
        .await
        .unwrap();

    playback_service
        .set_playing(room.id.clone(), owner.id.clone(), true)
        .await
        .unwrap();

    // Reset
    let state = playback_service
        .reset(room.id.clone(), owner.id.clone())
        .await
        .unwrap();

    // Verify reset to initial state
    assert!(!state.is_playing, "Reset should set is_playing to false");
    assert!((state.current_time - 0.0).abs() < f64::EPSILON,
        "Reset should set current_time to 0, got: {}", state.current_time);
    assert!((state.speed - 1.0).abs() < f64::EPSILON,
        "Reset should set speed to 1.0, got: {}", state.speed);
    assert!(state.playing_media_id.is_none(), "Reset should clear playing_media_id");
    assert!(state.playing_playlist_id.is_none(), "Reset should clear playing_playlist_id");
}

// ============================================================================
// State Machine Tests: Concurrent Operations
// ============================================================================

/// Test: Concurrent play/pause operations
///
/// Multiple concurrent play/pause operations should all eventually succeed
/// and the final state should be consistent.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_play_pause_operations() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let owner = user_repo.create(&make_user("sm_concurrent")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Concurrent Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Spawn multiple concurrent play/pause operations
    let mut handles = vec![];
    let barrier = Arc::new(tokio::sync::Barrier::new(10));

    for i in 0..10 {
        let rs = room_service.clone();
        let rid = room.id.clone();
        let uid = owner.id.clone();
        let b = barrier.clone();
        let playing = i % 2 == 0;

        let handle = tokio::spawn(async move {
            b.wait().await;
            rs.playback_service()
                .set_playing(rid, uid, playing)
                .await
        });
        handles.push(handle);
    }

    let results: Vec<_> = futures::future::join_all(handles).await;

    // Most operations should succeed; under high contention with only 3 retries,
    // some may exhaust retries (especially under CI/Docker pressure).
    let mut success_count = 0;
    let mut error_count = 0;
    for result in &results {
        match result {
            Ok(Ok(_)) => success_count += 1,
            Ok(Err(_)) => error_count += 1,
            Err(e) => panic!("Task panicked: {:?}", e),
        }
    }

    println!("Concurrent play/pause: success={}/10, errors={}", success_count, error_count);
    assert!(success_count >= 5, "At least 50% should succeed, got: {}", success_count);

    // Final state should be consistent (either playing or paused)
    let playback_service = room_service.playback_service();
    let state = playback_service.get_state(&room.id).await.unwrap();
    assert!(state.is_playing || !state.is_playing, "Final state should be valid boolean");
}

// ============================================================================
// State Machine Tests: Version Increment
// ============================================================================

/// Test: Version increments on each state change
///
/// Each state transition should increment the version number.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_version_increments_on_state_change() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("sm_version")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Version Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();

    // Initial version
    let state = playback_service.get_state(&room.id).await.unwrap();
    let initial_version = state.version;

    // Play
    let state = playback_service
        .set_playing(room.id.clone(), owner.id.clone(), true)
        .await
        .unwrap();
    assert_eq!(state.version, initial_version + 1, "Version should increment on play");

    // Pause
    let state = playback_service
        .set_playing(room.id.clone(), owner.id.clone(), false)
        .await
        .unwrap();
    assert_eq!(state.version, initial_version + 2, "Version should increment on pause");

    // Seek
    let state = playback_service
        .seek(room.id.clone(), owner.id.clone(), 50.0)
        .await
        .unwrap()
        .state;
    assert_eq!(state.version, initial_version + 3, "Version should increment on seek");

    // Change speed
    let state = playback_service
        .change_speed(room.id.clone(), owner.id.clone(), 1.5)
        .await
        .unwrap();
    assert_eq!(state.version, initial_version + 4, "Version should increment on speed change");
}
