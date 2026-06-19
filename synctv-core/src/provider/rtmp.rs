//! RTMP `MediaProvider`
//!
//! Provides HTTP-FLV and HLS playback media resources for SyncTV live streams published over RTMP.
//! URLs point to synctv's own HTTP-FLV and HLS endpoints.

use super::{
    playback_transport::PlaybackTransportAction, store::VersionedPlayback, MediaProvider,
    PlaybackResult, ProviderContext, ProviderError, SourceConfig,
};
use crate::models::media::{PlaybackMediaProvider, PlaybackRtmpMedia};
use crate::models::{MediaId, RoomId, TypedId};
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

    fn validate_config_shape(source_config: &Value) -> Result<(), ProviderError> {
        for field in ["room_id", "media_id"] {
            if source_config.get(field).is_some() {
                return Err(ProviderError::InvalidConfig(format!(
                    "Field '{field}' is not supported. Internal RTMP media identity comes from runtime context."
                )));
            }
        }

        Ok(())
    }

    fn metadata_typed_id<T>(
        versioned: &VersionedPlayback,
        field: &'static str,
        parse_public_id: impl FnOnce(&str) -> Result<T, ProviderError>,
    ) -> Result<T, ProviderError>
    where
        T: TypedId,
    {
        let value = versioned
            .result
            .metadata
            .get(field)
            .ok_or_else(|| ProviderError::ApiError(format!("Live playback missing {field}")))?;

        if let Some(id) = value.as_i64() {
            return T::try_from(id).map_err(|error| {
                ProviderError::InvalidConfig(format!(
                    "Invalid {field} in live playback metadata: {error}"
                ))
            });
        }

        if let Some(id) = value.as_u64() {
            let id = i64::try_from(id).map_err(|_| {
                ProviderError::InvalidConfig(format!(
                    "Invalid {field} in live playback metadata: exceeds i64"
                ))
            })?;
            return T::try_from(id).map_err(|error| {
                ProviderError::InvalidConfig(format!(
                    "Invalid {field} in live playback metadata: {error}"
                ))
            });
        }

        let value = value.as_str().ok_or_else(|| {
            ProviderError::InvalidConfig(format!(
                "Invalid {field} in live playback metadata: expected public ID string or numeric ID"
            ))
        })?;

        parse_public_id(value)
    }

    fn live_ids_from_metadata(
        versioned: &VersionedPlayback,
        public_id_codec: &PublicIdCodec,
    ) -> Result<(RoomId, MediaId), ProviderError> {
        let room_id = Self::metadata_typed_id(versioned, "room_id", |room_id| {
            super::playback_transport::parse_playback_room_id(
                public_id_codec,
                room_id,
                "live stream playback metadata",
            )
        })?;
        let media_id = Self::metadata_typed_id(versioned, "media_id", |media_id| {
            super::playback_transport::parse_playback_media_id(
                public_id_codec,
                media_id,
                "live stream playback metadata",
            )
        })?;
        Ok((room_id, media_id))
    }

    fn build_flv_action(
        versioned: &VersionedPlayback,
        claims: &crate::proxy_signature::ProxyUrlClaims,
        public_id_codec: &PublicIdCodec,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        let (room_id, media_id) = Self::live_ids_from_metadata(versioned, public_id_codec)?;
        Ok(PlaybackTransportAction::LiveFlv {
            provider_name: Self::NAME.to_string(),
            room_id,
            media_id,
            user_id: super::playback_transport::parse_playback_user_id(
                public_id_codec,
                &claims.user_id,
                "RTMP proxy claims",
            )?,
            expires_at: claims.expires_at,
        })
    }

    fn build_hls_playlist_action(
        versioned: &VersionedPlayback,
        public_id_codec: &PublicIdCodec,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        let (room_id, media_id) = Self::live_ids_from_metadata(versioned, public_id_codec)?;
        Ok(PlaybackTransportAction::LiveHlsPlaylist {
            provider_name: Self::NAME.to_string(),
            room_id,
            media_id,
            version: versioned.version.clone(),
        })
    }

    fn build_hls_segment_action(
        versioned: &VersionedPlayback,
        segment_name: &str,
        public_id_codec: &PublicIdCodec,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        let (room_id, media_id) = Self::live_ids_from_metadata(versioned, public_id_codec)?;
        Ok(PlaybackTransportAction::LiveHlsSegment {
            provider_name: Self::NAME.to_string(),
            room_id,
            media_id,
            segment_name: segment_name.to_string(),
            disguised_as_png: segment_name.ends_with(".png"),
        })
    }
}

fn mark_rtmp_playback_resources(result: &mut PlaybackResult, version: &str, expires_at: i64) {
    // RTMP playback is a SyncTV-managed live source. HLS and FLV use distinct
    // playback transport actions so clients can request either delivery format.
    for (mode_name, info) in &mut result.playback_infos {
        let is_hls = super::playback_info_is_hls(mode_name, info);
        for media in &mut info.medias {
            media.provider = if is_hls {
                PlaybackMediaProvider::Rtmp(PlaybackRtmpMedia::HlsPlaylist {
                    version: version.to_string(),
                    expires_at,
                    room_id: match &media.provider {
                        PlaybackMediaProvider::Rtmp(
                            PlaybackRtmpMedia::HlsPlaylist { room_id, .. }
                            | PlaybackRtmpMedia::FlvStream { room_id, .. },
                        ) => *room_id,
                        _ => continue,
                    },
                    media_id: match &media.provider {
                        PlaybackMediaProvider::Rtmp(
                            PlaybackRtmpMedia::HlsPlaylist { media_id, .. }
                            | PlaybackRtmpMedia::FlvStream { media_id, .. },
                        ) => *media_id,
                        _ => continue,
                    },
                })
            } else if mode_name == "flv" {
                PlaybackMediaProvider::Rtmp(PlaybackRtmpMedia::FlvStream {
                    version: version.to_string(),
                    expires_at,
                    room_id: match &media.provider {
                        PlaybackMediaProvider::Rtmp(
                            PlaybackRtmpMedia::HlsPlaylist { room_id, .. }
                            | PlaybackRtmpMedia::FlvStream { room_id, .. },
                        ) => *room_id,
                        _ => continue,
                    },
                    media_id: match &media.provider {
                        PlaybackMediaProvider::Rtmp(
                            PlaybackRtmpMedia::HlsPlaylist { media_id, .. }
                            | PlaybackRtmpMedia::FlvStream { media_id, .. },
                        ) => *media_id,
                        _ => continue,
                    },
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
        Self::validate_config_shape(source_config)?;
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
        Self::validate_config_shape(source_config)?;
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
        Self::build_flv_action(&versioned, claims, public_id_codec)
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
        Self::build_hls_playlist_action(&versioned, public_id_codec)
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
        Self::build_hls_segment_action(&versioned, segment_name, public_id_codec)
    }
}
