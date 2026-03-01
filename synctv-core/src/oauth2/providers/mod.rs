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

/// Initialize `OAuth2` provider registry
///
/// Call this during application startup to register all available provider types.
pub fn init_providers() {
    use crate::oauth2::register_provider_factory;

    // Register all provider factory functions
    register_provider_factory("github", github::github_factory);
    register_provider_factory("google", google::google_factory);
    register_provider_factory("logto", logto::logto_factory);
    register_provider_factory("oidc", oidc::oidc_factory);
}
