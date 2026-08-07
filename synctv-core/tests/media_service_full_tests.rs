//! `MediaService` integration tests (S8/S9)
//!
//! Tests `add_media` permission check, `add_media_batch` size limit,
//! `edit_media` cross-room check and optimistic lock retry with real `PostgreSQL`.
//!

use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    credential_encryption::CredentialEncryption,
    models::{
        AlistPlaylistSourceConfig, BilibiliMediaSourceConfig, BilibiliVideoSourceConfig,
        MediaSourceConfig, Playlist, PlaylistSourceConfig, RoomMemberPermissionBits,
        SourceProvider, User, UserId, UserRole, UserStatus,
    },
    provider::DynamicListQuery,
    repository::{ProviderInstanceRepository, UserProviderCredentialRepository, UserRepository},
    service::{
        AddMediaRequest, BackendPlaybackRequest, BruteForceProtection, CreatePlaylistRequest,
        EditMediaRequest, InMemoryTokenBlacklistStore, JwtService, ProvidersManager,
        RemoteProviderManager, RoomService, RoomServiceOptions, UserService,
    },
    Error,
};
use synctv_core_testing::create_test_pool;
use synctv_core_testing::{TestOptionExt, TestResultExt};

fn make_user_service(pool: &PgPool) -> UserService {
    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = JwtService::new(secret).checked("JWT service should be created");
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    UserService::new_for_tests(
        pool,
        jwt_service,
        username_cache,
        token_blacklist,
        key_builder,
        brute_force,
    )
}

fn make_room_service(pool: PgPool) -> RoomService {
    let user_service = make_user_service(&pool);

    RoomService::new_for_tests(pool, user_service).checked("room service should build")
}

fn make_room_service_with_provider_credentials(pool: PgPool) -> RoomService {
    let user_service = make_user_service(&pool);
    let credential_encryption =
        CredentialEncryption::new(&[0x42; 32]).checked("test encryption key should be valid");
    let credential_repo = Arc::new(UserProviderCredentialRepository::new_with_encryption(
        pool.clone(),
        credential_encryption.clone(),
    ));

    RoomService::new_with_options(
        pool.clone(),
        user_service,
        RoomServiceOptions {
            credential_encryption: Some(credential_encryption),
            credential_repo: Some(credential_repo),
            ..RoomServiceOptions::test_defaults_with_settings(pool)
        },
    )
    .checked("room service should build")
}

fn make_room_service_with_disabled_provider_ssrf(pool: PgPool) -> RoomService {
    let user_service = make_user_service(&pool);
    let provider_instance_repo = ProviderInstanceRepository::new(pool.clone());
    let provider_instance_manager =
        Arc::new(RemoteProviderManager::new(Arc::new(provider_instance_repo)));
    let providers_manager = Arc::new(
        ProvidersManager::new_with_ssrf_guard(
            provider_instance_manager,
            synctv_common::ssrf::SsrfGuard::disabled(),
        )
        .checked("providers manager should build"),
    );

    RoomService::new_with_providers_for_tests(pool, user_service, providers_manager)
        .checked("room service should build")
}

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

async fn create_top_level_playlist(
    pool: &PgPool,
    room_id: &synctv_core::models::RoomId,
) -> Playlist {
    let playlist = Playlist {
        id: synctv_core::models::PlaylistId::new(),
        room_id: *room_id,
        creator_id: None,
        name: "Top Level".to_string(),
        description: String::new(),
        cover_file_reference_id: None,
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
        .checked("top-level playlist should be created")
}

/// Register the default local `direct_url` provider used when `provider_instance_name` is `None`.
async fn register_direct_url_provider(room_service: &RoomService) {
    if let Err(error) = room_service
        .media_service()
        .providers_manager()
        .create_provider_with_default_config("direct_url", "direct_url")
        .await
    {
        std::panic::panic_any(format!(
            "direct_url provider should be registered: {error:?}"
        ));
    }
}

async fn register_bilibili_provider(room_service: &RoomService) {
    if let Err(error) = room_service
        .media_service()
        .providers_manager()
        .create_provider_with_default_config("bilibili", "bilibili")
        .await
    {
        std::panic::panic_any(format!("bilibili provider should be registered: {error:?}"));
    }
}

async fn register_alist_provider(room_service: &RoomService) {
    if let Err(error) = room_service
        .media_service()
        .providers_manager()
        .create_provider_with_default_config("alist", "alist")
        .await
    {
        std::panic::panic_any(format!("alist provider should be registered: {error:?}"));
    }
}

async fn register_live_proxy_provider(room_service: &RoomService) {
    if let Err(error) = room_service
        .media_service()
        .providers_manager()
        .create_provider_with_default_config("live_proxy", "live_proxy")
        .await
    {
        std::panic::panic_any(format!(
            "live_proxy provider should be registered: {error:?}"
        ));
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_media_without_permission_denied() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("addm_creator"))
        .await
        .checked("test operation should succeed");
    let member = user_repo
        .create(&make_user("addm_member"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Add Media Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    room_service
        .join_room(room.id, member.id, None)
        .await
        .checked("test operation should succeed");
    register_direct_url_provider(&room_service).await;

    // Revoke MANAGE_OWN_MEDIA from member
    room_service
        .member_service()
        .revoke_permission(
            room.id,
            creator.id,
            member.id,
            RoomMemberPermissionBits::MANAGE_OWN_MEDIA,
        )
        .await
        .checked("test operation should succeed");

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media_service = room_service.media_service();

    let request = AddMediaRequest {
        playlist_id: Some(playlist.id),
        name: "Forbidden Video".to_string(),
        description: String::new(),
        source_provider: SourceProvider::DirectUrl,
        provider_instance_name: None,
        source_config: synctv_core_testing::direct_url_media_source_config(
            "https://example.com/vid.mp4",
        ),
    };

    let result = media_service.add_media(room.id, member.id, request).await;

    assert!(
        result.is_err(),
        "Should fail without MANAGE_OWN_MEDIA permission"
    );
    match result.failed("operation should fail") {
        Error::Authorization(_) => {}
        other => std::panic::panic_any(format!("expected Authorization error, got: {other:?}")),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "Requires Docker"]
async fn test_add_media_with_permission_succeeds() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("addm2_creator"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Add Media OK Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    register_direct_url_provider(&room_service).await;

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media_service = room_service.media_service();

    let request = AddMediaRequest {
        playlist_id: Some(playlist.id),
        name: "Good Video".to_string(),
        description: String::new(),
        source_provider: SourceProvider::DirectUrl,
        provider_instance_name: None,
        source_config: synctv_core_testing::direct_url_media_source_config(
            "https://example.com/good.mp4",
        ),
    };

    let result = media_service.add_media(room.id, creator.id, request).await;

    assert!(result.is_ok(), "Creator should be able to add media");
    let media = result.checked("test operation should succeed");
    assert_eq!(media.name, "Good Video");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_media_rejects_missing_shared_bilibili_credential() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service_with_provider_credentials(pool.clone());

    let creator = user_repo
        .create(&make_user("addm_bili_creator"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Add Media Bilibili Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    register_bilibili_provider(&room_service).await;

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media_service = room_service.media_service();

    let request = AddMediaRequest {
        playlist_id: Some(playlist.id),
        name: "Bilibili Video".to_string(),
        description: String::new(),
        source_provider: SourceProvider::Bilibili,
        provider_instance_name: None,
        source_config: MediaSourceConfig::Bilibili(BilibiliMediaSourceConfig::Video(
            BilibiliVideoSourceConfig {
                bvid: Some("BV1GJ411x7gL".to_string()),
                aid: None,
                cid: 12345,
                shared: true,
            },
        )),
    };

    let err = media_service
        .add_media(room.id, creator.id, request)
        .await
        .failed("shared Bilibili media should require the creator credential");

    match err {
        Error::InvalidInput(message) => {
            assert!(message.contains("missing credential for provider 'bilibili'"));
        }
        other => std::panic::panic_any(format!("expected InvalidInput, got {other:?}")),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_media_with_bilibili_without_repo_allows_anonymous_playback() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("addm_bili_missing_repo"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Add Media Missing Repo".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    register_bilibili_provider(&room_service).await;

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let request = AddMediaRequest {
        playlist_id: Some(playlist.id),
        name: "Bilibili Missing Repo".to_string(),
        description: String::new(),
        source_provider: SourceProvider::Bilibili,
        provider_instance_name: None,
        source_config: synctv_core_testing::bilibili_video_media_source_config(
            "BV1GJ411x7gL",
            12345,
            false,
        ),
    };

    let media = room_service
        .media_service()
        .add_media(room.id, creator.id, request)
        .await
        .checked("Bilibili media should allow anonymous playback without credential repo");

    assert_eq!(media.source_provider, SourceProvider::Bilibili);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_backend_playback_for_static_live_proxy_binds_media_id() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service_with_disabled_provider_ssrf(pool.clone());

    let creator = user_repo
        .create(&make_user("live_proxy_backend_creator"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Live Proxy Backend Playback Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    register_live_proxy_provider(&room_service).await;

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media = room_service
        .media_service()
        .add_media(
            room.id,
            creator.id,
            AddMediaRequest {
                playlist_id: Some(playlist.id),
                name: "Live Proxy Source".to_string(),
                description: String::new(),
                source_provider: SourceProvider::LiveProxy,
                provider_instance_name: None,
                source_config: synctv_core_testing::live_proxy_pull_live_media_source_config(
                    "http://127.0.0.1/live/source.flv",
                ),
            },
        )
        .await
        .checked("live proxy media should be added with disabled SSRF in test provider");

    let playback = room_service
        .media_service()
        .generate_backend_playback_for_source(BackendPlaybackRequest {
            room_id: room.id,
            media_id: Some(media.id),
            playlist_id: None,
            target: None,
        })
        .await
        .checked("backend playback should be generated")
        .checked("static media should resolve to playback");

    assert_eq!(playback.default_mode, "hls");
    assert!(playback.playback_infos.contains_key("flv"));
    let metadata = playback
        .metadata
        .expect("live proxy playback should include metadata");
    let synctv_core::models::PlaybackMetadata::LiveProxy(metadata) = metadata else {
        panic!("live proxy playback should include live proxy metadata");
    };
    assert_eq!(metadata.media_id, media.id);
    assert_eq!(metadata.room_id, room.id);
    assert!(metadata.source_host.is_some());
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
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Dynamic Playlist Missing Repo".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    register_alist_provider(&room_service).await;

    let request = CreatePlaylistRequest {
        room_id: room.id,
        name: "Alist Dynamic".to_string(),
        description: String::new(),
        parent_id: None,
        source_provider: Some(SourceProvider::Alist),
        source_config: Some(PlaylistSourceConfig::Alist(AlistPlaylistSourceConfig {
            path: "/media/library".to_string(),
            server_id: "alist-server".to_string(),
            password: None,
        })),
        provider_instance_name: None,
    };

    let err = room_service
        .playlist_service()
        .create_playlist(room.id, creator.id, request)
        .await
        .failed("credential-backed dynamic playlist should fail closed without repo wiring");

    match err {
        Error::ServiceUnavailable(message) => {
            assert!(message.contains("alist"));
            assert!(message.contains("credential repository"));
        }
        other => std::panic::panic_any(format!("expected ServiceUnavailable, got {other:?}")),
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
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Dynamic Playlist Runtime Missing Repo".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    register_alist_provider(&room_service).await;

    let playlist = Playlist {
        id: synctv_core::models::PlaylistId::new(),
        room_id: room.id,
        creator_id: Some(creator.id),
        name: "Persisted Alist Dynamic".to_string(),
        description: String::new(),
        cover_file_reference_id: None,
        parent_id: None,
        position: 0.0,
        source_provider: Some(SourceProvider::Alist),
        source_config: Some(PlaylistSourceConfig::Alist(AlistPlaylistSourceConfig {
            path: "/media/library".to_string(),
            server_id: "alist-server".to_string(),
            password: None,
        })),
        provider_instance_name: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    let playlist = synctv_core::repository::PlaylistRepository::new(pool.clone())
        .create(&playlist)
        .await
        .checked("test operation should succeed");

    let err = room_service
        .media_service()
        .list_dynamic_playlist_items(
            room.id,
            creator.id,
            &playlist.id,
            None,
            DynamicListQuery {
                pagination: synctv_core::provider::DynamicPagination::Page { page: 1 },
                page_size: 20,
                ..DynamicListQuery::default()
            },
        )
        .await
        .failed("credential-backed dynamic listing should fail closed without repo wiring");

    match err {
        Error::ServiceUnavailable(message) => {
            assert!(message.contains("alist"));
            assert!(message.contains("credential repository"));
        }
        other => std::panic::panic_any(format!("expected ServiceUnavailable, got {other:?}")),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_media_cross_room_playlist_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("xroom_creator"))
        .await
        .checked("test operation should succeed");

    let (room_a, _) = room_service
        .create_room("Room A".to_string(), String::new(), creator.id, None, None)
        .await
        .checked("test operation should succeed");
    let (room_b, _) = room_service
        .create_room("Room B".to_string(), String::new(), creator.id, None, None)
        .await
        .checked("test operation should succeed");

    register_direct_url_provider(&room_service).await;

    // Get playlist from room B
    let playlist_b = create_top_level_playlist(&pool, &room_b.id).await;
    let media_service = room_service.media_service();

    // Try to add media to room A using room B's playlist
    let request = AddMediaRequest {
        playlist_id: Some(playlist_b.id),
        name: "Cross Room Video".to_string(),
        description: String::new(),
        source_provider: SourceProvider::DirectUrl,
        provider_instance_name: None,
        source_config: synctv_core_testing::direct_url_media_source_config(
            "https://example.com/cross.mp4",
        ),
    };

    let result = media_service
        .add_media(room_a.id, creator.id, request)
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

    let creator = user_repo
        .create(&make_user("batch_creator"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Batch Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    register_direct_url_provider(&room_service).await;
    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media_service = room_service.media_service();

    let requests: Vec<AddMediaRequest> = (0..101)
        .map(|i| AddMediaRequest {
            playlist_id: Some(playlist.id),
            name: format!("Batch Video {i}"),
            description: String::new(),
            source_provider: SourceProvider::DirectUrl,
            provider_instance_name: None,
            source_config: synctv_core_testing::direct_url_media_source_config(format!(
                "https://example.com/batch{i}.mp4"
            )),
        })
        .collect();

    let result = media_service
        .add_media_batch(room.id, creator.id, Some(playlist.id), requests)
        .await;

    assert!(result.is_err(), "Batch of 101 items should be rejected");
    match result.failed("operation should fail") {
        Error::InvalidInput(msg) => {
            assert!(
                msg.contains("100") || msg.contains("batch") || msg.contains("exceed"),
                "Should mention batch size limit: {msg}"
            );
        }
        other => std::panic::panic_any(format!("expected InvalidInput error, got: {other:?}")),
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
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Batch Empty Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let media_service = room_service.media_service();

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let result = media_service
        .add_media_batch(room.id, creator.id, Some(playlist.id), vec![])
        .await;

    assert!(result.is_ok(), "Empty batch should succeed");
    let media_list = result.checked("test operation should succeed");
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
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Batch 100 Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    register_direct_url_provider(&room_service).await;
    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media_service = room_service.media_service();

    let requests: Vec<AddMediaRequest> = (0..100)
        .map(|i| AddMediaRequest {
            playlist_id: Some(playlist.id),
            name: format!("Video {i}"),
            description: String::new(),
            source_provider: SourceProvider::DirectUrl,
            provider_instance_name: None,
            source_config: synctv_core_testing::direct_url_media_source_config(format!(
                "https://example.com/v{i}.mp4"
            )),
        })
        .collect();

    let result = media_service
        .add_media_batch(room.id, creator.id, Some(playlist.id), requests)
        .await;

    assert!(
        result.is_ok(),
        "Batch of exactly 100 items should be accepted"
    );
    let media_list = result.checked("test operation should succeed");
    assert_eq!(
        media_list.len(),
        100,
        "Should return 100 created media items"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "Requires Docker"]
async fn test_add_media_batch_uses_batch_target_playlist() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("batch_target_creator"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Batch Target Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    register_direct_url_provider(&room_service).await;
    let target_playlist = create_top_level_playlist(&pool, &room.id).await;
    let stray_playlist = create_top_level_playlist(&pool, &room.id).await;

    let requests: Vec<AddMediaRequest> = (0..2)
        .map(|i| AddMediaRequest {
            playlist_id: Some(stray_playlist.id),
            name: format!("Targeted Video {i}"),
            description: String::new(),
            source_provider: SourceProvider::DirectUrl,
            provider_instance_name: None,
            source_config: synctv_core_testing::direct_url_media_source_config(format!(
                "https://example.com/target{i}.mp4"
            )),
        })
        .collect();

    let media_list = room_service
        .media_service()
        .add_media_batch(room.id, creator.id, Some(target_playlist.id), requests)
        .await
        .checked("batch add should succeed");

    assert_eq!(media_list.len(), 2);
    assert!(
        media_list
            .iter()
            .all(|media| media.playlist_id == Some(target_playlist.id)),
        "batch add should use the batch target playlist for every item"
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
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Edit OLR Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    register_direct_url_provider(&room_service).await;
    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media_service = room_service.media_service();

    // Add a media item
    let add_req = AddMediaRequest {
        playlist_id: Some(playlist.id),
        name: "Original Name".to_string(),
        description: String::new(),
        source_provider: SourceProvider::DirectUrl,
        provider_instance_name: None,
        source_config: synctv_core_testing::direct_url_media_source_config(
            "https://example.com/edit.mp4",
        ),
    };
    let media = media_service
        .add_media(room.id, creator.id, add_req)
        .await
        .checked("test operation should succeed");

    // Continuously bump media version to trigger retry exhaustion
    let media_id = media.id.as_i64();
    let pool_clone = pool.clone();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = stop.clone();

    let bumper = tokio::spawn(async move {
        while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
            sqlx::query!(
                "UPDATE media SET position = position + 1 WHERE id = $1::bigint",
                media_id
            )
            .execute(&pool_clone)
            .await?;
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        Ok::<_, sqlx::Error>(())
    });

    let edit_req = EditMediaRequest {
        media_id: media.id,
        name: Some("Updated Name".to_string()),
        description: None,
    };

    let result = media_service
        .edit_media(room.id, creator.id, edit_req)
        .await;

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    bumper
        .await
        .checked("bumper task should not panic")
        .checked("bumper task should update media positions");

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
            std::panic::panic_any("OptimisticLockConflict should not leak to caller".to_string());
        }
        Err(other) => {
            std::panic::panic_any(format!("unexpected edit media error: {other:?}"));
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
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Move Media Validation".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playlist = create_top_level_playlist(&pool, &room.id).await;

    let media_repo = synctv_core::repository::MediaRepository::new(pool.clone());
    let media = synctv_core::models::Media {
        id: synctv_core::models::MediaId::new(),
        playlist_id: Some(playlist.id),
        room_id: room.id,
        name: "Media".to_string(),
        description: String::new(),
        position: 1024.0,
        source_provider: SourceProvider::DirectUrl,
        source_config: synctv_core_testing::direct_url_media_source_config(
            "https://example.com/media.mp4",
        ),
        provider_instance_name: None,
        creator_id: Some(owner.id),
        cover_file_reference_id: None,
        thumbnail_file_reference_id: None,
        added_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 0,
    };
    let media = media_repo
        .create(&media)
        .await
        .checked("test operation should succeed");

    let conflicting_anchor = room_service
        .media_service()
        .move_media(
            room.id,
            owner.id,
            synctv_core::service::MoveMediaRequest {
                media_ids: vec![media.id],
                source_playlist_id: None,
                target_playlist_id: None,
                all_from_scope: false,
                before_media_id: Some(media.id),
                after_media_id: Some(media.id),
            },
        )
        .await
        .failed("operation should fail");
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
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Move Media Order".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media_repo = synctv_core::repository::MediaRepository::new(pool.clone());

    let media1 = media_repo
        .create(&synctv_core::models::Media {
            id: synctv_core::models::MediaId::new(),
            playlist_id: Some(playlist.id),
            room_id: room.id,
            name: "Media 1".to_string(),
            description: String::new(),
            position: 1024.0,
            source_provider: SourceProvider::DirectUrl,
            source_config: synctv_core_testing::direct_url_media_source_config(
                "https://example.com/media-1.mp4",
            ),
            provider_instance_name: None,
            creator_id: Some(owner.id),
            cover_file_reference_id: None,
            thumbnail_file_reference_id: None,
            added_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 0,
        })
        .await
        .checked("test operation should succeed");
    let media2 = media_repo
        .create(&synctv_core::models::Media {
            id: synctv_core::models::MediaId::new(),
            playlist_id: Some(playlist.id),
            room_id: room.id,
            name: "Media 2".to_string(),
            description: String::new(),
            position: 2048.0,
            source_provider: SourceProvider::DirectUrl,
            source_config: synctv_core_testing::direct_url_media_source_config(
                "https://example.com/media-2.mp4",
            ),
            provider_instance_name: None,
            creator_id: Some(owner.id),
            cover_file_reference_id: None,
            thumbnail_file_reference_id: None,
            added_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 0,
        })
        .await
        .checked("test operation should succeed");

    let moved = room_service
        .media_service()
        .move_media(
            room.id,
            owner.id,
            synctv_core::service::MoveMediaRequest {
                media_ids: vec![media2.id],
                source_playlist_id: None,
                target_playlist_id: None,
                all_from_scope: false,
                before_media_id: Some(media1.id),
                after_media_id: None,
            },
        )
        .await
        .checked("test operation should succeed");

    let updated1 = media_repo
        .get_by_id(&media1.id)
        .await
        .checked("test operation should succeed")
        .checked("test operation should succeed");
    let updated2 = media_repo
        .get_by_id(&media2.id)
        .await
        .checked("test operation should succeed")
        .checked("test operation should succeed");
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
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Move Media Batch".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media_repo = synctv_core::repository::MediaRepository::new(pool.clone());

    let make_media = |name: &str, position: f64| synctv_core::models::Media {
        id: synctv_core::models::MediaId::new(),
        playlist_id: Some(playlist.id),
        room_id: room.id,
        name: name.to_string(),
        description: String::new(),
        position,
        source_provider: SourceProvider::DirectUrl,
        source_config: synctv_core_testing::direct_url_media_source_config(format!(
            "https://example.com/{name}.mp4"
        )),
        provider_instance_name: None,
        creator_id: Some(owner.id),
        cover_file_reference_id: None,
        thumbnail_file_reference_id: None,
        added_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 0,
    };

    let media1 = media_repo
        .create(&make_media("Media 1", 1024.0))
        .await
        .checked("test operation should succeed");
    let media2 = media_repo
        .create(&make_media("Media 2", 2048.0))
        .await
        .checked("test operation should succeed");
    let _media3 = media_repo
        .create(&make_media("Media 3", 3072.0))
        .await
        .checked("test operation should succeed");
    let media4 = media_repo
        .create(&make_media("Media 4", 4096.0))
        .await
        .checked("test operation should succeed");

    let moved = room_service
        .media_service()
        .move_media(
            room.id,
            owner.id,
            synctv_core::service::MoveMediaRequest {
                media_ids: vec![media4.id, media2.id],
                source_playlist_id: None,
                target_playlist_id: None,
                all_from_scope: false,
                before_media_id: Some(media1.id),
                after_media_id: None,
            },
        )
        .await
        .checked("test operation should succeed");

    assert_eq!(moved.len(), 2);
    let ordered = media_repo
        .get_by_playlist(&playlist.id)
        .await
        .checked("test operation should succeed");
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
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Move Media Cross Playlist".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let src = create_top_level_playlist(&pool, &room.id).await;
    let dst = synctv_core::repository::PlaylistRepository::new(pool.clone())
        .create(&Playlist {
            id: synctv_core::models::PlaylistId::new(),
            room_id: room.id,
            creator_id: Some(owner.id),
            name: "Destination".to_string(),
            description: String::new(),
            cover_file_reference_id: None,
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
        .checked("test operation should succeed");

    let media_repo = synctv_core::repository::MediaRepository::new(pool.clone());
    let moving = media_repo
        .create(&synctv_core::models::Media {
            id: synctv_core::models::MediaId::new(),
            playlist_id: Some(src.id),
            room_id: room.id,
            name: "Move Me".to_string(),
            description: String::new(),
            position: 1024.0,
            source_provider: SourceProvider::DirectUrl,
            source_config: synctv_core_testing::direct_url_media_source_config(
                "https://example.com/move-me.mp4",
            ),
            provider_instance_name: None,
            creator_id: Some(owner.id),
            cover_file_reference_id: None,
            thumbnail_file_reference_id: None,
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .checked("test operation should succeed");
    let existing = media_repo
        .create(&synctv_core::models::Media {
            id: synctv_core::models::MediaId::new(),
            playlist_id: Some(dst.id),
            room_id: room.id,
            name: "Already There".to_string(),
            description: String::new(),
            position: 1024.0,
            source_provider: SourceProvider::DirectUrl,
            source_config: synctv_core_testing::direct_url_media_source_config(
                "https://example.com/already-there.mp4",
            ),
            provider_instance_name: None,
            creator_id: Some(owner.id),
            cover_file_reference_id: None,
            thumbnail_file_reference_id: None,
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .checked("test operation should succeed");

    let moved = room_service
        .media_service()
        .move_media(
            room.id,
            owner.id,
            synctv_core::service::MoveMediaRequest {
                media_ids: vec![moving.id],
                source_playlist_id: None,
                target_playlist_id: Some(dst.id),
                all_from_scope: false,
                before_media_id: None,
                after_media_id: None,
            },
        )
        .await
        .checked("test operation should succeed");

    assert_eq!(moved.len(), 1);
    let moved_item = &moved[0];
    assert_eq!(moved_item.playlist_id.as_ref(), Some(&dst.id));
    assert!(moved_item.position > existing.position);
    assert!(media_repo
        .get_by_playlist(&src.id)
        .await
        .checked("test operation should succeed")
        .is_empty());

    let moved_to_root = room_service
        .media_service()
        .move_media(
            room.id,
            owner.id,
            synctv_core::service::MoveMediaRequest {
                media_ids: vec![moving.id],
                source_playlist_id: None,
                target_playlist_id: None,
                all_from_scope: false,
                before_media_id: None,
                after_media_id: None,
            },
        )
        .await
        .checked("test operation should succeed");

    assert_eq!(moved_to_root.len(), 1);
    assert_eq!(moved_to_root[0].playlist_id, None);
    let remaining_in_destination = media_repo
        .get_by_playlist(&dst.id)
        .await
        .checked("test operation should succeed");
    assert_eq!(remaining_in_destination.len(), 1);
    assert_eq!(remaining_in_destination[0].id, existing.id);
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
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Move All Media Scope".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let src = create_top_level_playlist(&pool, &room.id).await;
    let dst = synctv_core::repository::PlaylistRepository::new(pool.clone())
        .create(&Playlist {
            id: synctv_core::models::PlaylistId::new(),
            room_id: room.id,
            creator_id: Some(owner.id),
            name: "Target".to_string(),
            description: String::new(),
            cover_file_reference_id: None,
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
        .checked("test operation should succeed");

    let media_repo = synctv_core::repository::MediaRepository::new(pool.clone());
    let _a = media_repo
        .create(&synctv_core::models::Media {
            id: synctv_core::models::MediaId::new(),
            playlist_id: Some(src.id),
            room_id: room.id,
            name: "A".to_string(),
            description: String::new(),
            position: 1024.0,
            source_provider: SourceProvider::DirectUrl,
            source_config: synctv_core_testing::direct_url_media_source_config(
                "https://example.com/a.mp4",
            ),
            provider_instance_name: None,
            creator_id: Some(owner.id),
            cover_file_reference_id: None,
            thumbnail_file_reference_id: None,
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .checked("test operation should succeed");
    let _b = media_repo
        .create(&synctv_core::models::Media {
            id: synctv_core::models::MediaId::new(),
            playlist_id: Some(src.id),
            room_id: room.id,
            name: "B".to_string(),
            description: String::new(),
            position: 2048.0,
            source_provider: SourceProvider::DirectUrl,
            source_config: synctv_core_testing::direct_url_media_source_config(
                "https://example.com/b.mp4",
            ),
            provider_instance_name: None,
            creator_id: Some(owner.id),
            cover_file_reference_id: None,
            thumbnail_file_reference_id: None,
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .checked("test operation should succeed");

    let moved = room_service
        .media_service()
        .move_media(
            room.id,
            owner.id,
            synctv_core::service::MoveMediaRequest {
                media_ids: Vec::new(),
                source_playlist_id: Some(src.id),
                target_playlist_id: Some(dst.id),
                all_from_scope: true,
                before_media_id: None,
                after_media_id: None,
            },
        )
        .await
        .checked("test operation should succeed");

    assert_eq!(moved.len(), 2);
    let dst_names: Vec<String> = media_repo
        .get_by_playlist(&dst.id)
        .await
        .checked("test operation should succeed")
        .into_iter()
        .map(|item| item.name)
        .collect();
    assert_eq!(dst_names, vec!["A".to_string(), "B".to_string()]);
    assert!(media_repo
        .get_by_playlist(&src.id)
        .await
        .checked("test operation should succeed")
        .is_empty());
}
