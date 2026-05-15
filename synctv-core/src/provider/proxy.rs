// Provider Proxy Trait — Abstract proxy handling for MediaProvider
// Allows each provider to define its own proxy behavior (URL resolution,
// subtitle lookups, M3U8 rewriting) without depending on axum or synctv-proxy.
// The HTTP layer receives a `ProxyAction` and executes it generically.

use async_trait::async_trait;
use http::HeaderMap;
use std::collections::HashMap;
use std::sync::Arc;

use super::access::ProviderAccessService;
use super::error::ProviderError;
use super::store::{ProviderStore, ProviderStoreExt, VersionedPlayback};
use super::{ExecutionControl, MediaProvider};
use crate::models::{MediaId, RoomId, UserId};
use crate::repository::UserProviderCredentialRepository;
use crate::service::proxy_signature::{ProxySigningKey, ProxyUrlClaims};
use crate::service::{CredentialEncryption, RoomService};
use crate::PublicIdCodec;

/// What action the HTTP layer should perform after the provider resolves the request.
#[derive(Debug, Clone)]
pub enum ProxyAction {
    /// Fetch the URL and forward the response body (video stream, subtitle, etc.)
    FetchAndForward {
        url: String,
        headers: HashMap<String, String>,
        /// Provider-selected Range header for this request.
        ///
        /// This is intentionally separate from `headers`: it drives proxy
        /// range/slice behavior without becoming part of the resource cache key.
        range_header: Option<String>,
    },
    /// Fetch an M3U8 manifest, rewrite internal URLs for proxying, then forward.
    M3u8Rewrite {
        url: String,
        headers: HashMap<String, String>,
        proxy_base: String,
        proxy_url_claims: Option<ProxyUrlClaims>,
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
        room_id: RoomId,
        media_id: MediaId,
        user_id: UserId,
        expires_at: i64,
    },
    /// Generate an HLS playlist for a live stream.
    LiveHlsPlaylist {
        provider_name: String,
        room_id: RoomId,
        media_id: MediaId,
        version: String,
    },
    /// Serve a live HLS segment from the API layer.
    LiveHlsSegment {
        room_id: RoomId,
        media_id: MediaId,
        segment_name: String,
        disguised_as_png: bool,
    },
}

impl ProxyAction {
    #[must_use]
    pub const fn bypasses_unary_timeout(&self) -> bool {
        matches!(
            self,
            Self::LiveFlv { .. } | Self::LiveHlsPlaylist { .. } | Self::LiveHlsSegment { .. }
        )
    }
}

/// Services available to providers during proxy resolution.
///
/// Gives providers DB access (e.g., fetching media from playlists) without
/// depending on axum or the HTTP layer.
pub struct ProxyServices {
    pub room_service: Arc<RoomService>,
    pub credential_encryption: Option<CredentialEncryption>,
    pub credential_repo: Arc<UserProviderCredentialRepository>,
    pub provider_access_service: Option<Arc<dyn ProviderAccessService>>,
    pub signing_key: Arc<ProxySigningKey>,
    pub public_id_codec: Arc<PublicIdCodec>,
}

pub(crate) fn parse_proxy_user_id(
    codec: &PublicIdCodec,
    value: &str,
    context: &str,
) -> Result<UserId, ProviderError> {
    parse_proxy_id(codec, value, context)
}

pub(crate) fn parse_proxy_room_id(
    codec: &PublicIdCodec,
    value: &str,
    context: &str,
) -> Result<RoomId, ProviderError> {
    parse_proxy_id(codec, value, context)
}

pub(crate) fn parse_proxy_media_id(
    codec: &PublicIdCodec,
    value: &str,
    context: &str,
) -> Result<MediaId, ProviderError> {
    parse_proxy_id(codec, value, context)
}

fn parse_proxy_id<T>(codec: &PublicIdCodec, value: &str, context: &str) -> Result<T, ProviderError>
where
    T: crate::PublicIdType,
{
    codec
        .decode::<T>(value)
        .map_err(|error| ProviderError::InvalidConfig(format!("Invalid {error} in {context}")))
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
    /// Cooperative execution control propagated from the caller.
    pub request_context: Option<&'a ExecutionControl>,
    /// Original HTTP request headers exposed to providers.
    ///
    /// The proxy executor never forwards these directly. Providers must
    /// explicitly select request headers they want the proxy layer to honor.
    pub request_headers: &'a HeaderMap,
}

/// Return the client Range header selected by a provider for stream proxying.
///
/// Providers call this helper at the exact proxy endpoints where Range
/// semantics are desired. The lower proxy executor does not inspect raw client
/// headers by itself.
#[must_use]
pub fn selected_range_header(ctx: &ProxyRequestContext<'_>) -> Option<String> {
    ctx.request_headers
        .get(http::header::RANGE)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string)
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

struct MediaProviderProxyAdapter {
    provider: Arc<dyn MediaProvider>,
}

#[async_trait]
impl ProviderProxy for MediaProviderProxyAdapter {
    async fn resolve_proxy(
        &self,
        ctx: &ProxyRequestContext<'_>,
    ) -> Result<ProxyAction, ProviderError> {
        let proxy = self.provider.as_provider_proxy().ok_or_else(|| {
            ProviderError::UnsupportedFormat(format!(
                "Provider '{}' does not support proxy routes",
                self.provider.name()
            ))
        })?;
        proxy.resolve_proxy(ctx).await
    }
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

    /// Register a media provider when it advertises proxy support.
    ///
    /// This lets `ProvidersManager` supply the same provider instances used for
    /// playback to the HTTP proxy registry without constructing a second
    /// provider graph.
    pub fn register_media_provider(&self, provider: Arc<dyn MediaProvider>) {
        if provider.as_provider_proxy().is_some() {
            let name = provider.name().to_string();
            self.providers
                .insert(name, Arc::new(MediaProviderProxyAdapter { provider }));
        }
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
    request_context: Option<&ExecutionControl>,
) -> Result<VersionedPlayback, ProviderError> {
    if let Some(request_context) = request_context {
        request_context
            .check_active()
            .map_err(|err| ProviderError::NetworkError(err.to_string()))?;
    }

    let store = store.ok_or_else(|| ProviderError::ApiError("Store not configured".into()))?;
    let versioned: VersionedPlayback = store
        .get(&format!("v:{version}"))
        .await
        .map_err(|e| ProviderError::ApiError(format!("Store error: {e}")))?
        .ok_or(ProviderError::NotFound)?;

    if let Some(request_context) = request_context {
        request_context
            .check_active()
            .map_err(|err| ProviderError::NetworkError(err.to_string()))?;
    }

    if versioned.is_expired() {
        return Err(ProviderError::NotFound);
    }
    Ok(versioned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{store::InMemoryProviderStore, ExecutionControl};
    use crate::provider::{store::VersionedPlayback, PlaybackResult};
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn test_lookup_versioned_no_store() {
        let result = lookup_versioned(None, "v1", None).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ProviderError::ApiError(_)));
    }

    #[tokio::test]
    async fn test_lookup_versioned_not_found() {
        let store: Arc<dyn ProviderStore> = Arc::new(InMemoryProviderStore::new(100));
        let result = lookup_versioned(Some(&store), "nonexistent", None).await;
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
        let result = lookup_versioned(Some(&store), "v1", None).await;
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
        let result = lookup_versioned(Some(&store), "v1", None).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().version, "v1");
    }

    #[test]
    fn test_proxy_ids_require_public_id_prefixes() {
        let codec = PublicIdCodec::default_for_tests();

        assert_eq!(
            parse_proxy_room_id(&codec, "room_42", "proxy metadata").unwrap(),
            RoomId::expect_positive(42)
        );
        assert_eq!(
            parse_proxy_media_id(&codec, "med_99", "proxy metadata").unwrap(),
            MediaId::expect_positive(99)
        );
        assert_eq!(
            parse_proxy_user_id(&codec, "usr_7", "proxy metadata").unwrap(),
            UserId::expect_positive(7)
        );
        assert!(parse_proxy_room_id(&codec, "42", "proxy metadata").is_err());
        assert!(parse_proxy_media_id(&codec, "99", "proxy metadata").is_err());
        assert!(parse_proxy_user_id(&codec, "7", "proxy metadata").is_err());
    }

    #[tokio::test]
    async fn test_lookup_versioned_respects_cancellation() {
        let store: Arc<dyn ProviderStore> = Arc::new(InMemoryProviderStore::new(100));
        let vp = VersionedPlayback {
            version: "v1".to_string(),
            result: PlaybackResult {
                playback_infos: HashMap::new(),
                default_mode: "direct".to_string(),
                metadata: HashMap::new(),
            },
            expires_at: chrono::Utc::now().timestamp() + 60,
        };
        store
            .set("v:v1", &vp, Duration::from_mins(1))
            .await
            .unwrap();

        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let request_context = ExecutionControl::from_parts(None, cancellation);

        let result = lookup_versioned(Some(&store), "v1", Some(&request_context)).await;
        assert!(matches!(result, Err(ProviderError::NetworkError(_))));
    }
}
