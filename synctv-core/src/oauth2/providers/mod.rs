//! `OAuth2` provider implementations
//!
//! Each provider is implemented as a separate module with:
//! 1. Its own provider struct
//! 2. A `create()` factory function
//! 3. A public factory function for registration
//!
//! Factory pattern: providers are registered once, then created multiple times with different configs.

pub mod github;
pub mod google;
pub mod logto;
pub mod oidc;

// Re-export provider structs and config structs for convenience
pub use github::{GitHubConfig, GitHubProvider};
pub use google::{GoogleConfig, GoogleProvider};
pub use logto::{LogtoConfig, LogtoProvider};
pub use oidc::{OidcConfig, OidcProvider};

/// Build a registry populated with all built-in `OAuth2` providers.
#[must_use]
pub fn provider_registry() -> crate::oauth2::ProviderRegistry {
    let registry = crate::oauth2::ProviderRegistry::new();
    registry.register("github", github::github_factory);
    registry.register("google", google::google_factory);
    registry.register("logto", logto::logto_factory);
    registry.register("oidc", oidc::oidc_factory);
    registry
}
