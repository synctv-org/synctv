#![allow(clippy::missing_errors_doc)]

#[cfg(all(feature = "tls-aws-lc", feature = "tls-ring"))]
compile_error!("features \"tls-aws-lc\" and \"tls-ring\" are mutually exclusive - use only one");

// SyncTV Provider Clients
// This crate contains pure HTTP client implementations and gRPC servers for various media providers.
// These clients are independent of the MediaProvider trait and can be used standalone
// or as provider_instances in the SyncTV system.
// Architecture:
// - synctv-media-providers: Pure HTTP clients + gRPC servers (Alist, Bilibili, Emby)
// - synctv-core/provider: MediaProvider trait implementations (adapters calling these clients)
// - synctv-core/service: ProvidersManager for managing provider instances

// Shared error types
mod error;

// Shared circuit breaker primitives for provider gRPC serving.
pub mod circuit_breaker;

mod validation;

// Credential storage (trait and implementations)
mod credential;

// HTTP clients (no MediaProvider dependency)
pub mod alist;
pub mod bilibili;
pub mod emby;

// gRPC servers (wrap HTTP clients)
pub mod grpc;

// Re-export client types for convenience
pub use alist::{AlistClient, AlistError};
pub use bilibili::{BilibiliClient, BilibiliError};
pub use emby::{EmbyClient, EmbyError};
pub use error::{
    check_response, fetch_json, json_with_limit, provider_backoff, with_retry, ProviderClientError,
    MAX_RESPONSE_SIZE, PROVIDER_USER_AGENT,
};

// Re-export credential types
pub use credential::{
    CredentialData, CredentialStorage, CredentialStorageError, FieldEncryption,
    InMemoryCredentialStorage, ProviderType, Result as CredentialResult, StoredCredential,
};

/// Build the shared HTTP client configuration used by local media-provider clients.
#[must_use]
pub fn provider_http_client_builder(
    ssrf_guard: synctv_common::ssrf::SsrfGuard,
) -> synctv_common::http::SsrfSafeClientBuilder {
    synctv_common::http::SsrfSafeClientBuilder::new()
        .ssrf_guard(ssrf_guard)
        .connect_timeout(std::time::Duration::from_secs(10))
        .request_timeout(std::time::Duration::from_secs(30))
        .pool_max_idle_per_host(10)
}

/// Build a media-provider HTTP client.
pub fn build_provider_http_client(
    ssrf_guard: synctv_common::ssrf::SsrfGuard,
) -> Result<reqwest::Client, reqwest::Error> {
    provider_http_client_builder(ssrf_guard).build()
}
