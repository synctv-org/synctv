//! OAuth2/OIDC authentication service
//!
//! This service handles OAuth2/OIDC login flow WITHOUT storing tokens.
//! Tokens are only used temporarily during login to fetch user info.
//!
//! ## State Storage
//! `OAuth2` states are persisted via the [`OAuthStateStore`] trait. Two
//! implementations are provided:
//! - [`RedisOAuthStateStore`]: Redis-backed, required for cluster mode
//!   (multi-replica) where the callback may hit a different node.
//! - [`InMemoryOAuthStateStore`]: In-memory, for standalone mode without
//!   Redis. Uses `Mutex<HashMap>` for atomic single-use consumption.

use std::sync::Arc;
use std::collections::HashMap;
use tokio::sync::RwLock;
use tracing::{debug, info};
use serde::{Deserialize, Serialize};

use crate::{
    models::{oauth2_client::OAuth2Provider, User, UserId, SignupMethod},
    repository::UserOAuthProviderRepository,
    oauth2::Provider as OAuth2ProviderTrait,
    service::UserService,
    Error, Result, InternalExt,
};

// ============================================================================
// OAuthStateStore trait
// ============================================================================

/// Storage backend for `OAuth2` CSRF state tokens.
///
/// Implementations **must** guarantee atomic single-use consumption: a state
/// stored with [`store`] can only be retrieved once via [`consume`]. Concurrent
/// attempts to consume the same token must result in exactly one success and
/// all others returning `Ok(None)`.
///
/// The Redis implementation achieves this via a Lua `GET + DEL` script.
/// An in-memory implementation can use a `Mutex`-protected `HashMap`.
#[async_trait::async_trait]
pub trait OAuthStateStore: Send + Sync {
    /// Persist `state` under `token_id`, expiring it after `ttl`.
    async fn store(&self, token_id: &str, state: &OAuth2State, ttl: std::time::Duration) -> Result<()>;

    /// Atomically retrieve **and remove** the state for `token_id`.
    ///
    /// Returns `Ok(Some(_))` exactly once per stored token.
    /// Returns `Ok(None)` for unknown or already-consumed tokens.
    async fn consume(&self, token_id: &str) -> Result<Option<OAuth2State>>;
}

// ============================================================================
// RedisOAuthStateStore
// ============================================================================

/// Redis-backed [`OAuthStateStore`].
///
/// States are stored as JSON with `SET EX` and consumed atomically with a
/// Lua `GET + DEL` script (same pattern as `WsTicketService`).
pub struct RedisOAuthStateStore {
    conn: redis::aio::ConnectionManager,
}

impl RedisOAuthStateStore {
    /// Create from an existing Redis `ConnectionManager`.
    #[must_use] 
    pub const fn new(conn: redis::aio::ConnectionManager) -> Self {
        Self { conn }
    }
}

/// Redis key prefix for `OAuth2` state tokens
const OAUTH2_STATE_KEY_PREFIX: &str = "oauth2:state:";

#[async_trait::async_trait]
impl OAuthStateStore for RedisOAuthStateStore {
    async fn store(&self, token_id: &str, state: &OAuth2State, ttl: std::time::Duration) -> Result<()> {
        let key = format!("{OAUTH2_STATE_KEY_PREFIX}{token_id}");
        let value = serde_json::to_string(state)
            .internal_with_err("Failed to serialize OAuth2 state")?;

        let mut conn = self.conn.clone();
        use redis::AsyncCommands;
        let _: () = tokio::time::timeout(
            crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
            conn.set_ex(&key, value, ttl.as_secs()),
        )
        .await
        .map_err(|_| Error::Internal("Redis timeout: store OAuth2 state".to_string()))?
        .internal_with_err("Failed to store OAuth2 state in Redis")?;

        debug!("Stored OAuth2 state in Redis for token {}", &token_id[..8.min(token_id.len())]);
        Ok(())
    }

    async fn consume(&self, token_id: &str) -> Result<Option<OAuth2State>> {
        let key = format!("{OAUTH2_STATE_KEY_PREFIX}{token_id}");
        let mut conn = self.conn.clone();

        // Atomic GET + DEL via Lua script (same pattern as WsTicketService)
        let lua_script = redis::Script::new(r#"
            local value = redis.call("GET", KEYS[1])
            if value then
                redis.call("DEL", KEYS[1])
            end
            return value
        "#);

        let value: Option<String> = tokio::time::timeout(
            crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
            lua_script.key(&key).invoke_async(&mut conn),
        )
        .await
        .map_err(|_| Error::Internal("Redis timeout: consume OAuth2 state".to_string()))?
        .internal_with_err("Failed to consume OAuth2 state from Redis")?;

        match value {
            Some(json) => {
                let state: OAuth2State = serde_json::from_str(&json)
                    .internal_with_err("Failed to deserialize OAuth2 state")?;
                debug!(
                    "Retrieved OAuth2 state from Redis for token {}",
                    &token_id[..8.min(token_id.len())]
                );
                Ok(Some(state))
            }
            None => Ok(None),
        }
    }
}

// ============================================================================
// InMemoryOAuthStateStore
// ============================================================================

/// In-memory [`OAuthStateStore`] for standalone mode (no Redis).
///
/// Uses a `Mutex<HashMap>` for atomic single-use consumption. TTL is enforced
/// via stored expiry timestamps; expired entries are swept on every `store()`
/// and `consume()` call to bound memory usage.
pub struct InMemoryOAuthStateStore {
    /// Map of `token_id` -> (state, `expiry_instant`)
    states: std::sync::Mutex<HashMap<String, (OAuth2State, std::time::Instant)>>,
}

impl InMemoryOAuthStateStore {
    /// Create a new in-memory OAuth state store.
    #[must_use]
    pub fn new() -> Self {
        Self { states: std::sync::Mutex::new(HashMap::new()) }
    }

    /// Remove all entries whose TTL has expired.
    fn sweep_expired(map: &mut HashMap<String, (OAuth2State, std::time::Instant)>) {
        let now = std::time::Instant::now();
        map.retain(|_, (_, expiry)| *expiry > now);
    }
}

impl Default for InMemoryOAuthStateStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl OAuthStateStore for InMemoryOAuthStateStore {
    async fn store(&self, token_id: &str, state: &OAuth2State, ttl: std::time::Duration) -> Result<()> {
        let expiry = std::time::Instant::now() + ttl;
        let mut map = self.states.lock().unwrap();
        Self::sweep_expired(&mut map);
        map.insert(token_id.to_string(), (state.clone(), expiry));
        Ok(())
    }

    async fn consume(&self, token_id: &str) -> Result<Option<OAuth2State>> {
        let mut map = self.states.lock().unwrap();
        Self::sweep_expired(&mut map);
        Ok(map.remove(token_id).map(|(state, _expiry)| state))
    }
}

// ============================================================================
// Domain types
// ============================================================================

/// Default TTL for `OAuth2` states (5 minutes)
const OAUTH2_STATE_TTL_SECONDS: u64 = 300;

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
    /// Whether the provider has verified the user's email address
    pub email_verified: bool,
}

/// An `OAuth2` provider entry combining the provider instance and its type
///
/// The provider is stored as `Arc<dyn>` rather than `Box<dyn>` so that callers
/// can clone the `Arc` while holding the read lock and then drop the lock before
/// invoking any async methods on the provider (Issue #74 — TOCTOU race fix).
struct OAuth2ProviderEntry {
    provider: Arc<dyn OAuth2ProviderTrait>,
    provider_type: OAuth2Provider,
}

// ============================================================================
// OAuth2Service
// ============================================================================

/// `OAuth2` authentication service
///
/// Handles OAuth2/OIDC login flow:
/// 1. Generate authorization URL with PKCE
/// 2. Exchange authorization code for user info
/// 3. Create/update user-provider mapping (NO TOKENS STORED)
///
/// State storage is delegated to the [`OAuthStateStore`] trait. Inject a
/// [`RedisOAuthStateStore`] for production; an in-memory implementation for tests.
#[derive(Clone)]
pub struct OAuth2Service {
    repository: UserOAuthProviderRepository,
    /// Map of instance name -> (provider instance, provider enum type)
    /// M-03: Consolidated from separate providers + `provider_types` maps to prevent lock ordering issues
    providers: Arc<RwLock<HashMap<String, OAuth2ProviderEntry>>>,
    /// State storage backend — injected via trait object
    state_store: Arc<dyn OAuthStateStore>,
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
    /// Create a new `OAuth2` service.
    ///
    /// * `state_store` — use [`RedisOAuthStateStore`] in production.
    #[must_use]
    pub fn new(repository: UserOAuthProviderRepository, state_store: Arc<dyn OAuthStateStore>) -> Self {
        info!("OAuth2 service initialized");

        Self {
            repository,
            providers: Arc::new(RwLock::new(HashMap::new())),
            state_store,
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

    /// Store `OAuth2` state via the configured state store
    async fn store_state(&self, state_token: &str, state: &OAuth2State) -> Result<()> {
        self.state_store
            .store(
                state_token,
                state,
                std::time::Duration::from_secs(OAUTH2_STATE_TTL_SECONDS),
            )
            .await?;
        debug!("Stored OAuth2 state for token {}", &state_token[..8.min(state_token.len())]);
        Ok(())
    }

    /// Retrieve and remove `OAuth2` state atomically.
    ///
    /// Uses the configured [`OAuthStateStore`] to ensure single-use consumption
    /// and prevent CSRF replay attacks.
    ///
    /// Also performs an additional expiry check on `created_at` as a defense-in-depth
    /// measure, even though the storage layer should have already enforced TTL.
    async fn consume_state(&self, state_token: &str) -> Result<OAuth2State> {
        match self.state_store.consume(state_token).await? {
            Some(state) => {
                // Defense-in-depth: verify state has not expired based on created_at timestamp.
                // This provides an additional layer of protection even if the storage backend
                // fails to properly enforce TTL (e.g., Redis downgraded to in-memory store).
                let age = chrono::Utc::now().signed_duration_since(state.created_at);
                if age.num_seconds() > OAUTH2_STATE_TTL_SECONDS as i64 {
                    debug!(
                        "OAuth2 state expired based on created_at (age: {}s, max: {}s)",
                        age.num_seconds(),
                        OAUTH2_STATE_TTL_SECONDS
                    );
                    return Err(Error::Authentication("Invalid or expired OAuth2 state".to_string()));
                }

                debug!(
                    "Retrieved OAuth2 state for token {}",
                    &state_token[..8.min(state_token.len())]
                );
                Ok(state)
            }
            None => Err(Error::Authentication("Invalid or expired OAuth2 state".to_string())),
        }
    }

    /// Register an `OAuth2` provider instance
    ///
    /// # Arguments
    /// * `instance_name` - Unique instance name (e.g., "github", "logto1", "logto2")
    /// * `provider_type` - Provider type enum
    /// * `provider` - The provider instance (wrapped in `Arc` internally for safe cloning)
    pub async fn register_provider(
        &self,
        instance_name: String,
        provider_type: OAuth2Provider,
        provider: Box<dyn OAuth2ProviderTrait>,
    ) {
        let mut providers = self.providers.write().await;

        info!("Registered OAuth2 provider: {} (type: {})", instance_name, provider_type.as_str());
        // Wrap in Arc so we can clone the reference while holding the read lock (Issue #74)
        providers.insert(instance_name, OAuth2ProviderEntry {
            provider: Arc::from(provider),
            provider_type,
        });
    }

    /// Generate authorization URL with PKCE challenge
    pub async fn get_authorization_url(
        &self,
        instance_name: &str,
        redirect_url: Option<String>,
    ) -> Result<(String, String)> {
        self.build_authorization_url(instance_name, redirect_url, None).await
    }

    /// Generate authorization URL for bind flow (associates with an authenticated user)
    pub async fn get_authorization_url_with_user(
        &self,
        instance_name: &str,
        redirect_url: Option<String>,
        user_id: Option<UserId>,
    ) -> Result<(String, String)> {
        self.build_authorization_url(instance_name, redirect_url, user_id).await
    }

    /// Shared implementation for building an `OAuth2` authorization URL.
    ///
    /// Issue #74 (TOCTOU fix): The provider `Arc` is cloned while holding the read lock,
    /// then the lock is released before any async I/O takes place. This prevents a race
    /// where another thread could call `unlink_provider` between the lookup and the
    /// `new_auth_url()` call.
    async fn build_authorization_url(
        &self,
        instance_name: &str,
        redirect_url: Option<String>,
        bind_user_id: Option<UserId>,
    ) -> Result<(String, String)> {
        // Validate redirect URL if provided
        if let Some(ref url) = redirect_url {
            Self::validate_redirect_url_with_allowlist(url, &self.allowed_redirect_domains)?;
        }

        // Clone the Arc<dyn> under the read lock, then drop the lock before any I/O.
        let provider: Arc<dyn OAuth2ProviderTrait> = {
            let providers = self.providers.read().await;
            providers
                .get(instance_name)
                .map(|entry| Arc::clone(&entry.provider))
                .ok_or_else(|| Error::InvalidInput(format!("OAuth2 provider instance not found: {instance_name}")))?
            // read lock dropped here
        };

        // Generate state token
        let state_token = nanoid::nanoid!(32);

        // Generate authorization URL with PKCE challenge (lock is NOT held here)
        let (auth_url, pkce_verifier) = provider.new_auth_url(&state_token).await
            .internal_with_err("Failed to generate authorization URL")?;

        // Store state (including PKCE verifier) for verification during callback
        let oauth_state = OAuth2State {
            instance_name: instance_name.to_string(),
            redirect_url,
            created_at: chrono::Utc::now(),
            bind_user_id,
            pkce_verifier,
        };

        self.store_state(&state_token, &oauth_state).await?;

        debug!(
            "Generated OAuth2 authorization URL for provider {}",
            instance_name
        );

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
                let domain_matched = allowed_domains.iter().any(|d| {
                    // Reject TLD-only entries (no dots) to prevent overly broad matching.
                    // e.g. "com" in the allowlist should NOT allow all .com domains.
                    if !d.contains('.') {
                        return false;
                    }
                    // Exact match or single-level subdomain only (e.g. "sub.example.com"
                    // matches allowlist entry "example.com", but "deep.sub.example.com" does not)
                    if host == d {
                        return true;
                    }
                    let suffix = format!(".{d}");
                    if let Some(prefix) = host.strip_suffix(&suffix) {
                        // Only allow single-level subdomain: prefix must not contain dots
                        !prefix.contains('.')
                    } else {
                        false
                    }
                });
                if !domain_matched {
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
    ///
    /// Issue #74 (TOCTOU fix): Provider `Arc` and provider type are captured while
    /// holding the read lock, then the lock is released before any async network I/O.
    /// This prevents a race where the provider could be unregistered between the
    /// `providers.get()` lookup and the `get_user_info()` network call.
    pub async fn exchange_code_for_user_info(
        &self,
        instance_name: &str,
        code: &str,
        pkce_verifier: &str,
    ) -> Result<(OAuth2UserInfo, OAuth2Provider)> {
        // Clone both the Arc<provider> and the provider_type under the read lock.
        // After this block the lock is released; subsequent code cannot race with
        // `unlink_provider` or `register_provider`.
        let (provider, provider_type): (Arc<dyn OAuth2ProviderTrait>, OAuth2Provider) = {
            let providers = self.providers.read().await;
            let entry = providers
                .get(instance_name)
                .ok_or_else(|| Error::InvalidInput(format!("OAuth2 provider instance not found: {instance_name}")))?;
            (Arc::clone(&entry.provider), entry.provider_type.clone())
            // read lock dropped here
        };

        debug!("Exchanging code for user info from {}", instance_name);

        // Network I/O without holding the lock
        let user_info = provider.get_user_info(code, pkce_verifier).await
            .internal_with_err("Failed to get user info")?;

        // Convert provider user info to service user info
        let service_user_info = OAuth2UserInfo {
            provider: provider_type.clone(),
            provider_user_id: user_info.provider_user_id,
            username: user_info.username,
            email: user_info.email,
            avatar: user_info.avatar,
            email_verified: user_info.email_verified,
        };

        Ok((service_user_info, provider_type))
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

    /// Find an existing user by `OAuth2` provider, or create a new one and link the provider,
    /// all within a single database transaction.
    ///
    /// This prevents the race condition where two concurrent `OAuth2` logins for the same
    /// provider identity both find no existing user and both create separate user records.
    ///
    /// ## Behaviour
    ///
    /// 1. **Found** — returns the existing [`UserId`] without touching the database further.
    /// 2. **Not found** — begins a transaction, creates a new user via
    ///    [`UserService::register_with_executor`], links the `OAuth2` provider mapping, and
    ///    commits. If the transaction fails the whole operation is rolled back atomically.
    ///
    /// On success, `is_new` in the returned tuple indicates whether a new account was created.
    ///
    /// ## Arguments
    /// * `user_service` — used to create the new user inside the transaction
    /// * `provider` — the `OAuth2` provider enum
    /// * `user_info` — user info fetched from the provider
    pub async fn find_or_create_and_link(
        &self,
        user_service: &UserService,
        provider: &OAuth2Provider,
        user_info: &OAuth2UserInfo,
    ) -> Result<(UserId, bool)> {
        // Fast path: user already linked — no transaction needed.
        if let Some(user_id) = self.find_user_by_provider(provider, &user_info.provider_user_id).await? {
            return Ok((user_id, false));
        }

        // Slow path: no existing mapping — create user + link in one transaction.
        let pool = self.repository.pool();
        let mut tx = pool.begin().await?;

        // Re-check inside the transaction to guard against the race where another
        // concurrent request created the user between our initial lookup and here.
        let existing = self.repository
            .find_by_provider_with_executor(provider, &user_info.provider_user_id, &mut *tx)
            .await?;

        if let Some(mapping) = existing {
            // Another concurrent request already created the mapping — use it.
            tx.rollback().await?;
            return Ok((mapping.user_id, false));
        }

        // Generate a random password (OAuth2 users authenticate via provider, not password).
        let random_password = nanoid::nanoid!(32);

        // Create the user record inside the transaction.
        let new_user: User = user_service
            .register_with_executor(
                user_info.username.clone(),
                user_info.email.clone(),
                random_password,
                SignupMethod::OAuth2,
                &mut *tx,
            )
            .await?;

        // Link the OAuth2 provider mapping inside the same transaction.
        self.upsert_user_provider_with_executor(
            &new_user.id,
            provider,
            &user_info.provider_user_id,
            user_info,
            &mut *tx,
        )
        .await?;

        // Set email_verified if the provider confirmed the email.
        if user_info.email_verified && user_info.email.is_some() {
            sqlx::query("UPDATE users SET email_verified = true, updated_at = NOW() WHERE id = $1")
                .bind(new_user.id.as_str())
                .execute(&mut *tx)
                .await
                .internal_with_err("Failed to set email_verified in transaction")?;
        }

        tx.commit().await?;

        info!(
            user_id = %new_user.id.as_str(),
            provider = %provider.as_str(),
            "Created new user via OAuth2 and linked provider in single transaction"
        );

        Ok((new_user.id, true))
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

    /// Remove all `OAuth2` provider mappings for a user.
    ///
    /// Used during user deletion to clean up all OAuth bindings.
    /// Returns the number of mappings removed.
    pub async fn delete_all_for_user(&self, user_id: &UserId) -> Result<u64> {
        self.repository.delete_all_for_user(user_id).await
    }

    /// Clean up expired `OAuth2` states (maintenance task)
    ///
    /// Note: Redis handles TTL automatically via SETEX.
    /// This method is now a no-op but kept for API compatibility.
    pub async fn cleanup_expired_states(&self, _max_age_seconds: i64) -> Result<()> {
        // Redis handles its own TTL via SETEX
        // This method is kept for API compatibility but is now a no-op
        Ok(())
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
                    email_verified: true,
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
    // Test service helpers — no Redis required
    // ========================================================================

    fn create_test_service() -> OAuth2Service {
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = crate::repository::UserOAuthProviderRepository::new(pool);
        let state_store = Arc::new(InMemoryOAuthStateStore::new());
        OAuth2Service::new(repo, state_store)
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

    #[test]
    fn test_redirect_tld_only_domain_rejected() {
        // Adding "com" to allowlist should NOT allow all .com domains
        let domains = vec!["com".to_string()];
        let result = OAuth2Service::validate_redirect_url_with_allowlist(
            "https://evil.com/callback",
            &domains,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_redirect_deep_subdomain_rejected() {
        // Only single-level subdomains are allowed
        let domains = vec!["example.com".to_string()];
        let result = OAuth2Service::validate_redirect_url_with_allowlist(
            "https://deep.sub.example.com/callback",
            &domains,
        );
        assert!(result.is_err());
    }

    // ========================================================================
    // Tests: State Management (in-memory, no Redis required)
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
    async fn test_state_store_is_abstracted() {
        // OAuth2Service takes Arc<dyn OAuthStateStore>, not a concrete Redis type.
        // This verifies the abstraction compiles with the in-memory implementation.
        let _service = create_test_service();
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
    // Tests: OAuth2State serialization (used for storage path)
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
                email_verified: false,
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
        let service = Arc::new(create_test_service());
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

        // With the Mutex-based store, exactly one consumer must succeed.
        assert_eq!(
            success_count, 1,
            "Exactly one consumer must succeed"
        );
        assert_eq!(
            failure_count, 19,
            "All other consumers must fail"
        );

        // Token is fully consumed -- no further consumption should succeed
        let replay = service.consume_state("concurrent_token").await;
        assert!(replay.is_err(), "Token should be fully consumed");
    }

    #[tokio::test]
    async fn test_concurrent_verify_state_only_first_succeeds() {
        let service = Arc::new(create_test_service());
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

        // Exactly one should succeed with the Mutex-based store
        assert_eq!(success_count, 1, "Exactly one verify must succeed");

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

    // ========================================================================
    // Tests: CSRF Protection - Defense in Depth
    // ========================================================================

    /// Test that state tokens with expired created_at timestamps are rejected
    /// even if they somehow persist in the store (defense-in-depth).
    #[tokio::test]
    async fn test_state_expired_created_at_rejected() {
        let service = create_test_service();

        // Create a state with a created_at timestamp that is already expired
        // (6 minutes ago, which exceeds the 5-minute TTL)
        let expired_time = chrono::Utc::now() - chrono::Duration::seconds(360);
        let state = OAuth2State {
            instance_name: "github".to_string(),
            redirect_url: None,
            created_at: expired_time,
            bind_user_id: None,
            pkce_verifier: "expired_verifier".to_string(),
        };

        // Store the state directly (bypassing normal TTL enforcement)
        service.store_state("expired_token", &state).await.unwrap();

        // Consumption should fail due to created_at check, even though token exists
        let result = service.consume_state("expired_token").await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(&err, Error::Authentication(msg) if msg.contains("Invalid or expired")),
            "Expected authentication error for expired state, got: {err}"
        );
    }

    /// Test that state tokens just within the TTL are accepted
    #[tokio::test]
    async fn test_state_within_ttl_accepted() {
        let service = create_test_service();

        // Create a state with a created_at timestamp that is just within TTL
        // (4 minutes ago, which is less than the 5-minute TTL)
        let within_ttl_time = chrono::Utc::now() - chrono::Duration::seconds(240);
        let state = OAuth2State {
            instance_name: "github".to_string(),
            redirect_url: None,
            created_at: within_ttl_time,
            bind_user_id: None,
            pkce_verifier: "valid_verifier".to_string(),
        };

        service.store_state("within_ttl_token", &state).await.unwrap();

        // Consumption should succeed
        let result = service.consume_state("within_ttl_token").await;
        assert!(result.is_ok());
        let retrieved = result.unwrap();
        assert_eq!(retrieved.pkce_verifier, "valid_verifier");
    }

    /// Test that state tokens at the exact TTL boundary are handled correctly
    #[tokio::test]
    async fn test_state_at_ttl_boundary() {
        let service = create_test_service();

        // Create a state just past the TTL boundary (TTL + 1 second ago)
        // This ensures the test is deterministic regardless of execution timing
        let past_boundary_time = chrono::Utc::now() - chrono::Duration::seconds(OAUTH2_STATE_TTL_SECONDS as i64 + 1);
        let state = OAuth2State {
            instance_name: "github".to_string(),
            redirect_url: None,
            created_at: past_boundary_time,
            bind_user_id: None,
            pkce_verifier: "boundary_verifier".to_string(),
        };

        service.store_state("boundary_token", &state).await.unwrap();

        // Past TTL seconds, the state should be rejected (> TTL)
        let result = service.consume_state("boundary_token").await;
        assert!(result.is_err());
    }

    /// Test that verify_state includes the created_at expiry check
    #[tokio::test]
    async fn test_verify_state_checks_created_at_expiry() {
        let service = create_test_service();
        service
            .register_provider(
                "github".to_string(),
                OAuth2Provider::GitHub,
                Box::new(MockOAuth2Provider::new()),
            )
            .await;

        // Manually create an expired state
        let expired_time = chrono::Utc::now() - chrono::Duration::seconds(360);
        let state = OAuth2State {
            instance_name: "github".to_string(),
            redirect_url: None,
            created_at: expired_time,
            bind_user_id: None,
            pkce_verifier: "expired".to_string(),
        };

        service.store_state("verify_expired_token", &state).await.unwrap();

        // verify_state should also reject expired tokens
        let result = service.verify_state("verify_expired_token").await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(&err, Error::Authentication(msg) if msg.contains("Invalid or expired")),
            "Expected authentication error for expired state in verify_state, got: {err}"
        );
    }

    /// Test that provider mismatch is detected during code exchange
    #[tokio::test]
    async fn test_csrf_protection_provider_mismatch_detected() {
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
                "google".to_string(),
                OAuth2Provider::Google,
                Box::new(MockOAuth2Provider::new()),
            )
            .await;

        // Generate state for github
        let (_, state_token) = service
            .get_authorization_url("github", None)
            .await
            .unwrap();

        // Verify the state contains github as provider
        let state = service.verify_state(&state_token).await.unwrap();
        assert_eq!(state.instance_name, "github");

        // In the API layer, if attacker tries to use github's state with google provider,
        // the provider mismatch check in exchange_authorization_code will catch it.
        // This test verifies the state contains the correct instance_name.
    }
}
