//! Media cache invalidation tests
//!
//! Tests that media edits properly broadcast events.
//!

use chrono::Utc;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use synctv_core::{
    models::{
        MemberStatus, Playlist, PlaylistId, Room, RoomId, RoomMember, RoomRole, RoomStatus,
        SourceProvider, User, UserId, UserRole, UserStatus,
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
use synctv_core_testing::{create_test_pool, ok};

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

struct MediaEditFixture {
    room: Room,
    owner: User,
    playlist: Playlist,
    media_service: MediaService,
    notification_service: NotificationService,
}

async fn setup_media_edit_fixture(
    pool: &sqlx::PgPool,
    owner_name: &str,
    room_name: &str,
    notification_service: NotificationService,
) -> MediaEditFixture {
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    let owner = ok(
        user_repo.create(&make_user(owner_name)).await,
        "media edit owner should be created",
    );
    let room = ok(
        room_repo
            .create(&{
                let now = Utc::now();
                Room {
                    id: RoomId::new(),
                    name: room_name.to_string(),
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
            .await,
        "media edit room should be created",
    );

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
    ok(
        member_repo_setup.add(&owner_member).await,
        "media edit owner membership should be created",
    );

    let playlist = ok(
        playlist_repo
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
            .await,
        "media edit playlist should be created",
    );

    let member_repo = synctv_core::repository::RoomMemberRepository::new(pool.clone());
    let permission_service = ok(
        PermissionService::new(member_repo, room_repo.clone(), None, 1000, 300),
        "permission service should build",
    );

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
        providers: vec![SourceProvider::DirectUrl],
        enabled: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    ok(
        provider_repo.create(&provider).await,
        "direct_url provider row should be created",
    );

    let provider_instance_repo =
        synctv_core::repository::ProviderInstanceRepository::new(pool.clone());
    let remote_provider_manager =
        synctv_core::service::RemoteProviderManager::new(Arc::new(provider_instance_repo));
    let providers_manager = Arc::new(ok(
        ProvidersManager::new(Arc::new(remote_provider_manager)),
        "providers manager should build",
    ));

    ok(
        providers_manager
            .create_provider("direct_url", "direct_url", &json!({}))
            .await,
        "direct_url provider instance should be registered",
    );

    let media_service = MediaService::new(
        media_repo,
        playlist_repo,
        permission_service,
        providers_manager,
        notification_service.clone(),
    );

    MediaEditFixture {
        room,
        owner,
        playlist,
        media_service,
        notification_service,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_edit_media_sends_notification() {
    let (_container, pool) = create_test_pool().await;
    let fixture = setup_media_edit_fixture(
        &pool,
        "test_owner",
        "Test Room",
        NotificationService::default(),
    )
    .await;
    let mut rx = fixture.notification_service.subscribe();

    let add_req = AddMediaRequest {
        playlist_id: Some(fixture.playlist.id),
        name: "Test Media".to_string(),
        description: String::new(),
        source_provider: SourceProvider::DirectUrl,
        provider_instance_name: None,
        source_config: synctv_core_testing::direct_url_media_source_config(
            "https://example.com/video.mp4",
        ),
    };

    let media = ok(
        fixture
            .media_service
            .add_media(fixture.room.id, fixture.owner.id, add_req)
            .await,
        "media should be added before edit",
    );

    let edit_req = EditMediaRequest {
        media_id: media.id,
        name: Some("Updated Media".to_string()),
        description: None,
    };

    let updated_media = ok(
        fixture
            .media_service
            .edit_media(fixture.room.id, fixture.owner.id, edit_req)
            .await,
        "media should be edited",
    );

    assert_eq!(updated_media.name, "Updated Media");

    let mut found_update = false;
    for _ in 0..10 {
        let result = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
        match result {
            Ok(Ok((notif_room_id, event))) => {
                assert_eq!(notif_room_id, fixture.room.id);
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
                    other => std::panic::panic_any(format!(
                        "unexpected notification event while waiting for MediaUpdated: {other:?}"
                    )),
                }
            }
            Ok(Err(e)) => std::panic::panic_any(format!("channel error: {e}")),
            Err(_) => break,
        }
    }
    assert!(found_update, "Expected to receive MediaUpdated event");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "Requires Docker"]
async fn test_edit_media_without_notification_service_succeeds() {
    let (_container, pool) = create_test_pool().await;
    let fixture = setup_media_edit_fixture(
        &pool,
        "test_owner2",
        "Test Room 2",
        NotificationService::default(),
    )
    .await;

    let add_req = AddMediaRequest {
        playlist_id: Some(fixture.playlist.id),
        name: "Test Media".to_string(),
        description: String::new(),
        source_provider: SourceProvider::DirectUrl,
        provider_instance_name: None,
        source_config: synctv_core_testing::direct_url_media_source_config(
            "https://example.com/video.mp4",
        ),
    };

    let media = ok(
        fixture
            .media_service
            .add_media(fixture.room.id, fixture.owner.id, add_req)
            .await,
        "media should be added before edit",
    );

    let edit_req = EditMediaRequest {
        media_id: media.id,
        name: Some("Updated Media".to_string()),
        description: None,
    };

    let updated_media = ok(
        fixture
            .media_service
            .edit_media(fixture.room.id, fixture.owner.id, edit_req)
            .await,
        "media edit should succeed with default notification service",
    );

    assert_eq!(updated_media.name, "Updated Media");
}
