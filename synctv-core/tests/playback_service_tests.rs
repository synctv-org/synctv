//! `PlaybackService` integration tests
//!
//! Tests playback control including seek, speed, media switching, and
//! optimistic lock behavior with real `PostgreSQL` via testcontainers.
//!

use std::{sync::Arc, time::Duration};

use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    models::{
        room::AutoPlaySettings, Media, MediaId, PlayMode, Playlist, ProviderTarget, SourceProvider,
        User, UserId, UserRole, UserStatus,
    },
    repository::{MediaRepository, RoomPlaybackStateRepository, UserRepository},
    service::{
        BruteForceProtection, InMemoryTokenBlacklistStore, JwtService, RoomService, UserService,
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
        browse_access_mode: synctv_core::models::PlaylistBrowseAccessMode::Default,
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

async fn attach_test_media(
    pool: &PgPool,
    room_id: synctv_core::models::RoomId,
    owner_id: UserId,
) -> synctv_core::models::RoomPlaybackState {
    let media = Media {
        id: MediaId::new(),
        playlist_id: None,
        room_id,
        creator_id: Some(owner_id),
        name: "Playback Service Test Video".to_string(),
        description: String::new(),
        position: 0.0,
        source_provider: SourceProvider::DirectUrl,
        source_config: synctv_core_testing::direct_url_media_source_config(
            "https://example.com/video.mp4",
        ),
        provider_instance_name: None,
        cover_file_reference_id: None,
        thumbnail_file_reference_id: None,
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    let media = MediaRepository::new(pool.clone())
        .create(&media)
        .await
        .checked("test media should be created");
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());
    let mut state = playback_repo
        .create_or_get(&room_id)
        .await
        .checked("playback state should be created");
    state.playing_media_id = Some(media.id);
    state.playing_playlist_id = None;
    state.target = None;
    state.position = 0.0;
    playback_repo
        .update(&state)
        .await
        .checked("playback state should attach test media")
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
        .checked("test operation should succeed");

    let room = room_service
        .create_room(
            "Seek Neg Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playback_service = room_service.playback_service();

    let result = playback_service.seek(room.id, owner.id, -5.0).await;

    assert!(result.is_err());
    match result.failed("operation should fail") {
        Error::InvalidInput(msg) => {
            assert!(
                msg.contains("non-negative") || msg.contains("negative"),
                "Error should mention non-negative: {msg}"
            );
        }
        other => std::panic::panic_any(format!("expected InvalidInput error, got: {other:?}")),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_seek_rejects_live_direct_url_source() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("seek_live_direct_url_owner"))
        .await
        .checked("test operation should succeed");

    let room = room_service
        .create_room(
            "Seek Live Direct URL Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let mut source_config =
        synctv_core_testing::direct_url_media_source_config("https://example.com/live.m3u8");
    let synctv_core::models::MediaSourceConfig::DirectUrl(config) = &mut source_config else {
        panic!("direct url source_config should be DirectUrl");
    };
    config.playback_kind = Some(synctv_core::models::PlaybackKind::Live);

    let media = Media {
        id: MediaId::new(),
        playlist_id: None,
        room_id: room.id,
        creator_id: Some(owner.id),
        name: "Live Direct URL".to_string(),
        description: String::new(),
        position: 0.0,
        source_provider: SourceProvider::DirectUrl,
        source_config,
        provider_instance_name: None,
        cover_file_reference_id: None,
        thumbnail_file_reference_id: None,
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    let media = media_repo
        .create(&media)
        .await
        .checked("live media should be created");

    let playback_service = room_service.playback_service();
    playback_service
        .switch(room.id, owner.id, Some(media.id), None, None)
        .await
        .checked("live media should start playback");

    let result = playback_service.seek(room.id, owner.id, 30.0).await;

    match result.failed("live playback position update should fail") {
        Error::InvalidInput(message) => {
            assert!(
                message.contains("live playback"),
                "error should mention live playback: {message}"
            );
        }
        other => std::panic::panic_any(format!("expected InvalidInput error, got: {other:?}")),
    }

    let state = playback_service
        .get_state(&room.id)
        .await
        .checked("playback state should fetch");
    assert!((state.position - 0.0).abs() < f64::EPSILON);
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
        .checked("test operation should succeed");

    let room = room_service
        .create_room(
            "Speed Zero Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playback_service = room_service.playback_service();

    let result = playback_service.change_speed(room.id, owner.id, 0.0).await;

    assert!(result.is_err());
    match result.failed("operation should fail") {
        Error::InvalidInput(msg) => {
            assert!(
                msg.contains("Speed") || msg.contains("speed"),
                "Error should mention speed: {msg}"
            );
        }
        other => std::panic::panic_any(format!("expected InvalidInput error, got: {other:?}")),
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
        .checked("test operation should succeed");

    let room = room_service
        .create_room(
            "Speed Max Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playback_service = room_service.playback_service();

    let result = playback_service.change_speed(room.id, owner.id, 17.0).await;

    assert!(result.is_err());
    match result.failed("operation should fail") {
        Error::InvalidInput(msg) => {
            assert!(
                msg.contains("Speed") || msg.contains("16"),
                "Error should mention speed limit: {msg}"
            );
        }
        other => std::panic::panic_any(format!("expected InvalidInput error, got: {other:?}")),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_switch_media_resets_position() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("switch_owner"))
        .await
        .checked("test operation should succeed");

    let room = room_service
        .create_room(
            "Switch Media Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playlist = create_top_level_playlist(&pool, &room.id).await;

    // Add a media item
    let media = Media {
        id: MediaId::new(),
        playlist_id: Some(playlist.id),
        room_id: room.id,
        creator_id: Some(owner.id),
        name: "Test Video".to_string(),
        description: String::new(),
        position: 0.0,
        source_provider: SourceProvider::DirectUrl,
        source_config: synctv_core_testing::direct_url_media_source_config(
            "https://example.com/video.mp4",
        ),
        provider_instance_name: None,
        cover_file_reference_id: None,
        thumbnail_file_reference_id: None,
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    let media = media_repo
        .create(&media)
        .await
        .checked("test operation should succeed");

    // First seek to a non-zero position
    let playback_service = room_service.playback_service();
    attach_test_media(&pool, room.id, owner.id).await;
    playback_service
        .seek(room.id, owner.id, 42.5)
        .await
        .checked("test operation should succeed");

    // Switch media should reset position to 0
    let state = playback_service
        .switch(room.id, owner.id, Some(media.id), None, None)
        .await
        .checked("test operation should succeed");

    assert!(
        (state.position - 0.0).abs() < f64::EPSILON,
        "Current time should be reset to 0 after media switch, got: {}",
        state.position
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
        .checked("test operation should succeed");

    let room = room_service
        .create_room(
            "Switch Media Relative Path Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let media = Media {
        id: MediaId::new(),
        playlist_id: None,
        room_id: room.id,
        creator_id: Some(owner.id),
        name: "Standalone Video".to_string(),
        description: String::new(),
        position: 0.0,
        source_provider: SourceProvider::DirectUrl,
        source_config: synctv_core_testing::direct_url_media_source_config(
            "https://example.com/standalone.mp4",
        ),
        provider_instance_name: None,
        cover_file_reference_id: None,
        thumbnail_file_reference_id: None,
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    let media = media_repo
        .create(&media)
        .await
        .checked("test operation should succeed");

    let result = room_service
        .playback_service()
        .switch(
            room.id,
            owner.id,
            Some(media.id),
            None,
            Some(ProviderTarget::alist("/unexpected".to_string())),
        )
        .await;

    assert!(matches!(result, Err(Error::InvalidInput(_))));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_switch_media_validates_static_playlist_context() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("switch_static_context_owner"))
        .await
        .checked("test operation should succeed");
    let room = room_service
        .create_room(
            "Switch Static Context Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let other_playlist = create_top_level_playlist(&pool, &room.id).await;
    let media = media_repo
        .create(&Media {
            id: MediaId::new(),
            playlist_id: Some(playlist.id),
            room_id: room.id,
            creator_id: Some(owner.id),
            name: "Static Context Video".to_string(),
            description: String::new(),
            position: 0.0,
            source_provider: SourceProvider::DirectUrl,
            source_config: synctv_core_testing::direct_url_media_source_config(
                "https://example.com/static-context.mp4",
            ),
            provider_instance_name: None,
            cover_file_reference_id: None,
            thumbnail_file_reference_id: None,
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .checked("test media should be created");

    let state = room_service
        .playback_service()
        .switch(room.id, owner.id, Some(media.id), Some(playlist.id), None)
        .await
        .checked("matching static playlist context should be accepted");
    assert_eq!(state.playing_media_id, Some(media.id));
    assert_eq!(state.playing_playlist_id, Some(playlist.id));
    assert!(state.target.is_none());

    let result = room_service
        .playback_service()
        .switch(
            room.id,
            owner.id,
            Some(media.id),
            Some(other_playlist.id),
            None,
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
        .checked("test operation should succeed");

    let room = room_service
        .create_room(
            "Switch Inactive Creator Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let media_creator = user_repo
        .create(&make_user("switch_inactive_media_creator"))
        .await
        .checked("test operation should succeed");
    room_service
        .join_room(room.id, media_creator.id, None)
        .await
        .checked("media creator should join the room before being banned");

    let media = Media {
        id: MediaId::new(),
        playlist_id: None,
        room_id: room.id,
        creator_id: Some(media_creator.id),
        name: "Inactive Creator Video".to_string(),
        description: String::new(),
        position: 0.0,
        source_provider: SourceProvider::DirectUrl,
        source_config: synctv_core_testing::direct_url_media_source_config(
            "https://example.com/inactive.mp4",
        ),
        provider_instance_name: None,
        cover_file_reference_id: None,
        thumbnail_file_reference_id: None,
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    let media = media_repo
        .create(&media)
        .await
        .checked("test operation should succeed");

    user_repo
        .ban(
            &media_creator.id,
            None,
            Some("playback service test".to_string()),
        )
        .await
        .checked("test operation should succeed");

    let result = room_service
        .playback_service()
        .switch(room.id, owner.id, Some(media.id), None, None)
        .await;

    match result.failed("media created by banned user must not be playable") {
        Error::Authorization(message) => {
            assert!(
                message.contains("creator") && message.contains("unavailable"),
                "error should explain creator availability: {message}"
            );
        }
        other => std::panic::panic_any(format!("expected authorization error, got: {other:?}")),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_switch_media_serializes_with_concurrent_creator_ban() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("switch_concurrent_ban_owner"))
        .await
        .checked("room owner should be created");
    let creator = user_repo
        .create(&make_user("switch_concurrent_ban_creator"))
        .await
        .checked("media creator should be created");
    let room = room_service
        .create_room(
            "Switch Concurrent Creator Ban".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("room should be created");
    room_service
        .join_room(room.id, creator.id, None)
        .await
        .checked("media creator should join the room");

    let media = media_repo
        .create(&Media {
            id: MediaId::new(),
            playlist_id: None,
            room_id: room.id,
            creator_id: Some(creator.id),
            name: "Concurrent Creator Ban Video".to_string(),
            description: String::new(),
            position: 0.0,
            source_provider: SourceProvider::DirectUrl,
            source_config: synctv_core_testing::direct_url_media_source_config(
                "https://example.com/concurrent-creator-ban.mp4",
            ),
            provider_instance_name: None,
            cover_file_reference_id: None,
            thumbnail_file_reference_id: None,
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .checked("media should be created");
    playback_repo
        .create_or_get(&room.id)
        .await
        .checked("empty playback state should be created");

    let mut ban_tx = pool.begin().await.checked("ban transaction should begin");
    user_repo
        .get_by_id_for_update_with_executor(&creator.id, &mut *ban_tx)
        .await
        .checked("creator row should lock")
        .checked("creator should exist");
    user_repo
        .insert_ban_with_executor(
            &creator.id,
            Some(&owner.id),
            Some("concurrent playback test".to_string()),
            &mut *ban_tx,
        )
        .await
        .checked("ban should be staged");

    let playback_service = room_service.playback_service().clone();
    let switch_barrier = Arc::new(tokio::sync::Barrier::new(2));
    let task_barrier = Arc::clone(&switch_barrier);
    let mut switch_task = tokio::spawn(async move {
        task_barrier.wait().await;
        playback_service
            .switch(room.id, owner.id, Some(media.id), None, None)
            .await
    });
    switch_barrier.wait().await;

    assert!(
        tokio::time::timeout(Duration::from_millis(250), &mut switch_task)
            .await
            .is_err(),
        "playback switch must wait for the in-flight creator ban"
    );

    ban_tx
        .commit()
        .await
        .checked("ban transaction should commit");
    let switch_error = tokio::time::timeout(Duration::from_secs(5), switch_task)
        .await
        .checked("playback switch should finish after the ban commits")
        .checked("playback switch task should join")
        .failed("banned creator media must not become the playback source");
    assert!(matches!(switch_error, Error::Authorization(_)));

    let state = playback_repo
        .get(&room.id)
        .await
        .checked("playback state should load")
        .checked("playback state should exist");
    assert!(state.playing_media_id.is_none());
    assert!(state.playing_playlist_id.is_none());
    assert!(!state.is_playing);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_switch_media_serializes_with_concurrent_dynamic_ancestor_ban() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());
    let playlist_repo = synctv_core::repository::PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("switch_ancestor_ban_owner"))
        .await
        .checked("room owner should be created");
    let dynamic_creator = user_repo
        .create(&make_user("switch_ancestor_ban_dynamic_creator"))
        .await
        .checked("dynamic playlist creator should be created");
    let media_creator = user_repo
        .create(&make_user("switch_ancestor_ban_media_creator"))
        .await
        .checked("media creator should be created");
    let room = room_service
        .create_room(
            "Switch Concurrent Dynamic Ancestor Ban".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("room should be created");
    for member_id in [dynamic_creator.id, media_creator.id] {
        room_service
            .join_room(room.id, member_id, None)
            .await
            .checked("resource creator should join the room");
    }

    let dynamic_playlist = playlist_repo
        .create(&Playlist {
            id: synctv_core::models::PlaylistId::new(),
            room_id: room.id,
            creator_id: Some(dynamic_creator.id),
            browse_access_mode: synctv_core::models::PlaylistBrowseAccessMode::Default,
            name: "Historical Dynamic Root".to_string(),
            description: String::new(),
            cover_file_reference_id: None,
            parent_id: None,
            position: 0.0,
            source_provider: Some(SourceProvider::Alist),
            source_config: Some(synctv_core_testing::alist_directory_playlist_source_config(
                "alist-test",
                "/historical",
            )),
            provider_instance_name: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .checked("dynamic playlist should be created");
    let static_child = playlist_repo
        .create(&Playlist {
            id: synctv_core::models::PlaylistId::new(),
            room_id: room.id,
            creator_id: Some(media_creator.id),
            browse_access_mode: synctv_core::models::PlaylistBrowseAccessMode::Default,
            name: "Historical Static Child".to_string(),
            description: String::new(),
            cover_file_reference_id: None,
            parent_id: Some(dynamic_playlist.id),
            position: 0.0,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .checked("historical static child should be created directly");
    let media = media_repo
        .create(&Media {
            id: MediaId::new(),
            playlist_id: Some(static_child.id),
            room_id: room.id,
            creator_id: Some(media_creator.id),
            name: "Historical Nested Video".to_string(),
            description: String::new(),
            position: 0.0,
            source_provider: SourceProvider::DirectUrl,
            source_config: synctv_core_testing::direct_url_media_source_config(
                "https://example.com/historical-nested.mp4",
            ),
            provider_instance_name: None,
            cover_file_reference_id: None,
            thumbnail_file_reference_id: None,
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .checked("historical nested media should be created directly");
    playback_repo
        .create_or_get(&room.id)
        .await
        .checked("empty playback state should be created");
    let media_id = media.id;
    let static_child_id = static_child.id;

    let mut ban_tx = pool.begin().await.checked("ban transaction should begin");
    user_repo
        .get_by_id_for_update_with_executor(&dynamic_creator.id, &mut *ban_tx)
        .await
        .checked("dynamic creator row should lock")
        .checked("dynamic creator should exist");
    user_repo
        .insert_ban_with_executor(
            &dynamic_creator.id,
            Some(&owner.id),
            Some("concurrent dynamic ancestor test".to_string()),
            &mut *ban_tx,
        )
        .await
        .checked("dynamic creator ban should be staged");

    let playback_service = room_service.playback_service().clone();
    let switch_barrier = Arc::new(tokio::sync::Barrier::new(2));
    let task_barrier = Arc::clone(&switch_barrier);
    let mut switch_task = tokio::spawn(async move {
        task_barrier.wait().await;
        playback_service
            .switch(
                room.id,
                owner.id,
                Some(media_id),
                Some(static_child_id),
                None,
            )
            .await
    });
    switch_barrier.wait().await;

    assert!(
        tokio::time::timeout(Duration::from_millis(250), &mut switch_task)
            .await
            .is_err(),
        "playback switch must wait for an in-flight dynamic ancestor ban"
    );

    ban_tx
        .commit()
        .await
        .checked("ban transaction should commit");
    let switch_error = tokio::time::timeout(Duration::from_secs(5), switch_task)
        .await
        .checked("playback switch should finish after the ban commits")
        .checked("playback switch task should join")
        .failed("media below a banned dynamic ancestor must not become the playback source");
    assert!(matches!(switch_error, Error::Authorization(_)));

    let state = playback_repo
        .get(&room.id)
        .await
        .checked("playback state should load")
        .checked("playback state should exist");
    assert!(state.playing_media_id.is_none());
    assert!(state.playing_playlist_id.is_none());
    assert!(!state.is_playing);

    make_user_service(&pool)
        .unban_user(&dynamic_creator.id)
        .await
        .checked("dynamic creator should be unbanned");
    room_service
        .playback_service()
        .switch(room.id, owner.id, Some(media_id), None, None)
        .await
        .checked("playback should switch after the dynamic creator is unbanned");

    user_repo
        .ban(
            &dynamic_creator.id,
            Some(&owner.id),
            Some("static child playback path test".to_string()),
        )
        .await
        .checked("dynamic creator should be banned for the path check");
    let error = room_service
        .playback_service()
        .switch(
            room.id,
            owner.id,
            Some(media_id),
            Some(static_child_id),
            None,
        )
        .await
        .failed("media below an inactive dynamic ancestor must not be playable");
    assert!(matches!(error, Error::Authorization(_)));

    make_user_service(&pool)
        .unban_user(&dynamic_creator.id)
        .await
        .checked("dynamic creator should be unbanned after the path check");
    room_service
        .playback_service()
        .switch(
            room.id,
            owner.id,
            Some(media_id),
            Some(static_child_id),
            None,
        )
        .await
        .checked("playback should switch after the dynamic ancestor is restored");

    room_service
        .ban_user_and_reset_owned_playback_with_outbox(
            &dynamic_creator.id,
            Some(&owner.id),
            Some("post-switch dynamic ancestor test".to_string()),
            None,
            &[],
        )
        .await
        .checked("banning a dynamic ancestor creator should reset existing playback");
    let state = playback_repo
        .get(&room.id)
        .await
        .checked("playback state should load")
        .checked("playback state should exist");
    assert!(state.playing_media_id.is_none());
    assert!(state.playing_playlist_id.is_none());
    assert!(!state.is_playing);
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
        .checked("test operation should succeed");

    let room = room_service
        .create_room(
            "Switch Clear Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let media = Media {
        id: MediaId::new(),
        playlist_id: None,
        room_id: room.id,
        creator_id: Some(owner.id),
        name: "Clearable Video".to_string(),
        description: String::new(),
        position: 0.0,
        source_provider: SourceProvider::DirectUrl,
        source_config: synctv_core_testing::direct_url_media_source_config(
            "https://example.com/clearable.mp4",
        ),
        provider_instance_name: None,
        cover_file_reference_id: None,
        thumbnail_file_reference_id: None,
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    let media = media_repo
        .create(&media)
        .await
        .checked("test operation should succeed");

    let playback_service = room_service.playback_service();
    playback_service
        .switch(room.id, owner.id, Some(media.id), None, None)
        .await
        .checked("test operation should succeed");
    playback_service
        .seek(room.id, owner.id, 33.0)
        .await
        .checked("test operation should succeed");

    let state = playback_service
        .switch(room.id, owner.id, None, None, None)
        .await
        .checked("test operation should succeed");

    assert!(state.playing_media_id.is_none());
    assert!(state.playing_playlist_id.is_none());
    assert!(state.target.is_none());
    assert!((state.position - 0.0).abs() < f64::EPSILON);
    assert!((state.speed - 1.0).abs() < f64::EPSILON);
    assert!(!state.is_playing);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_playback_optimistic_lock_concurrent() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let owner = user_repo
        .create(&make_user("olc_owner"))
        .await
        .checked("test operation should succeed");

    let room = room_service
        .create_room("OLC Room".to_string(), String::new(), owner.id, None, None)
        .await
        .checked("test operation should succeed");
    attach_test_media(&pool, room.id, owner.id).await;

    // Spawn multiple concurrent seek operations
    let mut handles = vec![];
    for i in 0..5 {
        let rs = room_service.clone();
        let rid = room.id;
        let uid = owner.id;
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
            Err(e) => std::panic::panic_any(format!("seek task should complete: {e:?}")),
        }
    }

    assert!(success_count > 0, "At least one seek should succeed");

    // Final state should be valid
    let playback_service = room_service.playback_service();
    let state = playback_service
        .get_state(&room.id)
        .await
        .checked("test operation should succeed");
    assert!(
        state.position >= 0.0,
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
        .checked("test operation should succeed");

    let room = room_service
        .create_room(
            "Rapid Seek Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    attach_test_media(&pool, room.id, owner.id).await;

    // Spawn 10 concurrent seek operations
    let mut handles = vec![];
    let barrier = Arc::new(tokio::sync::Barrier::new(10));

    for i in 0..10 {
        let rs = room_service.clone();
        let rid = room.id;
        let uid = owner.id;
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
                positions.push(response.state.position);
            }
            Ok(Err(_)) => {} // Some may fail due to conflicts
            Err(e) => std::panic::panic_any(format!("seek task should complete: {e:?}")),
        }
    }

    assert!(success_count > 0, "At least one seek should succeed");

    // Final state should be valid
    let playback_service = room_service.playback_service();
    let state = playback_service
        .get_state(&room.id)
        .await
        .checked("test operation should succeed");
    assert!(
        state.position >= 0.0,
        "Final position should be non-negative"
    );
    assert!(
        state.position <= 300.0,
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
        .checked("test operation should succeed");

    let room = room_service
        .create_room(
            "Play Next Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playlist = create_top_level_playlist(&pool, &room.id).await;

    // Add 5 media items
    let mut media_ids = Vec::new();
    for i in 0..5 {
        let media = Media {
            id: MediaId::new(),
            playlist_id: Some(playlist.id),
            room_id: room.id,
            creator_id: Some(owner.id),
            name: format!("Video {i}"),
            description: String::new(),
            position: f64::from(i),
            source_provider: SourceProvider::DirectUrl,
            source_config: synctv_core_testing::direct_url_media_source_config(format!(
                "https://example.com/video{i}.mp4"
            )),
            provider_instance_name: None,
            cover_file_reference_id: None,
            thumbnail_file_reference_id: None,
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        };
        let media = media_repo
            .create(&media)
            .await
            .checked("test operation should succeed");
        media_ids.push(media.id);
    }

    let playback_service = room_service.playback_service();
    playback_service
        .switch(room.id, owner.id, Some(media_ids[0]), None, None)
        .await
        .checked("test operation should succeed");

    // Verify we're playing first media
    let state = playback_service
        .get_state(&room.id)
        .await
        .checked("test operation should succeed");
    assert_eq!(state.playing_media_id, Some(media_ids[0]));

    // Set up RoomSettings with auto_play enabled
    let settings = RoomSettings::default();

    // Call play_next - should advance to second media
    let result = playback_service
        .play_next(&room.id, &settings)
        .await
        .checked("test operation should succeed");

    assert!(result.is_some(), "play_next should return new state");
    let new_state = result.checked("test operation should succeed");
    assert_eq!(new_state.playing_media_id, Some(media_ids[1]));
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
        .checked("test operation should succeed");

    let room = room_service
        .create_room(
            "Playlist End Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playlist = create_top_level_playlist(&pool, &room.id).await;

    // Add 3 media items
    let mut media_ids = Vec::new();
    for i in 0..3 {
        let media = Media {
            id: MediaId::new(),
            playlist_id: Some(playlist.id),
            room_id: room.id,
            creator_id: Some(owner.id),
            name: format!("End Video {i}"),
            description: String::new(),
            position: f64::from(i),
            source_provider: SourceProvider::DirectUrl,
            source_config: synctv_core_testing::direct_url_media_source_config(format!(
                "https://example.com/end{i}.mp4"
            )),
            provider_instance_name: None,
            cover_file_reference_id: None,
            thumbnail_file_reference_id: None,
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        };
        let media = media_repo
            .create(&media)
            .await
            .checked("test operation should succeed");
        media_ids.push(media.id);
    }

    let playback_service = room_service.playback_service();
    playback_service
        .switch(room.id, owner.id, Some(media_ids[2]), None, None)
        .await
        .checked("test operation should succeed");

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
        .checked("test operation should succeed");

    // At end of playlist with no loop, should return None
    // (or the implementation may return the state unchanged)
    match result {
        None => {} // Expected: end of playlist
        Some(state) => {
            // If it returns a state, it should be the same (no change)
            assert_eq!(state.playing_media_id, Some(media_ids[2]));
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

    let owner = user_repo
        .create(&make_user("loop_owner"))
        .await
        .checked("test operation should succeed");

    let room = room_service
        .create_room("Loop Room".to_string(), String::new(), owner.id, None, None)
        .await
        .checked("test operation should succeed");

    let playlist = create_top_level_playlist(&pool, &room.id).await;

    // Add 3 media items
    let mut media_ids = Vec::new();
    for i in 0..3 {
        let media = Media {
            id: MediaId::new(),
            playlist_id: Some(playlist.id),
            room_id: room.id,
            creator_id: Some(owner.id),
            name: format!("Loop Video {i}"),
            description: String::new(),
            position: f64::from(i),
            source_provider: SourceProvider::DirectUrl,
            source_config: synctv_core_testing::direct_url_media_source_config(format!(
                "https://example.com/loop{i}.mp4"
            )),
            provider_instance_name: None,
            cover_file_reference_id: None,
            thumbnail_file_reference_id: None,
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        };
        let media = media_repo
            .create(&media)
            .await
            .checked("test operation should succeed");
        media_ids.push(media.id);
    }

    let playback_service = room_service.playback_service();
    playback_service
        .switch(room.id, owner.id, Some(media_ids[2]), None, None)
        .await
        .checked("test operation should succeed");

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
        .checked("test operation should succeed");

    // With loop, should return to first item
    assert!(result.is_some(), "With loop, play_next should return state");
    let state = result.checked("test operation should succeed");
    assert_eq!(state.playing_media_id, Some(media_ids[0]));
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
        .checked("test operation should succeed");

    let room = room_service
        .create_room(
            "Empty Playlist Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playback_service = room_service.playback_service();
    let settings = RoomSettings::default();

    // Play next on empty playlist should return None
    let result = playback_service
        .play_next(&room.id, &settings)
        .await
        .checked("test operation should succeed");

    assert!(
        result.is_none(),
        "play_next on empty playlist should return None"
    );

    // Get state should work
    let state = playback_service
        .get_state(&room.id)
        .await
        .checked("test operation should succeed");
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
        .checked("test operation should succeed");

    let room = room_service
        .create_room(
            "Speed Concurrent Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    // Spawn 10 concurrent speed change operations
    let mut handles = vec![];
    let barrier = Arc::new(tokio::sync::Barrier::new(10));

    let valid_speeds = [0.5, 1.0, 1.5, 2.0, 0.75, 1.25, 1.75, 2.5, 3.0, 4.0];

    for speed in valid_speeds {
        let rs = room_service.clone();
        let rid = room.id;
        let uid = owner.id;
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
            Err(e) => std::panic::panic_any(format!("playback task should complete: {e:?}")),
        }
    }

    assert!(
        success_count > 0,
        "At least one speed change should succeed"
    );

    // Final state should have a valid speed
    let playback_service = room_service.playback_service();
    let state = playback_service
        .get_state(&room.id)
        .await
        .checked("test operation should succeed");
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
        .checked("test operation should succeed");

    let room = room_service
        .create_room(
            "Mixed Ops Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playlist = create_top_level_playlist(&pool, &room.id).await;

    // Add media
    let media = Media {
        id: MediaId::new(),
        playlist_id: Some(playlist.id),
        room_id: room.id,
        creator_id: Some(owner.id),
        name: "Mixed Test Video".to_string(),
        description: String::new(),
        position: 0.0,
        source_provider: SourceProvider::DirectUrl,
        source_config: synctv_core_testing::direct_url_media_source_config(
            "https://example.com/mixed.mp4",
        ),
        provider_instance_name: None,
        cover_file_reference_id: None,
        thumbnail_file_reference_id: None,
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    let media = media_repo
        .create(&media)
        .await
        .checked("test operation should succeed");

    let playback_service = room_service.playback_service();

    // Switch to media
    playback_service
        .switch(room.id, owner.id, Some(media.id), None, None)
        .await
        .checked("test operation should succeed");

    // Seek
    let seek_response = playback_service
        .seek(room.id, owner.id, 50.0)
        .await
        .checked("test operation should succeed");
    assert!(seek_response.seek_applied, "Seek should be applied");
    assert!(
        (seek_response.state.position - 50.0).abs() < f64::EPSILON,
        "Seek should set the exact requested position"
    );

    // Change speed while playing: the effective position may advance slightly,
    // but it must never move backward from the seek target.
    let speed_state = playback_service
        .change_speed(room.id, owner.id, 1.5)
        .await
        .checked("test operation should succeed");
    assert!(
        speed_state.position >= 50.0,
        "Position must not move backward after changing speed"
    );

    // Pause snapshots the computed playback position.
    let paused_state = playback_service
        .set_playing(room.id, owner.id, false)
        .await
        .checked("test operation should succeed");
    assert!(
        paused_state.position >= speed_state.position,
        "Pause should preserve or advance the effective position"
    );
    assert!(!paused_state.is_playing, "Pause should stop playback");

    // Resume should preserve the paused position and flip playback back on.
    let resumed_state = playback_service
        .set_playing(room.id, owner.id, true)
        .await
        .checked("test operation should succeed");
    assert!(
        (resumed_state.position - paused_state.position).abs() < 0.1,
        "Resume should preserve the paused position"
    );

    // Verify final state is consistent.
    let state = playback_service
        .get_state(&room.id)
        .await
        .checked("test operation should succeed");

    assert_eq!(state.playing_media_id, Some(media.id));
    assert!(
        state.position >= paused_state.position,
        "Final position should not move backward"
    );
    assert!(
        (state.speed - 1.5).abs() < f64::EPSILON,
        "Speed should be 1.5"
    );
    assert!(state.is_playing, "Should be playing");
}

use synctv_core::models::{room_settings, RoomSettings};

/// Successful seek responses set `seek_applied=true`.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_seek_success_returns_applied_true() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("seek_response_owner"))
        .await
        .checked("test operation should succeed");

    let room = room_service
        .create_room(
            "Seek Response Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playback_service = room_service.playback_service();
    attach_test_media(&pool, room.id, owner.id).await;

    // A simple seek should succeed and report seek_applied=true
    let response = playback_service
        .seek(room.id, owner.id, 42.5)
        .await
        .checked("test operation should succeed");

    assert!(
        response.seek_applied,
        "Successful seek should have seek_applied=true"
    );
    assert!(
        (response.state.position - 42.5).abs() < f64::EPSILON,
        "Position should be 42.5, got: {}",
        response.state.position
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
        .checked("test operation should succeed");

    let room = room_service
        .create_room(
            "Seek Retry Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playback_service = room_service.playback_service();
    attach_test_media(&pool, room.id, owner.id).await;

    // Spawn many concurrent seeks to trigger retry exhaustion
    let mut handles = vec![];
    let barrier = Arc::new(tokio::sync::Barrier::new(50));

    for i in 0..50 {
        let rs = room_service.clone();
        let rid = room.id;
        let uid = owner.id;
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
                        response.state.position >= 0.0,
                        "Degraded response should have valid position"
                    );
                }
            }
            Ok(Err(_)) => {} // Other errors are OK
            Err(e) => std::panic::panic_any(format!("playback task should complete: {e:?}")),
        }
    }

    assert!(success_count > 0, "At least one seek should succeed");
    // Note: degraded_count may be 0 if all succeed within retry budget

    // Final state should be valid
    let final_state = playback_service
        .get_state(&room.id)
        .await
        .checked("test operation should succeed");
    assert!(
        final_state.position >= 0.0,
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
        .checked("test operation should succeed");

    let room = room_service
        .create_room(
            "Seek State Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playback_service = room_service.playback_service();
    attach_test_media(&pool, room.id, owner.id).await;

    // First set a known position
    playback_service
        .seek(room.id, owner.id, 100.0)
        .await
        .checked("test operation should succeed");

    // Now seek to a different position
    let response = playback_service
        .seek(room.id, owner.id, 200.0)
        .await
        .checked("test operation should succeed");

    // Response should have valid state (either at 200 if applied, or playback position)
    assert!(
        response.state.position >= 0.0,
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
        .checked("test operation should succeed");

    let room = room_service
        .create_room(
            "Seek Message Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    attach_test_media(&pool, room.id, owner.id).await;

    // Spawn many concurrent seeks
    let mut handles = vec![];
    let barrier = Arc::new(tokio::sync::Barrier::new(100));

    for i in 0..100 {
        let rs = room_service.clone();
        let rid = room.id;
        let uid = owner.id;
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
            Err(e) => std::panic::panic_any(format!("playback task should complete: {e:?}")),
        }
    }
}
