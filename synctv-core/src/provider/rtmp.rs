//! RTMP `MediaProvider`
//!
//! Provides HTTP-FLV and HLS playback media resources for SyncTV live streams published over RTMP.
//! URLs point to synctv's own HTTP-FLV and HLS endpoints.

use super::{
    playback_transport::PlaybackTransportAction, MediaProvider, PlaybackResult, ProviderContext,
    ProviderError, SourceConfig,
};
use crate::models::media::{PlaybackMediaProvider, PlaybackRtmpMedia};
use crate::models::{MediaId, RoomId, RtmpMediaSourceConfig};
use crate::PublicIdCodec;
use async_trait::async_trait;
use serde_json::Value;
use std::time::Duration;

/// Fields that should not be allowed in `source_config`.
/// `RtmpProvider` only serves the current SyncTV media from runtime context.
/// Any external URL field could be abused.
const FORBIDDEN_URL_FIELDS: &[&str] = &[
    "url",
    "rtmp_url",
    "rtmps_url",
    "source_url",
    "stream_url",
    "external_url",
];

fn parse_rtmp_source_config(source_config: &Value) -> Result<RtmpMediaSourceConfig, ProviderError> {
    super::parse_source_config(source_config, "RTMP")
}

/// RTMP `MediaProvider`
pub struct RtmpProvider {}

impl RtmpProvider {
    pub const NAME: &'static str = "rtmp";

    pub const fn new() -> Self {
        Self {}
    }

    fn resolve_live_binding<'a>(
        ctx: &'a ProviderContext<'a>,
    ) -> Result<(&'a RoomId, &'a MediaId), ProviderError> {
        let room_id = ctx.room_id().ok_or_else(|| {
            ProviderError::InvalidConfig(
                "Missing room_id in provider context for live stream playback".to_string(),
            )
        })?;

        let media_id = ctx.media_id().ok_or_else(|| {
            ProviderError::InvalidConfig(
                "Missing media_id in provider context for live stream playback".to_string(),
            )
        })?;

        Ok((room_id, media_id))
    }

    fn validate_config_fields(source_config: &Value) -> Result<(), ProviderError> {
        // SSRF protection: reject any URL fields in source_config.
        // RtmpProvider only accepts synctv-managed live stream bindings.
        for field in FORBIDDEN_URL_FIELDS {
            if source_config.get(field).is_some() {
                return Err(ProviderError::InvalidConfig(format!(
                    "Field '{field}' is not supported. RtmpProvider does not accept external URLs."
                )));
            }
        }

        Ok(())
    }

    fn validate_config_shape(
        source_config: &Value,
    ) -> Result<RtmpMediaSourceConfig, ProviderError> {
        for field in ["room_id", "media_id"] {
            if source_config.get(field).is_some() {
                return Err(ProviderError::InvalidConfig(format!(
                    "Field '{field}' is not supported. Internal RTMP media identity comes from runtime context."
                )));
            }
        }

        parse_rtmp_source_config(source_config)
    }
}

fn mark_rtmp_playback_resources(result: &mut PlaybackResult, version: &str, expires_at: i64) {
    // RTMP playback is a SyncTV-managed live source. HLS and FLV use distinct
    // playback transport actions so clients can request either delivery format.
    for (mode_name, info) in &mut result.playback_infos {
        let is_hls = super::playback_info_is_hls(mode_name, info);
        for media in &mut info.medias {
            let (room_id, media_id) = match &media.provider {
                PlaybackMediaProvider::Rtmp(
                    PlaybackRtmpMedia::HlsPlaylist {
                        room_id, media_id, ..
                    }
                    | PlaybackRtmpMedia::FlvStream {
                        room_id, media_id, ..
                    },
                ) => (*room_id, *media_id),
                _ => continue,
            };

            media.provider = if is_hls {
                PlaybackMediaProvider::Rtmp(PlaybackRtmpMedia::HlsPlaylist {
                    version: version.to_string(),
                    expires_at,
                    room_id,
                    media_id,
                })
            } else if mode_name == "flv" {
                PlaybackMediaProvider::Rtmp(PlaybackRtmpMedia::FlvStream {
                    version: version.to_string(),
                    expires_at,
                    room_id,
                    media_id,
                })
            } else {
                continue;
            };
        }
    }
}

impl Default for RtmpProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MediaProvider for RtmpProvider {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    async fn generate_playback(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<PlaybackResult, ProviderError> {
        Self::validate_config_fields(source_config)?;
        let _config = Self::validate_config_shape(source_config)?;
        let (room_id, media_id) = Self::resolve_live_binding(ctx)?;

        let result = super::build_live_playback(*media_id, *room_id);

        let cache_key = format!("playback:{room_id}:{media_id}");
        let cache_ttl = Duration::from_mins(5); // 5 minutes for live
        super::cache_versioned_playback_and_build_response(
            result,
            Self::NAME,
            &cache_key,
            cache_ttl,
            ctx,
            mark_rtmp_playback_resources,
        )
        .await
    }

    async fn validate_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<(), ProviderError> {
        let source_config = source_config.value();
        Self::validate_config_fields(source_config)?;
        let _config = Self::validate_config_shape(source_config)?;
        Ok(())
    }
}

impl RtmpProvider {
    pub async fn get_flv_stream(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        request_context: Option<&super::ExecutionControl>,
        claims: &crate::proxy_signature::ProxyUrlClaims,
        public_id_codec: &PublicIdCodec,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        super::live_helpers::build_flv_action(
            Self::NAME,
            &versioned,
            claims,
            public_id_codec,
            "RTMP",
        )
    }

    pub async fn get_hls_playlist(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        request_context: Option<&super::ExecutionControl>,
        public_id_codec: &PublicIdCodec,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        super::live_helpers::build_hls_playlist_action(
            Self::NAME,
            &versioned,
            public_id_codec,
            "RTMP",
        )
    }

    pub async fn get_hls_segment(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        segment_name: &str,
        request_context: Option<&super::ExecutionControl>,
        public_id_codec: &PublicIdCodec,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        super::live_helpers::build_hls_segment_action(
            Self::NAME,
            &versioned,
            segment_name,
            public_id_codec,
            "RTMP",
        )
    }
}
