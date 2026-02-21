//! PlaybackService integration tests
//!
//! Tests playback control including seek, speed, media switching, and
//! optimistic lock behavior with real PostgreSQL via testcontainers.
//!
//! Run with: cargo test -p synctv-core --test playback_service_tests -- --nocapture

use std::sync::Arc;

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
    Error,
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

// ========== Seek Validation Tests ==========

#[tokio::test]
async fn test_seek_negative_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("seek_neg_owner")).await.unwrap();

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
            assert!(msg.contains("non-negative") || msg.contains("negative"),
                "Error should mention non-negative: {}", msg);
        }
        other => panic!("Expected InvalidInput error, got: {:?}", other),
    }
}

// ========== Speed Validation Tests ==========

#[tokio::test]
async fn test_speed_zero_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("speed_zero_owner")).await.unwrap();

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
            assert!(msg.contains("Speed") || msg.contains("speed"),
                "Error should mention speed: {}", msg);
        }
        other => panic!("Expected InvalidInput error, got: {:?}", other),
    }
}

#[tokio::test]
async fn test_speed_above_max_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("speed_max_owner")).await.unwrap();

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
            assert!(msg.contains("Speed") || msg.contains("16"),
                "Error should mention speed limit: {}", msg);
        }
        other => panic!("Expected InvalidInput error, got: {:?}", other),
    }
}

// ========== Switch Media Tests ==========

#[tokio::test]
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

    // Get the root playlist that was created with the room
    let playlists: Vec<Playlist> = sqlx::query_as(
        "SELECT * FROM playlists WHERE room_id = $1"
    )
    .bind(room.id.as_str())
    .fetch_all(&pool)
    .await
    .unwrap();
    let root_playlist = &playlists[0];

    // Add a media item
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
        .switch_media(room.id.clone(), owner.id.clone(), media.id.clone())
        .await
        .unwrap();

    assert!((state.current_time - 0.0).abs() < f64::EPSILON,
        "Current time should be reset to 0 after media switch, got: {}", state.current_time);
    assert_eq!(state.playing_media_id, Some(media.id));
    assert!(state.is_playing, "Should be playing after media switch");
}

// ========== Optimistic Lock Concurrent Test ==========

#[tokio::test]
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
        let position = (i as f64) * 10.0;

        let handle = tokio::spawn(async move {
            rs.playback_service()
                .seek(rid, uid, position)
                .await
        });
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
                assert!(!matches!(e, Error::OptimisticLockConflict),
                    "OptimisticLockConflict should not leak to caller");
            }
            Err(e) => panic!("Task panicked: {:?}", e),
        }
    }

    assert!(success_count > 0, "At least one seek should succeed");

    // Final state should be valid
    let playback_service = room_service.playback_service();
    let state = playback_service.get_state(&room.id).await.unwrap();
    assert!(state.current_time >= 0.0, "Final position should be non-negative");
}
