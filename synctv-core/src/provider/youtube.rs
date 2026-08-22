//! YouTube media provider adapter.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use sha2::{Digest, Sha256};

use super::{
    DynamicListQuery, DynamicListResult, DynamicPagination, DynamicPlaylistItem,
    DynamicPlaylistItemSourceConfig, DynamicPlaylistItemThumbnail, DynamicPlaylistProvider,
    ItemType, MediaProvider, NextPlayItem, PlaybackInfo, PlaybackResult, ProviderContext,
    ProviderCredentialDependency, ProviderCredentialPolicy, ProviderError, SourceConfig,
    SourceCover,
};
use crate::models::{
    MediaSourceConfig, PlayMode, PlaybackMedia, PlaybackMediaMetadata, PlaybackMediaProvider,
    PlaybackMetadata, PlaybackSubtitle, PlaybackSubtitleProvider, PlaybackYoutubeMedia,
    PlaybackYoutubeSubtitle, PlaylistSourceConfig, ProviderCredential, ProviderTarget, UserId,
    UserProviderCredential, YoutubeChannelContent, YoutubeMediaSourceConfig,
    YoutubePlaybackMetadata, YoutubePlaybackResource, YoutubePlaylistSourceConfig,
};
use crate::repository::UserProviderCredentialRepository;
use synctv_media_providers::youtube::{
    normalize_video_id, YoutubeChannelTab, YoutubeClient, YoutubeFormat, YoutubeListItem,
    YoutubeListPage, YoutubePlayerResponse,
};

const DYNAMIC_PAGE_SIZE: usize = 50;
const DYNAMIC_SHUFFLE_LIMIT: usize = 200;

#[derive(Debug, Clone, Default)]
struct YoutubeSession {
    visitor_data: Option<String>,
    po_token: Option<String>,
    cookie: Option<String>,
}

pub struct YoutubeProvider {
    client: YoutubeClient,
    credential_repo: Option<Arc<UserProviderCredentialRepository>>,
}

#[derive(Debug, Clone)]
pub struct YoutubeBind {
    pub id: i64,
    pub server_id: String,
    pub label: String,
    pub has_visitor_data: bool,
    pub has_po_token: bool,
    pub has_cookie: bool,
    pub created_at: i64,
    pub provider_instance_name: Option<String>,
}

impl Default for YoutubeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl YoutubeProvider {
    pub const NAME: &'static str = "youtube";

    #[must_use]
    pub fn new() -> Self {
        Self {
            client: YoutubeClient::with_http_client(reqwest::Client::new())
                .expect("YouTube HTTP client should build"),
            credential_repo: None,
        }
    }

    #[must_use]
    pub fn with_http_client(client: reqwest::Client) -> Self {
        Self {
            client: YoutubeClient::with_http_client(client)
                .expect("YouTube HTTP client should build"),
            credential_repo: None,
        }
    }

    #[must_use]
    pub fn with_credential_repo(
        &self,
        credential_repo: Arc<UserProviderCredentialRepository>,
    ) -> Self {
        Self {
            client: self.client.clone(),
            credential_repo: Some(credential_repo),
        }
    }

    #[must_use]
    pub fn credential_server_id_for_instance(provider_instance_name: Option<&str>) -> String {
        let instance_name = crate::models::normalize_provider_instance_name(provider_instance_name)
            .unwrap_or_default();
        hex::encode(Sha256::digest(
            format!("youtube\n{instance_name}").as_bytes(),
        ))
    }

    fn credential_repo_or<'a>(
        &'a self,
        fallback: Option<&'a UserProviderCredentialRepository>,
    ) -> Option<&'a UserProviderCredentialRepository> {
        self.credential_repo.as_deref().or(fallback)
    }

    async fn session(
        &self,
        ctx: &ProviderContext<'_>,
        credential_policy: ProviderCredentialPolicy,
    ) -> Result<YoutubeSession, ProviderError> {
        let Some(repo) = self.credential_repo_or(ctx.credential_repo) else {
            return Ok(YoutubeSession::default());
        };
        let owner_id = ctx.selected_credential_user_id(credential_policy);
        let Some(owner_id) = owner_id else {
            return Ok(YoutubeSession::default());
        };
        let server_id =
            Self::credential_server_id_for_instance(super::bound_provider_instance_name(ctx));
        self.session_for_owner(repo, owner_id, &server_id).await
    }

    async fn session_for_owner(
        &self,
        repo: &UserProviderCredentialRepository,
        owner_id: UserId,
        server_id: &str,
    ) -> Result<YoutubeSession, ProviderError> {
        let Some(credential) = repo
            .get_by_provider_and_server(owner_id, Self::NAME, server_id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
        else {
            return Ok(YoutubeSession::default());
        };
        match credential.credential_data {
            ProviderCredential::Youtube {
                visitor_data,
                po_token,
                cookie,
                ..
            } => Ok(YoutubeSession {
                visitor_data,
                po_token,
                cookie,
            }),
            _ => Err(ProviderError::InvalidCredentialType),
        }
    }

    pub async fn persist_session(
        &self,
        user_id: UserId,
        label: String,
        visitor_data: Option<String>,
        po_token: Option<String>,
        cookie: Option<String>,
        provider_instance_name: Option<String>,
    ) -> Result<String, ProviderError> {
        let label = label.trim();
        if label.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "YouTube credential label is required".to_string(),
            ));
        }
        let visitor_data = normalize_secret(visitor_data);
        let po_token = normalize_secret(po_token);
        let cookie = normalize_secret(cookie);
        if visitor_data.is_none() && po_token.is_none() && cookie.is_none() {
            return Err(ProviderError::InvalidConfig(
                "YouTube Visitor Data, PO Token, or Cookie is required".to_string(),
            ));
        }
        let provider_instance_name =
            crate::models::normalize_provider_instance_name_owned(provider_instance_name);
        let server_id = Self::credential_server_id_for_instance(provider_instance_name.as_deref());
        let now = Utc::now();
        let credential = UserProviderCredential {
            id: 0,
            user_id,
            provider: Self::NAME.to_string(),
            server_id: server_id.clone(),
            provider_instance_name,
            credential_data: ProviderCredential::Youtube {
                label: label.to_string(),
                visitor_data,
                po_token,
                cookie,
            },
            expires_at: None,
            created_at: now,
            updated_at: now,
        };
        self.credential_repo
            .as_deref()
            .ok_or_else(|| {
                ProviderError::Internal("YouTube credential repository is unavailable".to_string())
            })?
            .upsert_by_user_provider_server(&credential)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?;
        Ok(server_id)
    }

    pub async fn list_binds(
        &self,
        user_id: UserId,
        provider_instance_name: Option<&str>,
    ) -> Result<Vec<YoutubeBind>, ProviderError> {
        let requested = crate::models::normalize_provider_instance_name(provider_instance_name);
        self.credential_repo
            .as_deref()
            .ok_or_else(|| {
                ProviderError::Internal("YouTube credential repository is unavailable".to_string())
            })?
            .get_readable_by_provider(user_id, Self::NAME)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
            .into_iter()
            .filter(|credential| {
                requested
                    .is_none_or(|name| credential.provider_instance_name.as_deref() == Some(name))
            })
            .map(|credential| {
                let ProviderCredential::Youtube {
                    label,
                    visitor_data,
                    po_token,
                    cookie,
                } = credential.credential_data
                else {
                    return Err(ProviderError::InvalidCredentialType);
                };
                Ok(YoutubeBind {
                    id: credential.id,
                    server_id: credential.server_id,
                    label,
                    has_visitor_data: visitor_data.is_some(),
                    has_po_token: po_token.is_some(),
                    has_cookie: cookie.is_some(),
                    created_at: credential.created_at.timestamp(),
                    provider_instance_name: credential.provider_instance_name,
                })
            })
            .collect()
    }

    pub async fn delete_credential(
        &self,
        user_id: UserId,
        server_id: &str,
    ) -> Result<bool, ProviderError> {
        let repo = self.credential_repo.as_deref().ok_or_else(|| {
            ProviderError::Internal("YouTube credential repository is unavailable".to_string())
        })?;
        let Some(credential) = repo
            .get_by_provider_and_server(user_id, Self::NAME, server_id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
        else {
            return Ok(false);
        };
        repo.delete(credential.id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?;
        Ok(true)
    }

    pub async fn resolve_for_user(
        &self,
        user_id: UserId,
        resource: &str,
        provider_instance_name: Option<&str>,
    ) -> Result<YoutubePlayerResponse, ProviderError> {
        let session = self.stored_session(user_id, provider_instance_name).await?;
        Ok(self
            .client
            .player(
                resource,
                session.visitor_data.as_deref(),
                session.po_token.as_deref(),
                session.cookie.as_deref(),
            )
            .await?)
    }

    pub async fn list_for_user(
        &self,
        user_id: UserId,
        config: &YoutubePlaylistSourceConfig,
        cursor: Option<&str>,
        provider_instance_name: Option<&str>,
    ) -> Result<YoutubeListPage, ProviderError> {
        let session = self.stored_session(user_id, provider_instance_name).await?;
        self.list_page(config, cursor, &session).await
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
        let PlaybackMediaProvider::Youtube(PlaybackYoutubeMedia::Refresh {
            video_id,
            resource,
            credential_owner_id,
            provider_instance_name,
        }) = &media.provider
        else {
            return Err(ProviderError::InvalidConfig(
                "YouTube cached playback resource is invalid".to_string(),
            ));
        };
        let session = match credential_owner_id {
            Some(owner_id) => {
                self.stored_session(*owner_id, provider_instance_name.as_deref())
                    .await?
            }
            None => YoutubeSession::default(),
        };
        let player = self
            .client
            .player(
                video_id,
                session.visitor_data.as_deref(),
                session.po_token.as_deref(),
                session.cookie.as_deref(),
            )
            .await?;
        let url = playback_resource_url(&player, resource)?;
        super::playback_transport::transport_action_for_target_url(
            url,
            youtube_headers(None),
            range_header,
        )
    }

    pub fn get_segment(
        &self,
        target_url: String,
        range_header: Option<&str>,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        super::playback_transport::transport_action_for_target_url(
            target_url,
            youtube_headers(None),
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
        range_header: Option<&str>,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let subtitle = versioned
            .result
            .playback_infos
            .get(mode_name)
            .and_then(|info| info.subtitles.get(subtitle_index))
            .ok_or(ProviderError::NotFound)?;
        let PlaybackSubtitleProvider::Youtube(PlaybackYoutubeSubtitle::Refresh {
            video_id,
            track_id,
            target_language_code,
            credential_owner_id,
            provider_instance_name,
        }) = &subtitle.provider
        else {
            return Err(ProviderError::InvalidConfig(
                "YouTube cached subtitle resource is invalid".to_string(),
            ));
        };
        let session = match credential_owner_id {
            Some(owner_id) => {
                self.stored_session(*owner_id, provider_instance_name.as_deref())
                    .await?
            }
            None => YoutubeSession::default(),
        };
        let player = self
            .client
            .player(
                video_id,
                session.visitor_data.as_deref(),
                session.po_token.as_deref(),
                session.cookie.as_deref(),
            )
            .await?;
        let track = player
            .captions
            .as_ref()
            .and_then(|captions| captions.player_captions_tracklist_renderer.as_ref())
            .and_then(|tracklist| {
                tracklist
                    .caption_tracks
                    .iter()
                    .find(|track| track.vss_id == *track_id)
            })
            .ok_or(ProviderError::NotFound)?;
        let url = youtube_caption_url(&track.base_url, target_language_code.as_deref())?;
        super::playback_transport::full_response_cache_action_for_target_url(
            url.into(),
            youtube_headers(session.cookie.as_deref()),
            range_header,
        )
    }

    async fn stored_session(
        &self,
        owner_id: UserId,
        provider_instance_name: Option<&str>,
    ) -> Result<YoutubeSession, ProviderError> {
        let Some(repo) = self.credential_repo.as_deref() else {
            return Ok(YoutubeSession::default());
        };
        let server_id = Self::credential_server_id_for_instance(provider_instance_name);
        self.session_for_owner(repo, owner_id, &server_id).await
    }

    fn media_config(
        config: &MediaSourceConfig,
    ) -> Result<&YoutubeMediaSourceConfig, ProviderError> {
        match config {
            MediaSourceConfig::Youtube(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "YouTube provider requires YouTube media source_config".to_string(),
            )),
        }
    }

    fn playlist_config(
        config: &PlaylistSourceConfig,
    ) -> Result<&YoutubePlaylistSourceConfig, ProviderError> {
        match config {
            PlaylistSourceConfig::Youtube(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "YouTube provider requires YouTube playlist source_config".to_string(),
            )),
        }
    }

    const fn media_credential_policy(
        config: &YoutubeMediaSourceConfig,
    ) -> ProviderCredentialPolicy {
        ProviderCredentialPolicy::from_shared(config.shared)
    }

    const fn playlist_credential_policy(
        config: &YoutubePlaylistSourceConfig,
    ) -> ProviderCredentialPolicy {
        ProviderCredentialPolicy::from_shared(match config {
            YoutubePlaylistSourceConfig::Playlist { shared, .. }
            | YoutubePlaylistSourceConfig::Channel { shared, .. }
            | YoutubePlaylistSourceConfig::Search { shared, .. }
            | YoutubePlaylistSourceConfig::Subscriptions { shared }
            | YoutubePlaylistSourceConfig::LikedVideos { shared }
            | YoutubePlaylistSourceConfig::WatchLater { shared } => *shared,
        })
    }

    const fn playlist_requires_cookie(config: &YoutubePlaylistSourceConfig) -> bool {
        matches!(
            config,
            YoutubePlaylistSourceConfig::Subscriptions { .. }
                | YoutubePlaylistSourceConfig::LikedVideos { .. }
                | YoutubePlaylistSourceConfig::WatchLater { .. }
        )
    }

    async fn list_page(
        &self,
        config: &YoutubePlaylistSourceConfig,
        cursor: Option<&str>,
        session: &YoutubeSession,
    ) -> Result<YoutubeListPage, ProviderError> {
        let visitor_data = session.visitor_data.as_deref();
        let cookie = session.cookie.as_deref();
        if Self::playlist_requires_cookie(config) && cookie.is_none() {
            return Err(ProviderError::CredentialRequired);
        }
        Ok(match config {
            YoutubePlaylistSourceConfig::Playlist { playlist_id, .. } => {
                self.client
                    .playlist(playlist_id, cursor, visitor_data, cookie)
                    .await?
            }
            YoutubePlaylistSourceConfig::Channel {
                channel_id,
                content,
                ..
            } => {
                let tab = match content {
                    YoutubeChannelContent::Videos => YoutubeChannelTab::Videos,
                    YoutubeChannelContent::Shorts => YoutubeChannelTab::Shorts,
                    YoutubeChannelContent::Live => YoutubeChannelTab::Live,
                };
                self.client
                    .channel(channel_id, tab, cursor, visitor_data, cookie)
                    .await?
            }
            YoutubePlaylistSourceConfig::Search { query, .. } => {
                self.client
                    .search(query, cursor, visitor_data, cookie)
                    .await?
            }
            YoutubePlaylistSourceConfig::Subscriptions { .. } => {
                self.client
                    .feed("FEsubscriptions", cursor, visitor_data, cookie)
                    .await?
            }
            YoutubePlaylistSourceConfig::LikedVideos { .. } => {
                self.client
                    .playlist("LL", cursor, visitor_data, cookie)
                    .await?
            }
            YoutubePlaylistSourceConfig::WatchLater { .. } => {
                self.client
                    .playlist("WL", cursor, visitor_data, cookie)
                    .await?
            }
        })
    }

    fn directory_item(
        item: YoutubeListItem,
        credential_policy: ProviderCredentialPolicy,
    ) -> DynamicPlaylistItem {
        let video_id = item.video_id.clone();
        let metadata = PlaybackMetadata::Youtube(YoutubePlaybackMetadata {
            video_id: video_id.clone(),
            channel_id: item.channel_id.clone(),
            channel_name: item.channel_name.clone(),
            description: String::new(),
            view_count: None,
            publish_date: None,
            upload_date: None,
            category: None,
            is_live: item.is_live,
            is_currently_live: item.is_live.then_some(true),
            live_start: None,
            live_end: None,
            storyboard_spec: None,
            automatic_caption_count: 0,
            manual_caption_count: 0,
            translation_languages: Vec::new(),
        });
        DynamicPlaylistItem {
            name: item.title,
            item_type: ItemType::Media,
            target: ProviderTarget::youtube(video_id.clone()),
            size: None,
            thumbnail: item
                .thumbnail
                .map(|thumbnail| DynamicPlaylistItemThumbnail::Url(thumbnail.url)),
            description: Some(
                [
                    item.channel_name,
                    item.view_count_text,
                    item.published_time_text,
                ]
                .into_iter()
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join(" · "),
            )
            .filter(|value| !value.is_empty()),
            modified_at: None,
            source_config: Some(DynamicPlaylistItemSourceConfig::Media(
                MediaSourceConfig::Youtube(YoutubeMediaSourceConfig {
                    video_id,
                    shared: credential_policy.uses_resource_owner(),
                }),
            )),
            metadata: Some(metadata),
        }
    }

    fn decode_target(target: &ProviderTarget) -> Result<&str, ProviderError> {
        let ProviderTarget::Youtube(target) = target else {
            return Err(ProviderError::InvalidConfig(
                "YouTube playlist requires a YouTube target".to_string(),
            ));
        };
        normalize_video_id(&target.video_id)?;
        Ok(&target.video_id)
    }

    fn next_item(
        base: &YoutubePlaylistSourceConfig,
        item: &DynamicPlaylistItem,
    ) -> Result<NextPlayItem, ProviderError> {
        let video_id = Self::decode_target(&item.target)?.to_string();
        Ok(NextPlayItem {
            name: item.name.clone(),
            item_type: ItemType::Media,
            source_config: MediaSourceConfig::Youtube(YoutubeMediaSourceConfig {
                video_id,
                shared: Self::playlist_credential_policy(base).uses_resource_owner(),
            }),
            target: item.target.clone(),
        })
    }

    fn metadata_model(
        player: &YoutubePlayerResponse,
    ) -> Result<YoutubePlaybackMetadata, ProviderError> {
        let details = player.video_details.as_ref().ok_or_else(|| {
            ProviderError::ApiError("YouTube player returned no video details".to_string())
        })?;
        let microformat = player
            .microformat
            .as_ref()
            .and_then(|value| value.player_microformat_renderer.as_ref());
        let live = microformat.and_then(|value| value.live_broadcast_details.as_ref());
        let tracklist = player
            .captions
            .as_ref()
            .and_then(|captions| captions.player_captions_tracklist_renderer.as_ref());
        let is_live = details.is_live || details.is_live_content;
        Ok(YoutubePlaybackMetadata {
            video_id: details.video_id.clone(),
            channel_id: details.channel_id.clone(),
            channel_name: details.author.clone(),
            description: details.short_description.clone(),
            view_count: details.view_count.parse().ok(),
            publish_date: microformat.and_then(|value| value.publish_date.clone()),
            upload_date: microformat.and_then(|value| value.upload_date.clone()),
            category: microformat.and_then(|value| value.category.clone()),
            is_live,
            is_currently_live: is_live.then_some(details.is_live),
            live_start: live.and_then(|value| value.start_timestamp.clone()),
            live_end: live.and_then(|value| value.end_timestamp.clone()),
            storyboard_spec: player
                .storyboards
                .as_ref()
                .and_then(|value| value.player_storyboard_spec_renderer.as_ref())
                .map(|value| value.spec.clone()),
            automatic_caption_count: tracklist.map_or(0, |value| {
                value
                    .caption_tracks
                    .iter()
                    .filter(|track| track.is_automatic())
                    .count()
            }),
            manual_caption_count: tracklist.map_or(0, |value| {
                value
                    .caption_tracks
                    .iter()
                    .filter(|track| !track.is_automatic())
                    .count()
            }),
            translation_languages: tracklist
                .into_iter()
                .flat_map(|value| &value.translation_languages)
                .map(|language| language.language_code.clone())
                .collect(),
        })
    }

    fn playback_result(
        player: &YoutubePlayerResponse,
        credential_owner_id: Option<UserId>,
        provider_instance_name: Option<&str>,
    ) -> Result<PlaybackResult, ProviderError> {
        let details = player.video_details.as_ref().ok_or_else(|| {
            ProviderError::ApiError("YouTube player returned no video details".to_string())
        })?;
        let streaming = player.streaming_data.as_ref().ok_or_else(|| {
            ProviderError::ApiError("YouTube player returned no streaming data".to_string())
        })?;
        let thumbnail = details
            .thumbnail
            .as_ref()
            .and_then(|collection| collection.thumbnails.last())
            .map(|thumbnail| thumbnail.url.clone());
        let p2p_enabled = !(details.is_live || details.is_live_content);
        let subtitles = youtube_subtitles(
            player,
            &details.video_id,
            credential_owner_id,
            provider_instance_name,
            p2p_enabled,
        );
        let mut playback_infos = HashMap::new();
        let mut best_progressive = None;
        for format in &streaming.formats {
            if format.url.is_none() {
                continue;
            }
            let media = playback_info(
                format.name(),
                format.container(),
                YoutubePlaybackResource::Format { itag: format.itag },
                format_metadata(format),
                thumbnail.clone(),
                subtitles.clone(),
                &details.video_id,
                credential_owner_id,
                provider_instance_name.map(str::to_owned),
                p2p_enabled,
            );
            let info = playback_infos
                .entry("progressive".to_string())
                .or_insert_with(|| PlaybackInfo {
                    thumbnail: thumbnail.clone(),
                    medias: Vec::new(),
                    default_media_index: None,
                    subtitles: subtitles.clone(),
                    default_subtitle_index: None,
                    danmakus: Vec::new(),
                    default_danmaku_index: None,
                });
            let media_index = info.medias.len();
            info.medias.extend(media.medias);
            if best_progressive
                .as_ref()
                .is_none_or(|(height, _)| format.height.unwrap_or_default() > *height)
            {
                best_progressive = Some((format.height.unwrap_or_default(), media_index));
                info.default_media_index = Some(media_index);
            }
        }
        if streaming.hls_manifest_url.is_some() {
            playback_infos.insert(
                "hls".to_string(),
                playback_info(
                    "HLS".to_string(),
                    "m3u8".to_string(),
                    YoutubePlaybackResource::HlsManifest,
                    None,
                    thumbnail.clone(),
                    subtitles.clone(),
                    &details.video_id,
                    credential_owner_id,
                    provider_instance_name.map(str::to_owned),
                    p2p_enabled,
                ),
            );
        }
        if streaming.dash_manifest_url.is_some() {
            playback_infos.insert(
                "dash".to_string(),
                playback_info(
                    "DASH".to_string(),
                    "mpd".to_string(),
                    YoutubePlaybackResource::DashManifest,
                    None,
                    thumbnail,
                    subtitles,
                    &details.video_id,
                    credential_owner_id,
                    provider_instance_name.map(str::to_owned),
                    p2p_enabled,
                ),
            );
        }
        let default_mode = if details.is_live {
            playback_infos
                .contains_key("hls")
                .then(|| "hls".to_string())
        } else {
            best_progressive.map(|_| "progressive".to_string())
        }
        .or_else(|| playback_infos.keys().next().cloned())
        .ok_or_else(|| {
            ProviderError::ApiError("YouTube returned no playable formats".to_string())
        })?;
        let metadata = Self::metadata_model(player)?;
        Ok(PlaybackResult {
            playback_infos,
            default_mode,
            provider: crate::models::SourceProvider::Youtube,
            provider_instance_name: None,
            duration_seconds: details.length_seconds.parse::<f64>().ok(),
            playback_kind: Some(if details.is_live || details.is_live_content {
                crate::models::PlaybackKind::Live
            } else {
                crate::models::PlaybackKind::Regular
            }),
            metadata: Some(PlaybackMetadata::Youtube(metadata)),
        })
    }
}

fn normalize_secret(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    })
}

fn youtube_headers(cookie: Option<&str>) -> HashMap<String, String> {
    let mut headers = HashMap::from([
        ("Origin".to_string(), "https://www.youtube.com".to_string()),
        (
            "Referer".to_string(),
            "https://www.youtube.com/".to_string(),
        ),
        (
            "User-Agent".to_string(),
            synctv_media_providers::PROVIDER_USER_AGENT.to_string(),
        ),
    ]);
    if let Some(cookie) = cookie.map(str::trim).filter(|value| !value.is_empty()) {
        headers.insert("Cookie".to_string(), cookie.to_string());
    }
    headers
}

fn playback_resource_url(
    player: &YoutubePlayerResponse,
    resource: &YoutubePlaybackResource,
) -> Result<String, ProviderError> {
    let streaming = player.streaming_data.as_ref().ok_or_else(|| {
        ProviderError::ApiError("YouTube player returned no streaming data".to_string())
    })?;
    match resource {
        YoutubePlaybackResource::Format { itag } => streaming
            .formats
            .iter()
            .chain(&streaming.adaptive_formats)
            .find(|format| format.itag == *itag)
            .and_then(|format| format.url.clone()),
        YoutubePlaybackResource::HlsManifest => streaming.hls_manifest_url.clone(),
        YoutubePlaybackResource::DashManifest => streaming.dash_manifest_url.clone(),
    }
    .ok_or(ProviderError::NotFound)
}

fn youtube_subtitles(
    player: &YoutubePlayerResponse,
    video_id: &str,
    credential_owner_id: Option<UserId>,
    provider_instance_name: Option<&str>,
    p2p_enabled: bool,
) -> Vec<PlaybackSubtitle> {
    let Some(tracklist) = player
        .captions
        .as_ref()
        .and_then(|captions| captions.player_captions_tracklist_renderer.as_ref())
    else {
        return Vec::new();
    };
    let mut subtitles = Vec::new();
    for track in &tracklist.caption_tracks {
        subtitles.push(PlaybackSubtitle {
            name: track.name.value(),
            language: track.language_code.clone(),
            format: "vtt".to_string(),
            p2p_swarm_id: p2p_enabled.then(|| {
                youtube_subtitle_swarm_id(video_id, &track.vss_id, None, provider_instance_name)
            }),
            provider: PlaybackSubtitleProvider::Youtube(PlaybackYoutubeSubtitle::Refresh {
                video_id: video_id.to_string(),
                track_id: track.vss_id.clone(),
                target_language_code: None,
                credential_owner_id,
                provider_instance_name: provider_instance_name.map(str::to_owned),
            }),
        });
        if track.is_translatable {
            subtitles.extend(tracklist.translation_languages.iter().map(|language| {
                PlaybackSubtitle {
                    name: format!(
                        "{} - {}",
                        track.name.value(),
                        language.language_name.value()
                    ),
                    language: language.language_code.clone(),
                    format: "vtt".to_string(),
                    p2p_swarm_id: p2p_enabled.then(|| {
                        youtube_subtitle_swarm_id(
                            video_id,
                            &track.vss_id,
                            Some(&language.language_code),
                            provider_instance_name,
                        )
                    }),
                    provider: PlaybackSubtitleProvider::Youtube(PlaybackYoutubeSubtitle::Refresh {
                        video_id: video_id.to_string(),
                        track_id: track.vss_id.clone(),
                        target_language_code: Some(language.language_code.clone()),
                        credential_owner_id,
                        provider_instance_name: provider_instance_name.map(str::to_owned),
                    }),
                }
            }));
        }
    }
    subtitles
}

fn youtube_subtitle_swarm_id(
    video_id: &str,
    track_id: &str,
    target_language_code: Option<&str>,
    provider_instance_name: Option<&str>,
) -> String {
    super::provider_p2p_swarm_id(
        YoutubeProvider::NAME,
        provider_instance_name,
        "subtitle",
        &format!(
            "video:{video_id}:track:{track_id}:target:{}:format:vtt",
            target_language_code.unwrap_or_default()
        ),
    )
}

fn youtube_caption_url(
    base_url: &str,
    target_language_code: Option<&str>,
) -> Result<reqwest::Url, ProviderError> {
    let mut url = reqwest::Url::parse(base_url).map_err(|error| {
        ProviderError::InvalidUrl(format!("Invalid YouTube subtitle URL: {error}"))
    })?;
    let source_query = url
        .query_pairs()
        .filter(|(key, _)| key != "fmt" && key != "tlang")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    url.set_query(None);
    url.query_pairs_mut()
        .extend_pairs(source_query)
        .append_pair("fmt", "vtt");
    if let Some(language_code) = target_language_code {
        url.query_pairs_mut().append_pair("tlang", language_code);
    }
    Ok(url)
}

#[allow(clippy::too_many_arguments)]
fn playback_info(
    name: String,
    format: String,
    resource: YoutubePlaybackResource,
    metadata: Option<PlaybackMediaMetadata>,
    thumbnail: Option<String>,
    subtitles: Vec<PlaybackSubtitle>,
    video_id: &str,
    credential_owner_id: Option<UserId>,
    provider_instance_name: Option<String>,
    p2p_enabled: bool,
) -> PlaybackInfo {
    let p2p_swarm_id = p2p_enabled.then(|| {
        super::provider_p2p_swarm_id(
            YoutubeProvider::NAME,
            provider_instance_name.as_deref(),
            "media",
            &youtube_media_resource_descriptor(video_id, &resource),
        )
    });
    PlaybackInfo {
        thumbnail,
        medias: vec![PlaybackMedia {
            name,
            format,
            expire_at: None,
            metadata,
            p2p_swarm_id,
            provider: PlaybackMediaProvider::Youtube(PlaybackYoutubeMedia::Refresh {
                video_id: video_id.to_string(),
                resource,
                credential_owner_id,
                provider_instance_name,
            }),
        }],
        default_media_index: Some(0),
        subtitles,
        default_subtitle_index: None,
        danmakus: Vec::new(),
        default_danmaku_index: None,
    }
}

fn youtube_media_resource_descriptor(video_id: &str, resource: &YoutubePlaybackResource) -> String {
    match resource {
        YoutubePlaybackResource::Format { itag } => {
            format!("video:{video_id}:format:{itag}")
        }
        YoutubePlaybackResource::HlsManifest => format!("video:{video_id}:hls"),
        YoutubePlaybackResource::DashManifest => format!("video:{video_id}:dash"),
    }
}

fn format_metadata(format: &YoutubeFormat) -> Option<PlaybackMediaMetadata> {
    Some(PlaybackMediaMetadata {
        resolution: format
            .width
            .zip(format.height)
            .map(|(width, height)| format!("{width}x{height}")),
        bitrate: i64::try_from(format.bitrate).ok(),
        codec: format.codecs().first().cloned(),
        fps: format.fps.and_then(|value| i32::try_from(value).ok()),
    })
}

fn mark_youtube_playback_resources(result: &mut PlaybackResult, version: &str, expires_at: i64) {
    for (mode_name, info) in &mut result.playback_infos {
        for (media_index, media) in info.medias.iter_mut().enumerate() {
            if matches!(
                media.provider,
                PlaybackMediaProvider::Youtube(PlaybackYoutubeMedia::Refresh { .. })
            ) {
                media.provider = PlaybackMediaProvider::Youtube(PlaybackYoutubeMedia::Proxy {
                    version: version.to_string(),
                    expires_at,
                    mode_name: mode_name.clone(),
                    media_index,
                });
            }
        }
        for (subtitle_index, subtitle) in info.subtitles.iter_mut().enumerate() {
            if matches!(
                subtitle.provider,
                PlaybackSubtitleProvider::Youtube(PlaybackYoutubeSubtitle::Refresh { .. })
            ) {
                subtitle.provider =
                    PlaybackSubtitleProvider::Youtube(PlaybackYoutubeSubtitle::Proxy {
                        version: version.to_string(),
                        expires_at,
                        mode_name: mode_name.clone(),
                        subtitle_index,
                    });
            }
        }
    }
}

#[async_trait]
impl MediaProvider for YoutubeProvider {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn as_dynamic_playlist_provider(&self) -> Option<&dyn DynamicPlaylistProvider> {
        Some(self)
    }

    async fn media_metadata(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &MediaSourceConfig,
    ) -> Result<Option<PlaybackMetadata>, ProviderError> {
        ctx.check_active()
            .map_err(|error| ProviderError::NetworkError(error.to_string()))?;
        let config = Self::media_config(source_config)?;
        let video_id = normalize_video_id(&config.video_id)?;
        let cache_key = serde_json::to_string(source_config)
            .map_err(|error| ProviderError::Internal(error.to_string()))?;
        super::cached_provider_metadata_or_fill(
            Self::NAME,
            &cache_key,
            Duration::from_secs(30),
            ctx,
            || async {
                let session = self
                    .session(ctx, Self::media_credential_policy(config))
                    .await?;
                let player = self
                    .client
                    .player(
                        &video_id,
                        session.visitor_data.as_deref(),
                        session.po_token.as_deref(),
                        session.cookie.as_deref(),
                    )
                    .await?;
                Ok(Some(PlaybackMetadata::Youtube(Self::metadata_model(
                    &player,
                )?)))
            },
        )
        .await
    }

    async fn generate_playback(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &MediaSourceConfig,
    ) -> Result<PlaybackResult, ProviderError> {
        ctx.check_active()
            .map_err(|error| ProviderError::NetworkError(error.to_string()))?;
        let config = Self::media_config(source_config)?;
        let video_id = normalize_video_id(&config.video_id)?;
        let credential_policy = Self::media_credential_policy(config);
        let owner_id = ctx.selected_credential_user_id(credential_policy);
        if credential_policy.uses_resource_owner() && owner_id.is_none() {
            return Err(ProviderError::Internal(
                "YouTube credential owner is unavailable".to_string(),
            ));
        }
        let session = self.session(ctx, credential_policy).await?;
        let instance_name = super::bound_provider_instance_name(ctx).map(ToString::to_string);
        let server_id = Self::credential_server_id_for_instance(instance_name.as_deref());
        let credential_partition =
            owner_id.map_or_else(|| "anonymous".to_string(), |id| id.to_string());
        let cache_key = format!("playback:{video_id}:{credential_partition}:{server_id}");
        let result = Box::pin(super::cached_versioned_playback_or_fill(
            Self::NAME,
            &cache_key,
            Duration::from_hours(5),
            ctx,
            mark_youtube_playback_resources,
            || async {
                let player = self
                    .client
                    .player(
                        &video_id,
                        session.visitor_data.as_deref(),
                        session.po_token.as_deref(),
                        session.cookie.as_deref(),
                    )
                    .await?;
                Self::playback_result(&player, owner_id, instance_name.as_deref())
            },
        ))
        .await?;
        super::filter_playback_routes_by_client(
            result,
            crate::models::PlaybackProxyMode::Only,
            ctx.playback_client_profile(),
        )
    }

    async fn validate_source_config(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<(), ProviderError> {
        match source_config {
            SourceConfig::Media(source) => {
                let config = Self::media_config(source)?;
                let session = self
                    .session(ctx, Self::media_credential_policy(config))
                    .await?;
                self.client
                    .player(
                        &config.video_id,
                        session.visitor_data.as_deref(),
                        session.po_token.as_deref(),
                        session.cookie.as_deref(),
                    )
                    .await?;
            }
            SourceConfig::DynamicPlaylist(source) => {
                let config = Self::playlist_config(source)?;
                let session = self
                    .session(ctx, Self::playlist_credential_policy(config))
                    .await?;
                self.list_page(config, None, &session).await?;
            }
        }
        Ok(())
    }

    fn credential_dependencies(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<Vec<ProviderCredentialDependency>, ProviderError> {
        let credential_policy = match source_config {
            SourceConfig::Media(source) => {
                Self::media_credential_policy(Self::media_config(source)?)
            }
            SourceConfig::DynamicPlaylist(source) => {
                Self::playlist_credential_policy(Self::playlist_config(source)?)
            }
        };
        let Some(owner_id) = ctx.selected_credential_user_id(credential_policy) else {
            return Ok(Vec::new());
        };
        Ok(vec![ProviderCredentialDependency::optional(
            crate::models::SourceProvider::Youtube,
            owner_id,
            Self::credential_server_id_for_instance(super::bound_provider_instance_name(ctx)),
        )])
    }

    async fn source_cover(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<Option<SourceCover>, ProviderError> {
        match source_config {
            SourceConfig::Media(source) => {
                let config = Self::media_config(source)?;
                let session = self
                    .session(ctx, Self::media_credential_policy(config))
                    .await?;
                let player = self
                    .client
                    .player(
                        &config.video_id,
                        session.visitor_data.as_deref(),
                        session.po_token.as_deref(),
                        session.cookie.as_deref(),
                    )
                    .await?;
                Ok(player
                    .video_details
                    .and_then(|details| details.thumbnail)
                    .and_then(|collection| collection.thumbnails.into_iter().last())
                    .map(|thumbnail| SourceCover::Url { url: thumbnail.url }))
            }
            SourceConfig::DynamicPlaylist(source) => {
                let config = Self::playlist_config(source)?;
                let session = self
                    .session(ctx, Self::playlist_credential_policy(config))
                    .await?;
                Ok(self
                    .list_page(config, None, &session)
                    .await?
                    .items
                    .into_iter()
                    .find_map(|item| item.thumbnail)
                    .map(|thumbnail| SourceCover::Url { url: thumbnail.url }))
            }
        }
    }
}

#[async_trait]
impl DynamicPlaylistProvider for YoutubeProvider {
    async fn list_playlist(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: Option<&ProviderTarget>,
        query: DynamicListQuery,
    ) -> Result<DynamicListResult, ProviderError> {
        if target.is_some() {
            return Err(ProviderError::InvalidConfig(
                "YouTube playlists have a single browse level".to_string(),
            ));
        }
        let config =
            Self::playlist_config(playlist.source_config.as_ref().ok_or_else(|| {
                ProviderError::InvalidConfig("Missing source_config".to_string())
            })?)?;
        let cursor = match &query.pagination {
            DynamicPagination::Cursor { cursor } => cursor.as_deref(),
            DynamicPagination::Page { page: 1 } => None,
            DynamicPagination::Page { .. } => {
                return Err(ProviderError::InvalidConfig(
                    "YouTube requires cursor pagination after the first page".to_string(),
                ));
            }
        };
        let credential_policy = Self::playlist_credential_policy(config);
        let session = self.session(ctx, credential_policy).await?;
        let page = self.list_page(config, cursor, &session).await?;
        let has_more = page.next_cursor.is_some();
        Ok(DynamicListResult {
            items: page
                .items
                .into_iter()
                .map(|item| Self::directory_item(item, credential_policy))
                .collect(),
            pagination: DynamicPagination::Cursor {
                cursor: page.next_cursor,
            },
            has_more,
            supports_search: false,
        })
    }

    async fn resolve_item(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: &ProviderTarget,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        Self::decode_target(target)?;
        let base =
            Self::playlist_config(playlist.source_config.as_ref().ok_or_else(|| {
                ProviderError::InvalidConfig("Missing source_config".to_string())
            })?)?;
        let mut cursor = None;
        loop {
            let result = self
                .list_playlist(
                    ctx,
                    playlist,
                    None,
                    DynamicListQuery {
                        pagination: DynamicPagination::Cursor { cursor },
                        page_size: DYNAMIC_PAGE_SIZE,
                        ..DynamicListQuery::default()
                    },
                )
                .await?;
            if let Some(item) = result.items.iter().find(|item| &item.target == target) {
                return Self::next_item(base, item).map(Some);
            }
            match result.pagination {
                DynamicPagination::Cursor { cursor: Some(next) } if result.has_more => {
                    cursor = Some(next);
                }
                _ => return Ok(None),
            }
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
        Self::decode_target(target)?;
        let base =
            Self::playlist_config(playlist.source_config.as_ref().ok_or_else(|| {
                ProviderError::InvalidConfig("Missing source_config".to_string())
            })?)?;
        let mut cursor = None;
        let mut first = None;
        let mut found_current = false;
        let mut shuffle = Vec::new();
        loop {
            let result = self
                .list_playlist(
                    ctx,
                    playlist,
                    None,
                    DynamicListQuery {
                        pagination: DynamicPagination::Cursor { cursor },
                        page_size: DYNAMIC_PAGE_SIZE,
                        ..DynamicListQuery::default()
                    },
                )
                .await?;
            let next_cursor = match &result.pagination {
                DynamicPagination::Cursor { cursor } if result.has_more => cursor.clone(),
                _ => None,
            };
            for item in result.items {
                first.get_or_insert_with(|| item.clone());
                if play_mode == PlayMode::Shuffle {
                    if item.target != *target && shuffle.len() < DYNAMIC_SHUFFLE_LIMIT {
                        shuffle.push(item);
                    }
                    continue;
                }
                if found_current {
                    return Self::next_item(base, &item).map(Some);
                }
                found_current = item.target == *target;
            }
            let Some(next) = next_cursor else {
                break;
            };
            if play_mode == PlayMode::Shuffle && shuffle.len() >= DYNAMIC_SHUFFLE_LIMIT {
                break;
            }
            cursor = Some(next);
        }
        let selected = match play_mode {
            PlayMode::RepeatAll if found_current => first.as_ref(),
            PlayMode::Shuffle => shuffle
                .get(rand::random_range(0..shuffle.len().max(1)))
                .or(first.as_ref()),
            PlayMode::Sequential | PlayMode::RepeatAll | PlayMode::RepeatOne => None,
        };
        selected.map(|item| Self::next_item(base, item)).transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player_response(is_live: bool) -> YoutubePlayerResponse {
        serde_json::from_value(serde_json::json!({
            "playabilityStatus": {"status": "OK"},
            "streamingData": {
                "expiresInSeconds": "21600",
                "formats": [
                    {
                        "itag": 18,
                        "url": "https://video.example/360.mp4",
                        "mimeType": "video/mp4; codecs=\"avc1.42001E, mp4a.40.2\"",
                        "bitrate": 500_000,
                        "width": 640,
                        "height": 360,
                        "fps": 30,
                        "qualityLabel": "360p"
                    },
                    {
                        "itag": 22,
                        "url": "https://video.example/720.mp4",
                        "mimeType": "video/mp4; codecs=\"avc1.64001F, mp4a.40.2\"",
                        "bitrate": 1_500_000,
                        "width": 1280,
                        "height": 720,
                        "fps": 60,
                        "qualityLabel": "720p"
                    }
                ],
                "adaptiveFormats": [
                    {
                        "itag": 137,
                        "url": "https://video.example/1080-video.mp4",
                        "mimeType": "video/mp4; codecs=\"avc1.640028\"",
                        "bitrate": 3_000_000,
                        "width": 1920,
                        "height": 1080,
                        "fps": 60,
                        "qualityLabel": "1080p"
                    }
                ],
                "hlsManifestUrl": "https://manifest.example/live.m3u8",
                "dashManifestUrl": "https://manifest.example/video.mpd"
            },
            "videoDetails": {
                "videoId": "dQw4w9WgXcQ",
                "title": "Example",
                "lengthSeconds": "212",
                "channelId": "UC-example",
                "author": "Example Channel",
                "shortDescription": "Description",
                "viewCount": "123456",
                "isLive": is_live,
                "isLiveContent": is_live,
                "thumbnail": {
                    "thumbnails": [
                        {"url": "https://img.example/default.jpg", "width": 120, "height": 90},
                        {"url": "https://img.example/maxres.jpg", "width": 1280, "height": 720}
                    ]
                }
            },
            "captions": {
                "playerCaptionsTracklistRenderer": {
                    "captionTracks": [
                        {
                            "baseUrl": "https://caption.example/en",
                            "name": {"simpleText": "English"},
                            "vssId": ".en",
                            "languageCode": "en",
                            "isTranslatable": true
                        },
                        {
                            "baseUrl": "https://caption.example/ja-auto",
                            "name": {"simpleText": "Japanese (auto)"},
                            "vssId": "a.ja",
                            "languageCode": "ja",
                            "kind": "asr",
                            "isTranslatable": true
                        }
                    ],
                    "translationLanguages": [
                        {"languageCode": "zh-Hans", "languageName": {"simpleText": "Chinese"}}
                    ]
                }
            },
            "storyboards": {
                "playerStoryboardSpecRenderer": {"spec": "https://i.ytimg.com/sb/storyboard"}
            },
            "microformat": {
                "playerMicroformatRenderer": {
                    "publishDate": "2025-01-02",
                    "uploadDate": "2025-01-01",
                    "category": "Entertainment"
                }
            }
        }))
        .expect("fixture should deserialize")
    }

    #[test]
    fn playback_result_maps_formats_manifests_subtitles_and_metadata() {
        let result = YoutubeProvider::playback_result(
            &player_response(false),
            Some(UserId::expect_positive(1)),
            Some("primary"),
        )
        .expect("player response should map");

        assert_eq!(result.default_mode, "progressive");
        assert_eq!(result.duration_seconds, Some(212.0));
        assert_eq!(
            result.playback_kind,
            Some(crate::models::PlaybackKind::Regular)
        );
        assert!(result.playback_infos.contains_key("progressive"));
        assert!(result.playback_infos.contains_key("hls"));
        assert!(result.playback_infos.contains_key("dash"));

        let progressive = &result.playback_infos["progressive"];
        assert_eq!(progressive.medias.len(), 2);
        assert_eq!(progressive.default_media_index, Some(1));
        assert_eq!(
            progressive.thumbnail.as_deref(),
            Some("https://img.example/maxres.jpg")
        );
        assert_eq!(progressive.subtitles.len(), 4);
        assert!(progressive
            .medias
            .iter()
            .all(|media| media.p2p_swarm_id.is_some()));
        assert!(progressive
            .subtitles
            .iter()
            .all(|subtitle| subtitle.p2p_swarm_id.is_some()));
        assert!(progressive.subtitles.iter().any(|subtitle| {
            subtitle.language == "zh-Hans"
                && matches!(
                    &subtitle.provider,
                    PlaybackSubtitleProvider::Youtube(PlaybackYoutubeSubtitle::Refresh {
                        track_id,
                        target_language_code: Some(language),
                        ..
                    }) if track_id == ".en" && language == "zh-Hans"
                )
        }));
        assert!(matches!(
            progressive.medias[1].provider,
            PlaybackMediaProvider::Youtube(PlaybackYoutubeMedia::Refresh {
                resource: YoutubePlaybackResource::Format { itag: 22 },
                ..
            })
        ));
        assert!(matches!(
            result.playback_infos["hls"].medias[0].provider,
            PlaybackMediaProvider::Youtube(PlaybackYoutubeMedia::Refresh {
                resource: YoutubePlaybackResource::HlsManifest,
                ..
            })
        ));
        assert!(matches!(
            result.playback_infos["dash"].medias[0].provider,
            PlaybackMediaProvider::Youtube(PlaybackYoutubeMedia::Refresh {
                resource: YoutubePlaybackResource::DashManifest,
                ..
            })
        ));

        let Some(PlaybackMetadata::Youtube(metadata)) = result.metadata else {
            panic!("YouTube metadata should be present");
        };
        assert_eq!(metadata.video_id, "dQw4w9WgXcQ");
        assert_eq!(metadata.view_count, Some(123_456));
        assert_eq!(metadata.manual_caption_count, 1);
        assert_eq!(metadata.automatic_caption_count, 1);
        assert_eq!(metadata.translation_languages, ["zh-Hans"]);
        assert_eq!(
            metadata.storyboard_spec.as_deref(),
            Some("https://i.ytimg.com/sb/storyboard")
        );
    }

    #[test]
    fn live_playback_prefers_hls_and_marks_proxy_resources() {
        let mut result = YoutubeProvider::playback_result(
            &player_response(true),
            Some(UserId::expect_positive(1)),
            None,
        )
        .expect("live player response should map");

        assert_eq!(result.default_mode, "hls");
        assert!(result.playback_infos.values().all(|info| {
            info.medias.iter().all(|media| media.p2p_swarm_id.is_none())
                && info
                    .subtitles
                    .iter()
                    .all(|subtitle| subtitle.p2p_swarm_id.is_none())
        }));
        mark_youtube_playback_resources(&mut result, "version-1", 1234);

        for (mode_name, info) in &result.playback_infos {
            assert!(info.medias.iter().all(|media| media.p2p_swarm_id.is_none()));
            assert!(info
                .subtitles
                .iter()
                .all(|subtitle| subtitle.p2p_swarm_id.is_none()));
            assert!(matches!(
                &info.medias[0].provider,
                PlaybackMediaProvider::Youtube(PlaybackYoutubeMedia::Proxy {
                    version,
                    expires_at: 1234,
                    mode_name: proxy_mode,
                    media_index: 0,
                }) if version == "version-1" && proxy_mode == mode_name
            ));
            for (subtitle_index, subtitle) in info.subtitles.iter().enumerate() {
                assert!(matches!(
                    &subtitle.provider,
                    PlaybackSubtitleProvider::Youtube(PlaybackYoutubeSubtitle::Proxy {
                        version,
                        expires_at: 1234,
                        mode_name: proxy_mode,
                        subtitle_index: proxy_index,
                    }) if version == "version-1"
                        && proxy_mode == mode_name
                        && *proxy_index == subtitle_index
                ));
            }
        }
    }

    #[test]
    fn directory_item_contains_typed_media_source_config() {
        let item = YoutubeProvider::directory_item(
            YoutubeListItem {
                video_id: "dQw4w9WgXcQ".to_string(),
                title: "Example".to_string(),
                ..YoutubeListItem::default()
            },
            ProviderCredentialPolicy::ResourceOwner,
        );

        let Some(DynamicPlaylistItemSourceConfig::Media(MediaSourceConfig::Youtube(config))) =
            item.source_config
        else {
            panic!("YouTube directory item should contain typed source config");
        };
        assert_eq!(config.video_id, "dQw4w9WgXcQ");
        assert!(config.shared);
    }

    #[test]
    fn provider_exposes_dynamic_playlist_capability() {
        crate::install_process_crypto_provider();
        let provider = YoutubeProvider::new();

        assert!(provider.as_dynamic_playlist_provider().is_some());
    }

    #[test]
    fn translated_caption_url_replaces_format_and_target_language() {
        let url = youtube_caption_url(
            "https://caption.example/api?v=dQw4w9WgXcQ&lang=en&fmt=srv3&tlang=fr",
            Some("zh-Hans"),
        )
        .expect("caption URL should build");
        let query = url.query_pairs().collect::<Vec<_>>();

        let value = |key: &str| {
            query
                .iter()
                .filter(|(candidate, _)| candidate == key)
                .map(|(_, value)| value.as_ref())
                .collect::<Vec<_>>()
        };

        assert_eq!(value("v"), vec!["dQw4w9WgXcQ"]);
        assert_eq!(value("lang"), vec!["en"]);
        assert_eq!(value("fmt"), vec!["vtt"]);
        assert_eq!(value("tlang"), vec!["zh-Hans"]);
    }
}
