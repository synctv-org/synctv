//! RTMP `MediaProvider`
//!
//! Provides HTTP-FLV and HLS playback media resources for SyncTV live streams published over RTMP.
//! Playback output references SyncTV live delivery resources.

use super::{
    playback_transport::PlaybackTransportAction, MediaProvider, PlaybackResult, ProviderContext,
    ProviderError, SourceConfig,
};
use crate::models::media::{PlaybackMediaProvider, PlaybackRtmpMedia};
use crate::models::{MediaId, RoomId};
use async_trait::async_trait;
use std::time::Duration;

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
}

fn mark_rtmp_playback_resources(result: &mut PlaybackResult, version: &str, expires_at: i64) {
    // RTMP playback is a SyncTV-managed live source. HLS and FLV use distinct
    // playback transport actions so clients can request either delivery format.
    for (mode_name, info) in &mut result.playback_infos {
        let is_hls = super::playback_info_is_hls(mode_name, info);
        for media in &mut info.medias {
            let (room_id, media_id) = match &media.provider {
                PlaybackMediaProvider::Rtmp(
                    PlaybackRtmpMedia::HlsMaster {
                        room_id, media_id, ..
                    }
                    | PlaybackRtmpMedia::FlvStream {
                        room_id, media_id, ..
                    },
                ) => (*room_id, *media_id),
                _ => continue,
            };

            media.provider = if is_hls {
                PlaybackMediaProvider::Rtmp(PlaybackRtmpMedia::HlsMaster {
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
        source_config: &crate::models::MediaSourceConfig,
    ) -> Result<PlaybackResult, ProviderError> {
        let crate::models::MediaSourceConfig::Rtmp(config) = source_config else {
            return Err(ProviderError::InvalidConfig(
                "RTMP requires RTMP media source_config".to_string(),
            ));
        };
        let (room_id, media_id) = Self::resolve_live_binding(ctx)?;

        let _config = config;
        let result = super::build_live_playback(*media_id, *room_id);

        let cache_key = format!("playback:{room_id}:{media_id}");
        let cache_ttl = Duration::from_mins(5); // 5 minutes for live
        let result = super::cache_versioned_playback_and_build_response(
            result,
            Self::NAME,
            &cache_key,
            cache_ttl,
            ctx,
            mark_rtmp_playback_resources,
        )
        .await?;
        super::filter_playback_routes_by_client(
            result,
            crate::models::PlaybackProxyMode::Only,
            ctx.playback_client_profile(),
        )
    }

    async fn validate_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<(), ProviderError> {
        let SourceConfig::Media(crate::models::MediaSourceConfig::Rtmp(_)) = source_config else {
            return Err(ProviderError::InvalidConfig(
                "RTMP requires RTMP media source_config".to_string(),
            ));
        };
        Ok(())
    }
}

impl RtmpProvider {
    pub async fn get_flv_stream(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        request_context: Option<&super::ExecutionControl>,
        access: super::playback_transport::LiveFlvAccess,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        super::live_helpers::build_flv_action(Self::NAME, &versioned, access)
    }

    pub async fn get_hls_master(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        super::live_helpers::build_hls_master_action(Self::NAME, &versioned)
    }

    pub async fn get_hls_playlist(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        generation_id: &str,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        super::live_helpers::build_hls_playlist_action(Self::NAME, &versioned, generation_id)
    }

    pub async fn get_hls_segment(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        generation_id: &str,
        segment_name: &str,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        super::live_helpers::build_hls_segment_action(
            Self::NAME,
            &versioned,
            generation_id,
            segment_name,
        )
    }
}
