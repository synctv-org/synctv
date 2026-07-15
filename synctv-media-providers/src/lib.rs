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
pub mod acfun;
pub mod alist;
pub mod bilibili;
pub mod cctv;
pub mod cloudreve;
pub mod douyin;
pub mod douyu;
pub mod emby;
pub mod fnos;
pub mod huya;
pub mod nextcloud;
pub mod qnap;
pub mod seafile;
pub mod synology;
pub mod tiktok;
pub mod truenas;
pub mod twitch;
pub mod youtube;

// gRPC servers (wrap HTTP clients)
pub mod grpc;

// Remote provider gRPC clients and transport connection helpers.
pub mod remote_transport;

// DTOs exchanged by local and remote provider clients.
pub mod transport_dto;

// Re-export client types for convenience
pub use alist::{AlistClient, AlistError};
pub use bilibili::{BilibiliClient, BilibiliError};
pub use cloudreve::CloudreveClient;
pub use emby::{EmbyClient, EmbyError};
pub use error::{
    check_response, fetch_json, json_with_limit, provider_backoff, text_with_limit, with_retry,
    ProviderClientError, MAX_RESPONSE_SIZE, PROVIDER_USER_AGENT,
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

#[cfg(any(
    feature = "tls-aws-lc",
    feature = "tls-ring",
    feature = "tls-webpki-roots",
    feature = "tls-native-roots"
))]
pub fn install_process_crypto_provider() {
    if rustls::crypto::CryptoProvider::install_default(default_crypto_provider()).is_err() {
        tracing::debug!("Process rustls crypto provider was already installed");
    }
}

#[cfg(not(any(
    feature = "tls-aws-lc",
    feature = "tls-ring",
    feature = "tls-webpki-roots",
    feature = "tls-native-roots"
)))]
pub const fn install_process_crypto_provider() {}

#[cfg(any(
    feature = "tls-aws-lc",
    feature = "tls-ring",
    feature = "tls-webpki-roots",
    feature = "tls-native-roots"
))]
fn default_crypto_provider() -> rustls::crypto::CryptoProvider {
    #[cfg(feature = "tls-aws-lc")]
    {
        rustls::crypto::aws_lc_rs::default_provider()
    }

    #[cfg(all(
        not(feature = "tls-aws-lc"),
        any(
            feature = "tls-ring",
            feature = "tls-webpki-roots",
            feature = "tls-native-roots"
        )
    ))]
    {
        rustls::crypto::ring::default_provider()
    }
}
