//! Cloudreve v4 media provider.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use futures::{stream, StreamExt};
use sha2::{Digest, Sha256};

use super::{
    DynamicBrowsePathSegment, DynamicListQuery, DynamicListResult, DynamicPagination,
    DynamicPlaylistItem, DynamicPlaylistItemThumbnail, DynamicPlaylistProvider, ItemType,
    MediaProvider, NextPlayItem, PlaybackInfo, PlaybackProxyAutoPolicy, PlaybackProxyAutoReason,
    PlaybackProxyPolicy, PlaybackResult, ProviderContext, ProviderCredentialDependency,
    ProviderError, SourceConfig, SourceCover,
};
use crate::models::media::{
    PlaybackCloudreveMedia, PlaybackCloudreveSubtitle, PlaybackMedia, PlaybackMediaProvider,
    PlaybackSubtitle, PlaybackSubtitleProvider,
};
use crate::models::{
    normalize_provider_instance_name, normalize_provider_instance_name_owned,
    CloudreveMediaSourceConfig, CloudrevePlaylistSourceConfig, MediaSourceConfig, PlayMode,
    PlaylistSourceConfig, ProviderCredential, ProviderTarget, UserId, UserProviderCredential,
};
use crate::repository::UserProviderCredentialRepository;
use synctv_media_providers::cloudreve::{CloudreveClient, CloudreveFile, CloudreveUser};

const DYNAMIC_PAGE_SIZE: usize = 50;
const DYNAMIC_MAX_ITEMS: usize = 200;
const RELATED_SUBTITLE_LIMIT: usize = 16;
const THUMBNAIL_CONCURRENCY: usize = 8;
const PLAYBACK_CACHE_TTL: Duration = Duration::from_mins(15);

#[derive(Debug, Clone, Copy)]
pub struct CloudreveHlsResourceRequest<'a> {
    pub version: &'a str,
    pub mode_name: &'a str,
    pub media_index: usize,
    pub target_url: &'a str,
    pub is_manifest: bool,
    pub range_header: Option<&'a str>,
}

#[derive(Default)]
struct SequentialMediaScan {
    first: Option<DynamicPlaylistItem>,
    found_current: bool,
}

impl SequentialMediaScan {
    fn observe(
        &mut self,
        item: DynamicPlaylistItem,
        current_target: &ProviderTarget,
    ) -> Option<DynamicPlaylistItem> {
        if self.first.is_none() {
            self.first = Some(item.clone());
        }
        if self.found_current {
            return Some(item);
        }
        if &item.target == current_target {
            self.found_current = true;
        }
        None
    }

    fn finish(self, repeat_all: bool) -> Option<DynamicPlaylistItem> {
        (repeat_all && self.found_current)
            .then_some(self.first)
            .flatten()
    }
}

#[derive(Debug, Clone)]
pub struct CloudreveBind {
    pub id: i64,
    pub server_id: String,
    pub host: String,
    pub email: String,
    pub created_at: i64,
    pub provider_instance_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CloudreveListResponse {
    pub content: Vec<CloudreveFile>,
    pub total: u64,
    pub pagination: DynamicPagination,
}

pub struct CloudreveProvider {
    http_client: reqwest::Client,
    credential_repo: Option<Arc<UserProviderCredentialRepository>>,
}

impl CloudreveProvider {
    pub const NAME: &'static str = "cloudreve";

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
            ProviderError::Internal("Cloudreve credential repository is unavailable".to_string())
        })
    }

    fn credential_repo_or<'a>(
        &'a self,
        fallback: Option<&'a UserProviderCredentialRepository>,
    ) -> Result<&'a UserProviderCredentialRepository, ProviderError> {
        self.credential_repo.as_deref().or(fallback).ok_or_else(|| {
            ProviderError::Internal("Cloudreve credential repository is unavailable".to_string())
        })
    }

    fn client(&self, host: &str) -> Result<CloudreveClient, ProviderError> {
        CloudreveClient::with_http_client(host, self.http_client.clone()).map_err(Into::into)
    }

    #[must_use]
    pub fn credential_server_id_for_instance(
        host: &str,
        provider_instance_name: Option<&str>,
    ) -> String {
        let host = host.trim().trim_end_matches('/');
        let instance_name =
            normalize_provider_instance_name(provider_instance_name).unwrap_or_default();
        hex::encode(Sha256::digest(
            format!("{host}\n{instance_name}").as_bytes(),
        ))
    }

    fn validate_path(path: &str) -> Result<(), ProviderError> {
        let trimmed = path.trim();
        if trimmed.split('/').any(|segment| segment == "..") {
            return Err(ProviderError::InvalidConfig(
                "Cloudreve path must not contain traversal".to_string(),
            ));
        }
        Ok(())
    }

    fn validate_file_path(path: &str) -> Result<(), ProviderError> {
        Self::validate_path(path)?;
        if path.trim().is_empty() || path.trim() == "/" {
            return Err(ProviderError::InvalidConfig(
                "Cloudreve media path must identify a file".to_string(),
            ));
        }
        Ok(())
    }

    fn normalize_path(path: &str) -> String {
        let path = path.trim();
        if path.is_empty() || path == "/" {
            "cloudreve://my/".to_string()
        } else {
            path.to_string()
        }
    }

    async fn credential(
        &self,
        user_id: UserId,
        server_id: &str,
    ) -> Result<(String, String, String, Option<String>), ProviderError> {
        self.credential_with_repo(self.credential_repo()?, user_id, server_id)
            .await
    }

    async fn credential_with_repo(
        &self,
        repo: &UserProviderCredentialRepository,
        user_id: UserId,
        server_id: &str,
    ) -> Result<(String, String, String, Option<String>), ProviderError> {
        let credential = repo
            .get_by_provider_and_server(user_id, Self::NAME, server_id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
            .ok_or(ProviderError::CredentialRequired)?;
        let instance_name = credential.provider_instance_name;
        match credential.credential_data {
            ProviderCredential::Cloudreve {
                host,
                email,
                password,
            } => Ok((host, email, password, instance_name)),
            _ => Err(ProviderError::InvalidCredentialType),
        }
    }

    async fn authenticated(
        &self,
        user_id: UserId,
        server_id: &str,
    ) -> Result<(CloudreveClient, String, Option<String>), ProviderError> {
        let (host, email, password, instance_name) = self.credential(user_id, server_id).await?;
        let client = self.client(&host)?;
        let token = client.login(&email, &password).await?;
        Ok((client, token.access_token, instance_name))
    }

    async fn authenticated_with_repo(
        &self,
        repo: &UserProviderCredentialRepository,
        user_id: UserId,
        server_id: &str,
    ) -> Result<(CloudreveClient, String, Option<String>), ProviderError> {
        let (host, email, password, instance_name) =
            self.credential_with_repo(repo, user_id, server_id).await?;
        let client = self.client(&host)?;
        let token = client.login(&email, &password).await?;
        Ok((client, token.access_token, instance_name))
    }

    async fn thumbnail_urls(
        client: &CloudreveClient,
        token: &str,
        paths: Vec<Option<String>>,
    ) -> Vec<Option<String>> {
        stream::iter(paths.into_iter().map(|path| async move {
            let path = path?;
            client
                .thumbnail(token, &path)
                .await
                .ok()
                .map(|thumbnail| thumbnail.url)
                .filter(|url| !url.trim().is_empty())
        }))
        .buffered(THUMBNAIL_CONCURRENCY)
        .collect()
        .await
    }

    pub async fn login_and_persist(
        &self,
        user_id: UserId,
        host: String,
        email: String,
        password: String,
        provider_instance_name: Option<String>,
    ) -> Result<(String, CloudreveUser), ProviderError> {
        if email.trim().is_empty() || password.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Cloudreve email and password are required".to_string(),
            ));
        }
        let client = self.client(&host)?;
        let token = client.login(&email, &password).await?;
        let user = client.me(&token.access_token).await?;
        let provider_instance_name = normalize_provider_instance_name_owned(provider_instance_name);
        let server_id =
            Self::credential_server_id_for_instance(&host, provider_instance_name.as_deref());
        let now = Utc::now();
        let credential = UserProviderCredential {
            id: 0,
            user_id,
            provider: Self::NAME.to_string(),
            server_id: server_id.clone(),
            provider_instance_name,
            credential_data: ProviderCredential::Cloudreve {
                host,
                email,
                password,
            },
            expires_at: None,
            created_at: now,
            updated_at: now,
        };
        self.credential_repo()?
            .upsert_by_user_provider_server(&credential)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?;
        Ok((server_id, user))
    }

    pub async fn list(
        &self,
        user_id: UserId,
        server_id: &str,
        path: &str,
        pagination: DynamicPagination,
        per_page: u32,
    ) -> Result<(CloudreveListResponse, Option<String>), ProviderError> {
        Self::validate_path(path)?;
        let path = Self::normalize_path(path);
        let (client, token, instance_name) = self.authenticated(user_id, server_id).await?;
        let (page, cursor) = match &pagination {
            DynamicPagination::Page { page } => (
                u32::try_from((*page).max(1)).map_err(|_| {
                    ProviderError::InvalidConfig("Cloudreve page exceeds u32::MAX".to_string())
                })?,
                None,
            ),
            DynamicPagination::Cursor { cursor } => (1, cursor.as_deref()),
        };
        let mut response = client.list(&token, &path, page, cursor, per_page).await?;
        let paths = response
            .files
            .iter()
            .map(|file| (!file.is_dir()).then(|| file.path.clone()))
            .collect();
        let thumbnails = Self::thumbnail_urls(&client, &token, paths).await;
        for (file, thumbnail) in response.files.iter_mut().zip(thumbnails) {
            file.thumbnail_url = thumbnail;
        }
        let total = response
            .pagination
            .as_ref()
            .map_or(response.files.len() as u64, |value| value.total_items);
        let pagination = response.pagination.as_ref().map_or(
            DynamicPagination::Page {
                page: usize::try_from(page).unwrap_or(usize::MAX),
            },
            |value| {
                if value.is_cursor {
                    DynamicPagination::Cursor {
                        cursor: Some(value.next_token.clone()).filter(|token| !token.is_empty()),
                    }
                } else {
                    DynamicPagination::Page {
                        page: usize::try_from(page).unwrap_or(usize::MAX),
                    }
                }
            },
        );
        Ok((
            CloudreveListResponse {
                content: response.files,
                total,
                pagination,
            },
            instance_name,
        ))
    }

    pub async fn search(
        &self,
        user_id: UserId,
        server_id: &str,
        keywords: &str,
        offset: u64,
    ) -> Result<(CloudreveListResponse, Option<String>), ProviderError> {
        if keywords.trim().is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Cloudreve search keywords are required".to_string(),
            ));
        }
        let (client, token, instance_name) = self.authenticated(user_id, server_id).await?;
        let mut response = client.search(&token, keywords, offset).await?;
        let paths = response
            .hits
            .iter()
            .map(|hit| (!hit.file.is_dir()).then(|| hit.file.path.clone()))
            .collect();
        let thumbnails = Self::thumbnail_urls(&client, &token, paths).await;
        for (hit, thumbnail) in response.hits.iter_mut().zip(thumbnails) {
            hit.file.thumbnail_url = thumbnail;
        }
        Ok((
            CloudreveListResponse {
                total: response.total,
                content: response.hits.into_iter().map(|hit| hit.file).collect(),
                pagination: DynamicPagination::Page { page: 1 },
            },
            instance_name,
        ))
    }

    pub async fn me(
        &self,
        user_id: UserId,
        server_id: &str,
    ) -> Result<(CloudreveUser, Option<String>), ProviderError> {
        let (client, token, instance_name) = self.authenticated(user_id, server_id).await?;
        Ok((client.me(&token).await?, instance_name))
    }

    pub async fn delete_credential(
        &self,
        user_id: UserId,
        server_id: &str,
    ) -> Result<bool, ProviderError> {
        let Some(credential) = self
            .credential_repo()?
            .get_by_provider_and_server(user_id, Self::NAME, server_id)
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

    pub async fn list_binds(
        &self,
        user_id: UserId,
        provider_instance_name: Option<&str>,
    ) -> Result<Vec<CloudreveBind>, ProviderError> {
        let requested = provider_instance_name
            .map(str::trim)
            .filter(|name| !name.is_empty());
        self.credential_repo()?
            .get_readable_by_provider(user_id, Self::NAME)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
            .into_iter()
            .filter(|credential| {
                requested
                    .is_none_or(|name| credential.provider_instance_name.as_deref() == Some(name))
            })
            .map(|credential| {
                let ProviderCredential::Cloudreve { host, email, .. } = credential.credential_data
                else {
                    return Err(ProviderError::InvalidCredentialType);
                };
                Ok(CloudreveBind {
                    id: credential.id,
                    server_id: credential.server_id,
                    host,
                    email,
                    created_at: credential.created_at.timestamp(),
                    provider_instance_name: credential.provider_instance_name,
                })
            })
            .collect()
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
        if super::playback_media_is_hls(mode_name, media) {
            return Err(ProviderError::InvalidConfig(
                "Cloudreve resource request references an HLS manifest".to_string(),
            ));
        }
        let PlaybackMediaProvider::Cloudreve(PlaybackCloudreveMedia::Direct { url, headers }) =
            &media.provider
        else {
            return Err(ProviderError::InvalidConfig(
                "Cloudreve cached playback resource is invalid".to_string(),
            ));
        };
        Ok(super::PlaybackTransportAction::FetchAndForward {
            url: url.clone(),
            headers: headers.clone(),
            range_header: range_header.map(ToString::to_string),
            proxy_strategy: super::PlaybackResourceProxyStrategy::SliceCache,
        })
    }

    pub async fn get_hls_manifest(
        &self,
        store: Option<&Arc<dyn super::ProviderStore>>,
        version: &str,
        mode_name: &str,
        media_index: usize,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let media = versioned
            .result
            .playback_infos
            .get(mode_name)
            .and_then(|info| info.medias.get(media_index))
            .ok_or(ProviderError::NotFound)?;
        if !super::playback_media_is_hls(mode_name, media) {
            return Err(ProviderError::InvalidConfig(
                "Cloudreve HLS manifest request references a regular media resource".to_string(),
            ));
        }
        let PlaybackMediaProvider::Cloudreve(PlaybackCloudreveMedia::Direct { url, headers }) =
            &media.provider
        else {
            return Err(ProviderError::InvalidConfig(
                "Cloudreve cached HLS manifest is invalid".to_string(),
            ));
        };
        Ok(super::PlaybackTransportAction::M3u8RewriteWithSource {
            url: url.clone(),
            headers: headers.clone(),
            source_url: super::playback_transport::dynamic_hls_source_url(url)?,
        })
    }

    pub async fn get_hls_resource(
        &self,
        store: Option<&Arc<dyn super::ProviderStore>>,
        request: CloudreveHlsResourceRequest<'_>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, request.version, request_context)
                .await?;
        let media = versioned
            .result
            .playback_infos
            .get(request.mode_name)
            .and_then(|info| info.medias.get(request.media_index))
            .ok_or(ProviderError::NotFound)?;
        if !super::playback_media_is_hls(request.mode_name, media) {
            return Err(ProviderError::InvalidConfig(
                "Cloudreve HLS child request references a regular media resource".to_string(),
            ));
        }
        let PlaybackMediaProvider::Cloudreve(PlaybackCloudreveMedia::Direct { url, headers }) =
            &media.provider
        else {
            return Err(ProviderError::InvalidConfig(
                "Cloudreve cached HLS resource is invalid".to_string(),
            ));
        };
        super::playback_transport::transport_action_for_dynamic_hls_target(
            url,
            url,
            headers.clone(),
            request.target_url,
            request.is_manifest,
            request.range_header,
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
        let PlaybackSubtitleProvider::Cloudreve(PlaybackCloudreveSubtitle::Direct {
            url,
            headers,
            ..
        }) = &subtitle.provider
        else {
            return Err(ProviderError::InvalidConfig(
                "Cloudreve cached subtitle resource is invalid".to_string(),
            ));
        };
        Ok(super::PlaybackTransportAction::FetchAndForward {
            url: url.clone(),
            headers: headers.clone(),
            range_header: None,
            proxy_strategy: super::PlaybackResourceProxyStrategy::FullResponseCache,
        })
    }

    fn config(
        source_config: &MediaSourceConfig,
    ) -> Result<&CloudreveMediaSourceConfig, ProviderError> {
        match source_config {
            MediaSourceConfig::Cloudreve(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "Expected Cloudreve media source_config".to_string(),
            )),
        }
    }

    fn playlist_config(
        source_config: &PlaylistSourceConfig,
    ) -> Result<&CloudrevePlaylistSourceConfig, ProviderError> {
        match source_config {
            PlaylistSourceConfig::Cloudreve(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "Expected Cloudreve playlist source_config".to_string(),
            )),
        }
    }

    fn source_parts(source_config: SourceConfig<'_>) -> Result<(&str, &str), ProviderError> {
        match source_config {
            SourceConfig::Media(config) => {
                let config = Self::config(config)?;
                Ok((&config.server_id, &config.path))
            }
            SourceConfig::DynamicPlaylist(config) => {
                let config = Self::playlist_config(config)?;
                Ok((&config.server_id, &config.path))
            }
        }
    }

    fn encode_target(relative_path: &str) -> Result<ProviderTarget, ProviderError> {
        if relative_path.trim().is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Cloudreve target relative_path cannot be empty".to_string(),
            ));
        }
        Ok(ProviderTarget::cloudreve(relative_path.to_string()))
    }

    fn decode_target(target: Option<&ProviderTarget>) -> Result<Option<String>, ProviderError> {
        let Some(target) = target else {
            return Ok(None);
        };
        let ProviderTarget::Cloudreve(target) = target else {
            return Err(ProviderError::InvalidConfig(
                "Cloudreve target must use cloudreve payload".to_string(),
            ));
        };
        if target.relative_path.trim().is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Cloudreve target relative_path cannot be empty".to_string(),
            ));
        }
        Self::validate_path(&target.relative_path)?;
        Ok(Some(target.relative_path.clone()))
    }

    fn join_path(base: &str, relative: &str) -> String {
        format!(
            "{}/{}",
            base.trim_end_matches('/'),
            relative.trim_start_matches('/')
        )
    }

    fn relative_path(base: &str, full_path: &str) -> Option<String> {
        let base = base.trim_end_matches('/');
        let relative = full_path.strip_prefix(base)?;
        if relative.is_empty() {
            Some("/".to_string())
        } else if relative.starts_with('/') {
            Some(relative.to_string())
        } else {
            None
        }
    }

    fn item_type(file: &CloudreveFile) -> Option<ItemType> {
        if file.is_dir() {
            return Some(ItemType::Playlist);
        }
        matches!(
            file.name
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
        .then_some(ItemType::Media)
    }

    fn is_related_subtitle(video_name: &str, candidate: &CloudreveFile) -> bool {
        if candidate.is_dir() {
            return false;
        }
        let Some((candidate_stem, extension)) = candidate.name.rsplit_once('.') else {
            return false;
        };
        if !matches!(
            extension.to_ascii_lowercase().as_str(),
            "srt" | "vtt" | "ass" | "ssa"
        ) {
            return false;
        }
        let video_stem = video_name
            .rsplit_once('.')
            .map_or(video_name, |(stem, _)| stem);
        candidate_stem == video_stem || candidate_stem.starts_with(&format!("{video_stem}."))
    }

    async fn related_subtitles(
        client: &CloudreveClient,
        token: &str,
        provider_instance_name: Option<&str>,
        server_id: &str,
        path: &str,
    ) -> Vec<PlaybackSubtitle> {
        let (parent, video_name) = path.rsplit_once('/').unwrap_or(("cloudreve://my", path));
        let parent = format!("{}/", parent.trim_end_matches('/'));
        let Ok(listing) = client.list(token, &parent, 1, None, 200).await else {
            return Vec::new();
        };
        let mut subtitles = Vec::new();
        for item in listing
            .files
            .into_iter()
            .filter(|item| Self::is_related_subtitle(video_name, item))
            .take(RELATED_SUBTITLE_LIMIT)
        {
            let Ok(response) = client.file_url(token, &item.path).await else {
                continue;
            };
            let response_expires_at = response.expires;
            let Some(entry) = response.urls.into_iter().next() else {
                continue;
            };
            let expire_at = response_expires_at.into_iter().chain(entry.expire_at).min();
            if expire_at.is_some_and(|value| value <= crate::SystemClock.now()) {
                continue;
            }
            let url = entry.url;
            let format = item
                .name
                .rsplit('.')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            subtitles.push(PlaybackSubtitle {
                name: item.name,
                language: String::new(),
                format,
                p2p_swarm_id: Some(super::provider_p2p_swarm_id(
                    Self::NAME,
                    provider_instance_name,
                    "subtitle",
                    &format!("server:{server_id}:path:{}", item.path),
                )),
                provider: PlaybackSubtitleProvider::Cloudreve(PlaybackCloudreveSubtitle::Direct {
                    url,
                    headers: HashMap::new(),
                    expire_at,
                }),
            });
        }
        subtitles
    }

    fn next_item(
        base: &CloudrevePlaylistSourceConfig,
        item: &DynamicPlaylistItem,
    ) -> Result<NextPlayItem, ProviderError> {
        let relative = Self::decode_target(Some(&item.target))?.ok_or_else(|| {
            ProviderError::InvalidConfig("Cloudreve item target is required".to_string())
        })?;
        Ok(NextPlayItem {
            name: item.name.clone(),
            item_type: item.item_type,
            source_config: MediaSourceConfig::Cloudreve(CloudreveMediaSourceConfig {
                server_id: base.server_id.clone(),
                path: Self::join_path(&base.path, &relative),
                proxy_mode: base.proxy_mode,
            }),
            target: item.target.clone(),
        })
    }

    fn next_pagination(result: &DynamicListResult) -> Option<DynamicPagination> {
        if !result.has_more {
            return None;
        }
        match &result.pagination {
            DynamicPagination::Page { page } => Some(DynamicPagination::Page {
                page: page.saturating_add(1),
            }),
            DynamicPagination::Cursor {
                cursor: Some(cursor),
            } => Some(DynamicPagination::Cursor {
                cursor: Some(cursor.clone()),
            }),
            DynamicPagination::Cursor { cursor: None } => None,
        }
    }

    fn format(path: &str) -> String {
        match path
            .rsplit('.')
            .next()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("m3u8") => "hls".to_string(),
            Some(extension) if !extension.contains('/') => extension.to_string(),
            _ => "video".to_string(),
        }
    }
}

fn cloudreve_route_selection(
    proxy_mode: crate::models::PlaybackProxyMode,
) -> super::PlaybackRouteSelection {
    use crate::models::PlaybackProxyMode;

    match proxy_mode {
        PlaybackProxyMode::Auto | PlaybackProxyMode::Only => {
            super::PlaybackRouteSelection::PROXY_ONLY
        }
        PlaybackProxyMode::Prefer => super::PlaybackRouteSelection::PROXY_PREFERRED,
        PlaybackProxyMode::DirectPrefer => super::PlaybackRouteSelection::DIRECT_PREFERRED,
        PlaybackProxyMode::DirectOnly => super::PlaybackRouteSelection::DIRECT_ONLY,
    }
}

fn mark_cloudreve_playback_resources(
    result: &mut PlaybackResult,
    version: &str,
    expires_at: i64,
    proxy_mode: crate::models::PlaybackProxyMode,
) {
    let original_default_mode = result.default_mode.clone();
    let mut default_selection = super::PlaybackRouteSelection::DIRECT_ONLY;
    let original_modes = result
        .playback_infos
        .iter()
        .map(|(mode_name, info)| (mode_name.clone(), info.clone()))
        .collect::<Vec<_>>();
    let mut generated = std::collections::HashMap::new();

    for (mode_name, original_info) in original_modes {
        if original_info.medias.is_empty() || mode_name.starts_with("proxy_") {
            continue;
        }
        let selection = cloudreve_route_selection(proxy_mode);
        if mode_name == original_default_mode {
            default_selection = selection;
        }
        if selection.direct {
            generated.insert(mode_name.clone(), original_info.clone());
        }
        if !selection.proxy {
            continue;
        }
        let proxy_mode_name = format!("proxy_{mode_name}");

        let mut proxy_info = original_info.clone();
        proxy_info.medias = original_info
            .medias
            .iter()
            .enumerate()
            .filter(|(_, media)| {
                matches!(
                    media.provider,
                    PlaybackMediaProvider::Cloudreve(PlaybackCloudreveMedia::Direct { .. })
                )
            })
            .map(|(media_index, media)| {
                let mut proxy = media.clone();
                proxy.expire_at = chrono::DateTime::from_timestamp(expires_at, 0);
                proxy.provider = PlaybackMediaProvider::Cloudreve(
                    if super::playback_media_is_hls(&mode_name, media) {
                        PlaybackCloudreveMedia::ProxyHlsManifest {
                            version: version.to_string(),
                            expires_at,
                            mode_name: mode_name.clone(),
                            media_index,
                        }
                    } else {
                        PlaybackCloudreveMedia::ProxyStream {
                            version: version.to_string(),
                            expires_at,
                            mode_name: mode_name.clone(),
                            media_index,
                        }
                    },
                );
                proxy
            })
            .collect();
        proxy_info.subtitles = original_info
            .subtitles
            .iter()
            .enumerate()
            .filter(|(_, subtitle)| {
                matches!(
                    subtitle.provider,
                    PlaybackSubtitleProvider::Cloudreve(PlaybackCloudreveSubtitle::Direct { .. })
                )
            })
            .map(|(subtitle_index, subtitle)| PlaybackSubtitle {
                name: subtitle.name.clone(),
                language: subtitle.language.clone(),
                format: subtitle.format.clone(),
                p2p_swarm_id: subtitle.p2p_swarm_id.clone(),
                provider: PlaybackSubtitleProvider::Cloudreve(PlaybackCloudreveSubtitle::Proxy {
                    version: version.to_string(),
                    expires_at,
                    mode_name: mode_name.clone(),
                    subtitle_index,
                }),
            })
            .collect();
        if !proxy_info.medias.is_empty() {
            generated.insert(proxy_mode_name, proxy_info);
        }
    }

    result.playback_infos = generated;
    let proxy_default_mode = format!("proxy_{original_default_mode}");
    let direct_default_available = result.playback_infos.contains_key(&original_default_mode);
    let proxy_default_available = result.playback_infos.contains_key(&proxy_default_mode);
    result.default_mode = if default_selection.prefer_proxy && proxy_default_available {
        proxy_default_mode
    } else if direct_default_available {
        original_default_mode
    } else if proxy_default_available {
        proxy_default_mode
    } else {
        result
            .playback_infos
            .keys()
            .min()
            .cloned()
            .unwrap_or_default()
    };
}

#[async_trait]
impl MediaProvider for CloudreveProvider {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn playback_proxy_policy(
        &self,
        source_config: SourceConfig<'_>,
    ) -> Result<Option<PlaybackProxyPolicy>, ProviderError> {
        let current_mode = match source_config {
            SourceConfig::Media(MediaSourceConfig::Cloudreve(config)) => config.proxy_mode,
            SourceConfig::DynamicPlaylist(PlaylistSourceConfig::Cloudreve(config)) => {
                config.proxy_mode
            }
            _ => {
                return Err(ProviderError::InvalidConfig(
                    "Cloudreve requires Cloudreve source_config".to_string(),
                ));
            }
        };
        Ok(Some(PlaybackProxyPolicy::all_modes(
            current_mode,
            vec![PlaybackProxyAutoPolicy::new(
                "file",
                crate::models::PlaybackProxyMode::Only,
                PlaybackProxyAutoReason::SignedResource,
            )],
        )))
    }

    fn set_playback_proxy_mode(
        &self,
        source_config: &mut MediaSourceConfig,
        mode: crate::models::PlaybackProxyMode,
    ) -> Result<(), ProviderError> {
        let MediaSourceConfig::Cloudreve(config) = source_config else {
            return Err(ProviderError::InvalidConfig(
                "Cloudreve requires Cloudreve media source_config".to_string(),
            ));
        };
        config.proxy_mode = mode;
        Ok(())
    }

    async fn generate_playback(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &MediaSourceConfig,
    ) -> Result<PlaybackResult, ProviderError> {
        let config = Self::config(source_config)?;
        Self::validate_file_path(&config.path)?;
        let user_id = *ctx
            .credential_owner_or_user_id()
            .ok_or(ProviderError::CredentialRequired)?;
        let repo = self.credential_repo_or(ctx.credential_repo)?;
        let (client, token, instance_name) = self
            .authenticated_with_repo(repo, user_id, &config.server_id)
            .await?;
        let (response, thumbnail, subtitles) = tokio::join!(
            client.file_url(&token, &config.path),
            client.thumbnail(&token, &config.path),
            Self::related_subtitles(
                &client,
                &token,
                instance_name.as_deref(),
                &config.server_id,
                &config.path,
            ),
        );
        let response = response?;
        let response_expires_at = response.expires;
        let first_url = response
            .urls
            .into_iter()
            .next()
            .ok_or(ProviderError::NotFound)?;
        let expire_at = response_expires_at
            .into_iter()
            .chain(first_url.expire_at)
            .min();
        if expire_at.is_some_and(|value| value <= crate::SystemClock.now()) {
            return Err(ProviderError::ApiError(
                "Cloudreve returned an expired media URL".to_string(),
            ));
        }
        let url = (!first_url.url.trim().is_empty())
            .then_some(first_url.url)
            .ok_or(ProviderError::NotFound)?;
        let name = config
            .path
            .rsplit('/')
            .next()
            .unwrap_or("Cloudreve media")
            .to_string();
        let media = PlaybackMedia {
            name,
            format: Self::format(&config.path),
            expire_at,
            metadata: None,
            p2p_swarm_id: Some(super::provider_p2p_swarm_id(
                Self::NAME,
                instance_name.as_deref(),
                "media",
                &format!("server:{}:path:{}", config.server_id, config.path),
            )),
            provider: PlaybackMediaProvider::Cloudreve(PlaybackCloudreveMedia::Direct {
                url,
                headers: HashMap::new(),
            }),
        };
        let result = PlaybackResult {
            playback_infos: HashMap::from([(
                "direct".to_string(),
                PlaybackInfo {
                    thumbnail: thumbnail.ok().map(|value| value.url),
                    medias: vec![media],
                    default_media_index: Some(0),
                    subtitles,
                    default_subtitle_index: None,
                    danmakus: Vec::new(),
                    default_danmaku_index: None,
                },
            )]),
            default_mode: "direct".to_string(),
            provider: crate::models::SourceProvider::Cloudreve,
            provider_instance_name: instance_name,
            duration_seconds: None,
            playback_kind: Some(crate::models::PlaybackKind::Regular),
            metadata: None,
        };
        let proxy_mode = config.proxy_mode;
        let result = super::cached_versioned_playback_or_fill(
            Self::NAME,
            &format!(
                "playback:{user_id}:{}:room:{}:{}",
                config.server_id,
                ctx.room_id
                    .map_or_else(|| "none".to_string(), |room| room.to_string()),
                config.path
            ),
            PLAYBACK_CACHE_TTL,
            ctx,
            |result, version, expires_at| {
                mark_cloudreve_playback_resources(result, version, expires_at, proxy_mode);
            },
            || async { Ok(result) },
        )
        .await?;
        super::filter_playback_routes_by_client(result, proxy_mode, ctx.playback_client_profile())
    }

    async fn validate_source_config(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<(), ProviderError> {
        let (server_id, path) = Self::source_parts(source_config)?;
        match source_config {
            SourceConfig::Media(_) => Self::validate_file_path(path)?,
            SourceConfig::DynamicPlaylist(_) => Self::validate_path(path)?,
        }
        if server_id.trim().is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Cloudreve server_id is required".to_string(),
            ));
        }
        let credential_owner_id = ctx.credential_owner_id().ok_or_else(|| {
            ProviderError::Internal(
                "credential_owner_id is unavailable in ProviderContext".to_string(),
            )
        })?;
        let credential = self
            .credential_repo_or(ctx.credential_repo)?
            .get_by_provider_and_server(*credential_owner_id, Self::NAME, server_id)
            .await
            .map_err(|error| {
                ProviderError::Internal(format!(
                    "Failed to verify Cloudreve credential reference: {error}"
                ))
            })?;
        if credential.is_none() {
            return Err(ProviderError::CredentialNotFound(format!(
                "Referenced Cloudreve credential not found for server_id '{server_id}'"
            )));
        }
        Ok(())
    }

    async fn source_cover(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<Option<SourceCover>, ProviderError> {
        let (server_id, path) = Self::source_parts(source_config)?;
        let user_id = *ctx
            .credential_owner_or_user_id()
            .ok_or(ProviderError::CredentialRequired)?;
        let repo = self.credential_repo_or(ctx.credential_repo)?;
        let (client, token, _) = self
            .authenticated_with_repo(repo, user_id, server_id)
            .await?;
        let thumbnail_path = match source_config {
            SourceConfig::Media(_) => Some(path.to_string()),
            SourceConfig::DynamicPlaylist(_) => client
                .list(&token, path, 1, None, 20)
                .await
                .ok()
                .and_then(|listing| {
                    listing
                        .files
                        .into_iter()
                        .find(|file| Self::item_type(file) == Some(ItemType::Media))
                        .map(|file| file.path)
                }),
        };
        let Some(thumbnail_path) = thumbnail_path else {
            return Ok(None);
        };
        Ok(client
            .thumbnail(&token, &thumbnail_path)
            .await
            .ok()
            .filter(|thumbnail| !thumbnail.url.trim().is_empty())
            .map(|thumbnail| SourceCover::Url { url: thumbnail.url }))
    }

    fn as_dynamic_playlist_provider(&self) -> Option<&dyn DynamicPlaylistProvider> {
        Some(self)
    }

    fn credential_dependencies(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<Vec<ProviderCredentialDependency>, ProviderError> {
        let (server_id, _) = Self::source_parts(source_config)?;
        let user_id = ctx
            .credential_owner_or_user_id()
            .ok_or(ProviderError::CredentialRequired)?;
        Ok(vec![ProviderCredentialDependency::new(
            crate::models::SourceProvider::Cloudreve,
            *user_id,
            server_id.to_string(),
        )])
    }
}

#[async_trait]
impl DynamicPlaylistProvider for CloudreveProvider {
    async fn list_playlist(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: Option<&ProviderTarget>,
        query: DynamicListQuery,
    ) -> Result<DynamicListResult, ProviderError> {
        let config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;
        let base = Self::playlist_config(config)?;
        let user_id = *ctx
            .credential_owner_or_user_id()
            .ok_or(ProviderError::CredentialRequired)?;
        let repo = self.credential_repo_or(ctx.credential_repo)?;
        let (client, token, _) = self
            .authenticated_with_repo(repo, user_id, &base.server_id)
            .await?;
        let relative = Self::decode_target(target)?;
        let path = relative.as_deref().map_or_else(
            || base.path.clone(),
            |relative| Self::join_path(&base.path, relative),
        );
        let path = Self::normalize_path(&path);
        let page_size = u32::try_from(query.page_size.max(1)).map_err(|_| {
            ProviderError::InvalidConfig("Cloudreve page size exceeds u32::MAX".to_string())
        })?;

        let (mut files, pagination_result, has_more) = if let Some(search) = query
            .search
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let DynamicPagination::Page { page } = query.pagination else {
                return Err(ProviderError::InvalidConfig(
                    "Cloudreve search requires page pagination".to_string(),
                ));
            };
            let start = page
                .saturating_sub(1)
                .saturating_mul(query.page_size.max(1));
            let end = start.saturating_add(query.page_size.max(1));
            let mut offset = 0_u64;
            let mut matches = Vec::new();
            loop {
                let response = client.search(&token, search, offset).await?;
                let batch_len = response.hits.len();
                matches.extend(
                    response
                        .hits
                        .into_iter()
                        .map(|hit| hit.file)
                        .filter(|file| Self::relative_path(&path, &file.path).is_some()),
                );
                if batch_len == 0 || matches.len() >= end {
                    break;
                }
                offset = offset.saturating_add(u64::try_from(batch_len).unwrap_or(u64::MAX));
            }
            let files = matches
                .into_iter()
                .skip(start)
                .take(end - start)
                .collect::<Vec<_>>();
            let has_more = files.len() >= query.page_size.max(1);
            (files, DynamicPagination::Page { page }, has_more)
        } else {
            let (page, cursor) = match query.pagination {
                DynamicPagination::Page { page } => (
                    u32::try_from(page.max(1)).map_err(|_| {
                        ProviderError::InvalidConfig("Cloudreve page exceeds u32::MAX".to_string())
                    })?,
                    None,
                ),
                DynamicPagination::Cursor { cursor } => (1, cursor),
            };
            let response = client
                .list(&token, &path, page, cursor.as_deref(), page_size)
                .await?;
            let has_more = response.pagination.as_ref().is_some_and(|value| {
                if value.is_cursor {
                    !value.next_token.is_empty()
                } else {
                    u64::from(page).saturating_mul(u64::from(page_size)) < value.total_items
                }
            });
            let pagination = response.pagination.as_ref().map_or(
                DynamicPagination::Page {
                    page: usize::try_from(page).unwrap_or(usize::MAX),
                },
                |value| {
                    if value.is_cursor {
                        DynamicPagination::Cursor {
                            cursor: Some(value.next_token.clone())
                                .filter(|token| !token.is_empty()),
                        }
                    } else {
                        DynamicPagination::Page {
                            page: usize::try_from(page).unwrap_or(usize::MAX),
                        }
                    }
                },
            );
            (response.files, pagination, has_more)
        };

        let thumbnail_paths = files
            .iter()
            .map(|file| (!file.is_dir()).then(|| file.path.clone()))
            .collect();
        let thumbnails = Self::thumbnail_urls(&client, &token, thumbnail_paths).await;
        for (file, thumbnail) in files.iter_mut().zip(thumbnails) {
            file.thumbnail_url = thumbnail;
        }

        let items = files
            .into_iter()
            .filter_map(|file| Self::item_type(&file).map(|item_type| (file, item_type)))
            .map(|(file, item_type)| {
                let relative_path =
                    Self::relative_path(&base.path, &file.path).ok_or_else(|| {
                        ProviderError::ApiError(format!(
                            "Cloudreve item path '{}' is outside playlist path '{}'",
                            file.path, base.path
                        ))
                    })?;
                let thumbnail = Some(file.thumbnail())
                    .filter(|value| !value.is_empty())
                    .map(DynamicPlaylistItemThumbnail::Url);
                Ok(DynamicPlaylistItem {
                    name: file.name,
                    item_type,
                    target: Self::encode_target(&relative_path)?,
                    size: u64::try_from(file.size.max(0)).ok(),
                    thumbnail,
                    description: None,
                    modified_at: file.updated_at.map(|value| value.timestamp()),
                    source_config: None,
                    metadata: None,
                })
            })
            .collect::<Result<Vec<_>, ProviderError>>()?;
        Ok(DynamicListResult {
            items,
            pagination: pagination_result,
            has_more,
            supports_search: true,
        })
    }

    async fn resolve_item(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: &ProviderTarget,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        let config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;
        let base = Self::playlist_config(config)?;
        let relative = Self::decode_target(Some(target))?.ok_or_else(|| {
            ProviderError::InvalidConfig("Cloudreve target is required".to_string())
        })?;
        let parent = relative
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .filter(|p| !p.is_empty());
        let parent_target = parent.map(Self::encode_target).transpose()?;
        let mut pagination = DynamicPagination::Cursor { cursor: None };
        loop {
            let result = self
                .list_playlist(
                    ctx,
                    playlist,
                    parent_target.as_ref(),
                    DynamicListQuery {
                        pagination: pagination.clone(),
                        page_size: DYNAMIC_PAGE_SIZE,
                        ..DynamicListQuery::default()
                    },
                )
                .await?;
            if let Some(item) = result
                .items
                .iter()
                .find(|item| item.item_type == ItemType::Media && &item.target == target)
            {
                return Self::next_item(base, item).map(Some);
            }
            let Some(next_pagination) = Self::next_pagination(&result) else {
                return Ok(None);
            };
            pagination = next_pagination;
        }
    }

    async fn next(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: &ProviderTarget,
        play_mode: PlayMode,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        if play_mode == PlayMode::RepeatOne {
            return Ok(None);
        }
        let config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;
        let base = Self::playlist_config(config)?;
        let relative = Self::decode_target(Some(target))?.ok_or_else(|| {
            ProviderError::InvalidConfig("Cloudreve target is required".to_string())
        })?;
        let parent = relative
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .filter(|p| !p.is_empty());
        let parent_target = parent.map(Self::encode_target).transpose()?;
        if matches!(play_mode, PlayMode::Sequential | PlayMode::RepeatAll) {
            let mut scan = SequentialMediaScan::default();
            let mut pagination = DynamicPagination::Cursor { cursor: None };
            loop {
                let result = self
                    .list_playlist(
                        ctx,
                        playlist,
                        parent_target.as_ref(),
                        DynamicListQuery {
                            pagination: pagination.clone(),
                            page_size: DYNAMIC_PAGE_SIZE,
                            ..DynamicListQuery::default()
                        },
                    )
                    .await?;
                let next_pagination = Self::next_pagination(&result);
                for item in result
                    .items
                    .into_iter()
                    .filter(|item| item.item_type == ItemType::Media)
                {
                    if let Some(selected) = scan.observe(item, target) {
                        return Self::next_item(base, &selected).map(Some);
                    }
                }
                let Some(next_pagination) = next_pagination else {
                    let selected = scan.finish(play_mode == PlayMode::RepeatAll);
                    return selected
                        .as_ref()
                        .map(|item| Self::next_item(base, item))
                        .transpose();
                };
                pagination = next_pagination;
            }
        }

        let mut media = Vec::new();
        let mut pagination = DynamicPagination::Cursor { cursor: None };
        loop {
            let result = self
                .list_playlist(
                    ctx,
                    playlist,
                    parent_target.as_ref(),
                    DynamicListQuery {
                        pagination: pagination.clone(),
                        page_size: DYNAMIC_PAGE_SIZE,
                        ..DynamicListQuery::default()
                    },
                )
                .await?;
            let next_pagination = Self::next_pagination(&result);
            media.extend(
                result
                    .items
                    .into_iter()
                    .filter(|item| item.item_type == ItemType::Media),
            );
            let Some(next_pagination) = next_pagination else {
                break;
            };
            if media.len() >= DYNAMIC_MAX_ITEMS {
                break;
            }
            pagination = next_pagination;
        }
        media.truncate(DYNAMIC_MAX_ITEMS);
        if media.is_empty() {
            return Ok(None);
        }

        let selected = match play_mode {
            PlayMode::Shuffle => {
                let candidates = media
                    .iter()
                    .filter(|item| &item.target != target)
                    .collect::<Vec<_>>();
                if candidates.is_empty() {
                    media.first()
                } else {
                    candidates
                        .get(rand::random_range(0..candidates.len()))
                        .copied()
                }
            }
            PlayMode::Sequential | PlayMode::RepeatAll | PlayMode::RepeatOne => None,
        };
        selected.map(|item| Self::next_item(base, item)).transpose()
    }

    async fn browse_path(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &crate::models::Playlist,
        target: Option<&ProviderTarget>,
    ) -> Result<Vec<DynamicBrowsePathSegment>, ProviderError> {
        let Some(relative) = Self::decode_target(target)? else {
            return Ok(Vec::new());
        };
        let parts = relative
            .split('/')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        parts
            .iter()
            .enumerate()
            .map(|(index, part)| {
                Ok(DynamicBrowsePathSegment {
                    name: (*part).to_string(),
                    target: Self::encode_target(&format!("/{}", parts[..=index].join("/")))?,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, path: &str, file_type: i64) -> CloudreveFile {
        CloudreveFile {
            id: path.to_string(),
            name: name.to_string(),
            path: path.to_string(),
            size: 1024,
            file_type,
            updated_at: None,
            metadata: HashMap::new(),
            thumbnail_url: None,
        }
    }

    #[test]
    fn classifies_dynamic_playlist_items() {
        assert_eq!(
            CloudreveProvider::item_type(&file("Season 1", "cloudreve://my/Shows/Season 1", 1)),
            Some(ItemType::Playlist)
        );
        assert_eq!(
            CloudreveProvider::item_type(&file(
                "Episode.MKV",
                "cloudreve://my/Shows/Episode.MKV",
                0
            )),
            Some(ItemType::Media)
        );
        assert_eq!(
            CloudreveProvider::item_type(&file("notes.txt", "cloudreve://my/Shows/notes.txt", 0)),
            None
        );
    }

    #[test]
    fn matches_related_subtitles_by_video_stem() {
        assert!(CloudreveProvider::is_related_subtitle(
            "movie.mp4",
            &file("movie.srt", "cloudreve://my/Movies/movie.srt", 0)
        ));
        assert!(CloudreveProvider::is_related_subtitle(
            "movie.mp4",
            &file("movie.en.ass", "cloudreve://my/Movies/movie.en.ass", 0)
        ));
        assert!(!CloudreveProvider::is_related_subtitle(
            "movie.mp4",
            &file("other.srt", "cloudreve://my/Movies/other.srt", 0)
        ));
    }

    #[test]
    fn cloudreve_target_round_trips() {
        let target = CloudreveProvider::encode_target("/Season 1/Episode 1.mp4")
            .expect("test operation should succeed");
        assert_eq!(
            CloudreveProvider::decode_target(Some(&target)).expect("test operation should succeed"),
            Some("/Season 1/Episode 1.mp4".to_string())
        );
    }

    #[test]
    fn empty_directory_path_normalizes_to_cloudreve_root() {
        CloudreveProvider::validate_path("").expect("test operation should succeed");
        CloudreveProvider::validate_path("/").expect("test operation should succeed");
        assert_eq!(CloudreveProvider::normalize_path(""), "cloudreve://my/");
        assert_eq!(CloudreveProvider::normalize_path(" / "), "cloudreve://my/");
        assert!(CloudreveProvider::validate_file_path("").is_err());
        assert!(CloudreveProvider::validate_file_path("/").is_err());
    }

    #[test]
    fn sequential_scan_advances_across_the_two_hundred_item_boundary() {
        let items = (1..=202)
            .map(|index| DynamicPlaylistItem {
                name: format!("Episode {index}"),
                item_type: ItemType::Media,
                target: CloudreveProvider::encode_target(&format!("/Episode {index}.mp4"))
                    .expect("test operation should succeed"),
                size: None,
                thumbnail: None,
                description: None,
                modified_at: None,
                source_config: None,
                metadata: None,
            })
            .collect::<Vec<_>>();
        let current = items[199].target.clone();
        let mut scan = SequentialMediaScan::default();
        let mut selected = None;
        for page in items.chunks(DYNAMIC_PAGE_SIZE) {
            for item in page.iter().cloned() {
                if let Some(item) = scan.observe(item, &current) {
                    selected = Some(item);
                    break;
                }
            }
            if selected.is_some() {
                break;
            }
        }

        assert_eq!(
            selected.expect("test operation should succeed").name,
            "Episode 201"
        );
    }

    #[test]
    fn repeat_all_scan_wraps_from_the_last_item_to_the_first() {
        let first = DynamicPlaylistItem {
            name: "Episode 1".to_string(),
            item_type: ItemType::Media,
            target: CloudreveProvider::encode_target("/Episode 1.mp4")
                .expect("test operation should succeed"),
            size: None,
            thumbnail: None,
            description: None,
            modified_at: None,
            source_config: None,
            metadata: None,
        };
        let last = DynamicPlaylistItem {
            name: "Episode 202".to_string(),
            item_type: ItemType::Media,
            target: CloudreveProvider::encode_target("/Episode 202.mp4")
                .expect("test operation should succeed"),
            size: None,
            thumbnail: None,
            description: None,
            modified_at: None,
            source_config: None,
            metadata: None,
        };
        let last_target = last.target.clone();
        let mut scan = SequentialMediaScan::default();
        assert!(scan.observe(first.clone(), &last_target).is_none());
        assert!(scan.observe(last, &last_target).is_none());
        assert_eq!(
            scan.finish(true)
                .expect("test operation should succeed")
                .target,
            first.target
        );
    }

    #[test]
    fn credential_server_id_is_scoped_to_provider_instance() {
        let default = CloudreveProvider::credential_server_id_for_instance(
            "https://cloudreve.example/",
            None,
        );
        let primary = CloudreveProvider::credential_server_id_for_instance(
            "https://cloudreve.example",
            Some(" primary "),
        );
        let secondary = CloudreveProvider::credential_server_id_for_instance(
            "https://cloudreve.example",
            Some("secondary"),
        );

        assert_ne!(default, primary);
        assert_ne!(primary, secondary);
        assert_eq!(
            primary,
            CloudreveProvider::credential_server_id_for_instance(
                "https://cloudreve.example/",
                Some("primary")
            )
        );
    }

    #[test]
    fn relative_path_requires_a_directory_boundary() {
        assert_eq!(
            CloudreveProvider::relative_path(
                "cloudreve://my/Shows",
                "cloudreve://my/Shows/Season 1"
            ),
            Some("/Season 1".to_string())
        );
        assert_eq!(
            CloudreveProvider::relative_path(
                "cloudreve://my/Shows",
                "cloudreve://my/Shows-old/Episode.mp4"
            ),
            None
        );
    }

    #[test]
    fn next_item_builds_media_source_from_relative_target() {
        let config = CloudrevePlaylistSourceConfig {
            server_id: "server-id".to_string(),
            path: "cloudreve://my/Shows".to_string(),
            proxy_mode: crate::models::PlaybackProxyMode::Auto,
        };
        let item = DynamicPlaylistItem {
            name: "Episode 1".to_string(),
            item_type: ItemType::Media,
            target: CloudreveProvider::encode_target("/Season 1/Episode 1.mp4")
                .expect("test operation should succeed"),
            size: Some(1024),
            thumbnail: None,
            description: None,
            modified_at: None,
            source_config: None,
            metadata: None,
        };

        let next =
            CloudreveProvider::next_item(&config, &item).expect("test operation should succeed");
        let MediaSourceConfig::Cloudreve(source) = next.source_config else {
            panic!("expected Cloudreve media source config");
        };
        assert_eq!(source.server_id, "server-id");
        assert_eq!(source.path, "cloudreve://my/Shows/Season 1/Episode 1.mp4");
        assert_eq!(next.target, item.target);
    }

    fn playback_result(format: &str) -> PlaybackResult {
        PlaybackResult {
            playback_infos: HashMap::from([(
                "direct".to_string(),
                PlaybackInfo {
                    thumbnail: None,
                    medias: vec![PlaybackMedia {
                        name: "movie".to_string(),
                        format: format.to_string(),
                        expire_at: None,
                        metadata: None,
                        p2p_swarm_id: Some("media-swarm".to_string()),
                        provider: PlaybackMediaProvider::Cloudreve(
                            PlaybackCloudreveMedia::Direct {
                                url: "https://storage.example/movie".to_string(),
                                headers: HashMap::from([(
                                    "Authorization".to_string(),
                                    "upstream".to_string(),
                                )]),
                            },
                        ),
                    }],
                    default_media_index: Some(0),
                    subtitles: vec![PlaybackSubtitle {
                        name: "movie.en.srt".to_string(),
                        language: "en".to_string(),
                        format: "srt".to_string(),
                        p2p_swarm_id: Some("subtitle-swarm".to_string()),
                        provider: PlaybackSubtitleProvider::Cloudreve(
                            PlaybackCloudreveSubtitle::Direct {
                                url: "https://storage.example/movie.en.srt".to_string(),
                                headers: HashMap::new(),
                                expire_at: None,
                            },
                        ),
                    }],
                    default_subtitle_index: None,
                    danmakus: Vec::new(),
                    default_danmaku_index: None,
                },
            )]),
            default_mode: "direct".to_string(),
            provider: crate::models::SourceProvider::Cloudreve,
            provider_instance_name: None,
            duration_seconds: None,
            playback_kind: Some(crate::models::PlaybackKind::Regular),
            metadata: None,
        }
    }

    #[test]
    fn proxy_resources_keep_the_direct_cache_and_hide_upstream_urls() {
        let mut result = playback_result("mp4");
        mark_cloudreve_playback_resources(
            &mut result,
            "version-1",
            1_900_000_000,
            crate::models::PlaybackProxyMode::Prefer,
        );

        let direct = &result.playback_infos["direct"];
        assert!(matches!(
            direct.medias[0].provider,
            PlaybackMediaProvider::Cloudreve(PlaybackCloudreveMedia::Direct { .. })
        ));
        let proxy = &result.playback_infos["proxy_direct"];
        assert!(matches!(
            proxy.medias[0].provider,
            PlaybackMediaProvider::Cloudreve(PlaybackCloudreveMedia::ProxyStream {
                ref version,
                media_index: 0,
                ..
            }) if version == "version-1"
        ));
        assert!(matches!(
            proxy.subtitles[0].provider,
            PlaybackSubtitleProvider::Cloudreve(PlaybackCloudreveSubtitle::Proxy {
                subtitle_index: 0,
                ..
            })
        ));
        assert_eq!(proxy.medias[0].p2p_swarm_id.as_deref(), Some("media-swarm"));
    }

    #[test]
    fn hls_media_uses_the_manifest_proxy_route() {
        let mut result = playback_result("hls");
        mark_cloudreve_playback_resources(
            &mut result,
            "version-1",
            1_900_000_000,
            crate::models::PlaybackProxyMode::Auto,
        );

        assert!(matches!(
            result.playback_infos["proxy_direct"].medias[0].provider,
            PlaybackMediaProvider::Cloudreve(PlaybackCloudreveMedia::ProxyHlsManifest { .. })
        ));
    }

    #[test]
    fn playback_proxy_modes_choose_and_lock_the_proxy_sibling() {
        let mut preferred = playback_result("mp4");
        mark_cloudreve_playback_resources(
            &mut preferred,
            "version-1",
            1_900_000_000,
            crate::models::PlaybackProxyMode::Prefer,
        );
        assert_eq!(preferred.default_mode, "proxy_direct");
        assert!(preferred.playback_infos.contains_key("direct"));

        let mut proxy_only = playback_result("mp4");
        mark_cloudreve_playback_resources(
            &mut proxy_only,
            "version-1",
            1_900_000_000,
            crate::models::PlaybackProxyMode::Only,
        );
        assert_eq!(proxy_only.default_mode, "proxy_direct");
        assert!(!proxy_only.playback_infos.contains_key("direct"));
        assert!(proxy_only
            .playback_infos
            .keys()
            .all(|mode_name| mode_name.starts_with("proxy_")));
    }
}
