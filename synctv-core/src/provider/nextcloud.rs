//! Nextcloud DAV media provider.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use rand::seq::IndexedRandom;
use sha2::{Digest, Sha256};

use super::{
    DirectoryItem, DirectoryItemThumbnail, DynamicBrowsePathSegment, DynamicListQuery,
    DynamicListResult, DynamicPagination, DynamicPlaylistProvider, ItemType, MediaProvider,
    NextPlayItem, PlaybackInfo, PlaybackResult, ProviderContext, ProviderCredentialDependency,
    ProviderError, SourceConfig, SourceCover,
};
use crate::models::{
    detect_direct_url_format, normalize_provider_instance_name,
    normalize_provider_instance_name_owned, MediaSourceConfig, NextcloudMediaSourceConfig,
    NextcloudPlaybackMetadata, NextcloudPlaylistSource, NextcloudPlaylistSourceConfig, PlayMode,
    PlaybackMedia, PlaybackMediaProvider, PlaybackMetadata, PlaybackNextcloudMedia,
    PlaybackNextcloudSubtitle, PlaybackSubtitle, PlaybackSubtitleProvider, PlaylistSourceConfig,
    ProviderCredential, ProviderTarget, UserId, UserProviderCredential,
};
use crate::repository::UserProviderCredentialRepository;
use synctv_media_providers::nextcloud::{
    NextcloudClient, NextcloudDavItem, NextcloudList, NextcloudLoginFlow, NextcloudServerInfo,
};

const PLAYBACK_CACHE_TTL: Duration = Duration::from_hours(2);
const SHUFFLE_LIMIT: usize = 200;
const RELATED_SUBTITLE_LIMIT: usize = 32;

#[derive(Debug, Clone)]
pub struct NextcloudBind {
    pub id: i64,
    pub server_id: String,
    pub endpoint: String,
    pub username: String,
    pub user_id: String,
    pub version: String,
    pub edition: String,
    pub created_at: i64,
    pub provider_instance_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NextcloudListResponse {
    pub content: Vec<NextcloudDavItem>,
    pub total: Option<u64>,
    pub page: usize,
    pub has_more: bool,
}

#[derive(Clone)]
struct AuthenticatedNextcloud {
    client: NextcloudClient,
    username: String,
    user_id: String,
    app_password: String,
    instance_name: Option<String>,
}

pub struct NextcloudProvider {
    http_client: reqwest::Client,
    credential_repo: Option<Arc<UserProviderCredentialRepository>>,
}

impl NextcloudProvider {
    pub const NAME: &'static str = "nextcloud";

    #[must_use]
    pub fn with_http_client(http_client: reqwest::Client) -> Self {
        Self {
            http_client,
            credential_repo: None,
        }
    }

    #[must_use]
    pub fn with_credential_repo(
        &self,
        credential_repo: Arc<UserProviderCredentialRepository>,
    ) -> Self {
        Self {
            http_client: self.http_client.clone(),
            credential_repo: Some(credential_repo),
        }
    }

    fn credential_repo(&self) -> Result<&UserProviderCredentialRepository, ProviderError> {
        self.credential_repo.as_deref().ok_or_else(|| {
            ProviderError::Internal("Nextcloud credential repository is unavailable".to_string())
        })
    }

    fn credential_repo_or<'a>(
        &'a self,
        fallback: Option<&'a UserProviderCredentialRepository>,
    ) -> Result<&'a UserProviderCredentialRepository, ProviderError> {
        self.credential_repo.as_deref().or(fallback).ok_or_else(|| {
            ProviderError::Internal("Nextcloud credential repository is unavailable".to_string())
        })
    }

    fn client(&self, endpoint: &str) -> Result<NextcloudClient, ProviderError> {
        NextcloudClient::with_http_client(endpoint, self.http_client.clone()).map_err(Into::into)
    }

    #[must_use]
    pub fn credential_server_id_for_instance(
        endpoint: &str,
        provider_instance_name: Option<&str>,
    ) -> String {
        let endpoint = endpoint.trim().trim_end_matches('/');
        let instance = normalize_provider_instance_name(provider_instance_name).unwrap_or_default();
        hex::encode(Sha256::digest(format!("{endpoint}\n{instance}").as_bytes()))
    }

    pub async fn login_and_persist(
        &self,
        user_id: UserId,
        endpoint: String,
        username: String,
        app_password: String,
        provider_instance_name: Option<String>,
    ) -> Result<(String, NextcloudServerInfo), ProviderError> {
        if username.trim().is_empty() || app_password.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Nextcloud username and app password are required".to_string(),
            ));
        }
        let client = self.client(&endpoint)?;
        let info = client.server_info(&username, &app_password).await?;
        let provider_instance_name = normalize_provider_instance_name_owned(provider_instance_name);
        let server_id =
            Self::credential_server_id_for_instance(&endpoint, provider_instance_name.as_deref());
        let now = Utc::now();
        self.credential_repo()?
            .upsert_by_user_provider_server(&UserProviderCredential {
                id: 0,
                user_id,
                provider: Self::NAME.to_string(),
                server_id: server_id.clone(),
                provider_instance_name,
                credential_data: ProviderCredential::Nextcloud {
                    endpoint,
                    username,
                    user_id: info.user.id.clone(),
                    app_password,
                    version: info.capabilities.version.clone(),
                    edition: info.capabilities.edition.clone(),
                    capabilities: info.capabilities.values.clone(),
                },
                expires_at: None,
                created_at: now,
                updated_at: now,
            })
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?;
        Ok((server_id, info))
    }

    pub async fn start_login_flow(
        &self,
        endpoint: &str,
    ) -> Result<NextcloudLoginFlow, ProviderError> {
        self.client(endpoint)?
            .start_login_flow()
            .await
            .map_err(Into::into)
    }

    pub async fn poll_login_and_persist(
        &self,
        owner: UserId,
        endpoint: String,
        poll_endpoint: &str,
        poll_token: &str,
        provider_instance_name: Option<String>,
    ) -> Result<(String, NextcloudServerInfo), ProviderError> {
        let credentials = self
            .client(&endpoint)?
            .poll_login_flow(poll_endpoint, poll_token)
            .await?;
        self.login_and_persist(
            owner,
            credentials.server,
            credentials.login_name,
            credentials.app_password,
            provider_instance_name,
        )
        .await
    }

    async fn authenticated_with_repo(
        &self,
        repo: &UserProviderCredentialRepository,
        owner: UserId,
        server_id: &str,
    ) -> Result<AuthenticatedNextcloud, ProviderError> {
        let credential = repo
            .get_by_provider_and_server(owner, Self::NAME, server_id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
            .ok_or(ProviderError::CredentialRequired)?;
        let ProviderCredential::Nextcloud {
            endpoint,
            username,
            user_id,
            app_password,
            version: _,
            edition: _,
            capabilities: _,
        } = credential.credential_data
        else {
            return Err(ProviderError::InvalidCredentialType);
        };
        Ok(AuthenticatedNextcloud {
            client: self.client(&endpoint)?,
            username,
            user_id,
            app_password,
            instance_name: credential.provider_instance_name,
        })
    }

    async fn authenticated(
        &self,
        owner: UserId,
        server_id: &str,
    ) -> Result<AuthenticatedNextcloud, ProviderError> {
        self.authenticated_with_repo(self.credential_repo()?, owner, server_id)
            .await
    }

    pub async fn list(
        &self,
        owner: UserId,
        server_id: &str,
        path: &str,
        page: usize,
        page_size: usize,
        search: Option<&str>,
    ) -> Result<(NextcloudListResponse, Option<String>), ProviderError> {
        validate_path(path)?;
        let auth = self.authenticated(owner, server_id).await?;
        let response = if let Some(search) = search.filter(|value| !value.trim().is_empty()) {
            auth.client
                .search(
                    &auth.username,
                    &auth.app_password,
                    path,
                    search,
                    page as u64,
                    u32::try_from(page_size).unwrap_or(u32::MAX),
                )
                .await?
        } else {
            auth.client
                .list(
                    &auth.username,
                    &auth.app_password,
                    path,
                    page as u64,
                    u32::try_from(page_size).unwrap_or(u32::MAX),
                )
                .await?
        };
        Ok((map_list_response(response), auth.instance_name))
    }

    pub async fn list_binds(
        &self,
        owner: UserId,
        provider_instance_name: Option<&str>,
    ) -> Result<Vec<NextcloudBind>, ProviderError> {
        let requested = normalize_provider_instance_name(provider_instance_name);
        self.credential_repo()?
            .get_readable_by_provider(owner, Self::NAME)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
            .into_iter()
            .filter(|credential| {
                requested
                    .is_none_or(|name| credential.provider_instance_name.as_deref() == Some(name))
            })
            .map(|credential| {
                let ProviderCredential::Nextcloud {
                    endpoint,
                    username,
                    user_id,
                    version,
                    edition,
                    ..
                } = credential.credential_data
                else {
                    return Err(ProviderError::InvalidCredentialType);
                };
                Ok(NextcloudBind {
                    id: credential.id,
                    server_id: credential.server_id,
                    endpoint,
                    username,
                    user_id,
                    version,
                    edition,
                    created_at: credential.created_at.timestamp(),
                    provider_instance_name: credential.provider_instance_name,
                })
            })
            .collect()
    }

    pub async fn list_favorites(
        &self,
        owner: UserId,
        server_id: &str,
        page: usize,
        page_size: usize,
    ) -> Result<(NextcloudListResponse, Option<String>), ProviderError> {
        let auth = self.authenticated(owner, server_id).await?;
        let response = auth
            .client
            .favorites(
                &auth.username,
                &auth.app_password,
                page.max(1) as u64,
                u32::try_from(page_size.clamp(1, 200)).unwrap_or(200),
            )
            .await?;
        Ok((map_list_response(response), auth.instance_name))
    }

    pub async fn delete_credential(
        &self,
        owner: UserId,
        server_id: &str,
    ) -> Result<bool, ProviderError> {
        let Some(credential) = self
            .credential_repo()?
            .get_by_provider_and_server(owner, Self::NAME, server_id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
        else {
            return Ok(false);
        };
        self.credential_repo()?
            .delete(credential.id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?;
        Ok(true)
    }

    pub async fn get_resource(
        &self,
        store: Option<&Arc<dyn super::ProviderStore>>,
        version: &str,
        mode_name: &str,
        media_index: usize,
        request_context: Option<&super::ExecutionControl>,
        range_header: Option<&str>,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let media = versioned
            .result
            .playback_infos
            .get(mode_name)
            .and_then(|info| info.medias.get(media_index))
            .ok_or(ProviderError::NotFound)?;
        let PlaybackMediaProvider::Nextcloud(PlaybackNextcloudMedia::Proxy {
            credential_owner_id,
            server_id,
            path,
            ..
        }) = &media.provider
        else {
            return Err(ProviderError::InvalidConfig(
                "Nextcloud cached playback resource is invalid".to_string(),
            ));
        };
        let owner = credential_owner_id
            .parse::<UserId>()
            .map_err(ProviderError::InvalidConfig)?;
        let auth = self.authenticated(owner, server_id).await?;
        super::playback_transport::transport_action_for_target_url(
            auth.client.file_url(&auth.user_id, path),
            NextcloudClient::auth_headers(&auth.username, &auth.app_password),
            range_header,
        )
    }

    pub async fn get_subtitle(
        &self,
        store: Option<&Arc<dyn super::ProviderStore>>,
        version: &str,
        mode_name: &str,
        subtitle_index: usize,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let subtitle = versioned
            .result
            .playback_infos
            .get(mode_name)
            .and_then(|info| info.subtitles.get(subtitle_index))
            .ok_or(ProviderError::NotFound)?;
        let PlaybackSubtitleProvider::Nextcloud(subtitle) = &subtitle.provider else {
            return Err(ProviderError::InvalidConfig(
                "Nextcloud cached subtitle resource is invalid".to_string(),
            ));
        };
        let media = versioned
            .result
            .playback_infos
            .get(mode_name)
            .and_then(|info| info.medias.first())
            .ok_or(ProviderError::NotFound)?;
        let PlaybackMediaProvider::Nextcloud(provider) = &media.provider else {
            return Err(ProviderError::NotFound);
        };
        let (owner, server_id) = match provider {
            PlaybackNextcloudMedia::Refresh {
                credential_owner_id,
                server_id,
                ..
            }
            | PlaybackNextcloudMedia::Proxy {
                credential_owner_id,
                server_id,
                ..
            } => (credential_owner_id, server_id),
        };
        let owner = owner
            .parse::<UserId>()
            .map_err(ProviderError::InvalidConfig)?;
        let auth = self.authenticated(owner, server_id).await?;
        super::playback_transport::transport_action_for_target_url(
            auth.client.file_url(&auth.user_id, &subtitle.path),
            NextcloudClient::auth_headers(&auth.username, &auth.app_password),
            None,
        )
    }

    pub async fn thumbnail_action(
        &self,
        owner: UserId,
        server_id: &str,
        file_id: u64,
        width: u32,
        height: u32,
        crop: bool,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        if file_id == 0 {
            return Err(ProviderError::InvalidConfig(
                "Nextcloud preview file_id is required".to_string(),
            ));
        }
        let auth = self.authenticated(owner, server_id).await?;
        super::playback_transport::transport_action_for_target_url(
            auth.client
                .preview_url(file_id, width.clamp(1, 2048), height.clamp(1, 2048), crop)?,
            NextcloudClient::auth_headers(&auth.username, &auth.app_password),
            None,
        )
    }

    fn media_config(
        config: &MediaSourceConfig,
    ) -> Result<&NextcloudMediaSourceConfig, ProviderError> {
        match config {
            MediaSourceConfig::Nextcloud(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "Expected Nextcloud media source_config".to_string(),
            )),
        }
    }

    fn playlist_config(
        config: &PlaylistSourceConfig,
    ) -> Result<&NextcloudPlaylistSourceConfig, ProviderError> {
        match config {
            PlaylistSourceConfig::Nextcloud(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "Expected Nextcloud playlist source_config".to_string(),
            )),
        }
    }

    fn source_server_id(source: SourceConfig<'_>) -> Result<&str, ProviderError> {
        match source {
            SourceConfig::Media(config) => Ok(&Self::media_config(config)?.server_id),
            SourceConfig::DynamicPlaylist(config) => Ok(&Self::playlist_config(config)?.server_id),
        }
    }

    async fn playlist_cover_file_id(
        auth: &AuthenticatedNextcloud,
        source: &NextcloudPlaylistSource,
    ) -> Result<Option<u64>, ProviderError> {
        match source {
            NextcloudPlaylistSource::Favorites => {
                let response = auth
                    .client
                    .favorites(&auth.username, &auth.app_password, 1, 200)
                    .await?;
                Ok(first_preview_file_id(&response.items))
            }
            NextcloudPlaylistSource::Search { path, query } => {
                let response = auth
                    .client
                    .search(&auth.username, &auth.app_password, path, query, 1, 200)
                    .await?;
                Ok(first_preview_file_id(&response.items))
            }
            NextcloudPlaylistSource::Folder { path } => {
                let mut queue = VecDeque::from([path.clone()]);
                let mut visited = HashSet::new();
                while let Some(path) = queue.pop_front() {
                    if visited.len() >= 32 || !visited.insert(path.clone()) {
                        continue;
                    }
                    let response = auth
                        .client
                        .list(&auth.username, &auth.app_password, &path, 1, 200)
                        .await?;
                    if let Some(file_id) = first_preview_file_id(&response.items) {
                        return Ok(Some(file_id));
                    }
                    queue.extend(
                        response
                            .items
                            .into_iter()
                            .filter(|item| item.is_directory)
                            .map(|item| item.path),
                    );
                }
                Ok(None)
            }
        }
    }
}

#[async_trait]
impl MediaProvider for NextcloudProvider {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    async fn generate_playback(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &MediaSourceConfig,
    ) -> Result<PlaybackResult, ProviderError> {
        let config = Self::media_config(source_config)?;
        validate_file_path(&config.path)?;
        if config.file_id == 0 {
            return Err(ProviderError::InvalidConfig(
                "Nextcloud media file_id is required".to_string(),
            ));
        }
        let owner = *ctx
            .credential_owner_or_user_id()
            .ok_or(ProviderError::CredentialRequired)?;
        let auth = self
            .authenticated_with_repo(
                self.credential_repo_or(ctx.credential_repo)?,
                owner,
                &config.server_id,
            )
            .await?;
        let item = auth
            .client
            .metadata(&auth.username, &auth.app_password, &config.path)
            .await?;
        if item.is_directory || !is_playable(&item) {
            return Err(ProviderError::InvalidConfig(
                "Nextcloud media path must identify a playable file".to_string(),
            ));
        }
        let duration_seconds = item
            .duration_millis
            .map(|duration| std::time::Duration::from_millis(duration).as_secs_f64());
        let metadata = NextcloudPlaybackMetadata {
            file_id: item.file_id,
            name: item.name.clone(),
            path: item.path.clone(),
            size: item.size,
            modified_at: item.modified_at.clone(),
            content_type: item.content_type.clone(),
            etag: item.etag.clone(),
            permissions: item.permissions.clone(),
            owner_id: item.owner_id.clone(),
            owner_display_name: item.owner_display_name.clone(),
            favorite: item.favorite,
            has_preview: item.has_preview,
            blurhash: item.blurhash.clone(),
            width: item.width,
            height: item.height,
            duration_millis: item.duration_millis,
        };
        let subtitles = discover_subtitles(&auth, &config.path).await?;
        let mut playback_infos = HashMap::new();
        playback_infos.insert(
            "original".to_string(),
            PlaybackInfo {
                thumbnail: item.has_preview.then(|| item.file_id.to_string()),
                medias: vec![PlaybackMedia {
                    name: item.name,
                    format: detect_direct_url_format(&config.path).to_string(),
                    expire_at: None,
                    metadata: None,
                    provider: PlaybackMediaProvider::Nextcloud(PlaybackNextcloudMedia::Refresh {
                        credential_owner_id: owner.to_string(),
                        server_id: config.server_id.clone(),
                        path: config.path.clone(),
                        file_id: item.file_id,
                    }),
                }],
                default_media_index: Some(0),
                subtitles,
                default_subtitle_index: None,
                danmakus: Vec::new(),
                default_danmaku_index: None,
            },
        );
        let result = PlaybackResult {
            playback_infos,
            default_mode: "original".to_string(),
            provider: Self::NAME.to_string(),
            provider_instance_name: auth.instance_name,
            duration_seconds,
            is_live: Some(false),
            metadata: Some(PlaybackMetadata::Nextcloud(metadata)),
        };
        super::cached_versioned_playback_or_fill(
            Self::NAME,
            &format!(
                "playback:{owner}:{}:room:{}:{}",
                config.server_id,
                ctx.room_id
                    .map_or_else(|| "none".to_string(), |room| room.to_string()),
                config.path
            ),
            PLAYBACK_CACHE_TTL,
            ctx,
            mark_playback_resources,
            || async { Ok(result) },
        )
        .await
    }

    async fn validate_source_config(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<(), ProviderError> {
        let server_id = Self::source_server_id(source_config)?;
        match source_config {
            SourceConfig::Media(config) => {
                let config = Self::media_config(config)?;
                validate_file_path(&config.path)?;
                if config.file_id == 0 {
                    return Err(ProviderError::InvalidConfig(
                        "Nextcloud media file_id is required".to_string(),
                    ));
                }
            }
            SourceConfig::DynamicPlaylist(config) => {
                validate_playlist_source(&Self::playlist_config(config)?.source)?;
            }
        }
        let owner = *ctx.credential_owner_id().ok_or_else(|| {
            ProviderError::Internal("credential_owner_id is unavailable".to_string())
        })?;
        let exists = self
            .credential_repo_or(ctx.credential_repo)?
            .get_by_provider_and_server(owner, Self::NAME, server_id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
            .is_some();
        if !exists {
            return Err(ProviderError::CredentialNotFound(format!(
                "Referenced Nextcloud credential not found for server_id '{server_id}'"
            )));
        }
        Ok(())
    }

    async fn source_cover(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<Option<SourceCover>, ProviderError> {
        let owner = *ctx
            .credential_owner_or_user_id()
            .ok_or(ProviderError::CredentialRequired)?;
        let (server_id, file_id) = match source_config {
            SourceConfig::Media(config) => {
                let config = Self::media_config(config)?;
                (config.server_id.clone(), Some(config.file_id))
            }
            SourceConfig::DynamicPlaylist(config) => {
                let config = Self::playlist_config(config)?;
                let auth = self
                    .authenticated_with_repo(
                        self.credential_repo_or(ctx.credential_repo)?,
                        owner,
                        &config.server_id,
                    )
                    .await?;
                let file_id = Self::playlist_cover_file_id(&auth, &config.source).await?;
                (config.server_id.clone(), file_id)
            }
        };
        Ok(file_id.map(|file_id| SourceCover::Nextcloud {
            server_id,
            credential_owner_id: owner,
            file_id,
        }))
    }

    fn as_dynamic_playlist_provider(&self) -> Option<&dyn DynamicPlaylistProvider> {
        Some(self)
    }

    fn credential_dependencies(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<Vec<ProviderCredentialDependency>, ProviderError> {
        let owner = ctx
            .credential_owner_or_user_id()
            .ok_or(ProviderError::CredentialRequired)?;
        Ok(vec![ProviderCredentialDependency::new(
            Self::NAME,
            owner.to_string(),
            Self::source_server_id(source_config)?.to_string(),
        )])
    }
}

#[async_trait]
impl DynamicPlaylistProvider for NextcloudProvider {
    async fn list_playlist(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: Option<&ProviderTarget>,
        query: DynamicListQuery,
    ) -> Result<DynamicListResult, ProviderError> {
        let config =
            Self::playlist_config(playlist.source_config.as_ref().ok_or_else(|| {
                ProviderError::InvalidConfig("Missing source_config".to_string())
            })?)?;
        let owner = *ctx
            .credential_owner_or_user_id()
            .ok_or(ProviderError::CredentialRequired)?;
        let DynamicPagination::Page { page } = query.pagination else {
            return Err(ProviderError::InvalidConfig(
                "Nextcloud uses page pagination".to_string(),
            ));
        };
        let auth = self
            .authenticated_with_repo(
                self.credential_repo_or(ctx.credential_repo)?,
                owner,
                &config.server_id,
            )
            .await?;
        let target_path = decode_target(target)?.map(|target| target.path);
        let page = page.max(1);
        let page_size = u32::try_from(query.page_size.max(1)).unwrap_or(u32::MAX);
        let response = if let Some(search) = query
            .search
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            let path = target_path
                .as_deref()
                .unwrap_or_else(|| source_folder(&config.source));
            auth.client
                .search(
                    &auth.username,
                    &auth.app_password,
                    path,
                    search,
                    page as u64,
                    page_size,
                )
                .await?
        } else if let Some(path) = target_path.as_deref() {
            auth.client
                .list(
                    &auth.username,
                    &auth.app_password,
                    path,
                    page as u64,
                    page_size,
                )
                .await?
        } else {
            match &config.source {
                NextcloudPlaylistSource::Folder { path } => {
                    auth.client
                        .list(
                            &auth.username,
                            &auth.app_password,
                            path,
                            page as u64,
                            page_size,
                        )
                        .await?
                }
                NextcloudPlaylistSource::Favorites => {
                    auth.client
                        .favorites(&auth.username, &auth.app_password, page as u64, page_size)
                        .await?
                }
                NextcloudPlaylistSource::Search { path, query } => {
                    auth.client
                        .search(
                            &auth.username,
                            &auth.app_password,
                            path,
                            query,
                            page as u64,
                            page_size,
                        )
                        .await?
                }
            }
        };
        let has_more = response.has_more;
        let items = response
            .items
            .into_iter()
            .filter_map(|item| map_directory_item(item, owner, &config.server_id))
            .collect();
        Ok(DynamicListResult {
            items,
            pagination: DynamicPagination::Page { page },
            has_more,
        })
    }

    async fn resolve_item(
        &self,
        _ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: &ProviderTarget,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        let config =
            Self::playlist_config(playlist.source_config.as_ref().ok_or_else(|| {
                ProviderError::InvalidConfig("Missing source_config".to_string())
            })?)?;
        let target = decode_target(Some(target))?.ok_or_else(|| {
            ProviderError::InvalidConfig("Nextcloud target is required".to_string())
        })?;
        validate_file_path(&target.path)?;
        let provider_target = ProviderTarget::nextcloud(target.path.clone(), target.file_id);
        Ok(Some(NextPlayItem {
            name: target
                .path
                .rsplit('/')
                .next()
                .unwrap_or("Nextcloud media")
                .to_string(),
            item_type: ItemType::Media,
            source_config: MediaSourceConfig::Nextcloud(NextcloudMediaSourceConfig {
                server_id: config.server_id.clone(),
                path: target.path,
                file_id: target.file_id,
            }),
            target: provider_target,
        }))
    }

    async fn next(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: &ProviderTarget,
        play_mode: PlayMode,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        if play_mode == PlayMode::RepeatOne {
            return self.resolve_item(ctx, playlist, target).await;
        }
        let mut media = Vec::new();
        let mut page = 1;
        loop {
            let result = self
                .list_playlist(
                    ctx,
                    playlist,
                    None,
                    DynamicListQuery {
                        pagination: DynamicPagination::Page { page },
                        page_size: 100,
                        ..DynamicListQuery::default()
                    },
                )
                .await?;
            media.extend(
                result
                    .items
                    .into_iter()
                    .filter(|item| item.item_type == ItemType::Media),
            );
            if enough_for_next(&media, target, play_mode) || !result.has_more {
                break;
            }
            page = page.saturating_add(1);
        }
        let selected = match play_mode {
            PlayMode::Sequential => media
                .iter()
                .position(|item| &item.target == target)
                .and_then(|index| media.get(index + 1)),
            PlayMode::RepeatAll => media
                .iter()
                .position(|item| &item.target == target)
                .and_then(|index| media.get(index + 1))
                .or_else(|| media.first()),
            PlayMode::Shuffle => media.choose(&mut rand::rng()),
            PlayMode::RepeatOne => media.iter().find(|item| &item.target == target),
        };
        match selected {
            Some(item) => self.resolve_item(ctx, playlist, &item.target).await,
            None => Ok(None),
        }
    }

    async fn browse_path(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &crate::models::Playlist,
        target: Option<&ProviderTarget>,
    ) -> Result<Vec<DynamicBrowsePathSegment>, ProviderError> {
        let Some(target) = decode_target(target)? else {
            return Ok(Vec::new());
        };
        let mut current = String::new();
        Ok(target
            .path
            .split('/')
            .filter(|part| !part.is_empty())
            .map(|part| {
                current.push('/');
                current.push_str(part);
                DynamicBrowsePathSegment {
                    name: part.to_string(),
                    target: ProviderTarget::nextcloud(current.clone(), 0),
                }
            })
            .collect())
    }
}

fn map_list_response(response: NextcloudList) -> NextcloudListResponse {
    NextcloudListResponse {
        content: response.items,
        total: response.total,
        page: usize::try_from(response.page).unwrap_or(usize::MAX),
        has_more: response.has_more,
    }
}

fn first_preview_file_id(items: &[NextcloudDavItem]) -> Option<u64> {
    items
        .iter()
        .find(|item| !item.is_directory && item.has_preview && is_playable(item))
        .map(|item| item.file_id)
}

fn map_directory_item(
    item: NextcloudDavItem,
    owner: UserId,
    server_id: &str,
) -> Option<DirectoryItem> {
    let item_type = if item.is_directory {
        ItemType::Playlist
    } else if is_playable(&item) {
        ItemType::Media
    } else {
        return None;
    };
    Some(DirectoryItem {
        name: item.name,
        item_type,
        target: ProviderTarget::nextcloud(item.path, item.file_id),
        size: (!item.is_directory).then_some(item.size),
        thumbnail: (!item.is_directory && item.has_preview).then(|| {
            DirectoryItemThumbnail::Nextcloud {
                server_id: server_id.to_string(),
                credential_owner_id: owner,
                file_id: item.file_id,
            }
        }),
        description: item.blurhash,
        modified_at: None,
        source_config: None,
    })
}

fn mark_playback_resources(result: &mut PlaybackResult, version: &str, expires_at: i64) {
    for (mode_name, info) in &mut result.playback_infos {
        for (media_index, media) in info.medias.iter_mut().enumerate() {
            if let PlaybackMediaProvider::Nextcloud(PlaybackNextcloudMedia::Refresh {
                credential_owner_id,
                server_id,
                path,
                file_id,
            }) = &media.provider
            {
                media.provider = PlaybackMediaProvider::Nextcloud(PlaybackNextcloudMedia::Proxy {
                    version: version.to_string(),
                    expires_at,
                    mode_name: mode_name.clone(),
                    media_index,
                    credential_owner_id: credential_owner_id.clone(),
                    server_id: server_id.clone(),
                    path: path.clone(),
                    file_id: *file_id,
                });
            }
        }
        for (subtitle_index, subtitle) in info.subtitles.iter_mut().enumerate() {
            if let PlaybackSubtitleProvider::Nextcloud(resource) = &mut subtitle.provider {
                resource.version = version.to_string();
                resource.expires_at = expires_at;
                resource.mode_name.clone_from(mode_name);
                resource.subtitle_index = subtitle_index;
            }
        }
    }
}

async fn discover_subtitles(
    auth: &AuthenticatedNextcloud,
    media_path: &str,
) -> Result<Vec<PlaybackSubtitle>, ProviderError> {
    let parent = parent_path(media_path);
    let mut page = 1;
    let mut subtitles = Vec::new();
    loop {
        let list = auth
            .client
            .list(&auth.username, &auth.app_password, &parent, page, 500)
            .await?;
        subtitles.extend(
            list.items
                .iter()
                .filter(|item| related_subtitle(media_path, item))
                .take(RELATED_SUBTITLE_LIMIT.saturating_sub(subtitles.len()))
                .map(|item| PlaybackSubtitle {
                    name: item.name.clone(),
                    language: subtitle_language(media_path, &item.name),
                    format: subtitle_format(&item.name).unwrap_or_default().to_string(),
                    provider: PlaybackSubtitleProvider::Nextcloud(PlaybackNextcloudSubtitle {
                        version: String::new(),
                        expires_at: 0,
                        mode_name: String::new(),
                        subtitle_index: 0,
                        path: item.path.clone(),
                    }),
                }),
        );
        if subtitles.len() >= RELATED_SUBTITLE_LIMIT || !list.has_more {
            break;
        }
        page = page.saturating_add(1);
    }
    Ok(subtitles)
}

fn parent_path(path: &str) -> String {
    path.rsplit_once('/')
        .map_or_else(String::new, |(parent, _)| {
            if parent.is_empty() {
                String::new()
            } else {
                parent.to_string()
            }
        })
}

fn related_subtitle(media_path: &str, item: &NextcloudDavItem) -> bool {
    !item.is_directory
        && subtitle_format(&item.name).is_some()
        && related_stem(
            media_path.rsplit('/').next().unwrap_or_default(),
            &item.name,
        )
}

fn related_stem(media_name: &str, subtitle_name: &str) -> bool {
    let media_stem = media_name
        .rsplit_once('.')
        .map_or(media_name, |(stem, _)| stem)
        .to_ascii_lowercase();
    let subtitle_stem = subtitle_name
        .rsplit_once('.')
        .map_or(subtitle_name, |(stem, _)| stem)
        .to_ascii_lowercase();
    subtitle_stem == media_stem
        || subtitle_stem
            .strip_prefix(&media_stem)
            .is_some_and(|suffix| suffix.starts_with(['.', '_', '-']))
}

fn subtitle_format(name: &str) -> Option<&str> {
    let extension = name.rsplit_once('.')?.1;
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "srt" | "vtt" | "ass" | "ssa" | "sub" | "ttml"
    )
    .then_some(extension)
}

fn subtitle_language(media_path: &str, subtitle_name: &str) -> String {
    let media_name = media_path.rsplit('/').next().unwrap_or_default();
    let media_stem = media_name
        .rsplit_once('.')
        .map_or(media_name, |(stem, _)| stem);
    let subtitle_stem = subtitle_name
        .rsplit_once('.')
        .map_or(subtitle_name, |(stem, _)| stem);
    subtitle_stem
        .strip_prefix(media_stem)
        .unwrap_or_default()
        .trim_start_matches(['.', '_', '-'])
        .to_string()
}

fn decode_target(
    target: Option<&ProviderTarget>,
) -> Result<Option<crate::models::NextcloudTarget>, ProviderError> {
    let Some(target) = target else {
        return Ok(None);
    };
    let ProviderTarget::Nextcloud(target) = target else {
        return Err(ProviderError::InvalidConfig(
            "Nextcloud target must use nextcloud payload".to_string(),
        ));
    };
    validate_path(&target.path)?;
    Ok(Some(target.clone()))
}

fn validate_playlist_source(source: &NextcloudPlaylistSource) -> Result<(), ProviderError> {
    match source {
        NextcloudPlaylistSource::Folder { path } => validate_path(path),
        NextcloudPlaylistSource::Favorites => Ok(()),
        NextcloudPlaylistSource::Search { path, query } => {
            validate_path(path)?;
            if query.trim().chars().count() < 3 {
                return Err(ProviderError::InvalidConfig(
                    "Nextcloud search query must contain at least 3 characters".to_string(),
                ));
            }
            Ok(())
        }
    }
}

fn source_folder(source: &NextcloudPlaylistSource) -> &str {
    match source {
        NextcloudPlaylistSource::Folder { path } | NextcloudPlaylistSource::Search { path, .. } => {
            path
        }
        NextcloudPlaylistSource::Favorites => "",
    }
}

fn validate_path(path: &str) -> Result<(), ProviderError> {
    if path.split('/').any(|segment| segment == "..") {
        return Err(ProviderError::InvalidConfig(
            "Nextcloud path must not contain traversal".to_string(),
        ));
    }
    Ok(())
}

fn validate_file_path(path: &str) -> Result<(), ProviderError> {
    validate_path(path)?;
    if path.trim_matches('/').is_empty() {
        return Err(ProviderError::InvalidConfig(
            "Nextcloud media path must identify a file".to_string(),
        ));
    }
    Ok(())
}

fn is_playable(item: &NextcloudDavItem) -> bool {
    item.content_type
        .as_deref()
        .is_some_and(|mime| mime.starts_with("video/") || mime.starts_with("audio/"))
        || matches!(
            item.name
                .rsplit('.')
                .next()
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some(
                "mp4"
                    | "mkv"
                    | "avi"
                    | "mov"
                    | "flv"
                    | "webm"
                    | "m4v"
                    | "wmv"
                    | "m3u8"
                    | "mp3"
                    | "flac"
                    | "wav"
                    | "aac"
                    | "m4a"
                    | "ogg"
            )
        )
}

fn enough_for_next(media: &[DirectoryItem], target: &ProviderTarget, play_mode: PlayMode) -> bool {
    match play_mode {
        PlayMode::Sequential | PlayMode::RepeatAll => media
            .iter()
            .position(|item| &item.target == target)
            .is_some_and(|index| media.get(index + 1).is_some()),
        PlayMode::Shuffle => media.len() >= SHUFFLE_LIMIT,
        PlayMode::RepeatOne => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_only_related_subtitle_names() {
        assert!(related_stem("Movie.mkv", "Movie.zh-CN.ass"));
        assert!(related_stem("Movie.mkv", "movie_en.srt"));
        assert!(!related_stem("Movie.mkv", "Movie 2.srt"));
        assert_eq!(subtitle_format("Movie.zh-CN.ass"), Some("ass"));
        assert_eq!(subtitle_format("Movie.txt"), None);
        assert_eq!(
            subtitle_language("/Videos/Movie.mkv", "Movie.zh-CN.ass"),
            "zh-CN"
        );
    }
}
