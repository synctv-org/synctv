//! OAuth2/OIDC provider system with registry and factory pattern
//!
//! # Architecture (similar to Go's synctv/internal/provider/providers)
//!
//! 1. **Provider Registry**: Map of provider type -> provider instance
//! 2. **Factory Pattern**: `create_provider()` looks up registry and clones with config
//! 3. **Decoupled**: Factory doesn't need to know about provider-specific configs
//! 4. **Clone Pattern**: Each provider implements Clone to create instances

pub mod config;
pub mod providers;

pub use config::ConfigLoader;
pub use providers::{GitHubConfig, GoogleConfig, LogtoConfig, OidcConfig};

use crate::Error;
use std::collections::HashMap;
use std::sync::LazyLock;
use async_trait::async_trait;

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
    async fn get_user_info(&self, code: &str, pkce_verifier: &str) -> Result<OAuth2UserInfo, Error>;
}

/// `OAuth2` user info from provider
#[derive(Debug, Clone)]
pub struct OAuth2UserInfo {
    pub provider_user_id: String,
    pub username: String,
    pub email: Option<String>,
    pub avatar: Option<String>,
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

/// Provider registry
///
/// Maps provider type strings to factory functions.
/// Similar to Go's `allProviders rwmap.RWMap[provider.OAuth2Provider, provider.Interface]`
///
/// Uses `parking_lot::RwLock` (non-poisoning) for consistency with the rest of the codebase.
/// Registration happens only during initialization and lookups are extremely fast.
static PROVIDER_REGISTRY: LazyLock<parking_lot::RwLock<HashMap<String, ProviderFactory>>> =
    LazyLock::new(|| parking_lot::RwLock::new(HashMap::new()));

/// Register an `OAuth2` provider factory function
///
/// Call this for each provider type during initialization.
/// Similar to Go's `RegisterProvider()` in providers.go
///
/// # Example
///
/// ```ignore
/// register_provider_factory("github", github_factory);
/// ```
pub fn register_provider_factory(provider_type: &str, factory: ProviderFactory) {
    let mut registry = PROVIDER_REGISTRY.write();
    registry.insert(provider_type.to_string(), factory);
}

/// Get a registered factory function by type
async fn get_provider_factory(provider_type: &str) -> Option<ProviderFactory> {
    let registry = PROVIDER_REGISTRY.read();
    registry.get(provider_type).copied()
}

// ============================================================================
// Factory Pattern
// ============================================================================

/// Create a provider instance with configuration
///
/// This is the factory function that creates fully-configured providers.
/// Similar to Go's `InitProvider()` in providers.go:
///
/// ```go
/// func InitProvider(p provider.OAuth2Provider, c provider.Oauth2Option) (provider.Interface, error) {
///     pi, ok := allProviders.Load(p)
///     if !ok { return nil, FormatNotImplementedError(p) }
///     pi.Init(c)
///     return pi, nil
/// }
/// ```
///
/// # Arguments
/// * `provider_type` - The type of provider ("github", "logto", "oidc", etc.)
/// * `config` - Full configuration including `client_id`, `client_secret`, `redirect_url`, etc.
///
/// # Example
///
/// ```ignore
/// // Create GitHub provider
/// let config = serde_json::from_str::<serde_json::Value>(json)?;
/// let github = create_provider("github", &config).await?;
///
/// // Create Logto provider with custom endpoint
/// let config = serde_json::from_str::<serde_json::Value>(json)?;
/// let logto = create_provider("logto", &config).await?;
/// ```
pub async fn create_provider(
    provider_type: &str,
    config: &serde_json::Value,
) -> Result<Box<dyn Provider>, Error> {
    // Look up factory function in registry
    let factory = get_provider_factory(provider_type).await
        .ok_or_else(|| Error::InvalidInput(format!("Unknown provider type: {provider_type}")))?;

    // Call factory function to create provider instance
    factory(config)
}
