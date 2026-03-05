// Media Provider System
//
// Three-tier architecture:
//
// Tier 1: synctv-media-providers (Pure provider HTTP clients)
//   - alist::AlistClient, bilibili::BilibiliClient, emby::EmbyClient
//   - Independent libraries with no MediaProvider dependency
//   - Can be used as provider_instances
//
// Tier 2: synctv-core/provider (MediaProvider adapters)
//   - AlistProvider, BilibiliProvider, EmbyProvider
//   - Call synctv-media-providers clients to implement MediaProvider trait
//
// Tier 3: synctv-core/service/providers_manager
//   - ProvidersManager - manages all MediaProvider instances
//   - Factory pattern for creating providers
//   - Integration with RemoteProviderManager

// Core traits and types
pub mod config;
pub mod context;
pub mod credential_resolver;
pub mod crypto_utils;
pub mod error;
pub mod provider_client;
pub mod proxy;
pub mod registry;
pub mod store;
pub mod traits;

// MediaProvider implementations (adapters)
pub mod alist;
pub mod bilibili;
pub mod direct_url;
pub mod emby;
pub mod live_proxy;
pub mod rtmp;

pub use config::*;
pub use context::*;
pub use credential_resolver::*;
pub use error::*;
pub use provider_client::ProviderClientManager;
pub use proxy::*;
pub use registry::*;
pub use store::*;
pub use traits::*;

// Re-export providers
pub use alist::AlistProvider;
pub use bilibili::BilibiliProvider;
pub use direct_url::DirectUrlProvider;
pub use emby::EmbyProvider;
pub use live_proxy::LiveProxyProvider;
pub use rtmp::RtmpProvider;

/// Bundle of all media provider instances.
///
/// Consolidates per-provider fields into a single struct. Pass this through
/// the application instead of 4 separate `Arc<XxxProvider>` fields.
/// Adding a new provider only requires updating this struct and its
/// `build_proxy_registry()` method.
#[derive(Clone)]
pub struct ProviderSet {
    pub alist: std::sync::Arc<AlistProvider>,
    pub bilibili: std::sync::Arc<BilibiliProvider>,
    pub emby: std::sync::Arc<EmbyProvider>,
    pub direct_url: std::sync::Arc<DirectUrlProvider>,
}

impl ProviderSet {
    /// Build a `ProxyProviderRegistry` from this provider set.
    ///
    /// Registers all proxy-capable providers under their canonical names.
    /// Called once at startup; both HTTP and gRPC share the resulting registry.
    #[must_use]
    pub fn build_proxy_registry(&self) -> ProxyProviderRegistry {
        let registry = ProxyProviderRegistry::new();
        registry.register(AlistProvider::NAME, self.alist.clone());
        registry.register(BilibiliProvider::NAME, self.bilibili.clone());
        registry.register(EmbyProvider::NAME, self.emby.clone());
        registry.register(DirectUrlProvider::NAME, self.direct_url.clone());
        registry
    }
}

/// Parse a `serde_json::Value` into a typed source config.
///
/// Common helper for provider `TryFrom<&Value>` implementations.
pub fn parse_source_config<T: serde::de::DeserializeOwned>(
    value: &serde_json::Value,
    provider_name: &str,
) -> std::result::Result<T, ProviderError> {
    serde_json::from_value(value.clone()).map_err(|e| {
        ProviderError::InvalidConfig(format!(
            "Failed to parse {provider_name} source config: {e}"
        ))
    })
}

/// Build a `PlaybackResult` with HLS and FLV playback URLs for a live stream.
///
/// Shared by `RtmpProvider` and `LiveProxyProvider` which both generate
/// identical playback URLs pointing to synctv's own HLS/FLV endpoints.
/// The only difference between the two is the `metadata` map (`live_proxy` adds
/// `source_url` and `provider` fields), which callers can extend after this
/// function returns.
#[must_use]
pub fn build_live_playback(base_url: &str, media_id: &str, room_id: &str) -> PlaybackResult {
    use serde_json::json;
    use std::collections::HashMap;

    let live_expires_at = Some(chrono::Utc::now().timestamp() + 30);

    let mut playback_infos = HashMap::new();

    // HLS URL — matches actual HTTP route: /api/room/movie/live/hls/list/:media_id?room_id=:room_id
    playback_infos.insert(
        "hls".to_string(),
        PlaybackInfo {
            urls: vec![format!(
                "{}/api/room/movie/live/hls/list/{}?room_id={}",
                base_url, media_id, room_id
            )],
            format: "m3u8".to_string(),
            headers: HashMap::new(),
            subtitles: Vec::new(),
            expires_at: live_expires_at,
            cors_proxy_required: false,
        },
    );

    // FLV URL — matches actual HTTP route: /api/room/movie/live/flv/:media_id.flv?room_id=:room_id
    playback_infos.insert(
        "flv".to_string(),
        PlaybackInfo {
            urls: vec![format!(
                "{}/api/room/movie/live/flv/{}.flv?room_id={}",
                base_url, media_id, room_id
            )],
            format: "flv".to_string(),
            headers: HashMap::new(),
            subtitles: Vec::new(),
            expires_at: live_expires_at,
            cors_proxy_required: false,
        },
    );

    let mut metadata = HashMap::new();
    metadata.insert("is_live".to_string(), json!(true));
    metadata.insert("media_id".to_string(), json!(media_id));
    metadata.insert("room_id".to_string(), json!(room_id));

    PlaybackResult {
        playback_infos,
        default_mode: "hls".to_string(),
        metadata,
    }
}

/// Standard Bilibili HTTP headers required for CDN requests.
///
/// These headers must be sent with all Bilibili media requests (video, audio, subtitles)
/// to avoid being blocked by Bilibili's CDN. Shared between the provider layer
/// (playback result headers) and the API proxy layer.
#[must_use]
pub fn bilibili_headers() -> std::collections::HashMap<String, String> {
    let mut headers = std::collections::HashMap::new();
    headers.insert(
        "Referer".to_string(),
        "https://www.bilibili.com".to_string(),
    );
    headers.insert(
        "User-Agent".to_string(),
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36".to_string(),
    );
    headers
}

/// Credential field names that must never be included in API responses.
///
/// These fields are stripped from `source_config` before serialization to
/// clients, preventing exposure of API keys, tokens, passwords, and cookies.
const CREDENTIAL_FIELDS: &[&str] = &[
    "token",
    "api_key",
    "password",
    "cookies",
    "secret",
    "access_token",
    "credential_ref",
];

/// Rewrite playback URLs in a `PlaybackResult` to use signed proxy URLs.
///
/// Called by providers after `generate_playback` when `signing_key`, `room_id`,
/// and `user_id` are available in the context. Each playback mode gets a signed
/// proxy URL based on whether `cors_proxy_required` is set or the mode suggests proxying.
///
/// The version and provider name are needed to construct the proxy path.
pub fn sign_playback_urls(
    result: &mut PlaybackResult,
    provider_name: &str,
    version: &str,
    signing_key: &crate::service::proxy_signature::ProxySigningKey,
    room_id: &str,
    user_id: &str,
    expires_at: i64,
) {
    for (mode_name, info) in &mut result.playback_infos {
        if info.urls.is_empty() {
            continue;
        }

        // Determine the action based on the mode/format
        let action = if info.format == "m3u8" || mode_name.contains("hls") {
            "m3u8"
        } else {
            "stream"
        };

        let signed_url = crate::service::proxy_signature::build_signed_proxy_url(
            provider_name,
            version,
            action,
            signing_key,
            room_id,
            user_id,
            expires_at,
        );

        // Replace the first URL with the signed proxy URL
        info.urls = vec![signed_url];
        // Proxy handles headers — client doesn't need them
        info.headers.clear();
        info.cors_proxy_required = false;

        // Also sign subtitle URLs
        for (idx, subtitle) in info.subtitles.iter_mut().enumerate() {
            subtitle.url = crate::service::proxy_signature::build_signed_proxy_url(
                provider_name,
                version,
                &format!("subtitle/{idx}"),
                signing_key,
                room_id,
                user_id,
                expires_at,
            );
        }
    }
}

/// Strip credential fields from a `source_config` value before sending to clients.
///
/// Returns a sanitized copy with sensitive fields replaced by `"[REDACTED]"`.
/// Recursively processes nested objects and arrays.
/// Non-object/non-array values are returned unchanged.
#[must_use]
pub fn strip_source_config_credentials(source_config: &serde_json::Value) -> serde_json::Value {
    match source_config {
        serde_json::Value::Object(map) => {
            let mut sanitized = serde_json::Map::with_capacity(map.len());
            for (key, value) in map {
                if CREDENTIAL_FIELDS.contains(&key.as_str()) {
                    sanitized.insert(
                        key.clone(),
                        serde_json::Value::String("[REDACTED]".to_string()),
                    );
                } else {
                    sanitized.insert(key.clone(), strip_source_config_credentials(value));
                }
            }
            serde_json::Value::Object(sanitized)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(strip_source_config_credentials).collect())
        }
        other => other.clone(),
    }
}
