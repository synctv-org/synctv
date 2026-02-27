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
pub mod crypto_utils;
pub mod error;
pub mod provider_client;
pub mod registry;
pub mod traits;

// MediaProvider implementations (adapters)
pub mod alist;
pub mod bilibili;
pub mod direct_url;
pub mod emby;
pub mod rtmp;
pub mod live_proxy;

pub use config::*;
pub use context::*;
pub use error::*;
pub use provider_client::{ProviderClientManager, global_client_manager};
pub use registry::*;
pub use traits::*;

// Re-export providers
pub use alist::AlistProvider;
pub use bilibili::BilibiliProvider;
pub use direct_url::DirectUrlProvider;
pub use emby::EmbyProvider;
pub use rtmp::RtmpProvider;
pub use live_proxy::LiveProxyProvider;

/// Parse a `serde_json::Value` into a typed source config.
///
/// Common helper for provider `TryFrom<&Value>` implementations.
pub fn parse_source_config<T: serde::de::DeserializeOwned>(
    value: &serde_json::Value,
    provider_name: &str,
) -> std::result::Result<T, ProviderError> {
    serde_json::from_value(value.clone()).map_err(|e| {
        ProviderError::InvalidConfig(format!("Failed to parse {provider_name} source config: {e}"))
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
pub fn build_live_playback(
    base_url: &str,
    media_id: &str,
    room_id: &str,
) -> PlaybackResult {
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

/// Build a cache key for provider playback results.
///
/// Uses SHA256 to create a deterministic cache key from the key prefix,
/// provider name, and content identifier. This ensures consistent cache keys
/// across different provider implementations.
///
/// # Arguments
///
/// * `key_prefix` - The cache key prefix (e.g., from `ProviderContext::key_prefix`)
/// * `provider_name` - The provider name (e.g., "bilibili", "alist", "emby")
/// * `identifier` - A unique identifier for the content (e.g., "host:token:path")
///
/// # Returns
///
/// A cache key string in the format: `{key_prefix}:playback:{provider}:{sha256_hash}`
#[must_use]
pub fn build_playback_cache_key(key_prefix: &str, provider_name: &str, identifier: &str) -> String {
    use sha2::{Sha256, Digest};
    format!(
        "{}:playback:{}:{:x}",
        key_prefix,
        provider_name,
        Sha256::digest(identifier.as_bytes())
    )
}

/// Build a fallback cache key for unknown/invalid provider configurations.
///
/// Returns a cache key that won't match any valid configuration but still
/// follows the expected format.
///
/// # Arguments
///
/// * `key_prefix` - The cache key prefix
/// * `provider_name` - The provider name
///
/// # Returns
///
/// A cache key string: `{key_prefix}:playback:{provider}:unknown`
#[must_use]
pub fn build_unknown_cache_key(key_prefix: &str, provider_name: &str) -> String {
    format!("{key_prefix}:playback:{provider_name}:unknown")
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
];

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
                    sanitized.insert(key.clone(), serde_json::Value::String("[REDACTED]".to_string()));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_playback_cache_key_produces_consistent_format() {
        let key = build_playback_cache_key("synctv:test", "bilibili", "video:BV123:12345:userhash");
        assert!(key.starts_with("synctv:test:playback:bilibili:"));
        // Should contain a hex hash (64 characters for SHA256)
        let parts: Vec<&str> = key.split(':').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[4].len(), 64); // SHA256 hex digest length
    }

    #[test]
    fn test_build_playback_cache_key_same_input_same_output() {
        let key1 = build_playback_cache_key("prefix", "alist", "host:token:path");
        let key2 = build_playback_cache_key("prefix", "alist", "host:token:path");
        assert_eq!(key1, key2);
    }

    #[test]
    fn test_build_playback_cache_key_different_input_different_output() {
        let key1 = build_playback_cache_key("prefix", "alist", "host:token1:path");
        let key2 = build_playback_cache_key("prefix", "alist", "host:token2:path");
        assert_ne!(key1, key2);
    }

    #[test]
    fn test_build_playback_cache_key_different_provider_different_output() {
        let identifier = "same:identifier";
        let key1 = build_playback_cache_key("prefix", "alist", identifier);
        let key2 = build_playback_cache_key("prefix", "emby", identifier);
        assert_ne!(key1, key2);
        assert!(key1.contains(":alist:"));
        assert!(key2.contains(":emby:"));
    }

    #[test]
    fn test_build_unknown_cache_key_format() {
        let key = build_unknown_cache_key("synctv:test", "bilibili");
        assert_eq!(key, "synctv:test:playback:bilibili:unknown");
    }

    #[test]
    fn test_build_unknown_cache_key_different_providers() {
        let key1 = build_unknown_cache_key("prefix", "alist");
        let key2 = build_unknown_cache_key("prefix", "emby");
        assert_ne!(key1, key2);
        assert!(key1.ends_with(":alist:unknown"));
        assert!(key2.ends_with(":emby:unknown"));
    }
}

