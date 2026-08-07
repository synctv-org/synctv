//! Provider client manager and facade.
//!
//! Local clients are managed by `ProviderClientManager` rather than global
//! statics. Remote provider adapters live with the remote transport code so
//! gRPC wire concerns stay grouped behind this facade.

use super::ProviderError;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Arc;
use synctv_media_providers::alist::AlistInterface;
use synctv_media_providers::bilibili::BilibiliInterface;
use synctv_media_providers::emby::EmbyInterface;
#[cfg(test)]
use synctv_media_providers::remote_transport::RemoteProviderConnection;
pub(crate) use synctv_media_providers::remote_transport::{
    create_remote_alist_client, create_remote_bilibili_client, create_remote_emby_client,
};

#[cfg(test)]
static PROVIDER_CLIENT_MANAGER_MARKER_SEQ: AtomicUsize = AtomicUsize::new(1);

pub(crate) type AlistClientArc = Arc<dyn AlistInterface>;
pub(crate) type BilibiliClientArc = Arc<dyn BilibiliInterface>;
pub(crate) type EmbyClientArc = Arc<dyn EmbyInterface>;

/// Manager for provider clients that supports dependency injection.
///
/// In a multi-replica architecture, local clients should be managed through
/// this struct rather than global statics. This enables:
/// - Proper sharing of client instances across the application
/// - Testability through mock injection
/// - Consistent behavior across replicas
///
/// # Example
///
/// ```
/// use synctv_core::provider::ProviderClientManager;
/// use std::sync::Arc;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let manager = ProviderClientManager::new()?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct ProviderClientManager {
    alist: AlistClientArc,
    bilibili: BilibiliClientArc,
    emby: EmbyClientArc,
    #[cfg(test)]
    marker: usize,
}

impl std::fmt::Debug for ProviderClientManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderClientManager")
            .field("alist", &"AlistClientArc")
            .field("bilibili", &"BilibiliClientArc")
            .field("emby", &"EmbyClientArc")
            .finish()
    }
}

impl ProviderClientManager {
    /// Create a new `ProviderClientManager` with default local clients.
    pub fn new() -> Result<Self, synctv_media_providers::ProviderClientError> {
        let provider_client = synctv_media_providers::build_provider_http_client(
            synctv_common::ssrf::SsrfGuard::strict_policy(),
        )?;
        Ok(Self::new_with_provider_http_client(provider_client))
    }

    #[cfg(test)]
    pub(crate) fn new_for_tests() -> Result<Self, synctv_media_providers::ProviderClientError> {
        Self::new()
    }

    /// Create a new `ProviderClientManager` with a shared local provider HTTP client.
    #[must_use]
    pub fn new_with_provider_http_client(client: reqwest::Client) -> Self {
        Self {
            alist: Arc::new(synctv_media_providers::alist::AlistService::with_client(
                client.clone(),
            )),
            bilibili: Arc::new(
                synctv_media_providers::bilibili::BilibiliService::with_client(client.clone()),
            ),
            emby: Arc::new(synctv_media_providers::emby::EmbyService::with_client(
                client,
            )),
            #[cfg(test)]
            marker: PROVIDER_CLIENT_MANAGER_MARKER_SEQ.fetch_add(1, AtomicOrdering::Relaxed),
        }
    }

    /// Create a new `ProviderClientManager` with custom local clients.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn with_custom_clients(
        alist: AlistClientArc,
        bilibili: BilibiliClientArc,
        emby: EmbyClientArc,
    ) -> Self {
        Self {
            alist,
            bilibili,
            emby,
            #[cfg(test)]
            marker: PROVIDER_CLIENT_MANAGER_MARKER_SEQ.fetch_add(1, AtomicOrdering::Relaxed),
        }
    }

    /// Get the local Alist client.
    #[must_use]
    pub(crate) fn local_alist_client(&self) -> AlistClientArc {
        self.alist.clone()
    }

    /// Get the local Bilibili client.
    #[must_use]
    pub(crate) fn local_bilibili_client(&self) -> BilibiliClientArc {
        self.bilibili.clone()
    }

    /// Get the local Emby client.
    #[must_use]
    pub(crate) fn local_emby_client(&self) -> EmbyClientArc {
        self.emby.clone()
    }

    /// Resolve an Alist client: use remote if a connection is provided, otherwise local.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn resolve_alist_client(
        &self,
        remote_connection: Option<RemoteProviderConnection>,
    ) -> AlistClientArc {
        match remote_connection {
            Some(connection) => create_remote_alist_client(connection),
            None => self.local_alist_client(),
        }
    }

    #[cfg(test)]
    pub(crate) fn marker(&self) -> usize {
        self.marker
    }
}

impl From<synctv_media_providers::ProviderClientError> for ProviderError {
    fn from(error: synctv_media_providers::ProviderClientError) -> Self {
        use synctv_media_providers::ProviderClientError;
        match error {
            ProviderClientError::Network(msg) => Self::NetworkError(msg),
            ProviderClientError::Api { message, .. } => Self::ApiError(message),
            ProviderClientError::Parse(msg) | ProviderClientError::InvalidHeader(msg) => {
                Self::ParseError(msg)
            }
            ProviderClientError::Auth(msg) => Self::Authentication(msg),
            ProviderClientError::InvalidConfig(msg) => Self::InvalidConfig(msg),
            ProviderClientError::Http { status, url, .. } => Self::UpstreamHttp {
                status: status.as_u16(),
                url,
            },
            ProviderClientError::ResponseTooLarge { size } => {
                Self::ApiError(format!("Response too large ({size} bytes)"))
            }
        }
    }
}

#[cfg(test)]
#[path = "provider_client_tests.rs"]
mod tests;
