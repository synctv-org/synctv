//! Douyu media provider adapter.

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
    DouyuMediaSourceConfig, DouyuPlaybackCodec, DouyuPlaybackFormat, DouyuPlaybackMetadata,
    MediaSourceConfig, PlaybackDanmaku, PlaybackDanmakuProvider, PlaybackDouyuDanmaku,
    PlaybackDouyuMedia, PlaybackMedia, PlaybackMediaMetadata, PlaybackMediaProvider,
    PlaybackMetadata,
};
use synctv_media_providers::douyu::{
    DouyuClient, DouyuCodec, DouyuDanmakuEvent as UpstreamDanmakuEvent, DouyuMedia, DouyuMetadata,
    DouyuResource, DouyuStreamFormat,
};

pub struct DouyuProvider {
    client: DouyuClient,
}

pub type DouyuDanmakuStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<DouyuDanmakuEvent, ProviderError>> + Send + 'static>,
>;

#[derive(Debug, Clone)]
pub struct DouyuDanmakuEvent {
    pub id: String,
    pub user_id: String,
    pub user_name: String,
    pub text: String,
    pub color: Option<String>,
    pub level: Option<u32>,
    pub badge_name: Option<String>,
    pub badge_level: Option<u32>,
    pub sent_at_ms: Option<u64>,
}

impl Default for DouyuProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl DouyuProvider {
    pub const NAME: &'static str = "douyu";

    #[must_use]
    pub fn new() -> Self {
        Self {
            client: DouyuClient::new().expect("Douyu HTTP client should build"),
        }
    }

    #[must_use]
    pub fn with_http_client(client: reqwest::Client) -> Self {
        Self {
            client: DouyuClient::with_http_client(client),
        }
    }

    pub async fn resolve_resource(&self, resource: &str) -> Result<DouyuMedia, ProviderError> {
        let resource = DouyuClient::parse_resource(resource)?;
        Ok(self.client.resolve(&resource, None).await?)
    }

    fn config(source_config: &MediaSourceConfig) -> Result<&DouyuMediaSourceConfig, ProviderError> {
        match source_config {
            MediaSourceConfig::Douyu(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "Douyu provider requires Douyu media source_config".to_string(),
            )),
        }
    }

    fn source_config(
        source_config: SourceConfig<'_>,
    ) -> Result<&DouyuMediaSourceConfig, ProviderError> {
        match source_config {
            SourceConfig::Media(config) => Self::config(config),
            SourceConfig::DynamicPlaylist(_) => Err(ProviderError::InvalidConfig(
                "Douyu dynamic playlists are unavailable".to_string(),
            )),
        }
    }

    fn resource(config: &DouyuMediaSourceConfig) -> Result<DouyuResource, ProviderError> {
        Ok(DouyuClient::parse_resource(&config.room)?)
    }

    fn cache_key(resource: &DouyuResource) -> String {
        format!("playback:{}", resource.room)
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
        let PlaybackMediaProvider::Douyu(PlaybackDouyuMedia::Refresh {
            room_id,
            cdn,
            rate,
            codec,
            ..
        }) = &media.provider
        else {
            return Err(ProviderError::InvalidConfig(
                "Douyu cached playback resource is invalid".to_string(),
            ));
        };
        let variant = self
            .client
            .variant(room_id, cdn, *rate, upstream_codec(*codec), None)
            .await?;
        super::playback_transport::transport_action_for_target_url(
            variant.url,
            douyu_headers(),
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
            douyu_headers(),
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
    ) -> Result<DouyuDanmakuStream, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let media = versioned
            .result
            .playback_infos
            .get(mode_name)
            .and_then(|info| info.medias.get(media_index))
            .ok_or(ProviderError::NotFound)?;
        let PlaybackMediaProvider::Douyu(PlaybackDouyuMedia::Refresh { room_id, .. }) =
            &media.provider
        else {
            return Err(ProviderError::InvalidConfig(
                "Douyu cached danmaku resource is invalid".to_string(),
            ));
        };
        let stream = synctv_media_providers::douyu::watch_danmaku(room_id).await?;
        Ok(Box::pin(stream.map(|event| {
            event.map(map_danmaku).map_err(ProviderError::from)
        })))
    }

    fn playback_result(media: DouyuMedia) -> Result<PlaybackResult, ProviderError> {
        let DouyuMedia { metadata, playback } = media;
        let mut infos = HashMap::new();
        for quality in playback.qualities {
            let mode = route_name(&quality.cdn_name);
            let format = match quality.format {
                DouyuStreamFormat::Flv => "flv",
                DouyuStreamFormat::Hls => "m3u8",
            };
            let codec = match quality.codec {
                DouyuCodec::Avc => "avc",
                DouyuCodec::Hevc => "hevc",
                DouyuCodec::Aac => "aac",
            };
            let info = infos.entry(mode).or_insert_with(|| PlaybackInfo {
                thumbnail: metadata.thumbnail_url.clone(),
                medias: Vec::new(),
                default_media_index: None,
                subtitles: Vec::new(),
                default_subtitle_index: None,
                danmakus: metadata
                    .is_live
                    .then(|| PlaybackDanmaku {
                        name: "Douyu Danmaku".to_string(),
                        format: Some("synctv-douyu-live".to_string()),
                        p2p_swarm_id: None,
                        provider: PlaybackDanmakuProvider::Douyu(PlaybackDouyuDanmaku::Refresh {
                            media_index: 0,
                        }),
                    })
                    .into_iter()
                    .collect(),
                default_danmaku_index: metadata.is_live.then_some(0),
            });
            let media_index = info.medias.len();
            let is_preferred = quality.name.to_ascii_lowercase().contains("original")
                && matches!(quality.codec, DouyuCodec::Avc);
            info.medias.push(PlaybackMedia {
                name: quality.name.clone(),
                format: format.to_string(),
                expire_at: None,
                metadata: Some(PlaybackMediaMetadata {
                    resolution: None,
                    bitrate: quality
                        .bitrate
                        .and_then(|value| value.checked_mul(1_000))
                        .and_then(|value| i64::try_from(value).ok()),
                    codec: Some(codec.to_string()),
                    fps: None,
                }),
                p2p_swarm_id: None,
                provider: PlaybackMediaProvider::Douyu(PlaybackDouyuMedia::Refresh {
                    room_id: playback.room_id.clone(),
                    quality_name: quality.name,
                    cdn: quality.cdn,
                    rate: quality.rate,
                    codec: playback_codec(quality.codec),
                    format: playback_format(quality.format),
                }),
            });
            if is_preferred || info.default_media_index.is_none() {
                info.default_media_index = Some(media_index);
            }
        }
        if infos.is_empty() {
            return Err(ProviderError::ApiError(
                "Douyu returned no playable qualities".to_string(),
            ));
        }
        let default_mode = infos
            .keys()
            .min()
            .cloned()
            .ok_or_else(|| ProviderError::ApiError("Douyu playback is empty".to_string()))?;
        Ok(PlaybackResult {
            playback_infos: infos,
            default_mode,
            provider: Self::NAME.to_string(),
            provider_instance_name: None,
            duration_seconds: None,
            playback_kind: Some(if metadata.is_live {
                crate::models::PlaybackKind::Live
            } else {
                crate::models::PlaybackKind::Regular
            }),
            metadata: Some(PlaybackMetadata::Douyu(metadata_model(metadata))),
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

fn map_danmaku(event: UpstreamDanmakuEvent) -> DouyuDanmakuEvent {
    DouyuDanmakuEvent {
        id: event.id,
        user_id: event.user_id,
        user_name: event.user_name,
        text: event.text,
        color: event.color,
        level: event.level,
        badge_name: event.badge_name,
        badge_level: event.badge_level,
        sent_at_ms: event.sent_at_ms,
    }
}

fn metadata_model(metadata: DouyuMetadata) -> DouyuPlaybackMetadata {
    DouyuPlaybackMetadata {
        room_id: metadata.room_id,
        title: metadata.title,
        author: metadata.author,
        category: metadata.category,
        thumbnail_url: metadata.thumbnail_url,
        avatar_url: metadata.avatar_url,
        is_replay: metadata.is_replay,
        is_vip: metadata.is_vip,
        viewer_count: metadata.viewer_count,
        started_at: metadata.started_at,
    }
}

fn douyu_headers() -> HashMap<String, String> {
    HashMap::from([
        ("Origin".to_string(), "https://www.douyu.com".to_string()),
        ("Referer".to_string(), "https://www.douyu.com/".to_string()),
    ])
}

const fn playback_codec(codec: DouyuCodec) -> DouyuPlaybackCodec {
    match codec {
        DouyuCodec::Avc => DouyuPlaybackCodec::Avc,
        DouyuCodec::Hevc => DouyuPlaybackCodec::Hevc,
        DouyuCodec::Aac => DouyuPlaybackCodec::Aac,
    }
}

const fn upstream_codec(codec: DouyuPlaybackCodec) -> DouyuCodec {
    match codec {
        DouyuPlaybackCodec::Avc => DouyuCodec::Avc,
        DouyuPlaybackCodec::Hevc => DouyuCodec::Hevc,
        DouyuPlaybackCodec::Aac => DouyuCodec::Aac,
    }
}

const fn playback_format(format: DouyuStreamFormat) -> DouyuPlaybackFormat {
    match format {
        DouyuStreamFormat::Flv => DouyuPlaybackFormat::Flv,
        DouyuStreamFormat::Hls => DouyuPlaybackFormat::Hls,
    }
}

fn mark_douyu_playback_resources(result: &mut PlaybackResult, version: &str, expires_at: i64) {
    for (mode_name, info) in &mut result.playback_infos {
        for (media_index, media) in info.medias.iter_mut().enumerate() {
            if matches!(
                media.provider,
                PlaybackMediaProvider::Douyu(PlaybackDouyuMedia::Refresh { .. })
            ) {
                media.provider = PlaybackMediaProvider::Douyu(PlaybackDouyuMedia::Proxy {
                    version: version.to_string(),
                    expires_at,
                    mode_name: mode_name.clone(),
                    media_index,
                });
            }
        }
        for danmaku in &mut info.danmakus {
            let PlaybackDanmakuProvider::Douyu(PlaybackDouyuDanmaku::Refresh { media_index }) =
                &danmaku.provider
            else {
                continue;
            };
            danmaku.provider = PlaybackDanmakuProvider::Douyu(PlaybackDouyuDanmaku::Proxy {
                version: version.to_string(),
                expires_at,
                mode_name: mode_name.clone(),
                media_index: *media_index,
            });
        }
    }
}

#[async_trait]
impl MediaProvider for DouyuProvider {
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
        super::cached_versioned_playback_or_fill(
            Self::NAME,
            &Self::cache_key(&resource),
            Duration::from_mins(2),
            ctx,
            mark_douyu_playback_resources,
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
    use synctv_media_providers::douyu::{DouyuPlayback, DouyuQuality};

    #[test]
    fn playback_preserves_douyu_quality_identity() {
        let result = DouyuProvider::playback_result(DouyuMedia {
            metadata: DouyuMetadata {
                room_id: "123".to_string(),
                title: "Live".to_string(),
                author: "Anchor".to_string(),
                category: Some("Games".to_string()),
                thumbnail_url: Some("https://img.test/live.jpg".to_string()),
                avatar_url: None,
                is_live: true,
                is_replay: false,
                is_vip: false,
                viewer_count: Some(42),
                started_at: None,
            },
            playback: DouyuPlayback {
                room_id: "123".to_string(),
                qualities: vec![DouyuQuality {
                    name: "Original".to_string(),
                    cdn: "tct-h5".to_string(),
                    cdn_name: "Tencent".to_string(),
                    rate: 0,
                    bitrate: Some(8_000),
                    codec: DouyuCodec::Hevc,
                    format: DouyuStreamFormat::Flv,
                }],
            },
        })
        .expect("playback should map");
        assert_eq!(result.default_mode, "tencent");
        assert!(matches!(
            result.playback_infos["tencent"].medias[0].provider,
            PlaybackMediaProvider::Douyu(PlaybackDouyuMedia::Refresh {
                codec: DouyuPlaybackCodec::Hevc,
                ..
            })
        ));
        assert_eq!(result.playback_infos["tencent"].danmakus.len(), 1);
    }

    #[test]
    fn replay_does_not_attach_live_room_danmaku() {
        let result = DouyuProvider::playback_result(DouyuMedia {
            metadata: DouyuMetadata {
                room_id: "123".to_string(),
                title: "Replay".to_string(),
                author: "Anchor".to_string(),
                category: None,
                thumbnail_url: None,
                avatar_url: None,
                is_live: false,
                is_replay: true,
                is_vip: false,
                viewer_count: None,
                started_at: None,
            },
            playback: DouyuPlayback {
                room_id: "123".to_string(),
                qualities: vec![DouyuQuality {
                    name: "Replay".to_string(),
                    cdn: "tct-h5".to_string(),
                    cdn_name: "Tencent".to_string(),
                    rate: 0,
                    bitrate: None,
                    codec: DouyuCodec::Avc,
                    format: DouyuStreamFormat::Hls,
                }],
            },
        })
        .expect("replay should map");
        let info = result.playback_infos.values().next().expect("replay mode");
        assert!(info.danmakus.is_empty());
        assert_eq!(info.default_danmaku_index, None);
    }
}
