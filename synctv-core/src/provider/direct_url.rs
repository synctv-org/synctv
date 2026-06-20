//! Direct URL `MediaProvider`
//!
//! Provides direct playback for HTTP(S) URLs

use super::{
    playback_transport::PlaybackTransportAction,
    store::{ProviderStoreExt, VersionedPlayback},
    MediaProvider, PlaybackInfo, PlaybackResult, ProviderContext, ProviderError, SourceConfig,
};
use crate::models::media::{
    PlaybackDirectUrlMedia, PlaybackDirectUrlSubtitle, PlaybackMedia, PlaybackMediaProvider,
    PlaybackSubtitle, PlaybackSubtitleProvider,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;
use synctv_common::ssrf::SsrfTargetError;
use url::Url;

/// Direct URL `MediaProvider`
pub struct DirectUrlProvider {
    ssrf_guard: synctv_common::ssrf::SsrfGuard,
}

impl DirectUrlProvider {
    /// Provider type name constant.
    pub const NAME: &'static str = "direct_url";

    #[must_use]
    pub fn new() -> Self {
        Self::new_with_ssrf_guard(synctv_common::ssrf::SsrfGuard::strict_policy())
    }

    #[must_use]
    pub const fn new_with_ssrf_guard(ssrf_guard: synctv_common::ssrf::SsrfGuard) -> Self {
        Self { ssrf_guard }
    }

    /// Forbidden header names that must not be set via user-supplied config.
    ///
    /// These headers can be exploited for request smuggling or SSRF
    /// amplification if user-controlled.
    const FORBIDDEN_HEADERS: &[&str] = &[
        "host",
        "transfer-encoding",
        "content-length",
        "connection",
        "upgrade",
        "proxy-authorization",
        "proxy-connection",
        "te",
        "trailer",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
        "x-real-ip",
        "x-original-url",
        "x-rewrite-url",
    ];

    /// Validate that custom headers do not include forbidden header names.
    fn validate_headers(headers: &HashMap<String, String>) -> Result<(), ProviderError> {
        for key in headers.keys() {
            let lower = key.to_lowercase();

            // Check exact forbidden headers list
            if Self::FORBIDDEN_HEADERS.contains(&lower.as_str()) {
                return Err(ProviderError::InvalidConfig(format!(
                    "DirectUrl header '{key}' is forbidden for security reasons"
                )));
            }

            // Block all Sec-* prefix headers (HTTP/3 security headers, Client Hints, WebSocket headers)
            if lower.starts_with("sec-") {
                return Err(ProviderError::InvalidConfig(format!(
                    "DirectUrl header '{key}' is forbidden (Sec- prefix blocked for security)"
                )));
            }

            // Block Priority header (HTTP/3 prioritization)
            if lower == "priority" {
                return Err(ProviderError::InvalidConfig(format!(
                    "DirectUrl header '{key}' is forbidden for security reasons"
                )));
            }
        }
        Ok(())
    }

    /// Detect format from URL path extension.
    ///
    /// Parses the URL to extract the path component, then checks the file
    /// extension. This avoids false positives from `contains()` matching
    /// against query parameters or hostnames (e.g., "cdn.flv.com/video").
    fn detect_format(url: &str) -> String {
        let path = Url::parse(url).map_or_else(|_| url.to_string(), |u| u.path().to_string());

        let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
        match ext.as_str() {
            "m3u8" => "m3u8",
            "flv" => "flv",
            "mp4" | "m4v" | "mov" => "mp4",
            "mkv" => "mkv",
            "webm" => "webm",
            "avi" => "avi",
            _ => "video",
        }
        .to_string()
    }

    fn validate_source_url(
        url: &str,
        guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Result<Url, ProviderError> {
        let parsed = Url::parse(url).map_err(|error| {
            ProviderError::InvalidConfig(format!("DirectUrl URL is invalid: {error}"))
        })?;

        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return Err(ProviderError::InvalidConfig(
                "DirectUrl only supports http:// and https:// schemes".to_string(),
            ));
        }

        let host = parsed.host_str().ok_or_else(|| {
            ProviderError::InvalidConfig("DirectUrl URL must include a host".to_string())
        })?;

        let port = parsed.port_or_known_default().ok_or_else(|| {
            ProviderError::InvalidConfig("DirectUrl URL port could not be determined".to_string())
        })?;
        guard
            .validate_url_target(host, port)
            .map_err(|error| match error {
                SsrfTargetError::BlockedHost(host) => ProviderError::InvalidConfig(format!(
                    "DirectUrl host '{host}' is blocked by SSRF policy"
                )),
                SsrfTargetError::BlockedIp(ip) => ProviderError::InvalidConfig(format!(
                    "DirectUrl IP '{ip}' is blocked by SSRF policy"
                )),
                SsrfTargetError::BlockedPort { port } => {
                    ProviderError::InvalidConfig(format!("DirectUrl port '{port}' is not allowed"))
                }
            })?;

        Ok(parsed)
    }

    fn playback_cache_key(config: &DirectUrlSourceConfig) -> String {
        use sha2::{Digest, Sha256};

        let mut hasher = Sha256::new();
        hasher.update(config.url.as_bytes());
        hasher.update(b"\0");
        let mut header_entries: Vec<_> = config.headers.iter().collect();
        header_entries.sort_unstable_by(|(left_name, left_value), (right_name, right_value)| {
            left_name
                .cmp(right_name)
                .then_with(|| left_value.cmp(right_value))
        });

        for (name, value) in header_entries {
            hasher.update(name.as_bytes());
            hasher.update(b"=");
            hasher.update(value.as_bytes());
            hasher.update(b"\0");
        }

        let cache_key_suffix: String = hex::encode(hasher.finalize()).chars().take(16).collect();
        format!("playback:{cache_key_suffix}")
    }
}

impl Default for DirectUrlProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// `DirectUrl` source configuration
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DirectUrlSourceConfig {
    url: String,
    #[serde(default)]
    headers: HashMap<String, String>,
}

impl TryFrom<&Value> for DirectUrlSourceConfig {
    type Error = ProviderError;

    fn try_from(value: &Value) -> Result<Self, Self::Error> {
        super::parse_source_config(value, "DirectUrl")
    }
}

fn playback_media(
    name: String,
    format: String,
    expires_at: Option<i64>,
    provider: PlaybackMediaProvider,
) -> PlaybackMedia {
    PlaybackMedia {
        name,
        format,
        expire_at: expires_at.and_then(|timestamp| chrono::DateTime::from_timestamp(timestamp, 0)),
        metadata: None,
        provider,
    }
}

fn mark_direct_url_playback_resources(result: &mut PlaybackResult, version: &str, expires_at: i64) {
    // Direct URL usually keeps the upstream mode as default. When headers are
    // required, the proxy sibling becomes default because the server must own
    // those transport headers for browser and app clients alike.
    let prefer_proxy_default = super::signed_playback_default_needs_proxy(result);
    let original_default_mode = result.default_mode.clone();
    let mut selected_default_mode = original_default_mode.clone();
    let original_modes = result
        .playback_infos
        .iter()
        .map(|(mode_name, info)| (mode_name.clone(), info.clone()))
        .collect::<Vec<_>>();

    for (mode_name, original_info) in original_modes {
        if original_info.medias.is_empty() || mode_name.starts_with("proxy_") {
            continue;
        }

        let original_has_transport_headers =
            super::playback_info_has_transport_headers(&original_info);

        let proxy_mode_name = format!("proxy_{mode_name}");
        if prefer_proxy_default && mode_name == original_default_mode {
            selected_default_mode.clone_from(&proxy_mode_name);
        }
        if result.playback_infos.contains_key(&proxy_mode_name) {
            if original_has_transport_headers {
                result.playback_infos.remove(&mode_name);
            }
            continue;
        }

        let mut proxy_info = original_info.clone();
        let proxy_is_hls = super::playback_info_is_hls(&mode_name, &original_info);
        proxy_info.medias = original_info
            .medias
            .iter()
            .enumerate()
            .filter_map(|(url_index, media)| {
                let url = media.upstream_url()?.to_string();
                let headers = media.upstream_headers();
                Some(playback_media(
                    media.name.clone(),
                    media.format.clone(),
                    media.expire_at.map(|dt| dt.timestamp()),
                    PlaybackMediaProvider::DirectUrl(if proxy_is_hls {
                        PlaybackDirectUrlMedia::ProxyHlsManifest {
                            version: version.to_string(),
                            expires_at,
                            mode_name: mode_name.clone(),
                            url_index,
                            url,
                            headers,
                        }
                    } else {
                        PlaybackDirectUrlMedia::ProxyStream {
                            version: version.to_string(),
                            expires_at,
                            mode_name: mode_name.clone(),
                            url_index,
                            url,
                            headers,
                        }
                    }),
                ))
            })
            .collect();
        proxy_info.subtitles = original_info
            .subtitles
            .iter()
            .enumerate()
            .map(|(subtitle_index, subtitle)| PlaybackSubtitle {
                name: subtitle.name().to_string(),
                language: subtitle.language().to_string(),
                format: subtitle.format().to_string(),
                provider: PlaybackSubtitleProvider::DirectUrl(PlaybackDirectUrlSubtitle {
                    version: version.to_string(),
                    expires_at,
                    mode_name: mode_name.clone(),
                    subtitle_index,
                    url: subtitle.upstream_url().to_string(),
                    headers: subtitle.upstream_headers(),
                }),
            })
            .collect();

        result.playback_infos.insert(proxy_mode_name, proxy_info);
        if original_has_transport_headers {
            result.playback_infos.remove(&mode_name);
        }
    }

    result.default_mode = selected_default_mode;
}

impl DirectUrlProvider {
    pub async fn get_stream(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        mode_name: &str,
        url_index: usize,
        request_context: Option<&super::ExecutionControl>,
        range_header: Option<&str>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let playback_info = versioned
            .result
            .playback_infos
            .get(mode_name)
            .ok_or(ProviderError::NotFound)?;
        let media = playback_info
            .medias
            .get(url_index)
            .ok_or(ProviderError::NotFound)?;
        let url = media.upstream_url().ok_or(ProviderError::NotFound)?;
        Ok(PlaybackTransportAction::FetchAndForward {
            url: url.to_string(),
            headers: media.upstream_headers(),
            range_header: range_header.map(ToString::to_string),
        })
    }

    pub async fn get_hls_manifest(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        mode_name: &str,
        url_index: usize,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let playback_info = versioned
            .result
            .playback_infos
            .get(mode_name)
            .ok_or(ProviderError::NotFound)?;
        let media = playback_info
            .medias
            .get(url_index)
            .ok_or(ProviderError::NotFound)?;
        let url = media.upstream_url().ok_or(ProviderError::NotFound)?;
        Ok(PlaybackTransportAction::M3u8Rewrite {
            url: url.to_string(),
            headers: media.upstream_headers(),
        })
    }

    pub async fn get_hls_segment(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        target_url: String,
        request_context: Option<&super::ExecutionControl>,
        range_header: Option<&str>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let headers = versioned
            .result
            .playback_infos
            .get(&versioned.result.default_mode)
            .and_then(|info| info.medias.first())
            .map_or_else(HashMap::new, PlaybackMedia::upstream_headers);
        super::playback_transport::transport_action_for_target_url(
            target_url,
            headers,
            range_header,
        )
    }

    pub async fn get_subtitle(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        mode_name: &str,
        subtitle_index: usize,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let playback_info = versioned
            .result
            .playback_infos
            .get(mode_name)
            .ok_or(ProviderError::NotFound)?;
        let subtitle = playback_info
            .subtitles
            .get(subtitle_index)
            .ok_or(ProviderError::NotFound)?;
        Ok(PlaybackTransportAction::FetchAndForward {
            url: subtitle.upstream_url().to_string(),
            headers: super::subtitle_headers_for_proxy(
                &playback_info
                    .medias
                    .first()
                    .map_or_else(HashMap::new, PlaybackMedia::upstream_headers),
                subtitle,
            ),
            range_header: None,
        })
    }
}

#[async_trait]
impl MediaProvider for DirectUrlProvider {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    async fn validate_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<(), ProviderError> {
        super::reject_source_config_provider_instance_name(source_config.value(), "DirectUrl")?;

        let config = DirectUrlSourceConfig::try_from(source_config.value())?;

        Self::validate_source_url(&config.url, &self.ssrf_guard)?;

        // Validate custom headers: reject forbidden header names that could be
        // used for request smuggling or credential injection.
        Self::validate_headers(&config.headers)?;

        Ok(())
    }

    async fn generate_playback(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<PlaybackResult, ProviderError> {
        super::reject_source_config_provider_instance_name(source_config, "DirectUrl")?;

        let config = DirectUrlSourceConfig::try_from(source_config)?;
        Self::validate_source_url(&config.url, &self.ssrf_guard)?;

        let cache_key = Self::playback_cache_key(&config);
        let cache_ttl = Duration::from_hours(1); // 1 hour for direct URLs

        let store = _ctx.store.as_ref();

        // Check cache
        if let Some(store) = store {
            if let Ok(Some(cached)) = store.get::<VersionedPlayback>(&cache_key).await {
                if !cached.is_expired() {
                    return super::build_cached_versioned_playback_response(
                        cached,
                        Self::NAME,
                        _ctx,
                        mark_direct_url_playback_resources,
                    )
                    .await;
                }
            }
        }

        let format = Self::detect_format(&config.url);

        let mut playback_infos = HashMap::new();
        playback_infos.insert(
            "direct".to_string(),
            PlaybackInfo {
                medias: vec![playback_media(
                    "Direct".to_string(),
                    format.clone(),
                    None,
                    PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::Direct {
                        url: config.url.clone(),
                        headers: config.headers,
                    }),
                )],
                default_media_index: None,
                subtitles: Vec::new(),
                default_subtitle_index: None,
                danmakus: Vec::new(),
                default_danmaku_index: None,
            },
        );

        let mut metadata = HashMap::new();
        metadata.insert("format".to_string(), json!(format));
        metadata.insert("is_live".to_string(), json!(false));

        if let Some(filename) = config.url.split('/').next_back() {
            metadata.insert("filename".to_string(), json!(filename));
        }

        let result = PlaybackResult {
            playback_infos,
            default_mode: "direct".to_string(),
            provider: Self::NAME.to_string(),
            provider_instance_name: _ctx.provider_instance_name().map(str::to_string),
            duration_seconds: None,
            metadata,
        };

        super::cache_versioned_playback_and_build_response(
            result,
            Self::NAME,
            &cache_key,
            cache_ttl,
            _ctx,
            mark_direct_url_playback_resources,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_source_url_allows_custom_port_for_allowed_host() {
        let guard = synctv_common::ssrf::SsrfGuard::builder()
            .extra_allowed_host("media.internal".to_string())
            .build();

        DirectUrlProvider::validate_source_url("http://media.internal:18000/video.mp4", &guard)
            .expect("allowed host custom port should pass DirectUrl validation");
    }

    #[test]
    fn validate_source_url_blocks_custom_port_for_regular_host() {
        let guard = synctv_common::ssrf::SsrfGuard::strict_policy();

        let error =
            DirectUrlProvider::validate_source_url("http://public.example:18000/video.mp4", &guard)
                .expect_err("regular host custom port should fail DirectUrl validation");

        assert!(
            matches!(error, ProviderError::InvalidConfig(ref message) if message.contains("port '18000'")),
            "unexpected error: {error}"
        );
    }
}
