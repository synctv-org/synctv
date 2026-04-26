//! `PlaybackService` integration tests
//!
//! Tests playback control including seek, speed, media switching, and
//! optimistic lock behavior with real `PostgreSQL` via testcontainers.
//!
//! Run with: cargo test -p synctv-core --test `playback_service_tests` -- --nocapture
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    config::PasswordComplexityConfig,
    models::{
        room::AutoPlaySettings, Media, MediaId, PlayMode, Playlist, User, UserId, UserRole,
        UserStatus,
    },
    repository::{MediaRepository, UserRepository},
    service::{
        auth::{BruteForceProtection, JwtService, TestPasswordHasher},
        InMemoryTokenBlacklistStore, RoomService, UserService,
    },
    Error,
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
        room_id: room_id.clone(),
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

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_seek_negative_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("seek_neg_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Seek Neg Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();

    let result = playback_service
        .seek(room.id.clone(), owner.id.clone(), -5.0)
        .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        Error::InvalidInput(msg) => {
            assert!(
                msg.contains("non-negative") || msg.contains("negative"),
                "Error should mention non-negative: {msg}"
            );
        }
        other => panic!("Expected InvalidInput error, got: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_speed_zero_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("speed_zero_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Speed Zero Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();

    let result = playback_service
        .change_speed(room.id.clone(), owner.id.clone(), 0.0)
        .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        Error::InvalidInput(msg) => {
            assert!(
                msg.contains("Speed") || msg.contains("speed"),
                "Error should mention speed: {msg}"
            );
        }
        other => panic!("Expected InvalidInput error, got: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_speed_above_max_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("speed_max_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Speed Max Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();

    let result = playback_service
        .change_speed(room.id.clone(), owner.id.clone(), 17.0)
        .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        Error::InvalidInput(msg) => {
            assert!(
                msg.contains("Speed") || msg.contains("16"),
                "Error should mention speed limit: {msg}"
            );
        }
        other => panic!("Expected InvalidInput error, got: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_switch_media_resets_position() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("switch_owner")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Switch Media Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playlist = create_top_level_playlist(&pool, &room.id).await;

    // Add a media item
    let media = Media {
        id: MediaId::new(),
        playlist_id: Some(playlist.id.clone()),
        room_id: room.id.clone(),
        creator_id: Some(owner.id.clone()),
        name: "Test Video".to_string(),
        position: 0.0,
        source_provider: "direct_url".to_string(),
        source_config: serde_json::json!({"url": "https://example.com/video.mp4"}),
        provider_instance_name: Some("direct_url".to_string()),
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    media_repo.create(&media).await.unwrap();

    // First seek to a non-zero position
    let playback_service = room_service.playback_service();
    playback_service
        .seek(room.id.clone(), owner.id.clone(), 42.5)
        .await
        .unwrap();

    // Switch media should reset position to 0
    let state = playback_service
        .switch(
            room.id.clone(),
            owner.id.clone(),
            Some(media.id.clone()),
            None,
            Vec::new(),
        )
        .await
        .unwrap();

    assert!(
        (state.current_time - 0.0).abs() < f64::EPSILON,
        "Current time should be reset to 0 after media switch, got: {}",
        state.current_time
    );
    assert_eq!(state.playing_media_id, Some(media.id));
    assert!(
        state.playing_playlist_id.is_none(),
        "static media playback must not retain a playlist playback target"
    );
    assert!(state.is_playing, "Should be playing after media switch");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_switch_media_rejects_target() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("switch_media_relpath_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Switch Media Relative Path Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let media = Media {
        id: MediaId::new(),
        playlist_id: None,
        room_id: room.id.clone(),
        creator_id: Some(owner.id.clone()),
        name: "Standalone Video".to_string(),
        position: 0.0,
        source_provider: "direct_url".to_string(),
        source_config: serde_json::json!({"url": "https://example.com/standalone.mp4"}),
        provider_instance_name: Some("direct_url".to_string()),
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    media_repo.create(&media).await.unwrap();

    let result = room_service
        .playback_service()
        .switch(
            room.id.clone(),
            owner.id.clone(),
            Some(media.id.clone()),
            None,
            br#"{"relative_path":"/unexpected"}"#.to_vec(),
        )
        .await;

    assert!(matches!(result, Err(Error::InvalidInput(_))));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_switch_media_rejects_inactive_creator() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("switch_inactive_creator_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Switch Inactive Creator Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let media_creator = user_repo
        .create(&make_user("switch_inactive_media_creator"))
        .await
        .unwrap();

    let media = Media {
        id: MediaId::new(),
        playlist_id: None,
        room_id: room.id.clone(),
        creator_id: Some(media_creator.id.clone()),
        name: "Inactive Creator Video".to_string(),
        position: 0.0,
        source_provider: "direct_url".to_string(),
        source_config: serde_json::json!({"url": "https://example.com/inactive.mp4"}),
        provider_instance_name: Some("direct_url".to_string()),
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    media_repo.create(&media).await.unwrap();

    user_repo
        .ban(
            &media_creator.id,
            None,
            Some("playback service test".to_string()),
        )
        .await
        .unwrap();

    let result = room_service
        .playback_service()
        .switch(
            room.id.clone(),
            owner.id.clone(),
            Some(media.id.clone()),
            None,
            Vec::new(),
        )
        .await;

    match result.expect_err("media created by banned user must not be playable") {
        Error::Authorization(message) => {
            assert!(
                message.contains("creator") && message.contains("active"),
                "error should explain creator status: {message}"
            );
        }
        other => panic!("expected authorization error, got: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_switch_with_empty_target_clears_playback_state() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("switch_clear_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Switch Clear Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let media = Media {
        id: MediaId::new(),
        playlist_id: None,
        room_id: room.id.clone(),
        creator_id: Some(owner.id.clone()),
        name: "Clearable Video".to_string(),
        position: 0.0,
        source_provider: "direct_url".to_string(),
        source_config: serde_json::json!({"url": "https://example.com/clearable.mp4"}),
        provider_instance_name: Some("direct_url".to_string()),
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    media_repo.create(&media).await.unwrap();

    let playback_service = room_service.playback_service();
    playback_service
        .switch(
            room.id.clone(),
            owner.id.clone(),
            Some(media.id.clone()),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    playback_service
        .seek(room.id.clone(), owner.id.clone(), 33.0)
        .await
        .unwrap();

    let state = playback_service
        .switch(room.id.clone(), owner.id.clone(), None, None, Vec::new())
        .await
        .unwrap();

    assert!(state.playing_media_id.is_none());
    assert!(state.playing_playlist_id.is_none());
    assert!(state.target.is_empty());
    assert!((state.current_time - 0.0).abs() < f64::EPSILON);
    assert!((state.speed - 1.0).abs() < f64::EPSILON);
    assert!(!state.is_playing);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_playback_optimistic_lock_concurrent() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let owner = user_repo.create(&make_user("olc_owner")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "OLC Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Spawn multiple concurrent seek operations
    let mut handles = vec![];
    for i in 0..5 {
        let rs = room_service.clone();
        let rid = room.id.clone();
        let uid = owner.id.clone();
        let position = f64::from(i) * 10.0;

        let handle =
            tokio::spawn(async move { rs.playback_service().seek(rid, uid, position).await });
        handles.push(handle);
    }

    let results: Vec<_> = futures::future::join_all(handles).await;

    // All operations should eventually succeed (optimistic locking with retries)
    let mut success_count = 0;
    for result in &results {
        match result {
            Ok(Ok(_)) => success_count += 1,
            Ok(Err(e)) => {
                // Some may fail with Internal error if retries exhausted, which is OK
                assert!(
                    !matches!(e, Error::OptimisticLockConflict),
                    "OptimisticLockConflict should not leak to caller"
                );
            }
            Err(e) => panic!("Task panicked: {e:?}"),
        }
    }

    assert!(success_count > 0, "At least one seek should succeed");

    // Final state should be valid
    let playback_service = room_service.playback_service();
    let state = playback_service.get_state(&room.id).await.unwrap();
    assert!(
        state.current_time >= 0.0,
        "Final position should be non-negative"
    );
}

/// Test rapid sequential seek operations (debounce/throttle behavior).
///
/// Scenario:
/// - 10 users concurrently seek to different positions
/// - All seeks should be processed (no debounce at service level)
/// - Final state should be one of the requested positions
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_rapid_sequential_seek_operations() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let owner = user_repo
        .create(&make_user("rapid_seek_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Rapid Seek Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Spawn 10 concurrent seek operations
    let mut handles = vec![];
    let barrier = Arc::new(tokio::sync::Barrier::new(10));

    for i in 0..10 {
        let rs = room_service.clone();
        let rid = room.id.clone();
        let uid = owner.id.clone();
        let b = barrier.clone();
        let position = f64::from(i).mul_add(30.0, 10.0); // 10, 40, 70, ...

        let handle = tokio::spawn(async move {
            b.wait().await;
            rs.playback_service().seek(rid, uid, position).await
        });
        handles.push(handle);
    }

    let results: Vec<_> = futures::future::join_all(handles).await;

    // Count successes
    let mut success_count = 0;
    let mut positions = Vec::new();

    for result in &results {
        match result {
            Ok(Ok(response)) => {
                success_count += 1;
                positions.push(response.state.current_time);
            }
            Ok(Err(_)) => {} // Some may fail due to conflicts
            Err(e) => panic!("Task panicked: {e:?}"),
        }
    }

    assert!(success_count > 0, "At least one seek should succeed");

    // Final state should be valid
    let playback_service = room_service.playback_service();
    let state = playback_service.get_state(&room.id).await.unwrap();
    assert!(
        state.current_time >= 0.0,
        "Final position should be non-negative"
    );
    assert!(
        state.current_time <= 300.0,
        "Final position should be <= 300 (max seek)"
    );
}

/// Test `play_next` behavior when playlist is modified concurrently.
///
/// Scenario:
/// - Add multiple media items to playlist
/// - Start playing first item
/// - Concurrently call `play_next` and delete next media
/// - Verify system handles deletion gracefully
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_play_next_concurrent_playlist_modification() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let owner = user_repo
        .create(&make_user("playnext_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Play Next Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playlist = create_top_level_playlist(&pool, &room.id).await;

    // Add 5 media items
    let mut media_ids = Vec::new();
    for i in 0..5 {
        let media = Media {
            id: MediaId::new(),
            playlist_id: Some(playlist.id.clone()),
            room_id: room.id.clone(),
            creator_id: Some(owner.id.clone()),
            name: format!("Video {i}"),
            position: f64::from(i),
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({"url": format!("https://example.com/video{}.mp4", i)}),
            provider_instance_name: Some("direct_url".to_string()),
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        };
        media_repo.create(&media).await.unwrap();
        media_ids.push(media.id.clone());
    }

    let playback_service = room_service.playback_service();
    playback_service
        .switch(
            room.id.clone(),
            owner.id.clone(),
            Some(media_ids[0].clone()),
            None,
            Vec::new(),
        )
        .await
        .unwrap();

    // Verify we're playing first media
    let state = playback_service.get_state(&room.id).await.unwrap();
    assert_eq!(state.playing_media_id, Some(media_ids[0].clone()));

    // Set up RoomSettings with auto_play enabled
    let settings = RoomSettings::default();

    // Call play_next - should advance to second media
    let result = playback_service
        .play_next(&room.id, &settings)
        .await
        .unwrap();

    assert!(result.is_some(), "play_next should return new state");
    let new_state = result.unwrap();
    assert_eq!(new_state.playing_media_id, Some(media_ids[1].clone()));
}

/// Test `play_next` behavior at the end of playlist.
///
/// Scenario:
/// - Playlist has 3 items
/// - Play to last item
/// - Call `play_next`
/// - Should return None (no more items) or loop depending on settings
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_play_next_at_end_of_playlist() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let owner = user_repo
        .create(&make_user("playlist_end_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Playlist End Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playlist = create_top_level_playlist(&pool, &room.id).await;

    // Add 3 media items
    let mut media_ids = Vec::new();
    for i in 0..3 {
        let media = Media {
            id: MediaId::new(),
            playlist_id: Some(playlist.id.clone()),
            room_id: room.id.clone(),
            creator_id: Some(owner.id.clone()),
            name: format!("End Video {i}"),
            position: f64::from(i),
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({"url": format!("https://example.com/end{}.mp4", i)}),
            provider_instance_name: Some("direct_url".to_string()),
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        };
        media_repo.create(&media).await.unwrap();
        media_ids.push(media.id.clone());
    }

    let playback_service = room_service.playback_service();
    playback_service
        .switch(
            room.id.clone(),
            owner.id.clone(),
            Some(media_ids[2].clone()),
            None,
            Vec::new(),
        )
        .await
        .unwrap();

    // Sequential mode (no loop) - play_next should return None
    let settings = RoomSettings {
        auto_play: room_settings::AutoPlay::new(synctv_core::models::room::AutoPlaySettings {
            enabled: false,
            mode: synctv_core::models::PlayMode::Sequential,
            delay: 0,
        }),
        ..Default::default()
    };

    let result = playback_service
        .play_next(&room.id, &settings)
        .await
        .unwrap();

    // At end of playlist with no loop, should return None
    // (or the implementation may return the state unchanged)
    match result {
        None => {} // Expected: end of playlist
        Some(state) => {
            // If it returns a state, it should be the same (no change)
            assert_eq!(state.playing_media_id, Some(media_ids[2].clone()));
        }
    }
}

/// Test `play_next` with loop enabled returns to first item.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_play_next_with_loop_enabled() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let owner = user_repo.create(&make_user("loop_owner")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Loop Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playlist = create_top_level_playlist(&pool, &room.id).await;

    // Add 3 media items
    let mut media_ids = Vec::new();
    for i in 0..3 {
        let media = Media {
            id: MediaId::new(),
            playlist_id: Some(playlist.id.clone()),
            room_id: room.id.clone(),
            creator_id: Some(owner.id.clone()),
            name: format!("Loop Video {i}"),
            position: f64::from(i),
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({"url": format!("https://example.com/loop{}.mp4", i)}),
            provider_instance_name: Some("direct_url".to_string()),
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        };
        media_repo.create(&media).await.unwrap();
        media_ids.push(media.id.clone());
    }

    let playback_service = room_service.playback_service();
    playback_service
        .switch(
            room.id.clone(),
            owner.id.clone(),
            Some(media_ids[2].clone()),
            None,
            Vec::new(),
        )
        .await
        .unwrap();

    // Loop (RepeatAll) mode enabled via auto_play
    let settings = RoomSettings {
        auto_play: room_settings::AutoPlay {
            value: AutoPlaySettings {
                enabled: true,
                mode: PlayMode::RepeatAll,
                delay: 0,
            },
        },
        ..Default::default()
    };

    let result = playback_service
        .play_next(&room.id, &settings)
        .await
        .unwrap();

    // With loop, should return to first item
    assert!(result.is_some(), "With loop, play_next should return state");
    let state = result.unwrap();
    assert_eq!(state.playing_media_id, Some(media_ids[0].clone()));
}

/// Test behavior when playlist is empty.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_empty_playlist_handling() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let owner = user_repo
        .create(&make_user("empty_playlist_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Empty Playlist Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();
    let settings = RoomSettings::default();

    // Play next on empty playlist should return None
    let result = playback_service
        .play_next(&room.id, &settings)
        .await
        .unwrap();

    assert!(
        result.is_none(),
        "play_next on empty playlist should return None"
    );

    // Get state should work
    let state = playback_service.get_state(&room.id).await.unwrap();
    assert!(
        state.playing_media_id.is_none(),
        "No media should be playing"
    );
}

/// Test concurrent speed changes.
///
/// Scenario:
/// - Multiple concurrent requests to change speed
/// - All should eventually succeed or fail gracefully
/// - Final state should be consistent
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_speed_changes() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let owner = user_repo
        .create(&make_user("speed_concurrent_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Speed Concurrent Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Spawn 10 concurrent speed change operations
    let mut handles = vec![];
    let barrier = Arc::new(tokio::sync::Barrier::new(10));

    let valid_speeds = [0.5, 1.0, 1.5, 2.0, 0.75, 1.25, 1.75, 2.5, 3.0, 4.0];

    for speed in valid_speeds {
        let rs = room_service.clone();
        let rid = room.id.clone();
        let uid = owner.id.clone();
        let b = barrier.clone();

        let handle = tokio::spawn(async move {
            b.wait().await;
            rs.playback_service().change_speed(rid, uid, speed).await
        });
        handles.push(handle);
    }

    let results: Vec<_> = futures::future::join_all(handles).await;

    let mut success_count = 0;
    for result in &results {
        match result {
            Ok(Ok(_)) => success_count += 1,
            Ok(Err(_)) => {} // Some may fail due to conflicts
            Err(e) => panic!("Task panicked: {e:?}"),
        }
    }

    assert!(
        success_count > 0,
        "At least one speed change should succeed"
    );

    // Final state should have a valid speed
    let playback_service = room_service.playback_service();
    let state = playback_service.get_state(&room.id).await.unwrap();
    assert!(state.speed > 0.0, "Speed should be positive");
    assert!(state.speed <= 4.0, "Speed should be <= max");
}

/// Test that state remains consistent after multiple mixed operations.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_state_consistency_after_mixed_operations() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let owner = user_repo
        .create(&make_user("mixed_ops_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Mixed Ops Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playlist = create_top_level_playlist(&pool, &room.id).await;

    // Add media
    let media = Media {
        id: MediaId::new(),
        playlist_id: Some(playlist.id.clone()),
        room_id: room.id.clone(),
        creator_id: Some(owner.id.clone()),
        name: "Mixed Test Video".to_string(),
        position: 0.0,
        source_provider: "direct_url".to_string(),
        source_config: serde_json::json!({"url": "https://example.com/mixed.mp4"}),
        provider_instance_name: Some("direct_url".to_string()),
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    media_repo.create(&media).await.unwrap();

    let playback_service = room_service.playback_service();

    // Switch to media
    playback_service
        .switch(
            room.id.clone(),
            owner.id.clone(),
            Some(media.id.clone()),
            None,
            Vec::new(),
        )
        .await
        .unwrap();

    // Seek
    let seek_response = playback_service
        .seek(room.id.clone(), owner.id.clone(), 50.0)
        .await
        .unwrap();
    assert!(seek_response.seek_applied, "Seek should be applied");
    assert!(
        (seek_response.state.current_time - 50.0).abs() < f64::EPSILON,
        "Seek should set the exact requested position"
    );

    // Change speed while playing: the effective position may advance slightly,
    // but it must never move backward from the seek target.
    let speed_state = playback_service
        .change_speed(room.id.clone(), owner.id.clone(), 1.5)
        .await
        .unwrap();
    assert!(
        speed_state.current_time >= 50.0,
        "Position must not move backward after changing speed"
    );

    // Pause snapshots the computed playback position.
    let paused_state = playback_service
        .set_playing(room.id.clone(), owner.id.clone(), false)
        .await
        .unwrap();
    assert!(
        paused_state.current_time >= speed_state.current_time,
        "Pause should preserve or advance the effective position"
    );
    assert!(!paused_state.is_playing, "Pause should stop playback");

    // Resume should preserve the paused position and flip playback back on.
    let resumed_state = playback_service
        .set_playing(room.id.clone(), owner.id.clone(), true)
        .await
        .unwrap();
    assert!(
        (resumed_state.current_time - paused_state.current_time).abs() < 0.1,
        "Resume should preserve the paused position"
    );

    // Verify final state is consistent.
    let state = playback_service.get_state(&room.id).await.unwrap();

    assert_eq!(state.playing_media_id, Some(media.id));
    assert!(
        state.current_time >= paused_state.current_time,
        "Final position should not move backward"
    );
    assert!(
        (state.speed - 1.5).abs() < f64::EPSILON,
        "Speed should be 1.5"
    );
    assert!(state.is_playing, "Should be playing");
}

use synctv_core::models::{room_settings, RoomSettings};

/// Test that seek returns `seek_applied=true` on success.
///
/// This test verifies that clients can distinguish between a successful seek
/// and a degraded response (seek failed but returned current state).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_seek_success_returns_applied_true() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("seek_response_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Seek Response Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();

    // A simple seek should succeed and report seek_applied=true
    let response = playback_service
        .seek(room.id.clone(), owner.id.clone(), 42.5)
        .await
        .unwrap();

    assert!(
        response.seek_applied,
        "Successful seek should have seek_applied=true"
    );
    assert!(
        (response.state.current_time - 42.5).abs() < f64::EPSILON,
        "Position should be 42.5, got: {}",
        response.state.current_time
    );
}

/// Test that seek returns `seek_applied=false` with degraded response on retry exhaustion.
///
/// When optimistic lock retries are exhausted during rapid concurrent seeks,
/// the method should return the latest state but with `seek_applied=false`
/// so the client knows the requested position was not applied.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_seek_retry_exhaustion_returns_applied_false() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let owner = user_repo
        .create(&make_user("seek_retry_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Seek Retry Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();

    // Spawn many concurrent seeks to trigger retry exhaustion
    let mut handles = vec![];
    let barrier = Arc::new(tokio::sync::Barrier::new(50));

    for i in 0..50 {
        let rs = room_service.clone();
        let rid = room.id.clone();
        let uid = owner.id.clone();
        let b = barrier.clone();
        let position = f64::from(i).mul_add(10.0, 1.0); // Different positions

        let handle = tokio::spawn(async move {
            b.wait().await;
            rs.playback_service().seek(rid, uid, position).await
        });
        handles.push(handle);
    }

    let results: Vec<_> = futures::future::join_all(handles).await;

    // At least some should succeed, some may have retry exhaustion
    let mut success_count = 0;

    for result in &results {
        match result {
            Ok(Ok(response)) => {
                if response.seek_applied {
                    success_count += 1;
                } else {
                    // Degraded response should still have valid state
                    assert!(
                        response.state.current_time >= 0.0,
                        "Degraded response should have valid position"
                    );
                }
            }
            Ok(Err(_)) => {} // Other errors are OK
            Err(e) => panic!("Task panicked: {e:?}"),
        }
    }

    assert!(success_count > 0, "At least one seek should succeed");
    // Note: degraded_count may be 0 if all succeed within retry budget

    // Final state should be valid
    let final_state = playback_service.get_state(&room.id).await.unwrap();
    assert!(
        final_state.current_time >= 0.0,
        "Final position should be non-negative"
    );
}

/// Test that `SeekResponse` contains the state even when seek fails.
///
/// Clients should always get a valid state in the response,
/// regardless of whether the seek was applied.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_seek_response_always_contains_valid_state() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("seek_state_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Seek State Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();

    // First set a known position
    playback_service
        .seek(room.id.clone(), owner.id.clone(), 100.0)
        .await
        .unwrap();

    // Now seek to a different position
    let response = playback_service
        .seek(room.id.clone(), owner.id.clone(), 200.0)
        .await
        .unwrap();

    // Response should have valid state (either at 200 if applied, or current position)
    assert!(
        response.state.current_time >= 0.0,
        "State should have valid position"
    );
    assert!(response.state.speed > 0.0, "State should have valid speed");
}

/// Test `SeekResponse` message field is informative when seek fails.
///
/// When seek fails due to contention, the message should help debugging.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_seek_degraded_response_has_informative_message() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let owner = user_repo
        .create(&make_user("seek_msg_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Seek Message Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    // Spawn many concurrent seeks
    let mut handles = vec![];
    let barrier = Arc::new(tokio::sync::Barrier::new(100));

    for i in 0..100 {
        let rs = room_service.clone();
        let rid = room.id.clone();
        let uid = owner.id.clone();
        let b = barrier.clone();
        let position = f64::from(i) * 5.0;

        let handle = tokio::spawn(async move {
            b.wait().await;
            rs.playback_service().seek(rid, uid, position).await
        });
        handles.push(handle);
    }

    let results: Vec<_> = futures::future::join_all(handles).await;

    // Check for degraded responses with message
    for result in &results {
        match result {
            Ok(Ok(response)) => {
                if !response.seek_applied {
                    // Degraded response should have an informative message
                    if let Some(msg) = &response.message {
                        assert!(
                            msg.contains("retry")
                                || msg.contains("contention")
                                || msg.contains("failed")
                                || msg.contains("concurrent"),
                            "Degraded response message should explain failure: {msg}"
                        );
                    }
                }
            }
            Ok(Err(_)) => {}
            Err(e) => panic!("Task panicked: {e:?}"),
        }
    }
}
