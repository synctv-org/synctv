//! Douyin media provider adapter.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use sha2::{Digest, Sha256};

use super::{
    DirectoryItem, DirectoryItemThumbnail, DynamicFolder, DynamicListQuery, DynamicListResult,
    DynamicPagination, ItemType, MediaProvider, NextPlayItem, PlaybackInfo, PlaybackResult,
    ProviderContext, ProviderCredentialDependency, ProviderError, SourceConfig, SourceCover,
};
use crate::models::{
    DouyinMediaSourceConfig, DouyinPlaybackMetadata, DouyinPlaybackResource,
    DouyinPlaylistSourceConfig, MediaSourceConfig, PlayMode, PlaybackDanmaku,
    PlaybackDanmakuProvider, PlaybackDouyinDanmaku, PlaybackDouyinMedia, PlaybackMedia,
    PlaybackMediaMetadata, PlaybackMediaProvider, PlaybackMetadata, PlaylistSourceConfig,
    ProviderCredential, ProviderTarget, UserId, UserProviderCredential,
};
use crate::repository::UserProviderCredentialRepository;
use synctv_media_providers::douyin::{
    DouyinClient, DouyinDanmakuEvent as ClientDanmakuEvent, DouyinListItem, DouyinMedia,
    DouyinMediaKind, DouyinSession, DouyinStreamFormat, DouyinVariant,
};

const PAGE_SIZE: usize = 20;
const SHUFFLE_LIMIT: usize = 200;

pub struct DouyinProvider {
    client: DouyinClient,
    credential_repo: Option<Arc<UserProviderCredentialRepository>>,
}

#[derive(Debug, Clone)]
pub struct DouyinBind {
    pub id: i64,
    pub server_id: String,
    pub label: String,
    pub created_at: i64,
    pub provider_instance_name: Option<String>,
}

pub type DouyinDanmakuStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<DouyinDanmakuEvent, ProviderError>> + Send + 'static>,
>;

#[derive(Debug, Clone)]
pub enum DouyinDanmakuEvent {
    Chat {
        id: String,
        user_id: String,
        user_name: String,
        text: String,
        color: Option<String>,
        sent_at_ms: Option<u64>,
    },
    StreamClosed {
        action: u64,
        message: Option<String>,
    },
}

impl Default for DouyinProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl DouyinProvider {
    pub const NAME: &'static str = "douyin";

    #[must_use]
    pub fn new() -> Self {
        Self {
            client: DouyinClient::new().expect("Douyin HTTP client should build"),
            credential_repo: None,
        }
    }

    #[must_use]
    pub fn with_http_client(client: reqwest::Client) -> Self {
        Self {
            client: DouyinClient::with_http_client(client),
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
            format!("douyin\n{instance_name}").as_bytes(),
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
    ) -> Result<DouyinSession, ProviderError> {
        let Some(repo) = self.credential_repo_or(ctx.credential_repo) else {
            return Ok(DouyinSession::default());
        };
        let owner_id = if shared {
            ctx.credential_owner_id()
        } else {
            ctx.user_id()
        };
        let Some(owner_id) = owner_id else {
            return Ok(DouyinSession::default());
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
    ) -> Result<DouyinSession, ProviderError> {
        let Some(credential) = repo
            .get_by_provider_and_server(owner_id, Self::NAME, server_id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
        else {
            return Ok(DouyinSession::default());
        };
        match credential.credential_data {
            ProviderCredential::Douyin { cookie, .. } => Ok(DouyinSession {
                cookie: Some(cookie),
            }),
            _ => Err(ProviderError::InvalidCredentialType),
        }
    }

    async fn stored_session(
        &self,
        owner_id: UserId,
        provider_instance_name: Option<&str>,
    ) -> Result<DouyinSession, ProviderError> {
        let Some(repo) = self.credential_repo.as_deref() else {
            return Ok(DouyinSession::default());
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
                "Douyin credential label is required".to_string(),
            ));
        }
        if cookie.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Douyin cookie is required".to_string(),
            ));
        }
        let provider_instance_name =
            crate::models::normalize_provider_instance_name_owned(provider_instance_name);
        let server_id = Self::credential_server_id_for_instance(provider_instance_name.as_deref());
        let now = Utc::now();
        self.credential_repo
            .as_deref()
            .ok_or_else(|| {
                ProviderError::Internal("Douyin credential repository is unavailable".to_string())
            })?
            .upsert_by_user_provider_server(&UserProviderCredential {
                id: 0,
                user_id,
                provider: Self::NAME.to_string(),
                server_id: server_id.clone(),
                provider_instance_name,
                credential_data: ProviderCredential::Douyin {
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
    ) -> Result<Vec<DouyinBind>, ProviderError> {
        let requested = crate::models::normalize_provider_instance_name(provider_instance_name);
        self.credential_repo
            .as_deref()
            .ok_or_else(|| {
                ProviderError::Internal("Douyin credential repository is unavailable".to_string())
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
                let ProviderCredential::Douyin { label, .. } = credential.credential_data else {
                    return Err(ProviderError::InvalidCredentialType);
                };
                Ok(DouyinBind {
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
            ProviderError::Internal("Douyin credential repository is unavailable".to_string())
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
    ) -> Result<DouyinMedia, ProviderError> {
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
    ) -> Result<synctv_media_providers::douyin::DouyinListPage, ProviderError> {
        let session = self.stored_session(user_id, provider_instance_name).await?;
        Ok(self
            .client
            .user_posts(sec_uid, cursor, page_size, Some(&session))
            .await?)
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
        let PlaybackMediaProvider::Douyin(PlaybackDouyinMedia::Refresh {
            resource,
            variant_key,
            credential_owner_id,
            provider_instance_name,
        }) = &media.provider
        else {
            return Err(ProviderError::InvalidConfig(
                "Douyin cached playback resource is invalid".to_string(),
            ));
        };
        let session = self
            .stored_session(*credential_owner_id, provider_instance_name.as_deref())
            .await?;
        let resolved = match resource {
            DouyinPlaybackResource::Video { aweme_id } => {
                self.client.video(aweme_id, Some(&session)).await?
            }
            DouyinPlaybackResource::Live { web_rid } => {
                self.client.live(web_rid, Some(&session)).await?
            }
        };
        let variant = resolved
            .variants
            .into_iter()
            .find(|variant| variant_key_for(variant) == *variant_key)
            .ok_or(ProviderError::NotFound)?;
        super::playback_transport::transport_action_for_target_url(
            variant.url,
            douyin_headers(session.cookie.as_deref(), resolved.metadata.kind),
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
            douyin_headers(None, DouyinMediaKind::Live),
            range_header,
        )
    }

    pub async fn watch_danmaku(
        &self,
        store: Option<&Arc<dyn super::ProviderStore>>,
        version: &str,
        mode_name: &str,
        media_index: usize,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<DouyinDanmakuStream, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let media = versioned
            .result
            .playback_infos
            .get(mode_name)
            .and_then(|info| info.medias.get(media_index))
            .ok_or(ProviderError::NotFound)?;
        let PlaybackMediaProvider::Douyin(PlaybackDouyinMedia::Refresh {
            resource: DouyinPlaybackResource::Live { .. },
            credential_owner_id,
            provider_instance_name,
            ..
        }) = &media.provider
        else {
            return Err(ProviderError::InvalidConfig(
                "Douyin danmaku requires a live resource".to_string(),
            ));
        };
        let room_id = versioned
            .result
            .metadata
            .as_ref()
            .and_then(|metadata| match metadata {
                PlaybackMetadata::Douyin(metadata) => metadata.room_id.as_deref(),
                _ => None,
            })
            .ok_or(ProviderError::NotFound)?;
        let session = self
            .stored_session(*credential_owner_id, provider_instance_name.as_deref())
            .await?;
        let stream = synctv_media_providers::douyin::watch_danmaku(room_id, Some(&session)).await?;
        Ok(Box::pin(stream.map(|event| {
            event
                .map(|event| match event {
                    ClientDanmakuEvent::Chat {
                        id,
                        user_id,
                        user_name,
                        text,
                        color,
                        sent_at_ms,
                    } => DouyinDanmakuEvent::Chat {
                        id,
                        user_id,
                        user_name,
                        text,
                        color,
                        sent_at_ms,
                    },
                    ClientDanmakuEvent::StreamClosed { action, message } => {
                        DouyinDanmakuEvent::StreamClosed { action, message }
                    }
                })
                .map_err(ProviderError::from)
        })))
    }

    fn media_config(config: &MediaSourceConfig) -> Result<&DouyinMediaSourceConfig, ProviderError> {
        match config {
            MediaSourceConfig::Douyin(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "Douyin provider requires Douyin media source_config".to_string(),
            )),
        }
    }

    fn playlist_config(
        config: &PlaylistSourceConfig,
    ) -> Result<&DouyinPlaylistSourceConfig, ProviderError> {
        match config {
            PlaylistSourceConfig::Douyin(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "Douyin provider requires Douyin playlist source_config".to_string(),
            )),
        }
    }

    const fn media_shared(config: &DouyinMediaSourceConfig) -> bool {
        match config {
            DouyinMediaSourceConfig::Video { shared, .. }
            | DouyinMediaSourceConfig::Live { shared, .. } => *shared,
        }
    }

    fn resource(config: &DouyinMediaSourceConfig) -> DouyinPlaybackResource {
        match config {
            DouyinMediaSourceConfig::Video { aweme_id, .. } => DouyinPlaybackResource::Video {
                aweme_id: aweme_id.clone(),
            },
            DouyinMediaSourceConfig::Live { web_rid, .. } => DouyinPlaybackResource::Live {
                web_rid: web_rid.clone(),
            },
        }
    }

    async fn resolve_media(
        &self,
        config: &DouyinMediaSourceConfig,
        session: &DouyinSession,
    ) -> Result<DouyinMedia, ProviderError> {
        Ok(match config {
            DouyinMediaSourceConfig::Video { aweme_id, .. } => {
                self.client.video(aweme_id, Some(session)).await?
            }
            DouyinMediaSourceConfig::Live { web_rid, .. } => {
                self.client.live(web_rid, Some(session)).await?
            }
        })
    }

    fn playback_result(
        media: DouyinMedia,
        resource: &DouyinPlaybackResource,
        credential_owner_id: UserId,
        provider_instance_name: Option<&str>,
    ) -> Result<PlaybackResult, ProviderError> {
        let mut infos = HashMap::new();
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
                            fps: variant.fps.and_then(|value| i32::try_from(value).ok()),
                        }),
                        provider: PlaybackMediaProvider::Douyin(PlaybackDouyinMedia::Refresh {
                            resource: resource.clone(),
                            variant_key: variant_key_for(variant),
                            credential_owner_id,
                            provider_instance_name: provider_instance_name.map(str::to_owned),
                        }),
                    }],
                    default_media_index: Some(0),
                    subtitles: Vec::new(),
                    default_subtitle_index: None,
                    danmakus: (media.metadata.kind == DouyinMediaKind::Live)
                        .then(|| PlaybackDanmaku {
                            name: "Douyin Live Danmaku".to_string(),
                            format: Some("synctv-douyin-live".to_string()),
                            provider: PlaybackDanmakuProvider::Douyin(
                                PlaybackDouyinDanmaku::Refresh { media_index: 0 },
                            ),
                        })
                        .into_iter()
                        .collect(),
                    default_danmaku_index: (media.metadata.kind == DouyinMediaKind::Live)
                        .then_some(0),
                },
            );
        }
        if infos.is_empty() {
            return Err(ProviderError::ApiError(
                if media.metadata.is_live {
                    "Douyin returned no playable variants"
                } else {
                    "Douyin live room is offline"
                }
                .to_string(),
            ));
        }
        let default_mode = infos
            .keys()
            .find(|mode| mode.contains("origin") && mode.contains("hls"))
            .cloned()
            .or_else(|| infos.keys().next().cloned())
            .ok_or_else(|| ProviderError::ApiError("Douyin playback is empty".to_string()))?;
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
            metadata: Some(PlaybackMetadata::Douyin(DouyinPlaybackMetadata {
                id: media.metadata.id,
                kind: match media.metadata.kind {
                    DouyinMediaKind::Video => "video",
                    DouyinMediaKind::Live => "live",
                }
                .to_string(),
                author_id: media.metadata.author.id,
                author_sec_uid: media.metadata.author.sec_uid,
                author_name: media.metadata.author.nickname,
                description: media.metadata.description,
                view_count: media.metadata.view_count,
                like_count: media.metadata.like_count,
                comment_count: media.metadata.comment_count,
                share_count: media.metadata.share_count,
                collect_count: media.metadata.collect_count,
                created_at: media.metadata.created_at,
                music_title: media.metadata.music_title,
                music_author: media.metadata.music_author,
                is_live: media.metadata.is_live,
                room_id: media.room_id,
            })),
        })
    }

    fn directory_item(item: DouyinListItem) -> DirectoryItem {
        DirectoryItem {
            name: item.title,
            item_type: ItemType::Media,
            target: ProviderTarget::douyin(item.aweme_id),
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
        let ProviderTarget::Douyin(target) = target else {
            return Err(ProviderError::InvalidConfig(
                "Douyin playlist requires a Douyin target".to_string(),
            ));
        };
        Ok(&target.aweme_id)
    }

    fn next_item(
        config: &DouyinPlaylistSourceConfig,
        item: &DirectoryItem,
    ) -> Result<NextPlayItem, ProviderError> {
        Ok(NextPlayItem {
            name: item.name.clone(),
            item_type: ItemType::Media,
            source_config: MediaSourceConfig::Douyin(DouyinMediaSourceConfig::Video {
                aweme_id: Self::decode_target(&item.target)?.to_string(),
                shared: config.shared,
            }),
            target: item.target.clone(),
        })
    }
}

fn mark_playback_resources(result: &mut PlaybackResult, version: &str, expires_at: i64) {
    for (mode_name, info) in &mut result.playback_infos {
        for (media_index, media) in info.medias.iter_mut().enumerate() {
            if matches!(
                media.provider,
                PlaybackMediaProvider::Douyin(PlaybackDouyinMedia::Refresh { .. })
            ) {
                media.provider = PlaybackMediaProvider::Douyin(PlaybackDouyinMedia::Proxy {
                    version: version.to_string(),
                    expires_at,
                    mode_name: mode_name.clone(),
                    media_index,
                });
            }
        }
        for danmaku in &mut info.danmakus {
            let PlaybackDanmakuProvider::Douyin(PlaybackDouyinDanmaku::Refresh { media_index }) =
                &danmaku.provider
            else {
                continue;
            };
            danmaku.provider = PlaybackDanmakuProvider::Douyin(PlaybackDouyinDanmaku::Proxy {
                version: version.to_string(),
                expires_at,
                mode_name: mode_name.clone(),
                media_index: *media_index,
            });
        }
    }
}

fn stream_format(format: DouyinStreamFormat) -> &'static str {
    match format {
        DouyinStreamFormat::Mp4 => "mp4",
        DouyinStreamFormat::Flv => "flv",
        DouyinStreamFormat::Hls | DouyinStreamFormat::LlHls => "m3u8",
        DouyinStreamFormat::Dash => "mpd",
        DouyinStreamFormat::Cmaf => "cmaf",
        DouyinStreamFormat::HttpTs => "ts",
    }
}

fn variant_key_for(variant: &DouyinVariant) -> String {
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
    variant: &DouyinVariant,
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

fn douyin_headers(cookie: Option<&str>, kind: DouyinMediaKind) -> HashMap<String, String> {
    let origin = match kind {
        DouyinMediaKind::Video => "https://www.douyin.com",
        DouyinMediaKind::Live => "https://live.douyin.com",
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
impl MediaProvider for DouyinProvider {
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
                ProviderError::Internal("Douyin credential owner is unavailable".to_string())
            })?
        } else {
            *ctx.user_id().ok_or_else(|| {
                ProviderError::Internal("Douyin viewer is unavailable".to_string())
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
            ProviderError::Internal("Douyin credential owner is unavailable".to_string())
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
impl DynamicFolder for DouyinProvider {
    async fn list_playlist(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: Option<&ProviderTarget>,
        query: DynamicListQuery,
    ) -> Result<DynamicListResult, ProviderError> {
        if target.is_some() {
            return Err(ProviderError::InvalidConfig(
                "Douyin user playlists have a single browse level".to_string(),
            ));
        }
        if query
            .search
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            return Err(ProviderError::InvalidConfig(
                "Douyin user playlists do not expose server-side search".to_string(),
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
                    "Douyin requires cursor pagination after the first page".to_string(),
                ));
            }
        };
        let count = u32::try_from(query.page_size.clamp(1, 50)).map_err(|_| {
            ProviderError::InvalidConfig("Douyin page size exceeds u32::MAX".to_string())
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
            for item in result.items {
                first.get_or_insert_with(|| item.clone());
                if play_mode == PlayMode::Shuffle {
                    if item.target != *target && shuffle.len() < SHUFFLE_LIMIT {
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
