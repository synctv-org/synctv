// Provider Proxy Trait — Abstract proxy handling for MediaProvider
//
// Allows each provider to define its own proxy behavior (URL resolution,
// subtitle lookups, M3U8 rewriting) without depending on axum or synctv-proxy.
// The HTTP layer receives a `ProxyAction` and executes it generically.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use super::error::ProviderError;
use super::store::{ProviderStore, ProviderStoreExt, VersionedPlayback};
use crate::repository::UserProviderCredentialRepository;
use crate::service::proxy_signature::{ProxySigningKey, ProxyUrlClaims};
use crate::service::{CredentialEncryption, RoomService};

/// What action the HTTP layer should perform after the provider resolves the request.
#[derive(Debug, Clone)]
pub enum ProxyAction {
    /// Fetch the URL and forward the response body (video stream, subtitle, etc.)
    FetchAndForward {
        url: String,
        headers: HashMap<String, String>,
    },
    /// Fetch an M3U8 manifest, rewrite internal URLs for proxying, then forward.
    M3u8Rewrite {
        url: String,
        headers: HashMap<String, String>,
        proxy_base: String,
    },
    /// Return a direct response body with a content type.
    ///
    /// Used for provider-specific responses that don't involve upstream proxying
    /// (e.g., SSE danmaku info, JSON metadata). The HTTP layer wraps this into
    /// an appropriate response.
    DirectBody {
        body: Vec<u8>,
        content_type: String,
        status: u16,
    },
    /// Execute a live FLV stream directly from the API layer.
    LiveFlv {
        provider_name: String,
        room_id: String,
        media_id: String,
        user_id: String,
        expires_at: i64,
    },
    /// Generate an HLS playlist for a live stream.
    LiveHlsPlaylist {
        provider_name: String,
        room_id: String,
        media_id: String,
        version: String,
    },
    /// Serve a live HLS segment from the API layer.
    LiveHlsSegment {
        room_id: String,
        media_id: String,
        segment_name: String,
        disguised_as_png: bool,
    },
}

/// Services available to providers during proxy resolution.
///
/// Gives providers DB access (e.g., fetching media from playlists) without
/// depending on axum or the HTTP layer.
pub struct ProxyServices {
    pub room_service: Arc<RoomService>,
    pub credential_encryption: Option<CredentialEncryption>,
    pub credential_repo: Arc<UserProviderCredentialRepository>,
    pub signing_key: Arc<ProxySigningKey>,
}

/// Abstract proxy request context (no axum/HTTP framework types).
pub struct ProxyRequestContext<'a> {
    /// The sub-path after the provider's proxy base.
    /// e.g., for `/api/providers/proxy/bilibili/abc123/subtitle/zh`,
    /// this would be `"abc123/subtitle/zh"`.
    pub sub_path: &'a str,
    /// Raw query string from the request URL (without leading `?`).
    pub query_string: Option<&'a str>,
    /// Provider store for looking up cached `VersionedPlayback`.
    pub store: Option<&'a Arc<dyn ProviderStore>>,
    /// The proxy base URL (for M3U8 rewriting).
    /// e.g., `"/api/providers/proxy/bilibili"`
    pub proxy_base: &'a str,
    /// Services for DB access during proxy resolution.
    pub services: &'a ProxyServices,
    /// Verified claims from the incoming request's HMAC signature (if available).
    /// Providers use these to re-sign M3U8 proxy_base URLs with the same claims.
    pub verified_claims: Option<&'a ProxyUrlClaims>,
}

/// Optional trait for providers that support HTTP proxy routes.
#[async_trait]
pub trait ProviderProxy: Send + Sync {
    /// Resolve a proxy request to an action.
    ///
    /// The provider parses `ctx.sub_path` to determine what to proxy.
    /// Returns `ProxyAction` telling the HTTP layer what to fetch.
    async fn resolve_proxy(
        &self,
        ctx: &ProxyRequestContext<'_>,
    ) -> Result<ProxyAction, ProviderError>;
}

/// Registry of proxy-capable providers, keyed by provider type name.
///
/// Populated at startup. The proxy handler looks up providers by name instead
/// of using a hardcoded match, so adding a new provider only requires
/// registering it here.
pub struct ProxyProviderRegistry {
    providers: dashmap::DashMap<String, Arc<dyn ProviderProxy>>,
}

impl ProxyProviderRegistry {
    /// Create a new empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            providers: dashmap::DashMap::new(),
        }
    }

    /// Register a proxy-capable provider under its type name (e.g., `"emby"`).
    pub fn register(&self, name: impl Into<String>, provider: Arc<dyn ProviderProxy>) {
        self.providers.insert(name.into(), provider);
    }

    /// Look up a provider by type name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn ProviderProxy>> {
        self.providers.get(name).map(|r| r.value().clone())
    }
}

impl Default for ProxyProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Look up a cached `VersionedPlayback` from the store by version string.
pub async fn lookup_versioned(
    store: Option<&Arc<dyn ProviderStore>>,
    version: &str,
) -> Result<VersionedPlayback, ProviderError> {
    let store = store.ok_or_else(|| ProviderError::ApiError("Store not configured".into()))?;
    let versioned: VersionedPlayback = store
        .get(&format!("v:{version}"))
        .await
        .map_err(|e| ProviderError::ApiError(format!("Store error: {e}")))?
        .ok_or(ProviderError::NotFound)?;
    if versioned.is_expired() {
        return Err(ProviderError::NotFound);
    }
    Ok(versioned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::store::InMemoryProviderStore;
    use crate::provider::{store::VersionedPlayback, PlaybackResult};
    use std::time::Duration;

    #[tokio::test]
    async fn test_lookup_versioned_no_store() {
        let result = lookup_versioned(None, "v1").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ProviderError::ApiError(_)));
    }

    #[tokio::test]
    async fn test_lookup_versioned_not_found() {
        let store: Arc<dyn ProviderStore> = Arc::new(InMemoryProviderStore::new(100));
        let result = lookup_versioned(Some(&store), "nonexistent").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ProviderError::NotFound));
    }

    #[tokio::test]
    async fn test_lookup_versioned_expired() {
        let store: Arc<dyn ProviderStore> = Arc::new(InMemoryProviderStore::new(100));
        let vp = VersionedPlayback {
            version: "v1".to_string(),
            result: PlaybackResult {
                playback_infos: HashMap::new(),
                default_mode: "direct".to_string(),
                metadata: HashMap::new(),
            },
            expires_at: 0, // Already expired
        };
        store
            .set("v:v1", &vp, Duration::from_mins(1))
            .await
            .unwrap();
        let result = lookup_versioned(Some(&store), "v1").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ProviderError::NotFound));
    }

    #[tokio::test]
    async fn test_lookup_versioned_success() {
        let store: Arc<dyn ProviderStore> = Arc::new(InMemoryProviderStore::new(100));
        let vp = VersionedPlayback {
            version: "v1".to_string(),
            result: PlaybackResult {
                playback_infos: HashMap::new(),
                default_mode: "direct".to_string(),
                metadata: HashMap::new(),
            },
            expires_at: chrono::Utc::now().timestamp() + 3600,
        };
        store
            .set("v:v1", &vp, Duration::from_mins(1))
            .await
            .unwrap();
        let result = lookup_versioned(Some(&store), "v1").await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().version, "v1");
    }
}
