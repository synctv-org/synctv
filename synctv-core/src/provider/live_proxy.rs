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
use crate::models::media::{PlaybackLiveProxyMedia, PlaybackMediaProvider, PlaybackRtmpMedia};
use crate::models::{MediaId, RoomId};
use crate::PublicIdCodec;
use async_trait::async_trait;
use base64::Engine as _;
use serde_json::{json, Value};
use std::time::Duration;

/// `LiveProxy` `MediaProvider`
///
/// Generates playback media resources for live streams from external sources.
/// The external URL is stored in `source_config.url` and validated on creation.
/// Playback URLs point to synctv's own HLS/FLV endpoints. Internal room/media
/// identity is injected at playback time through `ProviderContext`.
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
        // Validate URL format (only RTMP and HTTP-FLV are supported for pulling).
        // Use URL path parsing to avoid false positives from `.flv` appearing
        // in query parameters or other URL parts.
        let parsed_url = url::Url::parse(url).map_err(|error| {
            ProviderError::InvalidConfig(format!("Invalid LiveProxy source URL '{url}': {error}"))
        })?;
        let is_rtmp = parsed_url.scheme().eq_ignore_ascii_case("rtmp");
        let is_flv =
            matches!(parsed_url.scheme(), "http" | "https") && parsed_url.path().ends_with(".flv");
        if !is_rtmp && !is_flv {
            return Err(ProviderError::InvalidConfig(format!(
                "Unsupported source URL format: {url}. Expected rtmp:// or *.flv"
            )));
        }
        Self::reject_synctv_publish_url(&parsed_url)?;

        let host = parsed_url.host_str().ok_or_else(|| {
            ProviderError::InvalidConfig("LiveProxy source URL is missing a host".to_string())
        })?;
        if guard.is_host_blocked(host) {
            return Err(ProviderError::InvalidConfig(format!(
                "LiveProxy source host '{host}' is blocked by SSRF policy"
            )));
        }

        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            if guard.is_ip_blocked(&ip) {
                return Err(ProviderError::InvalidConfig(format!(
                    "LiveProxy source IP '{ip}' is blocked by SSRF policy"
                )));
            }
        } else if is_rtmp && guard.dns_resolver().is_some() {
            let port = parsed_url.port().unwrap_or(1935);
            let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, port))
                .await
                .map_err(|error| {
                    ProviderError::InvalidConfig(format!(
                        "LiveProxy RTMP source host '{host}' could not be resolved: {error}"
                    ))
                })?
                .collect();

            if addrs.is_empty() {
                return Err(ProviderError::InvalidConfig(format!(
                    "LiveProxy RTMP source host '{host}' did not resolve to any addresses"
                )));
            }

            if let Some(blocked_addr) = addrs.iter().find(|addr| guard.is_ip_blocked(&addr.ip())) {
                return Err(ProviderError::InvalidConfig(format!(
                    "LiveProxy RTMP source host '{host}' resolved to blocked IP '{}'",
                    blocked_addr.ip()
                )));
            }
        }

        Ok(())
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
            .get("perm_live_control")
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

    fn validate_config_shape(source_config: &Value) -> Result<(), ProviderError> {
        super::reject_source_config_provider_instance_name(source_config, "LiveProxy")?;

        for field in [
            "room_id",
            "media_id",
            "rtmp_url",
            "source_url",
            "stream_url",
        ] {
            if source_config.get(field).is_some() {
                return Err(ProviderError::InvalidConfig(format!(
                    "Field '{field}' is not supported. Live proxy source_config only accepts 'url'; internal room/media identity comes from runtime context."
                )));
            }
        }

        Ok(())
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
                PlaybackMediaProvider::LiveProxy(PlaybackLiveProxyMedia::HlsPlaylist {
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
        source_config: &Value,
    ) -> Result<PlaybackResult, ProviderError> {
        Self::validate_config_shape(source_config)?;
        let (room_id, media_id) = Self::resolve_live_binding(ctx)?;

        let source_url = source_config
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProviderError::InvalidConfig("Missing url".to_string()))?;
        Self::validate_live_source_url(source_url, &self.ssrf_guard).await?;

        let mut result = super::build_live_playback(*media_id, *room_id);
        let parsed_source_url = url::Url::parse(source_url).map_err(|error| {
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
        result
            .metadata
            .insert("source_host".to_string(), json!(redacted_host));
        result
            .metadata
            .insert("provider".to_string(), json!("live_proxy"));

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
        let source_config = source_config.value();
        Self::validate_config_shape(source_config)?;

        // Validate required fields
        let url = source_config
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProviderError::InvalidConfig("Missing url".to_string()))?;

        Self::validate_live_source_url(url, &self.ssrf_guard).await
    }
}

impl LiveProxyProvider {
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
            "LiveProxy",
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
            "LiveProxy",
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
            "LiveProxy",
        )
    }
}
