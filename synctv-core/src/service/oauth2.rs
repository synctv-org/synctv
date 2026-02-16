//! OAuth2/OIDC authentication service
//!
//! This service handles OAuth2/OIDC login flow WITHOUT storing tokens.
//! Tokens are only used temporarily during login to fetch user info.
//!
//! ## State Storage
//! `OAuth2` states are stored in Redis when available (for multi-node deployments).
//! Falls back to in-memory storage when Redis is not configured.

use std::sync::Arc;
use std::collections::HashMap;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::{
    models::{oauth2_client::OAuth2Provider, UserId},
    repository::UserOAuthProviderRepository,
    oauth2::Provider as OAuth2ProviderTrait,
    Error, Result,
};

/// Redis key prefix for `OAuth2` states
const OAUTH2_STATE_KEY_PREFIX: &str = "oauth2:state:";
/// Default TTL for `OAuth2` states (5 minutes)
const OAUTH2_STATE_TTL_SECONDS: u64 = 300;
/// Maximum number of in-memory `OAuth2` states (prevents unbounded memory growth)
const MAX_LOCAL_STATES: usize = 10_000;

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
}

/// `OAuth2` user info from provider (service layer)
#[derive(Debug, Clone)]
pub struct OAuth2UserInfo {
    pub provider: OAuth2Provider,
    pub provider_user_id: String,
    pub username: String,
    pub email: Option<String>,
    pub avatar: Option<String>,
}

/// An OAuth2 provider entry combining the provider instance and its type
struct OAuth2ProviderEntry {
    provider: Box<dyn OAuth2ProviderTrait>,
    provider_type: OAuth2Provider,
}

/// `OAuth2` authentication service
///
/// Handles OAuth2/OIDC login flow:
/// 1. Generate authorization URL with PKCE
/// 2. Exchange authorization code for user info
/// 3. Create/update user-provider mapping (NO TOKENS STORED)
///
/// State storage:
/// - When Redis is available: states are stored in Redis with TTL (multi-node safe)
/// - When Redis is not available: states are stored in memory with TTL via moka cache (single-node only)
#[derive(Clone)]
pub struct OAuth2Service {
    repository: UserOAuthProviderRepository,
    /// Map of instance name -> (provider instance, provider enum type)
    /// M-03: Consolidated from separate providers + provider_types maps to prevent lock ordering issues
    providers: Arc<RwLock<HashMap<String, OAuth2ProviderEntry>>>,
    /// In-memory state storage with TTL (fallback when Redis is not available)
    local_states: Arc<moka::future::Cache<String, OAuth2State>>,
    /// Redis connection manager for distributed state storage (multi-replica mode)
    redis_conn: Option<redis::aio::ConnectionManager>,
    /// Allowlist of permitted redirect domains (empty = relative paths only)
    allowed_redirect_domains: Arc<Vec<String>>,
}

impl std::fmt::Debug for OAuth2Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuth2Service")
            .field("repository", &std::any::type_name::<UserOAuthProviderRepository>())
            .finish_non_exhaustive()
    }
}

impl OAuth2Service {
    /// Create new `OAuth2` service (without Redis - single node only)
    ///
    /// # Note
    /// If `redis_conn` is `None`, the service will use in-memory storage,
    /// which is only suitable for single-replica deployments.
    #[must_use]
    pub fn new(repository: UserOAuthProviderRepository) -> Self {
        warn!(
            "OAuth2 service using in-memory state storage. \
             This is only suitable for single-replica deployments. \
             For multi-replica setups, configure Redis via with_redis()."
        );
        let local_states = moka::future::Cache::builder()
            .max_capacity(MAX_LOCAL_STATES as u64)
            .time_to_live(Duration::from_secs(OAUTH2_STATE_TTL_SECONDS))
            .build();
        Self {
            repository,
            providers: Arc::new(RwLock::new(HashMap::new())),
            local_states: Arc::new(local_states),
            redis_conn: None,
            allowed_redirect_domains: Arc::new(Vec::new()),
        }
    }

    /// Create new `OAuth2` service with Redis `ConnectionManager` (multi-replica safe)
    ///
    /// Uses a persistent `ConnectionManager` instead of creating new connections
    /// per operation, matching the pattern used by `WsTicketService`.
    #[must_use]
    pub fn with_redis(repository: UserOAuthProviderRepository, redis_conn: redis::aio::ConnectionManager) -> Self {
        let local_states = moka::future::Cache::builder()
            .max_capacity(MAX_LOCAL_STATES as u64)
            .time_to_live(Duration::from_secs(OAUTH2_STATE_TTL_SECONDS))
            .build();
        Self {
            repository,
            providers: Arc::new(RwLock::new(HashMap::new())),
            local_states: Arc::new(local_states),
            redis_conn: Some(redis_conn),
            allowed_redirect_domains: Arc::new(Vec::new()),
        }
    }

    /// Set allowlist of permitted redirect domains
    ///
    /// When set, absolute redirect URLs are only accepted if their host matches
    /// one of the allowed domains. When empty, only relative paths are allowed.
    pub fn set_allowed_redirect_domains(&mut self, domains: Vec<String>) {
        self.allowed_redirect_domains = Arc::new(domains);
    }

    /// Store `OAuth2` state (Redis if available, otherwise local memory)
    async fn store_state(&self, state_token: &str, state: &OAuth2State) -> Result<()> {
        if let Some(ref conn) = self.redis_conn {
            let key = format!("{OAUTH2_STATE_KEY_PREFIX}{state_token}");
            let value = serde_json::to_string(state)
                .map_err(|e| Error::Internal(format!("Failed to serialize OAuth2 state: {e}")))?;

            let mut conn = conn.clone();

            use redis::AsyncCommands;
            let _: () = conn
                .set_ex(&key, value, OAUTH2_STATE_TTL_SECONDS)
                .await
                .map_err(|e| Error::Internal(format!("Failed to store OAuth2 state in Redis: {e}")))?;

            debug!("Stored OAuth2 state in Redis for token {}", &state_token[..8]);
        } else {
            // moka cache handles capacity and TTL automatically
            self.local_states.insert(state_token.to_string(), state.clone()).await;
            debug!("Stored OAuth2 state in memory for token {}", &state_token[..8]);
        }
        Ok(())
    }

    /// Retrieve and remove `OAuth2` state atomically (Redis if available, otherwise local memory)
    ///
    /// Uses a Lua script to GET and DEL atomically, preventing race conditions where
    /// two callbacks with the same state token could both succeed.
    async fn consume_state(&self, state_token: &str) -> Result<OAuth2State> {
        if let Some(ref conn) = self.redis_conn {
            let key = format!("{OAUTH2_STATE_KEY_PREFIX}{state_token}");
            let mut conn = conn.clone();

            // Atomic GET + DEL via Lua script (same pattern as WsTicketService)
            let lua_script = redis::Script::new(r#"
                local value = redis.call("GET", KEYS[1])
                if value then
                    redis.call("DEL", KEYS[1])
                end
                return value
            "#);

            let value: Option<String> = lua_script
                .key(&key)
                .invoke_async(&mut conn)
                .await
                .map_err(|e| Error::Internal(format!("Failed to consume OAuth2 state from Redis: {e}")))?;

            match value {
                Some(json) => {
                    let state: OAuth2State = serde_json::from_str(&json)
                        .map_err(|e| Error::Internal(format!("Failed to deserialize OAuth2 state: {e}")))?;
                    debug!("Retrieved OAuth2 state from Redis for token {}", &state_token[..8]);
                    Ok(state)
                }
                None => Err(Error::Authentication("Invalid or expired OAuth2 state".to_string())),
            }
        } else {
            // moka cache: remove returns the removed value if it existed
            self.local_states
                .remove(state_token)
                .await
                .ok_or_else(|| Error::Authentication("Invalid or expired OAuth2 state".to_string()))
        }
    }

    /// Register an `OAuth2` provider instance
    ///
    /// # Arguments
    /// * `instance_name` - Unique instance name (e.g., "github", "logto1", "logto2")
    /// * `provider_type` - Provider type enum
    /// * `provider` - The provider instance
    pub async fn register_provider(
        &self,
        instance_name: String,
        provider_type: OAuth2Provider,
        provider: Box<dyn OAuth2ProviderTrait>,
    ) {
        let mut providers = self.providers.write().await;

        info!("Registered OAuth2 provider: {} (type: {})", instance_name, provider_type.as_str());
        providers.insert(instance_name, OAuth2ProviderEntry { provider, provider_type });
    }

    /// Generate authorization URL with PKCE challenge
    pub async fn get_authorization_url(
        &self,
        instance_name: &str,
        redirect_url: Option<String>,
    ) -> Result<(String, String)> {
        // Validate redirect URL if provided
        if let Some(ref url) = redirect_url {
            Self::validate_redirect_url_with_allowlist(url, &self.allowed_redirect_domains)?;
        }

        let providers = self.providers.read().await;
        let entry = providers.get(instance_name)
            .ok_or_else(|| Error::InvalidInput(format!("OAuth2 provider instance not found: {instance_name}")))?;

        // Generate state token
        let state_token = nanoid::nanoid!(32);

        // Generate authorization URL with PKCE challenge
        let (auth_url, pkce_verifier) = entry.provider.new_auth_url(&state_token).await
            .map_err(|e| Error::Internal(format!("Failed to generate authorization URL: {e}")))?;

        // Store state (including PKCE verifier) for verification during callback
        let oauth_state = OAuth2State {
            instance_name: instance_name.to_string(),
            redirect_url,
            created_at: chrono::Utc::now(),
            bind_user_id: None,
            pkce_verifier,
        };

        self.store_state(&state_token, &oauth_state).await?;

        debug!(
            "Generated OAuth2 authorization URL for provider {}",
            instance_name
        );

        Ok((auth_url, state_token))
    }

    /// Generate authorization URL for bind flow (associates with an authenticated user)
    pub async fn get_authorization_url_with_user(
        &self,
        instance_name: &str,
        redirect_url: Option<String>,
        user_id: Option<UserId>,
    ) -> Result<(String, String)> {
        // Validate redirect URL if provided
        if let Some(ref url) = redirect_url {
            Self::validate_redirect_url_with_allowlist(url, &self.allowed_redirect_domains)?;
        }

        let providers = self.providers.read().await;
        let entry = providers.get(instance_name)
            .ok_or_else(|| Error::InvalidInput(format!("OAuth2 provider instance not found: {instance_name}")))?;

        // Generate state token
        let state_token = nanoid::nanoid!(32);

        // Generate authorization URL with PKCE challenge
        let (auth_url, pkce_verifier) = entry.provider.new_auth_url(&state_token).await
            .map_err(|e| Error::Internal(format!("Failed to generate authorization URL: {e}")))?;

        // Store state with user_id for bind flow (including PKCE verifier)
        let oauth_state = OAuth2State {
            instance_name: instance_name.to_string(),
            redirect_url,
            created_at: chrono::Utc::now(),
            bind_user_id: user_id,
            pkce_verifier,
        };

        self.store_state(&state_token, &oauth_state).await?;

        Ok((auth_url, state_token))
    }

    /// Validate redirect URL to prevent open redirect vulnerabilities (CWE-601)
    ///
    /// Only relative paths and URLs matching the configured allowed domains are accepted.
    fn validate_redirect_url_with_allowlist(url: &str, allowed_domains: &[String]) -> Result<()> {
        // Empty or whitespace-only URLs are rejected
        if url.trim().is_empty() {
            return Err(Error::InvalidInput("Redirect URL cannot be empty".to_string()));
        }

        // Allow relative paths (must start with '/')
        if url.starts_with('/') {
            // Reject URLs with '//' (protocol-relative URLs can be used for open redirect)
            if url.starts_with("//") {
                return Err(Error::InvalidInput(
                    "Protocol-relative URLs are not allowed for security reasons".to_string()
                ));
            }
            // Valid relative path
            return Ok(());
        }

        // For absolute URLs, parse and validate
        match url::Url::parse(url) {
            Ok(parsed_url) => {
                // Only allow http and https schemes
                let scheme = parsed_url.scheme();
                if scheme != "http" && scheme != "https" {
                    return Err(Error::InvalidInput(format!(
                        "Invalid URL scheme: {scheme}. Only http and https are allowed"
                    )));
                }

                // Reject URLs with authentication credentials (user:pass@host)
                if parsed_url.username() != "" || parsed_url.password().is_some() {
                    return Err(Error::InvalidInput(
                        "URLs with embedded credentials are not allowed".to_string()
                    ));
                }

                // Check against allowed domains allowlist
                let host = parsed_url.host_str().unwrap_or("");
                if allowed_domains.is_empty() {
                    return Err(Error::InvalidInput(
                        "Absolute redirect URLs are not allowed. Use a relative path instead.".to_string()
                    ));
                }
                if !allowed_domains.iter().any(|d| host == d || host.ends_with(&format!(".{d}"))) {
                    return Err(Error::InvalidInput(format!(
                        "Redirect URL domain '{host}' is not in the allowed domains list"
                    )));
                }

                Ok(())
            }
            Err(_) => {
                Err(Error::InvalidInput(format!(
                    "Invalid redirect URL format: {url}"
                )))
            }
        }
    }

    /// Verify `OAuth2` state during callback
    pub async fn verify_state(&self, state_token: &str) -> Result<OAuth2State> {
        self.consume_state(state_token).await
    }

    /// Exchange authorization code for user info with PKCE verification
    pub async fn exchange_code_for_user_info(
        &self,
        instance_name: &str,
        code: &str,
        pkce_verifier: &str,
    ) -> Result<(OAuth2UserInfo, OAuth2Provider)> {
        // M-03: Single lock acquisition instead of two separate locks
        let providers = self.providers.read().await;

        let entry = providers.get(instance_name)
            .ok_or_else(|| Error::InvalidInput(format!("OAuth2 provider instance not found: {instance_name}")))?;

        debug!("Exchanging code for user info from {}", instance_name);

        // Use provider to get user info (with PKCE verifier)
        let user_info = entry.provider.get_user_info(code, pkce_verifier).await
            .map_err(|e| Error::Internal(format!("Failed to get user info: {e}")))?;

        // Convert provider user info to service user info
        let service_user_info = OAuth2UserInfo {
            provider: entry.provider_type.clone(),
            provider_user_id: user_info.provider_user_id,
            username: user_info.username,
            email: user_info.email,
            avatar: user_info.avatar,
        };

        Ok((service_user_info, entry.provider_type.clone()))
    }

    /// Create or update user-OAuth2 provider mapping
    pub async fn upsert_user_provider(
        &self,
        user_id: &UserId,
        provider: &OAuth2Provider,
        provider_user_id: &str,
        user_info: &OAuth2UserInfo,
    ) -> Result<()> {
        // Convert service user info to repository format
        let repo_user_info = crate::models::oauth2_client::OAuth2UserInfo {
            provider: provider.clone(),
            provider_user_id: user_info.provider_user_id.clone(),
            username: user_info.username.clone(),
            email: user_info.email.clone(),
            avatar: user_info.avatar.clone(),
        };

        self.repository
            .upsert(user_id, provider, provider_user_id, &repo_user_info)
            .await
    }

    /// Find user by `OAuth2` provider
    pub async fn find_user_by_provider(
        &self,
        provider: &OAuth2Provider,
        provider_user_id: &str,
    ) -> Result<Option<UserId>> {
        match self
            .repository
            .find_by_provider(provider, provider_user_id)
            .await?
        {
            Some(mapping) => Ok(Some(mapping.user_id)),
            None => Ok(None),
        }
    }

    /// Get all `OAuth2` providers for a user
    pub async fn get_user_providers(&self, user_id: &UserId) -> Result<Vec<OAuth2Provider>> {
        let mappings = self.repository.find_by_user(user_id).await?;
        Ok(mappings
            .into_iter()
            .filter_map(|m| m.provider_enum())
            .collect())
    }

    /// List all configured `OAuth2` provider instances
    ///
    /// Returns a list of (`instance_name`, `provider_type`) pairs for all registered providers.
    /// This is used by the HTTP API to tell clients which `OAuth2` login options are available.
    /// Returns an empty vector if no providers are configured. Order is not guaranteed.
    pub async fn list_available_instances(&self) -> Vec<(String, OAuth2Provider)> {
        let providers = self.providers.read().await;
        providers
            .iter()
            .map(|(name, entry)| (name.clone(), entry.provider_type.clone()))
            .collect()
    }

    /// Unlink `OAuth2` provider from user
    pub async fn unlink_provider(
        &self,
        user_id: &UserId,
        provider: &OAuth2Provider,
        provider_user_id: &str,
    ) -> Result<bool> {
        self.repository
            .delete(user_id, provider, provider_user_id)
            .await
    }

    /// Unlink all bindings for a specific `OAuth2` provider from user
    pub async fn unlink_provider_all(
        &self,
        user_id: &UserId,
        provider: &OAuth2Provider,
    ) -> Result<bool> {
        self.repository.delete_by_user_and_provider(user_id, provider).await
    }

    /// Remove all OAuth2 provider mappings for a user.
    ///
    /// Used during user deletion to clean up all OAuth bindings.
    /// Returns the number of mappings removed.
    pub async fn delete_all_for_user(&self, user_id: &UserId) -> Result<u64> {
        self.repository.delete_all_for_user(user_id).await
    }

    /// Clean up expired `OAuth2` states (maintenance task)
    ///
    /// Note: Both Redis and moka cache handle TTL automatically.
    /// This method is now a no-op but kept for API compatibility.
    pub async fn cleanup_expired_states(&self, _max_age_seconds: i64) -> Result<()> {
        // moka cache handles TTL expiration automatically via time_to_live policy
        // Redis handles its own TTL via SETEX
        // This method is kept for API compatibility but is now a no-op
        Ok(())
    }

    /// Check if Redis is being used for state storage
    #[must_use]
    pub const fn uses_redis(&self) -> bool {
        self.redis_conn.is_some()
    }
}
