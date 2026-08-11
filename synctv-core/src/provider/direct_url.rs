//! Direct URL `MediaProvider`
//!
//! Provides direct playback for HTTP(S) URLs

use super::{
    playback_transport::PlaybackTransportAction,
    store::{ProviderStoreExt, VersionedPlayback},
    MediaProvider, PlaybackInfo, PlaybackResult, ProviderContext, ProviderError, SourceConfig,
};
use crate::models::media::{
    DirectUrlPlaybackMetadata, PlaybackDanmaku, PlaybackDanmakuProvider, PlaybackDirectUrlDanmaku,
    PlaybackDirectUrlMedia, PlaybackDirectUrlSubtitle, PlaybackMedia, PlaybackMediaProvider,
    PlaybackMetadata, PlaybackSubtitle, PlaybackSubtitleProvider,
};
use crate::models::{detect_direct_url_format, DirectUrlMediaSourceConfig};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::Duration;
use synctv_common::ssrf::SsrfTargetError;
use url::Url;

/// Direct URL `MediaProvider`
pub struct DirectUrlProvider {
    ssrf_guard: synctv_common::ssrf::SsrfGuard,
}

#[derive(Debug, Clone, Copy)]
pub struct DirectUrlHlsResourceRequest<'a> {
    pub version: &'a str,
    pub mode_name: &'a str,
    pub url_index: usize,
    pub target_url: &'a str,
    pub is_manifest: bool,
    pub range_header: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
pub struct DirectUrlDashResourceRequest<'a> {
    pub version: &'a str,
    pub mode_name: &'a str,
    pub url_index: usize,
    pub scope_url: &'a str,
    pub resource_path: &'a str,
    pub resource_query: Option<&'a str>,
    pub is_manifest: bool,
    pub range_header: Option<&'a str>,
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

            // Block all Sec-* prefix headers used by browser security and transport protocols.
            if lower.starts_with("sec-") {
                return Err(ProviderError::InvalidConfig(format!(
                    "DirectUrl header '{key}' is forbidden (Sec- prefix blocked for security)"
                )));
            }

            // Block Priority header to prevent transport-level request manipulation.
            if lower == "priority" {
                return Err(ProviderError::InvalidConfig(format!(
                    "DirectUrl header '{key}' is forbidden for security reasons"
                )));
            }
        }
        Ok(())
    }

    fn detect_format(url: &str) -> String {
        detect_direct_url_format(url).to_string()
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

    fn playback_cache_key(config: &DirectUrlMediaSourceConfig) -> String {
        use sha2::{Digest, Sha256};

        let mut hasher = Sha256::new();
        let bytes = serde_json::to_vec(config).unwrap_or_default();
        hasher.update(bytes);

        let cache_key_suffix: String = hex::encode(hasher.finalize()).chars().take(16).collect();
        format!("playback:{cache_key_suffix}")
    }

    fn configured_duration_seconds(config: &DirectUrlMediaSourceConfig) -> Option<f64> {
        config.positive_duration_seconds()
    }
}

impl Default for DirectUrlProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl DirectUrlProvider {
    pub fn validate_prepared_config(
        &self,
        config: &DirectUrlMediaSourceConfig,
    ) -> Result<(), ProviderError> {
        Self::validate_config_shape(config)?;
        for media in &config.medias {
            Self::validate_source_url(&media.url, &self.ssrf_guard)?;
            Self::validate_headers(&media.headers)?;
        }
        for subtitle in &config.subtitles {
            Self::validate_source_url(&subtitle.url, &self.ssrf_guard)?;
            Self::validate_headers(&subtitle.headers)?;
        }
        for danmaku in &config.danmakus {
            Self::validate_source_url(&danmaku.url, &self.ssrf_guard)?;
            Self::validate_headers(&danmaku.headers)?;
        }
        Ok(())
    }

    fn validate_config_shape(config: &DirectUrlMediaSourceConfig) -> Result<(), ProviderError> {
        if config.medias.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "DirectUrl source_config.medias must contain at least one media".to_string(),
            ));
        }

        let check_index = |index: Option<usize>, len: usize, field: &str| match index {
            Some(index) if index >= len => Err(ProviderError::InvalidConfig(format!(
                "DirectUrl {field} {index} is out of bounds"
            ))),
            _ => Ok(()),
        };
        check_index(
            config.default_media_index,
            config.medias.len(),
            "default_media_index",
        )?;
        check_index(
            config.default_subtitle_index,
            config.subtitles.len(),
            "default_subtitle_index",
        )?;
        check_index(
            config.default_danmaku_index,
            config.danmakus.len(),
            "default_danmaku_index",
        )?;
        if config.playback_kind == Some(crate::models::PlaybackKind::Live)
            && DirectUrlProvider::configured_duration_seconds(config).is_some()
        {
            return Err(ProviderError::InvalidConfig(
                "DirectUrl live source_config cannot set duration_seconds".to_string(),
            ));
        }
        if config
            .medias
            .iter()
            .filter_map(|media| media.expires_at)
            .any(|expires_at| {
                expires_at <= 0 || chrono::DateTime::from_timestamp(expires_at, 0).is_none()
            })
        {
            return Err(ProviderError::InvalidConfig(
                "DirectUrl media expires_at must be a representable Unix timestamp in seconds"
                    .to_string(),
            ));
        }
        if config
            .subtitles
            .iter()
            .filter_map(|subtitle| subtitle.expires_at)
            .chain(
                config
                    .danmakus
                    .iter()
                    .filter_map(|danmaku| danmaku.expires_at),
            )
            .any(|expires_at| {
                expires_at <= 0 || chrono::DateTime::from_timestamp(expires_at, 0).is_none()
            })
        {
            return Err(ProviderError::InvalidConfig(
                "DirectUrl auxiliary expires_at must be a representable Unix timestamp in seconds"
                    .to_string(),
            ));
        }
        Ok(())
    }
}

fn direct_url_resource_descriptor(
    url: &str,
    headers: &HashMap<String, String>,
    format: &str,
) -> String {
    let mut headers = headers.iter().collect::<Vec<_>>();
    headers.sort_unstable_by(|left, right| left.0.cmp(right.0).then(left.1.cmp(right.1)));
    let canonical = serde_json::to_vec(&(
        "direct_url_v1",
        url.split('#').next().unwrap_or_default(),
        format.to_ascii_lowercase(),
        headers,
    ))
    .unwrap_or_default();
    hex::encode(Sha256::digest(canonical))
}

fn playback_media(
    name: String,
    format: String,
    expires_at: Option<i64>,
    p2p_swarm_id: Option<String>,
    provider: PlaybackMediaProvider,
) -> PlaybackMedia {
    PlaybackMedia {
        name,
        format,
        expire_at: expires_at.and_then(|timestamp| chrono::DateTime::from_timestamp(timestamp, 0)),
        metadata: None,
        p2p_swarm_id,
        provider,
    }
}

fn direct_url_expiration(explicit: Option<i64>, url: &str) -> Option<i64> {
    explicit
        .into_iter()
        .chain(super::url_expiration_timestamp(url))
        .min()
}

fn remap_filtered_default_index<T>(
    resources: &[(usize, T)],
    default_index: Option<usize>,
) -> Option<usize> {
    default_index.and_then(|default_index| {
        resources
            .iter()
            .position(|(source_index, _)| *source_index == default_index)
    })
}

fn mark_direct_url_playback_resources(
    result: &mut PlaybackResult,
    version: &str,
    expires_at: i64,
    proxy_mode: crate::models::PlaybackProxyMode,
) {
    let original_modes = result
        .playback_infos
        .iter()
        .map(|(mode_name, info)| (mode_name.clone(), info.clone()))
        .collect::<Vec<_>>();

    for (mode_name, original_info) in original_modes {
        if original_info.medias.is_empty() || mode_name.starts_with("proxy_") {
            continue;
        }

        let proxy_mode_name = format!("proxy_{mode_name}");
        if result.playback_infos.contains_key(&proxy_mode_name) {
            continue;
        }

        let mut proxy_info = original_info.clone();
        let proxy_medias = original_info
            .medias
            .iter()
            .enumerate()
            .filter_map(|(url_index, media)| {
                let url = media.upstream_url()?.to_string();
                let headers = media.upstream_headers();
                Some((
                    url_index,
                    playback_media(
                        media.name.clone(),
                        media.format.clone(),
                        media.expire_at.map(|dt| dt.timestamp()),
                        media.p2p_swarm_id.clone(),
                        PlaybackMediaProvider::DirectUrl(
                            if super::playback_media_is_dash(&mode_name, media) {
                                PlaybackDirectUrlMedia::ProxyDashManifest {
                                    version: version.to_string(),
                                    expires_at,
                                    mode_name: mode_name.clone(),
                                    url_index,
                                    url,
                                    headers,
                                }
                            } else if super::playback_media_is_hls(&mode_name, media) {
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
                            },
                        ),
                    ),
                ))
            })
            .collect::<Vec<_>>();
        if proxy_medias.is_empty() {
            continue;
        }
        let original_default_index = original_info.default_media_index.unwrap_or(0);
        let proxy_default_index = proxy_medias
            .iter()
            .position(|(source_index, _)| *source_index == original_default_index);
        proxy_info.default_media_index = original_info.default_media_index.and(proxy_default_index);
        proxy_info.medias = proxy_medias.into_iter().map(|(_, media)| media).collect();
        proxy_info.subtitles = original_info
            .subtitles
            .iter()
            .enumerate()
            .map(|(subtitle_index, subtitle)| PlaybackSubtitle {
                name: subtitle.name().to_string(),
                language: subtitle.language().to_string(),
                format: subtitle.format().to_string(),
                p2p_swarm_id: subtitle.p2p_swarm_id.clone(),
                provider: PlaybackSubtitleProvider::DirectUrl(PlaybackDirectUrlSubtitle::Proxy {
                    version: version.to_string(),
                    expires_at: subtitle
                        .expiration_timestamp()
                        .into_iter()
                        .chain(std::iter::once(expires_at))
                        .min()
                        .unwrap_or(expires_at),
                    mode_name: mode_name.clone(),
                    subtitle_index,
                    url: subtitle.upstream_url().to_string(),
                    headers: subtitle.upstream_headers(),
                }),
            })
            .collect();

        result.playback_infos.insert(proxy_mode_name, proxy_info);
    }
    super::apply_provider_playback_policy(result, proxy_mode, false);
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

    pub async fn get_hls_resource(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        request: DirectUrlHlsResourceRequest<'_>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, request.version, request_context)
                .await?;
        let media = versioned
            .result
            .playback_infos
            .get(request.mode_name)
            .and_then(|info| info.medias.get(request.url_index))
            .ok_or(ProviderError::NotFound)?;
        if request.is_manifest {
            Ok(PlaybackTransportAction::M3u8Rewrite {
                url: request.target_url.to_string(),
                headers: media.upstream_headers(),
            })
        } else {
            Ok(PlaybackTransportAction::FetchAndForward {
                url: request.target_url.to_string(),
                headers: media.upstream_headers(),
                range_header: request.range_header.map(ToString::to_string),
            })
        }
    }

    pub async fn get_dash_manifest(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        mode_name: &str,
        url_index: usize,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let media = versioned
            .result
            .playback_infos
            .get(mode_name)
            .and_then(|info| info.medias.get(url_index))
            .ok_or(ProviderError::NotFound)?;
        let url = media.upstream_url().ok_or(ProviderError::NotFound)?;
        Ok(PlaybackTransportAction::MpdRewrite {
            url: url.to_string(),
            headers: media.upstream_headers(),
        })
    }

    pub async fn get_dash_resource(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        request: DirectUrlDashResourceRequest<'_>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, request.version, request_context)
                .await?;
        let media = versioned
            .result
            .playback_infos
            .get(request.mode_name)
            .and_then(|info| info.medias.get(request.url_index))
            .ok_or(ProviderError::NotFound)?;
        let target_url = resolve_dash_scope_target(
            request.scope_url,
            request.resource_path,
            request.resource_query,
        )?;
        if request.is_manifest {
            Ok(PlaybackTransportAction::MpdRewrite {
                url: target_url,
                headers: media.upstream_headers(),
            })
        } else {
            Ok(PlaybackTransportAction::FetchAndForward {
                url: target_url,
                headers: media.upstream_headers(),
                range_header: request.range_header.map(ToString::to_string),
            })
        }
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

fn resolve_dash_scope_target(
    scope_url: &str,
    resource_path: &str,
    resource_query: Option<&str>,
) -> Result<String, ProviderError> {
    super::playback_transport::resolve_dash_scope_target(scope_url, resource_path, resource_query)
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
        let SourceConfig::Media(crate::models::MediaSourceConfig::DirectUrl(config)) =
            source_config
        else {
            return Err(ProviderError::InvalidConfig(
                "DirectUrl requires DirectUrl media source_config".to_string(),
            ));
        };
        self.validate_prepared_config(config)
    }

    async fn generate_playback(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &crate::models::MediaSourceConfig,
    ) -> Result<PlaybackResult, ProviderError> {
        let crate::models::MediaSourceConfig::DirectUrl(config) = source_config else {
            return Err(ProviderError::InvalidConfig(
                "DirectUrl requires DirectUrl media source_config".to_string(),
            ));
        };
        Self::validate_config_shape(config)?;
        for media in &config.medias {
            Self::validate_source_url(&media.url, &self.ssrf_guard)?;
        }
        for subtitle in &config.subtitles {
            Self::validate_source_url(&subtitle.url, &self.ssrf_guard)?;
        }
        for danmaku in &config.danmakus {
            Self::validate_source_url(&danmaku.url, &self.ssrf_guard)?;
        }

        let now = crate::SystemClock.now().timestamp();
        let medias = config
            .medias
            .iter()
            .enumerate()
            .filter_map(|(index, media)| {
                let expires_at = direct_url_expiration(media.expires_at, &media.url);
                if expires_at.is_some_and(|expires_at| expires_at <= now) {
                    tracing::warn!(
                        media = %media.name,
                        expires_at,
                        "Skipping expired DirectUrl media"
                    );
                    return None;
                }
                Some((index, media))
            })
            .collect::<Vec<_>>();
        if medias.is_empty() {
            return Err(ProviderError::ApiError(
                "All DirectUrl media resources have expired".to_string(),
            ));
        }
        let has_expired_resources = medias.len() != config.medias.len()
            || config.subtitles.iter().any(|subtitle| {
                direct_url_expiration(subtitle.expires_at, &subtitle.url)
                    .is_some_and(|expires_at| expires_at <= now)
            })
            || config.danmakus.iter().any(|danmaku| {
                direct_url_expiration(danmaku.expires_at, &danmaku.url)
                    .is_some_and(|expires_at| expires_at <= now)
            });

        let cache_key = Self::playback_cache_key(config);
        let cache_ttl = Duration::from_hours(1); // 1 hour for direct URLs

        let store = _ctx.store.as_ref();

        // Check cache
        if !has_expired_resources {
            if let Some(store) = store {
                if let Ok(Some(cached)) = store.get::<VersionedPlayback>(&cache_key).await {
                    if super::versioned_playback_is_fresh(&cached) {
                        return super::build_cached_versioned_playback_response(
                            cached,
                            Self::NAME,
                            _ctx,
                            |result, version, expires_at| {
                                mark_direct_url_playback_resources(
                                    result,
                                    version,
                                    expires_at,
                                    config.proxy_mode,
                                );
                            },
                        )
                        .await;
                    }
                }
            }
        }

        let first_media = medias
            .first()
            .map(|(_, media)| *media)
            .ok_or_else(|| ProviderError::InvalidConfig("DirectUrl medias is empty".to_string()))?;
        let format = if first_media.format.is_empty() {
            Self::detect_format(&first_media.url)
        } else {
            first_media.format.clone()
        };
        let playback_kind = config.inferred_playback_kind();
        let media_p2p_enabled = playback_kind == Some(crate::models::PlaybackKind::Regular);

        let subtitles = config
            .subtitles
            .iter()
            .enumerate()
            .filter_map(|(index, subtitle)| {
                let expires_at = direct_url_expiration(subtitle.expires_at, &subtitle.url);
                if expires_at.is_some_and(|expires_at| expires_at <= now) {
                    tracing::warn!(
                        subtitle = %subtitle.name,
                        expires_at,
                        "Skipping expired DirectUrl subtitle"
                    );
                    return None;
                }
                let format = if subtitle.format.is_empty() {
                    Self::detect_format(&subtitle.url)
                } else {
                    subtitle.format.clone()
                };
                let resource_descriptor =
                    direct_url_resource_descriptor(&subtitle.url, &subtitle.headers, &format);
                Some((
                    index,
                    PlaybackSubtitle {
                        name: if subtitle.name.is_empty() {
                            format!("Subtitle {}", index + 1)
                        } else {
                            subtitle.name.clone()
                        },
                        language: if subtitle.language.is_empty() {
                            "und".to_string()
                        } else {
                            subtitle.language.clone()
                        },
                        format,
                        p2p_swarm_id: Some(super::provider_p2p_swarm_id(
                            Self::NAME,
                            _ctx.provider_instance_name(),
                            "subtitle",
                            &resource_descriptor,
                        )),
                        provider: PlaybackSubtitleProvider::DirectUrl(
                            PlaybackDirectUrlSubtitle::Direct {
                                url: subtitle.url.clone(),
                                headers: subtitle.headers.clone(),
                                expire_at: expires_at.and_then(|timestamp| {
                                    chrono::DateTime::from_timestamp(timestamp, 0)
                                }),
                            },
                        ),
                    },
                ))
            })
            .collect::<Vec<_>>();
        let default_subtitle_index =
            remap_filtered_default_index(&subtitles, config.default_subtitle_index);
        let subtitles = subtitles
            .into_iter()
            .map(|(_, subtitle)| subtitle)
            .collect();

        let danmakus = config
            .danmakus
            .iter()
            .enumerate()
            .filter_map(|(index, danmaku)| {
                let expires_at = direct_url_expiration(danmaku.expires_at, &danmaku.url);
                if expires_at.is_some_and(|expires_at| expires_at <= now) {
                    tracing::warn!(
                        danmaku = %danmaku.name,
                        expires_at,
                        "Skipping expired DirectUrl danmaku"
                    );
                    return None;
                }
                let format = danmaku.format.as_deref().unwrap_or_default();
                let resource_descriptor =
                    direct_url_resource_descriptor(&danmaku.url, &danmaku.headers, format);
                Some((
                    index,
                    PlaybackDanmaku {
                        name: if danmaku.name.is_empty() {
                            format!("Danmaku {}", index + 1)
                        } else {
                            danmaku.name.clone()
                        },
                        format: danmaku.format.clone(),
                        p2p_swarm_id: Some(super::provider_p2p_swarm_id(
                            Self::NAME,
                            _ctx.provider_instance_name(),
                            "danmaku",
                            &resource_descriptor,
                        )),
                        provider: PlaybackDanmakuProvider::DirectUrl(PlaybackDirectUrlDanmaku {
                            url: danmaku.url.clone(),
                            headers: danmaku.headers.clone(),
                            expire_at: expires_at.and_then(|timestamp| {
                                chrono::DateTime::from_timestamp(timestamp, 0)
                            }),
                        }),
                    },
                ))
            })
            .collect::<Vec<_>>();
        let default_danmaku_index =
            remap_filtered_default_index(&danmakus, config.default_danmaku_index);
        let danmakus = danmakus.into_iter().map(|(_, danmaku)| danmaku).collect();

        let mut playback_infos = HashMap::new();
        let default_media_index = remap_filtered_default_index(&medias, config.default_media_index);
        playback_infos.insert(
            "direct".to_string(),
            PlaybackInfo {
                thumbnail: None,
                medias: medias
                    .iter()
                    .map(|(index, media)| {
                        let format = if media.format.is_empty() {
                            Self::detect_format(&media.url)
                        } else {
                            media.format.clone()
                        };
                        let name = if media.name.is_empty() {
                            format!("Direct {}", index + 1)
                        } else {
                            media.name.clone()
                        };
                        let expires_at = media
                            .expires_at
                            .into_iter()
                            .chain(super::url_expiration_timestamp(&media.url))
                            .min();
                        let resource_descriptor =
                            direct_url_resource_descriptor(&media.url, &media.headers, &format);
                        playback_media(
                            name,
                            format,
                            expires_at,
                            media_p2p_enabled.then(|| {
                                super::provider_p2p_swarm_id(
                                    Self::NAME,
                                    _ctx.provider_instance_name(),
                                    "media",
                                    &resource_descriptor,
                                )
                            }),
                            PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::Direct {
                                url: media.url.clone(),
                                headers: media.headers.clone(),
                            }),
                        )
                    })
                    .collect(),
                default_media_index,
                subtitles,
                default_subtitle_index,
                danmakus,
                default_danmaku_index,
            },
        );

        let metadata = PlaybackMetadata::DirectUrl(DirectUrlPlaybackMetadata {
            format: Some(format.clone()),
            filename: first_media
                .url
                .split('/')
                .next_back()
                .map(ToString::to_string),
        });
        let duration_seconds = if playback_kind == Some(crate::models::PlaybackKind::Live) {
            None
        } else {
            Self::configured_duration_seconds(config)
        };

        let result = PlaybackResult {
            playback_infos,
            default_mode: "direct".to_string(),
            provider: Self::NAME.to_string(),
            provider_instance_name: _ctx.provider_instance_name().map(str::to_string),
            duration_seconds,
            playback_kind,
            metadata: Some(metadata),
        };

        super::cache_versioned_playback_and_build_response(
            result,
            Self::NAME,
            &cache_key,
            cache_ttl,
            _ctx,
            |result, version, expires_at| {
                mark_direct_url_playback_resources(result, version, expires_at, config.proxy_mode);
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn test_context() -> ProviderContext<'static> {
        ProviderContext::new("test", crate::provider::ProviderActor::System).with_store(Arc::new(
            super::super::store::InMemoryProviderStore::new(100),
        ))
    }

    #[tokio::test]
    async fn generate_playback_marks_plain_file_video_probeable() {
        let provider = DirectUrlProvider::new();
        let ctx = test_context();
        let source_config = crate::models::MediaSourceConfig::DirectUrl(
            crate::models::DirectUrlMediaSourceConfig::single(
                "https://example.com/video.mp4".to_string(),
                HashMap::new(),
            ),
        );

        let result = provider
            .generate_playback(&ctx, &source_config)
            .await
            .expect("direct url playback should generate");

        assert_eq!(
            result.playback_kind,
            Some(crate::models::PlaybackKind::Regular)
        );
        assert_eq!(result.duration_seconds, None);
    }

    #[tokio::test]
    async fn generate_playback_keeps_manifest_liveness_unknown() {
        let provider = DirectUrlProvider::new();
        let ctx = test_context();
        let source_config = crate::models::MediaSourceConfig::DirectUrl(
            crate::models::DirectUrlMediaSourceConfig::single(
                "https://example.com/live.m3u8".to_string(),
                HashMap::new(),
            ),
        );

        let result = provider
            .generate_playback(&ctx, &source_config)
            .await
            .expect("direct url playback should generate");

        assert_eq!(result.playback_kind, None);
        assert_eq!(result.duration_seconds, None);
    }

    #[tokio::test]
    async fn generate_playback_returns_only_proxy_when_requested() {
        let provider = DirectUrlProvider::new();
        let ctx = test_context();
        let mut config = crate::models::DirectUrlMediaSourceConfig::single(
            "https://example.com/video.mp4".to_string(),
            HashMap::new(),
        );
        config.proxy_mode = crate::models::PlaybackProxyMode::Only;
        let source_config = crate::models::MediaSourceConfig::DirectUrl(config);

        let result = provider
            .generate_playback(&ctx, &source_config)
            .await
            .expect("direct url playback should generate");

        assert_eq!(result.default_mode, "proxy_direct");
        assert!(!result.playback_infos.contains_key("direct"));
        assert!(result.playback_infos.contains_key("proxy_direct"));
        let proxy = result.playback_infos["proxy_direct"].medias[0]
            .p2p_swarm_id
            .as_deref()
            .expect("proxy resource should carry provider identity");
        assert!(proxy.starts_with("sm3_"));
    }

    #[tokio::test]
    async fn generate_playback_prefers_proxy_while_returning_both_modes() {
        let provider = DirectUrlProvider::new();
        let ctx = test_context();
        let mut config = crate::models::DirectUrlMediaSourceConfig::single(
            "https://example.com/video.mp4".to_string(),
            HashMap::from([("authorization".to_string(), "Bearer secret".to_string())]),
        );
        config.proxy_mode = crate::models::PlaybackProxyMode::Prefer;

        let result = provider
            .generate_playback(&ctx, &crate::models::MediaSourceConfig::DirectUrl(config))
            .await
            .expect("preferred proxy playback should generate");

        assert_eq!(result.default_mode, "proxy_direct");
        assert!(result.playback_infos.contains_key("direct"));
        assert!(result.playback_infos.contains_key("proxy_direct"));
        assert_eq!(
            result.playback_infos["direct"].medias[0].upstream_headers()["authorization"],
            "Bearer secret"
        );
    }

    #[tokio::test]
    async fn auxiliary_expiry_filters_stale_resources_and_bounds_proxy_signatures() {
        let provider = DirectUrlProvider::new();
        let ctx = test_context();
        let now = crate::SystemClock.now().timestamp();
        let future_expiry = now + 300;
        let mut config = crate::models::DirectUrlMediaSourceConfig::single(
            "https://example.com/video.mp4".to_string(),
            HashMap::new(),
        );
        config.proxy_mode = crate::models::PlaybackProxyMode::Prefer;
        config.subtitles = vec![
            crate::models::DirectUrlSubtitleSourceConfig {
                name: "expired".to_string(),
                language: "en".to_string(),
                url: "https://example.com/expired.vtt".to_string(),
                headers: HashMap::new(),
                format: "vtt".to_string(),
                expires_at: Some(now - 1),
            },
            crate::models::DirectUrlSubtitleSourceConfig {
                name: "active".to_string(),
                language: "en".to_string(),
                url: "https://example.com/active.vtt".to_string(),
                headers: HashMap::new(),
                format: "vtt".to_string(),
                expires_at: Some(future_expiry),
            },
        ];
        config.default_subtitle_index = Some(1);
        config.danmakus = vec![
            crate::models::DirectUrlDanmakuSourceConfig {
                name: "expired".to_string(),
                url: "https://example.com/expired.xml".to_string(),
                headers: HashMap::new(),
                format: Some("xml".to_string()),
                expires_at: Some(now - 1),
            },
            crate::models::DirectUrlDanmakuSourceConfig {
                name: "active".to_string(),
                url: "https://example.com/danmaku.xml".to_string(),
                headers: HashMap::new(),
                format: Some("xml".to_string()),
                expires_at: Some(future_expiry + 60),
            },
        ];
        config.default_danmaku_index = Some(1);

        let result = provider
            .generate_playback(&ctx, &crate::models::MediaSourceConfig::DirectUrl(config))
            .await
            .expect("direct URL playback should keep active auxiliary resources");

        let direct = &result.playback_infos["direct"];
        assert_eq!(direct.subtitles.len(), 1);
        assert_eq!(direct.subtitles[0].name, "active");
        assert_eq!(direct.default_subtitle_index, Some(0));
        assert_eq!(direct.danmakus.len(), 1);
        assert_eq!(direct.danmakus[0].name, "active");
        assert_eq!(direct.default_danmaku_index, Some(0));
        assert_eq!(
            direct.subtitles[0].expiration_timestamp(),
            Some(future_expiry)
        );
        let proxy = &result.playback_infos["proxy_direct"];
        assert_eq!(proxy.subtitles.len(), 1);
        assert_eq!(proxy.default_subtitle_index, Some(0));
        assert_eq!(proxy.danmakus.len(), 1);
        assert_eq!(proxy.default_danmaku_index, Some(0));
        assert_eq!(
            proxy.subtitles[0].expiration_timestamp(),
            Some(future_expiry)
        );
    }

    #[tokio::test]
    async fn media_expiry_keeps_valid_fallbacks_and_remaps_the_default() {
        let provider = DirectUrlProvider::new();
        let ctx = test_context();
        let now = crate::SystemClock.now().timestamp();
        let config = crate::models::DirectUrlMediaSourceConfig {
            playback_kind: Some(crate::models::PlaybackKind::Regular),
            duration_seconds: None,
            proxy_mode: crate::models::PlaybackProxyMode::Auto,
            medias: vec![
                crate::models::DirectUrlMediaResourceConfig {
                    name: "expired".to_string(),
                    url: "https://example.com/expired.m3u8".to_string(),
                    headers: HashMap::new(),
                    format: "hls".to_string(),
                    expires_at: Some(now - 1),
                },
                crate::models::DirectUrlMediaResourceConfig {
                    name: "active".to_string(),
                    url: "https://example.com/active.m3u8".to_string(),
                    headers: HashMap::new(),
                    format: "hls".to_string(),
                    expires_at: Some(now + 300),
                },
            ],
            default_media_index: Some(1),
            subtitles: Vec::new(),
            default_subtitle_index: None,
            danmakus: Vec::new(),
            default_danmaku_index: None,
        };

        let result = provider
            .generate_playback(&ctx, &crate::models::MediaSourceConfig::DirectUrl(config))
            .await
            .expect("DirectUrl should retain an active fallback media");
        let direct = &result.playback_infos["direct"];

        assert_eq!(direct.medias.len(), 1);
        assert_eq!(direct.medias[0].name, "active");
        assert_eq!(direct.default_media_index, Some(0));
    }

    #[tokio::test]
    async fn media_expiry_fails_when_every_media_has_expired() {
        let provider = DirectUrlProvider::new();
        let ctx = test_context();
        let mut config = crate::models::DirectUrlMediaSourceConfig::single(
            "https://example.com/expired.mp4".to_string(),
            HashMap::new(),
        );
        config.medias[0].expires_at = Some(crate::SystemClock.now().timestamp() - 1);

        let error = provider
            .generate_playback(&ctx, &crate::models::MediaSourceConfig::DirectUrl(config))
            .await
            .expect_err("DirectUrl should fail when every media has expired");

        assert!(matches!(
            error,
            ProviderError::ApiError(message)
                if message == "All DirectUrl media resources have expired"
        ));
    }

    #[test]
    fn filtered_default_indexes_clear_when_the_selected_resource_is_removed() {
        let resources = vec![(0, "kept"), (2, "also-kept")];

        assert_eq!(remap_filtered_default_index(&resources, Some(0)), Some(0));
        assert_eq!(remap_filtered_default_index(&resources, Some(1)), None);
        assert_eq!(remap_filtered_default_index(&resources, Some(2)), Some(1));
        assert_eq!(remap_filtered_default_index(&resources, None), None);
    }

    #[tokio::test]
    async fn generate_playback_proxy_only_returns_proxy_modes() {
        let provider = DirectUrlProvider::new();
        let ctx = test_context();
        let mut config = crate::models::DirectUrlMediaSourceConfig::single(
            "https://example.com/video.mp4".to_string(),
            HashMap::new(),
        );
        config.proxy_mode = crate::models::PlaybackProxyMode::Only;

        let result = provider
            .generate_playback(&ctx, &crate::models::MediaSourceConfig::DirectUrl(config))
            .await
            .expect("proxy-only playback should generate");

        assert_eq!(result.default_mode, "proxy_direct");
        assert!(!result.playback_infos.contains_key("direct"));
        assert!(result.playback_infos.contains_key("proxy_direct"));
    }

    #[tokio::test]
    async fn generate_playback_selects_proxy_transport_per_media() {
        let provider = DirectUrlProvider::new();
        let ctx = test_context();
        let media = |name: &str, format: &str| crate::models::DirectUrlMediaResourceConfig {
            name: name.to_string(),
            url: format!("https://example.com/{name}.{format}"),
            headers: HashMap::new(),
            format: format.to_string(),
            expires_at: None,
        };
        let source_config = crate::models::MediaSourceConfig::DirectUrl(
            crate::models::DirectUrlMediaSourceConfig {
                playback_kind: Some(crate::models::PlaybackKind::Regular),
                duration_seconds: Some(20.0),
                proxy_mode: crate::models::PlaybackProxyMode::Auto,
                medias: vec![
                    media("video", "mp4"),
                    media("playlist", "hls"),
                    media("manifest", "dash"),
                    media("archive", "flv"),
                ],
                default_media_index: Some(0),
                subtitles: Vec::new(),
                default_subtitle_index: None,
                danmakus: Vec::new(),
                default_danmaku_index: None,
            },
        );

        let result = provider
            .generate_playback(&ctx, &source_config)
            .await
            .expect("mixed direct URL playback should generate");
        let proxy = &result.playback_infos["proxy_direct"];
        let direct = &result.playback_infos["direct"];

        assert_eq!(result.default_mode, "direct");
        assert_eq!(direct.medias.len(), 4);
        assert_eq!(direct.medias[0].p2p_swarm_id, proxy.medias[0].p2p_swarm_id);

        assert_eq!(
            proxy
                .medias
                .iter()
                .map(|media| media.format.as_str())
                .collect::<Vec<_>>(),
            vec!["mp4", "hls", "dash", "flv"]
        );
        assert!(matches!(
            proxy.medias[0].provider,
            PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::ProxyStream { .. })
        ));
        assert!(matches!(
            proxy.medias[1].provider,
            PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::ProxyHlsManifest { .. })
        ));
        assert!(matches!(
            proxy.medias[2].provider,
            PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::ProxyDashManifest { .. })
        ));
        assert!(matches!(
            proxy.medias[3].provider,
            PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::ProxyStream { .. })
        ));
    }

    #[tokio::test]
    async fn generate_playback_keeps_direct_and_proxy_modes_with_transport_headers() {
        let provider = DirectUrlProvider::new();
        let ctx = test_context();
        let source_config = crate::models::MediaSourceConfig::DirectUrl(
            crate::models::DirectUrlMediaSourceConfig {
                playback_kind: Some(crate::models::PlaybackKind::Regular),
                duration_seconds: Some(20.0),
                proxy_mode: crate::models::PlaybackProxyMode::Auto,
                medias: vec![
                    crate::models::DirectUrlMediaResourceConfig {
                        name: "Protected MP4".to_string(),
                        url: "https://example.com/protected.mp4".to_string(),
                        headers: HashMap::from([(
                            "authorization".to_string(),
                            "Bearer secret".to_string(),
                        )]),
                        format: "mp4".to_string(),
                        expires_at: None,
                    },
                    crate::models::DirectUrlMediaResourceConfig {
                        name: "Public DASH".to_string(),
                        url: "https://example.com/public.mpd".to_string(),
                        headers: HashMap::new(),
                        format: "dash".to_string(),
                        expires_at: None,
                    },
                ],
                default_media_index: Some(1),
                subtitles: Vec::new(),
                default_subtitle_index: None,
                danmakus: Vec::new(),
                default_danmaku_index: None,
            },
        );

        let result = provider
            .generate_playback(&ctx, &source_config)
            .await
            .expect("mixed protected MP4 and public DASH should generate");

        assert_eq!(result.default_mode, "direct");
        let direct = &result.playback_infos["direct"];
        assert_eq!(direct.default_media_index, Some(1));
        assert_eq!(direct.medias.len(), 2);
        assert_eq!(direct.medias[0].format, "mp4");
        assert_eq!(
            direct.medias[0].upstream_headers()["authorization"],
            "Bearer secret"
        );
        assert_eq!(direct.medias[1].format, "dash");
        let proxy = &result.playback_infos["proxy_direct"];
        assert_eq!(proxy.medias.len(), 2);
        assert_eq!(proxy.medias[0].format, "mp4");
        assert_eq!(proxy.medias[1].format, "dash");
        assert!(matches!(
            proxy.medias[1].provider,
            PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::ProxyDashManifest { .. })
        ));
    }

    #[tokio::test]
    async fn generate_playback_proxies_dash_transport_headers() {
        let config = crate::models::DirectUrlMediaSourceConfig {
            playback_kind: Some(crate::models::PlaybackKind::Regular),
            duration_seconds: None,
            proxy_mode: crate::models::PlaybackProxyMode::Only,
            medias: vec![crate::models::DirectUrlMediaResourceConfig {
                name: "Protected DASH".to_string(),
                url: "https://example.com/protected.mpd".to_string(),
                headers: HashMap::from([(
                    "authorization".to_string(),
                    "Bearer secret".to_string(),
                )]),
                format: "dash".to_string(),
                expires_at: None,
            }],
            default_media_index: None,
            subtitles: Vec::new(),
            default_subtitle_index: None,
            danmakus: Vec::new(),
            default_danmaku_index: None,
        };
        let provider = DirectUrlProvider::new();
        let ctx = test_context();
        let result = provider
            .generate_playback(&ctx, &crate::models::MediaSourceConfig::DirectUrl(config))
            .await
            .expect("protected DASH should generate through the proxy");

        assert_eq!(result.default_mode, "proxy_direct");
        assert!(!result.playback_infos.contains_key("direct"));
        let proxy = &result.playback_infos["proxy_direct"];
        assert_eq!(proxy.medias.len(), 1);
        assert!(matches!(
            &proxy.medias[0].provider,
            PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::ProxyDashManifest {
                headers,
                ..
            }) if headers.get("authorization").map(String::as_str) == Some("Bearer secret")
        ));
    }

    #[tokio::test]
    async fn dash_proxy_actions_preserve_media_headers_range_and_query() {
        let provider = DirectUrlProvider::new();
        let ctx = test_context();
        let mut config = crate::models::DirectUrlMediaSourceConfig::single(
            "https://cdn.example.com/dash/manifest.mpd".to_string(),
            HashMap::from([("authorization".to_string(), "Bearer secret".to_string())]),
        );
        config.proxy_mode = crate::models::PlaybackProxyMode::Only;
        config.medias[0].format = "dash".to_string();
        let result = provider
            .generate_playback(&ctx, &crate::models::MediaSourceConfig::DirectUrl(config))
            .await
            .expect("protected DASH should generate");
        let PlaybackMediaProvider::DirectUrl(PlaybackDirectUrlMedia::ProxyDashManifest {
            version,
            ..
        }) = &result.playback_infos["proxy_direct"].medias[0].provider
        else {
            panic!("DASH media should use the DASH proxy transport");
        };

        let manifest = provider
            .get_dash_manifest(ctx.store.as_ref(), version, "direct", 0, None)
            .await
            .expect("DASH manifest action should resolve");
        assert!(matches!(
            manifest,
            PlaybackTransportAction::MpdRewrite { ref url, ref headers }
                if url == "https://cdn.example.com/dash/manifest.mpd"
                    && headers.get("authorization").map(String::as_str) == Some("Bearer secret")
        ));

        let segment = provider
            .get_dash_resource(
                ctx.store.as_ref(),
                DirectUrlDashResourceRequest {
                    version,
                    mode_name: "direct",
                    url_index: 0,
                    scope_url: "https://cdn.example.com/dash/video/",
                    resource_path: "representation/segment-12.m4s",
                    resource_query: Some("token=a%2Bb"),
                    is_manifest: false,
                    range_header: Some("bytes=100-199"),
                },
                None,
            )
            .await
            .expect("DASH segment action should resolve");
        assert!(matches!(
            segment,
            PlaybackTransportAction::FetchAndForward {
                ref url,
                ref headers,
                range_header: Some(ref range),
            } if url == "https://cdn.example.com/dash/video/representation/segment-12.m4s?token=a%2Bb"
                && headers.get("authorization").map(String::as_str) == Some("Bearer secret")
                && range == "bytes=100-199"
        ));
    }

    #[test]
    fn dash_scope_resolves_relative_resources_and_preserves_query() {
        let target = resolve_dash_scope_target(
            "https://cdn.example.com/video/representation/",
            "segments/chunk-12.m4s",
            Some("token=a%2Bb&part=2"),
        )
        .expect("relative resource should resolve inside the signed scope");

        assert_eq!(
            target,
            "https://cdn.example.com/video/representation/segments/chunk-12.m4s?token=a%2Bb&part=2"
        );
    }

    #[test]
    fn dash_scope_supports_an_exact_resource() {
        let target = resolve_dash_scope_target(
            "https://cdn.example.com/live/refresh.mpd?token=abc",
            "",
            None,
        )
        .expect("empty path should resolve to the signed resource itself");

        assert_eq!(target, "https://cdn.example.com/live/refresh.mpd?token=abc");
    }

    #[test]
    fn dash_scope_rejects_path_and_origin_escape() {
        for resource_path in [
            "../secret.m4s",
            "%2e%2e/secret.m4s",
            "%252e%252e%252fsecret.m4s",
            "https://evil.example/secret.m4s",
            "\\\\evil.example\\secret.m4s",
        ] {
            assert!(
                resolve_dash_scope_target(
                    "https://cdn.example.com/video/representation/",
                    resource_path,
                    None,
                )
                .is_err(),
                "resource path should be rejected: {resource_path}"
            );
        }

        assert!(resolve_dash_scope_target(
            "https://cdn.example.com/live/refresh.mpd",
            "other.mpd",
            None,
        )
        .is_err());
    }

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
