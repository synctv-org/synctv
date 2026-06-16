#![allow(clippy::unwrap_used)]

mod support;

use std::sync::Arc;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::Utc;
use synctv_api::impls::ClientApiImpl;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    models::{
        FromProviderParams, Media, PlayMode, Playlist, PlaylistId, ProviderInstance, RoomId,
        SignupMethod, User, UserId, UserRole, UserStatus,
    },
    provider::{
        DirectoryItem, DynamicBrowsePathSegment, DynamicFolder, DynamicListQuery, ItemType,
        MediaProvider, NextPlayItem, PlaybackInfo, PlaybackResult, ProviderContext, ProviderError,
    },
    proxy_signature::ProxySigningKey,
    repository::{MediaRepository, ProviderInstanceRepository, UserRepository},
    service::{
        auth::{BruteForceProtection, JwtService},
        room::RoomServiceOptions,
        InMemoryTokenBlacklistStore, ProvidersManager, RemoteProviderManager, RoomService,
        UserService,
    },
    Config,
};
use synctv_core_testing::create_test_pool;
use synctv_realtime::sync::{ConnectionLimits, ConnectionManager};

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

const NEXT_ITEM_SOURCE_CONFIG_SECRET: &str = "dynamic-next-item-secret";

fn public_id_codec() -> synctv_core::PublicIdCodec {
    synctv_core::PublicIdCodec::plain()
}

fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
        signup_method: SignupMethod::Email,
        created_at: now,
        updated_at: now,
        version: 0,
        deleted_at: None,
    }
}

fn make_user_service(pool: &sqlx::PgPool) -> UserService {
    let jwt_service = JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap();
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));

    UserService::new_for_tests(
        pool,
        jwt_service,
        username_cache,
        token_blacklist,
        KeyBuilder::new("test"),
        BruteForceProtection::in_memory("test:user".to_string()),
    )
}

fn make_test_alist_providers_manager(pool: &sqlx::PgPool) -> Arc<ProvidersManager> {
    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        ProviderInstanceRepository::new(pool.clone()),
    )));
    let mut providers_manager =
        ProvidersManager::new(provider_instance_manager).expect("providers manager should build");
    providers_manager.register_factory(
        "alist",
        Box::new(|instance_id, _config, _instance_manager| {
            Ok(Arc::new(StubDynamicProvider::new(instance_id)))
        }),
    );
    Arc::new(providers_manager)
}

fn make_room_service_with_provider_credentials(
    pool: &sqlx::PgPool,
    user_service: &UserService,
    providers_manager: Arc<ProvidersManager>,
) -> RoomService {
    let credential_encryption = synctv_core::credential_encryption::CredentialEncryption::new(
        b"0123456789abcdef0123456789abcdef",
    )
    .unwrap();
    let credential_repo = Arc::new(
        synctv_core::repository::UserProviderCredentialRepository::new_with_encryption(
            pool.clone(),
            credential_encryption.clone(),
        ),
    );
    RoomService::new_with_providers_and_options(
        pool.clone(),
        user_service.clone(),
        providers_manager,
        RoomServiceOptions {
            credential_encryption: Some(credential_encryption),
            credential_repo: Some(credential_repo),
            ..RoomServiceOptions::test_defaults_with_settings(pool.clone())
        },
    )
    .expect("room service should build")
}

#[derive(Debug)]
struct StubDynamicProvider {
    instance_id: String,
}

impl StubDynamicProvider {
    fn new(instance_id: impl Into<String>) -> Self {
        Self {
            instance_id: instance_id.into(),
        }
    }

    fn is_bound_instance(&self) -> bool {
        self.instance_id != "alist_default"
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

    fn item(path: &str) -> NextPlayItem {
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        NextPlayItem {
            name,
            item_type: ItemType::Media,
            source_config: serde_json::json!({
                "path": path,
                "secret_token": NEXT_ITEM_SOURCE_CONFIG_SECRET,
            }),
            metadata: serde_json::json!({}),
            provider_data: serde_json::json!({}),
            target: dynamic_target(path),
        }
    }
}

#[async_trait]
impl MediaProvider for StubDynamicProvider {
    fn name(&self) -> &'static str {
        "alist"
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
            duration_seconds: None,
            metadata: std::collections::HashMap::new(),
        })
    }

    fn as_dynamic_folder(&self) -> Option<&dyn DynamicFolder> {
        Some(self)
    }
}

#[async_trait]
impl DynamicFolder for StubDynamicProvider {
    async fn list_playlist(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &Playlist,
        target: Option<&[u8]>,
        _query: DynamicListQuery,
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
                    description: None,
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
                    description: None,
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
                Some(Self::item(&cursor))
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
                Some(Self::item(if self.is_bound_instance() {
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
                Some(Self::item(self.playback_target_path()))
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
    let now = Utc::now();
    let instance = ProviderInstance {
        name: provider_instance_name.to_string(),
        endpoint: "http://localhost:50051".to_string(),
        comment: Some("test provider instance".to_string()),
        jwt_secret: None,
        custom_ca: None,
        timeout: "10s".to_string(),
        tls: false,
        insecure_tls: false,
        providers: vec!["alist".to_string()],
        enabled: true,
        created_at: now,
        updated_at: now,
    };
    ProviderInstanceRepository::new(pool.clone())
        .create(&instance)
        .await
        .unwrap();

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
        .unwrap()
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_playback_returns_dynamic_playlist_item_playback_info() {
    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(&pool));

    let providers_manager = make_test_alist_providers_manager(&pool);
    let room_service = make_room_service_with_provider_credentials(
        &pool,
        &user_service,
        providers_manager.clone(),
    );
    let room_service = Arc::new(room_service);

    providers_manager
        .create_provider("alist", "alist_default", &serde_json::json!({}))
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
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();
    let playlist = create_dynamic_playlist(&pool, &room.id, &owner.id, "alist_default").await;
    let codec = public_id_codec();
    let room_public_id = codec.encode_room_id(room.id).unwrap();
    let playlist_public_id = codec.encode_playlist_id(playlist.id).unwrap();

    let client_api = ClientApiImpl::new_with_runtime(
        synctv_api::impls::ClientApiConfig {
            read_pool: None,
            user_service,
            room_service: room_service.clone(),
            connection_service: Arc::new(ConnectionManager::new(ConnectionLimits::default())),
            config: Arc::new(Config::default()),
            publish_key_service: None,
            jwt_service: JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
            live_streaming_infrastructure: None,
            settings_registry: None,
            public_id_codec: Arc::new(synctv_core::PublicIdCodec::plain()),
            chat_service: None,
            provider_stores: Arc::new(synctv_core::provider::ProviderStoreRegistry::local_only(
                "test:provider:",
            )),
            email_api: None,
            passkey_service: None,
        },
        support::client_api_runtime(),
    );

    client_api
        .start_playback(
            &owner.id,
            &room_public_id,
            synctv_proto::client::StartPlaybackRequest {
                media_id: String::new(),
                playlist_id: playlist_public_id.clone(),
                target: br#"{"relative_path":"/episode-1.mp4"}"#.to_vec(),
            },
        )
        .await
        .unwrap();

    let response = client_api
        .get_playback(
            &owner.id,
            &room_public_id,
            synctv_proto::client::GetPlaybackRequest {
                playback_client_profile: None,
            },
        )
        .await
        .unwrap();

    let response_json = serde_json::to_string(&response).unwrap();
    assert!(
        !response_json.contains(NEXT_ITEM_SOURCE_CONFIG_SECRET),
        "dynamic NextPlayItem source_config must not be exposed in client playback responses"
    );

    let state = response.playback_state.unwrap();
    assert_eq!(state.playing_media_id, "");
    assert_eq!(state.playing_playlist_id, playlist_public_id);
    let state_target: serde_json::Value = serde_json::from_slice(&state.target).unwrap();
    assert_eq!(
        state_target,
        serde_json::json!({"relative_path":"/episode-1.mp4"})
    );

    let playback = response.playback.unwrap();
    assert_eq!(playback.playlist_id, playlist_public_id);
    assert_eq!(playback.name, "episode-1.mp4");
    let playback_target_meta = playback.metadata.get("target").unwrap();
    let playback_target_value: String = serde_json::from_str(playback_target_meta).unwrap();
    assert_eq!(
        playback_target_value,
        BASE64_STANDARD.encode(br#"{"relative_path":"/episode-1.mp4"}"#)
    );
    let direct = playback.playback_infos.get("direct").unwrap();
    assert_eq!(direct.urls.len(), 1);
    assert_eq!(
        direct.urls[0].url,
        "https://alist_default.example.com/episode-1.mp4"
    );

    let update_response = client_api
        .update_playback_state(
            &owner.id,
            &room_public_id,
            synctv_proto::client::UpdatePlaybackStateRequest {
                r#type: synctv_proto::client::PlaybackUpdateType::Seek as i32,
                playing: None,
                position: Some(12.5),
                speed: None,
                version: Some(state.version),
                expected_media_id: Some(state.playing_media_id.clone()),
                expected_playlist_id: Some(state.playing_playlist_id.clone()),
                expected_target_hash: Some(state.target_hash.clone()),
            },
        )
        .await
        .unwrap();
    let update_state = update_response
        .playback_state
        .expect("update response should include playback state");
    assert_eq!(update_state.playing_playlist_id, playlist_public_id);
    assert!(
        update_state.position >= 12.5,
        "playing update response should not rewind below the requested seek position"
    );
    assert!(
        update_state.position < 17.5,
        "playing update response should not jump far beyond the requested seek position"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_playlist_items_returns_current_path_for_dynamic_playlist() {
    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let user_service = Arc::new(make_user_service(&pool));

    let providers_manager = make_test_alist_providers_manager(&pool);
    let room_service = make_room_service_with_provider_credentials(
        &pool,
        &user_service,
        providers_manager.clone(),
    );
    let room_service = Arc::new(room_service);

    providers_manager
        .create_provider("alist", "alist_default", &serde_json::json!({}))
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
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let playlist = create_dynamic_playlist(&pool, &room.id, &owner.id, "alist_default").await;
    let codec = public_id_codec();
    let room_public_id = codec.encode_room_id(room.id).unwrap();
    let playlist_public_id = codec.encode_playlist_id(playlist.id).unwrap();

    let client_api = ClientApiImpl::new_with_runtime(
        synctv_api::impls::ClientApiConfig {
            read_pool: None,
            user_service,
            room_service: room_service.clone(),
            connection_service: Arc::new(ConnectionManager::new(ConnectionLimits::default())),
            config: Arc::new(Config::default()),
            publish_key_service: None,
            jwt_service: JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
            live_streaming_infrastructure: None,
            settings_registry: None,
            public_id_codec: Arc::new(synctv_core::PublicIdCodec::plain()),
            chat_service: None,
            provider_stores: Arc::new(synctv_core::provider::ProviderStoreRegistry::local_only(
                "test:provider:",
            )),
            email_api: None,
            passkey_service: None,
        },
        support::client_api_runtime(),
    );

    let response = client_api
        .list_playlist_items(
            &owner.id,
            &room_public_id,
            synctv_proto::client::ListPlaylistItemsRequest {
                playlist_id: playlist_public_id.clone(),
                target: br#"{"relative_path":"season-1"}"#.to_vec(),
                page: 1,
                page_size: 50,
                search: String::new(),
                source_provider: String::new(),
                provider_instance_name: String::new(),
                sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
                refresh: false,
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
    assert_eq!(response.current_path[0].playlist_id, playlist_public_id);
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
    let user_service = Arc::new(make_user_service(&pool));

    let providers_manager = make_test_alist_providers_manager(&pool);
    let room_service = make_room_service_with_provider_credentials(
        &pool,
        &user_service,
        providers_manager.clone(),
    );
    let room_service = Arc::new(room_service);

    providers_manager
        .create_provider("alist", "alist_default", &serde_json::json!({}))
        .await
        .unwrap();
    providers_manager
        .create_provider("alist", "alist_alt", &serde_json::json!({}))
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
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let playlist = create_dynamic_playlist(&pool, &room.id, &owner.id, "alist_alt").await;
    let codec = public_id_codec();
    let room_public_id = codec.encode_room_id(room.id).unwrap();
    let playlist_public_id = codec.encode_playlist_id(playlist.id).unwrap();

    let client_api = ClientApiImpl::new_with_runtime(
        synctv_api::impls::ClientApiConfig {
            read_pool: None,
            user_service,
            room_service: room_service.clone(),
            connection_service: Arc::new(ConnectionManager::new(ConnectionLimits::default())),
            config: Arc::new(Config::default()),
            publish_key_service: None,
            jwt_service: JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
            live_streaming_infrastructure: None,
            settings_registry: None,
            public_id_codec: Arc::new(synctv_core::PublicIdCodec::plain()),
            chat_service: None,
            provider_stores: Arc::new(synctv_core::provider::ProviderStoreRegistry::local_only(
                "test:provider:",
            )),
            email_api: None,
            passkey_service: None,
        },
        support::client_api_runtime(),
    );

    client_api
        .start_playback(
            &owner.id,
            &room_public_id,
            synctv_proto::client::StartPlaybackRequest {
                media_id: String::new(),
                playlist_id: playlist_public_id,
                target: br#"{"relative_path":"/bound-episode-1.mp4"}"#.to_vec(),
            },
        )
        .await
        .unwrap();

    let response = client_api
        .get_playback(
            &owner.id,
            &room_public_id,
            synctv_proto::client::GetPlaybackRequest {
                playback_client_profile: None,
            },
        )
        .await
        .unwrap();

    let playback = response.playback.unwrap();
    let direct = playback.playback_infos.get("direct").unwrap();
    assert_eq!(
        direct.urls[0].url,
        "https://alist_alt.example.com/bound-episode-1.mp4"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_static_provider_playback_with_signing_key_uses_provider_store_registry() {
    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());
    let user_service = Arc::new(make_user_service(&pool));

    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        ProviderInstanceRepository::new(pool.clone()),
    )));
    let providers_manager = Arc::new(
        ProvidersManager::new(provider_instance_manager).expect("providers manager should build"),
    );
    providers_manager
        .create_provider("direct_url", "direct_url", &serde_json::json!({}))
        .await
        .unwrap();

    let room_service = RoomService::new_with_providers_for_tests(
        pool.clone(),
        (*user_service).clone(),
        providers_manager.clone(),
    )
    .expect("room service should build");
    let room_service = Arc::new(room_service);

    let owner = user_repo
        .create(&make_user("api_signed_provider_owner"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "API Signed Provider Playback".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let media = Media::from_provider_with_params(FromProviderParams {
        playlist_id: None,
        room_id: room.id,
        creator_id: Some(owner.id),
        name: "Signed Provider Media".to_string(),
        description: String::new(),
        source_config: serde_json::json!({
            "url": "https://example.com/video.mp4",
            "headers": {
                "Authorization": "Bearer provider-token"
            }
        }),
        provider_name: "direct_url".to_string(),
        provider_instance_name: None,
        position: 0.0,
    });
    let media = media_repo.create(&media).await.unwrap();
    let codec = public_id_codec();
    let room_public_id = codec.encode_room_id(room.id).unwrap();
    let media_public_id = codec.encode_media_id(media.id).unwrap();

    let provider_stores = Arc::new(synctv_core::provider::ProviderStoreRegistry::local_only(
        "test:provider:".to_string(),
    ));
    let client_api = ClientApiImpl::new_with_runtime(
        synctv_api::impls::ClientApiConfig {
            read_pool: None,
            user_service,
            room_service,
            connection_service: Arc::new(ConnectionManager::new(ConnectionLimits::default())),
            config: Arc::new(Config::default()),
            publish_key_service: None,
            jwt_service: JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
            live_streaming_infrastructure: None,
            settings_registry: None,
            public_id_codec: Arc::new(synctv_core::PublicIdCodec::plain()),
            chat_service: None,
            provider_stores,
            email_api: None,
            passkey_service: None,
        },
        synctv_api::impls::ClientApiRuntime {
            signing_key: Arc::new(
                ProxySigningKey::try_derive_from(b"Test_Secret_Key_For_JWT_Tokens_32Bytes!!")
                    .expect("test proxy signing key should derive"),
            ),
            ..support::client_api_runtime()
        },
    );

    client_api
        .start_playback(
            &owner.id,
            &room_public_id,
            synctv_proto::client::StartPlaybackRequest {
                media_id: media_public_id.clone(),
                playlist_id: String::new(),
                target: Vec::new(),
            },
        )
        .await
        .unwrap();

    let response = client_api
        .get_playback(
            &owner.id,
            &room_public_id,
            synctv_proto::client::GetPlaybackRequest {
                playback_client_profile: None,
            },
        )
        .await
        .unwrap();

    let playback = response.playback.unwrap();
    let direct = playback.playback_infos.get("direct").unwrap();
    assert_eq!(direct.urls.len(), 1);
    assert!(
        direct.urls[0]
            .url
            .starts_with("/api/providers/proxy/direct_url/"),
        "signed provider playback should expose proxy URL, got {}",
        direct.urls[0].url
    );
    assert!(
        direct.urls[0].url.contains("/stream?"),
        "signed direct-url playback should use stream proxy contract, got {}",
        direct.urls[0].url
    );
    assert!(
        direct.urls[0].headers.is_empty(),
        "proxy-backed playback should not require client-side secret headers"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_playback_without_active_media_returns_idle_playback_info() {
    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let user_service = Arc::new(make_user_service(&pool));

    let room_service = RoomService::new_for_tests(pool.clone(), (*user_service).clone())
        .expect("room service should build");
    let room_service = Arc::new(room_service);

    let owner = user_repo
        .create(&make_user("api_idle_playback_owner"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "API Idle Playback".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();
    let room_public_id = public_id_codec().encode_room_id(room.id).unwrap();

    let client_api = ClientApiImpl::new_with_runtime(
        synctv_api::impls::ClientApiConfig {
            read_pool: None,
            user_service,
            room_service,
            connection_service: Arc::new(ConnectionManager::new(ConnectionLimits::default())),
            config: Arc::new(Config::default()),
            publish_key_service: None,
            jwt_service: JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
            live_streaming_infrastructure: None,
            settings_registry: None,
            public_id_codec: Arc::new(synctv_core::PublicIdCodec::plain()),
            chat_service: None,
            provider_stores: Arc::new(synctv_core::provider::ProviderStoreRegistry::local_only(
                "test:provider:",
            )),
            email_api: None,
            passkey_service: None,
        },
        support::client_api_runtime(),
    );

    let response = client_api
        .get_playback(
            &owner.id,
            &room_public_id,
            synctv_proto::client::GetPlaybackRequest {
                playback_client_profile: None,
            },
        )
        .await
        .unwrap();

    let _playback_state = response
        .playback_state
        .expect("idle room should still expose playback state");
    let playback = response
        .playback
        .expect("idle room should still expose playback");

    assert_eq!(playback.room_id, room_public_id);
    assert_eq!(playback.media_id, "");
    assert_eq!(playback.playlist_id, "");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_playback_returns_state_when_playback_info_generation_fails() {
    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());
    let user_service = Arc::new(make_user_service(&pool));

    let room_service = RoomService::new_for_tests(pool.clone(), (*user_service).clone())
        .expect("room service should build");
    let room_service = Arc::new(room_service);

    let owner = user_repo
        .create(&make_user("api_playback_state_only"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "API Playback State Only".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let media = Media::from_provider_with_params(FromProviderParams {
        playlist_id: None,
        room_id: room.id,
        creator_id: Some(owner.id),
        name: "Broken Playback Provider".to_string(),
        description: String::new(),
        source_config: serde_json::json!({ "opaque": true }),
        provider_name: "live_proxy".to_string(),
        provider_instance_name: None,
        position: 0.0,
    });
    let media = media_repo.create(&media).await.unwrap();
    let codec = public_id_codec();
    let room_public_id = codec.encode_room_id(room.id).unwrap();
    let media_public_id = codec.encode_media_id(media.id).unwrap();

    let client_api = ClientApiImpl::new_with_runtime(
        synctv_api::impls::ClientApiConfig {
            read_pool: None,
            user_service,
            room_service: room_service.clone(),
            connection_service: Arc::new(ConnectionManager::new(ConnectionLimits::default())),
            config: Arc::new(Config::default()),
            publish_key_service: None,
            jwt_service: JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
            live_streaming_infrastructure: None,
            settings_registry: None,
            public_id_codec: Arc::new(synctv_core::PublicIdCodec::plain()),
            chat_service: None,
            provider_stores: Arc::new(synctv_core::provider::ProviderStoreRegistry::local_only(
                "test:provider:",
            )),
            email_api: None,
            passkey_service: None,
        },
        support::client_api_runtime(),
    );

    client_api
        .start_playback(
            &owner.id,
            &room_public_id,
            synctv_proto::client::StartPlaybackRequest {
                media_id: media_public_id.clone(),
                playlist_id: String::new(),
                target: Vec::new(),
            },
        )
        .await
        .unwrap();

    let response = client_api
        .get_playback(
            &owner.id,
            &room_public_id,
            synctv_proto::client::GetPlaybackRequest {
                playback_client_profile: None,
            },
        )
        .await
        .unwrap();

    let playback_state = response
        .playback_state
        .expect("playback state should still be returned when playback info generation fails");
    assert_eq!(playback_state.playing_media_id, media_public_id);
    assert!(playback_state.is_playing);
    assert!(
        response.playback.is_none(),
        "playback info failures should degrade to state-only responses"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_dynamic_playlist_list_items_uses_bound_provider_instance() {
    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let user_service = Arc::new(make_user_service(&pool));

    let providers_manager = make_test_alist_providers_manager(&pool);
    let room_service = make_room_service_with_provider_credentials(
        &pool,
        &user_service,
        providers_manager.clone(),
    );
    let room_service = Arc::new(room_service);

    providers_manager
        .create_provider("alist", "alist_default", &serde_json::json!({}))
        .await
        .unwrap();
    providers_manager
        .create_provider("alist", "alist_alt", &serde_json::json!({}))
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
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let playlist = create_dynamic_playlist(&pool, &room.id, &owner.id, "alist_alt").await;
    let codec = public_id_codec();
    let room_public_id = codec.encode_room_id(room.id).unwrap();
    let playlist_public_id = codec.encode_playlist_id(playlist.id).unwrap();

    let client_api = ClientApiImpl::new_with_runtime(
        synctv_api::impls::ClientApiConfig {
            read_pool: None,
            user_service,
            room_service: room_service.clone(),
            connection_service: Arc::new(ConnectionManager::new(ConnectionLimits::default())),
            config: Arc::new(Config::default()),
            publish_key_service: None,
            jwt_service: JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
            live_streaming_infrastructure: None,
            settings_registry: None,
            public_id_codec: Arc::new(synctv_core::PublicIdCodec::plain()),
            chat_service: None,
            provider_stores: Arc::new(synctv_core::provider::ProviderStoreRegistry::local_only(
                "test:provider:",
            )),
            email_api: None,
            passkey_service: None,
        },
        support::client_api_runtime(),
    );

    let response = client_api
        .list_playlist_items(
            &owner.id,
            &room_public_id,
            synctv_proto::client::ListPlaylistItemsRequest {
                playlist_id: playlist_public_id,
                target: Vec::new(),
                page: 1,
                page_size: 50,
                search: String::new(),
                source_provider: String::new(),
                provider_instance_name: String::new(),
                sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
                refresh: false,
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
    let user_service = Arc::new(make_user_service(&pool));

    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        ProviderInstanceRepository::new(pool.clone()),
    )));
    let providers_manager = Arc::new(
        ProvidersManager::new(provider_instance_manager).expect("providers manager should build"),
    );

    let room_service = RoomService::new_with_providers_for_tests(
        pool.clone(),
        (*user_service).clone(),
        providers_manager,
    )
    .expect("room service should build");
    let room_service = Arc::new(room_service);

    let owner = user_repo
        .create(&make_user("api_root_items_owner"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "API Root Items".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let root_media = synctv_core::models::Media::from_direct_single_mode(
        None,
        room.id,
        Some(owner.id),
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
    )
    .expect("direct media should build");
    synctv_core::repository::MediaRepository::new(pool.clone())
        .create(&root_media)
        .await
        .unwrap();

    let client_api = ClientApiImpl::new_with_runtime(
        synctv_api::impls::ClientApiConfig {
            read_pool: None,
            user_service,
            room_service,
            connection_service: Arc::new(ConnectionManager::new(ConnectionLimits::default())),
            config: Arc::new(Config::default()),
            publish_key_service: None,
            jwt_service: JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
            live_streaming_infrastructure: None,
            settings_registry: None,
            public_id_codec: Arc::new(synctv_core::PublicIdCodec::plain()),
            chat_service: None,
            provider_stores: Arc::new(synctv_core::provider::ProviderStoreRegistry::local_only(
                "test:provider:",
            )),
            email_api: None,
            passkey_service: None,
        },
        support::client_api_runtime(),
    );
    let room_public_id = public_id_codec().encode_room_id(room.id).unwrap();

    let response = client_api
        .list_playlist_items(
            &owner.id,
            &room_public_id,
            synctv_proto::client::ListPlaylistItemsRequest {
                playlist_id: String::new(),
                target: Vec::new(),
                page: 1,
                page_size: 50,
                search: String::new(),
                source_provider: String::new(),
                provider_instance_name: String::new(),
                sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
                refresh: false,
            },
        )
        .await
        .unwrap();

    assert_eq!(response.playlists.len(), 0);
    assert_eq!(response.media.len(), 1);
    assert_eq!(response.media[0].name, "Root Media");
    assert!(response.dynamic_items.is_empty());
    assert!(response.current_path.is_empty());
}
