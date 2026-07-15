//! CCTV media provider adapter.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use super::{
    MediaProvider, PlaybackInfo, PlaybackResult, ProviderContext, ProviderError, SourceConfig,
    SourceCover,
};
use crate::models::{
    detect_direct_url_format, CctvChapterMetadata, CctvMediaSourceConfig, CctvPlaybackMetadata,
    CctvPlaybackStreamKind, MediaSourceConfig, PlaybackCctvMedia, PlaybackMedia,
    PlaybackMediaProvider, PlaybackMetadata,
};
use synctv_media_providers::cctv::{
    CctvClient, CctvMedia, CctvMetadata, CctvResource, CctvStreamKind,
};

pub struct CctvProvider {
    client: CctvClient,
}

impl Default for CctvProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CctvProvider {
    pub const NAME: &'static str = "cctv";

    #[must_use]
    pub fn new() -> Self {
        Self {
            client: CctvClient::new().expect("CCTV HTTP client should build"),
        }
    }

    #[must_use]
    pub fn with_http_client(client: reqwest::Client) -> Self {
        Self {
            client: CctvClient::with_http_client(client),
        }
    }

    fn config(source_config: &MediaSourceConfig) -> Result<&CctvMediaSourceConfig, ProviderError> {
        match source_config {
            MediaSourceConfig::Cctv(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "CCTV provider requires CCTV media source_config".to_string(),
            )),
        }
    }

    fn source_config(
        source_config: SourceConfig<'_>,
    ) -> Result<&CctvMediaSourceConfig, ProviderError> {
        match source_config {
            SourceConfig::Media(config) => Self::config(config),
            SourceConfig::DynamicPlaylist(_) => Err(ProviderError::InvalidConfig(
                "CCTV dynamic playlist source is unavailable".to_string(),
            )),
        }
    }

    fn resource(config: &CctvMediaSourceConfig) -> Result<CctvResource, ProviderError> {
        CctvClient::parse_resource(&config.resource).map_err(ProviderError::from)
    }

    pub async fn resolve_resource(&self, resource: &str) -> Result<CctvMedia, ProviderError> {
        let resource = CctvClient::parse_resource(resource)?;
        self.client
            .resolve(&resource)
            .await
            .map_err(ProviderError::from)
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
        let PlaybackMediaProvider::Cctv(PlaybackCctvMedia::Refresh {
            resource,
            stream_name,
            stream_kind,
        }) = &media.provider
        else {
            return Err(ProviderError::InvalidConfig(
                "CCTV cached playback resource is invalid".to_string(),
            ));
        };
        let resolved = self
            .client
            .resolve(&CctvClient::parse_resource(resource)?)
            .await?;
        let stream = resolved
            .playback
            .streams
            .into_iter()
            .find(|stream| {
                stream.name == *stream_name && playback_stream_kind(stream.kind) == *stream_kind
            })
            .ok_or(ProviderError::NotFound)?;
        super::playback_transport::transport_action_for_target_url(
            stream.url,
            cctv_headers(),
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
            cctv_headers(),
            range_header,
        )
    }

    fn playback_result(resource: &str, media: CctvMedia) -> Result<PlaybackResult, ProviderError> {
        let CctvMedia { metadata, playback } = media;
        let mut playback_infos = HashMap::new();
        for (index, stream) in playback.streams.into_iter().enumerate() {
            let mode = unique_mode_name(&playback_infos, &stream.name, stream.kind, index);
            let format = match stream.kind {
                CctvStreamKind::VideoHls | CctvStreamKind::AudioHls => "m3u8",
                CctvStreamKind::Http => detect_direct_url_format(&stream.url),
            };
            playback_infos.insert(
                mode,
                PlaybackInfo {
                    thumbnail: metadata.thumbnail_url.clone(),
                    medias: vec![PlaybackMedia {
                        name: stream.name.clone(),
                        format: format.to_string(),
                        expire_at: None,
                        metadata: None,
                        provider: PlaybackMediaProvider::Cctv(PlaybackCctvMedia::Refresh {
                            resource: resource.to_string(),
                            stream_name: stream.name,
                            stream_kind: playback_stream_kind(stream.kind),
                        }),
                    }],
                    default_media_index: Some(0),
                    subtitles: Vec::new(),
                    default_subtitle_index: None,
                    danmakus: Vec::new(),
                    default_danmaku_index: None,
                },
            );
        }
        let default_mode = playback_infos
            .iter()
            .find(|(_, info)| {
                info.medias
                    .first()
                    .is_some_and(|media| media.format == "m3u8")
            })
            .or_else(|| playback_infos.iter().next())
            .map(|(name, _)| name.clone())
            .ok_or_else(|| ProviderError::ApiError("CCTV playback is empty".to_string()))?;
        let duration_seconds = metadata.duration_seconds;
        Ok(PlaybackResult {
            playback_infos,
            default_mode,
            provider: Self::NAME.to_string(),
            provider_instance_name: None,
            duration_seconds,
            is_live: Some(false),
            metadata: Some(PlaybackMetadata::Cctv(metadata_model(metadata))),
        })
    }
}

fn metadata_model(metadata: CctvMetadata) -> CctvPlaybackMetadata {
    CctvPlaybackMetadata {
        video_id: metadata.video_id,
        title: metadata.title,
        description: metadata.description,
        uploader: metadata.uploader,
        producer: metadata.producer,
        channel: metadata.channel,
        column: metadata.column,
        tags: metadata.tags,
        thumbnail_url: metadata.thumbnail_url,
        published_at: metadata.published_at,
        protected: metadata.protected,
        chapters: metadata
            .chapters
            .into_iter()
            .map(|chapter| CctvChapterMetadata {
                id: chapter.id,
                title: chapter.title,
                start_ms: chapter.start_ms,
                end_ms: chapter.end_ms,
            })
            .collect(),
    }
}

fn cctv_headers() -> HashMap<String, String> {
    HashMap::from([
        ("Origin".to_string(), "https://www.cctv.com".to_string()),
        ("Referer".to_string(), "https://www.cctv.com/".to_string()),
    ])
}

const fn playback_stream_kind(kind: CctvStreamKind) -> CctvPlaybackStreamKind {
    match kind {
        CctvStreamKind::VideoHls => CctvPlaybackStreamKind::VideoHls,
        CctvStreamKind::AudioHls => CctvPlaybackStreamKind::AudioHls,
        CctvStreamKind::Http => CctvPlaybackStreamKind::Http,
    }
}

fn unique_mode_name(
    existing: &HashMap<String, PlaybackInfo>,
    name: &str,
    kind: CctvStreamKind,
    index: usize,
) -> String {
    let suffix = match kind {
        CctvStreamKind::VideoHls => "hls",
        CctvStreamKind::AudioHls => "audio",
        CctvStreamKind::Http => "http",
    };
    let base = format!("{name}_{suffix}")
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
    if existing.contains_key(&base) {
        format!("{base}_{index}")
    } else {
        base
    }
}

fn mark_cctv_playback_resources(result: &mut PlaybackResult, version: &str, expires_at: i64) {
    for (mode_name, info) in &mut result.playback_infos {
        for (media_index, media) in info.medias.iter_mut().enumerate() {
            if matches!(
                media.provider,
                PlaybackMediaProvider::Cctv(PlaybackCctvMedia::Refresh { .. })
            ) {
                media.provider = PlaybackMediaProvider::Cctv(PlaybackCctvMedia::Proxy {
                    version: version.to_string(),
                    expires_at,
                    mode_name: mode_name.clone(),
                    media_index,
                });
            }
        }
    }
}

#[async_trait]
impl MediaProvider for CctvProvider {
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
        let config = Self::config(source_config)?;
        let resource = Self::resource(config)?;
        super::cached_versioned_playback_or_fill(
            Self::NAME,
            &format!("playback:{}", config.resource),
            Duration::from_hours(2),
            ctx,
            mark_cctv_playback_resources,
            || async {
                Self::playback_result(&config.resource, self.client.resolve(&resource).await?)
            },
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
        self.client.metadata(&resource).await?;
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
            .metadata(&resource)
            .await?
            .thumbnail_url
            .map(|url| SourceCover::Url { url }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use synctv_media_providers::cctv::{CctvPlayback, CctvStream};

    #[test]
    fn playback_preserves_cctv_streams_chapters_and_metadata() {
        let result = CctvProvider::playback_result(
            "5c846c0518444308ba32c4159df3b3e0",
            CctvMedia {
                metadata: CctvMetadata {
                    video_id: "5c846c0518444308ba32c4159df3b3e0".to_string(),
                    title: "Episode".to_string(),
                    description: Some("Description".to_string()),
                    uploader: Some("Editor".to_string()),
                    producer: None,
                    channel: Some("CCTV-1".to_string()),
                    column: Some("Programme".to_string()),
                    tags: vec!["news".to_string()],
                    thumbnail_url: Some("https://img.test/cover.jpg".to_string()),
                    duration_seconds: Some(123.0),
                    published_at: Some(1_700_000_000),
                    chapters: vec![synctv_media_providers::cctv::CctvChapter {
                        id: "1".to_string(),
                        title: "Opening".to_string(),
                        start_ms: 0,
                        end_ms: 10_000,
                    }],
                    protected: false,
                },
                playback: CctvPlayback {
                    video_id: "5c846c0518444308ba32c4159df3b3e0".to_string(),
                    streams: vec![CctvStream {
                        name: "HLS".to_string(),
                        url: "https://media.test/master.m3u8".to_string(),
                        kind: CctvStreamKind::VideoHls,
                    }],
                },
            },
        )
        .expect("CCTV playback should map");
        assert_eq!(result.duration_seconds, Some(123.0));
        assert!(matches!(
            result.playback_infos[&result.default_mode].medias[0].provider,
            PlaybackMediaProvider::Cctv(PlaybackCctvMedia::Refresh {
                stream_kind: CctvPlaybackStreamKind::VideoHls,
                ..
            })
        ));
        let Some(PlaybackMetadata::Cctv(metadata)) = result.metadata else {
            panic!("CCTV metadata should be present");
        };
        assert_eq!(metadata.chapters.len(), 1);
    }
}
