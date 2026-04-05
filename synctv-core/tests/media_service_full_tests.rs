//! `MediaService` integration tests (S8/S9)
//!
//! Tests `add_media` permission check, `add_media_batch` size limit,
//! `edit_media` cross-room check and optimistic lock retry with real `PostgreSQL`.
//!
//! Run with: cargo test -p synctv-core --test `media_service_full_tests` -- --nocapture
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, NoopCacheL2, UsernameCache},
    config::PasswordComplexityConfig,
    models::{PermissionBits, Playlist, User, UserId, UserRole, UserStatus},
    repository::UserRepository,
    service::{
        auth::{BruteForceProtection, JwtService, TestPasswordHasher},
        media::{AddMediaRequest, EditMediaRequest},
        InMemoryTokenBlacklistStore, RoomService, UserService,
    },
    Error,
};
use synctv_core_testing::create_test_pool;
fn make_user_service(pool: PgPool) -> UserService {
    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = JwtService::new(secret).expect("Failed to create JwtService");
    let l2 = Arc::new(NoopCacheL2);
    let username_cache = UsernameCache::new(l2, "test:username:".to_string(), 100, 60);
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

/// Register a "`direct_url`" provider instance so `add_media` tests can reference it.
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
        .create_room(
            "Add Media Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    room_service
        .join_room(room.id.clone(), member.id.clone(), None)
        .await
        .unwrap();
    register_direct_url_provider(&room_service).await;

    // Revoke ADD_MOVIE from member
    room_service
        .member_service()
        .revoke_permission(
            room.id.clone(),
            creator.id.clone(),
            member.id.clone(),
            PermissionBits::ADD_MOVIE,
        )
        .await
        .unwrap();

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media_service = room_service.media_service();

    let request = AddMediaRequest {
        playlist_id: Some(playlist.id.clone()),
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
        other => panic!("Expected Authorization error, got: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "Requires Docker"]
async fn test_add_media_with_permission_succeeds() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("addm2_creator")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Add Media OK Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();
    register_direct_url_provider(&room_service).await;

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media_service = room_service.media_service();

    let request = AddMediaRequest {
        playlist_id: Some(playlist.id.clone()),
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
        .create_room(
            "Room A".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();
    let (room_b, _) = room_service
        .create_room(
            "Room B".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    register_direct_url_provider(&room_service).await;

    // Get playlist from room B
    let playlist_b = create_top_level_playlist(&pool, &room_b.id).await;
    let media_service = room_service.media_service();

    // Try to add media to room A using room B's playlist
    let request = AddMediaRequest {
        playlist_id: Some(playlist_b.id.clone()),
        name: "Cross Room Video".to_string(),
        provider_instance_name: "direct_url".to_string(),
        source_config: serde_json::json!({"url": "https://example.com/cross.mp4"}),
    };

    let result = media_service
        .add_media(room_a.id.clone(), creator.id.clone(), request)
        .await;

    assert!(
        result.is_err(),
        "Should fail when adding to cross-room playlist"
    );
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
        .create_room(
            "Batch Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    register_direct_url_provider(&room_service).await;
    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media_service = room_service.media_service();

    // Create 101 requests
    let requests: Vec<AddMediaRequest> = (0..101)
        .map(|i| AddMediaRequest {
            playlist_id: Some(playlist.id.clone()),
            name: format!("Batch Video {i}"),
            provider_instance_name: "direct_url".to_string(),
            source_config: serde_json::json!({"url": format!("https://example.com/batch{}.mp4", i)}),
        })
        .collect();

    let result = media_service
        .add_media_batch(
            room.id.clone(),
            creator.id.clone(),
            Some(playlist.id.clone()),
            requests,
        )
        .await;

    assert!(result.is_err(), "Batch of 101 items should be rejected");
    match result.unwrap_err() {
        Error::InvalidInput(msg) => {
            assert!(
                msg.contains("100") || msg.contains("batch") || msg.contains("exceed"),
                "Should mention batch size limit: {msg}"
            );
        }
        other => panic!("Expected InvalidInput error, got: {other:?}"),
    }
}

// ========== add_media_batch: empty slice returns empty vec ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_media_batch_empty_returns_empty() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("batch_empty_creator"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Batch Empty Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let media_service = room_service.media_service();

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let result = media_service
        .add_media_batch(
            room.id.clone(),
            creator.id.clone(),
            Some(playlist.id.clone()),
            vec![],
        )
        .await;

    assert!(result.is_ok(), "Empty batch should succeed");
    let media_list = result.unwrap();
    assert!(media_list.is_empty(), "Empty batch should return empty vec");
}

// ========== add_media_batch: exactly 100 accepted ==========

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "Requires Docker"]
async fn test_add_media_batch_exactly_100_accepted() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("batch100_creator"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Batch 100 Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    register_direct_url_provider(&room_service).await;
    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media_service = room_service.media_service();

    // Create exactly 100 requests
    let requests: Vec<AddMediaRequest> = (0..100)
        .map(|i| AddMediaRequest {
            playlist_id: Some(playlist.id.clone()),
            name: format!("Video {i}"),
            provider_instance_name: "direct_url".to_string(),
            source_config: serde_json::json!({"url": format!("https://example.com/v{}.mp4", i)}),
        })
        .collect();

    let result = media_service
        .add_media_batch(
            room.id.clone(),
            creator.id.clone(),
            Some(playlist.id.clone()),
            requests,
        )
        .await;

    assert!(
        result.is_ok(),
        "Batch of exactly 100 items should be accepted"
    );
    let media_list = result.unwrap();
    assert_eq!(
        media_list.len(),
        100,
        "Should return 100 created media items"
    );
}

// ========== edit_media: optimistic lock retry exhaustion ==========

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "Requires Docker"]
async fn test_edit_media_optimistic_lock_retry_exhaustion() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let creator = user_repo
        .create(&make_user("edit_olr_creator"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Edit OLR Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    register_direct_url_provider(&room_service).await;
    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media_service = room_service.media_service();

    // Add a media item
    let add_req = AddMediaRequest {
        playlist_id: Some(playlist.id.clone()),
        name: "Original Name".to_string(),
        provider_instance_name: "direct_url".to_string(),
        source_config: serde_json::json!({"url": "https://example.com/edit.mp4"}),
    };
    let media = media_service
        .add_media(room.id.clone(), creator.id.clone(), add_req)
        .await
        .unwrap();

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
            assert!(
                msg.contains("retri")
                    || msg.contains("retry")
                    || msg.contains("maximum")
                    || msg.contains("concurrent"),
                "Should mention retry exhaustion: {msg}"
            );
        }
        Err(Error::OptimisticLockConflict) => {
            panic!("OptimisticLockConflict should not leak to caller");
        }
        Err(other) => {
            panic!("Unexpected error: {other:?}");
        }
    }
}

// ========== Move Validation Tests ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_move_media_requires_exactly_one_anchor() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("move_media_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Move Media Validation".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playlist = create_top_level_playlist(&pool, &room.id).await;

    let media_repo = synctv_core::repository::MediaRepository::new(pool.clone());
    let media = synctv_core::models::Media {
        id: synctv_core::models::MediaId::new(),
        playlist_id: Some(playlist.id.clone()),
        room_id: room.id.clone(),
        name: "Media".to_string(),
        position: 1024.0,
        source_provider: "direct_url".to_string(),
        source_config: serde_json::json!({}),
        provider_instance_name: "direct_url".to_string(),
        creator_id: Some(owner.id.clone()),
        added_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 0,
    };
    let media = media_repo.create(&media).await.unwrap();

    let missing_anchor = room_service
        .media_service()
        .move_media(
            room.id.clone(),
            owner.id.clone(),
            synctv_core::service::media::MoveMediaRequest {
                media_id: media.id.clone(),
                before_media_id: None,
                after_media_id: None,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(missing_anchor, Error::InvalidInput(_)));

    let conflicting_anchor = room_service
        .media_service()
        .move_media(
            room.id.clone(),
            owner.id.clone(),
            synctv_core::service::media::MoveMediaRequest {
                media_id: media.id.clone(),
                before_media_id: Some(media.id.clone()),
                after_media_id: Some(media.id.clone()),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(conflicting_anchor, Error::InvalidInput(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_move_media_reorders_using_anchor_positions() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("move_media_order_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Move Media Order".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media_repo = synctv_core::repository::MediaRepository::new(pool.clone());

    let media1 = media_repo
        .create(&synctv_core::models::Media {
            id: synctv_core::models::MediaId::new(),
            playlist_id: Some(playlist.id.clone()),
            room_id: room.id.clone(),
            name: "Media 1".to_string(),
            position: 1024.0,
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({}),
            provider_instance_name: "direct_url".to_string(),
            creator_id: Some(owner.id.clone()),
            added_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 0,
        })
        .await
        .unwrap();
    let media2 = media_repo
        .create(&synctv_core::models::Media {
            id: synctv_core::models::MediaId::new(),
            playlist_id: Some(playlist.id.clone()),
            room_id: room.id.clone(),
            name: "Media 2".to_string(),
            position: 2048.0,
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({}),
            provider_instance_name: "direct_url".to_string(),
            creator_id: Some(owner.id.clone()),
            added_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 0,
        })
        .await
        .unwrap();

    let moved = room_service
        .media_service()
        .move_media(
            room.id.clone(),
            owner.id.clone(),
            synctv_core::service::media::MoveMediaRequest {
                media_id: media2.id.clone(),
                before_media_id: Some(media1.id.clone()),
                after_media_id: None,
            },
        )
        .await
        .unwrap();

    let updated1 = media_repo.get_by_id(&media1.id).await.unwrap().unwrap();
    let updated2 = media_repo.get_by_id(&media2.id).await.unwrap().unwrap();
    assert_eq!(moved.id, media2.id);
    assert!(updated2.position < updated1.position);
}
