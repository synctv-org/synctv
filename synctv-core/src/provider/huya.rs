//! Huya media provider adapter.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;

use super::{
    MediaProvider, PlaybackInfo, PlaybackResult, ProviderContext, ProviderError, SourceConfig,
    SourceCover,
};
use crate::models::{
    HuyaMediaSourceConfig, HuyaPlaybackFormat, HuyaPlaybackMetadata, HuyaPlaybackResourceKind,
    MediaSourceConfig, PlaybackDanmaku, PlaybackDanmakuProvider, PlaybackHuyaDanmaku,
    PlaybackHuyaMedia, PlaybackMedia, PlaybackMediaMetadata, PlaybackMediaProvider,
    PlaybackMetadata,
};
use synctv_media_providers::huya::{
    HuyaClient, HuyaMedia, HuyaMetadata, HuyaResource, HuyaResourceKind, HuyaStreamFormat,
};

pub struct HuyaProvider {
    client: HuyaClient,
}

pub type HuyaDanmakuStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<HuyaDanmakuEvent, ProviderError>> + Send + 'static>,
>;

#[derive(Debug, Clone)]
pub struct HuyaDanmakuEvent {
    pub id: String,
    pub user_id: String,
    pub user_name: String,
    pub text: String,
    pub color: Option<String>,
    pub avatar_url: Option<String>,
    pub sent_at_ms: Option<u64>,
}

impl Default for HuyaProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl HuyaProvider {
    pub const NAME: &'static str = "huya";

    #[must_use]
    pub fn new() -> Self {
        Self {
            client: HuyaClient::new().expect("Huya HTTP client should build"),
        }
    }

    #[must_use]
    pub fn with_http_client(client: reqwest::Client) -> Self {
        Self {
            client: HuyaClient::with_http_client(client),
        }
    }

    pub async fn resolve_resource(&self, resource: &str) -> Result<HuyaMedia, ProviderError> {
        let resource = HuyaClient::parse_resource(resource)?;
        Ok(self.client.resolve(&resource, None).await?)
    }

    fn config(source_config: &MediaSourceConfig) -> Result<&HuyaMediaSourceConfig, ProviderError> {
        match source_config {
            MediaSourceConfig::Huya(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "Huya provider requires Huya media source_config".to_string(),
            )),
        }
    }

    fn source_config(
        source_config: SourceConfig<'_>,
    ) -> Result<&HuyaMediaSourceConfig, ProviderError> {
        match source_config {
            SourceConfig::Media(config) => Self::config(config),
            SourceConfig::DynamicPlaylist(_) => Err(ProviderError::InvalidConfig(
                "Huya dynamic playlists are unavailable".to_string(),
            )),
        }
    }

    fn resource(config: &HuyaMediaSourceConfig) -> Result<HuyaResource, ProviderError> {
        let (kind, id) = match config {
            HuyaMediaSourceConfig::Live { room_id } => (HuyaResourceKind::Live, room_id),
            HuyaMediaSourceConfig::Video { video_id } => (HuyaResourceKind::Video, video_id),
        };
        if id.trim().is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Huya resource ID is required".to_string(),
            ));
        }
        let resource = HuyaClient::parse_resource(id)?;
        if resource.kind != kind {
            return Ok(HuyaResource {
                kind,
                id: id.trim().to_string(),
            });
        }
        Ok(resource)
    }

    fn cache_key(resource: &HuyaResource) -> String {
        format!(
            "playback:{}:{}",
            match resource.kind {
                HuyaResourceKind::Live => "live",
                HuyaResourceKind::Video => "video",
            },
            resource.id
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
        let PlaybackMediaProvider::Huya(PlaybackHuyaMedia::Refresh {
            resource_kind,
            resource_id,
            quality_name,
            cdn,
            format,
            bitrate,
        }) = &media.provider
        else {
            return Err(ProviderError::InvalidConfig(
                "Huya cached playback resource is invalid".to_string(),
            ));
        };
        let resource = HuyaResource {
            kind: match resource_kind {
                HuyaPlaybackResourceKind::Live => HuyaResourceKind::Live,
                HuyaPlaybackResourceKind::Video => HuyaResourceKind::Video,
            },
            id: resource_id.clone(),
        };
        let playback = self.client.playback(&resource, None).await?;
        let quality = playback
            .qualities
            .into_iter()
            .find(|quality| {
                quality.name == *quality_name
                    && quality.cdn == *cdn
                    && playback_format(quality.format) == *format
                    && quality.bitrate == *bitrate
            })
            .ok_or(ProviderError::NotFound)?;
        super::playback_transport::transport_action_for_target_url(
            quality.url,
            huya_headers(),
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
            huya_headers(),
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
    ) -> Result<HuyaDanmakuStream, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let media = versioned
            .result
            .playback_infos
            .get(mode_name)
            .and_then(|info| info.medias.get(media_index))
            .ok_or(ProviderError::NotFound)?;
        let PlaybackMediaProvider::Huya(PlaybackHuyaMedia::Refresh {
            resource_kind: HuyaPlaybackResourceKind::Live,
            resource_id,
            ..
        }) = &media.provider
        else {
            return Err(ProviderError::InvalidConfig(
                "Huya danmaku requires a live room resource".to_string(),
            ));
        };
        let identity = self.client.chat_identity(resource_id, None).await?;
        let stream = synctv_media_providers::huya::watch_danmaku(identity).await?;
        Ok(Box::pin(stream.map(|event| {
            event
                .map(|event| HuyaDanmakuEvent {
                    id: event.id,
                    user_id: event.user_id,
                    user_name: event.user_name,
                    text: event.text,
                    color: event.color,
                    avatar_url: event.avatar_url,
                    sent_at_ms: event.sent_at_ms,
                })
                .map_err(ProviderError::from)
        })))
    }

    fn playback_result(media: HuyaMedia) -> Result<PlaybackResult, ProviderError> {
        let HuyaMedia { metadata, playback } = media;
        let mut infos = HashMap::new();
        for quality in playback.qualities {
            let mode = route_name(&quality.cdn);
            let format = match quality.format {
                HuyaStreamFormat::Flv => "flv",
                HuyaStreamFormat::Hls => "m3u8",
            };
            let info = infos.entry(mode).or_insert_with(|| PlaybackInfo {
                thumbnail: metadata.thumbnail_url.clone(),
                medias: Vec::new(),
                default_media_index: None,
                subtitles: Vec::new(),
                default_subtitle_index: None,
                danmakus: (playback.resource.kind == HuyaResourceKind::Live)
                    .then(|| PlaybackDanmaku {
                        name: "Huya Danmaku".to_string(),
                        format: Some("synctv-huya-live".to_string()),
                        provider: PlaybackDanmakuProvider::Huya(PlaybackHuyaDanmaku::Refresh {
                            media_index: 0,
                        }),
                    })
                    .into_iter()
                    .collect(),
                default_danmaku_index: (playback.resource.kind == HuyaResourceKind::Live)
                    .then_some(0),
            });
            let media_index = info.medias.len();
            let is_preferred = matches!(quality.format, HuyaStreamFormat::Hls)
                && (quality.name.contains("流畅") || quality.name.eq_ignore_ascii_case("smooth"));
            info.medias.push(PlaybackMedia {
                name: quality.name.clone(),
                format: format.to_string(),
                expire_at: None,
                metadata: Some(PlaybackMediaMetadata {
                    resolution: quality
                        .width
                        .zip(quality.height)
                        .map(|(width, height)| format!("{width}x{height}")),
                    bitrate: quality
                        .bitrate
                        .and_then(|value| value.checked_mul(1_000))
                        .and_then(|value| i64::try_from(value).ok()),
                    codec: Some("avc".to_string()),
                    fps: None,
                }),
                provider: PlaybackMediaProvider::Huya(PlaybackHuyaMedia::Refresh {
                    resource_kind: resource_kind(playback.resource.kind),
                    resource_id: playback.resource.id.clone(),
                    quality_name: quality.name,
                    cdn: quality.cdn,
                    format: playback_format(quality.format),
                    bitrate: quality.bitrate,
                }),
            });
            if is_preferred || info.default_media_index.is_none() {
                info.default_media_index = Some(media_index);
            }
        }
        if infos.is_empty() {
            return Err(ProviderError::ApiError(
                "Huya returned no playable qualities".to_string(),
            ));
        }
        let default_mode = infos
            .keys()
            .min()
            .cloned()
            .ok_or_else(|| ProviderError::ApiError("Huya playback is empty".to_string()))?;
        Ok(PlaybackResult {
            playback_infos: infos,
            default_mode,
            provider: Self::NAME.to_string(),
            provider_instance_name: None,
            duration_seconds: metadata
                .duration_seconds
                .map(|value| std::time::Duration::from_secs(value).as_secs_f64()),
            is_live: Some(metadata.is_live),
            metadata: Some(PlaybackMetadata::Huya(metadata_model(metadata))),
        })
    }
}

fn route_name(cdn: &str) -> String {
    let normalized = cdn
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
    if normalized.is_empty() {
        "main".to_string()
    } else {
        normalized
    }
}

fn metadata_model(metadata: HuyaMetadata) -> HuyaPlaybackMetadata {
    HuyaPlaybackMetadata {
        resource_id: metadata.id,
        title: metadata.title,
        author: metadata.author,
        author_id: metadata.author_id,
        category: metadata.category,
        thumbnail_url: metadata.thumbnail_url,
        avatar_url: metadata.avatar_url,
        description: metadata.description,
        view_count: metadata.view_count,
        comment_count: metadata.comment_count,
        like_count: metadata.like_count,
        published_at: metadata.published_at,
    }
}

fn huya_headers() -> HashMap<String, String> {
    HashMap::from([
        ("Origin".to_string(), "https://www.huya.com".to_string()),
        ("Referer".to_string(), "https://www.huya.com/".to_string()),
    ])
}

const fn resource_kind(kind: HuyaResourceKind) -> HuyaPlaybackResourceKind {
    match kind {
        HuyaResourceKind::Live => HuyaPlaybackResourceKind::Live,
        HuyaResourceKind::Video => HuyaPlaybackResourceKind::Video,
    }
}

const fn playback_format(format: HuyaStreamFormat) -> HuyaPlaybackFormat {
    match format {
        HuyaStreamFormat::Flv => HuyaPlaybackFormat::Flv,
        HuyaStreamFormat::Hls => HuyaPlaybackFormat::Hls,
    }
}

fn mark_huya_playback_resources(result: &mut PlaybackResult, version: &str, expires_at: i64) {
    for (mode_name, info) in &mut result.playback_infos {
        for (media_index, media) in info.medias.iter_mut().enumerate() {
            if matches!(
                media.provider,
                PlaybackMediaProvider::Huya(PlaybackHuyaMedia::Refresh { .. })
            ) {
                media.provider = PlaybackMediaProvider::Huya(PlaybackHuyaMedia::Proxy {
                    version: version.to_string(),
                    expires_at,
                    mode_name: mode_name.clone(),
                    media_index,
                });
            }
        }
        for danmaku in &mut info.danmakus {
            let PlaybackDanmakuProvider::Huya(PlaybackHuyaDanmaku::Refresh { media_index }) =
                &danmaku.provider
            else {
                continue;
            };
            danmaku.provider = PlaybackDanmakuProvider::Huya(PlaybackHuyaDanmaku::Proxy {
                version: version.to_string(),
                expires_at,
                mode_name: mode_name.clone(),
                media_index: *media_index,
            });
        }
    }
}

#[async_trait]
impl MediaProvider for HuyaProvider {
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
        let cache_ttl = match resource.kind {
            HuyaResourceKind::Live => Duration::from_mins(2),
            HuyaResourceKind::Video => Duration::from_hours(2),
        };
        super::cached_versioned_playback_or_fill(
            Self::NAME,
            &Self::cache_key(&resource),
            cache_ttl,
            ctx,
            mark_huya_playback_resources,
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
    use synctv_media_providers::huya::HuyaPlayback;
    use synctv_media_providers::huya::HuyaQuality;

    #[test]
    fn playback_preserves_huya_quality_identity() {
        let result = HuyaProvider::playback_result(HuyaMedia {
            metadata: HuyaMetadata {
                id: "660000".to_string(),
                title: "Live".to_string(),
                author: "Streamer".to_string(),
                author_id: Some("1".to_string()),
                category: Some("Game".to_string()),
                thumbnail_url: Some("https://img.test/live.jpg".to_string()),
                avatar_url: None,
                is_live: true,
                description: None,
                duration_seconds: None,
                view_count: Some(42),
                comment_count: None,
                like_count: None,
                published_at: None,
                presenter_uid: Some(1),
            },
            playback: HuyaPlayback {
                resource: HuyaResource {
                    kind: HuyaResourceKind::Live,
                    id: "660000".to_string(),
                },
                qualities: vec![
                    HuyaQuality {
                        name: "HDR".to_string(),
                        cdn: "TX".to_string(),
                        format: HuyaStreamFormat::Hls,
                        url: "https://hls.test/hdr.m3u8".to_string(),
                        bitrate: Some(4_200),
                        width: Some(1920),
                        height: Some(1080),
                    },
                    HuyaQuality {
                        name: "Original".to_string(),
                        cdn: "AL".to_string(),
                        format: HuyaStreamFormat::Hls,
                        url: "https://hls.test/live.m3u8".to_string(),
                        bitrate: None,
                        width: Some(1920),
                        height: Some(1080),
                    },
                    HuyaQuality {
                        name: "流畅".to_string(),
                        cdn: "AL".to_string(),
                        format: HuyaStreamFormat::Hls,
                        url: "https://hls.test/smooth.m3u8".to_string(),
                        bitrate: None,
                        width: Some(800),
                        height: Some(480),
                    },
                ],
            },
        })
        .expect("playback should map");
        assert_eq!(result.default_mode, "al");
        assert_eq!(result.playback_infos["al"].medias.len(), 2);
        assert!(matches!(
            result.playback_infos["al"].medias[0].provider,
            PlaybackMediaProvider::Huya(PlaybackHuyaMedia::Refresh {
                format: HuyaPlaybackFormat::Hls,
                ..
            })
        ));
        assert_eq!(result.playback_infos["al"].danmakus.len(), 1);
    }
}
