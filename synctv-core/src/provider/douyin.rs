//! Douyin media provider adapter.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use sha2::{Digest, Sha256};

use super::{
    DynamicListQuery, DynamicListResult, DynamicPagination, DynamicPlaylistItem,
    DynamicPlaylistItemThumbnail, DynamicPlaylistProvider, ItemType, MediaProvider, NextPlayItem,
    PlaybackInfo, PlaybackResult, ProviderContext, ProviderCredentialDependency,
    ProviderCredentialPolicy, ProviderError, SourceConfig, SourceCover,
};
use crate::cache::{SingleFlight, SingleFlightError};
use crate::models::{
    DouyinMediaSourceConfig, DouyinPlaybackKind, DouyinPlaybackMetadata, DouyinPlaybackResource,
    DouyinPlaylistSourceConfig, MediaSourceConfig, PlayMode, PlaybackDanmaku,
    PlaybackDanmakuProvider, PlaybackDouyinDanmaku, PlaybackDouyinMedia, PlaybackMedia,
    PlaybackMediaMetadata, PlaybackMediaProvider, PlaybackMetadata, PlaylistSourceConfig,
    ProviderCredential, ProviderTarget, UserId, UserProviderCredential,
};
use crate::repository::UserProviderCredentialRepository;
use synctv_media_providers::douyin::{
    DouyinClient, DouyinDanmakuEvent as ClientDanmakuEvent, DouyinListItem, DouyinMedia,
    DouyinMediaKind, DouyinMetadata, DouyinSession, DouyinStreamFormat, DouyinVariant,
};

const PAGE_SIZE: usize = 20;
const SHUFFLE_LIMIT: usize = 200;
const PLAYBACK_RESOURCE_CACHE_TTL: Duration = Duration::from_secs(15);

pub struct DouyinProvider {
    client: DouyinClient,
    credential_repo: Option<Arc<UserProviderCredentialRepository>>,
    playback_resource_resolver: Arc<DouyinPlaybackResourceResolver>,
}

#[derive(Debug, Clone)]
struct ResolvedDouyinPlaybackTarget {
    url: String,
    original_root_url: String,
    headers: HashMap<String, String>,
    format: DouyinStreamFormat,
}

struct DouyinPlaybackResourceResolver {
    cache: moka::future::Cache<String, ResolvedDouyinPlaybackTarget>,
    singleflight:
        SingleFlight<String, ResolvedDouyinPlaybackTarget, super::ProviderPlaybackFillError>,
}

impl DouyinPlaybackResourceResolver {
    fn new() -> Self {
        Self {
            cache: moka::future::Cache::builder()
                .max_capacity(2_048)
                .time_to_live(PLAYBACK_RESOURCE_CACHE_TTL)
                .build(),
            singleflight: SingleFlight::new(),
        }
    }

    async fn invalidate_all(&self) {
        self.cache.invalidate_all();
        self.cache.run_pending_tasks().await;
    }
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
            playback_resource_resolver: Arc::new(DouyinPlaybackResourceResolver::new()),
        }
    }

    #[must_use]
    pub fn with_http_client(client: reqwest::Client) -> Self {
        Self {
            client: DouyinClient::with_http_client(client),
            credential_repo: None,
            playback_resource_resolver: Arc::new(DouyinPlaybackResourceResolver::new()),
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
            playback_resource_resolver: self.playback_resource_resolver.clone(),
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
        credential_policy: ProviderCredentialPolicy,
    ) -> Result<DouyinSession, ProviderError> {
        let Some(repo) = self.credential_repo_or(ctx.credential_repo) else {
            return Ok(DouyinSession::default());
        };
        let owner_id = ctx.selected_credential_user_id(credential_policy);
        let Some(owner_id) = owner_id else {
            return Ok(DouyinSession::default());
        };
        self.session_for_owner(
            repo,
            owner_id,
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
        self.playback_resource_resolver.invalidate_all().await;
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
        self.playback_resource_resolver.invalidate_all().await;
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
        let target = self
            .resolve_playback_target(store, version, mode_name, media_index, request_context)
            .await?;
        if matches!(
            target.format,
            DouyinStreamFormat::Hls | DouyinStreamFormat::LlHls
        ) {
            return Ok(super::PlaybackTransportAction::M3u8RewriteWithSource {
                url: target.url,
                headers: target.headers,
                source_url: super::playback_transport::dynamic_hls_source_url(
                    &target.original_root_url,
                )?,
            });
        }
        Ok(super::PlaybackTransportAction::FetchAndForward {
            url: target.url,
            headers: target.headers,
            range_header: range_header.map(ToString::to_string),
            proxy_strategy: super::PlaybackResourceProxyStrategy::SliceCache,
        })
    }

    async fn resolve_playback_target(
        &self,
        store: Option<&Arc<dyn super::ProviderStore>>,
        version: &str,
        mode_name: &str,
        media_index: usize,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<ResolvedDouyinPlaybackTarget, ProviderError> {
        let cache_key = format!("{version}\n{mode_name}\n{media_index}");
        if let Some(target) = self.playback_resource_resolver.cache.get(&cache_key).await {
            return Ok(target);
        }
        self.playback_resource_resolver
            .singleflight
            .do_work(cache_key.clone(), async {
                let result: Result<ResolvedDouyinPlaybackTarget, ProviderError> = async {
                    if let Some(target) =
                        self.playback_resource_resolver.cache.get(&cache_key).await
                    {
                        return Ok(target);
                    }
                    let versioned = super::playback_transport::lookup_versioned(
                        store,
                        version,
                        request_context,
                    )
                    .await?;
                    let media = versioned
                        .result
                        .playback_infos
                        .get(mode_name)
                        .and_then(|info| info.medias.get(media_index))
                        .ok_or(ProviderError::NotFound)?;
                    let PlaybackMediaProvider::Douyin(PlaybackDouyinMedia::Refresh {
                        resource,
                        variant_key,
                        root_url,
                        credential_owner_id,
                        provider_instance_name,
                    }) = &media.provider
                    else {
                        return Err(ProviderError::InvalidConfig(
                            "Douyin cached playback resource is invalid".to_string(),
                        ));
                    };
                    let stored_session = match credential_owner_id {
                        Some(owner_id) => {
                            self.stored_session(*owner_id, provider_instance_name.as_deref())
                                .await?
                        }
                        None => DouyinSession::default(),
                    };
                    let session = self.client.effective_session(Some(&stored_session)).await?;
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
                    let target = ResolvedDouyinPlaybackTarget {
                        url: variant.url,
                        original_root_url: root_url.clone(),
                        headers: douyin_headers(
                            variant
                                .headers_required
                                .then_some(session.cookie.as_deref())
                                .flatten(),
                            resolved.metadata.kind,
                        ),
                        format: variant.format,
                    };
                    self.playback_resource_resolver
                        .cache
                        .insert(cache_key, target.clone())
                        .await;
                    Ok(target)
                }
                .await;
                result.map_err(super::ProviderPlaybackFillError::from)
            })
            .await
            .map_err(|error| match error {
                SingleFlightError::Inner(error) => ProviderError::from(error),
                SingleFlightError::WorkerFailed => ProviderError::Internal(
                    "Douyin playback resource resolver worker failed".to_string(),
                ),
            })
    }

    pub async fn get_hls_resource(
        &self,
        store: Option<&Arc<dyn super::ProviderStore>>,
        request: super::HlsResourceRequest<'_>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        let root = self
            .resolve_playback_target(
                store,
                request.version,
                request.mode_name,
                request.media_index,
                request_context,
            )
            .await?;
        if !matches!(
            root.format,
            DouyinStreamFormat::Hls | DouyinStreamFormat::LlHls
        ) {
            return Err(ProviderError::InvalidConfig(
                "Douyin HLS resource requires an HLS playback media".to_string(),
            ));
        }
        super::playback_transport::transport_action_for_dynamic_hls_target(
            &root.original_root_url,
            &root.url,
            root.headers,
            request.target_url,
            request.is_manifest,
            request.range_header,
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
        let session = match credential_owner_id {
            Some(owner_id) => {
                self.stored_session(*owner_id, provider_instance_name.as_deref())
                    .await?
            }
            None => DouyinSession::default(),
        };
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

    const fn media_credential_policy(config: &DouyinMediaSourceConfig) -> ProviderCredentialPolicy {
        ProviderCredentialPolicy::from_shared(match config {
            DouyinMediaSourceConfig::Video { shared, .. }
            | DouyinMediaSourceConfig::Live { shared, .. } => *shared,
        })
    }

    const fn playlist_credential_policy(
        config: &DouyinPlaylistSourceConfig,
    ) -> ProviderCredentialPolicy {
        ProviderCredentialPolicy::from_shared(config.shared)
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

    fn metadata_model(metadata: DouyinMetadata, room_id: Option<String>) -> DouyinPlaybackMetadata {
        let is_live = metadata.kind == DouyinMediaKind::Live;
        DouyinPlaybackMetadata {
            id: metadata.id,
            kind: match metadata.kind {
                DouyinMediaKind::Video => DouyinPlaybackKind::Video,
                DouyinMediaKind::Live => DouyinPlaybackKind::Live,
            },
            author_id: metadata.author.id,
            author_sec_uid: metadata.author.sec_uid,
            author_name: metadata.author.nickname,
            description: metadata.description,
            view_count: metadata.view_count,
            like_count: metadata.like_count,
            comment_count: metadata.comment_count,
            share_count: metadata.share_count,
            collect_count: metadata.collect_count,
            created_at: metadata.created_at,
            music_title: metadata.music_title,
            music_author: metadata.music_author,
            is_live,
            is_currently_live: is_live.then_some(metadata.is_live),
            room_id,
        }
    }

    fn playback_result(
        media: DouyinMedia,
        resource: &DouyinPlaybackResource,
        credential_owner_id: Option<UserId>,
        provider_instance_name: Option<&str>,
    ) -> Result<PlaybackResult, ProviderError> {
        let mut infos = HashMap::new();
        let variants = semantically_distinct_variants(&media.variants);
        let preferred_video_key = (media.metadata.kind == DouyinMediaKind::Video)
            .then(|| {
                variants
                    .iter()
                    .max_by_key(|variant| video_variant_score(variant))
                    .map(|variant| variant_key_for(variant))
            })
            .flatten();
        for variant in variants {
            let format = stream_format(variant.format);
            let mode = match format {
                "m3u8" => "hls",
                other => other,
            };
            let info = infos
                .entry(mode.to_string())
                .or_insert_with(|| PlaybackInfo {
                    thumbnail: media.metadata.cover.as_ref().map(|cover| cover.url.clone()),
                    medias: Vec::new(),
                    default_media_index: None,
                    subtitles: Vec::new(),
                    default_subtitle_index: None,
                    danmakus: (media.metadata.kind == DouyinMediaKind::Live)
                        .then(|| PlaybackDanmaku {
                            name: "Douyin Live Danmaku".to_string(),
                            format: Some("synctv-douyin-live".to_string()),
                            p2p_swarm_id: None,
                            provider: PlaybackDanmakuProvider::Douyin(
                                PlaybackDouyinDanmaku::Refresh { media_index: 0 },
                            ),
                        })
                        .into_iter()
                        .collect(),
                    default_danmaku_index: (media.metadata.kind == DouyinMediaKind::Live)
                        .then_some(0),
                });
            let media_index = info.medias.len();
            let variant_key = variant_key_for(variant);
            let is_preferred = preferred_video_key.as_deref() == Some(variant_key.as_str())
                || variant.quality.to_ascii_lowercase().contains("origin");
            let p2p_swarm_id = match resource {
                DouyinPlaybackResource::Video { aweme_id } => Some(super::provider_p2p_swarm_id(
                    Self::NAME,
                    provider_instance_name,
                    "media",
                    &format!("video:{aweme_id}:variant:{variant_key}"),
                )),
                DouyinPlaybackResource::Live { .. } => None,
            };
            info.medias.push(PlaybackMedia {
                name: variant.quality.clone(),
                format: format.to_string(),
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
                p2p_swarm_id,
                provider: PlaybackMediaProvider::Douyin(PlaybackDouyinMedia::Refresh {
                    resource: resource.clone(),
                    variant_key,
                    root_url: variant.url.clone(),
                    credential_owner_id,
                    provider_instance_name: provider_instance_name.map(str::to_owned),
                }),
            });
            if is_preferred || info.default_media_index.is_none() {
                info.default_media_index = Some(media_index);
            }
        }
        if infos.is_empty() {
            return Err(ProviderError::ApiError(
                if media.metadata.is_live {
                    "Douyin live room is offline"
                } else {
                    "Douyin returned no playable variants"
                }
                .to_string(),
            ));
        }
        let default_mode = infos
            .contains_key("hls")
            .then(|| "hls".to_string())
            .or_else(|| infos.keys().min().cloned())
            .ok_or_else(|| ProviderError::ApiError("Douyin playback is empty".to_string()))?;
        let duration_seconds = media
            .metadata
            .duration_ms
            .map(|duration| std::time::Duration::from_millis(duration).as_secs_f64());
        let playback_kind = if media.metadata.kind == DouyinMediaKind::Live {
            crate::models::PlaybackKind::Live
        } else {
            crate::models::PlaybackKind::Regular
        };
        let metadata = Self::metadata_model(media.metadata, media.room_id);
        Ok(PlaybackResult {
            playback_infos: infos,
            default_mode,
            provider: crate::models::SourceProvider::Douyin,
            provider_instance_name: None,
            duration_seconds,
            playback_kind: Some(playback_kind),
            metadata: Some(PlaybackMetadata::Douyin(metadata)),
        })
    }

    fn directory_item(item: DouyinListItem) -> DynamicPlaylistItem {
        let metadata = PlaybackMetadata::Douyin(DouyinPlaybackMetadata {
            id: item.aweme_id.clone(),
            kind: DouyinPlaybackKind::Video,
            author_id: item.author.id.clone(),
            author_sec_uid: item.author.sec_uid.clone(),
            author_name: item.author.nickname.clone(),
            description: item.title.clone(),
            view_count: None,
            like_count: None,
            comment_count: None,
            share_count: None,
            collect_count: None,
            created_at: item.created_at,
            music_title: None,
            music_author: None,
            is_live: false,
            is_currently_live: None,
            room_id: None,
        });
        DynamicPlaylistItem {
            name: item.title,
            item_type: ItemType::Media,
            target: ProviderTarget::douyin(item.aweme_id),
            size: None,
            thumbnail: item
                .cover
                .map(|cover| DynamicPlaylistItemThumbnail::Url(cover.url)),
            description: Some(item.author.nickname).filter(|value| !value.is_empty()),
            modified_at: item.created_at,
            source_config: None,
            metadata: Some(metadata),
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
        item: &DynamicPlaylistItem,
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

fn mark_playback_resources(
    result: &mut PlaybackResult,
    version: &str,
    expires_at: i64,
    client_profile: Option<&super::PlaybackClientProfile>,
) {
    let original_default = result.default_mode.clone();
    let original_modes = std::mem::take(&mut result.playback_infos);
    for (mode_name, mut info) in original_modes {
        let source_medias = std::mem::take(&mut info.medias);
        let supported_indices = source_medias
            .iter()
            .enumerate()
            .filter_map(|(media_index, media)| {
                super::proxy_playback_media_supported_by_client(client_profile, &mode_name, media)
                    .then_some(media_index)
            })
            .collect::<std::collections::HashSet<_>>();
        let (medias, default_media_index) = super::map_playback_resources(
            &source_medias,
            info.default_media_index,
            |media_index, media| {
                if !supported_indices.contains(&media_index) {
                    return None;
                }
                let mut media = media.clone();
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
                Some(media)
            },
        );
        if medias.is_empty() {
            continue;
        }
        info.medias = medias;
        info.default_media_index = default_media_index;
        let (danmakus, default_danmaku_index) = super::map_playback_resources(
            &info.danmakus,
            info.default_danmaku_index,
            |_, danmaku| {
                (!matches!(
                    &danmaku.provider,
                    PlaybackDanmakuProvider::Douyin(PlaybackDouyinDanmaku::Refresh { media_index })
                        if !supported_indices.contains(media_index)
                ))
                .then(|| danmaku.clone())
            },
        );
        info.danmakus = danmakus;
        info.default_danmaku_index = default_danmaku_index;
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
        result.playback_infos.insert(mode_name, info);
    }
    super::select_generated_playback_default(result, &original_default, true);
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
        "{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
        stream_format(variant.format),
        variant.quality,
        variant.codec.as_deref().unwrap_or_default(),
        variant.width.unwrap_or_default(),
        variant.height.unwrap_or_default(),
        variant.fps.unwrap_or_default(),
        variant.bitrate.unwrap_or_default(),
        variant.audio_only
    )
}

fn semantically_distinct_variants(variants: &[DouyinVariant]) -> Vec<&DouyinVariant> {
    let mut seen = HashSet::new();
    variants
        .iter()
        .filter(|variant| seen.insert(variant_key_for(variant)))
        .collect()
}

fn video_variant_score(variant: &DouyinVariant) -> (bool, bool, bool, u64, u64, u32) {
    (
        !variant.audio_only,
        variant.codec.as_deref() == Some("avc"),
        variant.format == DouyinStreamFormat::Mp4,
        variant
            .width
            .zip(variant.height)
            .map_or(0, |(width, height)| u64::from(width) * u64::from(height)),
        variant.bitrate.unwrap_or_default(),
        variant.fps.unwrap_or_default(),
    )
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
        let credential_policy = Self::media_credential_policy(config);
        let credential_owner_id = ctx.selected_credential_user_id(credential_policy);
        if credential_policy.uses_resource_owner() && credential_owner_id.is_none() {
            return Err(ProviderError::Internal(
                "Douyin credential owner is unavailable".to_string(),
            ));
        }
        let session = self.session(ctx, credential_policy).await?;
        let provider_instance_name =
            super::bound_provider_instance_name(ctx).map(ToString::to_string);
        let resource = Self::resource(config);
        let cache_key = format!(
            "playback:{}:{}:{}",
            serde_json::to_string(&resource)
                .map_err(|error| ProviderError::Internal(error.to_string()))?,
            credential_owner_id.map_or_else(|| "anonymous".to_string(), |id| id.to_string()),
            Self::credential_server_id_for_instance(provider_instance_name.as_deref())
        );
        let client_profile = ctx.playback_client_profile();
        let result = super::cached_versioned_playback_or_fill(
            Self::NAME,
            &cache_key,
            Duration::from_mins(30),
            ctx,
            |result, version, expires_at| {
                mark_playback_resources(result, version, expires_at, client_profile);
            },
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
        .await?;
        super::require_compatible_playback_route(
            result,
            crate::models::PlaybackProxyMode::Only,
            client_profile,
        )
    }

    async fn media_metadata(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &MediaSourceConfig,
    ) -> Result<Option<PlaybackMetadata>, ProviderError> {
        ctx.check_active()
            .map_err(|error| ProviderError::NetworkError(error.to_string()))?;
        let config = Self::media_config(source_config)?;
        let cache_key = serde_json::to_string(source_config)
            .map_err(|error| ProviderError::Internal(error.to_string()))?;
        let credential_policy = Self::media_credential_policy(config);
        super::cached_provider_metadata_or_fill(
            Self::NAME,
            &cache_key,
            Duration::from_secs(15),
            ctx,
            || async {
                let session = self.session(ctx, credential_policy).await?;
                let media = self.resolve_media(config, &session).await?;
                Ok(Some(PlaybackMetadata::Douyin(Self::metadata_model(
                    media.metadata,
                    media.room_id,
                ))))
            },
        )
        .await
    }

    fn as_dynamic_playlist_provider(&self) -> Option<&dyn DynamicPlaylistProvider> {
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
                let session = self
                    .session(ctx, Self::media_credential_policy(config))
                    .await?;
                self.resolve_media(config, &session).await?;
            }
            SourceConfig::DynamicPlaylist(source) => {
                let config = Self::playlist_config(source)?;
                let session = self
                    .session(ctx, Self::playlist_credential_policy(config))
                    .await?;
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
            crate::models::SourceProvider::Douyin,
            owner_id,
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
                let session = self
                    .session(ctx, Self::media_credential_policy(config))
                    .await?;
                self.resolve_media(config, &session).await?.metadata.cover
            }
            SourceConfig::DynamicPlaylist(source) => {
                let config = Self::playlist_config(source)?;
                let session = self
                    .session(ctx, Self::playlist_credential_policy(config))
                    .await?;
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
impl DynamicPlaylistProvider for DouyinProvider {
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
        let session = self
            .session(ctx, Self::playlist_credential_policy(config))
            .await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn media(kind: DouyinMediaKind, id: &str, variant: DouyinVariant) -> DouyinMedia {
        DouyinMedia {
            resource: match kind {
                DouyinMediaKind::Video => synctv_media_providers::douyin::DouyinResource::Video {
                    aweme_id: id.to_string(),
                },
                DouyinMediaKind::Live => synctv_media_providers::douyin::DouyinResource::Live {
                    web_rid: id.to_string(),
                },
            },
            metadata: synctv_media_providers::douyin::DouyinMetadata {
                id: id.to_string(),
                kind,
                title: "Playback".to_string(),
                description: String::new(),
                author: synctv_media_providers::douyin::DouyinAuthor {
                    id: "author".to_string(),
                    sec_uid: "sec-author".to_string(),
                    unique_id: Some("author".to_string()),
                    nickname: "Author".to_string(),
                    avatar: None,
                },
                cover: None,
                dynamic_cover: None,
                duration_ms: (kind == DouyinMediaKind::Video).then_some(60_000),
                created_at: None,
                is_live: kind == DouyinMediaKind::Live,
                view_count: None,
                like_count: None,
                comment_count: None,
                share_count: None,
                collect_count: None,
                music_title: None,
                music_author: None,
            },
            room_id: (kind == DouyinMediaKind::Live).then(|| "room".to_string()),
            variants: vec![variant],
        }
    }

    fn variant(
        url: &str,
        quality: &str,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u64,
    ) -> DouyinVariant {
        DouyinVariant {
            url: url.to_string(),
            format: DouyinStreamFormat::Mp4,
            quality: quality.to_string(),
            codec: Some("avc".to_string()),
            width: Some(width),
            height: Some(height),
            fps: Some(fps),
            bitrate: Some(bitrate),
            audio_only: false,
            headers_required: true,
        }
    }

    #[test]
    fn semantic_video_variants_collapse_mirror_urls() {
        let variants = vec![
            variant(
                "https://mirror-a.test/video.mp4",
                "540p",
                960,
                540,
                30,
                900_000,
            ),
            variant(
                "https://mirror-b.test/video.mp4",
                "540p",
                960,
                540,
                30,
                900_000,
            ),
            variant(
                "https://mirror-a.test/video-720.mp4",
                "720p",
                1280,
                720,
                60,
                1_800_000,
            ),
        ];

        let distinct = semantically_distinct_variants(&variants);

        assert_eq!(distinct.len(), 2);
        assert_eq!(distinct[0].url, "https://mirror-a.test/video.mp4");
        assert_eq!(distinct[1].quality, "720p");
        assert_eq!(variant_key_for(&variants[0]), variant_key_for(&variants[1]));
    }

    #[test]
    fn video_variant_score_prefers_higher_quality_for_equivalent_sources() {
        let low = variant("https://cdn.test/low.mp4", "540p", 960, 540, 30, 900_000);
        let high = variant(
            "https://cdn.test/high.mp4",
            "1080p",
            1920,
            1080,
            60,
            4_000_000,
        );

        assert!(video_variant_score(&high) > video_variant_score(&low));
    }

    #[test]
    fn playback_assigns_swarm_only_to_video_resources() {
        let video_resource = DouyinPlaybackResource::Video {
            aweme_id: "video-1".to_string(),
        };
        let video = DouyinProvider::playback_result(
            media(
                DouyinMediaKind::Video,
                "video-1",
                variant(
                    "https://cdn.test/video.mp4",
                    "1080p",
                    1920,
                    1080,
                    60,
                    4_000_000,
                ),
            ),
            &video_resource,
            Some(UserId::expect_positive(1)),
            Some("primary"),
        )
        .expect("video playback should map");
        assert!(video
            .playback_infos
            .values()
            .flat_map(|info| &info.medias)
            .all(|media| media.p2p_swarm_id.is_some()));

        let mut live_variant = variant(
            "https://cdn.test/live.m3u8",
            "origin",
            1920,
            1080,
            60,
            4_000_000,
        );
        live_variant.format = DouyinStreamFormat::Hls;
        let live_resource = DouyinPlaybackResource::Live {
            web_rid: "live-1".to_string(),
        };
        let live = DouyinProvider::playback_result(
            media(DouyinMediaKind::Live, "live-1", live_variant),
            &live_resource,
            Some(UserId::expect_positive(1)),
            Some("primary"),
        )
        .expect("live playback should map");
        let info = &live.playback_infos[&live.default_mode];
        assert!(info.medias.iter().all(|media| media.p2p_swarm_id.is_none()));
        assert!(info
            .danmakus
            .iter()
            .all(|danmaku| danmaku.p2p_swarm_id.is_none()));
    }
}
