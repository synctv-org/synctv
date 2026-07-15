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
    AcFunMetadata, AcFunQuality, AcFunResource, AcFunResourceKind, AcFunStreamFormat,
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
        super::playback_transport::transport_action_for_target_url(
            quality.url,
            acfun_headers(resource.kind),
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
            acfun_headers(AcFunResourceKind::Video),
            range_header,
        )
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
        for (index, quality) in playback.qualities.into_iter().enumerate() {
            let mode = unique_mode_name(&infos, &quality, index);
            let format = match quality.format {
                AcFunStreamFormat::Hls => "m3u8",
                AcFunStreamFormat::Flv => "flv",
            };
            let danmaku = match playback.resource.kind {
                AcFunResourceKind::Live => Some(PlaybackDanmaku {
                    name: "AcFun Live Danmaku".to_string(),
                    format: Some("synctv-acfun-live".to_string()),
                    provider: PlaybackDanmakuProvider::AcFun(PlaybackAcFunDanmaku::LiveRefresh {
                        media_index: 0,
                    }),
                }),
                AcFunResourceKind::Video | AcFunResourceKind::Bangumi => Some(PlaybackDanmaku {
                    name: "AcFun Danmaku".to_string(),
                    format: Some("synctv-acfun-vod".to_string()),
                    provider: PlaybackDanmakuProvider::AcFun(PlaybackAcFunDanmaku::FileRefresh {
                        media_index: 0,
                    }),
                }),
            };
            infos.insert(
                mode,
                PlaybackInfo {
                    thumbnail: metadata.thumbnail_url.clone(),
                    medias: vec![PlaybackMedia {
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
                        provider: PlaybackMediaProvider::AcFun(PlaybackAcFunMedia::Refresh {
                            resource_kind: resource_kind(playback.resource.kind),
                            resource_id: playback.resource.id.clone(),
                            query: playback.resource.query.clone(),
                            quality_name: quality.name,
                            quality_type: quality.quality_type,
                            format: playback_format(quality.format),
                            bitrate: quality.bitrate,
                        }),
                    }],
                    default_media_index: Some(0),
                    subtitles: Vec::new(),
                    default_subtitle_index: None,
                    danmakus: danmaku.into_iter().collect(),
                    default_danmaku_index: Some(0),
                },
            );
        }
        if infos.is_empty() {
            return Err(ProviderError::ApiError(
                "AcFun returned no playable qualities".to_string(),
            ));
        }
        let default_mode = infos
            .iter()
            .max_by_key(|(_, info)| {
                info.medias
                    .first()
                    .and_then(|media| media.metadata.as_ref())
                    .and_then(|metadata| metadata.bitrate)
                    .unwrap_or_default()
            })
            .map(|(name, _)| name.clone())
            .ok_or_else(|| ProviderError::ApiError("AcFun playback is empty".to_string()))?;
        let duration_seconds = metadata.duration_seconds;
        let is_live = metadata.is_live;
        Ok(PlaybackResult {
            playback_infos: infos,
            default_mode,
            provider: Self::NAME.to_string(),
            provider_instance_name: None,
            duration_seconds,
            is_live: Some(is_live),
            metadata: Some(PlaybackMetadata::AcFun(metadata_model(metadata))),
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

fn metadata_model(metadata: AcFunMetadata) -> AcFunPlaybackMetadata {
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

fn unique_mode_name(
    existing: &HashMap<String, PlaybackInfo>,
    quality: &AcFunQuality,
    index: usize,
) -> String {
    let format = match quality.format {
        AcFunStreamFormat::Hls => "hls",
        AcFunStreamFormat::Flv => "flv",
    };
    let raw = format!("{}_{}", quality.name, format);
    let base = raw
        .to_ascii_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string();
    let base = if base.is_empty() {
        format!("quality_{index}")
    } else {
        base
    };
    if existing.contains_key(&base) {
        format!("{base}_{index}")
    } else {
        base
    }
}

fn mark_acfun_playback_resources(result: &mut PlaybackResult, version: &str, expires_at: i64) {
    for (mode_name, info) in &mut result.playback_infos {
        for (media_index, media) in info.medias.iter_mut().enumerate() {
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
        }
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
}

#[async_trait]
impl MediaProvider for AcFunProvider {
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
        let resource = Self::resource(Self::config(source_config)?)?;
        let cache_ttl = if resource.kind == AcFunResourceKind::Live {
            Duration::from_mins(2)
        } else {
            Duration::from_hours(2)
        };
        super::cached_versioned_playback_or_fill(
            Self::NAME,
            &Self::cache_key(&resource),
            cache_ttl,
            ctx,
            mark_acfun_playback_resources,
            || async { Self::playback_result(self.client.resolve(&resource, None).await?) },
        )
        .await
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
        assert!(matches!(
            info.medias[0].provider,
            PlaybackMediaProvider::AcFun(PlaybackAcFunMedia::Refresh {
                resource_kind: AcFunPlaybackResourceKind::Video,
                format: AcFunPlaybackFormat::Hls,
                ..
            })
        ));
    }
}
