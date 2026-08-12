#![allow(clippy::unwrap_used)]

mod support;

use synctv_api::ApiRuntimeSettings as Config;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use synctv_api::{ClientApiImpl, ProxySigningKey};
use synctv_core::models::media::{AlistPlaybackLocator, AlistPlaybackMediaLocator};
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    models::{
        AlistMediaSourceConfig, FromProviderParams, Media, MediaSourceConfig, PlayMode,
        PlaybackAlistMedia, Playlist, PlaylistId, ProviderInstance, ProviderTarget, RoomId,
        SignupMethod, SourceProvider, User, UserId, UserRole, UserStatus,
    },
    provider::{
        DynamicBrowsePathSegment, DynamicListQuery, DynamicListResult, DynamicPlaylistItem,
        DynamicPlaylistProvider, ItemType, MediaProvider, NextPlayItem, PlaybackInfo,
        PlaybackResult, ProviderContext, ProviderError,
    },
    repository::{MediaRepository, ProviderInstanceRepository, UserRepository},
    service::{
        BruteForceProtection, InMemoryTokenBlacklistStore, JwtService, ProvidersManager,
        RemoteProviderManager, RoomService, RoomServiceOptions, UserService,
    },
};
use synctv_core_testing::create_test_pool;
use synctv_realtime::sync::{ConnectionLimits, ConnectionManager};

fn alist_target(cursor: &str) -> ProviderTarget {
    ProviderTarget::alist(cursor.to_string())
}

fn proto_alist_target(relative_path: &str) -> Option<synctv_proto::client::ProviderTarget> {
    Some(synctv_proto::client::ProviderTarget {
        target: Some(synctv_proto::client::provider_target::Target::Alist(
            synctv_proto::client::AlistTarget {
                relative_path: relative_path.to_string(),
            },
        )),
    })
}

fn assert_proto_alist_target(
    target: Option<&synctv_proto::client::ProviderTarget>,
    expected_path: &str,
) {
    let Some(synctv_proto::client::ProviderTarget {
        target: Some(synctv_proto::client::provider_target::Target::Alist(target)),
    }) = target
    else {
        panic!("expected alist provider target");
    };
    assert_eq!(target.relative_path, expected_path);
}

fn decode_alist_target(target: &ProviderTarget) -> String {
    match target {
        ProviderTarget::Alist(target) => target.relative_path.clone(),
        _ => panic!("expected alist provider target"),
    }
}

const NEXT_ITEM_SOURCE_CONFIG_SECRET: &str = "dynamic-next-item-secret";

fn public_id_codec() -> synctv_api::PublicIdCodec {
    synctv_api::PublicIdCodec::plain()
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

#[derive(Debug)]
struct TransientPlaybackFailureProvider;

#[async_trait]
impl MediaProvider for TransientPlaybackFailureProvider {
    fn name(&self) -> &'static str {
        "direct_url"
    }

    async fn generate_playback(
        &self,
        _ctx: &ProviderContext<'_>,
        _source_config: &MediaSourceConfig,
    ) -> Result<PlaybackResult, ProviderError> {
        Err(ProviderError::NetworkError(
            "test provider temporarily unavailable".to_string(),
        ))
    }
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
            source_config: MediaSourceConfig::Alist(AlistMediaSourceConfig {
                server_id: NEXT_ITEM_SOURCE_CONFIG_SECRET.to_string(),
                path: path.to_string(),
                password: None,
                proxy_mode: synctv_core::models::PlaybackProxyMode::Auto,
            }),
            target: alist_target(path),
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
        source_config: &MediaSourceConfig,
    ) -> Result<PlaybackResult, ProviderError> {
        let MediaSourceConfig::Alist(source_config) = source_config else {
            return Err(ProviderError::InvalidConfig(
                "Missing Alist source_config".to_string(),
            ));
        };
        if source_config.server_id != NEXT_ITEM_SOURCE_CONFIG_SECRET {
            return Err(ProviderError::InvalidConfig(
                "Unexpected dynamic item server_id".to_string(),
            ));
        }
        let path = source_config.path.as_str();

        let direct_url = format!("https://{}.example.com{path}", self.instance_id);
        let mut infos = std::collections::HashMap::new();
        infos.insert("direct".to_string(), provider_playback_info(&direct_url));
        infos.insert(
            "proxy_direct".to_string(),
            provider_playback_info(&direct_url),
        );

        Ok(PlaybackResult {
            playback_infos: infos,
            default_mode: "direct".to_string(),
            provider: SourceProvider::Alist,
            provider_instance_name: Some(self.instance_id.clone()),
            duration_seconds: None,
            playback_kind: Some(synctv_core::models::PlaybackKind::Regular),
            metadata: None,
        })
    }

    fn as_dynamic_playlist_provider(&self) -> Option<&dyn DynamicPlaylistProvider> {
        Some(self)
    }
}

fn provider_playback_info(url: &str) -> PlaybackInfo {
    PlaybackInfo {
        thumbnail: None,
        medias: vec![synctv_core::models::PlaybackMedia {
            name: String::new(),
            format: "mp4".to_string(),
            expire_at: None,
            metadata: None,
            p2p_swarm_id: None,
            provider: synctv_core::models::PlaybackMediaProvider::Alist(
                PlaybackAlistMedia::Direct {
                    url: url.to_string(),
                    headers: std::collections::HashMap::new(),
                    locator: AlistPlaybackLocator {
                        server_id: NEXT_ITEM_SOURCE_CONFIG_SECRET.to_string(),
                        path: url.to_string(),
                        password: None,
                        credential_owner_id: UserId::new(),
                        credential_revision: "test".to_string(),
                        provider_instance_name: None,
                    },
                    resource: AlistPlaybackMediaLocator::File,
                },
            ),
        }],
        default_media_index: None,
        subtitles: Vec::new(),
        default_subtitle_index: None,
        danmakus: Vec::new(),
        default_danmaku_index: None,
    }
}

#[async_trait]
impl DynamicPlaylistProvider for StubDynamicProvider {
    async fn list_playlist(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &Playlist,
        target: Option<&ProviderTarget>,
        _query: DynamicListQuery,
    ) -> Result<DynamicListResult, ProviderError> {
        let items = match target
            .map(decode_alist_target)
            .as_deref()
            .unwrap_or_default()
        {
            "" => vec![DynamicPlaylistItem {
                name: self.folder_cursor().to_string(),
                item_type: ItemType::Playlist,
                target: alist_target(self.folder_cursor()),
                size: None,
                thumbnail: None,
                description: None,
                modified_at: None,
                source_config: None,
                metadata: None,
            }],
            cursor if cursor == self.folder_cursor() => vec![DynamicPlaylistItem {
                name: self
                    .first_item_path()
                    .rsplit('/')
                    .next()
                    .unwrap_or(self.first_item_path())
                    .to_string(),
                item_type: ItemType::Media,
                target: alist_target(self.first_item_path()),
                size: None,
                thumbnail: None,
                description: None,
                modified_at: None,
                source_config: None,
                metadata: None,
            }],
            _ => Vec::new(),
        };
        Ok(DynamicListResult {
            has_more: false,
            items,
            pagination: synctv_core::provider::DynamicPagination::Page { page: 1 },
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
        target: &ProviderTarget,
        play_mode: PlayMode,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        let cursor = decode_alist_target(target);
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
        target: Option<&ProviderTarget>,
    ) -> Result<Vec<DynamicBrowsePathSegment>, ProviderError> {
        let Some(cursor) = target.map(decode_alist_target) else {
            return Ok(Vec::new());
        };

        Ok(match cursor.as_str() {
            path if path == self.folder_cursor() || path == self.first_item_path() => {
                vec![DynamicBrowsePathSegment {
                    name: self.folder_cursor().to_string(),
                    target: alist_target(self.folder_cursor()),
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
        providers: vec![synctv_core::models::SourceProvider::Alist],
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
        source_provider: Some(synctv_core::models::SourceProvider::Alist),
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
        .create_provider_with_default_config("alist", "alist_default")
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
        synctv_api::ClientApiOptions {
            read_pool: None,
            user_service,
            room_service: room_service.clone(),
            connection_service: Arc::new(ConnectionManager::new(ConnectionLimits::default())),
            runtime_settings: Arc::new(Config::default()),
            publish_key_service: None,
            jwt_service: JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
            live_streaming_infrastructure: None,
            runtime_settings_store: None,
            public_id_codec: Arc::new(synctv_api::PublicIdCodec::plain()),
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
                target: proto_alist_target("/episode-1.mp4"),
                client_operation_id: None,
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
    assert_proto_alist_target(state.target.as_ref(), "/episode-1.mp4");

    let playback = response.playback.unwrap();
    assert_eq!(playback.playlist_id, playlist_public_id);
    assert_eq!(playback.name, "episode-1.mp4");
    assert_proto_alist_target(playback.target.as_ref(), "/episode-1.mp4");
    let direct = playback.playback_infos.get("proxy_direct").unwrap();
    assert_eq!(direct.medias.len(), 1);
    assert_eq!(
        direct.medias[0].url,
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
                client_operation_id: None,
                client_time_millis: None,
            },
        )
        .await
        .unwrap();
    let update_state = update_response;
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
        .create_provider_with_default_config("alist", "alist_default")
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
        synctv_api::ClientApiOptions {
            read_pool: None,
            user_service,
            room_service: room_service.clone(),
            connection_service: Arc::new(ConnectionManager::new(ConnectionLimits::default())),
            runtime_settings: Arc::new(Config::default()),
            publish_key_service: None,
            jwt_service: JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
            live_streaming_infrastructure: None,
            runtime_settings_store: None,
            public_id_codec: Arc::new(synctv_api::PublicIdCodec::plain()),
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
                target: proto_alist_target("season-1"),
                pagination: Some(
                    synctv_proto::client::list_playlist_items_request::Pagination::Page(
                        synctv_proto::client::PagePagination { page: 1 },
                    ),
                ),
                page_size: 50,
                search: String::new(),
                source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
                provider_instance_name: String::new(),
                sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
                refresh: false,
                preview_source_config: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(response.dynamic_items.len(), 1);
    assert_eq!(response.dynamic_items[0].name, "episode-1.mp4");
    assert_proto_alist_target(
        response.dynamic_items[0].target.as_ref(),
        "season-1/episode-1.mp4",
    );

    assert_eq!(response.current_path.len(), 2);
    assert_eq!(response.current_path[0].playlist_id, playlist_public_id);
    assert_eq!(response.current_path[0].name, "Dynamic Playlist");
    assert!(response.current_path[0].target.is_none());
    assert_eq!(response.current_path[1].playlist_id, "");
    assert_eq!(response.current_path[1].name, "season-1");
    assert_proto_alist_target(response.current_path[1].target.as_ref(), "season-1");
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
        .create_provider_with_default_config("alist", "alist_default")
        .await
        .unwrap();
    providers_manager
        .create_provider_with_default_config("alist", "alist_alt")
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
        synctv_api::ClientApiOptions {
            read_pool: None,
            user_service,
            room_service: room_service.clone(),
            connection_service: Arc::new(ConnectionManager::new(ConnectionLimits::default())),
            runtime_settings: Arc::new(Config::default()),
            publish_key_service: None,
            jwt_service: JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
            live_streaming_infrastructure: None,
            runtime_settings_store: None,
            public_id_codec: Arc::new(synctv_api::PublicIdCodec::plain()),
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
                target: proto_alist_target("/bound-episode-1.mp4"),
                client_operation_id: None,
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
        direct.medias[0].url,
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
        .create_provider_with_default_config("direct_url", "direct_url")
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
        .create(&make_user("api_playback_provider_owner"))
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
        source_config: synctv_core_testing::direct_url_media_source_config_with_headers(
            "https://example.com/video.mp4",
            HashMap::from([(
                "Authorization".to_string(),
                "Bearer provider-token".to_string(),
            )]),
        ),
        source_provider: synctv_core::models::SourceProvider::DirectUrl,
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
        synctv_api::ClientApiOptions {
            read_pool: None,
            user_service,
            room_service,
            connection_service: Arc::new(ConnectionManager::new(ConnectionLimits::default())),
            runtime_settings: Arc::new(Config::default()),
            publish_key_service: None,
            jwt_service: JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
            live_streaming_infrastructure: None,
            runtime_settings_store: None,
            public_id_codec: Arc::new(synctv_api::PublicIdCodec::plain()),
            chat_service: None,
            provider_stores,
            email_api: None,
            passkey_service: None,
        },
        synctv_api::ClientApiRuntime {
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
                target: None,
                client_operation_id: None,
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
    let direct = playback.playback_infos.get("proxy_direct").unwrap();
    assert_eq!(direct.medias.len(), 1);
    assert!(
        direct.medias[0]
            .url
            .starts_with("/api/playback-providers/direct-url/"),
        "signed provider playback should expose proxy URL, got {}",
        direct.medias[0].url
    );
    assert!(
        direct.medias[0].url.contains("/streams/direct/0?"),
        "signed direct-url playback should use stream proxy contract, got {}",
        direct.medias[0].url
    );
    assert!(
        direct.medias[0].headers.is_empty(),
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
        synctv_api::ClientApiOptions {
            read_pool: None,
            user_service,
            room_service,
            connection_service: Arc::new(ConnectionManager::new(ConnectionLimits::default())),
            runtime_settings: Arc::new(Config::default()),
            publish_key_service: None,
            jwt_service: JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
            live_streaming_infrastructure: None,
            runtime_settings_store: None,
            public_id_codec: Arc::new(synctv_api::PublicIdCodec::plain()),
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
    let provider_instance_manager = Arc::new(RemoteProviderManager::new(Arc::new(
        ProviderInstanceRepository::new(pool.clone()),
    )));
    let mut providers_manager =
        ProvidersManager::new(provider_instance_manager).expect("providers manager should build");
    providers_manager.register_factory(
        "direct_url",
        Box::new(|_instance_id, _config, _instance_manager| {
            Ok(Arc::new(TransientPlaybackFailureProvider))
        }),
    );
    providers_manager
        .create_provider_with_default_config("direct_url", "direct_url")
        .await
        .unwrap();

    let room_service = RoomService::new_with_providers_for_tests(
        pool.clone(),
        (*user_service).clone(),
        Arc::new(providers_manager),
    )
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
        name: "Transient Playback Provider".to_string(),
        description: String::new(),
        source_config: synctv_core_testing::direct_url_media_source_config(
            "https://example.com/video.mp4",
        ),
        source_provider: synctv_core::models::SourceProvider::DirectUrl,
        provider_instance_name: None,
        position: 0.0,
    });
    let media = media_repo.create(&media).await.unwrap();
    let codec = public_id_codec();
    let room_public_id = codec.encode_room_id(room.id).unwrap();
    let media_public_id = codec.encode_media_id(media.id).unwrap();

    let client_api = ClientApiImpl::new_with_runtime(
        synctv_api::ClientApiOptions {
            read_pool: None,
            user_service,
            room_service: room_service.clone(),
            connection_service: Arc::new(ConnectionManager::new(ConnectionLimits::default())),
            runtime_settings: Arc::new(Config::default()),
            publish_key_service: None,
            jwt_service: JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
            live_streaming_infrastructure: None,
            runtime_settings_store: None,
            public_id_codec: Arc::new(synctv_api::PublicIdCodec::plain()),
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
                target: None,
                client_operation_id: None,
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
async fn test_start_playback_returns_error_for_invalid_live_proxy_source_config() {
    let (_postgres, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());
    let user_service = Arc::new(make_user_service(&pool));

    let room_service = RoomService::new_for_tests(pool.clone(), (*user_service).clone())
        .expect("room service should build");
    let room_service = Arc::new(room_service);
    room_service
        .media_service()
        .providers_manager()
        .create_builtin_defaults()
        .await
        .expect("built-in providers should initialize");

    let owner = user_repo
        .create(&make_user("api_playback_invalid_live_proxy"))
        .await
        .unwrap();
    let (room, _) = room_service
        .create_room(
            "API Invalid Live Proxy Playback".to_string(),
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
        name: "Invalid Live Proxy Playback Provider".to_string(),
        description: String::new(),
        source_config: synctv_core_testing::live_proxy_pull_live_media_source_config("not-a-url"),
        source_provider: synctv_core::models::SourceProvider::LiveProxy,
        provider_instance_name: None,
        position: 0.0,
    });
    let media = media_repo.create(&media).await.unwrap();
    let codec = public_id_codec();
    let room_public_id = codec.encode_room_id(room.id).unwrap();
    let media_public_id = codec.encode_media_id(media.id).unwrap();

    let client_api = ClientApiImpl::new_with_runtime(
        synctv_api::ClientApiOptions {
            read_pool: None,
            user_service,
            room_service: room_service.clone(),
            connection_service: Arc::new(ConnectionManager::new(ConnectionLimits::default())),
            runtime_settings: Arc::new(Config::default()),
            publish_key_service: None,
            jwt_service: JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
            live_streaming_infrastructure: None,
            runtime_settings_store: None,
            public_id_codec: Arc::new(synctv_api::PublicIdCodec::plain()),
            chat_service: None,
            provider_stores: Arc::new(synctv_core::provider::ProviderStoreRegistry::local_only(
                "test:provider:",
            )),
            email_api: None,
            passkey_service: None,
        },
        support::client_api_runtime(),
    );

    let error = client_api
        .start_playback(
            &owner.id,
            &room_public_id,
            synctv_proto::client::StartPlaybackRequest {
                media_id: media_public_id,
                playlist_id: String::new(),
                target: None,
                client_operation_id: None,
            },
        )
        .await
        .expect_err("invalid provider config should reject the playback switch");

    assert!(matches!(
        error,
        synctv_api::ApiError::InvalidInput(message)
            if message.contains("Invalid LiveProxy source URL")
    ));
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
        .create_provider_with_default_config("alist", "alist_default")
        .await
        .unwrap();
    providers_manager
        .create_provider_with_default_config("alist", "alist_alt")
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
        synctv_api::ClientApiOptions {
            read_pool: None,
            user_service,
            room_service: room_service.clone(),
            connection_service: Arc::new(ConnectionManager::new(ConnectionLimits::default())),
            runtime_settings: Arc::new(Config::default()),
            publish_key_service: None,
            jwt_service: JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
            live_streaming_infrastructure: None,
            runtime_settings_store: None,
            public_id_codec: Arc::new(synctv_api::PublicIdCodec::plain()),
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
                target: None,
                pagination: Some(
                    synctv_proto::client::list_playlist_items_request::Pagination::Page(
                        synctv_proto::client::PagePagination { page: 1 },
                    ),
                ),
                page_size: 50,
                search: String::new(),
                source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
                provider_instance_name: String::new(),
                sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
                refresh: false,
                preview_source_config: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(response.dynamic_items.len(), 1);
    assert_eq!(response.dynamic_items[0].name, "bound-season-1");
    assert_proto_alist_target(response.dynamic_items[0].target.as_ref(), "bound-season-1");
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
            thumbnail: None,
            medias: vec![synctv_core::models::PlaybackMedia {
                name: String::new(),
                format: "mp4".to_string(),
                expire_at: None,
                metadata: None,
                p2p_swarm_id: None,
                provider: synctv_core::models::PlaybackMediaProvider::DirectUrl(
                    synctv_core::models::PlaybackDirectUrlMedia::Direct {
                        url: "https://example.com/root.mp4".to_string(),
                        headers: std::collections::HashMap::new(),
                    },
                ),
            }],
            default_media_index: None,
            subtitles: Vec::new(),
            default_subtitle_index: None,
            danmakus: Vec::new(),
            default_danmaku_index: None,
        },
        0.0,
    )
    .expect("direct media should build");
    synctv_core::repository::MediaRepository::new(pool.clone())
        .create(&root_media)
        .await
        .unwrap();

    let client_api = ClientApiImpl::new_with_runtime(
        synctv_api::ClientApiOptions {
            read_pool: None,
            user_service,
            room_service,
            connection_service: Arc::new(ConnectionManager::new(ConnectionLimits::default())),
            runtime_settings: Arc::new(Config::default()),
            publish_key_service: None,
            jwt_service: JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
            live_streaming_infrastructure: None,
            runtime_settings_store: None,
            public_id_codec: Arc::new(synctv_api::PublicIdCodec::plain()),
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
                target: None,
                pagination: Some(
                    synctv_proto::client::list_playlist_items_request::Pagination::Page(
                        synctv_proto::client::PagePagination { page: 1 },
                    ),
                ),
                page_size: 50,
                search: String::new(),
                source_provider: synctv_proto::source_config::SourceProvider::Unspecified as i32,
                provider_instance_name: String::new(),
                sort_by: synctv_proto::client::MediaListSortBy::Position as i32,
                sort_direction: synctv_proto::client::SortDirection::Asc as i32,
                availability: synctv_proto::client::ResourceAvailabilityFilter::All as i32,
                refresh: false,
                preview_source_config: None,
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
