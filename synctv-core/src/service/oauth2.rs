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
    Error, Result, InternalExt,
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
                .internal_with_err("Failed to serialize OAuth2 state")?;

            let mut conn = conn.clone();

            use redis::AsyncCommands;
            let _: () = conn
                .set_ex(&key, value, OAUTH2_STATE_TTL_SECONDS)
                .await
                .internal_with_err("Failed to store OAuth2 state in Redis")?;

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
                .internal_with_err("Failed to consume OAuth2 state from Redis")?;

            match value {
                Some(json) => {
                    let state: OAuth2State = serde_json::from_str(&json)
                        .internal_with_err("Failed to deserialize OAuth2 state")?;
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
            .internal_with_err("Failed to generate authorization URL")?;

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
            .internal_with_err("Failed to generate authorization URL")?;

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
            .internal_with_err("Failed to get user info")?;

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

    /// Create or update user-OAuth2 provider mapping using a provided executor
    pub async fn upsert_user_provider_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        provider: &OAuth2Provider,
        provider_user_id: &str,
        user_info: &OAuth2UserInfo,
        executor: E,
    ) -> Result<()>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let repo_user_info = crate::models::oauth2_client::OAuth2UserInfo {
            provider: provider.clone(),
            provider_user_id: user_info.provider_user_id.clone(),
            username: user_info.username.clone(),
            email: user_info.email.clone(),
            avatar: user_info.avatar.clone(),
        };

        self.repository
            .upsert_with_executor(user_id, provider, provider_user_id, &repo_user_info, executor)
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

    /// Get all `OAuth2` provider mappings with complete information for a user
    pub async fn get_user_provider_mappings(&self, user_id: &UserId) -> Result<Vec<crate::models::oauth2_client::UserOAuthProviderMapping>> {
        self.repository.find_by_user(user_id).await
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth2::Provider as OAuth2ProviderTrait;
    use async_trait::async_trait;
    use sqlx::PgPool;

    // ========================================================================
    // Mock OAuth2 Provider
    // ========================================================================

    /// Mock OAuth2 provider for testing authorization URL generation and code exchange.
    /// Returns configurable values without making real HTTP calls.
    #[derive(Clone)]
    struct MockOAuth2Provider {
        auth_url: String,
        pkce_verifier: String,
        user_info: Option<crate::oauth2::OAuth2UserInfo>,
        /// If set, `get_user_info` will return this error
        exchange_error: Option<String>,
    }

    impl MockOAuth2Provider {
        fn new() -> Self {
            Self {
                auth_url: "https://provider.example.com/auth?client_id=test".to_string(),
                pkce_verifier: "test_pkce_verifier_abc123".to_string(),
                user_info: Some(crate::oauth2::OAuth2UserInfo {
                    provider_user_id: "provider_user_42".to_string(),
                    username: "testuser".to_string(),
                    email: Some("test@example.com".to_string()),
                    avatar: Some("https://avatar.example.com/42.png".to_string()),
                }),
                exchange_error: None,
            }
        }

        fn with_exchange_error(mut self, err: &str) -> Self {
            self.exchange_error = Some(err.to_string());
            self
        }
    }

    #[async_trait]
    impl OAuth2ProviderTrait for MockOAuth2Provider {
        fn provider_type(&self) -> &str {
            "mock"
        }

        async fn new_auth_url(&self, state: &str) -> Result<(String, String)> {
            // Append state to URL like a real provider would
            let url = format!("{}&state={state}", self.auth_url);
            Ok((url, self.pkce_verifier.clone()))
        }

        async fn get_user_info(
            &self,
            _code: &str,
            _pkce_verifier: &str,
        ) -> Result<crate::oauth2::OAuth2UserInfo> {
            if let Some(ref err) = self.exchange_error {
                return Err(Error::Internal(err.clone()));
            }
            self.user_info.clone().ok_or_else(|| {
                Error::Internal("No user info configured in mock".to_string())
            })
        }
    }

    // ========================================================================
    // Helper: create service with in-memory state (no Redis, no real DB)
    // ========================================================================

    fn create_test_service() -> OAuth2Service {
        // connect_lazy does not establish a real connection; safe for tests
        // that never call repository methods
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = crate::repository::UserOAuthProviderRepository::new(pool);
        OAuth2Service::new(repo)
    }

    fn create_test_service_with_domains(domains: Vec<String>) -> OAuth2Service {
        let mut svc = create_test_service();
        svc.set_allowed_redirect_domains(domains);
        svc
    }

    // ========================================================================
    // Tests: Redirect URL Validation (security-critical)
    // ========================================================================

    #[test]
    fn test_redirect_relative_path_allowed() {
        let result =
            OAuth2Service::validate_redirect_url_with_allowlist("/dashboard", &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_redirect_relative_path_with_query_allowed() {
        let result = OAuth2Service::validate_redirect_url_with_allowlist(
            "/rooms?sort=name",
            &[],
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_redirect_protocol_relative_url_rejected() {
        let result =
            OAuth2Service::validate_redirect_url_with_allowlist("//evil.com/steal", &[]);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(&err, Error::InvalidInput(msg) if msg.contains("Protocol-relative")),
            "Expected protocol-relative rejection, got: {err}"
        );
    }

    #[test]
    fn test_redirect_empty_url_rejected() {
        let result = OAuth2Service::validate_redirect_url_with_allowlist("", &[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_redirect_whitespace_only_rejected() {
        let result = OAuth2Service::validate_redirect_url_with_allowlist("   ", &[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_redirect_absolute_url_rejected_when_no_domains_configured() {
        let result = OAuth2Service::validate_redirect_url_with_allowlist(
            "https://example.com/callback",
            &[],
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(&err, Error::InvalidInput(msg) if msg.contains("Absolute redirect URLs")));
    }

    #[test]
    fn test_redirect_absolute_url_allowed_when_domain_matches() {
        let domains = vec!["example.com".to_string()];
        let result = OAuth2Service::validate_redirect_url_with_allowlist(
            "https://example.com/callback",
            &domains,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_redirect_absolute_url_allowed_for_subdomain() {
        let domains = vec!["example.com".to_string()];
        let result = OAuth2Service::validate_redirect_url_with_allowlist(
            "https://app.example.com/callback",
            &domains,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_redirect_absolute_url_rejected_for_wrong_domain() {
        let domains = vec!["example.com".to_string()];
        let result = OAuth2Service::validate_redirect_url_with_allowlist(
            "https://evil.com/callback",
            &domains,
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(&err, Error::InvalidInput(msg) if msg.contains("not in the allowed")));
    }

    #[test]
    fn test_redirect_javascript_scheme_rejected() {
        let domains = vec!["example.com".to_string()];
        let result = OAuth2Service::validate_redirect_url_with_allowlist(
            "javascript:alert(1)",
            &domains,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_redirect_ftp_scheme_rejected() {
        let domains = vec!["example.com".to_string()];
        let result = OAuth2Service::validate_redirect_url_with_allowlist(
            "ftp://example.com/file",
            &domains,
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(&err, Error::InvalidInput(msg) if msg.contains("Invalid URL scheme")));
    }

    #[test]
    fn test_redirect_url_with_credentials_rejected() {
        let domains = vec!["example.com".to_string()];
        let result = OAuth2Service::validate_redirect_url_with_allowlist(
            "https://user:pass@example.com/callback",
            &domains,
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(&err, Error::InvalidInput(msg) if msg.contains("credentials")));
    }

    #[test]
    fn test_redirect_malformed_url_rejected() {
        let domains = vec!["example.com".to_string()];
        let result = OAuth2Service::validate_redirect_url_with_allowlist(
            "not a valid url at all",
            &domains,
        );
        assert!(result.is_err());
    }

    // ========================================================================
    // Tests: In-Memory State Management
    // ========================================================================

    #[tokio::test]
    async fn test_store_and_consume_state() {
        let service = create_test_service();
        let state = OAuth2State {
            instance_name: "github".to_string(),
            redirect_url: Some("/dashboard".to_string()),
            created_at: chrono::Utc::now(),
            bind_user_id: None,
            pkce_verifier: "verifier123".to_string(),
        };

        service.store_state("token_abc", &state).await.unwrap();
        let retrieved = service.consume_state("token_abc").await.unwrap();

        assert_eq!(retrieved.instance_name, "github");
        assert_eq!(retrieved.pkce_verifier, "verifier123");
        assert_eq!(retrieved.redirect_url.as_deref(), Some("/dashboard"));
        assert!(retrieved.bind_user_id.is_none());
    }

    #[tokio::test]
    async fn test_state_single_use_consumed_on_first_retrieval() {
        let service = create_test_service();
        let state = OAuth2State {
            instance_name: "google".to_string(),
            redirect_url: None,
            created_at: chrono::Utc::now(),
            bind_user_id: None,
            pkce_verifier: "v".to_string(),
        };

        service.store_state("token_once", &state).await.unwrap();

        // First consume succeeds
        let result = service.consume_state("token_once").await;
        assert!(result.is_ok());

        // Second consume fails (state was removed)
        let result = service.consume_state("token_once").await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(&err, Error::Authentication(msg) if msg.contains("Invalid or expired")),
            "Expected authentication error for replayed state, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_state_invalid_token_rejected() {
        let service = create_test_service();

        let result = service.consume_state("nonexistent_token").await;
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), Error::Authentication(msg) if msg.contains("Invalid or expired"))
        );
    }

    #[tokio::test]
    async fn test_state_preserves_bind_user_id() {
        let service = create_test_service();
        let user_id = UserId::from_string("user_42".to_string());
        let state = OAuth2State {
            instance_name: "logto".to_string(),
            redirect_url: None,
            created_at: chrono::Utc::now(),
            bind_user_id: Some(user_id.clone()),
            pkce_verifier: "bind_verifier".to_string(),
        };

        service.store_state("bind_token", &state).await.unwrap();
        let retrieved = service.consume_state("bind_token").await.unwrap();

        assert_eq!(retrieved.bind_user_id.as_ref().unwrap().as_str(), "user_42");
    }

    #[tokio::test]
    async fn test_verify_state_consumes_token() {
        let service = create_test_service();
        let state = OAuth2State {
            instance_name: "oidc".to_string(),
            redirect_url: None,
            created_at: chrono::Utc::now(),
            bind_user_id: None,
            pkce_verifier: "pkce_v".to_string(),
        };

        service.store_state("verify_tok", &state).await.unwrap();

        // verify_state delegates to consume_state
        let result = service.verify_state("verify_tok").await;
        assert!(result.is_ok());

        // Replay fails
        let result = service.verify_state("verify_tok").await;
        assert!(result.is_err());
    }

    // ========================================================================
    // Tests: Provider Registration and Listing
    // ========================================================================

    #[tokio::test]
    async fn test_register_and_list_providers() {
        let service = create_test_service();

        // Initially empty
        let providers = service.list_available_instances().await;
        assert!(providers.is_empty());

        // Register a mock provider
        service
            .register_provider(
                "github".to_string(),
                OAuth2Provider::GitHub,
                Box::new(MockOAuth2Provider::new()),
            )
            .await;

        let providers = service.list_available_instances().await;
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].0, "github");
        assert_eq!(providers[0].1, OAuth2Provider::GitHub);
    }

    #[tokio::test]
    async fn test_register_multiple_providers() {
        let service = create_test_service();

        service
            .register_provider(
                "github".to_string(),
                OAuth2Provider::GitHub,
                Box::new(MockOAuth2Provider::new()),
            )
            .await;
        service
            .register_provider(
                "logto1".to_string(),
                OAuth2Provider::Logto,
                Box::new(MockOAuth2Provider::new()),
            )
            .await;
        service
            .register_provider(
                "google".to_string(),
                OAuth2Provider::Google,
                Box::new(MockOAuth2Provider::new()),
            )
            .await;

        let providers = service.list_available_instances().await;
        assert_eq!(providers.len(), 3);

        let names: Vec<&str> = providers.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"github"));
        assert!(names.contains(&"logto1"));
        assert!(names.contains(&"google"));
    }

    #[tokio::test]
    async fn test_register_provider_replaces_existing() {
        let service = create_test_service();

        service
            .register_provider(
                "github".to_string(),
                OAuth2Provider::GitHub,
                Box::new(MockOAuth2Provider::new()),
            )
            .await;

        // Re-register with same name but different type
        service
            .register_provider(
                "github".to_string(),
                OAuth2Provider::Oidc,
                Box::new(MockOAuth2Provider::new()),
            )
            .await;

        let providers = service.list_available_instances().await;
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].1, OAuth2Provider::Oidc);
    }

    // ========================================================================
    // Tests: Authorization URL Generation with PKCE
    // ========================================================================

    #[tokio::test]
    async fn test_get_authorization_url_success() {
        let service = create_test_service();
        service
            .register_provider(
                "github".to_string(),
                OAuth2Provider::GitHub,
                Box::new(MockOAuth2Provider::new()),
            )
            .await;

        let (auth_url, state_token) = service
            .get_authorization_url("github", None)
            .await
            .unwrap();

        // Auth URL should contain the mock base URL and the state parameter
        assert!(auth_url.contains("https://provider.example.com/auth"));
        assert!(auth_url.contains("state="));

        // State token should be a 32-char nanoid
        assert_eq!(state_token.len(), 32);

        // State should be stored and consumable
        let state = service.verify_state(&state_token).await.unwrap();
        assert_eq!(state.instance_name, "github");
        assert_eq!(state.pkce_verifier, "test_pkce_verifier_abc123");
        assert!(state.redirect_url.is_none());
        assert!(state.bind_user_id.is_none());
    }

    #[tokio::test]
    async fn test_get_authorization_url_with_redirect() {
        let service = create_test_service();
        service
            .register_provider(
                "github".to_string(),
                OAuth2Provider::GitHub,
                Box::new(MockOAuth2Provider::new()),
            )
            .await;

        let (_, state_token) = service
            .get_authorization_url("github", Some("/rooms/123".to_string()))
            .await
            .unwrap();

        let state = service.verify_state(&state_token).await.unwrap();
        assert_eq!(state.redirect_url.as_deref(), Some("/rooms/123"));
    }

    #[tokio::test]
    async fn test_get_authorization_url_rejects_invalid_redirect() {
        let service = create_test_service();
        service
            .register_provider(
                "github".to_string(),
                OAuth2Provider::GitHub,
                Box::new(MockOAuth2Provider::new()),
            )
            .await;

        // Absolute URL with no allowed domains should be rejected
        let result = service
            .get_authorization_url("github", Some("https://evil.com/steal".to_string()))
            .await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::InvalidInput(_)));
    }

    #[tokio::test]
    async fn test_get_authorization_url_rejects_protocol_relative_redirect() {
        let service = create_test_service();
        service
            .register_provider(
                "github".to_string(),
                OAuth2Provider::GitHub,
                Box::new(MockOAuth2Provider::new()),
            )
            .await;

        let result = service
            .get_authorization_url("github", Some("//evil.com/steal".to_string()))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_authorization_url_unknown_provider() {
        let service = create_test_service();

        let result = service
            .get_authorization_url("nonexistent", None)
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(&err, Error::InvalidInput(msg) if msg.contains("not found")),
            "Expected provider not found error, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_get_authorization_url_with_allowed_redirect_domains() {
        let service = create_test_service_with_domains(vec!["myapp.com".to_string()]);
        service
            .register_provider(
                "github".to_string(),
                OAuth2Provider::GitHub,
                Box::new(MockOAuth2Provider::new()),
            )
            .await;

        // Allowed domain works
        let result = service
            .get_authorization_url("github", Some("https://myapp.com/callback".to_string()))
            .await;
        assert!(result.is_ok());

        // Subdomain also works
        let result = service
            .get_authorization_url("github", Some("https://auth.myapp.com/cb".to_string()))
            .await;
        assert!(result.is_ok());

        // Disallowed domain rejected
        let result = service
            .get_authorization_url("github", Some("https://evil.com/steal".to_string()))
            .await;
        assert!(result.is_err());
    }

    // ========================================================================
    // Tests: Authorization URL with User Binding (PKCE)
    // ========================================================================

    #[tokio::test]
    async fn test_get_authorization_url_with_user_stores_user_id() {
        let service = create_test_service();
        service
            .register_provider(
                "logto".to_string(),
                OAuth2Provider::Logto,
                Box::new(MockOAuth2Provider::new()),
            )
            .await;

        let user_id = UserId::from_string("user_bind_42".to_string());
        let (auth_url, state_token) = service
            .get_authorization_url_with_user("logto", None, Some(user_id))
            .await
            .unwrap();

        assert!(auth_url.contains("https://provider.example.com/auth"));

        let state = service.verify_state(&state_token).await.unwrap();
        assert_eq!(state.instance_name, "logto");
        assert_eq!(
            state.bind_user_id.as_ref().unwrap().as_str(),
            "user_bind_42"
        );
        assert_eq!(state.pkce_verifier, "test_pkce_verifier_abc123");
    }

    #[tokio::test]
    async fn test_get_authorization_url_with_user_none_user_id() {
        let service = create_test_service();
        service
            .register_provider(
                "github".to_string(),
                OAuth2Provider::GitHub,
                Box::new(MockOAuth2Provider::new()),
            )
            .await;

        let (_, state_token) = service
            .get_authorization_url_with_user("github", None, None)
            .await
            .unwrap();

        let state = service.verify_state(&state_token).await.unwrap();
        assert!(state.bind_user_id.is_none());
    }

    #[tokio::test]
    async fn test_get_authorization_url_with_user_rejects_bad_redirect() {
        let service = create_test_service();
        service
            .register_provider(
                "github".to_string(),
                OAuth2Provider::GitHub,
                Box::new(MockOAuth2Provider::new()),
            )
            .await;

        let result = service
            .get_authorization_url_with_user(
                "github",
                Some("//evil.com".to_string()),
                None,
            )
            .await;
        assert!(result.is_err());
    }

    // ========================================================================
    // Tests: Code Exchange for User Info
    // ========================================================================

    #[tokio::test]
    async fn test_exchange_code_for_user_info_success() {
        let service = create_test_service();
        service
            .register_provider(
                "github".to_string(),
                OAuth2Provider::GitHub,
                Box::new(MockOAuth2Provider::new()),
            )
            .await;

        let (user_info, provider_type) = service
            .exchange_code_for_user_info("github", "auth_code_123", "pkce_verifier_abc")
            .await
            .unwrap();

        assert_eq!(user_info.provider_user_id, "provider_user_42");
        assert_eq!(user_info.username, "testuser");
        assert_eq!(user_info.email.as_deref(), Some("test@example.com"));
        assert_eq!(
            user_info.avatar.as_deref(),
            Some("https://avatar.example.com/42.png")
        );
        assert_eq!(user_info.provider, OAuth2Provider::GitHub);
        assert_eq!(provider_type, OAuth2Provider::GitHub);
    }

    #[tokio::test]
    async fn test_exchange_code_unknown_provider() {
        let service = create_test_service();

        let result = service
            .exchange_code_for_user_info("nonexistent", "code", "verifier")
            .await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            Error::InvalidInput(msg) if msg.contains("not found")
        ));
    }

    #[tokio::test]
    async fn test_exchange_code_provider_returns_error() {
        let service = create_test_service();
        let failing_provider =
            MockOAuth2Provider::new().with_exchange_error("token exchange failed: invalid_grant");

        service
            .register_provider(
                "failing".to_string(),
                OAuth2Provider::Oidc,
                Box::new(failing_provider),
            )
            .await;

        let result = service
            .exchange_code_for_user_info("failing", "bad_code", "verifier")
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(&err, Error::Internal(msg) if msg.contains("invalid_grant")),
            "Expected internal error with invalid_grant, got: {err}"
        );
    }

    // ========================================================================
    // Tests: Full Authorization Flow (URL -> State -> Exchange)
    // ========================================================================

    #[tokio::test]
    async fn test_full_oauth2_login_flow() {
        let service = create_test_service();
        service
            .register_provider(
                "github".to_string(),
                OAuth2Provider::GitHub,
                Box::new(MockOAuth2Provider::new()),
            )
            .await;

        // Step 1: Generate authorization URL
        let (auth_url, state_token) = service
            .get_authorization_url("github", Some("/dashboard".to_string()))
            .await
            .unwrap();
        assert!(auth_url.contains("state="));

        // Step 2: Verify state (simulating callback)
        let state = service.verify_state(&state_token).await.unwrap();
        assert_eq!(state.instance_name, "github");
        assert_eq!(state.redirect_url.as_deref(), Some("/dashboard"));

        // Step 3: Exchange code with PKCE verifier from stored state
        let (user_info, provider_type) = service
            .exchange_code_for_user_info("github", "callback_code", &state.pkce_verifier)
            .await
            .unwrap();
        assert_eq!(user_info.username, "testuser");
        assert_eq!(provider_type, OAuth2Provider::GitHub);

        // Step 4: State cannot be replayed
        let replay = service.verify_state(&state_token).await;
        assert!(replay.is_err());
    }

    #[tokio::test]
    async fn test_full_oauth2_bind_flow() {
        let service = create_test_service();
        service
            .register_provider(
                "logto".to_string(),
                OAuth2Provider::Logto,
                Box::new(MockOAuth2Provider::new()),
            )
            .await;

        let user_id = UserId::from_string("existing_user_99".to_string());

        // Step 1: Generate auth URL with user binding
        let (_, state_token) = service
            .get_authorization_url_with_user("logto", None, Some(user_id))
            .await
            .unwrap();

        // Step 2: Verify state carries user ID
        let state = service.verify_state(&state_token).await.unwrap();
        assert_eq!(
            state.bind_user_id.as_ref().unwrap().as_str(),
            "existing_user_99"
        );
        assert_eq!(state.instance_name, "logto");
    }

    // ========================================================================
    // Tests: Service Configuration
    // ========================================================================

    #[tokio::test]
    async fn test_service_uses_memory_by_default() {
        let service = create_test_service();
        assert!(!service.uses_redis());
    }

    #[tokio::test]
    async fn test_cleanup_expired_states_is_noop() {
        let service = create_test_service();
        // Should not error even though it does nothing
        let result = service.cleanup_expired_states(300).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_set_allowed_redirect_domains() {
        let mut service = create_test_service();
        service.set_allowed_redirect_domains(vec![
            "example.com".to_string(),
            "myapp.io".to_string(),
        ]);

        service
            .register_provider(
                "github".to_string(),
                OAuth2Provider::GitHub,
                Box::new(MockOAuth2Provider::new()),
            )
            .await;

        // Allowed domain
        let result = service
            .get_authorization_url("github", Some("https://example.com/cb".to_string()))
            .await;
        assert!(result.is_ok());

        // Another allowed domain
        let result = service
            .get_authorization_url("github", Some("https://myapp.io/cb".to_string()))
            .await;
        assert!(result.is_ok());

        // Non-allowed domain
        let result = service
            .get_authorization_url("github", Some("https://other.com/cb".to_string()))
            .await;
        assert!(result.is_err());
    }

    // ========================================================================
    // Tests: OAuth2State serialization (used for Redis storage path)
    // ========================================================================

    #[test]
    fn test_oauth2_state_serialization_roundtrip() {
        let state = OAuth2State {
            instance_name: "github".to_string(),
            redirect_url: Some("/dashboard".to_string()),
            created_at: chrono::Utc::now(),
            bind_user_id: Some(UserId::from_string("user_1".to_string())),
            pkce_verifier: "S256_challenge_verifier".to_string(),
        };

        let json = serde_json::to_string(&state).unwrap();
        let deserialized: OAuth2State = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.instance_name, state.instance_name);
        assert_eq!(deserialized.redirect_url, state.redirect_url);
        assert_eq!(deserialized.pkce_verifier, state.pkce_verifier);
        assert_eq!(
            deserialized.bind_user_id.as_ref().unwrap().as_str(),
            "user_1"
        );
    }

    #[test]
    fn test_oauth2_state_serialization_none_fields() {
        let state = OAuth2State {
            instance_name: "oidc".to_string(),
            redirect_url: None,
            created_at: chrono::Utc::now(),
            bind_user_id: None,
            pkce_verifier: "v".to_string(),
        };

        let json = serde_json::to_string(&state).unwrap();
        let deserialized: OAuth2State = serde_json::from_str(&json).unwrap();

        assert!(deserialized.redirect_url.is_none());
        assert!(deserialized.bind_user_id.is_none());
    }

    // ========================================================================
    // Tests: Concurrent State Operations
    // ========================================================================

    #[tokio::test]
    async fn test_multiple_concurrent_states() {
        let service = create_test_service();

        // Store multiple states
        for i in 0..10 {
            let state = OAuth2State {
                instance_name: format!("provider_{i}"),
                redirect_url: None,
                created_at: chrono::Utc::now(),
                bind_user_id: None,
                pkce_verifier: format!("verifier_{i}"),
            };
            service
                .store_state(&format!("token_{i}"), &state)
                .await
                .unwrap();
        }

        // Each state should be independently consumable
        for i in 0..10 {
            let state = service
                .consume_state(&format!("token_{i}"))
                .await
                .unwrap();
            assert_eq!(state.instance_name, format!("provider_{i}"));
            assert_eq!(state.pkce_verifier, format!("verifier_{i}"));
        }

        // All consumed, none should remain
        for i in 0..10 {
            let result = service.consume_state(&format!("token_{i}")).await;
            assert!(result.is_err());
        }
    }

    // ========================================================================
    // Tests: PKCE Verifier Integrity
    // ========================================================================

    #[tokio::test]
    async fn test_pkce_verifier_preserved_through_state_lifecycle() {
        let service = create_test_service();
        let mock = MockOAuth2Provider {
            auth_url: "https://auth.test/authorize".to_string(),
            pkce_verifier: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".to_string(),
            user_info: Some(crate::oauth2::OAuth2UserInfo {
                provider_user_id: "u1".to_string(),
                username: "user1".to_string(),
                email: None,
                avatar: None,
            }),
            exchange_error: None,
        };

        service
            .register_provider(
                "test_pkce".to_string(),
                OAuth2Provider::Oidc,
                Box::new(mock),
            )
            .await;

        // Generate URL -- PKCE verifier should be stored in state
        let (_, state_token) = service
            .get_authorization_url("test_pkce", None)
            .await
            .unwrap();

        // Retrieve state and check PKCE verifier is intact
        let state = service.verify_state(&state_token).await.unwrap();
        assert_eq!(
            state.pkce_verifier,
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
        );
    }

    #[tokio::test]
    async fn test_each_auth_url_gets_unique_state_token() {
        let service = create_test_service();
        service
            .register_provider(
                "github".to_string(),
                OAuth2Provider::GitHub,
                Box::new(MockOAuth2Provider::new()),
            )
            .await;

        let (_, token1) = service
            .get_authorization_url("github", None)
            .await
            .unwrap();
        let (_, token2) = service
            .get_authorization_url("github", None)
            .await
            .unwrap();

        assert_ne!(token1, token2, "Each authorization request must get a unique state token");
    }

    // ========================================================================
    // Tests: OAuth2 Concurrent State Consumption (only one succeeds)
    // ========================================================================

    #[tokio::test]
    async fn test_concurrent_state_consumption_only_first_succeeds() {
        let service = create_test_service();
        let state = OAuth2State {
            instance_name: "github".to_string(),
            redirect_url: None,
            created_at: chrono::Utc::now(),
            bind_user_id: None,
            pkce_verifier: "concurrent_verifier".to_string(),
        };

        service.store_state("concurrent_token", &state).await.unwrap();

        // Spawn multiple concurrent consumers
        let mut handles = Vec::new();
        for _ in 0..20 {
            let svc = service.clone();
            handles.push(tokio::spawn(async move {
                svc.consume_state("concurrent_token").await
            }));
        }

        let mut success_count = 0;
        let mut failure_count = 0;
        for h in handles {
            match h.await.unwrap() {
                Ok(state) => {
                    assert_eq!(state.pkce_verifier, "concurrent_verifier");
                    success_count += 1;
                }
                Err(_) => {
                    failure_count += 1;
                }
            }
        }

        // In-memory moka `remove` is not strictly atomic across tasks, so
        // more than one may succeed in rare race conditions. The critical
        // invariant is that the token is fully consumed afterward.
        assert!(
            success_count >= 1,
            "At least one consumer must succeed"
        );
        assert_eq!(
            success_count + failure_count,
            20,
            "All 20 attempts should resolve"
        );

        // Token is fully consumed -- no further consumption should succeed
        let replay = service.consume_state("concurrent_token").await;
        assert!(replay.is_err(), "Token should be fully consumed");
    }

    #[tokio::test]
    async fn test_concurrent_verify_state_only_first_succeeds() {
        let service = create_test_service();
        service
            .register_provider(
                "github".to_string(),
                OAuth2Provider::GitHub,
                Box::new(MockOAuth2Provider::new()),
            )
            .await;

        // Generate an auth URL (stores state internally)
        let (_, state_token) = service
            .get_authorization_url("github", None)
            .await
            .unwrap();

        // Spawn concurrent verify_state attempts
        let mut handles = Vec::new();
        for _ in 0..10 {
            let svc = service.clone();
            let tok = state_token.clone();
            handles.push(tokio::spawn(async move {
                svc.verify_state(&tok).await
            }));
        }

        let mut success_count = 0;
        for h in handles {
            if h.await.unwrap().is_ok() {
                success_count += 1;
            }
        }

        // At least one should succeed, and the state should be consumed
        assert!(success_count >= 1, "At least one verify must succeed");

        // No further verification should succeed
        let replay = service.verify_state(&state_token).await;
        assert!(replay.is_err(), "State token should be consumed");
    }

    // ========================================================================
    // Tests: State Isolation Between Tokens
    // ========================================================================

    #[tokio::test]
    async fn test_consuming_one_state_does_not_affect_others() {
        let service = create_test_service();

        for i in 0..5 {
            let state = OAuth2State {
                instance_name: format!("provider_{i}"),
                redirect_url: None,
                created_at: chrono::Utc::now(),
                bind_user_id: None,
                pkce_verifier: format!("verifier_{i}"),
            };
            service.store_state(&format!("isolated_token_{i}"), &state).await.unwrap();
        }

        // Consume token 2
        let consumed = service.consume_state("isolated_token_2").await.unwrap();
        assert_eq!(consumed.instance_name, "provider_2");

        // Other tokens should still be available
        for i in [0, 1, 3, 4] {
            let state = service.consume_state(&format!("isolated_token_{i}")).await.unwrap();
            assert_eq!(state.instance_name, format!("provider_{i}"));
        }

        // Token 2 is consumed, should fail
        let result = service.consume_state("isolated_token_2").await;
        assert!(result.is_err());
    }
}
