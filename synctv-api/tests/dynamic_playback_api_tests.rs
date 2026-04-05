#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::Utc;
use synctv_api::impls::ClientApiImpl;
use synctv_cluster::sync::{ConnectionLimits, ConnectionManager};
use synctv_core::{
    cache::{KeyBuilder, NoopCacheL2, UsernameCache},
    config::PasswordComplexityConfig,
    models::{
        PlayMode, Playlist, PlaylistId, RoomId, SignupMethod, User, UserId, UserRole, UserStatus,
    },
    provider::{
        DirectoryItem, DynamicBrowsePathSegment, DynamicFolder, ItemType, MediaProvider,
        NextPlayItem, PlaybackInfo, PlaybackResult, ProviderContext, ProviderError,
    },
    repository::{ProviderInstanceRepository, UserRepository},
    service::{
        auth::{BruteForceProtection, JwtService, TestPasswordHasher},
        InMemoryTokenBlacklistStore, ProvidersManager, RemoteProviderManager, RoomService,
        UserService,
    },
    Config,
};
use synctv_core_testing::create_test_pool;

fn dynamic_target(cursor: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "relative_path": cursor }))
        .expect("dynamic target should serialize")
}

fn decode_dynamic_target(target: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(target)
        .expect("dynamic target should deserialize")
        .get("relative_path")
        .and_then(serde_json::Value::as_str)
        .expect("dynamic target should contain provider cursor")
        .to_string()
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
        signup_method: SignupMethod::Email,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
    }
}

fn make_user_service(pool: sqlx::PgPool) -> UserService {
    let jwt_service = JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap();
    let username_cache =
        UsernameCache::new(Arc::new(NoopCacheL2), "test:username:".to_string(), 100, 60);
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let mut svc = UserService::new(
        pool,
        jwt_service,
        username_cache,
        PasswordComplexityConfig::default(),
        token_blacklist,
        KeyBuilder::new("test"),
        BruteForceProtection::in_memory("test:user".to_string()),
    );
    svc.set_password_hasher(Arc::new(TestPasswordHasher::new()));
    svc
}

#[derive(Debug)]
struct FakeDynamicProvider {
    instance_id: String,
}

impl FakeDynamicProvider {
    fn new(instance_id: impl Into<String>) -> Self {
        Self {
            instance_id: instance_id.into(),
        }
    }

    fn is_bound_instance(&self) -> bool {
        self.instance_id != "fake_dynamic_default"
    }

    fn folder_cursor(&self) -> &'static str {
        if self.is_bound_instance() {
            "bound-season-1"
        } else {
            "season-1"
        }
    }

    fn first_item_path(&self) -> &'static str {
        if self.is_bound_instance() {
            "bound-season-1/bound-episode-1.mp4"
        } else {
            "season-1/episode-1.mp4"
        }
    }

    fn playback_target_path(&self) -> &'static str {
        if self.is_bound_instance() {
            "/bound-episode-1.mp4"
        } else {
            "/episode-1.mp4"
        }
    }

    fn item(&self, path: &str) -> NextPlayItem {
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
        "fake_dynamic"
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

        Ok(PlaybackResult {
            playback_infos: infos,
            default_mode: "direct".to_string(),
            metadata: std::collections::HashMap::new(),
        })
    }

    fn as_dynamic_folder(&self) -> Option<&dyn DynamicFolder> {
        Some(self)
    }
}

#[async_trait]
impl DynamicFolder for FakeDynamicProvider {
    async fn list_playlist(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &Playlist,
        target: Option<&[u8]>,
        _page: usize,
        _page_size: usize,
    ) -> Result<Vec<DirectoryItem>, ProviderError> {
        Ok(
            match target
                .map(decode_dynamic_target)
                .as_deref()
                .unwrap_or_default()
            {
                "" => vec![DirectoryItem {
                    name: self.folder_cursor().to_string(),
                    item_type: ItemType::Playlist,
                    target: dynamic_target(self.folder_cursor()),
                    size: None,
                    thumbnail: None,
                    modified_at: None,
                }],
                cursor if cursor == self.folder_cursor() => vec![DirectoryItem {
                    name: self
                        .first_item_path()
                        .rsplit('/')
                        .next()
                        .unwrap_or(self.first_item_path())
                        .to_string(),
                    item_type: ItemType::Media,
                    target: dynamic_target(self.first_item_path()),
                    size: None,
                    thumbnail: None,
                    modified_at: None,
                }],
                _ => Vec::new(),
            },
        )
    }

    async fn resolve_item(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &Playlist,
        target: &[u8],
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        let cursor = decode_dynamic_target(target);
        Ok(match cursor.as_str() {
            path if path == self.playback_target_path() || path == self.first_item_path() => {
                Some(self.item(&cursor))
            }
            _ => None,
        })
    }

    async fn next(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &Playlist,
        _playing_media: &synctv_core::models::Media,
        target: &[u8],
        play_mode: PlayMode,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        let cursor = decode_dynamic_target(target);
        Ok(match (cursor.as_str(), play_mode) {
            (path, PlayMode::Sequential | PlayMode::RepeatAll | PlayMode::Shuffle)
                if path == self.playback_target_path() =>
            {
                Some(self.item(if self.is_bound_instance() {
                    "/bound-episode-2.mp4"
                } else {
                    "/episode-2.mp4"
                }))
            }
            (path, PlayMode::RepeatAll)
                if path
                    == if self.is_bound_instance() {
                        "/bound-episode-2.mp4"
                    } else {
                        "/episode-2.mp4"
                    } =>
            {
                Some(self.item(self.playback_target_path()))
            }
            (path, PlayMode::RepeatOne)
                if path == self.playback_target_path()
                    || path
                        == if self.is_bound_instance() {
                            "/bound-episode-2.mp4"
                        } else {
                            "/episode-2.mp4"
                        } =>
            {
                None
            }
            _ => None,
        })
    }

    async fn browse_path(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &Playlist,
        target: Option<&[u8]>,
    ) -> Result<Vec<DynamicBrowsePathSegment>, ProviderError> {
        let Some(cursor) = target.map(decode_dynamic_target) else {
            return Ok(Vec::new());
        };

        Ok(match cursor.as_str() {
            path if path == self.folder_cursor() || path == self.first_item_path() => {
                vec![DynamicBrowsePathSegment {
                    name: self.folder_cursor().to_string(),
                    target: dynamic_target(self.folder_cursor()),
                }]
            }
            _ => Vec::new(),
        })
    }
}

async fn create_dynamic_playlist(
    pool: &sqlx::PgPool,
    room_id: &RoomId,
    owner_id: &UserId,
    provider_instance_name: &str,
) -> Playlist {
    let playlist = Playlist {
        id: PlaylistId::new(),
        room_id: room_id.clone(),
        creator_id: Some(owner_id.clone()),
        name: "Dynamic Playlist".to_string(),
        parent_id: None,
        position: 0.0,
        source_provider: Some("fake_dynamic".to_string()),
        source_config: Some(serde_json::json!({})),
        provider_instance_name: Some(provider_instance_name.to_string()),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    synctv_core::repository::PlaylistRepository::new(pool.clone())
        .create(&playlist)
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_playback_returns_dynamic_playlist_item_playback_info() {
    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));

    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        ProviderInstanceRepository::new(pool.clone()),
    )));
    let mut providers_manager = ProvidersManager::new(provider_instance_manager);
    providers_manager.register_factory(
        "fake_dynamic",
        Box::new(|instance_id, _config, _instance_manager| {
            Ok(Arc::new(FakeDynamicProvider::new(instance_id)))
        }),
    );
    let providers_manager = Arc::new(providers_manager);

    let mut room_service = RoomService::new_with_providers(
        pool.clone(),
        (*user_service).clone(),
        providers_manager.clone(),
    );
    room_service.set_password_hasher(Arc::new(TestPasswordHasher::new()));
    let room_service = Arc::new(room_service);

    providers_manager
        .create_provider(
            "fake_dynamic",
            "fake_dynamic_default",
            &serde_json::json!({}),
        )
        .await
        .unwrap();

    let owner = user_repo
        .create(&make_user("api_dynamic_owner"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "API Dynamic Playback".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playlist =
        create_dynamic_playlist(&pool, &room.id, &owner.id, "fake_dynamic_default").await;

    let client_api = ClientApiImpl::new(
        user_service,
        room_service.clone(),
        Arc::new(ConnectionManager::new(ConnectionLimits::default())),
        Arc::new(Config::default()),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        Some(providers_manager),
        None,
    );

    client_api
        .start_playback(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::StartPlaybackRequest {
                media_id: String::new(),
                playlist_id: playlist.id.as_str().to_string(),
                target: br#"{"relative_path":"/episode-1.mp4"}"#.to_vec(),
            },
        )
        .await
        .unwrap();

    let response = client_api
        .get_playback(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::GetPlaybackRequest {},
        )
        .await
        .unwrap();

    let state = response.playback_state.unwrap();
    assert_eq!(state.playing_media_id, "");
    assert_eq!(state.playing_playlist_id, playlist.id.as_str());
    let state_target: serde_json::Value = serde_json::from_slice(&state.target).unwrap();
    assert_eq!(
        state_target,
        serde_json::json!({"relative_path":"/episode-1.mp4"})
    );

    let playback_result = response.playback_result.unwrap();
    assert_eq!(playback_result.playlist_id, playlist.id.as_str());
    assert_eq!(playback_result.name, "episode-1.mp4");
    let playback_target_meta = playback_result.metadata.get("target").unwrap();
    let playback_target_value: String = serde_json::from_str(playback_target_meta).unwrap();
    assert_eq!(
        playback_target_value,
        BASE64_STANDARD.encode(br#"{"relative_path":"/episode-1.mp4"}"#)
    );
    let direct = playback_result.playback_infos.get("direct").unwrap();
    assert_eq!(direct.urls.len(), 1);
    assert_eq!(
        direct.urls[0].url,
        "https://fake_dynamic_default.example.com/episode-1.mp4"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_playlist_items_returns_current_path_for_dynamic_playlist() {
    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let user_service = Arc::new(make_user_service(pool.clone()));

    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        ProviderInstanceRepository::new(pool.clone()),
    )));
    let mut providers_manager = ProvidersManager::new(provider_instance_manager);
    providers_manager.register_factory(
        "fake_dynamic",
        Box::new(|instance_id, _config, _instance_manager| {
            Ok(Arc::new(FakeDynamicProvider::new(instance_id)))
        }),
    );
    let providers_manager = Arc::new(providers_manager);

    let mut room_service = RoomService::new_with_providers(
        pool.clone(),
        (*user_service).clone(),
        providers_manager.clone(),
    );
    room_service.set_password_hasher(Arc::new(TestPasswordHasher::new()));
    let room_service = Arc::new(room_service);

    providers_manager
        .create_provider(
            "fake_dynamic",
            "fake_dynamic_default",
            &serde_json::json!({}),
        )
        .await
        .unwrap();

    let owner = user_repo
        .create(&make_user("api_dynamic_path_owner"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "API Dynamic Path".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playlist =
        create_dynamic_playlist(&pool, &room.id, &owner.id, "fake_dynamic_default").await;

    let client_api = ClientApiImpl::new(
        user_service,
        room_service.clone(),
        Arc::new(ConnectionManager::new(ConnectionLimits::default())),
        Arc::new(Config::default()),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        Some(providers_manager),
        None,
    );

    let response = client_api
        .list_playlist_items(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::ListPlaylistItemsRequest {
                playlist_id: playlist.id.as_str().to_string(),
                target: br#"{"relative_path":"season-1"}"#.to_vec(),
                page: 1,
                page_size: 50,
                search: String::new(),
                source_provider: String::new(),
                provider_instance_name: String::new(),
                sort_by: synctv_api::proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_api::proto::client::SortDirection::Asc as i32,
                availability: synctv_api::proto::client::ResourceAvailabilityFilter::All as i32,
            },
        )
        .await
        .unwrap();

    assert_eq!(response.dynamic_items.len(), 1);
    assert_eq!(response.dynamic_items[0].name, "episode-1.mp4");
    let item_target: serde_json::Value =
        serde_json::from_slice(&response.dynamic_items[0].target).unwrap();
    assert_eq!(
        item_target,
        serde_json::json!({"relative_path":"season-1/episode-1.mp4"})
    );

    assert_eq!(response.current_path.len(), 2);
    assert_eq!(response.current_path[0].playlist_id, playlist.id.as_str());
    assert_eq!(response.current_path[0].name, "Dynamic Playlist");
    assert!(response.current_path[0].target.is_empty());
    assert_eq!(response.current_path[1].playlist_id, "");
    assert_eq!(response.current_path[1].name, "season-1");
    let target: serde_json::Value =
        serde_json::from_slice(&response.current_path[1].target).unwrap();
    assert_eq!(target, serde_json::json!({"relative_path":"season-1"}));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_dynamic_playlist_get_playback_uses_bound_provider_instance() {
    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let user_service = Arc::new(make_user_service(pool.clone()));

    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        ProviderInstanceRepository::new(pool.clone()),
    )));
    let mut providers_manager = ProvidersManager::new(provider_instance_manager);
    providers_manager.register_factory(
        "fake_dynamic",
        Box::new(|instance_id, _config, _instance_manager| {
            Ok(Arc::new(FakeDynamicProvider::new(instance_id)))
        }),
    );
    let providers_manager = Arc::new(providers_manager);

    let mut room_service = RoomService::new_with_providers(
        pool.clone(),
        (*user_service).clone(),
        providers_manager.clone(),
    );
    room_service.set_password_hasher(Arc::new(TestPasswordHasher::new()));
    let room_service = Arc::new(room_service);

    providers_manager
        .create_provider(
            "fake_dynamic",
            "fake_dynamic_default",
            &serde_json::json!({}),
        )
        .await
        .unwrap();
    providers_manager
        .create_provider("fake_dynamic", "fake_dynamic_alt", &serde_json::json!({}))
        .await
        .unwrap();

    let owner = user_repo
        .create(&make_user("api_dynamic_bound_owner"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "API Dynamic Bound Playback".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playlist = create_dynamic_playlist(&pool, &room.id, &owner.id, "fake_dynamic_alt").await;

    let client_api = ClientApiImpl::new(
        user_service,
        room_service.clone(),
        Arc::new(ConnectionManager::new(ConnectionLimits::default())),
        Arc::new(Config::default()),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        Some(providers_manager),
        None,
    );

    client_api
        .start_playback(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::StartPlaybackRequest {
                media_id: String::new(),
                playlist_id: playlist.id.as_str().to_string(),
                target: br#"{"relative_path":"/bound-episode-1.mp4"}"#.to_vec(),
            },
        )
        .await
        .unwrap();

    let response = client_api
        .get_playback(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::GetPlaybackRequest {},
        )
        .await
        .unwrap();

    let playback_result = response.playback_result.unwrap();
    let direct = playback_result.playback_infos.get("direct").unwrap();
    assert_eq!(
        direct.urls[0].url,
        "https://fake_dynamic_alt.example.com/bound-episode-1.mp4"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_dynamic_playlist_list_items_uses_bound_provider_instance() {
    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let user_service = Arc::new(make_user_service(pool.clone()));

    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        ProviderInstanceRepository::new(pool.clone()),
    )));
    let mut providers_manager = ProvidersManager::new(provider_instance_manager);
    providers_manager.register_factory(
        "fake_dynamic",
        Box::new(|instance_id, _config, _instance_manager| {
            Ok(Arc::new(FakeDynamicProvider::new(instance_id)))
        }),
    );
    let providers_manager = Arc::new(providers_manager);

    let mut room_service = RoomService::new_with_providers(
        pool.clone(),
        (*user_service).clone(),
        providers_manager.clone(),
    );
    room_service.set_password_hasher(Arc::new(TestPasswordHasher::new()));
    let room_service = Arc::new(room_service);

    providers_manager
        .create_provider(
            "fake_dynamic",
            "fake_dynamic_default",
            &serde_json::json!({}),
        )
        .await
        .unwrap();
    providers_manager
        .create_provider("fake_dynamic", "fake_dynamic_alt", &serde_json::json!({}))
        .await
        .unwrap();

    let owner = user_repo
        .create(&make_user("api_dynamic_bound_list_owner"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "API Dynamic Bound Browse".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playlist = create_dynamic_playlist(&pool, &room.id, &owner.id, "fake_dynamic_alt").await;

    let client_api = ClientApiImpl::new(
        user_service,
        room_service.clone(),
        Arc::new(ConnectionManager::new(ConnectionLimits::default())),
        Arc::new(Config::default()),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        Some(providers_manager),
        None,
    );

    let response = client_api
        .list_playlist_items(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::ListPlaylistItemsRequest {
                playlist_id: playlist.id.as_str().to_string(),
                target: Vec::new(),
                page: 1,
                page_size: 50,
                search: String::new(),
                source_provider: String::new(),
                provider_instance_name: String::new(),
                sort_by: synctv_api::proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_api::proto::client::SortDirection::Asc as i32,
                availability: synctv_api::proto::client::ResourceAvailabilityFilter::All as i32,
            },
        )
        .await
        .unwrap();

    assert_eq!(response.dynamic_items.len(), 1);
    assert_eq!(response.dynamic_items[0].name, "bound-season-1");
    let item_target: serde_json::Value =
        serde_json::from_slice(&response.dynamic_items[0].target).unwrap();
    assert_eq!(
        item_target,
        serde_json::json!({"relative_path":"bound-season-1"})
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_playlist_items_allows_room_root_with_empty_playlist_id() {
    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let user_service = Arc::new(make_user_service(pool.clone()));

    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        ProviderInstanceRepository::new(pool.clone()),
    )));
    let providers_manager = Arc::new(ProvidersManager::new(provider_instance_manager));

    let mut room_service =
        RoomService::new_with_providers(pool.clone(), (*user_service).clone(), providers_manager);
    room_service.set_password_hasher(Arc::new(TestPasswordHasher::new()));
    let room_service = Arc::new(room_service);

    let owner = user_repo
        .create(&make_user("api_root_items_owner"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "API Root Items".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let root_media = synctv_core::models::Media::from_direct_single_mode(
        None,
        room.id.clone(),
        Some(owner.id.clone()),
        "Root Media".to_string(),
        "direct",
        synctv_core::models::PlaybackInfo {
            urls: vec![synctv_core::models::PlaybackUrl::simple(
                String::new(),
                "https://example.com/root.mp4".to_string(),
            )],
            default_url_index: 0,
            subtitles: Vec::new(),
            default_subtitle_index: None,
            danmakus: Vec::new(),
            format: "mp4".to_string(),
        },
        0.0,
    );
    synctv_core::repository::MediaRepository::new(pool.clone())
        .create(&root_media)
        .await
        .unwrap();

    let client_api = ClientApiImpl::new(
        user_service,
        room_service,
        Arc::new(ConnectionManager::new(ConnectionLimits::default())),
        Arc::new(Config::default()),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
    );

    let response = client_api
        .list_playlist_items(
            owner.id.as_str(),
            room.id.as_str(),
            synctv_api::proto::client::ListPlaylistItemsRequest {
                playlist_id: String::new(),
                target: Vec::new(),
                page: 1,
                page_size: 50,
                search: String::new(),
                source_provider: String::new(),
                provider_instance_name: String::new(),
                sort_by: synctv_api::proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_api::proto::client::SortDirection::Asc as i32,
                availability: synctv_api::proto::client::ResourceAvailabilityFilter::All as i32,
            },
        )
        .await
        .unwrap();

    assert_eq!(response.playlists.len(), 0);
    assert_eq!(response.media.len(), 1);
    assert_eq!(response.media[0].title, "Root Media");
    assert!(response.dynamic_items.is_empty());
    assert!(response.current_path.is_empty());
}
