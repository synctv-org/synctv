//! OAuth2/OIDC provider system with registry and factory pattern
//!
//! # Architecture (similar to Go's synctv/internal/provider/providers)
//!
//! 1. **Provider Registry**: Map of provider type -> provider instance
//! 2. **Factory Pattern**: `ProviderRegistry::create_provider()` looks up registry and clones with config
//! 3. **Decoupled**: Factory doesn't need to know about provider-specific configs
//! 4. **Clone Pattern**: Each provider implements Clone to create instances

pub mod providers;

pub use providers::{GitHubConfig, GoogleConfig, LogtoConfig, OidcConfig};

use crate::{service::OAuth2ProviderPrivateConfig, Error};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

// Provider Trait

/// Provider-generated authorization request context.
#[derive(Debug, Clone)]
pub struct OAuth2Authorization {
    pub auth_url: String,
    pub pkce_verifier: String,
    pub nonce: Option<String>,
}

impl OAuth2Authorization {
    #[must_use]
    pub fn new(auth_url: String, pkce_verifier: String) -> Self {
        Self {
            auth_url,
            pkce_verifier,
            nonce: None,
        }
    }

    #[must_use]
    pub fn with_nonce(mut self, nonce: String) -> Self {
        self.nonce = Some(nonce);
        self
    }
}

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
    /// Returns an authorization context containing the URL plus provider-generated
    /// values that must be stored and passed back during `get_user_info`.
    ///
    /// Similar to Go's `NewAuthURL()` method, extended with PKCE (RFC 7636).
    async fn new_auth_url(
        &self,
        state: &str,
        redirect_url: Option<&str>,
    ) -> Result<OAuth2Authorization, Error>;

    /// Exchange authorization code for user info, verifying the PKCE challenge
    ///
    /// This method:
    /// 1. Exchanges the code for an access token (with PKCE verifier)
    /// 2. Fetches user info using the token
    /// 3. Returns user info (token is discarded)
    ///
    /// Similar to Go's `GetUserInfo()` method, extended with PKCE (RFC 7636).
    async fn get_user_info(
        &self,
        code: &str,
        redirect_url: Option<&str>,
        pkce_verifier: &str,
        nonce: Option<&str>,
    ) -> Result<OAuth2UserInfo, Error>;
}

/// `OAuth2` user info from provider
#[derive(Debug, Clone)]
pub struct OAuth2UserInfo {
    pub provider_user_id: String,
    pub username: String,
    pub avatar: Option<String>,
}

// Provider Registry

/// Factory function type for creating providers
///
/// Each provider type registers a factory function that knows how to
/// create instances of that provider with configuration.
/// All parameters (`client_id`, `client_secret`, `redirect_url`, etc.) are in config.
pub type OAuth2ProviderFactory =
    Arc<dyn Fn(&OAuth2ProviderPrivateConfig) -> Result<Box<dyn Provider>, Error> + Send + Sync>;

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
    factories: Arc<parking_lot::RwLock<HashMap<String, OAuth2ProviderFactory>>>,
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
    pub fn register(&self, provider_type: &str, factory: OAuth2ProviderFactory) {
        let mut registry = self.factories.write();
        registry.insert(provider_type.to_string(), factory);
    }

    /// Get a registered factory function by type.
    #[must_use]
    pub fn get_factory(&self, provider_type: &str) -> Option<OAuth2ProviderFactory> {
        let registry = self.factories.read();
        registry.get(provider_type).cloned()
    }

    /// Create a provider instance with configuration.
    ///
    /// Looks up the factory function in the registry and calls it.
    pub fn create_provider(
        &self,
        provider_type: &str,
        config: &OAuth2ProviderPrivateConfig,
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
