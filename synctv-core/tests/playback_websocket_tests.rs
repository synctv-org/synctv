//! Playback WebSocket notification integration tests
//!
//! Tests for WebSocket notifications on playback state changes,
//! multi-user synchronization, and cluster broadcast behavior.
//!
//! Note: This tests the notification/broadcast layer, not actual WebSocket connections.
//! The `PlaybackService` uses `PlaybackBroadcaster` trait for abstraction.
//!
//! Run with: cargo test -p synctv-core --test `playback_websocket_tests` -- --nocapture
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use chrono::Utc;
use parking_lot::RwLock;
use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    config::PasswordComplexityConfig,
    models::{Playlist, RoomPlaybackState, User, UserId, UserRole, UserStatus},
    repository::UserRepository,
    service::{
        auth::{BruteForceProtection, JwtService, TestPasswordHasher},
        playback::{BroadcastResult, PlaybackBroadcaster},
        InMemoryTokenBlacklistStore, RoomService, UserService,
    },
};
use synctv_core_testing::create_test_pool;
fn make_user_service(pool: PgPool) -> UserService {
    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = JwtService::new(secret).expect("Failed to create JwtService");
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let password_complexity = PasswordComplexityConfig::default();
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    let mut svc = UserService::new(
        pool,
        jwt_service,
        username_cache,
        password_complexity,
        token_blacklist,
        key_builder,
        brute_force,
    );
    svc.set_password_hasher(Arc::new(TestPasswordHasher::new()));
    svc
}

fn make_room_service(pool: PgPool) -> RoomService {
    let user_service = make_user_service(pool.clone());
    let mut svc = RoomService::new(pool, user_service);
    svc.set_password_hasher(Arc::new(TestPasswordHasher::new()));
    svc
}

fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{username}@test.com")),
        password_hash: "hash".to_string(),
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: synctv_core::models::SignupMethod::Email,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    }
}

async fn create_top_level_playlist(
    pool: &PgPool,
    room_id: &synctv_core::models::RoomId,
) -> Playlist {
    let playlist = Playlist {
        id: synctv_core::models::PlaylistId::new(),
        room_id: *room_id,
        creator_id: None,
        name: "Top Level".to_string(),
        parent_id: None,
        position: 0.0,
        source_provider: None,
        source_config: None,
        provider_instance_name: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };

    synctv_core::repository::PlaylistRepository::new(pool.clone())
        .create(&playlist)
        .await
        .expect("Top-level playlist should be created")
}

// Mock Broadcaster for Testing

/// Mock broadcaster that records all broadcast calls
#[derive(Debug, Default)]
struct MockBroadcaster {
    broadcasts: Arc<RwLock<Vec<RoomPlaybackState>>>,
    fail_broadcasts: Arc<RwLock<bool>>,
}

impl MockBroadcaster {
    fn new() -> Self {
        Self::default()
    }

    fn get_broadcasts(&self) -> Vec<RoomPlaybackState> {
        self.broadcasts.read().clone()
    }

    fn clear_broadcasts(&self) {
        self.broadcasts.write().clear();
    }

    fn set_fail(&self, fail: bool) {
        *self.fail_broadcasts.write() = fail;
    }

    fn broadcast_count(&self) -> usize {
        self.broadcasts.read().len()
    }
}

impl PlaybackBroadcaster for MockBroadcaster {
    fn broadcast_playback_state(&self, state: &RoomPlaybackState) -> BroadcastResult {
        if *self.fail_broadcasts.read() {
            return BroadcastResult::default();
        }

        self.broadcasts.write().push(state.clone());

        BroadcastResult {
            local_sent: 1,
            redis_sent: true,
            single_node: false,
        }
    }
}

// WebSocket Push Tests: State Change Notifications

/// Test: Play/pause triggers broadcast
///
/// When play/pause is called, a broadcast should be sent.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_play_pause_triggers_broadcast() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("ws_play")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "WS Play Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    // Set up mock broadcaster
    let mock_broadcaster = Arc::new(MockBroadcaster::new());
    room_service
        .playback_service()
        .set_cluster_broadcaster(mock_broadcaster.clone());

    // Trigger play
    room_service
        .playback_service()
        .set_playing(room.id, owner.id, true)
        .await
        .unwrap();

    // Should have broadcast
    assert_eq!(
        mock_broadcaster.broadcast_count(),
        1,
        "Should have one broadcast"
    );
    let broadcast = &mock_broadcaster.get_broadcasts()[0];
    assert!(
        broadcast.is_playing,
        "Broadcast should show is_playing = true"
    );

    mock_broadcaster.clear_broadcasts();

    // Trigger pause
    room_service
        .playback_service()
        .set_playing(room.id, owner.id, false)
        .await
        .unwrap();

    assert_eq!(
        mock_broadcaster.broadcast_count(),
        1,
        "Should have one broadcast"
    );
    let broadcast = &mock_broadcaster.get_broadcasts()[0];
    assert!(
        !broadcast.is_playing,
        "Broadcast should show is_playing = false"
    );
}

/// Test: Seek triggers broadcast
///
/// When seek is called, a broadcast should be sent.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_seek_triggers_broadcast() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("ws_seek")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "WS Seek Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    // Set up mock broadcaster
    let mock_broadcaster = Arc::new(MockBroadcaster::new());
    room_service
        .playback_service()
        .set_cluster_broadcaster(mock_broadcaster.clone());

    // Trigger seek
    room_service
        .playback_service()
        .seek(room.id, owner.id, 120.5)
        .await
        .unwrap();

    // Should have broadcast
    assert_eq!(
        mock_broadcaster.broadcast_count(),
        1,
        "Should have one broadcast"
    );
    let broadcast = &mock_broadcaster.get_broadcasts()[0];
    assert!(
        (broadcast.current_time - 120.5).abs() < f64::EPSILON,
        "Broadcast should show correct position"
    );
}

/// Test: Speed change triggers broadcast
///
/// When speed is changed, a broadcast should be sent.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_speed_change_triggers_broadcast() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("ws_speed")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "WS Speed Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    // Set up mock broadcaster
    let mock_broadcaster = Arc::new(MockBroadcaster::new());
    room_service
        .playback_service()
        .set_cluster_broadcaster(mock_broadcaster.clone());

    // Trigger speed change
    room_service
        .playback_service()
        .change_speed(room.id, owner.id, 2.0)
        .await
        .unwrap();

    // Should have broadcast
    assert_eq!(
        mock_broadcaster.broadcast_count(),
        1,
        "Should have one broadcast"
    );
    let broadcast = &mock_broadcaster.get_broadcasts()[0];
    assert!(
        (broadcast.speed - 2.0).abs() < f64::EPSILON,
        "Broadcast should show correct speed"
    );
}

/// Test: Media switch triggers broadcast
///
/// When media is switched, a broadcast should be sent.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_media_switch_triggers_broadcast() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let media_repo = synctv_core::repository::MediaRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("ws_switch")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "WS Switch Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let playlist = create_top_level_playlist(&pool, &room.id).await;

    // Add media
    let now_media = Utc::now();
    let media = synctv_core::models::Media {
        id: synctv_core::models::MediaId::new(),
        playlist_id: Some(playlist.id),
        room_id: room.id,
        creator_id: Some(owner.id),
        name: "Test Video".to_string(),
        position: 0.0,
        source_provider: "direct_url".to_string(),
        source_config: serde_json::json!({"url": "https://example.com/video.mp4"}),
        provider_instance_name: None,
        added_at: now_media,
        updated_at: now_media,
        version: 0,
    };
    media_repo.create(&media).await.unwrap();

    // Set up mock broadcaster
    let mock_broadcaster = Arc::new(MockBroadcaster::new());
    room_service
        .playback_service()
        .set_cluster_broadcaster(mock_broadcaster.clone());

    // Trigger media switch
    room_service
        .playback_service()
        .switch(room.id, owner.id, Some(media.id), None, Vec::new())
        .await
        .unwrap();

    // Should have broadcast
    assert_eq!(
        mock_broadcaster.broadcast_count(),
        1,
        "Should have one broadcast"
    );
    let broadcast = &mock_broadcaster.get_broadcasts()[0];
    assert_eq!(
        broadcast.playing_media_id,
        Some(media.id),
        "Broadcast should show correct media"
    );
    assert!(broadcast.is_playing, "Should be playing after media switch");
}

/// Test: Reset triggers broadcast
///
/// When playback is reset, a broadcast should be sent.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_reset_triggers_broadcast() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("ws_reset")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "WS Reset Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    // Set up mock broadcaster
    let mock_broadcaster = Arc::new(MockBroadcaster::new());
    room_service
        .playback_service()
        .set_cluster_broadcaster(mock_broadcaster.clone());

    // Trigger reset
    room_service
        .playback_service()
        .reset(room.id, owner.id)
        .await
        .unwrap();

    // Should have broadcast
    assert_eq!(
        mock_broadcaster.broadcast_count(),
        1,
        "Should have one broadcast"
    );
    let broadcast = &mock_broadcaster.get_broadcasts()[0];
    assert!(
        !broadcast.is_playing,
        "Broadcast should show is_playing = false"
    );
    assert!(
        (broadcast.current_time - 0.0).abs() < f64::EPSILON,
        "Position should be 0"
    );
    assert!(broadcast.playing_media_id.is_none(), "Media should be None");
}

// WebSocket Push Tests: Multi-User Sync

/// Test: Multiple state changes each trigger broadcast
///
/// Each state change should trigger exactly one broadcast.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_multiple_state_changes_trigger_broadcasts() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("ws_multi")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "WS Multi Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    // Set up mock broadcaster
    let mock_broadcaster = Arc::new(MockBroadcaster::new());
    room_service
        .playback_service()
        .set_cluster_broadcaster(mock_broadcaster.clone());

    // Multiple operations
    room_service
        .playback_service()
        .set_playing(room.id, owner.id, true)
        .await
        .unwrap();

    room_service
        .playback_service()
        .seek(room.id, owner.id, 50.0)
        .await
        .unwrap();

    room_service
        .playback_service()
        .change_speed(room.id, owner.id, 1.5)
        .await
        .unwrap();

    room_service
        .playback_service()
        .set_playing(room.id, owner.id, false)
        .await
        .unwrap();

    // Should have 4 broadcasts
    assert_eq!(
        mock_broadcaster.broadcast_count(),
        4,
        "Should have 4 broadcasts"
    );

    // Check broadcasts are in order
    let broadcasts = mock_broadcaster.get_broadcasts();
    assert!(broadcasts[0].is_playing, "1st: playing");
    assert!(
        (broadcasts[1].current_time - 50.0).abs() < f64::EPSILON,
        "2nd: seek to 50"
    );
    assert!(
        (broadcasts[2].speed - 1.5).abs() < f64::EPSILON,
        "3rd: speed 1.5"
    );
    assert!(!broadcasts[3].is_playing, "4th: paused");
}

/// Test: Broadcast contains correct `room_id`
///
/// Each broadcast should contain the correct `room_id`.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_broadcast_contains_correct_room_id() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("ws_roomid")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "WS RoomID Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    // Set up mock broadcaster
    let mock_broadcaster = Arc::new(MockBroadcaster::new());
    room_service
        .playback_service()
        .set_cluster_broadcaster(mock_broadcaster.clone());

    // Trigger operation
    room_service
        .playback_service()
        .set_playing(room.id, owner.id, true)
        .await
        .unwrap();

    // Check room_id
    let broadcasts = mock_broadcaster.get_broadcasts();
    assert_eq!(
        broadcasts[0].room_id, room.id,
        "Broadcast should have correct room_id"
    );
}

// WebSocket Push Tests: Broadcast Failure Handling

/// Test: Operations succeed even if broadcast fails
///
/// The service should continue to work even if broadcasting fails.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_operations_succeed_when_broadcast_fails() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("ws_fail")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "WS Fail Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    // Set up mock broadcaster that always fails
    let mock_broadcaster = Arc::new(MockBroadcaster::new());
    mock_broadcaster.set_fail(true);
    room_service
        .playback_service()
        .set_cluster_broadcaster(mock_broadcaster.clone());

    // Operations should still succeed
    let result = room_service
        .playback_service()
        .set_playing(room.id, owner.id, true)
        .await;

    assert!(
        result.is_ok(),
        "Operation should succeed even if broadcast fails"
    );

    // Verify state was actually changed
    let state = room_service
        .playback_service()
        .get_state(&room.id)
        .await
        .unwrap();
    assert!(state.is_playing, "State should be updated");
}

/// Test: No broadcaster configured
///
/// Operations should succeed when no broadcaster is configured (single-node mode).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_no_broadcaster_configured() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("ws_no_bc")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "WS No BC Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    // No broadcaster set - should work fine
    let result = room_service
        .playback_service()
        .set_playing(room.id, owner.id, true)
        .await;

    assert!(
        result.is_ok(),
        "Operation should succeed without broadcaster"
    );

    let state = room_service
        .playback_service()
        .get_state(&room.id)
        .await
        .unwrap();
    assert!(state.is_playing, "State should be updated");
}

// WebSocket Push Tests: BroadcastResult

/// Test: `BroadcastResult` `is_success` method
///
/// Test the `BroadcastResult::is_success()` method.
#[test]
fn test_broadcast_result_is_success() {
    // Both local and redis sent
    let result = BroadcastResult {
        local_sent: 5,
        redis_sent: true,
        single_node: false,
    };
    assert!(result.is_success());

    // Only local sent
    let result = BroadcastResult {
        local_sent: 5,
        redis_sent: false,
        single_node: false,
    };
    assert!(result.is_success());

    // Only redis sent
    let result = BroadcastResult {
        local_sent: 0,
        redis_sent: true,
        single_node: false,
    };
    assert!(result.is_success());

    // Neither sent (not single-node) - failure
    let result = BroadcastResult {
        local_sent: 0,
        redis_sent: false,
        single_node: false,
    };
    assert!(!result.is_success());

    // Single-node mode - always success even with no subscribers
    let result = BroadcastResult::single_node();
    assert!(result.is_success());

    // Default (no fields set) - still failure
    let result = BroadcastResult::default();
    assert!(!result.is_success());
}

#[test]
fn test_broadcast_result_warns_missing_redis_delivery_only_in_distributed_mode() {
    let single_node = BroadcastResult::single_node();
    assert!(
        !single_node.should_warn_missing_redis_delivery(),
        "single-node broadcasts must not be reported as missing Redis delivery"
    );

    let local_only_distributed = BroadcastResult {
        local_sent: 2,
        redis_sent: false,
        single_node: false,
    };
    assert!(
        local_only_distributed.should_warn_missing_redis_delivery(),
        "distributed broadcasts that only reach local clients should warn"
    );

    let redis_delivered = BroadcastResult {
        local_sent: 0,
        redis_sent: true,
        single_node: false,
    };
    assert!(
        !redis_delivered.should_warn_missing_redis_delivery(),
        "successful Redis delivery must not warn"
    );

    let total_failure = BroadcastResult::default();
    assert!(
        !total_failure.should_warn_missing_redis_delivery(),
        "complete failures are handled by the separate total-failure warning path"
    );
}

// WebSocket Push Tests: Version in Broadcasts

/// Test: Broadcast contains correct version
///
/// Each broadcast should contain the updated version number.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_broadcast_contains_correct_version() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("ws_ver")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "WS Version Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    // Set up mock broadcaster
    let mock_broadcaster = Arc::new(MockBroadcaster::new());
    room_service
        .playback_service()
        .set_cluster_broadcaster(mock_broadcaster.clone());

    // Initial version
    let state = room_service
        .playback_service()
        .get_state(&room.id)
        .await
        .unwrap();
    let initial_version = state.version;

    // Trigger operation
    room_service
        .playback_service()
        .set_playing(room.id, owner.id, true)
        .await
        .unwrap();

    // Check version in broadcast
    let broadcasts = mock_broadcaster.get_broadcasts();
    assert_eq!(
        broadcasts[0].version,
        initial_version + 1,
        "Broadcast should contain incremented version"
    );
}

// WebSocket Push Tests: Concurrent Broadcasts

/// Test: Concurrent operations produce consistent broadcasts
///
/// Even with concurrent operations, each broadcast should contain
/// consistent state.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_operations_produce_consistent_broadcasts() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let owner = user_repo.create(&make_user("ws_concurrent")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "WS Concurrent Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    // Set up mock broadcaster
    let mock_broadcaster = Arc::new(MockBroadcaster::new());
    room_service
        .playback_service()
        .set_cluster_broadcaster(mock_broadcaster.clone());

    // Spawn concurrent operations
    let mut handles = vec![];
    let barrier = Arc::new(tokio::sync::Barrier::new(5));

    for i in 0..5 {
        let rs = room_service.clone();
        let rid = room.id;
        let uid = owner.id;
        let b = barrier.clone();
        let pos = f64::from(i) * 10.0;

        handles.push(tokio::spawn(async move {
            b.wait().await;
            rs.playback_service().seek(rid, uid, pos).await
        }));
    }

    let results: Vec<_> = futures::future::join_all(handles).await;

    // All should succeed
    let success_count = results
        .iter()
        .filter(|r| r.as_ref().unwrap().is_ok())
        .count();
    assert!(success_count >= 3, "Most operations should succeed");

    // All broadcasts should have valid state
    let broadcasts = mock_broadcaster.get_broadcasts();
    assert!(broadcasts.len() >= 3, "Should have at least 3 broadcasts");

    for (i, broadcast) in broadcasts.iter().enumerate() {
        assert!(
            broadcast.current_time >= 0.0,
            "Broadcast {i} should have valid position"
        );
        assert!(
            broadcast.version > 0 || i == 0,
            "Broadcast {i} should have valid version"
        );
    }
}

// WebSocket Push Tests: Cluster Mode Simulation

/// Test: Simulated cluster broadcast with multiple rooms
///
/// Test that broadcasts for different rooms are independent.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cluster_mode_multiple_rooms() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("ws_cluster")).await.unwrap();

    let (room1, _) = room_service
        .create_room(
            "WS Cluster Room 1".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let (room2, _) = room_service
        .create_room(
            "WS Cluster Room 2".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    // Set up mock broadcaster
    let mock_broadcaster = Arc::new(MockBroadcaster::new());
    room_service
        .playback_service()
        .set_cluster_broadcaster(mock_broadcaster.clone());

    // Update room1
    room_service
        .playback_service()
        .seek(room1.id, owner.id, 100.0)
        .await
        .unwrap();

    // Update room2
    room_service
        .playback_service()
        .seek(room2.id, owner.id, 200.0)
        .await
        .unwrap();

    // Should have 2 broadcasts, one for each room
    assert_eq!(mock_broadcaster.broadcast_count(), 2);

    let broadcasts = mock_broadcaster.get_broadcasts();
    assert_eq!(broadcasts[0].room_id, room1.id);
    assert_eq!(broadcasts[1].room_id, room2.id);

    // Positions should be different
    assert!((broadcasts[0].current_time - 100.0).abs() < f64::EPSILON);
    assert!((broadcasts[1].current_time - 200.0).abs() < f64::EPSILON);
}
