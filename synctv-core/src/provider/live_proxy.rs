//! `LiveProxy` `MediaProvider`
//!
//! Provides playback media resources for live streams sourced from external URLs.
//! The external source URL is stored in `source_config`, while the internal
//! room/media binding comes from the runtime provider context. Playback output
//! points to SyncTV live delivery resources.
//!
//! The `PullStreamManager` handles the actual pulling from the external source.

use super::{
    playback_transport::PlaybackTransportAction, MediaProvider, PlaybackResult, ProviderContext,
    ProviderError, SourceConfig,
};
use crate::models::media::{
    LiveProxyPlaybackMetadata, PlaybackLiveProxyMedia, PlaybackMediaProvider, PlaybackMetadata,
    PlaybackRtmpMedia,
};
use crate::models::{MediaId, RoomId};
use async_trait::async_trait;
use base64::Engine as _;
use serde_json::Value;
use std::time::Duration;
use synctv_common::ssrf::SsrfTargetError;

/// `LiveProxy` `MediaProvider`
///
/// Generates playback media resources for live streams from external sources.
/// The external protocol and URL are stored in `source_config.source` and validated on creation.
/// Playback output references SyncTV live delivery resources. Internal
/// room/media identity is injected at playback time through `ProviderContext`.
pub struct LiveProxyProvider {
    ssrf_guard: synctv_common::ssrf::SsrfGuard,
}

impl Default for LiveProxyProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveProxyProvider {
    pub const NAME: &'static str = "live_proxy";

    pub fn new() -> Self {
        Self::new_with_ssrf_guard(synctv_common::ssrf::SsrfGuard::strict_policy())
    }

    #[must_use]
    pub const fn new_with_ssrf_guard(ssrf_guard: synctv_common::ssrf::SsrfGuard) -> Self {
        Self { ssrf_guard }
    }

    async fn validate_live_source_url(
        url: &str,
        guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Result<(), ProviderError> {
        // Validate the external live-source URL format.
        // Use URL path parsing to avoid false positives from `.flv` appearing
        // outside the upstream URL path.
        let parsed_url = url::Url::parse(url).map_err(|error| {
            ProviderError::InvalidConfig(format!("Invalid LiveProxy source URL '{url}': {error}"))
        })?;
        let is_rtmp = parsed_url.scheme().eq_ignore_ascii_case("rtmp");
        let is_rtsp = parsed_url.scheme().eq_ignore_ascii_case("rtsp");
        let is_flv =
            matches!(parsed_url.scheme(), "http" | "https") && parsed_url.path().ends_with(".flv");
        if !is_rtmp && !is_rtsp && !is_flv {
            return Err(ProviderError::InvalidConfig(format!(
                "Unsupported source URL format: {url}. Expected rtmp://, rtsp://, or *.flv"
            )));
        }
        Self::reject_synctv_publish_url(&parsed_url)?;

        let host = parsed_url.host_str().ok_or_else(|| {
            ProviderError::InvalidConfig("LiveProxy source URL is missing a host".to_string())
        })?;
        let default_port = if is_rtmp {
            1935
        } else if is_rtsp {
            554
        } else if parsed_url.scheme() == "https" {
            443
        } else {
            80
        };
        let port = parsed_url.port().unwrap_or(default_port);

        guard
            .validate_url_target_with_default_port(host, port, default_port)
            .map_err(|error| match error {
                SsrfTargetError::BlockedHost(host) => ProviderError::InvalidConfig(format!(
                    "LiveProxy source host '{host}' is blocked by SSRF policy"
                )),
                SsrfTargetError::BlockedIp(ip) => ProviderError::InvalidConfig(format!(
                    "LiveProxy source IP '{ip}' is blocked by SSRF policy"
                )),
                SsrfTargetError::BlockedPort { port } => ProviderError::InvalidConfig(format!(
                    "LiveProxy source port '{port}' is blocked by SSRF policy"
                )),
            })?;

        if host.parse::<std::net::IpAddr>().is_err()
            && (is_rtmp || is_rtsp)
            && guard.dns_resolver().is_some()
        {
            let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, port))
                .await
                .map_err(|error| {
                    ProviderError::InvalidConfig(format!(
                        "LiveProxy streaming source host '{host}' could not be resolved: {error}"
                    ))
                })?
                .collect();

            if addrs.is_empty() {
                return Err(ProviderError::InvalidConfig(format!(
                    "LiveProxy streaming source host '{host}' did not resolve to any addresses"
                )));
            }

            if let Some(blocked_addr) = addrs
                .iter()
                .find(|addr| guard.is_ip_blocked_for_host(host, &addr.ip()))
            {
                return Err(ProviderError::InvalidConfig(format!(
                    "LiveProxy streaming source host '{host}' resolved to blocked IP '{}'",
                    blocked_addr.ip()
                )));
            }
        }

        Ok(())
    }

    async fn validate_external_source(
        source: &crate::models::ExternalLiveSourceConfig,
        guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Result<(), ProviderError> {
        if let crate::models::ExternalLiveSourceConfig::Rtsp {
            video_track: crate::models::RtspTrackSelection::Disabled,
            audio_track: crate::models::RtspTrackSelection::Disabled,
            ..
        } = source
        {
            return Err(ProviderError::InvalidConfig(
                "RTSP source must enable a video or audio track".to_string(),
            ));
        }
        let url = source.url();
        let parsed = url::Url::parse(url).map_err(|error| {
            ProviderError::InvalidConfig(format!("Invalid LiveProxy source URL '{url}': {error}"))
        })?;
        let protocol_matches = match source {
            crate::models::ExternalLiveSourceConfig::Rtmp { .. } => parsed.scheme() == "rtmp",
            crate::models::ExternalLiveSourceConfig::Rtsp { .. } => parsed.scheme() == "rtsp",
            crate::models::ExternalLiveSourceConfig::HttpFlv { .. } => {
                matches!(parsed.scheme(), "http" | "https") && parsed.path().ends_with(".flv")
            }
        };
        if !protocol_matches {
            return Err(ProviderError::InvalidConfig(
                "External live source protocol and URL disagree".to_string(),
            ));
        }
        Self::validate_live_source_url(url, guard).await
    }

    fn reject_synctv_publish_url(parsed_url: &url::Url) -> Result<(), ProviderError> {
        if !parsed_url.scheme().eq_ignore_ascii_case("rtmp") {
            return Ok(());
        }

        let Some(token) = parsed_url
            .query_pairs()
            .find_map(|(key, value)| (key == "token").then_some(value.into_owned()))
        else {
            return Ok(());
        };

        if !Self::looks_like_synctv_publish_key(&token) {
            return Ok(());
        }

        Err(ProviderError::InvalidConfig(
            "LiveProxy source URL points at a SyncTV RTMP publish endpoint. Use the original upstream RTMP/HTTP-FLV source URL, or use the RTMP provider for SyncTV-managed live media.".to_string(),
        ))
    }

    fn looks_like_synctv_publish_key(token: &str) -> bool {
        let mut parts = token.split('.');
        let (Some(_header), Some(payload), Some(_signature), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return false;
        };

        let Ok(payload) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload) else {
            return false;
        };
        let Ok(payload) = serde_json::from_slice::<Value>(&payload) else {
            return false;
        };

        payload
            .get("perm_manage_live_streams")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && payload.get("room_id").is_some()
            && payload.get("media_id").is_some()
    }

    fn resolve_live_binding<'a>(
        ctx: &'a ProviderContext<'a>,
    ) -> Result<(&'a RoomId, &'a MediaId), ProviderError> {
        let room_id = ctx.room_id().ok_or_else(|| {
            ProviderError::InvalidConfig(
                "Missing room_id in provider context for live proxy playback".to_string(),
            )
        })?;

        let media_id = ctx.media_id().ok_or_else(|| {
            ProviderError::InvalidConfig(
                "Missing media_id in provider context for live proxy playback".to_string(),
            )
        })?;

        Ok((room_id, media_id))
    }
}

fn mark_live_proxy_playback_resources(result: &mut PlaybackResult, version: &str, expires_at: i64) {
    // Live proxy playback is owned by SyncTV, so generated modes point at
    // provider-playback transport actions directly.
    let default_mode = result.default_mode.clone();
    for (mode_name, info) in &mut result.playback_infos {
        let is_hls = super::playback_info_is_hls(mode_name, info);
        let media_count = info.medias.len();
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
                PlaybackMediaProvider::LiveProxy(PlaybackLiveProxyMedia::HlsMaster {
                    version: version.to_string(),
                    expires_at,
                    room_id,
                    media_id,
                })
            } else if mode_name == "flv" || (mode_name == &default_mode && media_count == 1) {
                PlaybackMediaProvider::LiveProxy(PlaybackLiveProxyMedia::FlvStream {
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

#[async_trait]
impl MediaProvider for LiveProxyProvider {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    async fn generate_playback(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &crate::models::MediaSourceConfig,
    ) -> Result<PlaybackResult, ProviderError> {
        let crate::models::MediaSourceConfig::LiveProxy(source_config) = source_config else {
            return Err(ProviderError::InvalidConfig(
                "LiveProxy requires LiveProxy media source_config".to_string(),
            ));
        };
        let (room_id, media_id) = Self::resolve_live_binding(ctx)?;

        let source_url = source_config.source.url().to_string();
        Self::validate_external_source(&source_config.source, &self.ssrf_guard).await?;
        let mut result = super::build_live_playback(*media_id, *room_id);
        let parsed_source_url = url::Url::parse(&source_url).map_err(|error| {
            ProviderError::InvalidConfig(format!(
                "Invalid LiveProxy source URL '{source_url}': {error}"
            ))
        })?;
        let redacted_host = parsed_source_url
            .host_str()
            .ok_or_else(|| {
                ProviderError::InvalidConfig("LiveProxy source URL is missing a host".to_string())
            })?
            .to_string();
        result.metadata = Some(PlaybackMetadata::LiveProxy(LiveProxyPlaybackMetadata {
            media_id: *media_id,
            room_id: *room_id,
            source_host: Some(redacted_host),
        }));

        let cache_key = format!("playback:{room_id}:{media_id}");
        let cache_ttl = Duration::from_mins(5);
        super::cache_versioned_playback_and_build_response(
            result,
            Self::NAME,
            &cache_key,
            cache_ttl,
            ctx,
            mark_live_proxy_playback_resources,
        )
        .await
    }

    async fn validate_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<(), ProviderError> {
        let SourceConfig::Media(crate::models::MediaSourceConfig::LiveProxy(source_config)) =
            source_config
        else {
            return Err(ProviderError::InvalidConfig(
                "LiveProxy requires LiveProxy media source_config".to_string(),
            ));
        };
        Self::validate_external_source(&source_config.source, &self.ssrf_guard).await
    }
}

impl LiveProxyProvider {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn validate_live_source_url_allows_custom_port_for_allowed_host() {
        let guard = synctv_common::ssrf::SsrfGuard::builder()
            .extra_allowed_host("media.internal".to_string())
            .build();

        LiveProxyProvider::validate_live_source_url("http://media.internal:18000/live.flv", &guard)
            .await
            .expect("allowed host custom port should pass LiveProxy validation");
    }

    #[tokio::test]
    async fn validate_live_source_url_blocks_custom_port_for_regular_host() {
        let guard = synctv_common::ssrf::SsrfGuard::strict_policy();

        let error = LiveProxyProvider::validate_live_source_url(
            "http://public.example:18000/live.flv",
            &guard,
        )
        .await
        .expect_err("regular host custom port should fail LiveProxy validation");

        assert!(
            matches!(error, ProviderError::InvalidConfig(ref message) if message.contains("port '18000'")),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn validates_explicit_rtsp_source_protocol() {
        let guard = synctv_common::ssrf::SsrfGuard::disabled();
        let source = crate::models::ExternalLiveSourceConfig::Rtsp {
            url: "rtsp://camera.example/live".to_string(),
            transport: crate::models::RtspTransport::Udp,
            video_track: crate::models::RtspTrackSelection::Index(2),
            audio_track: crate::models::RtspTrackSelection::Disabled,
        };
        LiveProxyProvider::validate_external_source(&source, &guard)
            .await
            .expect("explicit RTSP source should validate");

        let mismatched = crate::models::ExternalLiveSourceConfig::Rtsp {
            url: "rtmp://camera.example/live".to_string(),
            transport: crate::models::RtspTransport::Tcp,
            video_track: crate::models::RtspTrackSelection::FirstCompatible,
            audio_track: crate::models::RtspTrackSelection::FirstCompatible,
        };
        let error = LiveProxyProvider::validate_external_source(&mismatched, &guard)
            .await
            .expect_err("protocol mismatch should fail validation");
        assert!(error.to_string().contains("protocol and URL disagree"));
    }

    #[tokio::test]
    async fn rejects_rtsp_source_with_both_tracks_disabled() {
        let guard = synctv_common::ssrf::SsrfGuard::disabled();
        let source = crate::models::ExternalLiveSourceConfig::Rtsp {
            url: "rtsp://camera.example/live".to_string(),
            transport: crate::models::RtspTransport::Tcp,
            video_track: crate::models::RtspTrackSelection::Disabled,
            audio_track: crate::models::RtspTrackSelection::Disabled,
        };
        let error = LiveProxyProvider::validate_external_source(&source, &guard)
            .await
            .expect_err("RTSP source with no enabled media must fail validation");
        assert!(error
            .to_string()
            .contains("must enable a video or audio track"));
    }
}
