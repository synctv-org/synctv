//! AcFun media provider adapter.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use serde::Serialize;

use super::{
    MediaProvider, PlaybackInfo, PlaybackResult, ProviderContext, ProviderError, SourceConfig,
    SourceCover,
};
use crate::models::{
    AcFunMediaSourceConfig, AcFunPlaybackFormat, AcFunPlaybackMetadata, AcFunPlaybackResourceKind,
    MediaSourceConfig, PlaybackAcFunDanmaku, PlaybackAcFunMedia, PlaybackDanmaku,
    PlaybackDanmakuProvider, PlaybackMedia, PlaybackMediaMetadata, PlaybackMediaProvider,
    PlaybackMetadata,
};
use synctv_media_providers::acfun::{
    AcFunClient, AcFunDanmaku, AcFunLiveDanmakuEvent as UpstreamLiveDanmaku, AcFunMedia,
    AcFunMetadata, AcFunResource, AcFunResourceKind, AcFunStreamFormat,
};

pub struct AcFunProvider {
    client: AcFunClient,
}

pub type AcFunDanmakuStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<AcFunLiveDanmakuEvent, ProviderError>> + Send + 'static>,
>;

#[derive(Debug, Clone)]
pub struct AcFunLiveDanmakuEvent {
    pub id: String,
    pub user_id: String,
    pub user_name: String,
    pub avatar_url: Option<String>,
    pub text: String,
    pub color: Option<String>,
    pub badge_name: Option<String>,
    pub badge_level: Option<u32>,
    pub sent_at_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcFunDanmakuDocument {
    version: u32,
    comments: Vec<AcFunDanmakuComment>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcFunDanmakuComment {
    id: String,
    user_id: String,
    text: String,
    color: String,
    position_ms: u64,
    created_at_ms: Option<u64>,
    mode: u32,
    size: u32,
}

impl Default for AcFunProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AcFunProvider {
    pub const NAME: &'static str = "acfun";

    #[must_use]
    pub fn new() -> Self {
        Self {
            client: AcFunClient::new().expect("AcFun HTTP client should build"),
        }
    }

    #[must_use]
    pub fn with_http_client(client: reqwest::Client) -> Self {
        Self {
            client: AcFunClient::with_http_client(client),
        }
    }

    pub async fn resolve_resource(&self, resource: &str) -> Result<AcFunMedia, ProviderError> {
        let resource = AcFunClient::parse_resource(resource)?;
        Ok(self.client.resolve(&resource, None).await?)
    }

    fn config(source_config: &MediaSourceConfig) -> Result<&AcFunMediaSourceConfig, ProviderError> {
        match source_config {
            MediaSourceConfig::AcFun(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "AcFun provider requires AcFun media source_config".to_string(),
            )),
        }
    }

    fn source_config(
        source_config: SourceConfig<'_>,
    ) -> Result<&AcFunMediaSourceConfig, ProviderError> {
        match source_config {
            SourceConfig::Media(config) => Self::config(config),
            SourceConfig::DynamicPlaylist(_) => Err(ProviderError::InvalidConfig(
                "AcFun dynamic playlists are unavailable".to_string(),
            )),
        }
    }

    fn resource(config: &AcFunMediaSourceConfig) -> Result<AcFunResource, ProviderError> {
        let (kind, id, query) = match config {
            AcFunMediaSourceConfig::Video { video_id } => {
                (AcFunResourceKind::Video, video_id, None)
            }
            AcFunMediaSourceConfig::Bangumi {
                bangumi_id,
                episode_query,
            } => (
                AcFunResourceKind::Bangumi,
                bangumi_id,
                episode_query.clone(),
            ),
            AcFunMediaSourceConfig::Live { author_id } => {
                (AcFunResourceKind::Live, author_id, None)
            }
        };
        let parsed = AcFunClient::parse_resource(id)?;
        Ok(AcFunResource {
            kind,
            id: parsed.id,
            query,
        })
    }

    fn cache_key(resource: &AcFunResource) -> String {
        format!(
            "playback:{}:{}:{}",
            match resource.kind {
                AcFunResourceKind::Video => "video",
                AcFunResourceKind::Bangumi => "bangumi",
                AcFunResourceKind::Live => "live",
            },
            resource.id,
            resource.query.as_deref().unwrap_or_default()
        )
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
        let PlaybackMediaProvider::AcFun(PlaybackAcFunMedia::Refresh {
            resource_kind,
            resource_id,
            query,
            quality_name,
            quality_type,
            format,
            bitrate,
        }) = &media.provider
        else {
            return Err(ProviderError::InvalidConfig(
                "AcFun cached playback resource is invalid".to_string(),
            ));
        };
        let resource = resource_from_playback(*resource_kind, resource_id.clone(), query.clone());
        let playback = self.client.playback(&resource, None).await?;
        let quality = playback
            .qualities
            .into_iter()
            .find(|quality| {
                quality.name == *quality_name
                    && quality.quality_type == *quality_type
                    && playback_format(quality.format) == *format
                    && quality.bitrate == *bitrate
            })
            .ok_or(ProviderError::NotFound)?;
        let headers = acfun_headers(resource.kind);
        if *format == AcFunPlaybackFormat::Hls {
            Ok(super::PlaybackTransportAction::M3u8Rewrite {
                url: quality.url,
                headers,
            })
        } else {
            Ok(super::PlaybackTransportAction::FetchAndForward {
                url: quality.url,
                headers,
                range_header: range_header.map(ToString::to_string),
                proxy_strategy: super::PlaybackResourceProxyStrategy::SliceCache,
            })
        }
    }

    pub async fn get_hls_resource(
        &self,
        store: Option<&Arc<dyn super::ProviderStore>>,
        request: super::HlsResourceRequest<'_>,
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
        let PlaybackMediaProvider::AcFun(PlaybackAcFunMedia::Refresh {
            resource_kind,
            format: AcFunPlaybackFormat::Hls,
            ..
        }) = &media.provider
        else {
            return Err(ProviderError::InvalidConfig(
                "AcFun HLS resource requires an HLS playback media".to_string(),
            ));
        };
        let headers = acfun_headers(match resource_kind {
            AcFunPlaybackResourceKind::Video => AcFunResourceKind::Video,
            AcFunPlaybackResourceKind::Bangumi => AcFunResourceKind::Bangumi,
            AcFunPlaybackResourceKind::Live => AcFunResourceKind::Live,
        });
        if request.is_manifest {
            Ok(super::PlaybackTransportAction::M3u8Rewrite {
                url: request.target_url.to_string(),
                headers,
            })
        } else {
            Ok(super::PlaybackTransportAction::FetchAndForward {
                url: request.target_url.to_string(),
                headers,
                range_header: request.range_header.map(ToString::to_string),
                proxy_strategy: super::PlaybackResourceProxyStrategy::SliceCache,
            })
        }
    }

    pub async fn get_danmaku_file(
        &self,
        store: Option<&Arc<dyn super::ProviderStore>>,
        version: &str,
        mode_name: &str,
        media_index: usize,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        let resource =
            playback_resource(store, version, mode_name, media_index, request_context).await?;
        if resource.kind == AcFunResourceKind::Live {
            return Err(ProviderError::InvalidConfig(
                "AcFun VOD danmaku requires a video or bangumi resource".to_string(),
            ));
        }
        let metadata = self.client.metadata(&resource, None).await?;
        let resource_id = metadata.danmaku_resource_id.ok_or_else(|| {
            ProviderError::ApiError("AcFun danmaku resource ID is unavailable".to_string())
        })?;
        let comments = self
            .client
            .video_danmakus(&resource_id, None)
            .await?
            .into_iter()
            .map(map_file_danmaku)
            .collect();
        let body = serde_json::to_vec(&AcFunDanmakuDocument {
            version: 1,
            comments,
        })
        .map_err(|error| ProviderError::ApiError(error.to_string()))?;
        Ok(super::PlaybackTransportAction::DirectBody {
            body,
            content_type: "application/json; charset=utf-8".to_string(),
            status: 200,
        })
    }

    pub async fn watch_danmaku(
        &self,
        store: Option<&Arc<dyn super::ProviderStore>>,
        version: &str,
        mode_name: &str,
        media_index: usize,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<AcFunDanmakuStream, ProviderError> {
        let resource =
            playback_resource(store, version, mode_name, media_index, request_context).await?;
        if resource.kind != AcFunResourceKind::Live {
            return Err(ProviderError::InvalidConfig(
                "AcFun live danmaku requires a live resource".to_string(),
            ));
        }
        let session = self
            .client
            .resolve(&resource, None)
            .await?
            .live_session
            .ok_or_else(|| ProviderError::ApiError("AcFun live is offline".to_string()))?;
        let stream = synctv_media_providers::acfun::watch_danmaku(session).await?;
        Ok(Box::pin(stream.map(|event| {
            event.map(map_live_danmaku).map_err(ProviderError::from)
        })))
    }

    fn playback_result(media: AcFunMedia) -> Result<PlaybackResult, ProviderError> {
        let AcFunMedia {
            metadata, playback, ..
        } = media;
        let mut infos = HashMap::new();
        for quality in playback.qualities {
            let format = match quality.format {
                AcFunStreamFormat::Hls => "m3u8",
                AcFunStreamFormat::Flv => "flv",
            };
            let mode = match quality.format {
                AcFunStreamFormat::Hls => "hls",
                AcFunStreamFormat::Flv => "flv",
            };
            let vod_resource_descriptor = match playback.resource.kind {
                AcFunResourceKind::Video | AcFunResourceKind::Bangumi => Some(format!(
                    "{}:{}:query:{}",
                    acfun_resource_kind_name(playback.resource.kind),
                    playback.resource.id,
                    playback.resource.query.as_deref().unwrap_or_default()
                )),
                AcFunResourceKind::Live => None,
            };
            let danmaku = match playback.resource.kind {
                AcFunResourceKind::Live => Some(PlaybackDanmaku {
                    name: "AcFun Live Danmaku".to_string(),
                    format: Some("synctv-acfun-live".to_string()),
                    p2p_swarm_id: None,
                    provider: PlaybackDanmakuProvider::AcFun(PlaybackAcFunDanmaku::LiveRefresh {
                        media_index: 0,
                    }),
                }),
                AcFunResourceKind::Video | AcFunResourceKind::Bangumi => Some(PlaybackDanmaku {
                    name: "AcFun Danmaku".to_string(),
                    format: Some("synctv-acfun-vod".to_string()),
                    p2p_swarm_id: vod_resource_descriptor.as_ref().map(|descriptor| {
                        super::provider_p2p_swarm_id(Self::NAME, None, "danmaku", descriptor)
                    }),
                    provider: PlaybackDanmakuProvider::AcFun(PlaybackAcFunDanmaku::FileRefresh {
                        media_index: 0,
                    }),
                }),
            };
            let info = infos
                .entry(mode.to_string())
                .or_insert_with(|| PlaybackInfo {
                    thumbnail: metadata.thumbnail_url.clone(),
                    medias: Vec::new(),
                    default_media_index: None,
                    subtitles: Vec::new(),
                    default_subtitle_index: None,
                    danmakus: danmaku.into_iter().collect(),
                    default_danmaku_index: Some(0),
                });
            let media_index = info.medias.len();
            let bitrate = quality.bitrate;
            let playback_format = playback_format(quality.format);
            let p2p_swarm_id = vod_resource_descriptor.as_ref().map(|descriptor| {
                super::provider_p2p_swarm_id(
                    Self::NAME,
                    None,
                    "media",
                    &format!(
                        "{descriptor}:quality:{}:type:{}:format:{}:bitrate:{}",
                        quality.name,
                        quality.quality_type.as_deref().unwrap_or_default(),
                        acfun_format_name(playback_format),
                        quality.bitrate.unwrap_or_default()
                    ),
                )
            });
            info.medias.push(PlaybackMedia {
                name: quality.name.clone(),
                format: format.to_string(),
                expire_at: None,
                metadata: Some(PlaybackMediaMetadata {
                    resolution: quality
                        .width
                        .zip(quality.height)
                        .map(|(width, height)| format!("{width}x{height}")),
                    bitrate: quality.bitrate.and_then(|value| i64::try_from(value).ok()),
                    codec: quality.codec.clone(),
                    fps: quality.fps.and_then(|value| i32::try_from(value).ok()),
                }),
                p2p_swarm_id,
                provider: PlaybackMediaProvider::AcFun(PlaybackAcFunMedia::Refresh {
                    resource_kind: resource_kind(playback.resource.kind),
                    resource_id: playback.resource.id.clone(),
                    query: playback.resource.query.clone(),
                    quality_name: quality.name,
                    quality_type: quality.quality_type,
                    format: playback_format,
                    bitrate: quality.bitrate,
                }),
            });
            let current_bitrate = info
                .default_media_index
                .and_then(|index| info.medias.get(index))
                .and_then(|media| media.metadata.as_ref())
                .and_then(|metadata| metadata.bitrate)
                .unwrap_or_default();
            if bitrate
                .and_then(|value| i64::try_from(value).ok())
                .unwrap_or_default()
                >= current_bitrate
            {
                info.default_media_index = Some(media_index);
            }
        }
        if infos.is_empty() {
            return Err(ProviderError::ApiError(
                "AcFun returned no playable qualities".to_string(),
            ));
        }
        let default_mode = infos
            .contains_key("hls")
            .then(|| "hls".to_string())
            .or_else(|| infos.keys().min().cloned())
            .ok_or_else(|| ProviderError::ApiError("AcFun playback is empty".to_string()))?;
        let duration_seconds = metadata.duration_seconds;
        let is_live = metadata.is_live;
        Ok(PlaybackResult {
            playback_infos: infos,
            default_mode,
            provider: crate::models::SourceProvider::AcFun,
            provider_instance_name: None,
            duration_seconds,
            playback_kind: Some(if is_live {
                crate::models::PlaybackKind::Live
            } else {
                crate::models::PlaybackKind::Regular
            }),
            metadata: Some(PlaybackMetadata::AcFun(metadata_model(
                metadata,
                playback.resource.kind == AcFunResourceKind::Live,
            ))),
        })
    }
}

async fn playback_resource(
    store: Option<&Arc<dyn super::ProviderStore>>,
    version: &str,
    mode_name: &str,
    media_index: usize,
    request_context: Option<&super::ExecutionControl>,
) -> Result<AcFunResource, ProviderError> {
    let versioned =
        super::playback_transport::lookup_versioned(store, version, request_context).await?;
    let media = versioned
        .result
        .playback_infos
        .get(mode_name)
        .and_then(|info| info.medias.get(media_index))
        .ok_or(ProviderError::NotFound)?;
    let PlaybackMediaProvider::AcFun(PlaybackAcFunMedia::Refresh {
        resource_kind,
        resource_id,
        query,
        ..
    }) = &media.provider
    else {
        return Err(ProviderError::InvalidConfig(
            "AcFun cached danmaku resource is invalid".to_string(),
        ));
    };
    Ok(resource_from_playback(
        *resource_kind,
        resource_id.clone(),
        query.clone(),
    ))
}

fn map_file_danmaku(value: AcFunDanmaku) -> AcFunDanmakuComment {
    AcFunDanmakuComment {
        id: value.id,
        user_id: value.user_id,
        text: value.text,
        color: format!("#{:06X}", value.color.min(0xff_ff_ff)),
        position_ms: value.position_ms,
        created_at_ms: value.created_at_ms,
        mode: value.mode,
        size: value.size,
    }
}

fn map_live_danmaku(value: UpstreamLiveDanmaku) -> AcFunLiveDanmakuEvent {
    AcFunLiveDanmakuEvent {
        id: value.id,
        user_id: value.user_id,
        user_name: value.user_name,
        avatar_url: value.avatar_url,
        text: value.text,
        color: value.color,
        badge_name: value.badge_name,
        badge_level: value.badge_level,
        sent_at_ms: value.sent_at_ms,
    }
}

fn metadata_model(metadata: AcFunMetadata, is_live_resource: bool) -> AcFunPlaybackMetadata {
    AcFunPlaybackMetadata {
        resource_id: metadata.id,
        title: metadata.title,
        author: metadata.author,
        author_id: metadata.author_id,
        category: metadata.category,
        thumbnail_url: metadata.thumbnail_url,
        avatar_url: metadata.avatar_url,
        description: metadata.description,
        tags: metadata.tags,
        view_count: metadata.view_count,
        like_count: metadata.like_count,
        comment_count: metadata.comment_count,
        published_at: metadata.published_at,
        started_at: metadata.started_at,
        is_live: is_live_resource,
        is_currently_live: is_live_resource.then_some(metadata.is_live),
    }
}

fn acfun_headers(kind: AcFunResourceKind) -> HashMap<String, String> {
    let referer = match kind {
        AcFunResourceKind::Live => "https://live.acfun.cn/",
        AcFunResourceKind::Video | AcFunResourceKind::Bangumi => "https://www.acfun.cn/",
    };
    HashMap::from([
        ("Origin".to_string(), "https://www.acfun.cn".to_string()),
        ("Referer".to_string(), referer.to_string()),
    ])
}

const fn resource_kind(kind: AcFunResourceKind) -> AcFunPlaybackResourceKind {
    match kind {
        AcFunResourceKind::Video => AcFunPlaybackResourceKind::Video,
        AcFunResourceKind::Bangumi => AcFunPlaybackResourceKind::Bangumi,
        AcFunResourceKind::Live => AcFunPlaybackResourceKind::Live,
    }
}

const fn resource_from_playback(
    kind: AcFunPlaybackResourceKind,
    id: String,
    query: Option<String>,
) -> AcFunResource {
    AcFunResource {
        kind: match kind {
            AcFunPlaybackResourceKind::Video => AcFunResourceKind::Video,
            AcFunPlaybackResourceKind::Bangumi => AcFunResourceKind::Bangumi,
            AcFunPlaybackResourceKind::Live => AcFunResourceKind::Live,
        },
        id,
        query,
    }
}

const fn playback_format(format: AcFunStreamFormat) -> AcFunPlaybackFormat {
    match format {
        AcFunStreamFormat::Hls => AcFunPlaybackFormat::Hls,
        AcFunStreamFormat::Flv => AcFunPlaybackFormat::Flv,
    }
}

const fn acfun_resource_kind_name(kind: AcFunResourceKind) -> &'static str {
    match kind {
        AcFunResourceKind::Video => "video",
        AcFunResourceKind::Bangumi => "bangumi",
        AcFunResourceKind::Live => "live",
    }
}

const fn acfun_format_name(format: AcFunPlaybackFormat) -> &'static str {
    match format {
        AcFunPlaybackFormat::Hls => "hls",
        AcFunPlaybackFormat::Flv => "flv",
    }
}

fn mark_acfun_playback_resources(
    result: &mut PlaybackResult,
    version: &str,
    expires_at: i64,
    client_profile: Option<&super::PlaybackClientProfile>,
) {
    let original_default = result.default_mode.clone();
    for (mode_name, info) in &mut result.playback_infos {
        let source_medias = std::mem::take(&mut info.medias);
        let supported_indices = source_medias
            .iter()
            .enumerate()
            .filter_map(|(media_index, media)| {
                super::proxy_playback_media_supported_by_client(client_profile, mode_name, media)
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
                    PlaybackMediaProvider::AcFun(PlaybackAcFunMedia::Refresh { .. })
                ) {
                    media.provider = PlaybackMediaProvider::AcFun(PlaybackAcFunMedia::Proxy {
                        version: version.to_string(),
                        expires_at,
                        mode_name: mode_name.clone(),
                        media_index,
                    });
                }
                Some(media)
            },
        );
        info.medias = medias;
        info.default_media_index = default_media_index;
        let (danmakus, default_danmaku_index) = super::map_playback_resources(
            &info.danmakus,
            info.default_danmaku_index,
            |_, danmaku| match &danmaku.provider {
                PlaybackDanmakuProvider::AcFun(
                    PlaybackAcFunDanmaku::FileRefresh { media_index }
                    | PlaybackAcFunDanmaku::LiveRefresh { media_index },
                ) if !supported_indices.contains(media_index) => None,
                _ => Some(danmaku.clone()),
            },
        );
        info.danmakus = danmakus;
        info.default_danmaku_index = default_danmaku_index;
        for danmaku in &mut info.danmakus {
            danmaku.provider = match &danmaku.provider {
                PlaybackDanmakuProvider::AcFun(PlaybackAcFunDanmaku::FileRefresh {
                    media_index,
                }) => PlaybackDanmakuProvider::AcFun(PlaybackAcFunDanmaku::FileProxy {
                    version: version.to_string(),
                    expires_at,
                    mode_name: mode_name.clone(),
                    media_index: *media_index,
                }),
                PlaybackDanmakuProvider::AcFun(PlaybackAcFunDanmaku::LiveRefresh {
                    media_index,
                }) => PlaybackDanmakuProvider::AcFun(PlaybackAcFunDanmaku::LiveProxy {
                    version: version.to_string(),
                    expires_at,
                    mode_name: mode_name.clone(),
                    media_index: *media_index,
                }),
                _ => continue,
            };
        }
    }
    result
        .playback_infos
        .retain(|_, info| !info.medias.is_empty());
    super::select_generated_playback_default(result, &original_default, true);
}

#[async_trait]
impl MediaProvider for AcFunProvider {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    async fn media_metadata(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &MediaSourceConfig,
    ) -> Result<Option<PlaybackMetadata>, ProviderError> {
        ctx.check_active()
            .map_err(|error| ProviderError::NetworkError(error.to_string()))?;
        let resource = Self::resource(Self::config(source_config)?)?;
        let cache_ttl = if resource.kind == AcFunResourceKind::Live {
            Duration::from_secs(15)
        } else {
            Duration::from_hours(2)
        };
        super::cached_provider_metadata_or_fill(
            Self::NAME,
            &Self::cache_key(&resource),
            cache_ttl,
            ctx,
            || async move {
                let metadata = self.client.metadata(&resource, None).await?;
                Ok(Some(PlaybackMetadata::AcFun(metadata_model(
                    metadata,
                    resource.kind == AcFunResourceKind::Live,
                ))))
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
        let resource = Self::resource(Self::config(source_config)?)?;
        let cache_ttl = if resource.kind == AcFunResourceKind::Live {
            Duration::from_mins(2)
        } else {
            Duration::from_hours(2)
        };
        let client_profile = ctx.playback_client_profile();
        let result = super::cached_versioned_playback_or_fill(
            Self::NAME,
            &Self::cache_key(&resource),
            cache_ttl,
            ctx,
            |result, version, expires_at| {
                mark_acfun_playback_resources(result, version, expires_at, client_profile);
            },
            || async { Self::playback_result(self.client.resolve(&resource, None).await?) },
        )
        .await?;
        super::require_compatible_playback_route(
            result,
            crate::models::PlaybackProxyMode::Only,
            client_profile,
        )
    }

    async fn validate_source_config(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<(), ProviderError> {
        ctx.check_active()
            .map_err(|error| ProviderError::NetworkError(error.to_string()))?;
        let resource = Self::resource(Self::source_config(source_config)?)?;
        self.client.metadata(&resource, None).await?;
        Ok(())
    }

    async fn source_cover(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<Option<SourceCover>, ProviderError> {
        ctx.check_active()
            .map_err(|error| ProviderError::NetworkError(error.to_string()))?;
        let resource = Self::resource(Self::source_config(source_config)?)?;
        Ok(self
            .client
            .metadata(&resource, None)
            .await?
            .thumbnail_url
            .map(|url| SourceCover::Url { url }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{
        HlsResourceRequest, InMemoryProviderStore, ProviderStore, ProviderStoreExt,
        VersionedPlayback,
    };
    use synctv_media_providers::acfun::{AcFunPlayback, AcFunQuality};

    #[test]
    fn playback_preserves_acfun_resource_quality_and_vod_danmaku() {
        let result = AcFunProvider::playback_result(AcFunMedia {
            metadata: AcFunMetadata {
                id: "ac123_2".to_string(),
                title: "Video P02".to_string(),
                author: "Author".to_string(),
                author_id: Some("42".to_string()),
                category: None,
                thumbnail_url: Some("https://img.test/cover.jpg".to_string()),
                avatar_url: None,
                description: None,
                tags: vec!["Animation".to_string()],
                is_live: false,
                duration_seconds: Some(90.0),
                view_count: Some(100),
                like_count: Some(5),
                comment_count: Some(3),
                published_at: Some(1_700_000_000),
                started_at: None,
                danmaku_resource_id: Some("1002".to_string()),
            },
            playback: AcFunPlayback {
                resource: AcFunResource {
                    kind: AcFunResourceKind::Video,
                    id: "ac123_2".to_string(),
                    query: None,
                },
                qualities: vec![AcFunQuality {
                    name: "1080P".to_string(),
                    url: "https://media.test/video.m3u8".to_string(),
                    format: AcFunStreamFormat::Hls,
                    bitrate: Some(4_000_000),
                    width: Some(1920),
                    height: Some(1080),
                    fps: Some(60),
                    codec: Some("avc1.64002a".to_string()),
                    quality_type: Some("QUALITY_1080P".to_string()),
                }],
            },
            live_session: None,
        })
        .expect("AcFun playback should map");
        let info = &result.playback_infos[&result.default_mode];
        assert_eq!(result.duration_seconds, Some(90.0));
        assert_eq!(info.danmakus[0].format.as_deref(), Some("synctv-acfun-vod"));
        assert!(info.medias[0].p2p_swarm_id.is_some());
        assert!(info.danmakus[0].p2p_swarm_id.is_some());
        assert!(matches!(
            info.medias[0].provider,
            PlaybackMediaProvider::AcFun(PlaybackAcFunMedia::Refresh {
                resource_kind: AcFunPlaybackResourceKind::Video,
                format: AcFunPlaybackFormat::Hls,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn hls_resource_restores_the_indexed_resource_referer() {
        let store: Arc<dyn ProviderStore> = Arc::new(InMemoryProviderStore::new(8));
        let result = PlaybackResult {
            playback_infos: HashMap::from([(
                "hls".to_string(),
                PlaybackInfo {
                    thumbnail: None,
                    medias: vec![PlaybackMedia {
                        name: "Live HLS".to_string(),
                        format: "m3u8".to_string(),
                        expire_at: None,
                        metadata: None,
                        p2p_swarm_id: None,
                        provider: PlaybackMediaProvider::AcFun(PlaybackAcFunMedia::Refresh {
                            resource_kind: AcFunPlaybackResourceKind::Live,
                            resource_id: "123".to_string(),
                            query: None,
                            quality_name: "Live".to_string(),
                            quality_type: None,
                            format: AcFunPlaybackFormat::Hls,
                            bitrate: None,
                        }),
                    }],
                    default_media_index: Some(0),
                    subtitles: Vec::new(),
                    default_subtitle_index: None,
                    danmakus: Vec::new(),
                    default_danmaku_index: None,
                },
            )]),
            default_mode: "hls".to_string(),
            provider: crate::models::SourceProvider::AcFun,
            provider_instance_name: None,
            duration_seconds: None,
            playback_kind: Some(crate::models::PlaybackKind::Live),
            metadata: None,
        };
        store
            .set(
                "v:test",
                &VersionedPlayback {
                    version: "test".to_string(),
                    result,
                    expires_at: crate::SystemClock.now().timestamp() + 60,
                    playback_context: None,
                },
                Duration::from_mins(1),
            )
            .await
            .expect("version mapping should store");

        let action = AcFunProvider::new()
            .get_hls_resource(
                Some(&store),
                HlsResourceRequest {
                    version: "test",
                    mode_name: "hls",
                    media_index: 0,
                    target_url: "https://cdn.example/live/segment.ts",
                    is_manifest: false,
                    range_header: Some("bytes=0-99"),
                },
                None,
            )
            .await
            .expect("indexed HLS resource should resolve");

        assert!(matches!(
            action,
            super::super::PlaybackTransportAction::FetchAndForward {
                headers,
                range_header: Some(range),
                ..
            } if headers.get("Referer").map(String::as_str) == Some("https://live.acfun.cn/")
                && range == "bytes=0-99"
        ));
    }
}
