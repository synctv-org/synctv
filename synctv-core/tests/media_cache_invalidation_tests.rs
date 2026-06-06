//! Media cache invalidation tests
//!
//! Tests that media edits properly broadcast events.
//!
#![allow(clippy::unwrap_used)]

use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use synctv_core::{
    models::{
        MemberStatus, Playlist, PlaylistId, Room, RoomId, RoomMember, RoomRole, RoomStatus, User,
        UserId, UserRole, UserStatus,
    },
    repository::{
        MediaRepository, PlaylistRepository, RoomMemberRepository, RoomRepository, UserRepository,
    },
    service::{
        media::{AddMediaRequest, EditMediaRequest, MediaService},
        notification::RoomEvent,
        permission::PermissionService,
        NotificationService, ProvidersManager,
    },
};
use synctv_core_testing::create_test_pool;

/// Default `PostgreSQL` version for test containers
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_edit_media_sends_notification() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("test_owner")).await.unwrap();
    let room = room_repo
        .create(&{
            let now = Utc::now();
            Room {
                id: RoomId::new(),
                name: "Test Room".to_string(),
                description: String::new(),
                cover_file_reference_id: None,
                created_by: owner.id,
                status: RoomStatus::Active,
                is_banned: false,
                closed_at: None,
                created_at: now,
                updated_at: now,
                deleted_at: None,
                version: 0,
                last_activity_at: now,
            }
        })
        .await
        .unwrap();

    // Add room owner as a member so permission checks pass
    let member_repo_setup = RoomMemberRepository::new(pool.clone());
    let owner_member = RoomMember {
        room_id: room.id,
        user_id: owner.id,
        role: RoomRole::Creator,
        status: MemberStatus::Active,
        added_permissions: 0,
        removed_permissions: 0,
        admin_added_permissions: 0,
        admin_removed_permissions: 0,
        joined_at: Utc::now(),
        version: 0,
    };
    member_repo_setup
        .add(&owner_member)
        .await
        .expect("Failed to add owner as room member");

    let playlist = playlist_repo
        .create(&{
            let now = Utc::now();
            Playlist {
                id: PlaylistId::new(),
                room_id: room.id,
                creator_id: Some(owner.id),
                name: String::new(),
                description: String::new(),
                cover_file_reference_id: None,
                parent_id: None,
                position: 0.0,
                source_provider: None,
                source_config: None,
                provider_instance_name: None,
                created_at: now,
                updated_at: now,
                version: 0,
            }
        })
        .await
        .unwrap();

    let notification_service = NotificationService::default();
    let mut rx = notification_service.subscribe();

    let member_repo = synctv_core::repository::RoomMemberRepository::new(pool.clone());
    let permission_service = PermissionService::new(
        member_repo,
        room_repo.clone(),
        None, // No settings registry
        1000, // cache_size
        300,  // cache_ttl_secs
    )
    .expect("permission service should build");

    // Register direct_url provider instance BEFORE creating RemoteProviderManager
    // (the manager caches instances at init, so it must exist first)
    let provider_repo = synctv_core::repository::ProviderInstanceRepository::new(pool.clone());
    let provider = synctv_core::models::ProviderInstance {
        name: "direct_url".to_string(),
        endpoint: "http://localhost:50051".to_string(),
        comment: None,
        jwt_secret: None,
        custom_ca: None,
        timeout: "30".to_string(),
        tls: false,
        insecure_tls: false,
        providers: vec!["direct_url".to_string()],
        enabled: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    provider_repo.create(&provider).await.unwrap();

    let provider_instance_repo =
        synctv_core::repository::ProviderInstanceRepository::new(pool.clone());
    let remote_provider_manager =
        synctv_core::service::RemoteProviderManager::new(Arc::new(provider_instance_repo));
    let providers_manager = Arc::new(
        ProvidersManager::new(Arc::new(remote_provider_manager))
            .expect("providers manager should build"),
    );

    // Register the "direct_url" provider in the in-memory instances map
    // (DB insert alone is insufficient — ProvidersManager.get() reads from memory)
    providers_manager
        .create_provider("direct_url", "direct_url", &json!({}))
        .await
        .expect("Failed to create direct_url provider instance");

    let media_service = MediaService::new(
        media_repo,
        playlist_repo,
        permission_service,
        providers_manager,
        notification_service.clone(),
    );

    // Add media
    let add_req = AddMediaRequest {
        playlist_id: Some(playlist.id),
        name: "Test Media".to_string(),
        description: String::new(),
        source_provider: "direct_url".to_string(),
        provider_instance_name: None,
        source_config: json!({
            "url": "https://example.com/video.mp4"
        }),
    };

    let media = media_service
        .add_media(room.id, owner.id, add_req)
        .await
        .expect("Failed to add media");

    // Edit media
    let edit_req = EditMediaRequest {
        media_id: media.id,
        name: Some("Updated Media".to_string()),
        description: None,
    };

    let updated_media = media_service
        .edit_media(room.id, owner.id, edit_req)
        .await
        .expect("Failed to edit media");

    assert_eq!(updated_media.name, "Updated Media");

    let mut found_update = false;
    for _ in 0..10 {
        let result = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
        match result {
            Ok(Ok((notif_room_id, event))) => {
                assert_eq!(notif_room_id, room.id);
                match event {
                    RoomEvent::MediaUpdated {
                        media_id, title, ..
                    } => {
                        assert_eq!(media_id, media.id);
                        assert_eq!(title, "Updated Media");
                        found_update = true;
                        break;
                    }
                    RoomEvent::MediaAdded { .. } => {}
                    other => panic!(
                        "unexpected notification event while waiting for MediaUpdated: {other:?}"
                    ),
                }
            }
            Ok(Err(e)) => panic!("Channel error: {e}"),
            Err(_) => break, // timeout
        }
    }
    assert!(found_update, "Expected to receive MediaUpdated event");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_edit_media_without_notification_service_succeeds() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    let owner = user_repo.create(&make_user("test_owner2")).await.unwrap();
    let room = room_repo
        .create(&{
            let now = Utc::now();
            Room {
                id: RoomId::new(),
                name: "Test Room 2".to_string(),
                description: String::new(),
                cover_file_reference_id: None,
                created_by: owner.id,
                status: RoomStatus::Active,
                is_banned: false,
                closed_at: None,
                created_at: now,
                updated_at: now,
                deleted_at: None,
                version: 0,
                last_activity_at: now,
            }
        })
        .await
        .unwrap();

    // Add room owner as a member so permission checks pass
    let member_repo_setup = RoomMemberRepository::new(pool.clone());
    let owner_member = RoomMember {
        room_id: room.id,
        user_id: owner.id,
        role: RoomRole::Creator,
        status: MemberStatus::Active,
        added_permissions: 0,
        removed_permissions: 0,
        admin_added_permissions: 0,
        admin_removed_permissions: 0,
        joined_at: Utc::now(),
        version: 0,
    };
    member_repo_setup
        .add(&owner_member)
        .await
        .expect("Failed to add owner as room member");

    let playlist = playlist_repo
        .create(&{
            let now = Utc::now();
            Playlist {
                id: PlaylistId::new(),
                room_id: room.id,
                creator_id: Some(owner.id),
                name: String::new(),
                description: String::new(),
                cover_file_reference_id: None,
                parent_id: None,
                position: 0.0,
                source_provider: None,
                source_config: None,
                provider_instance_name: None,
                created_at: now,
                updated_at: now,
                version: 0,
            }
        })
        .await
        .unwrap();

    let member_repo = synctv_core::repository::RoomMemberRepository::new(pool.clone());
    let permission_service = PermissionService::new(
        member_repo,
        room_repo.clone(),
        None, // No settings registry
        1000, // cache_size
        300,  // cache_ttl_secs
    )
    .expect("permission service should build");

    // Register direct_url provider instance BEFORE creating RemoteProviderManager
    let provider_repo = synctv_core::repository::ProviderInstanceRepository::new(pool.clone());
    let provider = synctv_core::models::ProviderInstance {
        name: "direct_url".to_string(),
        endpoint: "http://localhost:50051".to_string(),
        comment: None,
        jwt_secret: None,
        custom_ca: None,
        timeout: "30".to_string(),
        tls: false,
        insecure_tls: false,
        providers: vec!["direct_url".to_string()],
        enabled: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    provider_repo.create(&provider).await.unwrap();

    let provider_instance_repo =
        synctv_core::repository::ProviderInstanceRepository::new(pool.clone());
    let remote_provider_manager =
        synctv_core::service::RemoteProviderManager::new(Arc::new(provider_instance_repo));
    let providers_manager = Arc::new(
        ProvidersManager::new(Arc::new(remote_provider_manager))
            .expect("providers manager should build"),
    );

    // Register the "direct_url" provider in the in-memory instances map
    providers_manager
        .create_provider("direct_url", "direct_url", &json!({}))
        .await
        .expect("Failed to create direct_url provider instance");

    let media_service = MediaService::new(
        media_repo,
        playlist_repo,
        permission_service,
        providers_manager,
        NotificationService::default(),
    );

    // Add media
    let add_req = AddMediaRequest {
        playlist_id: Some(playlist.id),
        name: "Test Media".to_string(),
        description: String::new(),
        source_provider: "direct_url".to_string(),
        provider_instance_name: None,
        source_config: json!({
            "url": "https://example.com/video.mp4"
        }),
    };

    let media = media_service
        .add_media(room.id, owner.id, add_req)
        .await
        .expect("Failed to add media");

    // Edit media (should succeed even without notification service)
    let edit_req = EditMediaRequest {
        media_id: media.id,
        name: Some("Updated Media".to_string()),
        description: None,
    };

    let updated_media = media_service
        .edit_media(room.id, owner.id, edit_req)
        .await
        .expect("Failed to edit media");

    assert_eq!(updated_media.name, "Updated Media");
}
