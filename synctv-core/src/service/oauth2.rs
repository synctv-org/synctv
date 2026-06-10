//! OAuth2/OIDC authentication service
//!
//! This service handles OAuth2/OIDC login flow WITHOUT storing tokens.
//! Tokens are only used temporarily during login to fetch user info.
//!
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use synctv_common::ExecutionControl;
use tokio::sync::RwLock;

use crate::{
    models::{oauth2_client::OAuth2Provider, UserId},
    oauth2::Provider as OAuth2ProviderTrait,
    repository::UserOAuthProviderRepository,
    service::{OAuth2SignupPolicy, SettingsRegistry},
    Result,
};

mod authorization;
mod constructor;
mod exchange;
mod linking;
mod mappings;
mod providers;
mod state_store;
pub(crate) use state_store::state_store_from_shared_state_profile;
pub use state_store::{local_oauth_state_store, OAuthStateStore, RedisOAuthStateStore};

/// Default TTL for `OAuth2` states (5 minutes)
const OAUTH2_STATE_TTL_SECONDS: u64 = 300;
const OAUTH2_STATE_TTL_SECONDS_I64: i64 = 300;

/// `OAuth2` state (for CSRF protection and PKCE during authorization flow)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuth2State {
    pub instance_name: String,
    pub redirect_url: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// User ID for bind flow (None for login flow)
    pub bind_user_id: Option<UserId>,
    /// PKCE code verifier (RFC 7636) - stored server-side, sent during token exchange
    pub pkce_verifier: String,
    /// Provider nonce for OIDC ID Token replay protection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
}

/// `OAuth2` user info from provider (service layer)
#[derive(Debug, Clone)]
pub struct OAuth2UserInfo {
    pub provider: OAuth2Provider,
    pub provider_instance_name: String,
    pub provider_issuer: Option<String>,
    pub provider_user_id: String,
    pub username: String,
    pub email: Option<String>,
    pub avatar: Option<String>,
    /// Whether the provider has verified the user's email address
    pub email_verified: bool,
}

#[derive(Debug, Clone)]
pub struct OAuth2PendingRegistration {
    pub request_id: UserId,
}

pub struct PreparedOAuth2Authorization {
    pub auth_url: String,
    pub state_token: String,
    pub(super) oauth_state: OAuth2State,
}

#[derive(Debug, Clone)]
pub enum OAuth2LinkResult {
    Linked { user_id: UserId, is_new: bool },
    PendingReview(OAuth2PendingRegistration),
}

/// An `OAuth2` provider entry combining the provider instance and its type
///
/// The provider is stored as `Arc<dyn>` rather than `Box<dyn>` so that callers
/// can clone the `Arc` while holding the read lock and then drop the lock before
/// invoking any async methods on the provider (TOCTOU race fix).
#[derive(Clone)]
pub(super) struct OAuth2ProviderEntry {
    provider: Arc<dyn OAuth2ProviderTrait>,
    provider_type: OAuth2Provider,
    signup_policy: OAuth2SignupPolicy,
}

// OAuth2Service

/// `OAuth2` authentication service
///
/// Handles OAuth2/OIDC login flow:
/// 1. Generate authorization URL with PKCE
/// 2. Exchange authorization code for user info
/// 3. Create/update user-provider mapping (NO TOKENS STORED)
///
/// State storage is delegated to the [`OAuthStateStore`] trait. Inject a
/// shared single-use store for clustered deployments and a local-only
/// implementation for standalone/test environments.
#[derive(Clone)]
pub struct OAuth2Service {
    repository: Option<UserOAuthProviderRepository>,
    /// Map of instance name -> (provider instance, provider enum type).
    /// Consolidated from separate providers and `provider_types` maps to
    /// prevent lock ordering issues.
    providers: Arc<RwLock<HashMap<String, OAuth2ProviderEntry>>>,
    /// State storage backend injected via trait object.
    state_store: Arc<dyn OAuthStateStore>,
    /// Factory registry used to build providers without relying on global state.
    provider_registry: crate::oauth2::ProviderRegistry,
    /// Runtime SSRF policy used when validating dynamic provider settings.
    ssrf_guard: synctv_common::ssrf::SsrfGuard,
    /// Allowlist of permitted non-loopback redirect domains.
    allowed_redirect_domains: Arc<Vec<String>>,
    settings_registry: Option<Arc<SettingsRegistry>>,
    providers_fingerprint: Arc<RwLock<Option<String>>>,
}

#[derive(Clone, Default)]
pub struct OAuth2ServiceRuntime {
    pub allowed_redirect_domains: Vec<String>,
    pub settings_registry: Option<Arc<SettingsRegistry>>,
}

impl std::fmt::Debug for OAuth2Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuth2Service")
            .field(
                "repository",
                &std::any::type_name::<UserOAuthProviderRepository>(),
            )
            .finish_non_exhaustive()
    }
}

impl OAuth2Service {
    pub(crate) fn repository(&self) -> Result<&UserOAuthProviderRepository> {
        self.repository.as_ref().ok_or_else(|| {
            crate::Error::ServiceUnavailable(
                "OAuth2 user-provider repository is not configured".to_string(),
            )
        })
    }

    /// Verify `OAuth2` state during callback
    pub async fn verify_state(&self, state_token: &str) -> Result<OAuth2State> {
        self.verify_state_with_control(state_token, None).await
    }

    pub async fn verify_state_with_control(
        &self,
        state_token: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<OAuth2State> {
        self.consume_state_with_control(state_token, control).await
    }
}

#[cfg(test)]
mod tests;
