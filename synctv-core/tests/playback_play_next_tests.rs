//! `PlaybackService::play_next` logic tests
//!
//! Tests the `play_next` method's playlist navigation logic for each `PlayMode`,
//! including edge cases like deleted media and empty playlists.
//!
//! These tests exercise the `play_next` decision logic with a real `PostgreSQL`
//! via testcontainers, since `play_next` reads from the DB repo layer.
//!
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use sqlx::PgPool;
use synctv_core::models::media::{AlistPlaybackLocator, AlistPlaybackMediaLocator};
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    credential_encryption::CredentialEncryption,
    models::{
        room::AutoPlaySettings, room_settings::AutoPlay, Media, MediaId, PlayMode,
        PlaybackAlistMedia, PlaybackMedia, PlaybackMediaProvider, Playlist, PlaylistId,
        ProviderInstance, ProviderTarget, RoomAdminPermissionBits, RoomId, RoomRole, RoomSettings,
        SourceProvider, User, UserId, UserRole, UserStatus,
    },
    provider::{
        DynamicListQuery, DynamicListResult, DynamicPagination, DynamicPlaylistItem,
        DynamicPlaylistProvider, ItemType, MediaProvider, NextPlayItem, PlaybackInfo,
        PlaybackResult, ProviderContext, ProviderError,
    },
    repository::{
        MediaRepository, PlaybackHistoryRepository, ProviderInstanceRepository,
        UserProviderCredentialRepository, UserRepository,
    },
    service::{
        BruteForceProtection, InMemoryTokenBlacklistStore, JwtService, RoomService,
        RoomServiceOptions, UserService,
    },
    service::{ProvidersManager, RemoteProviderManager},
};
use synctv_core_testing::{
    create_test_pool, ensure_playback_history_partition_for, TestOptionExt, TestResultExt,
};
fn make_user_service(pool: &PgPool) -> UserService {
    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = JwtService::new(secret).checked("Failed to create JwtService");
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

fn make_room_service(pool: &PgPool) -> RoomService {
    let user_service = make_user_service(pool);

    RoomService::new_for_tests(pool.clone(), user_service).checked("room service should build")
}

fn make_room_service_without_builtin_providers(pool: &PgPool) -> RoomService {
    let user_service = make_user_service(pool);
    let instance_repo = Arc::new(ProviderInstanceRepository::new(pool.clone()));
    let instance_manager = Arc::new(RemoteProviderManager::new(instance_repo));
    let providers_manager =
        Arc::new(ProvidersManager::new(instance_manager).checked("providers manager should build"));

    RoomService::new_with_providers_for_tests(pool.clone(), user_service, providers_manager)
        .checked("room service should build")
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_switch_rejects_missing_provider_before_state_change() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service_without_builtin_providers(&pool);
    let owner = user_repo
        .create(&make_user("missing_provider_owner"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Missing Provider".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    let media = insert_root_media(&pool, &room.id, "missing_provider_media", 0).await;
    let playback = room_service.playback_service();

    let error = playback
        .switch(room.id, owner.id, Some(media.id), None, None)
        .await
        .expect_err("missing provider should reject source selection");
    assert!(
        matches!(error, synctv_core::Error::NotFound(message) if message.starts_with("Provider not found:"))
    );

    let state = playback
        .get_state(&room.id)
        .await
        .checked("playback state should remain readable");
    assert!(state.playing_media_id.is_none());
    assert!(state.playing_playlist_id.is_none());
    assert!(!state.is_playing);
}

fn make_room_service_with_providers(
    pool: &PgPool,
    providers_manager: Arc<ProvidersManager>,
) -> RoomService {
    let user_service = make_user_service(pool);
    let credential_encryption =
        CredentialEncryption::new(&[0x42; 32]).checked("test encryption key should be valid");
    let credential_repo = Arc::new(UserProviderCredentialRepository::new_with_encryption(
        pool.clone(),
        credential_encryption.clone(),
    ));

    RoomService::new_with_providers_and_options(
        pool.clone(),
        user_service,
        providers_manager,
        RoomServiceOptions {
            credential_encryption: Some(credential_encryption),
            credential_repo: Some(credential_repo),
            ..RoomServiceOptions::test_defaults_with_settings(pool.clone())
        },
    )
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

fn make_settings_with_mode(mode: PlayMode) -> RoomSettings {
    RoomSettings {
        auto_play: AutoPlay::new(AutoPlaySettings {
            enabled: true,
            mode,
            delay: 0,
        }),
        ..Default::default()
    }
}

fn alist_target(cursor: &str) -> ProviderTarget {
    ProviderTarget::alist(cursor.to_string())
}

fn decode_alist_target(target: &ProviderTarget) -> String {
    match target {
        ProviderTarget::Alist(target) => target.relative_path.clone(),
        _ => panic!("expected alist target"),
    }
}

fn assert_alist_target(target: Option<&ProviderTarget>, expected: &str) {
    let Some(ProviderTarget::Alist(target)) = target else {
        panic!("expected alist target");
    };
    assert_eq!(target.relative_path, expected);
}

/// Helper: create a top-level playlist for a room.
async fn create_top_level_playlist(pool: &PgPool, room_id: &RoomId) -> Playlist {
    let playlist = Playlist {
        id: PlaylistId::new(),
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
        .checked("Top-level playlist should be created")
}

/// Helper: insert a media item into the playlist at a given position
async fn insert_media(
    pool: &PgPool,
    playlist_id: &PlaylistId,
    room_id: &RoomId,
    name: &str,
    position: i32,
) -> Media {
    let media = Media {
        id: MediaId::new(),
        playlist_id: Some(*playlist_id),
        room_id: *room_id,
        creator_id: None,
        name: name.to_string(),
        description: String::new(),
        position: f64::from(position),
        source_provider: SourceProvider::DirectUrl,
        source_config: synctv_core_testing::direct_url_media_source_config(format!(
            "https://example.com/{name}.mp4"
        )),
        provider_instance_name: None,
        cover_file_reference_id: None,
        thumbnail_file_reference_id: None,
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    let media_repo = MediaRepository::new(pool.clone());
    media_repo
        .create(&media)
        .await
        .checked("Failed to create media")
}

async fn insert_root_media(pool: &PgPool, room_id: &RoomId, name: &str, position: i32) -> Media {
    let media = Media {
        id: MediaId::new(),
        playlist_id: None,
        room_id: *room_id,
        creator_id: None,
        name: name.to_string(),
        description: String::new(),
        position: f64::from(position),
        source_provider: SourceProvider::DirectUrl,
        source_config: synctv_core_testing::direct_url_media_source_config(format!(
            "https://example.com/{name}.mp4"
        )),
        provider_instance_name: None,
        cover_file_reference_id: None,
        thumbnail_file_reference_id: None,
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    MediaRepository::new(pool.clone())
        .create(&media)
        .await
        .checked("Failed to create root media")
}

#[derive(Debug)]
struct TestDynamicProvider {
    instance_id: String,
    provider_type: &'static str,
    require_credential_encryption: bool,
}

impl TestDynamicProvider {
    fn new(instance_id: impl Into<String>) -> Self {
        Self::with_provider_type("alist", instance_id, false)
    }

    fn with_provider_type(
        provider_type: &'static str,
        instance_id: impl Into<String>,
        require_credential_encryption: bool,
    ) -> Self {
        Self {
            instance_id: instance_id.into(),
            provider_type,
            require_credential_encryption,
        }
    }

    fn requiring_credential_encryption(instance_id: impl Into<String>) -> Self {
        Self::with_provider_type("alist", instance_id, true)
    }

    fn is_bound_instance(&self) -> bool {
        self.instance_id != format!("{}_default", self.provider_type)
    }

    fn first_episode_path(&self) -> &'static str {
        if self.is_bound_instance() {
            "/bound-episode-1.mp4"
        } else {
            "/episode-1.mp4"
        }
    }

    fn second_episode_path(&self) -> &'static str {
        if self.is_bound_instance() {
            "/bound-episode-2.mp4"
        } else {
            "/episode-2.mp4"
        }
    }

    fn playback_result_for(&self, path: &str) -> PlaybackResult {
        let mut infos = std::collections::HashMap::new();
        infos.insert(
            "direct".to_string(),
            PlaybackInfo {
                thumbnail: None,
                medias: vec![PlaybackMedia {
                    name: String::new(),
                    format: "mp4".to_string(),
                    expire_at: None,
                    metadata: None,
                    p2p_swarm_id: None,
                    provider: PlaybackMediaProvider::Alist(PlaybackAlistMedia::Direct {
                        url: format!("https://{}.example.com{path}", self.instance_id),
                        headers: std::collections::HashMap::new(),
                        locator: AlistPlaybackLocator {
                            server_id: self.instance_id.clone(),
                            path: path.to_string(),
                            password: None,
                            credential_owner_id: UserId::new(),
                            credential_revision: "test".to_string(),
                            provider_instance_name: Some(self.instance_id.clone()),
                        },
                        resource: AlistPlaybackMediaLocator::File,
                    }),
                }],
                default_media_index: None,
                subtitles: Vec::new(),
                default_subtitle_index: None,
                danmakus: Vec::new(),
                default_danmaku_index: None,
            },
        );
        PlaybackResult {
            playback_infos: infos,
            default_mode: "direct".to_string(),
            provider: self.provider_type.to_string(),
            provider_instance_name: Some(self.instance_id.clone()),
            duration_seconds: None,
            playback_kind: Some(synctv_core::models::PlaybackKind::Regular),
            metadata: None,
        }
    }

    fn item(path: &str) -> NextPlayItem {
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        NextPlayItem {
            name,
            item_type: ItemType::Media,
            source_config: synctv_core_testing::alist_file_media_source_config("alist", path),
            target: alist_target(path),
        }
    }
}

#[async_trait]
impl MediaProvider for TestDynamicProvider {
    fn name(&self) -> &'static str {
        self.provider_type
    }

    async fn generate_playback(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &synctv_core::models::MediaSourceConfig,
    ) -> Result<PlaybackResult, ProviderError> {
        let synctv_core::models::MediaSourceConfig::Alist(source_config) = source_config else {
            return Err(ProviderError::InvalidConfig(
                "Missing Alist source_config".to_string(),
            ));
        };
        let path = source_config.path.as_str();
        Ok(self.playback_result_for(path))
    }

    fn as_dynamic_playlist_provider(&self) -> Option<&dyn DynamicPlaylistProvider> {
        Some(self)
    }
}

#[async_trait]
impl DynamicPlaylistProvider for TestDynamicProvider {
    async fn list_playlist(
        &self,
        ctx: &ProviderContext<'_>,
        _playlist: &Playlist,
        target: Option<&ProviderTarget>,
        _query: DynamicListQuery,
    ) -> Result<DynamicListResult, ProviderError> {
        if self.require_credential_encryption && ctx.credential_encryption.is_none() {
            return Err(ProviderError::EncryptionRequired(self.provider_type));
        }
        let items = match target
            .map(decode_alist_target)
            .as_deref()
            .unwrap_or_default()
        {
            "" => vec![
                DynamicPlaylistItem {
                    name: self
                        .first_episode_path()
                        .trim_start_matches('/')
                        .to_string(),
                    item_type: ItemType::Media,
                    target: alist_target(self.first_episode_path()),
                    size: None,
                    thumbnail: None,
                    description: None,
                    modified_at: None,
                    source_config: None,
                    metadata: None,
                },
                DynamicPlaylistItem {
                    name: self
                        .second_episode_path()
                        .trim_start_matches('/')
                        .to_string(),
                    item_type: ItemType::Media,
                    target: alist_target(self.second_episode_path()),
                    size: None,
                    thumbnail: None,
                    description: None,
                    modified_at: None,
                    source_config: None,
                    metadata: None,
                },
            ],
            _ => Vec::new(),
        };
        Ok(DynamicListResult {
            has_more: false,
            items,
            pagination: DynamicPagination::Page { page: 1 },
        })
    }

    async fn resolve_item(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &Playlist,
        target: &ProviderTarget,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        let cursor = decode_alist_target(target);
        Ok(match cursor.as_str() {
            path if path == self.first_episode_path() || path == self.second_episode_path() => {
                Some(Self::item(&cursor))
            }
            _ => None,
        })
    }

    async fn next(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &Playlist,
        target: &ProviderTarget,
        play_mode: PlayMode,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        let cursor = decode_alist_target(target);
        Ok(match (cursor.as_str(), play_mode) {
            (path, PlayMode::Sequential | PlayMode::RepeatAll | PlayMode::Shuffle)
                if path == self.first_episode_path() =>
            {
                Some(Self::item(self.second_episode_path()))
            }
            (path, PlayMode::RepeatAll) if path == self.second_episode_path() => {
                Some(Self::item(self.first_episode_path()))
            }
            (path, PlayMode::RepeatOne)
                if path == self.first_episode_path() || path == self.second_episode_path() =>
            {
                None
            }
            _ => None,
        })
    }
}

async fn register_alist_provider(room_service: &RoomService) {
    register_alist_provider_instance(room_service, "alist_default").await;
}

async fn register_alist_provider_instance(room_service: &RoomService, instance_id: &str) {
    if let Err(error) = room_service
        .media_service()
        .providers_manager()
        .create_provider_with_default_config("alist", instance_id)
        .await
    {
        std::panic::panic_any(format!(
            "failed to register fake dynamic provider: {error:?}"
        ));
    }
}

async fn register_alist_provider_instance_requiring_encryption(
    room_service: &RoomService,
    instance_id: &str,
) {
    if let Err(error) = room_service
        .media_service()
        .providers_manager()
        .create_provider_with_default_config("alist", instance_id)
        .await
    {
        std::panic::panic_any(format!(
            "failed to register fake dynamic provider requiring encryption: {error:?}"
        ));
    }
}

async fn create_dynamic_playlist(
    pool: &PgPool,
    room_id: &RoomId,
    owner_id: &UserId,
    provider_instance_name: &str,
) -> Playlist {
    insert_test_provider_instance(pool, provider_instance_name, "alist").await;

    let playlist = Playlist {
        id: PlaylistId::new(),
        room_id: *room_id,
        creator_id: Some(*owner_id),
        name: "Dynamic Playlist".to_string(),
        description: String::new(),
        cover_file_reference_id: None,
        parent_id: None,
        position: 0.0,
        source_provider: Some(SourceProvider::Alist),
        source_config: Some(synctv_core_testing::alist_directory_playlist_source_config(
            provider_instance_name,
            "/",
        )),
        provider_instance_name: Some(provider_instance_name.to_string()),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    synctv_core::repository::PlaylistRepository::new(pool.clone())
        .create(&playlist)
        .await
        .checked("Dynamic playlist should be created")
}

async fn create_dynamic_sensitive_playlist(
    pool: &PgPool,
    room_id: &RoomId,
    owner_id: &UserId,
    provider_instance_name: &str,
) -> Playlist {
    insert_test_provider_instance(pool, provider_instance_name, "alist").await;

    let playlist = Playlist {
        id: PlaylistId::new(),
        room_id: *room_id,
        creator_id: Some(*owner_id),
        name: "Dynamic Sensitive Playlist".to_string(),
        description: String::new(),
        cover_file_reference_id: None,
        parent_id: None,
        position: 0.0,
        source_provider: Some(SourceProvider::Alist),
        source_config: Some(synctv_core_testing::alist_directory_playlist_source_config(
            provider_instance_name,
            "/sensitive",
        )),
        provider_instance_name: Some(provider_instance_name.to_string()),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    synctv_core::repository::PlaylistRepository::new(pool.clone())
        .create(&playlist)
        .await
        .checked("Dynamic sensitive playlist should be created")
}

async fn insert_test_provider_instance(pool: &PgPool, name: &str, provider: &str) {
    let now = Utc::now();
    let instance = ProviderInstance {
        name: name.to_string(),
        endpoint: "http://localhost:50051".to_string(),
        comment: Some("test provider instance".to_string()),
        jwt_secret: None,
        custom_ca: None,
        timeout: "10s".to_string(),
        tls: false,
        insecure_tls: false,
        providers: vec![provider
            .parse::<SourceProvider>()
            .checked("test provider should be known")],
        enabled: true,
        created_at: now,
        updated_at: now,
    };
    ProviderInstanceRepository::new(pool.clone())
        .create(&instance)
        .await
        .checked("test operation should succeed");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_sequential_advance_to_next() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(&pool);

    let owner = user_repo
        .create(&make_user("seq_next_owner"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room("Seq Next".to_string(), String::new(), owner.id, None, None)
        .await
        .checked("test operation should succeed");

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "video1", 0).await;
    let media2 = insert_media(&pool, &playlist.id, &room.id, "video2", 1).await;

    // Set currently playing to media1
    let playback = room_service.playback_service();
    playback
        .switch(room.id, owner.id, Some(media1.id), None, None)
        .await
        .checked("test operation should succeed");

    let settings = make_settings_with_mode(PlayMode::Sequential);
    let result = playback
        .play_next(&room.id, &settings)
        .await
        .checked("test operation should succeed");

    assert!(result.is_some(), "Should advance to next media");
    let state = result.checked("test operation should succeed");
    assert_eq!(
        state.playing_media_id,
        Some(media2.id),
        "Should be playing media2"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_sequential_advance_preserves_static_playlist_context() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(&pool);
    let owner = user_repo
        .create(&make_user("seq_context_owner"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Sequential Context".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "context_1", 0).await;
    let media2 = insert_media(&pool, &playlist.id, &room.id, "context_2", 1).await;
    let playback = room_service.playback_service();

    let selected = playback
        .switch(room.id, owner.id, Some(media1.id), Some(playlist.id), None)
        .await
        .checked("static playlist selection should succeed");
    assert_eq!(selected.playing_playlist_id, Some(playlist.id));

    let advanced = playback
        .play_next(&room.id, &make_settings_with_mode(PlayMode::Sequential))
        .await
        .checked("play next should succeed")
        .checked("second playlist item should exist");
    assert_eq!(advanced.playing_media_id, Some(media2.id));
    assert_eq!(advanced.playing_playlist_id, Some(playlist.id));
    assert!(advanced.target.is_none());

    let history = playback
        .list_playback_history(&room.id, None, 10)
        .await
        .checked("history should be listed");
    assert_eq!(history.entries.len(), 2);
    assert!(history
        .entries
        .iter()
        .all(|entry| entry.playlist_id == Some(playlist.id)));
    let latest_entry = history
        .entries
        .first()
        .checked("latest history entry should exist");
    assert_eq!(latest_entry.media_id, Some(media2.id));
    assert_eq!(latest_entry.media_name.as_deref(), Some("context_2"));
    assert_eq!(
        latest_entry.source_provider,
        Some(SourceProvider::DirectUrl)
    );
    assert_eq!(latest_entry.provider_instance_name, None);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_auto_advance_after_previous_uses_recorded_forward_history() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(&pool);
    let mut committed_events = room_service
        .notification_service()
        .subscribe_committed_realtime_events();
    let owner = user_repo
        .create(&make_user("history_forward_owner"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "History Forward".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media_a = insert_media(&pool, &playlist.id, &room.id, "history_a", 0).await;
    let media_b = insert_media(&pool, &playlist.id, &room.id, "history_b", 2).await;
    let playback = room_service.playback_service();
    playback
        .switch(room.id, owner.id, Some(media_a.id), None, None)
        .await
        .checked("test operation should succeed");
    playback
        .seek(room.id, owner.id, 42.0)
        .await
        .checked("test operation should succeed");
    let settings = make_settings_with_mode(PlayMode::Sequential);
    playback
        .play_next(&room.id, &settings)
        .await
        .checked("test operation should succeed")
        .checked("A should advance to B");
    let previous = playback
        .play_previous_for_user(&room.id, owner.id, None)
        .await
        .checked("test operation should succeed")
        .checked("B should return to A");
    assert_eq!(previous.position, 0.0);
    let history = playback
        .list_playback_history(&room.id, None, 10)
        .await
        .checked("history should be listed");
    assert_eq!(history.entries.len(), 2);
    let history_cursor_id = history
        .history_cursor_id
        .checked("history cursor should exist");
    assert_eq!(
        history
            .entries
            .iter()
            .find(|entry| entry.id == history_cursor_id)
            .and_then(|entry| entry.media_id),
        Some(media_a.id)
    );

    let media_c = insert_media(&pool, &playlist.id, &room.id, "history_c", 1).await;
    let state = playback
        .play_next(&room.id, &settings)
        .await
        .checked("test operation should succeed")
        .checked("history should contain a forward entry");

    assert_eq!(state.playing_media_id, Some(media_b.id));
    assert_ne!(state.playing_media_id, Some(media_c.id));
    let system_message_count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM chat_messages WHERE room_id = $1 AND message_type = $2",
    )
    .bind(room.id.as_i64())
    .bind(i16::from(
        synctv_core::models::ChatMessageType::SystemPlaybackChanged,
    ))
    .fetch_one(&pool)
    .await
    .checked("playback system messages should be queryable");
    assert_eq!(system_message_count, 4);
    let latest_reason = sqlx::query_scalar::<_, String>(
        "SELECT metadata ->> 'reason' FROM chat_messages WHERE room_id = $1 AND message_type = $2 ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(room.id.as_i64())
    .bind(i16::from(
        synctv_core::models::ChatMessageType::SystemPlaybackChanged,
    ))
    .fetch_one(&pool)
    .await
    .checked("playback system message reason should be queryable");
    assert_eq!(latest_reason, "auto_advance");

    let mut received_playback_changes = 0;
    while received_playback_changes < 4 {
        let event =
            tokio::time::timeout(std::time::Duration::from_secs(1), committed_events.recv())
                .await
                .checked("committed playback chat event should arrive")
                .checked("committed playback chat channel should remain open");
        if let synctv_core::models::RealtimeEvent::ChatMessageEvent { event, .. } = event {
            assert_eq!(
                event.message.message.message_type,
                synctv_core::models::ChatMessageType::SystemPlaybackChanged,
            );
            received_playback_changes += 1;
        }
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_navigation_from_empty_playback_uses_first_item_and_recent_history() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(&pool);
    let owner = user_repo
        .create(&make_user("empty_navigation_owner"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Empty Navigation".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    let media = insert_root_media(&pool, &room.id, "empty_first", 0).await;
    let playback = room_service.playback_service();
    let settings = RoomSettings {
        auto_play: AutoPlay::new(AutoPlaySettings {
            enabled: false,
            mode: PlayMode::Sequential,
            delay: 0,
        }),
        ..Default::default()
    };

    let first = playback
        .play_next_for_user(&room.id, owner.id, &settings, None)
        .await
        .checked("manual next should work with auto play disabled")
        .checked("manual next should select the first item");
    assert_eq!(first.playing_media_id, Some(media.id));

    playback
        .reset(room.id, owner.id)
        .await
        .checked("playback should stop");
    let restored = playback
        .play_previous_for_user(&room.id, owner.id, None)
        .await
        .checked("previous should use recent history")
        .checked("recent history should be available");
    assert_eq!(restored.playing_media_id, Some(media.id));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_playback_history_cleanup_preserves_cursor_and_adjacent_navigation() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(&pool);
    let owner = user_repo
        .create(&make_user("history_cleanup_owner"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "History Cleanup".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    let media = [
        insert_root_media(&pool, &room.id, "cleanup_a", 0).await,
        insert_root_media(&pool, &room.id, "cleanup_b", 1).await,
        insert_root_media(&pool, &room.id, "cleanup_c", 2).await,
        insert_root_media(&pool, &room.id, "cleanup_d", 3).await,
    ];
    let playback = room_service.playback_service();
    for item in &media {
        playback
            .switch(room.id, owner.id, Some(item.id), None, None)
            .await
            .checked("history item should be recorded");
    }
    let old_timestamp = Utc::now() - chrono::Duration::days(2);
    ensure_playback_history_partition_for(&pool, old_timestamp).await;
    sqlx::query(
        r"UPDATE room_playback_history AS history
           SET created_at = $2
           WHERE history.room_id = $1
             AND history.id <> (
                 SELECT state.history_cursor_id
                 FROM room_playback_state AS state
                 WHERE state.room_id = $1
             )",
    )
    .bind(room.id.as_i64())
    .bind(old_timestamp)
    .execute(&pool)
    .await
    .checked("history timestamps should be updated");

    let deleted = PlaybackHistoryRepository::new(pool.clone())
        .cleanup(1, 0)
        .await
        .checked("history cleanup should succeed");
    assert_eq!(deleted, 2);
    let history = playback
        .list_playback_history(&room.id, None, 10)
        .await
        .checked("retained history should be listed");
    assert_eq!(history.entries.len(), 2);
    assert_eq!(
        history
            .entries
            .iter()
            .find(|entry| Some(entry.id) == history.history_cursor_id)
            .and_then(|entry| entry.media_id),
        Some(media[3].id)
    );

    let previous = playback
        .play_previous_for_user(&room.id, owner.id, None)
        .await
        .checked("previous navigation should succeed")
        .checked("the adjacent retained entry should exist");
    assert_eq!(previous.playing_media_id, Some(media[2].id));

    playback
        .switch(room.id, owner.id, Some(media[1].id), None, None)
        .await
        .checked("a new branch entry should be recorded");
    playback
        .switch(room.id, owner.id, Some(media[0].id), None, None)
        .await
        .checked("a second branch entry should be recorded");
    let deleted = PlaybackHistoryRepository::new(pool.clone())
        .cleanup(0, 2)
        .await
        .checked("count-based history cleanup should succeed");
    assert_eq!(deleted, 1);
    let retained = playback
        .list_playback_history(&room.id, None, 10)
        .await
        .checked("count-limited history should be listed");
    assert_eq!(retained.entries.len(), 2);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_deleted_media_and_playlist_cascade_playback_history() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(&pool);
    let owner = user_repo
        .create(&make_user("history_cascade_owner"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "History Cascade".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    let first_playlist = create_top_level_playlist(&pool, &room.id).await;
    let deleted_media = insert_media(&pool, &first_playlist.id, &room.id, "deleted_media", 0).await;
    let retained_media = insert_root_media(&pool, &room.id, "retained_media", 1).await;
    let playback = room_service.playback_service();

    playback
        .switch(room.id, owner.id, Some(deleted_media.id), None, None)
        .await
        .checked("deleted media history should be recorded");
    playback
        .switch(room.id, owner.id, Some(retained_media.id), None, None)
        .await
        .checked("retained media history should be recorded");
    sqlx::query("DELETE FROM media WHERE id = $1 AND room_id = $2")
        .bind(deleted_media.id.as_i64())
        .bind(room.id.as_i64())
        .execute(&pool)
        .await
        .checked("media deletion should succeed");

    let second_playlist = create_top_level_playlist(&pool, &room.id).await;
    let playlist_media = insert_media(
        &pool,
        &second_playlist.id,
        &room.id,
        "deleted_playlist_media",
        0,
    )
    .await;
    playback
        .switch(room.id, owner.id, Some(playlist_media.id), None, None)
        .await
        .checked("playlist media history should be recorded");
    playback
        .switch(room.id, owner.id, Some(retained_media.id), None, None)
        .await
        .checked("retained media should be current");
    sqlx::query("DELETE FROM media WHERE playlist_id = $1 AND room_id = $2")
        .bind(second_playlist.id.as_i64())
        .bind(room.id.as_i64())
        .execute(&pool)
        .await
        .checked("playlist media deletion should succeed");
    sqlx::query("DELETE FROM playlists WHERE id = $1 AND room_id = $2")
        .bind(second_playlist.id.as_i64())
        .bind(room.id.as_i64())
        .execute(&pool)
        .await
        .checked("playlist deletion should succeed");

    let history = playback
        .list_playback_history(&room.id, None, 20)
        .await
        .checked("history should be listed");
    assert!(history
        .entries
        .iter()
        .all(|entry| entry.media_id != Some(deleted_media.id)
            && entry.media_id != Some(playlist_media.id)));
    assert!(history
        .entries
        .iter()
        .any(|entry| Some(entry.id) == history.history_cursor_id));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_history_view_permission_cannot_change_playback() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(&pool);
    let owner = user_repo
        .create(&make_user("history_permission_owner"))
        .await
        .checked("test operation should succeed");
    let viewer = user_repo
        .create(&make_user("history_permission_viewer"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "History Permission".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    room_service
        .join_room(room.id, viewer.id, None)
        .await
        .checked("viewer should join");
    room_service
        .member_service()
        .set_member_role(room.id, owner.id, viewer.id, RoomRole::Admin)
        .await
        .checked("viewer should become an admin");
    room_service
        .member_service()
        .set_member_permissions(
            room.id,
            owner.id,
            viewer.id,
            RoomAdminPermissionBits::VIEW_PLAYBACK_HISTORY,
            RoomAdminPermissionBits::NAVIGATE_PLAYBACK,
        )
        .await
        .checked("viewer permissions should be updated");

    let media_a = insert_root_media(&pool, &room.id, "permission_a", 0).await;
    let media_b = insert_root_media(&pool, &room.id, "permission_b", 1).await;
    let playback = room_service.playback_service();
    playback
        .switch(room.id, owner.id, Some(media_a.id), None, None)
        .await
        .checked("first history entry should be recorded");
    playback
        .switch(room.id, owner.id, Some(media_b.id), None, None)
        .await
        .checked("second history entry should be recorded");
    let history = playback
        .list_playback_history(&room.id, None, 10)
        .await
        .checked("history should be listed");
    let entry_id = history
        .entries
        .last()
        .checked("history should contain an entry")
        .id;
    let settings = make_settings_with_mode(PlayMode::Sequential);

    assert!(playback
        .play_next_for_user(&room.id, viewer.id, &settings, None)
        .await
        .is_err());
    assert!(playback
        .play_history_entry_for_user(&room.id, viewer.id, entry_id, None)
        .await
        .is_err());
    let state = playback
        .get_state(&room.id)
        .await
        .checked("playback state should remain available");
    assert_eq!(state.playing_media_id, Some(media_b.id));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_previous_recomputes_history_after_optimistic_retry() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(&pool);
    let owner = user_repo
        .create(&make_user("previous_retry_owner"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Previous Retry".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    let media_a = insert_root_media(&pool, &room.id, "retry_a", 0).await;
    let media_b = insert_root_media(&pool, &room.id, "retry_b", 1).await;
    let media_c = insert_root_media(&pool, &room.id, "retry_c", 2).await;
    let media_d = insert_root_media(&pool, &room.id, "retry_d", 3).await;
    let playback = room_service.playback_service();
    for media in [&media_a, &media_b, &media_c] {
        playback
            .switch(room.id, owner.id, Some(media.id), None, None)
            .await
            .checked("history entry should be recorded");
    }

    let mut blocker = pool
        .begin()
        .await
        .checked("blocker transaction should begin");
    sqlx::query(
        "SELECT id FROM room_playback_history WHERE room_id = $1 AND media_id = $2 FOR UPDATE",
    )
    .bind(room.id.as_i64())
    .bind(media_c.id.as_i64())
    .fetch_one(&mut *blocker)
    .await
    .checked("current C history row should be locked");

    let playback_for_previous = playback.clone();
    let previous_room_id = room.id;
    let previous_owner_id = owner.id;
    let previous = tokio::spawn(async move {
        playback_for_previous
            .play_previous_for_user(&previous_room_id, previous_owner_id, None)
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut tx = pool.begin().await.checked("D transaction should begin");
    let inserted = sqlx::query_as::<_, (i64, chrono::DateTime<Utc>)>(
        r"INSERT INTO room_playback_history (
               room_id, sequence, media_id, target_hash, position_seconds, selected_by_user_id
           )
           SELECT $1, 4, $2, target_hash, 0.0, $3
           FROM room_playback_history
           WHERE room_id = $1 AND media_id = $4
           ORDER BY sequence DESC
           LIMIT 1
           RETURNING id, created_at",
    )
    .bind(room.id.as_i64())
    .bind(media_d.id.as_i64())
    .bind(owner.id.as_i64())
    .bind(media_c.id.as_i64())
    .fetch_one(&mut *tx)
    .await
    .checked("D history entry should be inserted");
    sqlx::query(
        r"UPDATE room_playback_state
           SET playing_media_id = $2,
               playing_playlist_id = NULL,
               target = NULL,
               current_progress_id = NULL,
               history_cursor_id = $3,
               history_cursor_created_at = $4,
               playback_generation = playback_generation + 1,
               version = version + 1
           WHERE room_id = $1",
    )
    .bind(room.id.as_i64())
    .bind(media_d.id.as_i64())
    .bind(inserted.0)
    .bind(inserted.1)
    .execute(&mut *tx)
    .await
    .checked("D state should be prepared");
    tx.commit().await.checked("D transition should commit");
    blocker
        .commit()
        .await
        .checked("current history lock should be released");

    let state = previous
        .await
        .checked("previous task should join")
        .checked("previous should succeed")
        .checked("previous history entry should exist");
    assert_eq!(state.playing_media_id, Some(media_c.id));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_sequential_advance_restarts_next_media_with_saved_progress() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(&pool);

    let owner = user_repo
        .create(&make_user("seq_next_saved_owner"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Seq Next Saved Progress".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "video1_saved_next", 0).await;
    let media2 = insert_media(&pool, &playlist.id, &room.id, "video2_saved_next", 1).await;

    let playback = room_service.playback_service();
    playback
        .switch(room.id, owner.id, Some(media2.id), None, None)
        .await
        .checked("test operation should succeed");
    playback
        .seek(room.id, owner.id, 125.0)
        .await
        .checked("test operation should succeed");
    playback
        .switch(room.id, owner.id, Some(media1.id), None, None)
        .await
        .checked("test operation should succeed");
    playback
        .seek(room.id, owner.id, 12.0)
        .await
        .checked("test operation should succeed");

    let settings = make_settings_with_mode(PlayMode::Sequential);
    let state = playback
        .play_next(&room.id, &settings)
        .await
        .checked("test operation should succeed")
        .checked("sequential play_next should advance");

    assert_eq!(state.playing_media_id, Some(media2.id));
    assert!(
        (state.position - 0.0).abs() < f64::EPSILON,
        "play_next should anchor the new media at the start, got {}",
        state.position
    );

    let stored = playback
        .get_state(&room.id)
        .await
        .checked("playback state should reload");
    assert_eq!(stored.playing_media_id, Some(media2.id));
    assert!(
        stored.position < 1.0,
        "stored playback state should keep the same restart anchor, got {}",
        stored.position
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_sequential_end_of_playlist_persists_paused_state() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(&pool);

    let owner = user_repo
        .create(&make_user("seq_end_owner"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room("Seq End".to_string(), String::new(), owner.id, None, None)
        .await
        .checked("test operation should succeed");

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "last_video", 0).await;

    let playback = room_service.playback_service();
    playback
        .switch(room.id, owner.id, Some(media1.id), None, None)
        .await
        .checked("test operation should succeed");
    playback
        .seek(room.id, owner.id, 4.0)
        .await
        .checked("test operation should succeed");

    let settings = make_settings_with_mode(PlayMode::Sequential);
    let result = playback
        .play_next(&room.id, &settings)
        .await
        .checked("test operation should succeed");

    let state = result.checked("playlist end should persist a stable state");
    assert_eq!(
        state.playing_media_id,
        Some(media1.id),
        "playlist end should keep the completed media selected"
    );
    assert!(
        !state.is_playing,
        "playlist end should pause playback so background scans skip it"
    );
    assert!(
        state.position >= 4.0,
        "playlist end should snapshot the completed playback position"
    );

    let stored = playback
        .get_state(&room.id)
        .await
        .checked("playback state should reload");
    assert_eq!(stored.playing_media_id, Some(media1.id));
    assert!(!stored.is_playing);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_repeat_one_replays_current() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(&pool);

    let owner = user_repo
        .create(&make_user("rep1_owner"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room("Rep1".to_string(), String::new(), owner.id, None, None)
        .await
        .checked("test operation should succeed");

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "repeat_me", 0).await;
    let _media2 = insert_media(&pool, &playlist.id, &room.id, "other", 1).await;

    let playback = room_service.playback_service();
    playback
        .switch(room.id, owner.id, Some(media1.id), None, None)
        .await
        .checked("test operation should succeed");

    let settings = make_settings_with_mode(PlayMode::RepeatOne);
    let result = playback
        .play_next(&room.id, &settings)
        .await
        .checked("test operation should succeed");

    assert!(result.is_some(), "RepeatOne should replay current");
    let state = result.checked("test operation should succeed");
    assert_eq!(
        state.playing_media_id,
        Some(media1.id),
        "Should replay the same media"
    );
    assert!(
        (state.position - 0.0).abs() < f64::EPSILON,
        "Should reset to start"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_repeat_all_wraps_around_at_end() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(&pool);

    let owner = user_repo
        .create(&make_user("repa_owner"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room("RepAll".to_string(), String::new(), owner.id, None, None)
        .await
        .checked("test operation should succeed");

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "first", 0).await;
    let _media2 = insert_media(&pool, &playlist.id, &room.id, "second", 1).await;
    let media3 = insert_media(&pool, &playlist.id, &room.id, "third", 2).await;

    let playback = room_service.playback_service();
    playback
        .switch(room.id, owner.id, Some(media3.id), None, None)
        .await
        .checked("test operation should succeed");

    let settings = make_settings_with_mode(PlayMode::RepeatAll);
    let result = playback
        .play_next(&room.id, &settings)
        .await
        .checked("test operation should succeed");

    assert!(result.is_some(), "RepeatAll should wrap around");
    let state = result.checked("test operation should succeed");
    assert_eq!(
        state.playing_media_id,
        Some(media1.id),
        "Should wrap back to first item"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_repeat_all_middle_advances_to_next() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(&pool);

    let owner = user_repo
        .create(&make_user("repa_mid_owner"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "RepAll Mid".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "vid_a", 0).await;
    let media2 = insert_media(&pool, &playlist.id, &room.id, "vid_b", 1).await;
    let _media3 = insert_media(&pool, &playlist.id, &room.id, "vid_c", 2).await;

    let playback = room_service.playback_service();
    playback
        .switch(room.id, owner.id, Some(media1.id), None, None)
        .await
        .checked("test operation should succeed");

    let settings = make_settings_with_mode(PlayMode::RepeatAll);
    let result = playback
        .play_next(&room.id, &settings)
        .await
        .checked("test operation should succeed");

    assert!(result.is_some());
    let state = result.checked("test operation should succeed");
    assert_eq!(
        state.playing_media_id,
        Some(media2.id),
        "RepeatAll mid-playlist should advance normally"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_shuffle_with_single_item_keeps_current_media() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(&pool);

    let owner = user_repo
        .create(&make_user("shuf_single_owner"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Shuffle Single".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "single", 0).await;

    let playback = room_service.playback_service();
    playback
        .switch(room.id, owner.id, Some(media1.id), None, None)
        .await
        .checked("test operation should succeed");

    let settings = make_settings_with_mode(PlayMode::Shuffle);
    let result = playback
        .play_next(&room.id, &settings)
        .await
        .checked("test operation should succeed");

    assert!(result.is_some(), "Shuffle should keep playback active");
    let state = result.checked("test operation should succeed");
    assert_eq!(
        state.playing_media_id,
        Some(media1.id),
        "Single-item shuffle must keep the only available media selected"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_shuffle_with_multiple_items_excludes_current_media() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(&pool);

    let owner = user_repo
        .create(&make_user("shuf_multi_owner"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Shuffle Multi".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "multi1", 0).await;
    let media2 = insert_media(&pool, &playlist.id, &room.id, "multi2", 1).await;

    let playback = room_service.playback_service();
    playback
        .switch(room.id, owner.id, Some(media1.id), None, None)
        .await
        .checked("test operation should succeed");

    let settings = make_settings_with_mode(PlayMode::Shuffle);
    let result = playback
        .play_next(&room.id, &settings)
        .await
        .checked("test operation should succeed");

    assert!(result.is_some(), "Shuffle should choose an available media");
    let state = result.checked("test operation should succeed");
    assert_eq!(
        state.playing_media_id,
        Some(media2.id),
        "With one alternative media, shuffle must select that alternative instead of repeating current"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_auto_play_disabled_returns_none() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(&pool);

    let owner = user_repo
        .create(&make_user("noauto_owner"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room("No Auto".to_string(), String::new(), owner.id, None, None)
        .await
        .checked("test operation should succeed");

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "vid", 0).await;
    insert_media(&pool, &playlist.id, &room.id, "vid2", 1).await;

    let playback = room_service.playback_service();
    playback
        .switch(room.id, owner.id, Some(media1.id), None, None)
        .await
        .checked("test operation should succeed");

    // Disabled: auto_play.enabled = false
    let settings = RoomSettings {
        auto_play: AutoPlay::new(AutoPlaySettings {
            enabled: false,
            mode: PlayMode::Sequential,
            delay: 0,
        }),
        ..Default::default()
    };

    let result = playback
        .play_next(&room.id, &settings)
        .await
        .checked("test operation should succeed");
    assert!(
        result.is_none(),
        "play_next should return None when auto_play disabled"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_empty_playlist_returns_none() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(&pool);

    let owner = user_repo
        .create(&make_user("empty_pl_owner"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room("Empty PL".to_string(), String::new(), owner.id, None, None)
        .await
        .checked("test operation should succeed");

    // Don't add any media -- playlist is empty
    let playback = room_service.playback_service();
    let settings = make_settings_with_mode(PlayMode::Sequential);
    let result = playback
        .play_next(&room.id, &settings)
        .await
        .checked("test operation should succeed");

    assert!(result.is_none(), "Empty playlist should return None");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_no_current_media_plays_first() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(&pool);

    let owner = user_repo
        .create(&make_user("nocur_owner"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "No Current".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let media_repo = MediaRepository::new(pool.clone());
    let media1 = media_repo
        .create(&Media {
            id: MediaId::new(),
            playlist_id: None,
            room_id: room.id,
            creator_id: None,
            name: "first_vid".to_string(),
            description: String::new(),
            position: 0.0,
            source_provider: SourceProvider::DirectUrl,
            source_config: synctv_core_testing::direct_url_media_source_config(
                "https://example.com/first_vid.mp4",
            ),
            provider_instance_name: None,
            cover_file_reference_id: None,
            thumbnail_file_reference_id: None,
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .checked("test operation should succeed");
    media_repo
        .create(&Media {
            id: MediaId::new(),
            playlist_id: None,
            room_id: room.id,
            creator_id: None,
            name: "second_vid".to_string(),
            description: String::new(),
            position: 1.0,
            source_provider: SourceProvider::DirectUrl,
            source_config: synctv_core_testing::direct_url_media_source_config(
                "https://example.com/second_vid.mp4",
            ),
            provider_instance_name: None,
            cover_file_reference_id: None,
            thumbnail_file_reference_id: None,
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .checked("test operation should succeed");

    // Don't switch to any media -- playing_media_id is None
    let playback = room_service.playback_service();
    let settings = make_settings_with_mode(PlayMode::Sequential);
    let result = playback
        .play_next(&room.id, &settings)
        .await
        .checked("test operation should succeed");

    assert!(
        result.is_some(),
        "Should play first item when nothing is playing"
    );
    let state = result.checked("test operation should succeed");
    assert_eq!(
        state.playing_media_id,
        Some(media1.id),
        "Should start with first item"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_no_current_media_ignores_other_rooms_root_media() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(&pool);

    let owner = user_repo
        .create(&make_user("nocur_root_scope_owner"))
        .await
        .checked("test operation should succeed");
    let (room_a, _) = room_service
        .create_room(
            "No Current Root A".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    let (room_b, _) = room_service
        .create_room(
            "No Current Root B".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let room_a_media = insert_root_media(&pool, &room_a.id, "room_a_first", 1).await;
    insert_root_media(&pool, &room_b.id, "room_b_first", 0).await;

    let playback = room_service.playback_service();
    let settings = make_settings_with_mode(PlayMode::Sequential);
    let result = playback
        .play_next(&room_a.id, &settings)
        .await
        .checked("test operation should succeed");

    assert!(
        result.is_some(),
        "play_next should still find room-local root media"
    );
    let state = result.checked("test operation should succeed");
    assert_eq!(
        state.playing_media_id,
        Some(room_a_media.id),
        "root playback must not advance into another room's media"
    );
    assert_eq!(state.playing_playlist_id, None);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_play_next_stops_when_next_media_creator_becomes_inactive() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(&pool);

    let room_owner = user_repo
        .create(&make_user("play_next_owner"))
        .await
        .checked("test operation should succeed");
    let next_creator = user_repo
        .create(&make_user("play_next_inactive_creator"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Play Next Inactive Media".to_string(),
            String::new(),
            room_owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media_repo = MediaRepository::new(pool.clone());
    let media1 = Media {
        id: MediaId::new(),
        playlist_id: Some(playlist.id),
        room_id: room.id,
        creator_id: Some(room_owner.id),
        name: "episode-1".to_string(),
        description: String::new(),
        position: 0.0,
        source_provider: SourceProvider::DirectUrl,
        source_config: synctv_core_testing::direct_url_media_source_config(
            "https://example.com/episode-1.mp4",
        ),
        provider_instance_name: None,
        cover_file_reference_id: None,
        thumbnail_file_reference_id: None,
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    let media2 = Media {
        id: MediaId::new(),
        playlist_id: Some(playlist.id),
        room_id: room.id,
        creator_id: Some(next_creator.id),
        name: "episode-2".to_string(),
        description: String::new(),
        position: 1.0,
        source_provider: SourceProvider::DirectUrl,
        source_config: synctv_core_testing::direct_url_media_source_config(
            "https://example.com/episode-2.mp4",
        ),
        provider_instance_name: None,
        cover_file_reference_id: None,
        thumbnail_file_reference_id: None,
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    let media1 = media_repo
        .create(&media1)
        .await
        .checked("test operation should succeed");
    media_repo
        .create(&media2)
        .await
        .checked("test operation should succeed");

    room_service
        .playback_service()
        .switch(room.id, room_owner.id, Some(media1.id), None, None)
        .await
        .checked("test operation should succeed");

    user_repo
        .ban(&next_creator.id, None, Some("play next test".to_string()))
        .await
        .checked("test operation should succeed");

    let settings = make_settings_with_mode(PlayMode::Sequential);
    let state = room_service
        .playback_service()
        .play_next(&room.id, &settings)
        .await
        .checked("test operation should succeed")
        .checked("play_next should stop playback when next media creator is inactive");

    assert!(state.playing_media_id.is_none());
    assert!(state.playing_playlist_id.is_none());
    assert!(!state.is_playing);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_dynamic_playlist_sequential_advances_by_target() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        synctv_core::repository::ProviderInstanceRepository::new(pool.clone()),
    )));
    let mut providers_manager =
        ProvidersManager::new(provider_instance_manager).checked("providers manager should build");
    providers_manager.register_factory(
        "alist",
        Box::new(|instance_id, _config, _instance_manager| {
            Ok(Arc::new(TestDynamicProvider::new(instance_id)))
        }),
    );
    let providers_manager = Arc::new(providers_manager);
    let room_service = make_room_service_with_providers(&pool, providers_manager);

    let owner = user_repo
        .create(&make_user("dynamic_seq_owner"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Dynamic Seq".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    register_alist_provider(&room_service).await;
    let playlist = create_dynamic_playlist(&pool, &room.id, &owner.id, "alist_default").await;

    let playback = room_service.playback_service();
    playback
        .switch(
            room.id,
            owner.id,
            None,
            Some(playlist.id),
            Some(alist_target("/episode-1.mp4")),
        )
        .await
        .checked("test operation should succeed");

    let settings = make_settings_with_mode(PlayMode::Sequential);
    let result = playback
        .play_next(&room.id, &settings)
        .await
        .checked("test operation should succeed");

    let state = result.checked("dynamic playlist should advance to next item");
    assert!(state.playing_media_id.is_none());
    assert_eq!(state.playing_playlist_id, Some(playlist.id));
    assert_alist_target(state.target.as_ref(), "/episode-2.mp4");
    assert!((state.position - 0.0).abs() < f64::EPSILON);
    assert!(state.is_playing);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_switch_dynamic_playlist_rejects_inactive_creator() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        synctv_core::repository::ProviderInstanceRepository::new(pool.clone()),
    )));
    let mut providers_manager =
        ProvidersManager::new(provider_instance_manager).checked("providers manager should build");
    providers_manager.register_factory(
        "alist",
        Box::new(|instance_id, _config, _instance_manager| {
            Ok(Arc::new(TestDynamicProvider::new(instance_id)))
        }),
    );
    let providers_manager = Arc::new(providers_manager);
    let room_service = make_room_service_with_providers(&pool, providers_manager);

    let room_owner = user_repo
        .create(&make_user("dynamic_inactive_owner"))
        .await
        .checked("test operation should succeed");
    let playlist_creator = user_repo
        .create(&make_user("dynamic_inactive_creator"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Dynamic Inactive Creator".to_string(),
            String::new(),
            room_owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    register_alist_provider(&room_service).await;
    let playlist =
        create_dynamic_playlist(&pool, &room.id, &playlist_creator.id, "alist_default").await;

    user_repo
        .ban(
            &playlist_creator.id,
            None,
            Some("play next test".to_string()),
        )
        .await
        .checked("test operation should succeed");

    let result = room_service
        .playback_service()
        .switch(
            room.id,
            room_owner.id,
            None,
            Some(playlist.id),
            Some(alist_target("/episode-1.mp4")),
        )
        .await;

    match result.failed("dynamic playlist created by banned user must not be playable") {
        synctv_core::Error::Authorization(message) => {
            assert!(
                message.contains("creator") && message.contains("active"),
                "error should explain creator status: {message}"
            );
        }
        other => std::panic::panic_any(format!("expected authorization error, got: {other:?}")),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_play_next_stops_when_dynamic_playlist_creator_becomes_inactive() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        synctv_core::repository::ProviderInstanceRepository::new(pool.clone()),
    )));
    let mut providers_manager =
        ProvidersManager::new(provider_instance_manager).checked("providers manager should build");
    providers_manager.register_factory(
        "alist",
        Box::new(|instance_id, _config, _instance_manager| {
            Ok(Arc::new(TestDynamicProvider::new(instance_id)))
        }),
    );
    let providers_manager = Arc::new(providers_manager);
    let room_service = make_room_service_with_providers(&pool, providers_manager);

    let room_owner = user_repo
        .create(&make_user("dynamic_play_next_owner"))
        .await
        .checked("test operation should succeed");
    let playlist_creator = user_repo
        .create(&make_user("dynamic_play_next_inactive_creator"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Dynamic Play Next Inactive Creator".to_string(),
            String::new(),
            room_owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    register_alist_provider(&room_service).await;
    let playlist =
        create_dynamic_playlist(&pool, &room.id, &playlist_creator.id, "alist_default").await;

    room_service
        .playback_service()
        .switch(
            room.id,
            room_owner.id,
            None,
            Some(playlist.id),
            Some(alist_target("/episode-1.mp4")),
        )
        .await
        .checked("test operation should succeed");

    user_repo
        .ban(
            &playlist_creator.id,
            None,
            Some("play next test".to_string()),
        )
        .await
        .checked("test operation should succeed");

    let settings = make_settings_with_mode(PlayMode::Sequential);
    let state = room_service
        .playback_service()
        .play_next(&room.id, &settings)
        .await
        .checked("test operation should succeed")
        .checked("play_next should stop playback when dynamic playlist creator is inactive");

    assert!(state.playing_media_id.is_none());
    assert!(state.playing_playlist_id.is_none());
    assert!(!state.is_playing);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_dynamic_playlist_repeat_all_wraps_to_first_item() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        synctv_core::repository::ProviderInstanceRepository::new(pool.clone()),
    )));
    let mut providers_manager =
        ProvidersManager::new(provider_instance_manager).checked("providers manager should build");
    providers_manager.register_factory(
        "alist",
        Box::new(|instance_id, _config, _instance_manager| {
            Ok(Arc::new(TestDynamicProvider::new(instance_id)))
        }),
    );
    let providers_manager = Arc::new(providers_manager);
    let room_service = make_room_service_with_providers(&pool, providers_manager);

    let owner = user_repo
        .create(&make_user("dynamic_repeat_all_owner"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Dynamic Repeat All".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    register_alist_provider(&room_service).await;
    let playlist = create_dynamic_playlist(&pool, &room.id, &owner.id, "alist_default").await;

    let playback = room_service.playback_service();
    playback
        .switch(
            room.id,
            owner.id,
            None,
            Some(playlist.id),
            Some(alist_target("/episode-2.mp4")),
        )
        .await
        .checked("test operation should succeed");

    let settings = make_settings_with_mode(PlayMode::RepeatAll);
    let result = playback
        .play_next(&room.id, &settings)
        .await
        .checked("test operation should succeed");

    let state = result.checked("dynamic playlist repeat-all should wrap to first item");
    assert!(state.playing_media_id.is_none());
    assert_eq!(state.playing_playlist_id, Some(playlist.id));
    assert_alist_target(state.target.as_ref(), "/episode-1.mp4");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_dynamic_playlist_play_next_uses_bound_provider_instance() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        synctv_core::repository::ProviderInstanceRepository::new(pool.clone()),
    )));
    let mut providers_manager =
        ProvidersManager::new(provider_instance_manager).checked("providers manager should build");
    providers_manager.register_factory(
        "alist",
        Box::new(|instance_id, _config, _instance_manager| {
            Ok(Arc::new(TestDynamicProvider::new(instance_id)))
        }),
    );
    let providers_manager = Arc::new(providers_manager);
    let room_service = make_room_service_with_providers(&pool, providers_manager);

    let owner = user_repo
        .create(&make_user("dynamic_bound_instance_owner"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Dynamic Bound Instance".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    register_alist_provider(&room_service).await;
    register_alist_provider_instance(&room_service, "alist_alt").await;
    let playlist = create_dynamic_playlist(&pool, &room.id, &owner.id, "alist_alt").await;

    let playback = room_service.playback_service();
    playback
        .switch(
            room.id,
            owner.id,
            None,
            Some(playlist.id),
            Some(alist_target("/bound-episode-1.mp4")),
        )
        .await
        .checked("test operation should succeed");

    let settings = make_settings_with_mode(PlayMode::Sequential);
    let result = playback
        .play_next(&room.id, &settings)
        .await
        .checked("test operation should succeed");

    let state = result.checked("dynamic playlist should advance using the bound provider instance");
    assert!(state.playing_media_id.is_none());
    assert_eq!(state.playing_playlist_id, Some(playlist.id));
    assert_alist_target(state.target.as_ref(), "/bound-episode-2.mp4");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_dynamic_playlist_items_passes_credential_encryption_to_provider_context() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        synctv_core::repository::ProviderInstanceRepository::new(pool.clone()),
    )));
    let mut providers_manager =
        ProvidersManager::new(provider_instance_manager).checked("providers manager should build");
    providers_manager.register_factory(
        "alist",
        Box::new(|instance_id, _config, _instance_manager| {
            Ok(Arc::new(
                TestDynamicProvider::requiring_credential_encryption(instance_id),
            ))
        }),
    );
    let providers_manager = Arc::new(providers_manager);
    let room_service = RoomService::new_with_providers_and_options(
        pool.clone(),
        make_user_service(&pool),
        providers_manager,
        RoomServiceOptions {
            credential_encryption: Some(
                CredentialEncryption::new(&[0x42; 32])
                    .checked("test encryption key should be valid"),
            ),
            credential_repo: Some(Arc::new(UserProviderCredentialRepository::new(
                pool.clone(),
            ))),
            ..RoomServiceOptions::test_defaults_with_settings(pool.clone())
        },
    )
    .checked("room service should build");

    let owner = user_repo
        .create(&make_user("dynamic_sensitive_list_owner"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Dynamic Sensitive List".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    register_alist_provider_instance_requiring_encryption(&room_service, "alist_sensitive_default")
        .await;
    let playlist =
        create_dynamic_sensitive_playlist(&pool, &room.id, &owner.id, "alist_sensitive_default")
            .await;

    let items = room_service
        .media_service()
        .list_dynamic_playlist_items(
            room.id,
            owner.id,
            &playlist.id,
            None,
            DynamicListQuery {
                pagination: DynamicPagination::Page { page: 1 },
                page_size: 20,
                ..DynamicListQuery::default()
            },
        )
        .await
        .checked("dynamic playlist listing should receive credential encryption");

    assert_eq!(items.len(), 2);
}
