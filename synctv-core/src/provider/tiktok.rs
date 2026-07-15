//! TikTok media provider adapter.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use sha2::{Digest, Sha256};

use super::{
    DirectoryItem, DirectoryItemThumbnail, DynamicFolder, DynamicListQuery, DynamicListResult,
    DynamicPagination, ItemType, MediaProvider, NextPlayItem, PlaybackInfo, PlaybackResult,
    ProviderContext, ProviderCredentialDependency, ProviderError, SourceConfig, SourceCover,
};
use crate::models::{
    MediaSourceConfig, PlayMode, PlaybackMedia, PlaybackMediaMetadata, PlaybackMediaProvider,
    PlaybackMetadata, PlaybackSubtitle, PlaybackSubtitleProvider, PlaybackTikTokMedia,
    PlaybackTikTokSubtitle, PlaylistSourceConfig, ProviderCredential, ProviderTarget,
    TikTokMediaSourceConfig, TikTokPlaybackMetadata, TikTokPlaybackResource,
    TikTokPlaylistSourceConfig, UserId, UserProviderCredential,
};
use crate::repository::UserProviderCredentialRepository;
use synctv_media_providers::tiktok::{
    TikTokClient, TikTokListItem, TikTokMedia, TikTokMediaKind, TikTokSession, TikTokStreamFormat,
    TikTokVariant,
};

const PAGE_SIZE: usize = 20;
const SHUFFLE_LIMIT: usize = 200;

pub struct TikTokProvider {
    client: TikTokClient,
    credential_repo: Option<Arc<UserProviderCredentialRepository>>,
}

#[derive(Debug, Clone)]
pub struct TikTokBind {
    pub id: i64,
    pub server_id: String,
    pub label: String,
    pub created_at: i64,
    pub provider_instance_name: Option<String>,
}

impl Default for TikTokProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl TikTokProvider {
    pub const NAME: &'static str = "tiktok";

    #[must_use]
    pub fn new() -> Self {
        Self {
            client: TikTokClient::new().expect("TikTok HTTP client should build"),
            credential_repo: None,
        }
    }

    #[must_use]
    pub fn with_http_client(client: reqwest::Client) -> Self {
        Self {
            client: TikTokClient::with_http_client(client),
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
            format!("tiktok\n{instance_name}").as_bytes(),
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
        shared: bool,
    ) -> Result<TikTokSession, ProviderError> {
        let Some(repo) = self.credential_repo_or(ctx.credential_repo) else {
            return Ok(TikTokSession::default());
        };
        let owner_id = if shared {
            ctx.credential_owner_id()
        } else {
            ctx.user_id()
        };
        let Some(owner_id) = owner_id else {
            return Ok(TikTokSession::default());
        };
        self.session_for_owner(
            repo,
            *owner_id,
            &Self::credential_server_id_for_instance(super::bound_provider_instance_name(ctx)),
        )
        .await
    }

    async fn session_for_owner(
        &self,
        repo: &UserProviderCredentialRepository,
        owner_id: UserId,
        server_id: &str,
    ) -> Result<TikTokSession, ProviderError> {
        let Some(credential) = repo
            .get_by_provider_and_server(owner_id, Self::NAME, server_id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
        else {
            return Ok(TikTokSession::default());
        };
        match credential.credential_data {
            ProviderCredential::TikTok { cookie, .. } => Ok(TikTokSession {
                cookie: Some(cookie),
            }),
            _ => Err(ProviderError::InvalidCredentialType),
        }
    }

    async fn stored_session(
        &self,
        owner_id: UserId,
        provider_instance_name: Option<&str>,
    ) -> Result<TikTokSession, ProviderError> {
        let Some(repo) = self.credential_repo.as_deref() else {
            return Ok(TikTokSession::default());
        };
        self.session_for_owner(
            repo,
            owner_id,
            &Self::credential_server_id_for_instance(provider_instance_name),
        )
        .await
    }

    pub async fn persist_session(
        &self,
        user_id: UserId,
        label: String,
        cookie: String,
        provider_instance_name: Option<String>,
    ) -> Result<String, ProviderError> {
        let label = label.trim();
        let cookie = cookie.trim();
        if label.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "TikTok credential label is required".to_string(),
            ));
        }
        if cookie.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "TikTok cookie is required".to_string(),
            ));
        }
        let provider_instance_name =
            crate::models::normalize_provider_instance_name_owned(provider_instance_name);
        let server_id = Self::credential_server_id_for_instance(provider_instance_name.as_deref());
        let now = Utc::now();
        self.credential_repo
            .as_deref()
            .ok_or_else(|| {
                ProviderError::Internal("TikTok credential repository is unavailable".to_string())
            })?
            .upsert_by_user_provider_server(&UserProviderCredential {
                id: 0,
                user_id,
                provider: Self::NAME.to_string(),
                server_id: server_id.clone(),
                provider_instance_name,
                credential_data: ProviderCredential::TikTok {
                    label: label.to_string(),
                    cookie: cookie.to_string(),
                },
                expires_at: None,
                created_at: now,
                updated_at: now,
            })
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?;
        Ok(server_id)
    }

    pub async fn list_binds(
        &self,
        user_id: UserId,
        provider_instance_name: Option<&str>,
    ) -> Result<Vec<TikTokBind>, ProviderError> {
        let requested = crate::models::normalize_provider_instance_name(provider_instance_name);
        self.credential_repo
            .as_deref()
            .ok_or_else(|| {
                ProviderError::Internal("TikTok credential repository is unavailable".to_string())
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
                let ProviderCredential::TikTok { label, .. } = credential.credential_data else {
                    return Err(ProviderError::InvalidCredentialType);
                };
                Ok(TikTokBind {
                    id: credential.id,
                    server_id: credential.server_id,
                    label,
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
            ProviderError::Internal("TikTok credential repository is unavailable".to_string())
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
    ) -> Result<TikTokMedia, ProviderError> {
        let session = self.stored_session(user_id, provider_instance_name).await?;
        Ok(self.client.resolve(resource, Some(&session)).await?)
    }

    pub async fn list_user_posts_for_user(
        &self,
        user_id: UserId,
        sec_uid: &str,
        cursor: Option<&str>,
        page_size: u32,
        provider_instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::tiktok::TikTokListPage, ProviderError> {
        let session = self.stored_session(user_id, provider_instance_name).await?;
        Ok(self
            .client
            .user_posts(sec_uid, cursor, page_size, Some(&session))
            .await?)
    }

    pub async fn user_sec_uid_for_user(
        &self,
        user_id: UserId,
        unique_id: &str,
        provider_instance_name: Option<&str>,
    ) -> Result<String, ProviderError> {
        let session = self.stored_session(user_id, provider_instance_name).await?;
        Ok(self.client.user_sec_uid(unique_id, Some(&session)).await?)
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
        let PlaybackMediaProvider::TikTok(PlaybackTikTokMedia::Refresh {
            resource,
            variant_key,
            credential_owner_id,
            provider_instance_name,
        }) = &media.provider
        else {
            return Err(ProviderError::InvalidConfig(
                "TikTok cached playback resource is invalid".to_string(),
            ));
        };
        let session = self
            .stored_session(*credential_owner_id, provider_instance_name.as_deref())
            .await?;
        let resolved = match resource {
            TikTokPlaybackResource::Video { video_id } => {
                self.client.video(video_id, Some(&session)).await?
            }
            TikTokPlaybackResource::Live { unique_id } => {
                self.client.live(unique_id, Some(&session)).await?
            }
        };
        let variant = resolved
            .variants
            .into_iter()
            .find(|variant| variant_key_for(variant) == *variant_key)
            .ok_or(ProviderError::NotFound)?;
        super::playback_transport::transport_action_for_target_url(
            variant.url,
            tiktok_headers(session.cookie.as_deref(), resolved.metadata.kind),
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
            tiktok_headers(None, TikTokMediaKind::Live),
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
        let PlaybackSubtitleProvider::TikTok(PlaybackTikTokSubtitle::Refresh {
            resource,
            language,
            format,
            credential_owner_id,
            provider_instance_name,
        }) = &subtitle.provider
        else {
            return Err(ProviderError::InvalidConfig(
                "TikTok cached subtitle resource is invalid".to_string(),
            ));
        };
        let session = self
            .stored_session(*credential_owner_id, provider_instance_name.as_deref())
            .await?;
        let media = match resource {
            TikTokPlaybackResource::Video { video_id } => {
                self.client.video(video_id, Some(&session)).await?
            }
            TikTokPlaybackResource::Live { .. } => return Err(ProviderError::NotFound),
        };
        let subtitle = media
            .metadata
            .subtitles
            .into_iter()
            .find(|subtitle| subtitle.language == *language && subtitle.format == *format)
            .ok_or(ProviderError::NotFound)?;
        super::playback_transport::transport_action_for_target_url(
            subtitle.url,
            tiktok_headers(session.cookie.as_deref(), TikTokMediaKind::Video),
            range_header,
        )
    }

    fn media_config(config: &MediaSourceConfig) -> Result<&TikTokMediaSourceConfig, ProviderError> {
        match config {
            MediaSourceConfig::TikTok(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "TikTok provider requires TikTok media source_config".to_string(),
            )),
        }
    }

    fn playlist_config(
        config: &PlaylistSourceConfig,
    ) -> Result<&TikTokPlaylistSourceConfig, ProviderError> {
        match config {
            PlaylistSourceConfig::TikTok(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "TikTok provider requires TikTok playlist source_config".to_string(),
            )),
        }
    }

    const fn media_shared(config: &TikTokMediaSourceConfig) -> bool {
        match config {
            TikTokMediaSourceConfig::Video { shared, .. }
            | TikTokMediaSourceConfig::Live { shared, .. } => *shared,
        }
    }

    fn resource(config: &TikTokMediaSourceConfig) -> TikTokPlaybackResource {
        match config {
            TikTokMediaSourceConfig::Video { video_id, .. } => TikTokPlaybackResource::Video {
                video_id: video_id.clone(),
            },
            TikTokMediaSourceConfig::Live { unique_id, .. } => TikTokPlaybackResource::Live {
                unique_id: unique_id.clone(),
            },
        }
    }

    async fn resolve_media(
        &self,
        config: &TikTokMediaSourceConfig,
        session: &TikTokSession,
    ) -> Result<TikTokMedia, ProviderError> {
        Ok(match config {
            TikTokMediaSourceConfig::Video { video_id, .. } => {
                self.client.video(video_id, Some(session)).await?
            }
            TikTokMediaSourceConfig::Live { unique_id, .. } => {
                self.client.live(unique_id, Some(session)).await?
            }
        })
    }

    fn playback_result(
        media: TikTokMedia,
        resource: &TikTokPlaybackResource,
        credential_owner_id: UserId,
        provider_instance_name: Option<&str>,
    ) -> Result<PlaybackResult, ProviderError> {
        let mut infos = HashMap::new();
        let subtitle_count = media.metadata.subtitles.len();
        let subtitles = media
            .metadata
            .subtitles
            .iter()
            .map(|subtitle| PlaybackSubtitle {
                name: subtitle.language.clone(),
                language: subtitle.language.clone(),
                format: subtitle.format.clone(),
                provider: PlaybackSubtitleProvider::TikTok(PlaybackTikTokSubtitle::Refresh {
                    resource: resource.clone(),
                    language: subtitle.language.clone(),
                    format: subtitle.format.clone(),
                    credential_owner_id,
                    provider_instance_name: provider_instance_name.map(str::to_owned),
                }),
            })
            .collect::<Vec<_>>();
        for (index, variant) in media.variants.iter().enumerate() {
            let mode = unique_mode_name(&infos, variant, index);
            infos.insert(
                mode,
                PlaybackInfo {
                    thumbnail: media.metadata.cover.as_ref().map(|cover| cover.url.clone()),
                    medias: vec![PlaybackMedia {
                        name: variant.quality.clone(),
                        format: stream_format(variant.format).to_string(),
                        expire_at: None,
                        metadata: Some(PlaybackMediaMetadata {
                            resolution: variant
                                .width
                                .zip(variant.height)
                                .map(|(width, height)| format!("{width}x{height}")),
                            bitrate: variant.bitrate.and_then(|value| i64::try_from(value).ok()),
                            codec: variant.codec.clone(),
                            fps: None,
                        }),
                        provider: PlaybackMediaProvider::TikTok(PlaybackTikTokMedia::Refresh {
                            resource: resource.clone(),
                            variant_key: variant_key_for(variant),
                            credential_owner_id,
                            provider_instance_name: provider_instance_name.map(str::to_owned),
                        }),
                    }],
                    default_media_index: Some(0),
                    subtitles: subtitles.clone(),
                    default_subtitle_index: (!subtitles.is_empty()).then_some(0),
                    danmakus: Vec::new(),
                    default_danmaku_index: None,
                },
            );
        }
        if infos.is_empty() {
            return Err(ProviderError::ApiError(
                if media.metadata.kind == TikTokMediaKind::Live && !media.metadata.is_live {
                    "TikTok live room is offline"
                } else {
                    "TikTok returned no playable variants"
                }
                .to_string(),
            ));
        }
        let default_mode = infos
            .keys()
            .find(|mode| mode.contains("origin") && mode.contains("hls"))
            .cloned()
            .or_else(|| infos.keys().next().cloned())
            .ok_or_else(|| ProviderError::ApiError("TikTok playback is empty".to_string()))?;
        Ok(PlaybackResult {
            playback_infos: infos,
            default_mode,
            provider: Self::NAME.to_string(),
            provider_instance_name: None,
            duration_seconds: media
                .metadata
                .duration_ms
                .map(|duration| std::time::Duration::from_millis(duration).as_secs_f64()),
            is_live: Some(media.metadata.is_live),
            metadata: Some(PlaybackMetadata::TikTok(TikTokPlaybackMetadata {
                id: media.metadata.id,
                kind: match media.metadata.kind {
                    TikTokMediaKind::Video => "video",
                    TikTokMediaKind::Live => "live",
                }
                .to_string(),
                author_id: media.metadata.author.id,
                author_sec_uid: media.metadata.author.sec_uid,
                author_unique_id: media.metadata.author.unique_id,
                author_name: media.metadata.author.nickname,
                description: media.metadata.description,
                view_count: media.metadata.view_count,
                like_count: media.metadata.like_count,
                comment_count: media.metadata.comment_count,
                share_count: media.metadata.share_count,
                collect_count: media.metadata.collect_count,
                concurrent_viewers: media.metadata.concurrent_viewers,
                created_at: media.metadata.created_at,
                music_title: media.metadata.music_title,
                music_author: media.metadata.music_author,
                subtitle_count,
                is_live: media.metadata.is_live,
                room_id: media.room_id,
            })),
        })
    }

    fn directory_item(item: TikTokListItem) -> DirectoryItem {
        DirectoryItem {
            name: item.title,
            item_type: ItemType::Media,
            target: ProviderTarget::tiktok(item.video_id),
            size: None,
            thumbnail: item
                .cover
                .map(|cover| DirectoryItemThumbnail::Url(cover.url)),
            description: Some(item.author.nickname).filter(|value| !value.is_empty()),
            modified_at: item.created_at,
            source_config: None,
        }
    }

    fn decode_target(target: &ProviderTarget) -> Result<&str, ProviderError> {
        let ProviderTarget::TikTok(target) = target else {
            return Err(ProviderError::InvalidConfig(
                "TikTok playlist requires a TikTok target".to_string(),
            ));
        };
        Ok(&target.video_id)
    }

    fn next_item(
        config: &TikTokPlaylistSourceConfig,
        item: &DirectoryItem,
    ) -> Result<NextPlayItem, ProviderError> {
        Ok(NextPlayItem {
            name: item.name.clone(),
            item_type: ItemType::Media,
            source_config: MediaSourceConfig::TikTok(TikTokMediaSourceConfig::Video {
                video_id: Self::decode_target(&item.target)?.to_string(),
                shared: config.shared,
            }),
            target: item.target.clone(),
        })
    }

    fn scan_ordered_page(
        items: Vec<DirectoryItem>,
        target: &ProviderTarget,
        first: &mut Option<DirectoryItem>,
        found_current: &mut bool,
    ) -> Option<DirectoryItem> {
        for item in items {
            first.get_or_insert_with(|| item.clone());
            if *found_current {
                return Some(item);
            }
            *found_current = item.target == *target;
        }
        None
    }
}

fn mark_playback_resources(result: &mut PlaybackResult, version: &str, expires_at: i64) {
    for (mode_name, info) in &mut result.playback_infos {
        for (media_index, media) in info.medias.iter_mut().enumerate() {
            if matches!(
                media.provider,
                PlaybackMediaProvider::TikTok(PlaybackTikTokMedia::Refresh { .. })
            ) {
                media.provider = PlaybackMediaProvider::TikTok(PlaybackTikTokMedia::Proxy {
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
                PlaybackSubtitleProvider::TikTok(PlaybackTikTokSubtitle::Refresh { .. })
            ) {
                subtitle.provider =
                    PlaybackSubtitleProvider::TikTok(PlaybackTikTokSubtitle::Proxy {
                        version: version.to_string(),
                        expires_at,
                        mode_name: mode_name.clone(),
                        subtitle_index,
                    });
            }
        }
    }
}

fn stream_format(format: TikTokStreamFormat) -> &'static str {
    match format {
        TikTokStreamFormat::Mp4 => "mp4",
        TikTokStreamFormat::Flv => "flv",
        TikTokStreamFormat::Hls => "m3u8",
        TikTokStreamFormat::Audio => "m4a",
    }
}

fn variant_key_for(variant: &TikTokVariant) -> String {
    format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        stream_format(variant.format),
        variant.quality,
        variant.codec.as_deref().unwrap_or_default(),
        variant.width.unwrap_or_default(),
        variant.height.unwrap_or_default(),
        variant.audio_only
    )
}

fn unique_mode_name(
    infos: &HashMap<String, PlaybackInfo>,
    variant: &TikTokVariant,
    index: usize,
) -> String {
    let base = format!("{}_{}", variant.quality, stream_format(variant.format))
        .to_ascii_lowercase()
        .chars()
        .map(|value| {
            if value.is_ascii_alphanumeric() {
                value
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string();
    let base = if base.is_empty() {
        format!("variant_{index}")
    } else {
        base
    };
    if infos.contains_key(&base) {
        format!("{base}_{index}")
    } else {
        base
    }
}

fn tiktok_headers(cookie: Option<&str>, kind: TikTokMediaKind) -> HashMap<String, String> {
    let origin = match kind {
        TikTokMediaKind::Video | TikTokMediaKind::Live => "https://www.tiktok.com",
    };
    let mut headers = HashMap::from([
        ("Origin".to_string(), origin.to_string()),
        ("Referer".to_string(), format!("{origin}/")),
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

#[async_trait]
impl MediaProvider for TikTokProvider {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    async fn generate_playback(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &MediaSourceConfig,
    ) -> Result<PlaybackResult, ProviderError> {
        ctx.check_active()
            .map_err(|error| ProviderError::NetworkError(error.to_string()))?;
        let config = Self::media_config(source_config)?;
        let shared = Self::media_shared(config);
        let credential_owner_id = if shared {
            *ctx.credential_owner_id().ok_or_else(|| {
                ProviderError::Internal("TikTok credential owner is unavailable".to_string())
            })?
        } else {
            *ctx.user_id().ok_or_else(|| {
                ProviderError::Internal("TikTok viewer is unavailable".to_string())
            })?
        };
        let session = self.session(ctx, shared).await?;
        let provider_instance_name =
            super::bound_provider_instance_name(ctx).map(ToString::to_string);
        let resource = Self::resource(config);
        let cache_key = format!(
            "playback:{}:{}:{}",
            serde_json::to_string(&resource)
                .map_err(|error| ProviderError::Internal(error.to_string()))?,
            credential_owner_id,
            Self::credential_server_id_for_instance(provider_instance_name.as_deref())
        );
        super::cached_versioned_playback_or_fill(
            Self::NAME,
            &cache_key,
            Duration::from_mins(30),
            ctx,
            mark_playback_resources,
            || async {
                let media = self.resolve_media(config, &session).await?;
                Self::playback_result(
                    media,
                    &resource,
                    credential_owner_id,
                    provider_instance_name.as_deref(),
                )
            },
        )
        .await
    }

    fn as_dynamic_folder(&self) -> Option<&dyn DynamicFolder> {
        Some(self)
    }

    async fn validate_source_config(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<(), ProviderError> {
        match source_config {
            SourceConfig::Media(source) => {
                let config = Self::media_config(source)?;
                let session = self.session(ctx, Self::media_shared(config)).await?;
                self.resolve_media(config, &session).await?;
            }
            SourceConfig::DynamicPlaylist(source) => {
                let config = Self::playlist_config(source)?;
                let session = self.session(ctx, config.shared).await?;
                self.client
                    .user_posts(&config.sec_uid, None, 1, Some(&session))
                    .await?;
            }
        }
        Ok(())
    }

    fn credential_dependencies(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<Vec<ProviderCredentialDependency>, ProviderError> {
        let shared = match source_config {
            SourceConfig::Media(source) => Self::media_shared(Self::media_config(source)?),
            SourceConfig::DynamicPlaylist(source) => Self::playlist_config(source)?.shared,
        };
        let owner_id = if shared {
            ctx.credential_owner_id()
        } else {
            ctx.user_id()
        }
        .ok_or_else(|| {
            ProviderError::Internal("TikTok credential owner is unavailable".to_string())
        })?;
        Ok(vec![ProviderCredentialDependency::optional(
            Self::NAME,
            owner_id.to_string(),
            Self::credential_server_id_for_instance(super::bound_provider_instance_name(ctx)),
        )])
    }

    async fn source_cover(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<Option<SourceCover>, ProviderError> {
        let cover = match source_config {
            SourceConfig::Media(source) => {
                let config = Self::media_config(source)?;
                let session = self.session(ctx, Self::media_shared(config)).await?;
                self.resolve_media(config, &session).await?.metadata.cover
            }
            SourceConfig::DynamicPlaylist(source) => {
                let config = Self::playlist_config(source)?;
                let session = self.session(ctx, config.shared).await?;
                self.client
                    .user_posts(&config.sec_uid, None, 1, Some(&session))
                    .await?
                    .items
                    .into_iter()
                    .find_map(|item| item.cover)
            }
        };
        Ok(cover.map(|cover| SourceCover::Url { url: cover.url }))
    }
}

#[async_trait]
impl DynamicFolder for TikTokProvider {
    async fn list_playlist(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: Option<&ProviderTarget>,
        query: DynamicListQuery,
    ) -> Result<DynamicListResult, ProviderError> {
        if target.is_some() {
            return Err(ProviderError::InvalidConfig(
                "TikTok user playlists have a single browse level".to_string(),
            ));
        }
        if query
            .search
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            return Err(ProviderError::InvalidConfig(
                "TikTok user playlists do not expose server-side search".to_string(),
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
                    "TikTok requires cursor pagination after the first page".to_string(),
                ));
            }
        };
        let count = u32::try_from(query.page_size.clamp(1, 50)).map_err(|_| {
            ProviderError::InvalidConfig("TikTok page size exceeds u32::MAX".to_string())
        })?;
        let session = self.session(ctx, config.shared).await?;
        let page = self
            .client
            .user_posts(&config.sec_uid, cursor, count, Some(&session))
            .await?;
        Ok(DynamicListResult {
            items: page.items.into_iter().map(Self::directory_item).collect(),
            pagination: DynamicPagination::Cursor {
                cursor: page.cursor,
            },
            has_more: page.has_more,
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
                        page_size: PAGE_SIZE,
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
                        page_size: PAGE_SIZE,
                        ..DynamicListQuery::default()
                    },
                )
                .await?;
            let next_cursor = match &result.pagination {
                DynamicPagination::Cursor { cursor } if result.has_more => cursor.clone(),
                _ => None,
            };
            if play_mode == PlayMode::Shuffle {
                for item in result.items {
                    first.get_or_insert_with(|| item.clone());
                    if item.target != *target && shuffle.len() < SHUFFLE_LIMIT {
                        shuffle.push(item);
                    }
                }
            } else if let Some(item) =
                Self::scan_ordered_page(result.items, target, &mut first, &mut found_current)
            {
                return Self::next_item(base, &item).map(Some);
            }
            let Some(next) = next_cursor else {
                break;
            };
            if play_mode == PlayMode::Shuffle && shuffle.len() >= SHUFFLE_LIMIT {
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

    fn item(video_id: &str) -> DirectoryItem {
        DirectoryItem {
            name: video_id.to_string(),
            item_type: ItemType::Media,
            target: ProviderTarget::tiktok(video_id.to_string()),
            size: None,
            thumbnail: None,
            description: None,
            modified_at: None,
            source_config: None,
        }
    }

    #[test]
    fn sequential_scan_advances_across_cursor_page_boundary() {
        let target = ProviderTarget::tiktok("2".to_string());
        let mut first = None;
        let mut found_current = false;

        let first_page = TikTokProvider::scan_ordered_page(
            vec![item("1"), item("2")],
            &target,
            &mut first,
            &mut found_current,
        );
        assert!(first_page.is_none());
        assert!(found_current);

        let next = TikTokProvider::scan_ordered_page(
            vec![item("3"), item("4")],
            &target,
            &mut first,
            &mut found_current,
        )
        .expect("the first item on the next cursor page should be selected");
        assert_eq!(next.target, ProviderTarget::tiktok("3".to_string()));
    }

    #[test]
    fn repeat_all_scan_preserves_first_item_for_cursor_wrap() {
        let target = ProviderTarget::tiktok("4".to_string());
        let mut first = None;
        let mut found_current = false;

        assert!(TikTokProvider::scan_ordered_page(
            vec![item("1"), item("2")],
            &target,
            &mut first,
            &mut found_current,
        )
        .is_none());
        assert!(TikTokProvider::scan_ordered_page(
            vec![item("3"), item("4")],
            &target,
            &mut first,
            &mut found_current,
        )
        .is_none());
        assert!(found_current);
        assert_eq!(
            first
                .expect("repeat-all should retain the first item")
                .target,
            ProviderTarget::tiktok("1".to_string())
        );
    }
}
