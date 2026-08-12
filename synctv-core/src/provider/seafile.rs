//! Seafile library and file media provider.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rand::seq::IndexedRandom;
use sha2::{Digest, Sha256};

use super::{
    DynamicBrowsePathSegment, DynamicListQuery, DynamicListResult, DynamicPagination,
    DynamicPlaylistItem, DynamicPlaylistItemThumbnail, DynamicPlaylistProvider, ItemType,
    MediaProvider, NextPlayItem, PlaybackInfo, PlaybackResult, ProviderContext,
    ProviderCredentialDependency, ProviderError, SourceConfig, SourceCover,
};
use crate::models::{
    detect_direct_url_format, normalize_provider_instance_name,
    normalize_provider_instance_name_owned, MediaSourceConfig, PlayMode, PlaybackMedia,
    PlaybackMediaProvider, PlaybackMetadata, PlaybackSeafileMedia, PlaybackSeafileSubtitle,
    PlaybackSubtitle, PlaybackSubtitleProvider, PlaylistSourceConfig, ProviderCredential,
    ProviderTarget, SeafileMediaSourceConfig, SeafilePlaybackMetadata, SeafilePlaylistSource,
    SeafilePlaylistSourceConfig, UserId, UserProviderCredential,
};
use crate::repository::UserProviderCredentialRepository;
use synctv_media_providers::seafile::{
    SeafileAccount, SeafileClient, SeafileItem, SeafileList, SeafileServerInfo,
};

const PLAYBACK_CACHE_TTL: Duration = Duration::from_hours(2);
const SHUFFLE_LIMIT: usize = 200;
const RELATED_SUBTITLE_LIMIT: usize = 32;

#[derive(Debug, Clone)]
pub struct SeafileBind {
    pub id: i64,
    pub server_id: String,
    pub endpoint: String,
    pub username: String,
    pub version: String,
    pub features: Vec<String>,
    pub created_at: i64,
    pub provider_instance_name: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct SeafileHlsResourceRequest<'a> {
    pub version: &'a str,
    pub mode_name: &'a str,
    pub media_index: usize,
    pub target_url: &'a str,
    pub is_manifest: bool,
    pub range_header: Option<&'a str>,
}

#[async_trait]
impl MediaProvider for SeafileProvider {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    async fn generate_playback(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &MediaSourceConfig,
    ) -> Result<PlaybackResult, ProviderError> {
        let config = Self::media_config(source_config)?;
        validate_repository_id(&config.repository_id)?;
        validate_file_path(&config.path)?;
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
        auth.unlock_if_configured(&config.repository_id).await?;
        let info = auth
            .client
            .file_info(&auth.token, &config.repository_id, &config.path)
            .await?;
        if !is_playable(&info.name) {
            return Err(ProviderError::InvalidConfig(
                "Seafile media path must identify a playable file".to_string(),
            ));
        }
        let metadata = SeafilePlaybackMetadata {
            repository_id: config.repository_id.clone(),
            object_id: info.object_id.clone(),
            name: info.name.clone(),
            path: config.path.clone(),
            size: info.size,
            modified_at: info.modified_at,
            is_locked: info.is_locked,
            can_preview: info.can_preview,
            can_edit: info.can_edit,
            has_thumbnail: config.has_thumbnail,
        };
        let subtitles = discover_subtitles(
            &auth,
            &config.server_id,
            &config.repository_id,
            &config.path,
        )
        .await?;
        let mut playback_infos = HashMap::new();
        playback_infos.insert(
            "original".to_string(),
            PlaybackInfo {
                thumbnail: config.has_thumbnail.then(|| config.path.clone()),
                medias: vec![PlaybackMedia {
                    name: info.name,
                    format: detect_direct_url_format(&config.path).to_string(),
                    expire_at: None,
                    metadata: None,
                    p2p_swarm_id: Some(super::provider_p2p_swarm_id(
                        Self::NAME,
                        auth.instance_name.as_deref(),
                        "media",
                        &format!(
                            "server:{}:repository:{}:object:{}",
                            config.server_id, config.repository_id, info.object_id
                        ),
                    )),
                    provider: PlaybackMediaProvider::Seafile(PlaybackSeafileMedia::Refresh {
                        credential_owner_id: owner.to_string(),
                        server_id: config.server_id.clone(),
                        repository_id: config.repository_id.clone(),
                        path: config.path.clone(),
                        object_id: info.object_id,
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
            provider: crate::models::SourceProvider::Seafile,
            provider_instance_name: auth.instance_name,
            duration_seconds: None,
            playback_kind: Some(crate::models::PlaybackKind::Regular),
            metadata: Some(PlaybackMetadata::Seafile(metadata)),
        };
        let mut result = result;
        if config.proxy_mode == crate::models::PlaybackProxyMode::Prefer {
            let url = auth
                .client
                .download_url(&auth.token, &config.repository_id, &config.path)
                .await?;
            let headers = synctv_media_providers::seafile::SeafileClient::auth_headers(&auth.token);
            let direct = result
                .playback_infos
                .get("original")
                .cloned()
                .ok_or_else(|| {
                    ProviderError::Internal("Seafile playback mode missing".to_string())
                })?
                .medias
                .into_iter()
                .map(|mut media| {
                    media.provider = PlaybackMediaProvider::Seafile(PlaybackSeafileMedia::Direct {
                        url: url.to_string(),
                        headers: headers.clone(),
                    });
                    media
                })
                .collect::<Vec<_>>();
            if let Some(info) = result.playback_infos.get("original").cloned() {
                result.playback_infos.insert(
                    "direct".to_string(),
                    PlaybackInfo {
                        medias: direct,
                        ..info
                    },
                );
            }
        }
        super::cached_versioned_playback_or_fill(
            Self::NAME,
            &format!(
                "playback:{owner}:{}:{}:room:{}:{}:proxy:{}",
                config.server_id,
                config.repository_id,
                ctx.room_id
                    .map_or_else(|| "none".to_string(), |room| room.to_string()),
                config.path,
                config.proxy_mode.as_str()
            ),
            PLAYBACK_CACHE_TTL,
            ctx,
            |result, version, expires_at| {
                mark_playback_resources(result, version, expires_at);
                super::apply_provider_playback_policy(result, config.proxy_mode, true);
            },
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
                validate_repository_id(&config.repository_id)?;
                validate_file_path(&config.path)?;
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
                "Referenced Seafile credential not found for server_id '{server_id}'"
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
        let (server_id, cover) = match source_config {
            SourceConfig::Media(config) => {
                let config = Self::media_config(config)?;
                let cover = config
                    .has_thumbnail
                    .then(|| (config.repository_id.clone(), config.path.clone()));
                (config.server_id.clone(), cover)
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
                let cover = Self::playlist_cover(&auth, &config.source).await?;
                (config.server_id.clone(), cover)
            }
        };
        Ok(cover.map(|(repository_id, path)| SourceCover::Seafile {
            server_id,
            credential_owner_id: owner,
            repository_id,
            path,
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
            crate::models::SourceProvider::Seafile,
            *owner,
            Self::source_server_id(source_config)?.to_string(),
        )])
    }
}

#[async_trait]
impl DynamicPlaylistProvider for SeafileProvider {
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
                "Seafile uses page pagination".to_string(),
            ));
        };
        let auth = self
            .authenticated_with_repo(
                self.credential_repo_or(ctx.credential_repo)?,
                owner,
                &config.server_id,
            )
            .await?;
        let target = decode_target(target)?;
        let page = page.max(1);
        let page_size = u32::try_from(query.page_size.clamp(1, 200)).unwrap_or(200);
        let response = if let Some(target) = target.as_ref() {
            auth.unlock_if_configured(&target.repository_id).await?;
            if let Some(search) = query
                .search
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                auth.client
                    .search(
                        &auth.token,
                        &target.repository_id,
                        search,
                        page as u64,
                        page_size,
                    )
                    .await?
            } else {
                auth.client
                    .list(
                        &auth.token,
                        &target.repository_id,
                        &target.path,
                        page as u64,
                        page_size,
                    )
                    .await?
            }
        } else {
            match &config.source {
                SeafilePlaylistSource::Folder {
                    repository_id,
                    path,
                } => {
                    auth.unlock_if_configured(repository_id).await?;
                    if let Some(search) = query
                        .search
                        .as_deref()
                        .filter(|value| !value.trim().is_empty())
                    {
                        auth.client
                            .search(&auth.token, repository_id, search, page as u64, page_size)
                            .await?
                    } else {
                        auth.client
                            .list(&auth.token, repository_id, path, page as u64, page_size)
                            .await?
                    }
                }
                SeafilePlaylistSource::Starred => {
                    auth.client
                        .starred(&auth.token, page as u64, page_size)
                        .await?
                }
                SeafilePlaylistSource::Search {
                    repository_id,
                    query,
                } => {
                    auth.unlock_if_configured(repository_id).await?;
                    auth.client
                        .search(&auth.token, repository_id, query, page as u64, page_size)
                        .await?
                }
            }
        };
        let has_more = response.page.saturating_mul(u64::from(response.page_size)) < response.total;
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
            ProviderError::InvalidConfig("Seafile target is required".to_string())
        })?;
        validate_file_path(&target.path)?;
        let provider_target = ProviderTarget::seafile(
            target.repository_id.clone(),
            target.path.clone(),
            target.object_id.clone(),
            target.has_thumbnail,
        );
        Ok(Some(NextPlayItem {
            name: target
                .path
                .rsplit('/')
                .next()
                .unwrap_or("Seafile media")
                .to_string(),
            item_type: ItemType::Media,
            source_config: MediaSourceConfig::Seafile(SeafileMediaSourceConfig {
                server_id: config.server_id.clone(),
                repository_id: target.repository_id,
                path: target.path,
                object_id: target.object_id,
                has_thumbnail: target.has_thumbnail,
                proxy_mode: config.proxy_mode,
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
                    target: ProviderTarget::seafile(
                        target.repository_id.clone(),
                        current.clone(),
                        String::new(),
                        false,
                    ),
                }
            })
            .collect())
    }
}

fn map_list_response(response: SeafileList) -> SeafileListResponse {
    let has_more = response.page.saturating_mul(u64::from(response.page_size)) < response.total;
    SeafileListResponse {
        content: response.items,
        total: response.total,
        page: usize::try_from(response.page).unwrap_or(usize::MAX),
        has_more,
    }
}

fn map_directory_item(
    item: SeafileItem,
    owner: UserId,
    server_id: &str,
) -> Option<DynamicPlaylistItem> {
    let item_type = if item.is_directory {
        ItemType::Playlist
    } else if is_playable(&item.name) {
        ItemType::Media
    } else {
        return None;
    };
    let modified_at = item.modified_at.parse::<i64>().ok().or_else(|| {
        DateTime::parse_from_rfc3339(&item.modified_at)
            .ok()
            .map(|value| value.timestamp())
    });
    Some(DynamicPlaylistItem {
        name: item.name,
        item_type,
        target: ProviderTarget::seafile(
            item.repository_id.clone(),
            item.path.clone(),
            item.object_id,
            item.has_thumbnail,
        ),
        size: (!item.is_directory).then_some(item.size),
        thumbnail: (!item.is_directory && item.has_thumbnail).then(|| {
            DynamicPlaylistItemThumbnail::Seafile {
                server_id: server_id.to_string(),
                credential_owner_id: owner,
                repository_id: item.repository_id,
                path: item.path,
            }
        }),
        description: (!item.repository_name.is_empty()).then_some(item.repository_name),
        modified_at,
        source_config: None,
        metadata: None,
    })
}

fn mark_playback_resources(result: &mut PlaybackResult, version: &str, expires_at: i64) {
    let original_modes = result
        .playback_infos
        .iter()
        .map(|(name, info)| (name.clone(), info.clone()))
        .collect::<Vec<_>>();
    for (mode_name, original_info) in original_modes {
        if mode_name.starts_with("proxy_") {
            continue;
        }
        let mut proxy_info = original_info.clone();
        proxy_info.medias = original_info
            .medias
            .iter()
            .enumerate()
            .filter_map(|(media_index, media)| {
                let PlaybackMediaProvider::Seafile(PlaybackSeafileMedia::Refresh {
                    credential_owner_id,
                    server_id,
                    repository_id,
                    path,
                    object_id,
                }) = &media.provider
                else {
                    return None;
                };
                let mut proxy = media.clone();
                proxy.provider = PlaybackMediaProvider::Seafile(PlaybackSeafileMedia::Proxy {
                    version: version.to_string(),
                    expires_at,
                    mode_name: mode_name.clone(),
                    media_index,
                    credential_owner_id: credential_owner_id.clone(),
                    server_id: server_id.clone(),
                    repository_id: repository_id.clone(),
                    path: path.clone(),
                    object_id: object_id.clone(),
                });
                Some(proxy)
            })
            .collect();
        if proxy_info.medias.is_empty() {
            continue;
        }
        for (subtitle_index, subtitle) in proxy_info.subtitles.iter_mut().enumerate() {
            if let PlaybackSubtitleProvider::Seafile(resource) = &mut subtitle.provider {
                resource.version = version.to_string();
                resource.expires_at = expires_at;
                resource.mode_name.clone_from(&mode_name);
                resource.subtitle_index = subtitle_index;
            }
        }
        result
            .playback_infos
            .insert(format!("proxy_{mode_name}"), proxy_info);
        if original_info.medias.iter().all(|media| {
            matches!(
                media.provider,
                PlaybackMediaProvider::Seafile(PlaybackSeafileMedia::Refresh { .. })
            )
        }) {
            result.playback_infos.remove(&mode_name);
        }
    }
}

async fn discover_subtitles(
    auth: &AuthenticatedSeafile,
    server_id: &str,
    repository_id: &str,
    media_path: &str,
) -> Result<Vec<PlaybackSubtitle>, ProviderError> {
    let parent = parent_path(media_path);
    let mut page = 1;
    let mut subtitles = Vec::new();
    loop {
        let list = auth
            .client
            .list(&auth.token, repository_id, &parent, page, 500)
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
                    p2p_swarm_id: Some(super::provider_p2p_swarm_id(
                        SeafileProvider::NAME,
                        auth.instance_name.as_deref(),
                        "subtitle",
                        &format!(
                            "server:{server_id}:repository:{repository_id}:path:{}",
                            item.path
                        ),
                    )),
                    provider: PlaybackSubtitleProvider::Seafile(PlaybackSeafileSubtitle {
                        version: String::new(),
                        expires_at: 0,
                        mode_name: String::new(),
                        subtitle_index: 0,
                        repository_id: repository_id.to_string(),
                        path: item.path.clone(),
                    }),
                }),
        );
        if subtitles.len() >= RELATED_SUBTITLE_LIMIT
            || list.page.saturating_mul(u64::from(list.page_size)) >= list.total
        {
            break;
        }
        page = page.saturating_add(1);
    }
    Ok(subtitles)
}

fn related_subtitle(media_path: &str, item: &SeafileItem) -> bool {
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
) -> Result<Option<crate::models::SeafileTarget>, ProviderError> {
    let Some(target) = target else {
        return Ok(None);
    };
    let ProviderTarget::Seafile(target) = target else {
        return Err(ProviderError::InvalidConfig(
            "Seafile target must use seafile payload".to_string(),
        ));
    };
    validate_repository_id(&target.repository_id)?;
    validate_path(&target.path)?;
    Ok(Some(target.clone()))
}

fn validate_playlist_source(source: &SeafilePlaylistSource) -> Result<(), ProviderError> {
    match source {
        SeafilePlaylistSource::Folder {
            repository_id,
            path,
        } => {
            validate_repository_id(repository_id)?;
            validate_path(path)
        }
        SeafilePlaylistSource::Starred => Ok(()),
        SeafilePlaylistSource::Search {
            repository_id,
            query,
        } => {
            validate_repository_id(repository_id)?;
            if query.trim().is_empty() {
                return Err(ProviderError::InvalidConfig(
                    "Seafile search query is required".to_string(),
                ));
            }
            Ok(())
        }
    }
}

fn validate_repository_id(repository_id: &str) -> Result<(), ProviderError> {
    if repository_id.trim().is_empty() {
        return Err(ProviderError::InvalidConfig(
            "Seafile repository_id is required".to_string(),
        ));
    }
    Ok(())
}

fn validate_path(path: &str) -> Result<(), ProviderError> {
    if path.split('/').any(|segment| segment == "..") {
        return Err(ProviderError::InvalidConfig(
            "Seafile path must not contain traversal".to_string(),
        ));
    }
    Ok(())
}

fn validate_file_path(path: &str) -> Result<(), ProviderError> {
    validate_path(path)?;
    if path.trim_matches('/').is_empty() {
        return Err(ProviderError::InvalidConfig(
            "Seafile media path must identify a file".to_string(),
        ));
    }
    Ok(())
}

fn is_playable(name: &str) -> bool {
    matches!(
        name.rsplit('.')
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

fn enough_for_next(
    media: &[DynamicPlaylistItem],
    target: &ProviderTarget,
    play_mode: PlayMode,
) -> bool {
    match play_mode {
        PlayMode::Sequential | PlayMode::RepeatAll => media
            .iter()
            .position(|item| &item.target == target)
            .is_some_and(|index| media.get(index + 1).is_some()),
        PlayMode::Shuffle => media.len() >= SHUFFLE_LIMIT,
        PlayMode::RepeatOne => true,
    }
}

#[derive(Debug, Clone)]
pub struct SeafileListResponse {
    pub content: Vec<SeafileItem>,
    pub total: u64,
    pub page: usize,
    pub has_more: bool,
}

pub struct SeafileListRequest<'a> {
    pub owner: UserId,
    pub server_id: &'a str,
    pub repository_id: &'a str,
    pub path: &'a str,
    pub page: usize,
    pub page_size: usize,
    pub search: Option<&'a str>,
}

#[derive(Clone)]
struct AuthenticatedSeafile {
    client: SeafileClient,
    token: String,
    library_passwords: HashMap<String, String>,
    instance_name: Option<String>,
}

impl AuthenticatedSeafile {
    async fn unlock_if_configured(&self, repository_id: &str) -> Result<(), ProviderError> {
        if let Some(password) = self.library_passwords.get(repository_id) {
            self.client
                .unlock_repository(&self.token, repository_id, password)
                .await?;
        }
        Ok(())
    }
}

pub struct SeafileProvider {
    http_client: reqwest::Client,
    credential_repo: Option<Arc<UserProviderCredentialRepository>>,
}

impl SeafileProvider {
    pub const NAME: &'static str = "seafile";

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
            ProviderError::Internal("Seafile credential repository is unavailable".to_string())
        })
    }

    fn credential_repo_or<'a>(
        &'a self,
        fallback: Option<&'a UserProviderCredentialRepository>,
    ) -> Result<&'a UserProviderCredentialRepository, ProviderError> {
        self.credential_repo.as_deref().or(fallback).ok_or_else(|| {
            ProviderError::Internal("Seafile credential repository is unavailable".to_string())
        })
    }

    fn client(&self, endpoint: &str) -> Result<SeafileClient, ProviderError> {
        SeafileClient::with_http_client(endpoint, self.http_client.clone()).map_err(Into::into)
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
        owner: UserId,
        endpoint: String,
        username: String,
        password: String,
        provider_instance_name: Option<String>,
    ) -> Result<(String, SeafileAccount, SeafileServerInfo), ProviderError> {
        if username.trim().is_empty() || password.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Seafile username and password are required".to_string(),
            ));
        }
        let client = self.client(&endpoint)?;
        let token = client.login(username.trim(), &password).await?;
        let (account, server) = tokio::try_join!(client.account(&token), client.server_info())?;
        let provider_instance_name = normalize_provider_instance_name_owned(provider_instance_name);
        let server_id =
            Self::credential_server_id_for_instance(&endpoint, provider_instance_name.as_deref());
        let now = Utc::now();
        self.credential_repo()?
            .upsert_by_user_provider_server(&UserProviderCredential {
                id: 0,
                user_id: owner,
                provider: Self::NAME.to_string(),
                server_id: server_id.clone(),
                provider_instance_name,
                credential_data: ProviderCredential::Seafile {
                    endpoint,
                    username: account.email.clone(),
                    token,
                    version: server.version.clone(),
                    features: server.features.clone(),
                    library_passwords: HashMap::new(),
                },
                expires_at: None,
                created_at: now,
                updated_at: now,
            })
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?;
        Ok((server_id, account, server))
    }

    async fn authenticated_with_repo(
        &self,
        repo: &UserProviderCredentialRepository,
        owner: UserId,
        server_id: &str,
    ) -> Result<AuthenticatedSeafile, ProviderError> {
        let credential = repo
            .get_by_provider_and_server(owner, Self::NAME, server_id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
            .ok_or(ProviderError::CredentialRequired)?;
        let ProviderCredential::Seafile {
            endpoint,
            token,
            library_passwords,
            ..
        } = credential.credential_data
        else {
            return Err(ProviderError::InvalidCredentialType);
        };
        Ok(AuthenticatedSeafile {
            client: self.client(&endpoint)?,
            token,
            library_passwords,
            instance_name: credential.provider_instance_name,
        })
    }

    async fn authenticated(
        &self,
        owner: UserId,
        server_id: &str,
    ) -> Result<AuthenticatedSeafile, ProviderError> {
        self.authenticated_with_repo(self.credential_repo()?, owner, server_id)
            .await
    }

    pub async fn unlock_library(
        &self,
        owner: UserId,
        server_id: &str,
        repository_id: &str,
        password: String,
    ) -> Result<Option<String>, ProviderError> {
        validate_repository_id(repository_id)?;
        if password.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Seafile library password is required".to_string(),
            ));
        }
        let mut credential = self
            .credential_repo()?
            .get_by_provider_and_server(owner, Self::NAME, server_id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
            .ok_or(ProviderError::CredentialRequired)?;
        let ProviderCredential::Seafile {
            endpoint,
            token,
            library_passwords,
            ..
        } = &mut credential.credential_data
        else {
            return Err(ProviderError::InvalidCredentialType);
        };
        self.client(endpoint)?
            .unlock_repository(token, repository_id, &password)
            .await?;
        library_passwords.insert(repository_id.to_string(), password);
        credential.updated_at = Utc::now();
        self.credential_repo()?
            .upsert_by_user_provider_server(&credential)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?;
        Ok(credential.provider_instance_name)
    }

    pub async fn list_repositories(
        &self,
        owner: UserId,
        server_id: &str,
        page: usize,
        page_size: usize,
    ) -> Result<(SeafileListResponse, Option<String>), ProviderError> {
        let auth = self.authenticated(owner, server_id).await?;
        let list = auth
            .client
            .repositories(
                &auth.token,
                page as u64,
                u32::try_from(page_size).unwrap_or(u32::MAX),
            )
            .await?;
        Ok((map_list_response(list), auth.instance_name))
    }

    pub async fn list(
        &self,
        request: SeafileListRequest<'_>,
    ) -> Result<(SeafileListResponse, Option<String>), ProviderError> {
        let SeafileListRequest {
            owner,
            server_id,
            repository_id,
            path,
            page,
            page_size,
            search,
        } = request;
        validate_repository_id(repository_id)?;
        validate_path(path)?;
        let auth = self.authenticated(owner, server_id).await?;
        auth.unlock_if_configured(repository_id).await?;
        let list = if let Some(query) = search.filter(|query| !query.trim().is_empty()) {
            auth.client
                .search(
                    &auth.token,
                    repository_id,
                    query,
                    page as u64,
                    u32::try_from(page_size).unwrap_or(u32::MAX),
                )
                .await?
        } else {
            auth.client
                .list(
                    &auth.token,
                    repository_id,
                    path,
                    page as u64,
                    u32::try_from(page_size).unwrap_or(u32::MAX),
                )
                .await?
        };
        Ok((map_list_response(list), auth.instance_name))
    }

    pub async fn list_starred(
        &self,
        owner: UserId,
        server_id: &str,
        page: usize,
        page_size: usize,
    ) -> Result<(SeafileListResponse, Option<String>), ProviderError> {
        let auth = self.authenticated(owner, server_id).await?;
        let list = auth
            .client
            .starred(
                &auth.token,
                page as u64,
                u32::try_from(page_size).unwrap_or(u32::MAX),
            )
            .await?;
        Ok((map_list_response(list), auth.instance_name))
    }

    pub async fn list_binds(
        &self,
        owner: UserId,
        provider_instance_name: Option<&str>,
    ) -> Result<Vec<SeafileBind>, ProviderError> {
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
                let ProviderCredential::Seafile {
                    endpoint,
                    username,
                    version,
                    features,
                    ..
                } = credential.credential_data
                else {
                    return Err(ProviderError::InvalidCredentialType);
                };
                Ok(SeafileBind {
                    id: credential.id,
                    server_id: credential.server_id,
                    endpoint,
                    username,
                    version,
                    features,
                    created_at: credential.created_at.timestamp(),
                    provider_instance_name: credential.provider_instance_name,
                })
            })
            .collect()
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
        let PlaybackMediaProvider::Seafile(provider) = &media.provider else {
            return Err(ProviderError::InvalidConfig(
                "Seafile cached playback resource is invalid".to_string(),
            ));
        };
        let (credential_owner_id, server_id, repository_id, path) = resource_descriptor(provider)?;
        let owner = credential_owner_id
            .parse::<UserId>()
            .map_err(ProviderError::InvalidConfig)?;
        let auth = self.authenticated(owner, server_id).await?;
        auth.unlock_if_configured(repository_id).await?;
        let url = auth
            .client
            .download_url(&auth.token, repository_id, path)
            .await?;
        super::playback_transport::transport_action_for_target_url(
            url.into(),
            HashMap::new(),
            range_header,
        )
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
                "Seafile HLS manifest request references a non-HLS media resource".to_string(),
            ));
        }
        let PlaybackMediaProvider::Seafile(provider) = &media.provider else {
            return Err(ProviderError::InvalidConfig(
                "Seafile cached HLS resource is invalid".to_string(),
            ));
        };
        let (owner, server_id, repository_id, path) = resource_descriptor(provider)?;
        let owner = owner
            .parse::<UserId>()
            .map_err(ProviderError::InvalidConfig)?;
        let auth = self.authenticated(owner, server_id).await?;
        auth.unlock_if_configured(repository_id).await?;
        let url = auth
            .client
            .download_url(&auth.token, repository_id, path)
            .await?;
        super::playback_transport::transport_action_for_storage_hls_target(
            url.into(),
            HashMap::new(),
            path,
            true,
            None,
        )
    }

    pub async fn get_hls_resource(
        &self,
        store: Option<&Arc<dyn super::ProviderStore>>,
        request: SeafileHlsResourceRequest<'_>,
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
                "Seafile HLS child request references a non-HLS media resource".to_string(),
            ));
        }
        let PlaybackMediaProvider::Seafile(provider) = &media.provider else {
            return Err(ProviderError::InvalidConfig(
                "Seafile cached HLS resource is invalid".to_string(),
            ));
        };
        let (owner, server_id, repository_id, root_path) = resource_descriptor(provider)?;
        let path =
            super::playback_transport::storage_hls_resource_path(root_path, request.target_url)?;
        validate_file_path(&path)?;
        let owner = owner
            .parse::<UserId>()
            .map_err(ProviderError::InvalidConfig)?;
        let auth = self.authenticated(owner, server_id).await?;
        auth.unlock_if_configured(repository_id).await?;
        let url = auth
            .client
            .download_url(&auth.token, repository_id, &path)
            .await?;
        super::playback_transport::transport_action_for_storage_hls_target(
            url.into(),
            HashMap::new(),
            &path,
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
        let PlaybackSubtitleProvider::Seafile(subtitle) = &subtitle.provider else {
            return Err(ProviderError::InvalidConfig(
                "Seafile cached subtitle resource is invalid".to_string(),
            ));
        };
        let media = versioned
            .result
            .playback_infos
            .get(mode_name)
            .and_then(|info| info.medias.first())
            .ok_or(ProviderError::NotFound)?;
        let PlaybackMediaProvider::Seafile(provider) = &media.provider else {
            return Err(ProviderError::NotFound);
        };
        let (owner, server_id, _, _) = resource_descriptor(provider)?;
        let owner = owner
            .parse::<UserId>()
            .map_err(ProviderError::InvalidConfig)?;
        let auth = self.authenticated(owner, server_id).await?;
        auth.unlock_if_configured(&subtitle.repository_id).await?;
        let url = auth
            .client
            .download_url(&auth.token, &subtitle.repository_id, &subtitle.path)
            .await?;
        super::playback_transport::transport_action_for_target_url(
            url.to_string(),
            HashMap::new(),
            None,
        )
    }

    pub async fn thumbnail_action(
        &self,
        owner: UserId,
        server_id: &str,
        repository_id: &str,
        path: &str,
        size: u32,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        validate_repository_id(repository_id)?;
        validate_file_path(path)?;
        let auth = self.authenticated(owner, server_id).await?;
        auth.unlock_if_configured(repository_id).await?;
        super::playback_transport::transport_action_for_target_url(
            auth.client
                .thumbnail_url(repository_id, path, size.clamp(32, 2048))?,
            SeafileClient::auth_headers(&auth.token),
            None,
        )
    }

    fn media_config(
        config: &MediaSourceConfig,
    ) -> Result<&SeafileMediaSourceConfig, ProviderError> {
        match config {
            MediaSourceConfig::Seafile(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "Expected Seafile media source_config".to_string(),
            )),
        }
    }

    fn playlist_config(
        config: &PlaylistSourceConfig,
    ) -> Result<&SeafilePlaylistSourceConfig, ProviderError> {
        match config {
            PlaylistSourceConfig::Seafile(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "Expected Seafile playlist source_config".to_string(),
            )),
        }
    }

    fn source_server_id(source: SourceConfig<'_>) -> Result<&str, ProviderError> {
        match source {
            SourceConfig::Media(config) => Ok(&Self::media_config(config)?.server_id),
            SourceConfig::DynamicPlaylist(config) => Ok(&Self::playlist_config(config)?.server_id),
        }
    }

    async fn playlist_cover(
        auth: &AuthenticatedSeafile,
        source: &SeafilePlaylistSource,
    ) -> Result<Option<(String, String)>, ProviderError> {
        match source {
            SeafilePlaylistSource::Starred => {
                let response = auth.client.starred(&auth.token, 1, 200).await?;
                Ok(first_thumbnail(&response.items))
            }
            SeafilePlaylistSource::Folder {
                repository_id,
                path,
            } => {
                auth.unlock_if_configured(repository_id).await?;
                let mut queue = VecDeque::from([path.clone()]);
                let mut visited = HashSet::new();
                while let Some(path) = queue.pop_front() {
                    if visited.len() >= 32 || !visited.insert(path.clone()) {
                        continue;
                    }
                    let response = auth
                        .client
                        .list(&auth.token, repository_id, &path, 1, 200)
                        .await?;
                    if let Some(cover) = first_thumbnail(&response.items) {
                        return Ok(Some(cover));
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
            SeafilePlaylistSource::Search {
                repository_id,
                query,
            } => {
                auth.unlock_if_configured(repository_id).await?;
                let response = auth
                    .client
                    .search(&auth.token, repository_id, query, 1, 200)
                    .await?;
                for item in response
                    .items
                    .into_iter()
                    .filter(|item| !item.is_directory && is_playable(&item.name))
                    .take(16)
                {
                    let parent = parent_path(&item.path);
                    let directory = auth
                        .client
                        .list(&auth.token, repository_id, &parent, 1, u32::MAX)
                        .await?;
                    if let Some(candidate) = directory.items.iter().find(|candidate| {
                        candidate.path == item.path
                            && candidate.has_thumbnail
                            && is_playable(&candidate.name)
                    }) {
                        return Ok(Some((
                            candidate.repository_id.clone(),
                            candidate.path.clone(),
                        )));
                    }
                }
                Ok(None)
            }
        }
    }
}

fn first_thumbnail(items: &[SeafileItem]) -> Option<(String, String)> {
    items
        .iter()
        .find(|item| !item.is_directory && item.has_thumbnail && is_playable(&item.name))
        .map(|item| (item.repository_id.clone(), item.path.clone()))
}

fn parent_path(path: &str) -> String {
    path.rsplit_once('/').map_or_else(
        || "/".to_string(),
        |(parent, _)| {
            if parent.is_empty() {
                "/".to_string()
            } else {
                parent.to_string()
            }
        },
    )
}

fn resource_descriptor(
    provider: &PlaybackSeafileMedia,
) -> Result<(&str, &str, &str, &str), ProviderError> {
    match provider {
        PlaybackSeafileMedia::Refresh {
            credential_owner_id,
            server_id,
            repository_id,
            path,
            ..
        }
        | PlaybackSeafileMedia::Proxy {
            credential_owner_id,
            server_id,
            repository_id,
            path,
            ..
        } => Ok((credential_owner_id, server_id, repository_id, path)),
        PlaybackSeafileMedia::Direct { .. } => Err(ProviderError::InvalidConfig(
            "Seafile direct media has no provider resource descriptor".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{
        InMemoryProviderStore, ProviderStore, ProviderStoreExt, VersionedPlayback,
    };

    #[tokio::test]
    async fn resource_action_accepts_refresh_media_from_version_mapping() {
        let version = "seafile-refresh";
        let store: Arc<dyn ProviderStore> = Arc::new(InMemoryProviderStore::new(16));
        let media = PlaybackSeafileMedia::Refresh {
            credential_owner_id: "42".to_string(),
            server_id: "seafile-main".to_string(),
            repository_id: "library".to_string(),
            path: "/Videos/Movie.mkv".to_string(),
            object_id: "object".to_string(),
        };
        let versioned = VersionedPlayback {
            version: version.to_string(),
            result: PlaybackResult {
                playback_infos: HashMap::from([(
                    "proxy".to_string(),
                    PlaybackInfo {
                        thumbnail: None,
                        medias: vec![PlaybackMedia {
                            name: "Movie".to_string(),
                            format: "mkv".to_string(),
                            expire_at: None,
                            metadata: None,
                            p2p_swarm_id: None,
                            provider: PlaybackMediaProvider::Seafile(media),
                        }],
                        default_media_index: Some(0),
                        subtitles: Vec::new(),
                        default_subtitle_index: None,
                        danmakus: Vec::new(),
                        default_danmaku_index: None,
                    },
                )]),
                default_mode: "proxy".to_string(),
                provider: crate::models::SourceProvider::Seafile,
                provider_instance_name: None,
                duration_seconds: None,
                playback_kind: Some(crate::models::PlaybackKind::Regular),
                metadata: None,
            },
            expires_at: crate::SystemClock.now().timestamp() + 60,
            playback_context: None,
        };
        store
            .set(&format!("v:{version}"), &versioned, Duration::from_mins(1))
            .await
            .expect("version mapping should be stored");

        let provider = SeafileProvider::with_http_client(reqwest::Client::new());
        let Err(error) = provider
            .get_resource(Some(&store), version, "proxy", 0, None, None)
            .await
        else {
            panic!("missing credentials should prevent a successful action");
        };

        assert!(matches!(
            error,
            ProviderError::Internal(message)
                if message == "Seafile credential repository is unavailable"
        ));
    }

    #[test]
    fn resource_descriptor_accepts_refresh_and_proxy_media() {
        let refresh = PlaybackSeafileMedia::Refresh {
            credential_owner_id: "42".to_string(),
            server_id: "seafile-main".to_string(),
            repository_id: "library".to_string(),
            path: "/Videos/Movie.mkv".to_string(),
            object_id: "object".to_string(),
        };
        let proxy = PlaybackSeafileMedia::Proxy {
            version: "version".to_string(),
            expires_at: 1_800_000_000,
            mode_name: "direct".to_string(),
            media_index: 0,
            credential_owner_id: "42".to_string(),
            server_id: "seafile-main".to_string(),
            repository_id: "library".to_string(),
            path: "/Videos/Movie.mkv".to_string(),
            object_id: "object".to_string(),
        };

        let expected = ("42", "seafile-main", "library", "/Videos/Movie.mkv");
        assert_eq!(
            resource_descriptor(&refresh).expect("refresh descriptor"),
            expected
        );
        assert_eq!(
            resource_descriptor(&proxy).expect("proxy descriptor"),
            expected
        );
    }

    #[test]
    fn discovers_only_related_subtitle_names() {
        assert!(related_stem("Movie.mkv", "movie.zh-CN.ass"));
        assert!(related_stem("movie.mp4", "MOVIE_en.srt"));
        assert!(related_stem("movie.mkv", "movie.vtt"));
        assert!(!related_stem("movie.mkv", "movie2.srt"));
        assert!(!related_stem("movie.mkv", "trailer.srt"));
        assert_eq!(subtitle_format("movie.zh-CN.ASS"), Some("ASS"));
        assert_eq!(subtitle_format("movie.txt"), None);
        assert_eq!(
            subtitle_language("/Videos/Movie.mkv", "Movie.zh-CN.ass"),
            "zh-CN"
        );
    }
}
