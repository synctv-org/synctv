#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use async_trait::async_trait;
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
        DynamicFolder, ItemType, MediaProvider, NextPlayItem, PlaybackInfo, PlaybackResult,
        ProviderContext, ProviderError,
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
struct FakeDynamicProvider;

impl FakeDynamicProvider {
    fn item(path: &str) -> NextPlayItem {
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        NextPlayItem {
            name,
            item_type: ItemType::Media,
            source_config: serde_json::json!({ "path": path }),
            metadata: serde_json::json!({}),
            provider_data: serde_json::json!({}),
            relative_path: path.to_string(),
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
                urls: vec![format!("https://example.com{path}")],
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
        _relative_path: Option<&str>,
        _page: usize,
        _page_size: usize,
    ) -> Result<Vec<synctv_core::provider::DirectoryItem>, ProviderError> {
        Ok(Vec::new())
    }

    async fn resolve_item(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &Playlist,
        relative_path: &str,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        Ok(match relative_path {
            "/episode-1.mp4" | "/episode-2.mp4" => Some(Self::item(relative_path)),
            _ => None,
        })
    }

    async fn next(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &Playlist,
        _playing_media: &synctv_core::models::Media,
        relative_path: &str,
        play_mode: PlayMode,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        Ok(match (relative_path, play_mode) {
            ("/episode-1.mp4", PlayMode::Sequential | PlayMode::RepeatAll | PlayMode::Shuffle) => {
                Some(Self::item("/episode-2.mp4"))
            }
            ("/episode-2.mp4", PlayMode::RepeatAll) => Some(Self::item("/episode-1.mp4")),
            ("/episode-1.mp4" | "/episode-2.mp4", PlayMode::RepeatOne) => None,
            _ => None,
        })
    }
}

async fn create_dynamic_playlist(
    pool: &sqlx::PgPool,
    room_id: &RoomId,
    owner_id: &UserId,
) -> Playlist {
    let playlist = Playlist {
        id: PlaylistId::new(),
        room_id: room_id.clone(),
        creator_id: Some(owner_id.clone()),
        name: "Dynamic Playlist".to_string(),
        parent_id: None,
        position: 0,
        source_provider: Some("fake_dynamic".to_string()),
        source_config: Some(serde_json::json!({})),
        provider_instance_name: Some("fake_dynamic_default".to_string()),
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
        Box::new(|_instance_id, _config, _instance_manager| Ok(Arc::new(FakeDynamicProvider))),
    );
    let providers_manager = Arc::new(providers_manager);

    let mut room_service =
        RoomService::new_with_providers(pool.clone(), (*user_service).clone(), providers_manager.clone());
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

    let owner = user_repo.create(&make_user("api_dynamic_owner")).await.unwrap();
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

    let playlist = create_dynamic_playlist(&pool, &room.id, &owner.id).await;

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
                relative_path: "/episode-1.mp4".to_string(),
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
    assert_eq!(state.relative_path, "/episode-1.mp4");

    let playback_result = response.playback_result.unwrap();
    assert_eq!(playback_result.playlist_id, playlist.id.as_str());
    assert_eq!(playback_result.name, "episode-1.mp4");
    let relative_path_meta = playback_result.metadata.get("relative_path").unwrap();
    let relative_path_value: serde_json::Value = serde_json::from_str(relative_path_meta).unwrap();
    assert_eq!(relative_path_value, serde_json::json!("/episode-1.mp4"));
    let direct = playback_result.playback_infos.get("direct").unwrap();
    assert_eq!(direct.urls.len(), 1);
    assert_eq!(direct.urls[0].url, "https://example.com/episode-1.mp4");
}
