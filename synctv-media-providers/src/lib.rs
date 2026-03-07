#![cfg_attr(test, allow(clippy::unwrap_used))]
// SyncTV Provider Clients
//
// This crate contains pure HTTP client implementations and gRPC servers for various media providers.
// These clients are independent of the MediaProvider trait and can be used standalone
// or as provider_instances in the SyncTV system.
//
// Architecture:
// - synctv-media-providers: Pure HTTP clients + gRPC servers (Alist, Bilibili, Emby)
// - synctv-core/provider: MediaProvider trait implementations (adapters calling these clients)
// - synctv-core/service: ProvidersManager for managing provider instances

// Shared error types
pub mod error;

// Shared circuit breaker primitives for provider gRPC serving.
pub mod circuit_breaker;

// SSRF protection primitives (shared with synctv-core)
pub mod ssrf;

// Credential storage (trait and implementations)
pub mod credential;

// HTTP clients (no MediaProvider dependency)
pub mod alist;
pub mod bilibili;
pub mod emby;

// gRPC servers (wrap HTTP clients)
pub mod grpc;

// Re-export client types for convenience
pub use alist::error::AlistError;
pub use alist::AlistClient;
pub use bilibili::error::BilibiliError;
pub use bilibili::BilibiliClient;
pub use emby::error::EmbyError;
pub use emby::EmbyClient;
pub use error::ProviderClientError;

// Re-export credential types
pub use credential::{
    CredentialData, CredentialStorage, CredentialStorageError, FieldEncryption,
    InMemoryCredentialStorage, ProviderType, Result as CredentialResult, StoredCredential,
};

#[cfg(feature = "postgres")]
pub use credential::PostgresCredentialStorage;
