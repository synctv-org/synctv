//! PlaybackService::play_next logic tests
//!
//! Tests the play_next method's playlist navigation logic for each PlayMode,
//! including edge cases like deleted media, empty playlists, and legacy field mapping.
//!
//! These tests exercise the play_next decision logic with a real PostgreSQL
//! via testcontainers, since play_next reads from the DB repo layer.
//!
//! Run with: cargo test -p synctv-core --test playback_play_next_tests -- --nocapture

use std::sync::Arc;

use synctv_core::{
    cache::{KeyBuilder, UsernameCache, NoopCacheL2},
    config::PasswordComplexityConfig,
    models::{
        UserId, User, UserRole, UserStatus,
        Media, MediaId, Playlist, PlaylistId,
        RoomId, RoomSettings, PlayMode,
        room::AutoPlaySettings,
        room_settings::{AutoPlay, AutoPlayNext, LoopPlaylist, ShufflePlaylist},
    },
    repository::{UserRepository, MediaRepository},
    service::{
        RoomService, UserService, InMemoryTokenBlacklistStore,
        auth::{JwtService, BruteForceProtection},
    },
};
use chrono::Utc;
use sqlx::PgPool;
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;

const POSTGRES_VERSION: &str = "16-alpine";

async fn create_test_pool() -> (ContainerAsync<Postgres>, PgPool) {
    let postgres = Postgres::default()
        .with_db_name("synctv_test")
        .with_user("synctv")
        .with_password("synctv_test")
        .with_tag(POSTGRES_VERSION)
        .start()
        .await
        .expect("Failed to start Postgres container");

    let connection_string = format!(
        "postgresql://synctv:synctv_test@127.0.0.1:{}/synctv_test",
        postgres.get_host_port_ipv4(5432).await.expect("Failed to get port")
    );

    let pool = PgPool::connect(&connection_string)
        .await
        .expect("Failed to create pool");

    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    (postgres, pool)
}

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

fn make_settings_with_mode(mode: PlayMode) -> RoomSettings {
    let mut settings = RoomSettings::default();
    settings.auto_play = AutoPlay::new(AutoPlaySettings {
        enabled: true,
        mode,
        delay: 0,
    });
    settings.auto_play_next = AutoPlayNext(true);
    settings
}

/// Helper: get the root playlist for a room
async fn get_root_playlist(pool: &PgPool, room_id: &RoomId) -> Playlist {
    sqlx::query_as::<_, Playlist>("SELECT * FROM playlists WHERE room_id = $1 LIMIT 1")
        .bind(room_id.as_str())
        .fetch_one(pool)
        .await
        .expect("Root playlist should exist")
}

/// Helper: insert a media item into the playlist at a given position
async fn insert_media(pool: &PgPool, playlist_id: &PlaylistId, room_id: &RoomId, name: &str, position: i32) -> Media {
    let media = Media {
        id: MediaId::new(),
        playlist_id: playlist_id.clone(),
        room_id: room_id.clone(),
        creator_id: None,
        name: name.to_string(),
        position,
        source_provider: "direct_url".to_string(),
        source_config: serde_json::json!({"url": format!("https://example.com/{}.mp4", name)}),
        provider_instance_name: None,
        added_at: Utc::now(),
        version: 0,
    };
    let media_repo = MediaRepository::new(pool.clone());
    media_repo.create(&media).await.expect("Failed to create media")
}

// ========== Sequential Mode Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_sequential_advance_to_next() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("seq_next_owner")).await.unwrap();
    let (room, _) = room_service
        .create_room("Seq Next".to_string(), String::new(), owner.id.clone(), None, None)
        .await
        .unwrap();

    let playlist = get_root_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "video1", 0).await;
    let media2 = insert_media(&pool, &playlist.id, &room.id, "video2", 1).await;

    // Set currently playing to media1
    let playback = room_service.playback_service();
    playback.switch_media(room.id.clone(), owner.id.clone(), media1.id.clone()).await.unwrap();

    let settings = make_settings_with_mode(PlayMode::Sequential);
    let result = playback.play_next(&room.id, &settings).await.unwrap();

    assert!(result.is_some(), "Should advance to next media");
    let state = result.unwrap();
    assert_eq!(state.playing_media_id, Some(media2.id), "Should be playing media2");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_sequential_end_of_playlist_returns_none() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("seq_end_owner")).await.unwrap();
    let (room, _) = room_service
        .create_room("Seq End".to_string(), String::new(), owner.id.clone(), None, None)
        .await
        .unwrap();

    let playlist = get_root_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "last_video", 0).await;

    let playback = room_service.playback_service();
    playback.switch_media(room.id.clone(), owner.id.clone(), media1.id.clone()).await.unwrap();

    let settings = make_settings_with_mode(PlayMode::Sequential);
    let result = playback.play_next(&room.id, &settings).await.unwrap();

    assert!(result.is_none(), "Should return None at end of playlist");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_sequential_deleted_current_falls_back_to_first() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("seq_del_owner")).await.unwrap();
    let (room, _) = room_service
        .create_room("Seq Del".to_string(), String::new(), owner.id.clone(), None, None)
        .await
        .unwrap();

    let playlist = get_root_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "video_a", 0).await;
    let _media2 = insert_media(&pool, &playlist.id, &room.id, "video_b", 1).await;

    // Set playing to a non-existent media ID (simulating deletion)
    let playback = room_service.playback_service();
    playback.switch_media(room.id.clone(), owner.id.clone(), media1.id.clone()).await.unwrap();

    // Delete media1 from the database to simulate concurrent deletion
    sqlx::query("DELETE FROM media WHERE id = $1")
        .bind(media1.id.as_str())
        .execute(&pool)
        .await
        .unwrap();

    let settings = make_settings_with_mode(PlayMode::Sequential);
    let result = playback.play_next(&room.id, &settings).await.unwrap();

    assert!(result.is_some(), "Should fall back to first item when current is deleted");
    let state = result.unwrap();
    assert_eq!(state.playing_media_id, Some(_media2.id), "Should fall back to first remaining item");
}

// ========== RepeatOne Mode Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_repeat_one_replays_current() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("rep1_owner")).await.unwrap();
    let (room, _) = room_service
        .create_room("Rep1".to_string(), String::new(), owner.id.clone(), None, None)
        .await
        .unwrap();

    let playlist = get_root_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "repeat_me", 0).await;
    let _media2 = insert_media(&pool, &playlist.id, &room.id, "other", 1).await;

    let playback = room_service.playback_service();
    playback.switch_media(room.id.clone(), owner.id.clone(), media1.id.clone()).await.unwrap();

    let settings = make_settings_with_mode(PlayMode::RepeatOne);
    let result = playback.play_next(&room.id, &settings).await.unwrap();

    assert!(result.is_some(), "RepeatOne should replay current");
    let state = result.unwrap();
    assert_eq!(state.playing_media_id, Some(media1.id), "Should replay the same media");
    assert!((state.current_time - 0.0).abs() < f64::EPSILON, "Should reset to start");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_repeat_one_deleted_current_falls_back_to_first() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("rep1_del_owner")).await.unwrap();
    let (room, _) = room_service
        .create_room("Rep1 Del".to_string(), String::new(), owner.id.clone(), None, None)
        .await
        .unwrap();

    let playlist = get_root_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "deleted_rep", 0).await;
    let media2 = insert_media(&pool, &playlist.id, &room.id, "fallback", 1).await;

    let playback = room_service.playback_service();
    playback.switch_media(room.id.clone(), owner.id.clone(), media1.id.clone()).await.unwrap();

    // Delete media1
    sqlx::query("DELETE FROM media WHERE id = $1")
        .bind(media1.id.as_str())
        .execute(&pool)
        .await
        .unwrap();

    let settings = make_settings_with_mode(PlayMode::RepeatOne);
    let result = playback.play_next(&room.id, &settings).await.unwrap();

    // Issue #29: RepeatOne with deleted media should fall back to first item
    assert!(result.is_some(), "Should fall back when RepeatOne media deleted");
    let state = result.unwrap();
    assert_eq!(state.playing_media_id, Some(media2.id), "Should fall back to first remaining item");
}

// ========== RepeatAll Mode Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_repeat_all_wraps_around_at_end() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("repa_owner")).await.unwrap();
    let (room, _) = room_service
        .create_room("RepAll".to_string(), String::new(), owner.id.clone(), None, None)
        .await
        .unwrap();

    let playlist = get_root_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "first", 0).await;
    let _media2 = insert_media(&pool, &playlist.id, &room.id, "second", 1).await;
    let media3 = insert_media(&pool, &playlist.id, &room.id, "third", 2).await;

    let playback = room_service.playback_service();
    // Start at last item
    playback.switch_media(room.id.clone(), owner.id.clone(), media3.id.clone()).await.unwrap();

    let settings = make_settings_with_mode(PlayMode::RepeatAll);
    let result = playback.play_next(&room.id, &settings).await.unwrap();

    assert!(result.is_some(), "RepeatAll should wrap around");
    let state = result.unwrap();
    assert_eq!(state.playing_media_id, Some(media1.id), "Should wrap back to first item");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_repeat_all_middle_advances_to_next() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("repa_mid_owner")).await.unwrap();
    let (room, _) = room_service
        .create_room("RepAll Mid".to_string(), String::new(), owner.id.clone(), None, None)
        .await
        .unwrap();

    let playlist = get_root_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "vid_a", 0).await;
    let media2 = insert_media(&pool, &playlist.id, &room.id, "vid_b", 1).await;
    let _media3 = insert_media(&pool, &playlist.id, &room.id, "vid_c", 2).await;

    let playback = room_service.playback_service();
    playback.switch_media(room.id.clone(), owner.id.clone(), media1.id.clone()).await.unwrap();

    let settings = make_settings_with_mode(PlayMode::RepeatAll);
    let result = playback.play_next(&room.id, &settings).await.unwrap();

    assert!(result.is_some());
    let state = result.unwrap();
    assert_eq!(state.playing_media_id, Some(media2.id), "RepeatAll mid-playlist should advance normally");
}

// ========== Shuffle Mode Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_shuffle_returns_different_item() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("shuf_owner")).await.unwrap();
    let (room, _) = room_service
        .create_room("Shuffle".to_string(), String::new(), owner.id.clone(), None, None)
        .await
        .unwrap();

    let playlist = get_root_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "shuf1", 0).await;
    // Add enough items to make collision very unlikely
    for i in 2..=10 {
        insert_media(&pool, &playlist.id, &room.id, &format!("shuf{}", i), i - 1).await;
    }

    let playback = room_service.playback_service();
    playback.switch_media(room.id.clone(), owner.id.clone(), media1.id.clone()).await.unwrap();

    let settings = make_settings_with_mode(PlayMode::Shuffle);

    // Run shuffle multiple times; at least once should return a different item
    let mut got_different = false;
    for _ in 0..10 {
        // Re-set the current to media1 before each try
        playback.switch_media(room.id.clone(), owner.id.clone(), media1.id.clone()).await.unwrap();

        let result = playback.play_next(&room.id, &settings).await.unwrap();
        assert!(result.is_some(), "Shuffle should always return some item");
        let state = result.unwrap();
        if state.playing_media_id != Some(media1.id.clone()) {
            got_different = true;
            break;
        }
    }
    assert!(got_different, "Shuffle should eventually return a different item");
}

// ========== Auto-Play Disabled ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_auto_play_disabled_returns_none() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("noauto_owner")).await.unwrap();
    let (room, _) = room_service
        .create_room("No Auto".to_string(), String::new(), owner.id.clone(), None, None)
        .await
        .unwrap();

    let playlist = get_root_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "vid", 0).await;
    insert_media(&pool, &playlist.id, &room.id, "vid2", 1).await;

    let playback = room_service.playback_service();
    playback.switch_media(room.id.clone(), owner.id.clone(), media1.id.clone()).await.unwrap();

    // Disabled: auto_play.enabled = false AND auto_play_next = false
    let mut settings = RoomSettings::default();
    settings.auto_play = AutoPlay::new(AutoPlaySettings {
        enabled: false,
        mode: PlayMode::Sequential,
        delay: 0,
    });
    settings.auto_play_next = AutoPlayNext(false);

    let result = playback.play_next(&room.id, &settings).await.unwrap();
    assert!(result.is_none(), "play_next should return None when auto_play disabled");
}

// ========== Legacy Field Mapping Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_legacy_loop_playlist_maps_to_repeat_all() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("legacy_loop_owner")).await.unwrap();
    let (room, _) = room_service
        .create_room("Legacy Loop".to_string(), String::new(), owner.id.clone(), None, None)
        .await
        .unwrap();

    let playlist = get_root_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "loop_last", 0).await;

    let playback = room_service.playback_service();
    playback.switch_media(room.id.clone(), owner.id.clone(), media1.id.clone()).await.unwrap();

    // Use legacy field: loop_playlist = true, auto_play.mode = Sequential (default)
    let mut settings = RoomSettings::default();
    settings.auto_play_next = AutoPlayNext(true);
    settings.loop_playlist = LoopPlaylist(true);
    // auto_play.mode stays Sequential, which triggers legacy fallback

    let result = playback.play_next(&room.id, &settings).await.unwrap();
    // Single item + RepeatAll should wrap to itself
    assert!(result.is_some(), "Legacy loop_playlist should enable RepeatAll behavior");
    let state = result.unwrap();
    assert_eq!(state.playing_media_id, Some(media1.id), "Should wrap around (RepeatAll from legacy field)");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_legacy_shuffle_playlist_maps_to_shuffle() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("legacy_shuf_owner")).await.unwrap();
    let (room, _) = room_service
        .create_room("Legacy Shuf".to_string(), String::new(), owner.id.clone(), None, None)
        .await
        .unwrap();

    let playlist = get_root_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "s1", 0).await;
    for i in 2..=5 {
        insert_media(&pool, &playlist.id, &room.id, &format!("s{}", i), i - 1).await;
    }

    let playback = room_service.playback_service();
    playback.switch_media(room.id.clone(), owner.id.clone(), media1.id.clone()).await.unwrap();

    // Legacy shuffle_playlist = true
    let mut settings = RoomSettings::default();
    settings.auto_play_next = AutoPlayNext(true);
    settings.shuffle_playlist = ShufflePlaylist(true);

    let result = playback.play_next(&room.id, &settings).await.unwrap();
    assert!(result.is_some(), "Legacy shuffle_playlist should enable shuffle behavior");
}

// ========== Empty Playlist ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_empty_playlist_returns_none() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("empty_pl_owner")).await.unwrap();
    let (room, _) = room_service
        .create_room("Empty PL".to_string(), String::new(), owner.id.clone(), None, None)
        .await
        .unwrap();

    // Don't add any media -- playlist is empty
    let playback = room_service.playback_service();
    let settings = make_settings_with_mode(PlayMode::Sequential);
    let result = playback.play_next(&room.id, &settings).await.unwrap();

    assert!(result.is_none(), "Empty playlist should return None");
}

// ========== No Currently Playing Media ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_no_current_media_plays_first() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("nocur_owner")).await.unwrap();
    let (room, _) = room_service
        .create_room("No Current".to_string(), String::new(), owner.id.clone(), None, None)
        .await
        .unwrap();

    let playlist = get_root_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "first_vid", 0).await;
    insert_media(&pool, &playlist.id, &room.id, "second_vid", 1).await;

    // Don't switch to any media -- playing_media_id is None
    let playback = room_service.playback_service();
    let settings = make_settings_with_mode(PlayMode::Sequential);
    let result = playback.play_next(&room.id, &settings).await.unwrap();

    assert!(result.is_some(), "Should play first item when nothing is playing");
    let state = result.unwrap();
    assert_eq!(state.playing_media_id, Some(media1.id), "Should start with first item");
}
