//! OAuth2/OIDC provider system with registry and factory pattern
//!
//! # Architecture (similar to Go's synctv/internal/provider/providers)
//!
//! 1. **Provider Registry**: Map of provider type -> provider instance
//! 2. **Factory Pattern**: `ProviderRegistry::create_provider()` looks up registry and clones with config
//! 3. **Decoupled**: Factory doesn't need to know about provider-specific configs
//! 4. **Clone Pattern**: Each provider implements Clone to create instances

pub mod config;
pub mod providers;

pub use config::ConfigLoader;
pub use providers::{GitHubConfig, GoogleConfig, LogtoConfig, OidcConfig};

use crate::Error;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

// ============================================================================
// Provider Trait
// ============================================================================

/// `OAuth2` provider trait
///
/// All `OAuth2` providers must implement this trait.
/// Similar to Go's `provider.Interface` from synctv/internal/provider
///
/// Only two methods needed:
/// 1. `NewAuthURL` - generate authorization URL
/// 2. `GetUserInfo` - exchange code for user info
#[async_trait]
pub trait Provider: Send + Sync {
    /// Provider type identifier (e.g., "github", "logto", "oidc")
    fn provider_type(&self) -> &str;

    /// Generate authorization URL with state and PKCE challenge
    ///
    /// Returns `(authorization_url, pkce_verifier)` where the PKCE verifier must be
    /// stored and passed back during `get_user_info` to complete the PKCE flow.
    ///
    /// Similar to Go's `NewAuthURL()` method, extended with PKCE (RFC 7636).
    async fn new_auth_url(&self, state: &str) -> Result<(String, String), Error>;

    /// Exchange authorization code for user info, verifying the PKCE challenge
    ///
    /// This method:
    /// 1. Exchanges the code for an access token (with PKCE verifier)
    /// 2. Fetches user info using the token
    /// 3. Returns user info (token is discarded)
    ///
    /// Similar to Go's `GetUserInfo()` method, extended with PKCE (RFC 7636).
    async fn get_user_info(&self, code: &str, pkce_verifier: &str)
        -> Result<OAuth2UserInfo, Error>;
}

/// `OAuth2` user info from provider
#[derive(Debug, Clone)]
pub struct OAuth2UserInfo {
    pub provider_user_id: String,
    pub username: String,
    pub email: Option<String>,
    pub avatar: Option<String>,
    /// Whether the provider has verified the user's email address
    pub email_verified: bool,
}

// ============================================================================
// Provider Registry
// ============================================================================

/// Factory function type for creating providers
///
/// Each provider type registers a factory function that knows how to
/// create instances of that provider with configuration.
/// All parameters (`client_id`, `client_secret`, `redirect_url`, etc.) are in config.
pub type ProviderFactory = fn(config: &serde_json::Value) -> Result<Box<dyn Provider>, Error>;

/// Instance-based provider registry.
///
/// Maps provider type strings to factory functions.
/// Similar to Go's `allProviders rwmap.RWMap[provider.OAuth2Provider, provider.Interface]`
///
/// Uses `parking_lot::RwLock` (non-poisoning) for consistency with the rest of the codebase.
/// Registration happens only during initialization and lookups are extremely fast.
///
/// Wrapped in `Arc` so it can be shared across services via dependency injection
/// rather than relying on a global static.
#[derive(Clone)]
pub struct ProviderRegistry {
    factories: Arc<parking_lot::RwLock<HashMap<String, ProviderFactory>>>,
}

impl ProviderRegistry {
    /// Create a new empty provider registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            factories: Arc::new(parking_lot::RwLock::new(HashMap::new())),
        }
    }

    /// Register a provider factory function.
    pub fn register(&self, provider_type: &str, factory: ProviderFactory) {
        let mut registry = self.factories.write();
        registry.insert(provider_type.to_string(), factory);
    }

    /// Get a registered factory function by type.
    #[must_use]
    pub fn get_factory(&self, provider_type: &str) -> Option<ProviderFactory> {
        let registry = self.factories.read();
        registry.get(provider_type).copied()
    }

    /// Create a provider instance with configuration.
    ///
    /// Looks up the factory function in the registry and calls it.
    pub fn create_provider(
        &self,
        provider_type: &str,
        config: &serde_json::Value,
    ) -> Result<Box<dyn Provider>, Error> {
        let factory = self.get_factory(provider_type).ok_or_else(|| {
            Error::InvalidInput(format!("Unknown provider type: {provider_type}"))
        })?;
        factory(config)
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.factories.read().len();
        f.debug_struct("ProviderRegistry")
            .field("registered_count", &count)
            .finish()
    }
}
