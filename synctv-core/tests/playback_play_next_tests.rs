//! `PlaybackService::play_next` logic tests
//!
//! Tests the `play_next` method's playlist navigation logic for each `PlayMode`,
//! including edge cases like deleted media and empty playlists.
//!
//! These tests exercise the `play_next` decision logic with a real `PostgreSQL`
//! via testcontainers, since `play_next` reads from the DB repo layer.
//!
//! Run with: cargo test -p synctv-core --test `playback_play_next_tests` -- --nocapture
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    config::PasswordComplexityConfig,
    credential_encryption::CredentialEncryption,
    models::{
        room::AutoPlaySettings, room_settings::AutoPlay, Media, MediaId, PlayMode, Playlist,
        PlaylistId, ProviderInstance, RoomId, RoomSettings, User, UserId, UserRole, UserStatus,
    },
    provider::{
        DirectoryItem, DynamicFolder, DynamicListQuery, ItemType, MediaProvider, NextPlayItem,
        PlaybackInfo, PlaybackResult, ProviderContext, ProviderError,
    },
    repository::{
        MediaRepository, ProviderInstanceRepository, UserProviderCredentialRepository,
        UserRepository,
    },
    service::{
        auth::{BruteForceProtection, JwtService, TestPasswordHasher},
        room::RoomServiceOptions,
        InMemoryTokenBlacklistStore, RoomService, UserService,
    },
    service::{ProvidersManager, RemoteProviderManager},
};
use synctv_core_testing::create_test_pool;
fn make_user_service(pool: &PgPool) -> UserService {
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
    let user_service = make_user_service(&pool);
    let mut svc = RoomService::new(pool, user_service);
    svc.set_password_hasher(Arc::new(TestPasswordHasher::new()));
    svc
}

fn make_room_service_with_providers(
    pool: PgPool,
    providers_manager: Arc<ProvidersManager>,
) -> RoomService {
    let user_service = make_user_service(&pool);
    let credential_encryption =
        CredentialEncryption::new(&[0x42; 32]).expect("test encryption key should be valid");
    let credential_repo = Arc::new(UserProviderCredentialRepository::new_with_encryption(
        pool.clone(),
        credential_encryption.clone(),
    ));
    let mut svc = RoomService::new_with_providers_and_options(
        pool,
        user_service,
        providers_manager,
        RoomServiceOptions {
            credential_encryption: Some(credential_encryption),
            credential_repo: Some(credential_repo),
            ..RoomServiceOptions::default()
        },
    );
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
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        signup_method: synctv_core::models::SignupMethod::Email,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
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

fn dynamic_target(cursor: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "relative_path": cursor }))
        .expect("dynamic playback target should serialize")
}

fn decode_dynamic_target(target: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(target)
        .expect("dynamic playback target should deserialize")
        .get("relative_path")
        .and_then(serde_json::Value::as_str)
        .expect("dynamic playback target should contain provider cursor")
        .to_string()
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
        .expect("Top-level playlist should be created")
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
        source_provider: "direct_url".to_string(),
        source_config: serde_json::json!({"url": format!("https://example.com/{}.mp4", name)}),
        provider_instance_name: None,
        cover_file_reference_id: None,
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    let media_repo = MediaRepository::new(pool.clone());
    media_repo
        .create(&media)
        .await
        .expect("Failed to create media")
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
        source_provider: "direct_url".to_string(),
        source_config: serde_json::json!({"url": format!("https://example.com/{}.mp4", name)}),
        provider_instance_name: None,
        cover_file_reference_id: None,
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    MediaRepository::new(pool.clone())
        .create(&media)
        .await
        .expect("Failed to create root media")
}

#[derive(Debug)]
struct FakeDynamicProvider {
    instance_id: String,
    provider_type: &'static str,
    require_credential_encryption: bool,
}

impl FakeDynamicProvider {
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
                urls: vec![format!("https://{}.example.com{path}", self.instance_id)],
                format: "mp4".to_string(),
                headers: std::collections::HashMap::new(),
                subtitles: Vec::new(),
                expires_at: None,
                cors_proxy_required: false,
            },
        );
        PlaybackResult {
            playback_infos: infos,
            default_mode: "direct".to_string(),
            metadata: std::collections::HashMap::new(),
        }
    }

    fn item(path: &str) -> NextPlayItem {
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        NextPlayItem {
            name,
            item_type: ItemType::Media,
            source_config: serde_json::json!({ "path": path }),
            metadata: serde_json::json!({}),
            provider_data: serde_json::json!({}),
            target: dynamic_target(path),
        }
    }
}

#[async_trait]
impl MediaProvider for FakeDynamicProvider {
    fn name(&self) -> &'static str {
        self.provider_type
    }

    async fn generate_playback(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &serde_json::Value,
    ) -> Result<PlaybackResult, ProviderError> {
        let path = source_config
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ProviderError::InvalidConfig("Missing path".to_string()))?;
        Ok(self.playback_result_for(path))
    }

    fn as_dynamic_folder(&self) -> Option<&dyn DynamicFolder> {
        Some(self)
    }
}

#[async_trait]
impl DynamicFolder for FakeDynamicProvider {
    async fn list_playlist(
        &self,
        ctx: &ProviderContext<'_>,
        _playlist: &Playlist,
        target: Option<&[u8]>,
        _query: DynamicListQuery,
    ) -> Result<Vec<DirectoryItem>, ProviderError> {
        if self.require_credential_encryption && ctx.credential_encryption.is_none() {
            return Err(ProviderError::EncryptionRequired(self.provider_type));
        }
        let items = match target
            .map(decode_dynamic_target)
            .as_deref()
            .unwrap_or_default()
        {
            "" => vec![
                DirectoryItem {
                    name: self
                        .first_episode_path()
                        .trim_start_matches('/')
                        .to_string(),
                    item_type: ItemType::Media,
                    target: dynamic_target(self.first_episode_path()),
                    size: None,
                    thumbnail: None,
                    description: None,
                    modified_at: None,
                },
                DirectoryItem {
                    name: self
                        .second_episode_path()
                        .trim_start_matches('/')
                        .to_string(),
                    item_type: ItemType::Media,
                    target: dynamic_target(self.second_episode_path()),
                    size: None,
                    thumbnail: None,
                    description: None,
                    modified_at: None,
                },
            ],
            _ => Vec::new(),
        };
        Ok(items)
    }

    async fn resolve_item(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &Playlist,
        target: &[u8],
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        let cursor = decode_dynamic_target(target);
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
        _playing_media: &Media,
        target: &[u8],
        play_mode: PlayMode,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        let cursor = decode_dynamic_target(target);
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
    room_service
        .media_service()
        .providers_manager()
        .create_provider("alist", instance_id, &serde_json::json!({}))
        .await
        .expect("Failed to register fake dynamic provider");
}

async fn register_alist_provider_instance_requiring_encryption(
    room_service: &RoomService,
    instance_id: &str,
) {
    room_service
        .media_service()
        .providers_manager()
        .create_provider("alist", instance_id, &serde_json::json!({}))
        .await
        .expect("Failed to register fake dynamic provider requiring encryption");
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
        source_provider: Some("alist".to_string()),
        source_config: Some(serde_json::json!({})),
        provider_instance_name: Some(provider_instance_name.to_string()),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    synctv_core::repository::PlaylistRepository::new(pool.clone())
        .create(&playlist)
        .await
        .expect("Dynamic playlist should be created")
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
        source_provider: Some("alist".to_string()),
        source_config: Some(serde_json::json!({})),
        provider_instance_name: Some(provider_instance_name.to_string()),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    synctv_core::repository::PlaylistRepository::new(pool.clone())
        .create(&playlist)
        .await
        .expect("Dynamic sensitive playlist should be created")
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
        providers: vec![provider.to_string()],
        enabled: true,
        created_at: now,
        updated_at: now,
    };
    ProviderInstanceRepository::new(pool.clone())
        .create(&instance)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_sequential_advance_to_next() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("seq_next_owner"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room("Seq Next".to_string(), String::new(), owner.id, None, None)
        .await
        .unwrap();

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "video1", 0).await;
    let media2 = insert_media(&pool, &playlist.id, &room.id, "video2", 1).await;

    // Set currently playing to media1
    let playback = room_service.playback_service();
    playback
        .switch(room.id, owner.id, Some(media1.id), None, Vec::new())
        .await
        .unwrap();

    let settings = make_settings_with_mode(PlayMode::Sequential);
    let result = playback.play_next(&room.id, &settings).await.unwrap();

    assert!(result.is_some(), "Should advance to next media");
    let state = result.unwrap();
    assert_eq!(
        state.playing_media_id,
        Some(media2.id),
        "Should be playing media2"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_sequential_end_of_playlist_returns_none() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("seq_end_owner")).await.unwrap();
    let (room, _) = room_service
        .create_room("Seq End".to_string(), String::new(), owner.id, None, None)
        .await
        .unwrap();

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "last_video", 0).await;

    let playback = room_service.playback_service();
    playback
        .switch(room.id, owner.id, Some(media1.id), None, Vec::new())
        .await
        .unwrap();

    let settings = make_settings_with_mode(PlayMode::Sequential);
    let result = playback.play_next(&room.id, &settings).await.unwrap();

    assert!(result.is_none(), "Should return None at end of playlist");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_repeat_one_replays_current() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("rep1_owner")).await.unwrap();
    let (room, _) = room_service
        .create_room("Rep1".to_string(), String::new(), owner.id, None, None)
        .await
        .unwrap();

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "repeat_me", 0).await;
    let _media2 = insert_media(&pool, &playlist.id, &room.id, "other", 1).await;

    let playback = room_service.playback_service();
    playback
        .switch(room.id, owner.id, Some(media1.id), None, Vec::new())
        .await
        .unwrap();

    let settings = make_settings_with_mode(PlayMode::RepeatOne);
    let result = playback.play_next(&room.id, &settings).await.unwrap();

    assert!(result.is_some(), "RepeatOne should replay current");
    let state = result.unwrap();
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
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("repa_owner")).await.unwrap();
    let (room, _) = room_service
        .create_room("RepAll".to_string(), String::new(), owner.id, None, None)
        .await
        .unwrap();

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "first", 0).await;
    let _media2 = insert_media(&pool, &playlist.id, &room.id, "second", 1).await;
    let media3 = insert_media(&pool, &playlist.id, &room.id, "third", 2).await;

    let playback = room_service.playback_service();
    playback
        .switch(room.id, owner.id, Some(media3.id), None, Vec::new())
        .await
        .unwrap();

    let settings = make_settings_with_mode(PlayMode::RepeatAll);
    let result = playback.play_next(&room.id, &settings).await.unwrap();

    assert!(result.is_some(), "RepeatAll should wrap around");
    let state = result.unwrap();
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
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("repa_mid_owner"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "RepAll Mid".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "vid_a", 0).await;
    let media2 = insert_media(&pool, &playlist.id, &room.id, "vid_b", 1).await;
    let _media3 = insert_media(&pool, &playlist.id, &room.id, "vid_c", 2).await;

    let playback = room_service.playback_service();
    playback
        .switch(room.id, owner.id, Some(media1.id), None, Vec::new())
        .await
        .unwrap();

    let settings = make_settings_with_mode(PlayMode::RepeatAll);
    let result = playback.play_next(&room.id, &settings).await.unwrap();

    assert!(result.is_some());
    let state = result.unwrap();
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
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("shuf_single_owner"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "Shuffle Single".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "single", 0).await;

    let playback = room_service.playback_service();
    playback
        .switch(room.id, owner.id, Some(media1.id), None, Vec::new())
        .await
        .unwrap();

    let settings = make_settings_with_mode(PlayMode::Shuffle);
    let result = playback.play_next(&room.id, &settings).await.unwrap();

    assert!(result.is_some(), "Shuffle should keep playback active");
    let state = result.unwrap();
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
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("shuf_multi_owner"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "Shuffle Multi".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "multi1", 0).await;
    let media2 = insert_media(&pool, &playlist.id, &room.id, "multi2", 1).await;

    let playback = room_service.playback_service();
    playback
        .switch(room.id, owner.id, Some(media1.id), None, Vec::new())
        .await
        .unwrap();

    let settings = make_settings_with_mode(PlayMode::Shuffle);
    let result = playback.play_next(&room.id, &settings).await.unwrap();

    assert!(result.is_some(), "Shuffle should choose an available media");
    let state = result.unwrap();
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
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("noauto_owner")).await.unwrap();
    let (room, _) = room_service
        .create_room("No Auto".to_string(), String::new(), owner.id, None, None)
        .await
        .unwrap();

    let playlist = create_top_level_playlist(&pool, &room.id).await;
    let media1 = insert_media(&pool, &playlist.id, &room.id, "vid", 0).await;
    insert_media(&pool, &playlist.id, &room.id, "vid2", 1).await;

    let playback = room_service.playback_service();
    playback
        .switch(room.id, owner.id, Some(media1.id), None, Vec::new())
        .await
        .unwrap();

    // Disabled: auto_play.enabled = false
    let settings = RoomSettings {
        auto_play: AutoPlay::new(AutoPlaySettings {
            enabled: false,
            mode: PlayMode::Sequential,
            delay: 0,
        }),
        ..Default::default()
    };

    let result = playback.play_next(&room.id, &settings).await.unwrap();
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
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("empty_pl_owner"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room("Empty PL".to_string(), String::new(), owner.id, None, None)
        .await
        .unwrap();

    // Don't add any media -- playlist is empty
    let playback = room_service.playback_service();
    let settings = make_settings_with_mode(PlayMode::Sequential);
    let result = playback.play_next(&room.id, &settings).await.unwrap();

    assert!(result.is_none(), "Empty playlist should return None");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_no_current_media_plays_first() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("nocur_owner")).await.unwrap();
    let (room, _) = room_service
        .create_room(
            "No Current".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

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
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({"url": "https://example.com/first_vid.mp4"}),
            provider_instance_name: None,
            cover_file_reference_id: None,
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .unwrap();
    media_repo
        .create(&Media {
            id: MediaId::new(),
            playlist_id: None,
            room_id: room.id,
            creator_id: None,
            name: "second_vid".to_string(),
            description: String::new(),
            position: 1.0,
            source_provider: "direct_url".to_string(),
            source_config: serde_json::json!({"url": "https://example.com/second_vid.mp4"}),
            provider_instance_name: None,
            cover_file_reference_id: None,
            added_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        })
        .await
        .unwrap();

    // Don't switch to any media -- playing_media_id is None
    let playback = room_service.playback_service();
    let settings = make_settings_with_mode(PlayMode::Sequential);
    let result = playback.play_next(&room.id, &settings).await.unwrap();

    assert!(
        result.is_some(),
        "Should play first item when nothing is playing"
    );
    let state = result.unwrap();
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
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("nocur_root_scope_owner"))
        .await
        .unwrap();
    let (room_a, _) = room_service
        .create_room(
            "No Current Root A".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();
    let (room_b, _) = room_service
        .create_room(
            "No Current Root B".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let room_a_media = insert_root_media(&pool, &room_a.id, "room_a_first", 1).await;
    insert_root_media(&pool, &room_b.id, "room_b_first", 0).await;

    let playback = room_service.playback_service();
    let settings = make_settings_with_mode(PlayMode::Sequential);
    let result = playback.play_next(&room_a.id, &settings).await.unwrap();

    assert!(
        result.is_some(),
        "play_next should still find room-local root media"
    );
    let state = result.unwrap();
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
    let room_service = make_room_service(pool.clone());

    let room_owner = user_repo
        .create(&make_user("play_next_owner"))
        .await
        .unwrap();
    let next_creator = user_repo
        .create(&make_user("play_next_inactive_creator"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "Play Next Inactive Media".to_string(),
            String::new(),
            room_owner.id,
            None,
            None,
        )
        .await
        .unwrap();

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
        source_provider: "direct_url".to_string(),
        source_config: serde_json::json!({"url": "https://example.com/episode-1.mp4"}),
        provider_instance_name: None,
        cover_file_reference_id: None,
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
        source_provider: "direct_url".to_string(),
        source_config: serde_json::json!({"url": "https://example.com/episode-2.mp4"}),
        provider_instance_name: None,
        cover_file_reference_id: None,
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    let media1 = media_repo.create(&media1).await.unwrap();
    media_repo.create(&media2).await.unwrap();

    room_service
        .playback_service()
        .switch(room.id, room_owner.id, Some(media1.id), None, Vec::new())
        .await
        .unwrap();

    user_repo
        .ban(&next_creator.id, None, Some("play next test".to_string()))
        .await
        .unwrap();

    let settings = make_settings_with_mode(PlayMode::Sequential);
    let state = room_service
        .playback_service()
        .play_next(&room.id, &settings)
        .await
        .unwrap()
        .expect("play_next should stop playback when next media creator is inactive");

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
    let mut providers_manager = ProvidersManager::new(provider_instance_manager);
    providers_manager.register_factory(
        "alist",
        Box::new(|instance_id, _config, _instance_manager| {
            Ok(Arc::new(FakeDynamicProvider::new(instance_id)))
        }),
    );
    let providers_manager = Arc::new(providers_manager);
    let room_service = make_room_service_with_providers(pool.clone(), providers_manager);

    let owner = user_repo
        .create(&make_user("dynamic_seq_owner"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "Dynamic Seq".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    register_alist_provider(&room_service).await;
    let playlist = create_dynamic_playlist(&pool, &room.id, &owner.id, "alist_default").await;

    let playback = room_service.playback_service();
    playback
        .switch(
            room.id,
            owner.id,
            None,
            Some(playlist.id),
            dynamic_target("/episode-1.mp4"),
        )
        .await
        .unwrap();

    let settings = make_settings_with_mode(PlayMode::Sequential);
    let result = playback.play_next(&room.id, &settings).await.unwrap();

    let state = result.expect("dynamic playlist should advance to next item");
    assert!(state.playing_media_id.is_none());
    assert_eq!(state.playing_playlist_id, Some(playlist.id));
    let target: serde_json::Value = serde_json::from_slice(&state.target).unwrap();
    assert_eq!(
        target,
        serde_json::json!({"relative_path":"/episode-2.mp4"})
    );
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
    let mut providers_manager = ProvidersManager::new(provider_instance_manager);
    providers_manager.register_factory(
        "alist",
        Box::new(|instance_id, _config, _instance_manager| {
            Ok(Arc::new(FakeDynamicProvider::new(instance_id)))
        }),
    );
    let providers_manager = Arc::new(providers_manager);
    let room_service = make_room_service_with_providers(pool.clone(), providers_manager);

    let room_owner = user_repo
        .create(&make_user("dynamic_inactive_owner"))
        .await
        .unwrap();
    let playlist_creator = user_repo
        .create(&make_user("dynamic_inactive_creator"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Dynamic Inactive Creator".to_string(),
            String::new(),
            room_owner.id,
            None,
            None,
        )
        .await
        .unwrap();

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
        .unwrap();

    let result = room_service
        .playback_service()
        .switch(
            room.id,
            room_owner.id,
            None,
            Some(playlist.id),
            dynamic_target("/episode-1.mp4"),
        )
        .await;

    match result.expect_err("dynamic playlist created by banned user must not be playable") {
        synctv_core::Error::Authorization(message) => {
            assert!(
                message.contains("creator") && message.contains("active"),
                "error should explain creator status: {message}"
            );
        }
        other => panic!("expected authorization error, got: {other:?}"),
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
    let mut providers_manager = ProvidersManager::new(provider_instance_manager);
    providers_manager.register_factory(
        "alist",
        Box::new(|instance_id, _config, _instance_manager| {
            Ok(Arc::new(FakeDynamicProvider::new(instance_id)))
        }),
    );
    let providers_manager = Arc::new(providers_manager);
    let room_service = make_room_service_with_providers(pool.clone(), providers_manager);

    let room_owner = user_repo
        .create(&make_user("dynamic_play_next_owner"))
        .await
        .unwrap();
    let playlist_creator = user_repo
        .create(&make_user("dynamic_play_next_inactive_creator"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Dynamic Play Next Inactive Creator".to_string(),
            String::new(),
            room_owner.id,
            None,
            None,
        )
        .await
        .unwrap();

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
            dynamic_target("/episode-1.mp4"),
        )
        .await
        .unwrap();

    user_repo
        .ban(
            &playlist_creator.id,
            None,
            Some("play next test".to_string()),
        )
        .await
        .unwrap();

    let settings = make_settings_with_mode(PlayMode::Sequential);
    let state = room_service
        .playback_service()
        .play_next(&room.id, &settings)
        .await
        .unwrap()
        .expect("play_next should stop playback when dynamic playlist creator is inactive");

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
    let mut providers_manager = ProvidersManager::new(provider_instance_manager);
    providers_manager.register_factory(
        "alist",
        Box::new(|instance_id, _config, _instance_manager| {
            Ok(Arc::new(FakeDynamicProvider::new(instance_id)))
        }),
    );
    let providers_manager = Arc::new(providers_manager);
    let room_service = make_room_service_with_providers(pool.clone(), providers_manager);

    let owner = user_repo
        .create(&make_user("dynamic_repeat_all_owner"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "Dynamic Repeat All".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    register_alist_provider(&room_service).await;
    let playlist = create_dynamic_playlist(&pool, &room.id, &owner.id, "alist_default").await;

    let playback = room_service.playback_service();
    playback
        .switch(
            room.id,
            owner.id,
            None,
            Some(playlist.id),
            dynamic_target("/episode-2.mp4"),
        )
        .await
        .unwrap();

    let settings = make_settings_with_mode(PlayMode::RepeatAll);
    let result = playback.play_next(&room.id, &settings).await.unwrap();

    let state = result.expect("dynamic playlist repeat-all should wrap to first item");
    assert!(state.playing_media_id.is_none());
    assert_eq!(state.playing_playlist_id, Some(playlist.id));
    let target: serde_json::Value = serde_json::from_slice(&state.target).unwrap();
    assert_eq!(
        target,
        serde_json::json!({"relative_path":"/episode-1.mp4"})
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_dynamic_playlist_play_next_uses_bound_provider_instance() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        synctv_core::repository::ProviderInstanceRepository::new(pool.clone()),
    )));
    let mut providers_manager = ProvidersManager::new(provider_instance_manager);
    providers_manager.register_factory(
        "alist",
        Box::new(|instance_id, _config, _instance_manager| {
            Ok(Arc::new(FakeDynamicProvider::new(instance_id)))
        }),
    );
    let providers_manager = Arc::new(providers_manager);
    let room_service = make_room_service_with_providers(pool.clone(), providers_manager);

    let owner = user_repo
        .create(&make_user("dynamic_bound_instance_owner"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "Dynamic Bound Instance".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

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
            dynamic_target("/bound-episode-1.mp4"),
        )
        .await
        .unwrap();

    let settings = make_settings_with_mode(PlayMode::Sequential);
    let result = playback.play_next(&room.id, &settings).await.unwrap();

    let state = result.expect("dynamic playlist should advance using the bound provider instance");
    assert!(state.playing_media_id.is_none());
    assert_eq!(state.playing_playlist_id, Some(playlist.id));
    let target: serde_json::Value = serde_json::from_slice(&state.target).unwrap();
    assert_eq!(
        target,
        serde_json::json!({"relative_path":"/bound-episode-2.mp4"})
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_dynamic_playlist_items_passes_credential_encryption_to_provider_context() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        synctv_core::repository::ProviderInstanceRepository::new(pool.clone()),
    )));
    let mut providers_manager = ProvidersManager::new(provider_instance_manager);
    providers_manager.register_factory(
        "alist",
        Box::new(|instance_id, _config, _instance_manager| {
            Ok(Arc::new(
                FakeDynamicProvider::requiring_credential_encryption(instance_id),
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
                    .expect("test encryption key should be valid"),
            ),
            credential_repo: Some(Arc::new(UserProviderCredentialRepository::new(
                pool.clone(),
            ))),
            password_hasher: Some(Arc::new(TestPasswordHasher::new())),
            ..RoomServiceOptions::default()
        },
    );

    let owner = user_repo
        .create(&make_user("dynamic_sensitive_list_owner"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "Dynamic Sensitive List".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

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
                page: 1,
                page_size: 20,
                ..DynamicListQuery::default()
            },
        )
        .await
        .expect("dynamic playlist listing should receive credential encryption");

    assert_eq!(items.len(), 2);
}
