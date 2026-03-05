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
}

/// Abstract proxy request context (no axum/HTTP framework types).
pub struct ProxyRequestContext<'a> {
    /// The sub-path after the provider's proxy base.
    /// e.g., for `/api/providers/bilibili/proxy/abc123/subtitle/zh`,
    /// this would be `"abc123/subtitle/zh"`.
    pub sub_path: &'a str,
    /// Provider store for looking up cached `VersionedPlayback`.
    pub store: Option<&'a Arc<dyn ProviderStore>>,
    /// The proxy base URL (for M3U8 rewriting).
    /// e.g., `"/api/providers/bilibili/proxy"`
    pub proxy_base: &'a str,
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
