//! Media cache invalidation tests
//!
//! Tests that media edits properly broadcast events.
//!
//! Run with: cargo test -p synctv-core --test media_cache_invalidation_tests -- --nocapture

use std::sync::Arc;
use synctv_core_testing::{create_test_pool, create_test_jwt_service};
use synctv_core::{
    models::{Room, RoomId, RoomMember, RoomRole, MemberStatus, RoomStatus, UserId, User, UserRole, UserStatus, Playlist, PlaylistId},
    repository::{UserRepository, RoomRepository, RoomMemberRepository, PlaylistRepository, MediaRepository},
    service::{
        media::{MediaService, AddMediaRequest, EditMediaRequest},
        permission::PermissionService,
        notification::NotificationService,
        ProvidersManager,
    },
};
use chrono::Utc;
use serde_json::json;
use sqlx::PgPool;
use std::time::Duration;

/// Default PostgreSQL version for test containers
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

// ========== Test: Media edit with mock notification service ==========

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_edit_media_sends_notification() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    // Create test data
    let owner = user_repo.create(&make_user("test_owner")).await.unwrap();
    let room = room_repo.create(&{
        let now = Utc::now();
        Room {
            id: RoomId::new(),
            name: "Test Room".to_string(),
            description: String::new(),
            created_by: owner.id.clone(),
            status: RoomStatus::Active,
            is_banned: false,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            version: 0,
        }
    }).await.unwrap();

    // Add room owner as a member so permission checks pass
    let member_repo_setup = RoomMemberRepository::new(pool.clone());
    let owner_member = RoomMember {
        room_id: room.id.clone(),
        user_id: owner.id.clone(),
        role: RoomRole::Creator,
        status: MemberStatus::Active,
        added_permissions: 0,
        removed_permissions: 0,
        admin_added_permissions: 0,
        admin_removed_permissions: 0,
        joined_at: Utc::now(),
        left_at: None,
        version: 0,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    };
    member_repo_setup.add(&owner_member).await.expect("Failed to add owner as room member");

    let playlist = playlist_repo.create(&{
        let now = Utc::now();
        Playlist {
            id: PlaylistId::new(),
            room_id: room.id.clone(),
            creator_id: Some(owner.id.clone()),
            name: String::new(),
            parent_id: None,
            position: 0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: now,
            updated_at: now,
            version: 0,
        }
    }).await.unwrap();

    // Create a mock notification service that tracks calls
    use std::sync::atomic::{AtomicBool, Ordering};

    let notification_sent = Arc::new(AtomicBool::new(false));
    let notification_sent_clone = notification_sent.clone();

    struct MockBroadcaster {
        notified: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl synctv_core::service::notification::EventBroadcaster for MockBroadcaster {
        async fn broadcast_to_room(
            &self,
            _room_id: &RoomId,
            _event: &synctv_core::service::notification::RoomEvent,
        ) -> Result<usize, synctv_core::Error> {
            self.notified.store(true, Ordering::Release);
            Ok(1)
        }

        async fn send_to_user(
            &self,
            _room_id: &RoomId,
            _user_id: &UserId,
            _event: &synctv_core::service::notification::RoomEvent,
        ) -> Result<bool, synctv_core::Error> {
            Ok(true)
        }

        async fn broadcast_to_cluster(
            &self,
            _room_id: &RoomId,
            _event: &synctv_core::service::notification::RoomEvent,
        ) -> Result<(), synctv_core::Error> {
            Ok(())
        }
    }

    let broadcaster = Arc::new(MockBroadcaster {
        notified: notification_sent_clone,
    });

    let notification_service = NotificationService::new(broadcaster);
    let mut rx = notification_service.subscribe();

    // Create media service with notification
    let member_repo = synctv_core::repository::RoomMemberRepository::new(pool.clone());
    let permission_service = PermissionService::new(
        member_repo,
        room_repo.clone(),
        None, // No settings registry
        1000, // cache_size
        300,  // cache_ttl_secs
    );

    // Register direct_url provider instance BEFORE creating RemoteProviderManager
    // (the manager caches instances at init, so it must exist first)
    let provider_repo = synctv_core::repository::ProviderInstanceRepository::new(pool.clone());
    let provider = synctv_core::models::ProviderInstance {
        name: "direct_url".to_string(),
        endpoint: "grpc://localhost:50051".to_string(),
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

    let provider_instance_repo = synctv_core::repository::ProviderInstanceRepository::new(pool.clone());
    let remote_provider_manager = synctv_core::service::RemoteProviderManager::new(
        Arc::new(provider_instance_repo),
        None, // No Redis
        None, // No cluster manager
    );
    let providers_manager = Arc::new(ProvidersManager::new(Arc::new(remote_provider_manager)));

    // Register the "direct_url" provider in the in-memory instances map
    // (DB insert alone is insufficient — ProvidersManager.get() reads from memory)
    providers_manager
        .create_provider("direct_url", "direct_url", &json!({}))
        .await
        .expect("Failed to create direct_url provider instance");

    let mut media_service = MediaService::new(
        media_repo,
        playlist_repo,
        permission_service,
        providers_manager,
    );

    media_service.set_notification_service(notification_service);

    // Add media
    let add_req = AddMediaRequest {
        playlist_id: playlist.id.clone(),
        name: "Test Media".to_string(),
        provider_instance_name: "direct_url".to_string(),
        source_config: json!({
            "url": "https://example.com/video.mp4"
        }),
    };

    let media = media_service
        .add_media(room.id.clone(), owner.id.clone(), add_req)
        .await
        .expect("Failed to add media");

    // Edit media
    let edit_req = EditMediaRequest {
        media_id: media.id.clone(),
        name: Some("Updated Media".to_string()),
        position: None,
    };

    let updated_media = media_service
        .edit_media(room.id.clone(), owner.id.clone(), edit_req)
        .await
        .expect("Failed to edit media");

    assert_eq!(updated_media.name, "Updated Media");

    // Drain events until we find MediaUpdated (add_media also emits MediaAdded)
    let mut found_update = false;
    for _ in 0..10 {
        let result = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
        match result {
            Ok(Ok((notif_room_id, event))) => {
                assert_eq!(notif_room_id, room.id);
                if let synctv_core::service::notification::RoomEvent::MediaUpdated { media_id, title, .. } = event {
                    assert_eq!(media_id, media.id.as_str());
                    assert_eq!(title, "Updated Media");
                    found_update = true;
                    break;
                }
                // Otherwise it's a MediaAdded event from add_media — skip it
            }
            Ok(Err(e)) => panic!("Channel error: {e}"),
            Err(_) => break, // timeout
        }
    }
    assert!(found_update, "Expected to receive MediaUpdated event");

    assert!(notification_sent.load(Ordering::Acquire), "Mock broadcaster should have been called");
}

// ========== Test: Media edit without notification service doesn't panic ==========

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_edit_media_without_notification_service_succeeds() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    // Create test data
    let owner = user_repo.create(&make_user("test_owner2")).await.unwrap();
    let room = room_repo.create(&{
        let now = Utc::now();
        Room {
            id: RoomId::new(),
            name: "Test Room 2".to_string(),
            description: String::new(),
            created_by: owner.id.clone(),
            status: RoomStatus::Active,
            is_banned: false,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            version: 0,
        }
    }).await.unwrap();

    // Add room owner as a member so permission checks pass
    let member_repo_setup = RoomMemberRepository::new(pool.clone());
    let owner_member = RoomMember {
        room_id: room.id.clone(),
        user_id: owner.id.clone(),
        role: RoomRole::Creator,
        status: MemberStatus::Active,
        added_permissions: 0,
        removed_permissions: 0,
        admin_added_permissions: 0,
        admin_removed_permissions: 0,
        joined_at: Utc::now(),
        left_at: None,
        version: 0,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    };
    member_repo_setup.add(&owner_member).await.expect("Failed to add owner as room member");

    let playlist = playlist_repo.create(&{
        let now = Utc::now();
        Playlist {
            id: PlaylistId::new(),
            room_id: room.id.clone(),
            creator_id: Some(owner.id.clone()),
            name: String::new(),
            parent_id: None,
            position: 0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: now,
            updated_at: now,
            version: 0,
        }
    }).await.unwrap();

    // Create media service WITHOUT notification
    let member_repo = synctv_core::repository::RoomMemberRepository::new(pool.clone());
    let permission_service = PermissionService::new(
        member_repo,
        room_repo.clone(),
        None, // No settings registry
        1000, // cache_size
        300,  // cache_ttl_secs
    );

    // Register direct_url provider instance BEFORE creating RemoteProviderManager
    let provider_repo = synctv_core::repository::ProviderInstanceRepository::new(pool.clone());
    let provider = synctv_core::models::ProviderInstance {
        name: "direct_url".to_string(),
        endpoint: "grpc://localhost:50051".to_string(),
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

    let provider_instance_repo = synctv_core::repository::ProviderInstanceRepository::new(pool.clone());
    let remote_provider_manager = synctv_core::service::RemoteProviderManager::new(
        Arc::new(provider_instance_repo),
        None, // No Redis
        None, // No cluster manager
    );
    let providers_manager = Arc::new(ProvidersManager::new(Arc::new(remote_provider_manager)));

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
    );

    // Add media
    let add_req = AddMediaRequest {
        playlist_id: playlist.id.clone(),
        name: "Test Media".to_string(),
        provider_instance_name: "direct_url".to_string(),
        source_config: json!({
            "url": "https://example.com/video.mp4"
        }),
    };

    let media = media_service
        .add_media(room.id.clone(), owner.id.clone(), add_req)
        .await
        .expect("Failed to add media");

    // Edit media (should succeed even without notification service)
    let edit_req = EditMediaRequest {
        media_id: media.id.clone(),
        name: Some("Updated Media".to_string()),
        position: None,
    };

    let updated_media = media_service
        .edit_media(room.id.clone(), owner.id.clone(), edit_req)
        .await
        .expect("Failed to edit media");

    assert_eq!(updated_media.name, "Updated Media");
}
