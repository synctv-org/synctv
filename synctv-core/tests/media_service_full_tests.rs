//! `MediaService` integration tests (S8/S9)
//!
//! Tests `add_media` permission check, `add_media_batch` size limit,
//! `edit_media` cross-room check and optimistic lock retry with real `PostgreSQL`.
//!
//! Run with: cargo test -p synctv-core --test `media_service_full_tests` -- --nocapture
#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    config::PasswordComplexityConfig,
    models::{
        PermissionBits, Playlist, User, UserId, UserProviderCredential, UserRole, UserStatus,
    },
    repository::{UserProviderCredentialRepository, UserRepository},
    service::{
        auth::{BruteForceProtection, JwtService, TestPasswordHasher},
        media::{AddMediaRequest, EditMediaRequest},
        playlist::CreatePlaylistRequest,
        CredentialEncryption, InMemoryTokenBlacklistStore, RoomService, UserService,
    },
    Error,
};
use synctv_core_testing::create_test_pool;

fn test_encryption() -> CredentialEncryption {
    CredentialEncryption::new(&[0x24; 32]).expect("test credential encryption")
}
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

async fn register_bilibili_provider(room_service: &RoomService) {
    room_service
        .media_service()
        .providers_manager()
        .create_provider("bilibili", "bilibili", &serde_json::json!({}))
        .await
        .expect("Failed to register bilibili provider");
}

async fn register_alist_provider(room_service: &RoomService) {
    room_service
        .media_service()
        .providers_manager()
        .create_provider("alist", "alist", &serde_json::json!({}))
        .await
        .expect("Failed to register alist provider");
}

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

    // Revoke ADD_MEDIA from member
    room_service
        .member_service()
        .revoke_permission(
            room.id.clone(),
            creator.id.clone(),
            member.id.clone(),
            PermissionBits::ADD_MEDIA,
        )
        .await
        .unwrap();

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media_service = room_service.media_service();

    let request = AddMediaRequest {
        playlist_id: Some(playlist.id.clone()),
        name: "Forbidden Video".to_string(),
        source_provider: "direct_url".to_string(),
        provider_instance_name: Some("direct_url".to_string()),
        source_config: serde_json::json!({"url": "https://example.com/vid.mp4"}),
    };

    let result = media_service
        .add_media(room.id.clone(), member.id.clone(), request)
        .await;

    assert!(result.is_err(), "Should fail without ADD_MEDIA permission");
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
        source_provider: "direct_url".to_string(),
        provider_instance_name: Some("direct_url".to_string()),
        source_config: serde_json::json!({"url": "https://example.com/good.mp4"}),
    };

    let result = media_service
        .add_media(room.id.clone(), creator.id.clone(), request)
        .await;

    assert!(result.is_ok(), "Creator should be able to add media");
    let media = result.unwrap();
    assert_eq!(media.name, "Good Video");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_media_ignores_forged_credential_owner_id_for_bilibili() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let mut room_service = make_room_service(pool.clone());
    let encryption = test_encryption();
    let credential_repo = Arc::new(UserProviderCredentialRepository::new_with_encryption(
        pool.clone(),
        encryption.clone(),
    ));

    room_service.set_media_credential_encryption(encryption);
    room_service.set_media_credential_repo(Arc::clone(&credential_repo));

    let creator = user_repo
        .create(&make_user("addm_bili_creator"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Add Media Bilibili Room".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();
    register_bilibili_provider(&room_service).await;

    let server_id = UserProviderCredential::bilibili_server_id(None);
    credential_repo
        .create(&UserProviderCredential {
            id: synctv_common::snanoid!(12),
            user_id: creator.id.to_string(),
            provider: "bilibili".to_string(),
            server_id: server_id.clone(),
            provider_instance_name: None,
            credential_data: serde_json::to_value(
                synctv_core::models::ProviderCredential::bilibili(HashMap::from([(
                    "SESSDATA".to_string(),
                    "test_session".to_string(),
                )])),
            )
            .expect("bilibili credential should serialize"),
            expires_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        })
        .await
        .unwrap();

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media_service = room_service.media_service();

    let request = AddMediaRequest {
        playlist_id: Some(playlist.id.clone()),
        name: "Bilibili Video".to_string(),
        source_provider: "bilibili".to_string(),
        provider_instance_name: Some("bilibili".to_string()),
        source_config: serde_json::json!({
            "type": "video",
            "bvid": "BV1GJ411x7gL",
            "cid": 12345,
            "credential_ref": {
                "credential_owner_id": "forged-user-id",
                "server_id": server_id
            }
        }),
    };

    let media = media_service
        .add_media(room.id.clone(), creator.id.clone(), request)
        .await
        .expect("add_media should ignore forged credential owner ids");

    assert_eq!(
        media.source_config["credential_ref"]["credential_owner_id"],
        serde_json::Value::String(creator.id.to_string())
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_media_with_credential_backed_provider_without_repo_fails_closed() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("addm_bili_missing_repo"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Add Media Missing Repo".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();
    register_bilibili_provider(&room_service).await;

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let request = AddMediaRequest {
        playlist_id: Some(playlist.id.clone()),
        name: "Bilibili Missing Repo".to_string(),
        source_provider: "bilibili".to_string(),
        provider_instance_name: Some("bilibili".to_string()),
        source_config: serde_json::json!({
            "type": "video",
            "bvid": "BV1GJ411x7gL",
            "cid": 12345,
            "credential_ref": {
                "credential_owner_id": creator.id.to_string(),
                "server_id": "bilibili"
            }
        }),
    };

    let err = room_service
        .media_service()
        .add_media(room.id.clone(), creator.id.clone(), request)
        .await
        .expect_err("credential-backed media should fail closed without repo wiring");

    match err {
        Error::ServiceUnavailable(message) => {
            assert!(message.contains("bilibili"));
            assert!(message.contains("credential repository"));
        }
        other => panic!("expected ServiceUnavailable, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_dynamic_playlist_with_credential_backed_provider_without_repo_fails_closed() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("plist_alist_missing_repo"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Dynamic Playlist Missing Repo".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();
    register_alist_provider(&room_service).await;

    let request = CreatePlaylistRequest {
        room_id: room.id.clone(),
        name: "Alist Dynamic".to_string(),
        parent_id: None,
        source_provider: Some("alist".to_string()),
        source_config: Some(serde_json::json!({
            "path": "/media/library",
            "credential_ref": {
                "credential_owner_id": creator.id.to_string(),
                "server_id": "alist-server"
            }
        })),
        provider_instance_name: Some("alist".to_string()),
    };

    let err = room_service
        .playlist_service()
        .create_playlist(room.id.clone(), creator.id.clone(), request)
        .await
        .expect_err("credential-backed dynamic playlist should fail closed without repo wiring");

    match err {
        Error::ServiceUnavailable(message) => {
            assert!(message.contains("alist"));
            assert!(message.contains("credential repository"));
        }
        other => panic!("expected ServiceUnavailable, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_dynamic_playlist_items_with_credential_backed_provider_without_repo_fails_closed(
) {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("alist_dynamic_runtime_missing_repo"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Dynamic Playlist Runtime Missing Repo".to_string(),
            String::new(),
            creator.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();
    register_alist_provider(&room_service).await;

    let playlist = Playlist {
        id: synctv_core::models::PlaylistId::new(),
        room_id: room.id.clone(),
        creator_id: Some(creator.id.clone()),
        name: "Persisted Alist Dynamic".to_string(),
        parent_id: None,
        position: 0.0,
        source_provider: Some("alist".to_string()),
        source_config: Some(serde_json::json!({
            "path": "/media/library",
            "credential_ref": {
                "credential_owner_id": creator.id.to_string(),
                "server_id": "alist-server"
            }
        })),
        provider_instance_name: Some("alist".to_string()),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    synctv_core::repository::PlaylistRepository::new(pool.clone())
        .create(&playlist)
        .await
        .unwrap();

    let err = room_service
        .media_service()
        .list_dynamic_playlist_items(
            room.id.clone(),
            creator.id.clone(),
            &playlist.id,
            None,
            0,
            20,
        )
        .await
        .expect_err("credential-backed dynamic listing should fail closed without repo wiring");

    match err {
        Error::ServiceUnavailable(message) => {
            assert!(message.contains("alist"));
            assert!(message.contains("credential repository"));
        }
        other => panic!("expected ServiceUnavailable, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_media_cross_room_playlist_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo.create(&make_user("xroom_creator")).await.unwrap();

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
        source_provider: "direct_url".to_string(),
        provider_instance_name: Some("direct_url".to_string()),
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

    let requests: Vec<AddMediaRequest> = (0..101)
        .map(|i| AddMediaRequest {
            playlist_id: Some(playlist.id.clone()),
            name: format!("Batch Video {i}"),
            source_provider: "direct_url".to_string(),
            provider_instance_name: Some("direct_url".to_string()),
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

    let requests: Vec<AddMediaRequest> = (0..100)
        .map(|i| AddMediaRequest {
            playlist_id: Some(playlist.id.clone()),
            name: format!("Video {i}"),
            source_provider: "direct_url".to_string(),
            provider_instance_name: Some("direct_url".to_string()),
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
        source_provider: "direct_url".to_string(),
        provider_instance_name: Some("direct_url".to_string()),
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

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_move_media_rejects_conflicting_anchor_flags() {
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
        provider_instance_name: Some("direct_url".to_string()),
        creator_id: Some(owner.id.clone()),
        added_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 0,
    };
    let media = media_repo.create(&media).await.unwrap();

    let conflicting_anchor = room_service
        .media_service()
        .move_media(
            room.id.clone(),
            owner.id.clone(),
            synctv_core::service::media::MoveMediaRequest {
                media_ids: vec![media.id.clone()],
                source_playlist_id: None,
                target_playlist_id: None,
                all_from_scope: false,
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
            provider_instance_name: Some("direct_url".to_string()),
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
            provider_instance_name: Some("direct_url".to_string()),
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
                media_ids: vec![media2.id.clone()],
                source_playlist_id: None,
                target_playlist_id: None,
                all_from_scope: false,
                before_media_id: Some(media1.id.clone()),
                after_media_id: None,
            },
        )
        .await
        .unwrap();

    let updated1 = media_repo.get_by_id(&media1.id).await.unwrap().unwrap();
    let updated2 = media_repo.get_by_id(&media2.id).await.unwrap().unwrap();
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].id, media2.id);
    assert!(updated2.position < updated1.position);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_move_media_batch_preserves_request_order() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("move_media_batch_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Move Media Batch".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media_repo = synctv_core::repository::MediaRepository::new(pool.clone());

    let make_media = |name: &str, position: f64| synctv_core::models::Media {
        id: synctv_core::models::MediaId::new(),
        playlist_id: Some(playlist.id.clone()),
        room_id: room.id.clone(),
        name: name.to_string(),
        position,
        source_provider: "direct_url".to_string(),
        source_config: serde_json::json!({}),
        provider_instance_name: Some("direct_url".to_string()),
        creator_id: Some(owner.id.clone()),
        added_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 0,
    };

    let media1 = media_repo
        .create(&make_media("Media 1", 1024.0))
        .await
        .unwrap();
    let media2 = media_repo
        .create(&make_media("Media 2", 2048.0))
        .await
        .unwrap();
    let _media3 = media_repo
        .create(&make_media("Media 3", 3072.0))
        .await
        .unwrap();
    let media4 = media_repo
        .create(&make_media("Media 4", 4096.0))
        .await
        .unwrap();

    let moved = room_service
        .media_service()
        .move_media(
            room.id.clone(),
            owner.id.clone(),
            synctv_core::service::media::MoveMediaRequest {
                media_ids: vec![media4.id.clone(), media2.id.clone()],
                source_playlist_id: None,
                target_playlist_id: None,
                all_from_scope: false,
                before_media_id: Some(media1.id.clone()),
                after_media_id: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(moved.len(), 2);
    let ordered = media_repo.get_by_playlist(&playlist.id).await.unwrap();
    let ordered_names: Vec<String> = ordered.into_iter().map(|item| item.name).collect();
    assert_eq!(
        ordered_names,
        vec![
            "Media 4".to_string(),
            "Media 2".to_string(),
            "Media 1".to_string(),
            "Media 3".to_string()
        ]
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_move_media_to_another_playlist_appends_by_default() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("move_media_cross_playlist_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Move Media Cross Playlist".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let src = create_top_level_playlist(&pool, &room.id).await;
    let dst = synctv_core::repository::PlaylistRepository::new(pool.clone())
        .create(&Playlist {
            id: synctv_core::models::PlaylistId::new(),
            room_id: room.id.clone(),
            creator_id: Some(owner.id.clone()),
            name: "Destination".to_string(),
            parent_id: None,
            position: 1024.0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .unwrap();

    let media_repo = synctv_core::repository::MediaRepository::new(pool.clone());
    let moving = media_repo
        .create(&synctv_core::models::Media {
            id: synctv_core::models::MediaId::new(),
            playlist_id: Some(src.id.clone()),
            room_id: room.id.clone(),
            name: "Move Me".to_string(),
            position: 1024.0,
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({}),
            provider_instance_name: Some("direct_url".to_string()),
            creator_id: Some(owner.id.clone()),
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .unwrap();
    let existing = media_repo
        .create(&synctv_core::models::Media {
            id: synctv_core::models::MediaId::new(),
            playlist_id: Some(dst.id.clone()),
            room_id: room.id.clone(),
            name: "Already There".to_string(),
            position: 1024.0,
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({}),
            provider_instance_name: Some("direct_url".to_string()),
            creator_id: Some(owner.id.clone()),
            added_at: Utc::now(),
            updated_at: Utc::now(),
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
                media_ids: vec![moving.id.clone()],
                source_playlist_id: None,
                target_playlist_id: Some(dst.id.clone()),
                all_from_scope: false,
                before_media_id: None,
                after_media_id: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(moved.len(), 1);
    let moved_item = &moved[0];
    assert_eq!(moved_item.playlist_id.as_ref(), Some(&dst.id));
    assert!(moved_item.position > existing.position);
    assert!(media_repo
        .get_by_playlist(&src.id)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_move_all_media_from_scope_to_playlist_preserves_source_order() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("move_all_media_scope_owner"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Move All Media Scope".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let src = create_top_level_playlist(&pool, &room.id).await;
    let dst = synctv_core::repository::PlaylistRepository::new(pool.clone())
        .create(&Playlist {
            id: synctv_core::models::PlaylistId::new(),
            room_id: room.id.clone(),
            creator_id: Some(owner.id.clone()),
            name: "Target".to_string(),
            parent_id: None,
            position: 1024.0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .unwrap();

    let media_repo = synctv_core::repository::MediaRepository::new(pool.clone());
    let _a = media_repo
        .create(&synctv_core::models::Media {
            id: synctv_core::models::MediaId::new(),
            playlist_id: Some(src.id.clone()),
            room_id: room.id.clone(),
            name: "A".to_string(),
            position: 1024.0,
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({}),
            provider_instance_name: Some("direct_url".to_string()),
            creator_id: Some(owner.id.clone()),
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .unwrap();
    let _b = media_repo
        .create(&synctv_core::models::Media {
            id: synctv_core::models::MediaId::new(),
            playlist_id: Some(src.id.clone()),
            room_id: room.id.clone(),
            name: "B".to_string(),
            position: 2048.0,
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({}),
            provider_instance_name: Some("direct_url".to_string()),
            creator_id: Some(owner.id.clone()),
            added_at: Utc::now(),
            updated_at: Utc::now(),
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
                media_ids: Vec::new(),
                source_playlist_id: Some(src.id.clone()),
                target_playlist_id: Some(dst.id.clone()),
                all_from_scope: true,
                before_media_id: None,
                after_media_id: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(moved.len(), 2);
    let dst_names: Vec<String> = media_repo
        .get_by_playlist(&dst.id)
        .await
        .unwrap()
        .into_iter()
        .map(|item| item.name)
        .collect();
    assert_eq!(dst_names, vec!["A".to_string(), "B".to_string()]);
    assert!(media_repo
        .get_by_playlist(&src.id)
        .await
        .unwrap()
        .is_empty());
}
