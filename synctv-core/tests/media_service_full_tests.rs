//! MediaService integration tests (S8/S9)
//!
//! Tests add_media permission check, add_media_batch size limit,
//! edit_media cross-room check and optimistic lock retry with real PostgreSQL.
//!
//! Run with: cargo test -p synctv-core --test media_service_full_tests -- --nocapture

use std::sync::Arc;

use synctv_core::{
    cache::{KeyBuilder, UsernameCache, NoopCacheL2},
    config::PasswordComplexityConfig,
    models::{
        UserId, User, UserRole, UserStatus,
        PermissionBits, Playlist,
    },
    repository::UserRepository,
    service::{
        RoomService, UserService, InMemoryTokenBlacklistStore,
        media::{AddMediaRequest, EditMediaRequest},
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

async fn get_root_playlist(pool: &PgPool, room_id: &synctv_core::models::RoomId) -> Playlist {
    sqlx::query_as::<_, Playlist>("SELECT * FROM playlists WHERE room_id = $1 LIMIT 1")
        .bind(room_id.as_str())
        .fetch_one(pool)
        .await
        .expect("Root playlist should exist")
}

/// Register a "direct_url" provider instance so add_media tests can reference it.
async fn register_direct_url_provider(room_service: &RoomService) {
    room_service
        .media_service()
        .providers_manager()
        .create_provider("direct_url", "direct_url", &serde_json::json!({}))
        .await
        .expect("Failed to register direct_url provider");
}

// ========== add_media: ADD_MEDIA permission check ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_media_without_permission_denied() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("addm_creator")).await.unwrap();
    let member = user_repo.create(&make_user("addm_member")).await.unwrap();

    let (room, _) = room_service
        .create_room("Add Media Room".to_string(), String::new(), creator.id.clone(), None, None)
        .await
        .unwrap();

    room_service.join_room(room.id.clone(), member.id.clone(), None).await.unwrap();
    register_direct_url_provider(&room_service).await;

    // Revoke ADD_MOVIE from member
    room_service.member_service().revoke_permission(
        room.id.clone(),
        creator.id.clone(),
        member.id.clone(),
        PermissionBits::ADD_MOVIE,
    ).await.unwrap();

    let playlist = get_root_playlist(&pool, &room.id).await;
    let media_service = room_service.media_service();

    let request = AddMediaRequest {
        playlist_id: playlist.id.clone(),
        name: "Forbidden Video".to_string(),
        provider_instance_name: "direct_url".to_string(),
        source_config: serde_json::json!({"url": "https://example.com/vid.mp4"}),
    };

    let result = media_service
        .add_media(room.id.clone(), member.id.clone(), request)
        .await;

    assert!(result.is_err(), "Should fail without ADD_MOVIE permission");
    match result.unwrap_err() {
        Error::Authorization(_) => {}
        other => panic!("Expected Authorization error, got: {:?}", other),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_media_with_permission_succeeds() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("addm2_creator")).await.unwrap();

    let (room, _) = room_service
        .create_room("Add Media OK Room".to_string(), String::new(), creator.id.clone(), None, None)
        .await
        .unwrap();
    register_direct_url_provider(&room_service).await;

    let playlist = get_root_playlist(&pool, &room.id).await;
    let media_service = room_service.media_service();

    let request = AddMediaRequest {
        playlist_id: playlist.id.clone(),
        name: "Good Video".to_string(),
        provider_instance_name: "direct_url".to_string(),
        source_config: serde_json::json!({"url": "https://example.com/good.mp4"}),
    };

    let result = media_service
        .add_media(room.id.clone(), creator.id.clone(), request)
        .await;

    assert!(result.is_ok(), "Creator should be able to add media");
    let media = result.unwrap();
    assert_eq!(media.name, "Good Video");
}

// ========== add_media: cross-room playlist authorization ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_media_cross_room_playlist_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("xroom_creator")).await.unwrap();

    // Create two rooms
    let (room_a, _) = room_service
        .create_room("Room A".to_string(), String::new(), creator.id.clone(), None, None)
        .await
        .unwrap();
    let (room_b, _) = room_service
        .create_room("Room B".to_string(), String::new(), creator.id.clone(), None, None)
        .await
        .unwrap();

    register_direct_url_provider(&room_service).await;

    // Get playlist from room B
    let playlist_b = get_root_playlist(&pool, &room_b.id).await;
    let media_service = room_service.media_service();

    // Try to add media to room A using room B's playlist
    let request = AddMediaRequest {
        playlist_id: playlist_b.id.clone(),
        name: "Cross Room Video".to_string(),
        provider_instance_name: "direct_url".to_string(),
        source_config: serde_json::json!({"url": "https://example.com/cross.mp4"}),
    };

    let result = media_service
        .add_media(room_a.id.clone(), creator.id.clone(), request)
        .await;

    assert!(result.is_err(), "Should fail when adding to cross-room playlist");
}

// ========== add_media_batch: size limit (>100 items rejected) ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_media_batch_over_100_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("batch_creator")).await.unwrap();

    let (room, _) = room_service
        .create_room("Batch Room".to_string(), String::new(), creator.id.clone(), None, None)
        .await
        .unwrap();

    register_direct_url_provider(&room_service).await;
    let playlist = get_root_playlist(&pool, &room.id).await;
    let media_service = room_service.media_service();

    // Create 101 requests
    let requests: Vec<AddMediaRequest> = (0..101)
        .map(|i| AddMediaRequest {
            playlist_id: playlist.id.clone(),
            name: format!("Batch Video {}", i),
            provider_instance_name: "direct_url".to_string(),
            source_config: serde_json::json!({"url": format!("https://example.com/batch{}.mp4", i)}),
        })
        .collect();

    let result = media_service
        .add_media_batch(room.id.clone(), creator.id.clone(), playlist.id.clone(), requests)
        .await;

    assert!(result.is_err(), "Batch of 101 items should be rejected");
    match result.unwrap_err() {
        Error::InvalidInput(msg) => {
            assert!(msg.contains("100") || msg.contains("batch") || msg.contains("exceed"),
                "Should mention batch size limit: {}", msg);
        }
        other => panic!("Expected InvalidInput error, got: {:?}", other),
    }
}

// ========== add_media_batch: empty slice returns empty vec ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_media_batch_empty_returns_empty() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("batch_empty_creator")).await.unwrap();

    let (room, _) = room_service
        .create_room("Batch Empty Room".to_string(), String::new(), creator.id.clone(), None, None)
        .await
        .unwrap();

    let media_service = room_service.media_service();

    let playlist = get_root_playlist(&pool, &room.id).await;
    let result = media_service
        .add_media_batch(room.id.clone(), creator.id.clone(), playlist.id.clone(), vec![])
        .await;

    assert!(result.is_ok(), "Empty batch should succeed");
    let media_list = result.unwrap();
    assert!(media_list.is_empty(), "Empty batch should return empty vec");
}

// ========== add_media_batch: exactly 100 accepted ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_media_batch_exactly_100_accepted() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("batch100_creator")).await.unwrap();

    let (room, _) = room_service
        .create_room("Batch 100 Room".to_string(), String::new(), creator.id.clone(), None, None)
        .await
        .unwrap();

    register_direct_url_provider(&room_service).await;
    let playlist = get_root_playlist(&pool, &room.id).await;
    let media_service = room_service.media_service();

    // Create exactly 100 requests
    let requests: Vec<AddMediaRequest> = (0..100)
        .map(|i| AddMediaRequest {
            playlist_id: playlist.id.clone(),
            name: format!("Video {}", i),
            provider_instance_name: "direct_url".to_string(),
            source_config: serde_json::json!({"url": format!("https://example.com/v{}.mp4", i)}),
        })
        .collect();

    let result = media_service
        .add_media_batch(room.id.clone(), creator.id.clone(), playlist.id.clone(), requests)
        .await;

    assert!(result.is_ok(), "Batch of exactly 100 items should be accepted");
    let media_list = result.unwrap();
    assert_eq!(media_list.len(), 100, "Should return 100 created media items");
}

// ========== edit_media: optimistic lock retry exhaustion ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_edit_media_optimistic_lock_retry_exhaustion() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let creator = user_repo.create(&make_user("edit_olr_creator")).await.unwrap();

    let (room, _) = room_service
        .create_room("Edit OLR Room".to_string(), String::new(), creator.id.clone(), None, None)
        .await
        .unwrap();

    register_direct_url_provider(&*room_service).await;
    let playlist = get_root_playlist(&pool, &room.id).await;
    let media_service = room_service.media_service();

    // Add a media item
    let add_req = AddMediaRequest {
        playlist_id: playlist.id.clone(),
        name: "Original Name".to_string(),
        provider_instance_name: "direct_url".to_string(),
        source_config: serde_json::json!({"url": "https://example.com/edit.mp4"}),
    };
    let media = media_service.add_media(room.id.clone(), creator.id.clone(), add_req).await.unwrap();

    // Continuously bump media version to trigger retry exhaustion
    let media_id_str = media.id.as_str().to_string();
    let pool_clone = pool.clone();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = stop.clone();

    let bumper = tokio::spawn(async move {
        while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = sqlx::query("UPDATE media SET position = position + 1 WHERE id = $1")
                .bind(&media_id_str)
                .execute(&pool_clone)
                .await;
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    });

    let edit_req = EditMediaRequest {
        media_id: media.id.clone(),
        name: Some("Updated Name".to_string()),
        position: None,
    };

    let result = media_service
        .edit_media(room.id.clone(), creator.id.clone(), edit_req)
        .await;

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = bumper.await;

    // Either success (got lucky) or Internal (retry exhaustion)
    match result {
        Ok(_) => {}
        Err(Error::Internal(msg)) => {
            assert!(msg.contains("retry") || msg.contains("maximum"),
                "Should mention retry exhaustion: {}", msg);
        }
        Err(Error::OptimisticLockConflict) => {
            panic!("OptimisticLockConflict should not leak to caller");
        }
        Err(other) => {
            panic!("Unexpected error: {:?}", other);
        }
    }
}
