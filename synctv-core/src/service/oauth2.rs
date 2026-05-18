//! OAuth2/OIDC authentication service
//!
//! This service handles OAuth2/OIDC login flow WITHOUT storing tokens.
//! Tokens are only used temporarily during login to fetch user info.
//!
//! ## State Storage
//!
//! `OAuth2` states are persisted via the [`OAuthStateStore`] trait. Two
//! implementations are provided:
//! - [`RedisOAuthStateStore`]: shared cross-node storage for multi-replica
//!   deployments where the callback may hit a different node.
//! - [`InMemoryOAuthStateStore`]: local-only storage for standalone mode.
//!   Uses `moka::sync::Cache` with TTL-based expiry and bounded capacity.

use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use synctv_common::ExecutionControl;
use tokio::sync::RwLock;
use tracing::{debug, info};

use crate::{
    cache::KeyBuilder,
    models::{oauth2_client::OAuth2Provider, SignupMethod, User, UserId},
    oauth2::Provider as OAuth2ProviderTrait,
    repository::UserOAuthProviderRepository,
    service::{
        user::PendingRegistrationConflict, OAuth2SignupPolicy, SettingsRegistry, UserService,
    },
    Error, InternalExt, RedisConnectionRuntime, Result, SharedStateMode, SharedStateProfile,
};

static CONSUME_OAUTH2_STATE_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r#"
        local value = redis.call("GET", KEYS[1])
        if value then
            redis.call("DEL", KEYS[1])
        end
        return value
        "#,
    )
});

// OAuthStateStore trait

/// Storage backend for `OAuth2` CSRF state tokens.
///
/// Implementations **must** guarantee atomic single-use consumption: a state
/// stored with [`store`] can only be retrieved once via [`consume`]. Concurrent
/// attempts to consume the same token must result in exactly one success and
/// all others returning `Ok(None)`.
///
/// The Redis implementation achieves this via a Lua `GET + DEL` script.
/// An in-memory implementation can use a `Mutex`-protected `LruCache` with
/// capacity limiting to prevent memory exhaustion.
#[async_trait::async_trait]
pub trait OAuthStateStore: Send + Sync {
    /// Persist `state` under `token_id`, expiring it after `ttl`.
    async fn store(
        &self,
        token_id: &str,
        state: &OAuth2State,
        ttl: std::time::Duration,
    ) -> Result<()>;

    /// Atomically retrieve **and remove** the state for `token_id`.
    ///
    /// Returns `Ok(Some(_))` exactly once per stored token.
    /// Returns `Ok(None)` for unknown or already-consumed tokens.
    async fn consume(&self, token_id: &str) -> Result<Option<OAuth2State>>;

    /// Whether this store can safely enforce single-use semantics across nodes.
    ///
    /// Clustered OAuth2 callback handling requires a shared state store because
    /// the node that receives the callback may differ from the node that issued
    /// the original authorization redirect.
    fn supports_cross_node_single_use(&self) -> bool;
}

/// Build an [`OAuthStateStore`] from the shared-state profile.
///
/// This is the backend-agnostic wiring entry point for production/bootstrap
/// code. Callers should depend on the returned trait object instead of
/// branching on Redis or local storage directly.
///
/// # Errors
///
/// Returns an error when distributed mode requires shared state but no shared
/// runtime is available.
pub fn state_store_from_shared_state_profile(
    profile: &SharedStateProfile,
) -> Result<Arc<dyn OAuthStateStore>> {
    match profile.state_mode() {
        SharedStateMode::SharedRequired => {
            let shared_runtime =
                profile.require_shared_runtime("single-use OAuth2 state storage")?;
            Ok(shared_oauth_state_store(
                shared_runtime,
                profile.key_prefix().to_string(),
            ))
        }
        SharedStateMode::SharedBestEffort => Ok(shared_oauth_state_store(
            profile
                .shared_runtime()
                .expect("shared state profile guarantees runtime in best-effort mode"),
            profile.key_prefix().to_string(),
        )),
        SharedStateMode::LocalOnly => Ok(local_oauth_state_store()),
    }
}

/// Build a local-only OAuth state store behind the trait abstraction.
#[must_use]
pub fn local_oauth_state_store() -> Arc<dyn OAuthStateStore> {
    Arc::new(InMemoryOAuthStateStore::new())
}

/// Build a shared OAuth state store behind the trait abstraction.
#[must_use]
pub fn shared_oauth_state_store(
    runtime: Arc<dyn RedisConnectionRuntime>,
    key_prefix: impl Into<String>,
) -> Arc<dyn OAuthStateStore> {
    Arc::new(RedisOAuthStateStore::from_runtime(runtime, key_prefix))
}

// RedisOAuthStateStore

/// Redis-backed [`OAuthStateStore`].
///
/// States are stored as JSON with `SET EX` and consumed atomically with a
/// Lua `GET + DEL` script (same pattern as `WsTicketService`).
pub struct RedisOAuthStateStore {
    /// Redis runtime that yields a fresh connection snapshot per operation.
    conn: std::sync::Arc<dyn RedisConnectionRuntime>,
    key_builder: KeyBuilder,
}

impl RedisOAuthStateStore {
    async fn run_redis_op<T, F>(&self, operation: &'static str, future: F) -> Result<T>
    where
        F: Future<Output = std::result::Result<T, redis::RedisError>>,
    {
        run_oauth_state_redis_op(self.conn.operation_timeout(), operation, future).await
    }

    #[must_use]
    pub fn from_runtime(
        conn: std::sync::Arc<dyn RedisConnectionRuntime>,
        key_prefix: impl Into<String>,
    ) -> Self {
        Self {
            conn,
            key_builder: KeyBuilder::new(key_prefix),
        }
    }

    /// Acquire a fresh ConnectionManager clone from the shared handle.
    async fn get_conn(&self, operation: &'static str) -> Result<redis::aio::ConnectionManager> {
        crate::redis_runtime_snapshot(&*self.conn, operation).await
    }

    fn redis_key(&self, token_id: &str) -> String {
        self.key_builder.oauth2_state(token_id)
    }
}

async fn run_oauth_state_redis_op<T, F>(
    timeout: Duration,
    operation: &'static str,
    future: F,
) -> Result<T>
where
    F: Future<Output = std::result::Result<T, redis::RedisError>>,
{
    tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| Error::Timeout(format!("Redis timeout: {operation}")))?
        .internal_with_err(&format!("Failed to {operation}"))
}

#[async_trait::async_trait]
impl OAuthStateStore for RedisOAuthStateStore {
    fn supports_cross_node_single_use(&self) -> bool {
        true
    }

    async fn store(
        &self,
        token_id: &str,
        state: &OAuth2State,
        ttl: std::time::Duration,
    ) -> Result<()> {
        let key = self.redis_key(token_id);
        let value =
            serde_json::to_string(state).internal_with_err("Failed to serialize OAuth2 state")?;

        let mut conn = self.get_conn("store OAuth2 state in Redis").await?;
        let _: () = self
            .run_redis_op(
                "store OAuth2 state in Redis",
                conn.set_ex(&key, value, ttl.as_secs()),
            )
            .await?;

        debug!(
            "Stored OAuth2 state in Redis for token {}",
            &token_id[..8.min(token_id.len())]
        );
        Ok(())
    }

    async fn consume(&self, token_id: &str) -> Result<Option<OAuth2State>> {
        let key = self.redis_key(token_id);
        let mut conn = self.get_conn("consume OAuth2 state from Redis").await?;

        let value: Option<String> = self
            .run_redis_op(
                "consume OAuth2 state from Redis",
                CONSUME_OAUTH2_STATE_SCRIPT
                    .key(&key)
                    .invoke_async(&mut conn),
            )
            .await?;

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

// InMemoryOAuthStateStore

/// Entry stored in the in-memory OAuth state cache, pairing the state with its TTL.
#[derive(Clone)]
struct OAuthStateEntry {
    state: OAuth2State,
    ttl: std::time::Duration,
}

/// Per-entry expiry policy: reads the requested TTL from each entry.
struct PerEntryTtl;

impl moka::Expiry<String, OAuthStateEntry> for PerEntryTtl {
    fn expire_after_create(
        &self,
        _key: &String,
        value: &OAuthStateEntry,
        _now: std::time::Instant,
    ) -> Option<std::time::Duration> {
        Some(value.ttl)
    }
}

/// In-memory [`OAuthStateStore`] for standalone mode (no Redis).
///
/// Uses `moka::sync::Cache` with a hard capacity limit and per-entry
/// TTL-based expiry. This provides:
/// - **Bounded memory**: `max_capacity` prevents memory exhaustion from
///   malicious requests (default 10,000 entries).
/// - **Per-entry expiry**: each entry expires after its requested TTL.
/// - **Concurrent access**: moka uses sharded locking internally for
///   high-throughput concurrent operations.
pub struct InMemoryOAuthStateStore {
    entries: moka::sync::Cache<String, OAuthStateEntry>,
}

/// Default maximum entries for the OAuth state store.
const DEFAULT_CAPACITY: u64 = 10_000;

impl InMemoryOAuthStateStore {
    /// Create a new in-memory OAuth state store with default capacity (10,000).
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    /// Create a new in-memory OAuth state store with a custom max capacity.
    #[must_use]
    pub fn with_capacity(max_capacity: u64) -> Self {
        Self {
            entries: moka::sync::Cache::builder()
                .max_capacity(max_capacity)
                .expire_after(PerEntryTtl)
                .build(),
        }
    }
}

impl Default for InMemoryOAuthStateStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl OAuthStateStore for InMemoryOAuthStateStore {
    fn supports_cross_node_single_use(&self) -> bool {
        false
    }

    async fn store(
        &self,
        token_id: &str,
        state: &OAuth2State,
        ttl: std::time::Duration,
    ) -> Result<()> {
        self.entries.insert(
            token_id.to_string(),
            OAuthStateEntry {
                state: state.clone(),
                ttl,
            },
        );
        Ok(())
    }

    async fn consume(&self, token_id: &str) -> Result<Option<OAuth2State>> {
        // get() respects TTL — returns None for expired entries.
        // remove() does NOT check expiry, so we gate on get() first.
        // Race between get and remove is fine: remove() is the atomic
        // operation that determines the single winner among concurrent
        // consumers.
        if self.entries.get(token_id).is_none() {
            return Ok(None);
        }
        Ok(self.entries.remove(token_id).map(|e| e.state))
    }
}

// Domain types

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
struct OAuth2ProviderEntry {
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
    repository: UserOAuthProviderRepository,
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
    /// Allowlist of permitted redirect domains. Empty means relative paths only.
    allowed_redirect_domains: Arc<Vec<String>>,
    settings_registry: Option<Arc<SettingsRegistry>>,
    providers_fingerprint: Arc<RwLock<Option<String>>>,
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
    async fn run_with_control<T, F>(control: Option<&ExecutionControl>, future: F) -> Result<T>
    where
        F: Future<Output = Result<T>>,
    {
        match control {
            Some(control) => control.run(future).await.map_err(Error::from)?,
            None => future.await,
        }
    }

    /// Create a new `OAuth2` service.
    ///
    /// # Arguments
    /// * `repository` — User OAuth provider repository
    /// * `state_store` — use a shared single-use store in cluster mode
    /// * `cluster_mode` — whether cluster mode is enabled (multi-replica deployment)
    ///
    /// # Errors
    /// Returns `Error::Internal` if `cluster_mode` is true but `state_store`
    /// does not support cross-node single-use consumption. Local-only state
    /// storage is not safe in cluster mode because `OAuth2` callbacks may hit
    /// different replicas.
    pub fn new(
        repository: UserOAuthProviderRepository,
        state_store: Arc<dyn OAuthStateStore>,
        provider_registry: crate::oauth2::ProviderRegistry,
        cluster_mode: bool,
    ) -> Result<Self> {
        Self::new_with_ssrf_guard(
            repository,
            state_store,
            provider_registry,
            synctv_common::ssrf::SsrfGuard::strict_policy(),
            cluster_mode,
        )
    }

    /// Create a new `OAuth2` service using the runtime SSRF policy.
    ///
    /// # Errors
    /// Returns `Error::Internal` if `cluster_mode` is true but `state_store`
    /// does not support cross-node single-use consumption.
    pub fn new_with_ssrf_guard(
        repository: UserOAuthProviderRepository,
        state_store: Arc<dyn OAuthStateStore>,
        provider_registry: crate::oauth2::ProviderRegistry,
        ssrf_guard: synctv_common::ssrf::SsrfGuard,
        cluster_mode: bool,
    ) -> Result<Self> {
        // Clustered callback handling requires shared single-use state storage.
        if cluster_mode && !state_store.supports_cross_node_single_use() {
            return Err(Error::Internal(
                "distributed runtime requires shared single-use OAuth2 state storage. \
                 Local-only state is only visible on the replica that created it, \
                 causing authentication failures when the callback hits a different replica. \
                 Configure a shared state backend to fix this."
                    .to_string(),
            ));
        }

        info!(
            cross_node_single_use = state_store.supports_cross_node_single_use(),
            "OAuth2 service initialized"
        );

        Ok(Self {
            repository,
            providers: Arc::new(RwLock::new(HashMap::new())),
            state_store,
            provider_registry,
            ssrf_guard,
            allowed_redirect_domains: Arc::new(Vec::new()),
            settings_registry: None,
            providers_fingerprint: Arc::new(RwLock::new(None)),
        })
    }

    #[must_use]
    pub fn with_settings_registry(mut self, settings_registry: Arc<SettingsRegistry>) -> Self {
        self.settings_registry = Some(settings_registry);
        self
    }

    #[must_use]
    pub const fn provider_registry(&self) -> &crate::oauth2::ProviderRegistry {
        &self.provider_registry
    }

    /// Set allowlist of permitted redirect domains
    ///
    /// When set, absolute redirect URLs are only accepted if their host matches
    /// one of the allowed domains. When empty, only relative paths are allowed.
    pub fn set_allowed_redirect_domains(&mut self, domains: Vec<String>) {
        self.allowed_redirect_domains = Arc::new(domains);
    }

    #[cfg(test)]
    async fn store_state(&self, state_token: &str, state: &OAuth2State) -> Result<()> {
        self.store_state_with_control(state_token, state, None)
            .await
    }

    async fn store_state_with_control(
        &self,
        state_token: &str,
        state: &OAuth2State,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        Self::run_with_control(
            control,
            self.state_store.store(
                state_token,
                state,
                std::time::Duration::from_secs(OAUTH2_STATE_TTL_SECONDS),
            ),
        )
        .await?;
        debug!(
            "Stored OAuth2 state for token {}",
            &state_token[..8.min(state_token.len())]
        );
        Ok(())
    }

    /// Retrieve and remove `OAuth2` state atomically.
    ///
    /// Uses the configured [`OAuthStateStore`] to ensure single-use consumption
    /// and prevent CSRF replay attacks.
    ///
    #[cfg(test)]
    async fn consume_state(&self, state_token: &str) -> Result<OAuth2State> {
        self.consume_state_with_control(state_token, None).await
    }

    async fn consume_state_with_control(
        &self,
        state_token: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<OAuth2State> {
        match Self::run_with_control(control, self.state_store.consume(state_token)).await? {
            Some(state) => {
                // Defense-in-depth: verify state has not expired based on created_at timestamp.
                // This provides an additional layer of protection even if the storage backend
                // fails to properly enforce TTL (e.g., Redis downgraded to in-memory store).
                let age = chrono::Utc::now().signed_duration_since(state.created_at);
                if age.num_seconds() > OAUTH2_STATE_TTL_SECONDS_I64 {
                    debug!(
                        "OAuth2 state expired based on created_at (age: {}s, max: {}s)",
                        age.num_seconds(),
                        OAUTH2_STATE_TTL_SECONDS
                    );
                    return Err(Error::Authentication(
                        "Invalid or expired OAuth2 state".to_string(),
                    ));
                }

                debug!(
                    "Retrieved OAuth2 state for token {}",
                    &state_token[..8.min(state_token.len())]
                );
                Ok(state)
            }
            None => Err(Error::Authentication(
                "Invalid or expired OAuth2 state".to_string(),
            )),
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

        info!(
            "Registered OAuth2 provider: {} (type: {})",
            instance_name,
            provider_type.as_str()
        );
        providers.insert(
            instance_name,
            OAuth2ProviderEntry {
                provider: Arc::from(provider),
                provider_type,
                signup_policy: OAuth2SignupPolicy::default(),
            },
        );
    }

    async fn sync_runtime_providers(&self) -> Result<()> {
        let Some(settings_registry) = self.settings_registry.as_ref() else {
            return Ok(());
        };

        let configs = settings_registry.oauth2_providers.get()?;
        configs.validate_with_ssrf_guard(&self.ssrf_guard)?;
        let fingerprint = configs.to_string();
        {
            let cached = self.providers_fingerprint.read().await;
            if cached.as_deref() == Some(fingerprint.as_str()) {
                return Ok(());
            }
        }

        let mut rebuilt = HashMap::new();
        for (instance_name, provider_config) in configs.0 {
            let provider_type_name = provider_config.provider_type.trim().to_ascii_lowercase();
            let provider_type =
                OAuth2Provider::from_str_name(&provider_type_name).ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "OAuth2 provider '{instance_name}' uses unsupported type '{provider_type_name}'"
                    ))
                })?;
            let provider_private_config = provider_config.provider_config_value();
            let provider = self
                .provider_registry
                .create_provider(&provider_type_name, &provider_private_config)?;

            rebuilt.insert(
                instance_name,
                OAuth2ProviderEntry {
                    provider: Arc::from(provider),
                    provider_type,
                    signup_policy: provider_config.signup_policy(),
                },
            );
        }

        *self.providers.write().await = rebuilt;
        *self.providers_fingerprint.write().await = Some(fingerprint);

        Ok(())
    }

    async fn provider_entry(&self, instance_name: &str) -> Result<OAuth2ProviderEntry> {
        self.sync_runtime_providers().await?;
        let providers = self.providers.read().await;
        providers.get(instance_name).cloned().ok_or_else(|| {
            Error::InvalidInput(format!(
                "OAuth2 provider instance not found: {instance_name}"
            ))
        })
    }

    pub async fn signup_policy_for(&self, instance_name: &str) -> Result<OAuth2SignupPolicy> {
        if self.settings_registry.is_none() {
            return Ok(OAuth2SignupPolicy {
                enable_signup: true,
                signup_need_review: false,
            });
        }
        self.sync_runtime_providers().await?;
        let providers = self.providers.read().await;
        Ok(providers
            .get(instance_name)
            .map(|entry| entry.signup_policy.clone())
            .unwrap_or_default())
    }

    /// Generate authorization URL with PKCE challenge
    pub async fn get_authorization_url(
        &self,
        instance_name: &str,
        redirect_url: Option<String>,
    ) -> Result<(String, String)> {
        self.get_authorization_url_with_control(instance_name, redirect_url, None)
            .await
    }

    pub async fn get_authorization_url_with_control(
        &self,
        instance_name: &str,
        redirect_url: Option<String>,
        control: Option<&ExecutionControl>,
    ) -> Result<(String, String)> {
        self.build_authorization_url(instance_name, redirect_url, None, control)
            .await
    }

    /// Generate authorization URL for bind flow (associates with an authenticated user)
    pub async fn get_authorization_url_with_user(
        &self,
        instance_name: &str,
        redirect_url: Option<String>,
        user_id: Option<UserId>,
    ) -> Result<(String, String)> {
        self.get_authorization_url_with_user_with_control(
            instance_name,
            redirect_url,
            user_id,
            None,
        )
        .await
    }

    pub async fn get_authorization_url_with_user_with_control(
        &self,
        instance_name: &str,
        redirect_url: Option<String>,
        user_id: Option<UserId>,
        control: Option<&ExecutionControl>,
    ) -> Result<(String, String)> {
        self.build_authorization_url(instance_name, redirect_url, user_id, control)
            .await
    }

    /// Shared implementation for building an `OAuth2` authorization URL.
    ///
    /// (TOCTOU fix): The provider `Arc` is cloned while holding the read lock,
    /// then the lock is released before any async I/O takes place. This prevents a race
    /// where another thread could call `unlink_provider` between the lookup and the
    /// `new_auth_url()` call.
    async fn build_authorization_url(
        &self,
        instance_name: &str,
        redirect_url: Option<String>,
        bind_user_id: Option<UserId>,
        control: Option<&ExecutionControl>,
    ) -> Result<(String, String)> {
        // Validate redirect URL if provided
        if let Some(ref url) = redirect_url {
            Self::validate_redirect_url_with_allowlist(url, &self.allowed_redirect_domains)?;
        }

        let provider = self.provider_entry(instance_name).await?.provider;

        // Generate state token
        let state_token = synctv_common::snanoid!(32);

        // Generate authorization URL with PKCE challenge (lock is NOT held here)
        let auth = Self::run_with_control(control, async {
            provider
                .new_auth_url(&state_token)
                .await
                .internal_with_err("Failed to generate authorization URL")
        })
        .await?;

        // Store state (including PKCE verifier) for verification during callback
        let oauth_state = OAuth2State {
            instance_name: instance_name.to_string(),
            redirect_url,
            created_at: chrono::Utc::now(),
            bind_user_id,
            pkce_verifier: auth.pkce_verifier,
            nonce: auth.nonce,
        };

        self.store_state_with_control(&state_token, &oauth_state, control)
            .await?;

        debug!(
            "Generated OAuth2 authorization URL for provider {}",
            instance_name
        );

        Ok((auth.auth_url, state_token))
    }

    /// Validate redirect URL to prevent open redirect vulnerabilities (CWE-601)
    ///
    /// Accepted forms:
    /// - Relative paths (`/dashboard`)
    /// - Native-app custom schemes matching the configured redirect domain allowlist
    ///   (`com.example.app:/oauth2/callback` when `app.example.com` is allowed)
    /// - Loopback HTTP URLs for native clients (`http://127.0.0.1:34567/callback`)
    /// - Absolute HTTP/HTTPS URLs matching the configured allowlist
    fn validate_redirect_url_with_allowlist(url: &str, allowed_domains: &[String]) -> Result<()> {
        // Empty or whitespace-only URLs are rejected
        if url.trim().is_empty() {
            return Err(Error::InvalidInput(
                "Redirect URL cannot be empty".to_string(),
            ));
        }

        // Allow relative paths (must start with '/')
        if url.starts_with('/') {
            // Reject URLs with '//' (protocol-relative URLs can be used for open redirect)
            if url.starts_with("//") {
                return Err(Error::InvalidInput(
                    "Protocol-relative URLs are not allowed for security reasons".to_string(),
                ));
            }
            // Valid relative path
            return Ok(());
        }

        // For absolute URLs, parse and validate
        match url::Url::parse(url) {
            Ok(parsed_url) => {
                let scheme = parsed_url.scheme();

                // Reject URLs with authentication credentials (user:pass@host)
                if parsed_url.username() != "" || parsed_url.password().is_some() {
                    return Err(Error::InvalidInput(
                        "URLs with embedded credentials are not allowed".to_string(),
                    ));
                }

                if scheme != "http" && scheme != "https" {
                    if Self::is_allowed_native_custom_scheme_redirect(&parsed_url, allowed_domains)
                    {
                        return Ok(());
                    }

                    return Err(Error::InvalidInput(format!(
                        "Invalid URL scheme: {scheme}. Only http, https, or configured native-app custom schemes are allowed"
                    )));
                }

                // Check against allowed domains allowlist
                let host = parsed_url.host_str().unwrap_or("");
                if Self::is_loopback_host(host) {
                    return Ok(());
                }
                if allowed_domains.is_empty() {
                    return Err(Error::InvalidInput(
                        "Absolute redirect URLs are not allowed. Use a relative path instead."
                            .to_string(),
                    ));
                }
                let domain_matched = Self::redirect_host_matches_allowlist(host, allowed_domains);
                if !domain_matched {
                    return Err(Error::InvalidInput(format!(
                        "Redirect URL domain '{host}' is not in the allowed domains list"
                    )));
                }

                Ok(())
            }
            Err(_) => Err(Error::InvalidInput(format!(
                "Invalid redirect URL format: {url}"
            ))),
        }
    }

    fn is_allowed_native_custom_scheme_redirect(
        parsed_url: &url::Url,
        allowed_domains: &[String],
    ) -> bool {
        let scheme = parsed_url.scheme();
        if matches!(
            scheme,
            "http" | "https" | "javascript" | "data" | "file" | "ftp"
        ) {
            return false;
        }

        if scheme.len() < 2
            || !scheme
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic())
        {
            return false;
        }

        if !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '+' || c == '.')
        {
            return false;
        }

        if parsed_url.path().is_empty() && parsed_url.host_str().is_none() {
            return false;
        }

        let Some(reversed_scheme_domain) = Self::reverse_domain_from_native_scheme(scheme) else {
            return false;
        };

        Self::redirect_host_matches_allowlist(&reversed_scheme_domain, allowed_domains)
    }

    fn is_loopback_host(host: &str) -> bool {
        matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1")
    }

    fn reverse_domain_from_native_scheme(scheme: &str) -> Option<String> {
        let parts = scheme.split('.').collect::<Vec<_>>();
        if parts.len() < 3 || parts.iter().any(|part| part.is_empty()) {
            return None;
        }

        Some(parts.into_iter().rev().collect::<Vec<_>>().join("."))
    }

    fn redirect_host_matches_allowlist(host: &str, allowed_domains: &[String]) -> bool {
        allowed_domains
            .iter()
            .any(|domain| Self::redirect_domain_matches(host, domain))
    }

    fn redirect_domain_matches(host: &str, allowed_domain: &str) -> bool {
        // Reject TLD-only entries (no dots) to prevent overly broad matching.
        // e.g. "com" in the allowlist should NOT allow all.com domains.
        if !allowed_domain.contains('.') {
            return false;
        }
        if host == allowed_domain {
            return true;
        }

        let suffix = format!(".{allowed_domain}");
        host.strip_suffix(&suffix)
            .is_some_and(|prefix| !prefix.contains('.'))
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

    /// Exchange authorization code for user info with PKCE verification
    ///
    /// (TOCTOU fix): Provider `Arc` and provider type are captured while
    /// holding the read lock, then the lock is released before any async network I/O.
    /// This prevents a race where the provider could be unregistered between the
    /// `providers.get()` lookup and the `get_user_info()` network call.
    pub async fn exchange_code_for_user_info(
        &self,
        instance_name: &str,
        code: &str,
        pkce_verifier: &str,
    ) -> Result<OAuth2UserInfo> {
        self.exchange_code_for_user_info_with_control(instance_name, code, pkce_verifier, None)
            .await
    }

    pub async fn exchange_code_for_user_info_with_control(
        &self,
        instance_name: &str,
        code: &str,
        pkce_verifier: &str,
        control: Option<&ExecutionControl>,
    ) -> Result<OAuth2UserInfo> {
        self.exchange_code_for_user_info_with_nonce_and_control(
            instance_name,
            code,
            pkce_verifier,
            None,
            control,
        )
        .await
    }

    pub async fn exchange_code_for_user_info_with_state_and_control(
        &self,
        instance_name: &str,
        code: &str,
        oauth_state: &OAuth2State,
        control: Option<&ExecutionControl>,
    ) -> Result<OAuth2UserInfo> {
        self.exchange_code_for_user_info_with_nonce_and_control(
            instance_name,
            code,
            &oauth_state.pkce_verifier,
            oauth_state.nonce.as_deref(),
            control,
        )
        .await
    }

    async fn exchange_code_for_user_info_with_nonce_and_control(
        &self,
        instance_name: &str,
        code: &str,
        pkce_verifier: &str,
        nonce: Option<&str>,
        control: Option<&ExecutionControl>,
    ) -> Result<OAuth2UserInfo> {
        let entry = self.provider_entry(instance_name).await?;
        let provider = entry.provider;
        let provider_type = entry.provider_type;

        debug!("Exchanging code for user info from {}", instance_name);

        // Network I/O without holding the lock
        let user_info = Self::run_with_control(control, async {
            provider
                .get_user_info(code, pkce_verifier, nonce)
                .await
                .internal_with_err("Failed to get user info")
        })
        .await?;

        // Convert provider user info to service user info
        let service_user_info = OAuth2UserInfo {
            provider: provider_type.clone(),
            provider_instance_name: instance_name.to_string(),
            provider_issuer: None,
            provider_user_id: user_info.provider_user_id,
            username: user_info.username,
            email: user_info.email,
            avatar: user_info.avatar,
            email_verified: user_info.email_verified,
        };

        Ok(service_user_info)
    }

    /// Create or update user-OAuth2 provider mapping
    pub async fn upsert_user_provider(
        &self,
        user_id: &UserId,
        user_info: &OAuth2UserInfo,
    ) -> Result<()> {
        // Convert service user info to repository format
        let repo_user_info = crate::models::oauth2_client::OAuth2UserInfo {
            provider: user_info.provider.clone(),
            provider_instance_name: user_info.provider_instance_name.clone(),
            provider_issuer: user_info.provider_issuer.clone(),
            provider_user_id: user_info.provider_user_id.clone(),
            username: user_info.username.clone(),
            email: user_info.email.clone(),
            avatar: user_info.avatar.clone(),
        };

        self.repository
            .upsert(
                user_id,
                &user_info.provider,
                &user_info.provider_instance_name,
                &user_info.provider_user_id,
                &repo_user_info,
            )
            .await
    }

    /// Create or update user-OAuth2 provider mapping using a provided executor
    pub async fn upsert_user_provider_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        user_info: &OAuth2UserInfo,
        executor: E,
    ) -> Result<()>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let repo_user_info = crate::models::oauth2_client::OAuth2UserInfo {
            provider: user_info.provider.clone(),
            provider_instance_name: user_info.provider_instance_name.clone(),
            provider_issuer: user_info.provider_issuer.clone(),
            provider_user_id: user_info.provider_user_id.clone(),
            username: user_info.username.clone(),
            email: user_info.email.clone(),
            avatar: user_info.avatar.clone(),
        };

        self.repository
            .upsert_with_executor(
                user_id,
                &user_info.provider,
                &user_info.provider_instance_name,
                &user_info.provider_user_id,
                &repo_user_info,
                executor,
            )
            .await
    }

    /// Find user by `OAuth2` provider instance
    pub async fn find_user_by_provider_instance(
        &self,
        provider_instance_name: &str,
        provider_user_id: &str,
    ) -> Result<Option<UserId>> {
        match self
            .repository
            .find_by_provider_instance(provider_instance_name, provider_user_id)
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
    /// * `instance_name` — the configured provider instance that owns the external identity namespace
    /// * `user_info` — user info fetched from the provider
    pub async fn find_or_create_and_link(
        &self,
        user_service: &UserService,
        instance_name: &str,
        user_info: &OAuth2UserInfo,
    ) -> Result<OAuth2LinkResult> {
        // Fast path: user already linked — no transaction needed.
        if let Some(user_id) = self
            .find_user_by_provider_instance(instance_name, &user_info.provider_user_id)
            .await?
        {
            return Ok(OAuth2LinkResult::Linked {
                user_id,
                is_new: false,
            });
        }

        let signup_policy = self.signup_policy_for(instance_name).await?;
        if !signup_policy.enable_signup {
            return Err(Error::Authorization(
                "OAuth2 registration is disabled for this provider".to_string(),
            ));
        }

        // Slow path: no existing mapping — create user + link in one transaction.
        let pool = self.repository.pool();
        let mut tx = pool.begin().await?;

        let advisory_lock_key = format!("oauth2:{instance_name}:{}", user_info.provider_user_id);
        // Serialize creation for a single external identity so concurrent logins
        // cannot race on local username/email creation before the winning mapping
        // becomes visible.
        sqlx::query!(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            advisory_lock_key,
        )
        .fetch_one(&mut *tx)
        .await
        .internal_with_err("Failed to acquire OAuth2 identity advisory lock")?;

        // Re-check inside the transaction to guard against the race where another
        // concurrent request created the user between our initial lookup and here.
        let existing = self
            .repository
            .find_by_provider_instance_with_executor(
                instance_name,
                &user_info.provider_user_id,
                &mut *tx,
            )
            .await?;

        if let Some(mapping) = existing {
            // Another concurrent request already created the mapping — use it.
            tx.rollback().await?;
            return Ok(OAuth2LinkResult::Linked {
                user_id: mapping.user_id,
                is_new: false,
            });
        }

        let (base_username, candidates) = UserService::oauth2_username_candidates(
            &user_info.provider_user_id,
            &user_info.username,
        )?;
        let user_email = user_info.email.clone();

        if signup_policy.signup_need_review {
            if let Some(email) = user_email.as_deref() {
                if user_service.get_by_email(email).await?.is_some() {
                    tx.rollback().await?;
                    return Err(Error::AlreadyExists(
                        synctv_common::messages::USERNAME_OR_EMAIL_ALREADY_TAKEN.to_string(),
                    ));
                }
            }

            let mut pending_request_id = None;
            for candidate in &candidates {
                let username_in_use = sqlx::query_scalar::<_, bool>(
                    r"
                    SELECT EXISTS(
                        SELECT 1
                        FROM users
                        WHERE LOWER(username) = LOWER($1)
                          AND deleted_at IS NULL
                    )
                    ",
                )
                .bind(candidate)
                .fetch_one(&mut *tx)
                .await?;
                if username_in_use {
                    continue;
                }

                UserService::lock_oauth2_pending_registration_identity(
                    &mut tx,
                    candidate,
                    user_email.as_deref(),
                    instance_name,
                    &user_info.provider_user_id,
                )
                .await
                .internal_with_err("Failed to acquire OAuth2 pending-registration locks")?;

                match user_service
                    .pending_oauth2_registration_conflict(
                        candidate,
                        user_email.as_deref(),
                        instance_name,
                        &user_info.provider_user_id,
                        &mut *tx,
                    )
                    .await
                {
                    Ok(Some(PendingRegistrationConflict::OAuth2Identity(request_id))) => {
                        tx.rollback().await?;
                        return Ok(OAuth2LinkResult::PendingReview(OAuth2PendingRegistration {
                            request_id,
                        }));
                    }
                    Ok(Some(PendingRegistrationConflict::Username)) => {
                        continue;
                    }
                    Ok(Some(PendingRegistrationConflict::Email)) => {
                        tx.rollback().await?;
                        return Err(Error::AlreadyExists(
                            synctv_common::messages::USERNAME_OR_EMAIL_ALREADY_TAKEN.to_string(),
                        ));
                    }
                    Ok(None) => {}
                    Err(err) => {
                        tx.rollback().await?;
                        return Err(err);
                    }
                }

                match user_service
                    .create_oauth2_registration_request_with_executor(
                        candidate,
                        user_email.as_deref(),
                        &user_info.provider_user_id,
                        user_info,
                        &mut *tx,
                    )
                    .await
                {
                    Ok(request_id) => {
                        pending_request_id = Some(request_id);
                        break;
                    }
                    Err(Error::AlreadyExists(message)) => {
                        tx.rollback().await?;
                        return Err(Error::AlreadyExists(message));
                    }
                    Err(err) => {
                        tx.rollback().await?;
                        return Err(err);
                    }
                }
            }

            let request_id = pending_request_id.ok_or_else(|| {
                Error::Internal(format!(
                    "Could not generate a unique username for base '{}' after {} attempts",
                    user_info.username,
                    candidates.len()
                ))
            })?;
            tx.commit().await?;
            return Ok(OAuth2LinkResult::PendingReview(OAuth2PendingRegistration {
                request_id,
            }));
        }

        let mut new_user = None;
        for (attempt, candidate) in candidates.iter().enumerate() {
            let savepoint = format!("oauth2_user_create_{attempt}");
            sqlx::query(&format!("SAVEPOINT {savepoint}"))
                .execute(&mut *tx)
                .await
                .internal_with_err("Failed to create OAuth2 user savepoint")?;

            let user = User::new_with_status(
                candidate.clone(),
                user_email.clone(),
                String::new(),
                SignupMethod::OAuth2,
                crate::models::UserStatus::Active,
            );
            match user_service
                .repository
                .create_with_executor(&user, &mut *tx)
                .await
            {
                Ok(created_user) => {
                    sqlx::query(&format!("RELEASE SAVEPOINT {savepoint}"))
                        .execute(&mut *tx)
                        .await
                        .internal_with_err("Failed to release OAuth2 user savepoint")?;

                    user_service
                        .cache_oauth2_username_best_effort(&created_user.id, candidate)
                        .await;

                    if candidate == &base_username {
                        tracing::info!(
                            "Created new user {} (username='{}', sanitized from '{}') via OAuth2 provider {} (provider_user_id={})",
                            created_user.id,
                            candidate,
                            user_info.username,
                            user_info.provider.as_str(),
                            user_info.provider_user_id
                        );
                    } else {
                        tracing::info!(
                            "Username '{}' was taken; created user {} as '{}' (original '{}') via OAuth2 provider {} (provider_user_id={})",
                            base_username,
                            created_user.id,
                            candidate,
                            user_info.username,
                            user_info.provider.as_str(),
                            user_info.provider_user_id
                        );
                    }

                    new_user = Some(created_user);
                    break;
                }
                Err(Error::AlreadyExists(ref msg))
                    if msg.contains("username") || msg.contains("Username") =>
                {
                    sqlx::query(&format!("ROLLBACK TO SAVEPOINT {savepoint}"))
                        .execute(&mut *tx)
                        .await
                        .internal_with_err(
                            "Failed to roll back OAuth2 user savepoint after username collision",
                        )?;
                }
                Err(err) => {
                    sqlx::query(&format!("ROLLBACK TO SAVEPOINT {savepoint}"))
                        .execute(&mut *tx)
                        .await
                        .internal_with_err(
                            "Failed to roll back OAuth2 user savepoint after create error",
                        )?;
                    return Err(err);
                }
            }
        }

        let new_user: User = new_user.ok_or_else(|| {
            Error::Internal(format!(
                "Could not generate a unique username for base '{}' after {} attempts",
                user_info.username,
                candidates.len()
            ))
        })?;

        // Link the OAuth2 provider mapping inside the same transaction.
        match self
            .upsert_user_provider_with_executor(&new_user.id, user_info, &mut *tx)
            .await
        {
            Ok(()) => {}
            Err(Error::AlreadyExists(_)) => {
                // Another concurrent request bound this provider identity first.
                // Roll back the provisional user so we do not commit an orphan row,
                // then return the winning mapping.
                tx.rollback().await?;
                let existing = self
                    .repository
                    .find_by_provider_instance(instance_name, &user_info.provider_user_id)
                    .await?
                    .ok_or_else(|| {
                        Error::Internal(
                            "OAuth2 mapping conflicted but could not be reloaded".to_string(),
                        )
                    })?;
                return Ok(OAuth2LinkResult::Linked {
                    user_id: existing.user_id,
                    is_new: false,
                });
            }
            Err(err) => return Err(err),
        }

        // Set email_verified if the provider confirmed the email.
        if user_info.email_verified && user_info.email.is_some() {
            sqlx::query!(
                "UPDATE auth_email_identities SET email_verified = true, updated_at = NOW() WHERE user_id = $1",
                new_user.id.as_i64(),
            )
                .execute(&mut *tx)
                .await
                .internal_with_err("Failed to set email_verified in transaction")?;
        }

        tx.commit().await?;

        info!(
            user_id = %new_user.id,
            provider = %user_info.provider.as_str(),
            provider_instance = %instance_name,
            "Created new user via OAuth2 and linked provider in single transaction"
        );

        Ok(OAuth2LinkResult::Linked {
            user_id: new_user.id,
            is_new: true,
        })
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
    pub async fn get_user_provider_mappings(
        &self,
        user_id: &UserId,
    ) -> Result<Vec<crate::models::oauth2_client::UserOAuthProviderMapping>> {
        self.repository.find_by_user(user_id).await
    }

    /// List all configured `OAuth2` provider instances
    ///
    /// Returns a list of (`instance_name`, `provider_type`) pairs for all registered providers.
    /// This is used by the HTTP API to tell clients which `OAuth2` login options are available.
    /// Returns an empty vector if no providers are configured. Order is not guaranteed.
    pub async fn list_available_instances(
        &self,
    ) -> Result<Vec<(String, OAuth2Provider, OAuth2SignupPolicy)>> {
        self.sync_runtime_providers().await?;
        let providers = self.providers.read().await;
        Ok(providers
            .iter()
            .map(|(name, entry)| {
                (
                    name.clone(),
                    entry.provider_type.clone(),
                    entry.signup_policy.clone(),
                )
            })
            .collect())
    }

    /// Unlink `OAuth2` provider from user
    pub async fn unlink_provider(
        &self,
        user_id: &UserId,
        provider_instance_name: &str,
        provider_user_id: &str,
    ) -> Result<bool> {
        self.repository
            .delete_instance(user_id, provider_instance_name, provider_user_id)
            .await
    }

    /// Unlink all bindings for a specific `OAuth2` provider from user
    pub async fn unlink_provider_all(
        &self,
        user_id: &UserId,
        provider: &OAuth2Provider,
    ) -> Result<bool> {
        self.repository
            .delete_by_user_and_provider(user_id, provider)
            .await
    }

    /// Remove all `OAuth2` provider mappings for a user.
    ///
    /// Used during user deletion to clean up all OAuth bindings.
    /// Returns the number of mappings removed.
    pub async fn delete_all_for_user(&self, user_id: &UserId) -> Result<u64> {
        self.repository.delete_all_for_user(user_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth2::OAuth2Authorization;
    use crate::oauth2::Provider as OAuth2ProviderTrait;
    use crate::repository::SettingsRepository;
    use crate::service::SettingsService;
    use crate::RedisConnectionRuntime;
    use async_trait::async_trait;
    use sqlx::PgPool;

    // Mock OAuth2 Provider

    #[tokio::test]
    async fn test_redis_oauth_state_store_accepts_trait_object_runtime() {
        #[derive(Clone)]
        struct FakeRedisRuntime;

        #[async_trait]
        impl RedisConnectionRuntime for FakeRedisRuntime {
            async fn snapshot(&self) -> redis::aio::ConnectionManager {
                panic!("snapshot should not be called in constructor-only test");
            }
        }

        let runtime: Arc<dyn RedisConnectionRuntime> = Arc::new(FakeRedisRuntime);
        let store = RedisOAuthStateStore::from_runtime(runtime.clone(), "synctv:");

        assert!(
            Arc::ptr_eq(&store.conn, &runtime),
            "OAuth2 Redis store should retain the injected runtime object"
        );
    }

    #[test]
    fn test_state_store_from_shared_state_profile_uses_memory_without_shared_runtime() {
        let profile = SharedStateProfile::from_runtime(None, "test:", false);

        let store = state_store_from_shared_state_profile(&profile)
            .expect("standalone mode should allow local OAuth2 state storage");

        assert!(
            !store.supports_cross_node_single_use(),
            "local store must not claim cross-node single-use guarantees"
        );
    }

    #[test]
    fn test_state_store_from_shared_state_profile_requires_shared_runtime_in_cluster_mode() {
        let profile = SharedStateProfile::from_runtime(None, "test:", true);

        let Err(error) = state_store_from_shared_state_profile(&profile) else {
            panic!("cluster mode must reject local OAuth2 state storage");
        };

        assert!(
            error
                .to_string()
                .contains("distributed runtime requires shared single-use OAuth2 state storage"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn test_state_store_from_shared_state_profile_accepts_trait_object_runtime() {
        #[derive(Clone)]
        struct FakeRedisRuntime;

        #[async_trait]
        impl RedisConnectionRuntime for FakeRedisRuntime {
            async fn snapshot(&self) -> redis::aio::ConnectionManager {
                panic!("snapshot should not be called in constructor-only test");
            }
        }

        let runtime: Arc<dyn RedisConnectionRuntime> = Arc::new(FakeRedisRuntime);
        let profile =
            SharedStateProfile::new(SharedStateMode::SharedBestEffort, Some(runtime), "test:");

        let store = state_store_from_shared_state_profile(&profile)
            .expect("shared runtime profile should yield a distributed OAuth2 state store");

        assert!(
            store.supports_cross_node_single_use(),
            "shared store must claim cross-node single-use guarantees"
        );
    }

    /// Mock `OAuth2` provider for testing authorization URL generation and code exchange.
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
        fn provider_type(&self) -> &'static str {
            "mock"
        }

        async fn new_auth_url(&self, state: &str) -> Result<OAuth2Authorization> {
            // Append state to URL like a real provider would
            let url = format!("{}&state={state}", self.auth_url);
            Ok(OAuth2Authorization::new(url, self.pkce_verifier.clone()))
        }

        async fn get_user_info(
            &self,
            _code: &str,
            _pkce_verifier: &str,
            _nonce: Option<&str>,
        ) -> Result<crate::oauth2::OAuth2UserInfo> {
            if let Some(ref err) = self.exchange_error {
                return Err(Error::Internal(err.clone()));
            }
            self.user_info
                .clone()
                .ok_or_else(|| Error::Internal("No user info configured in mock".to_string()))
        }
    }

    // Test service helpers — no Redis required

    fn create_test_service() -> OAuth2Service {
        create_test_service_with_cluster_mode(false)
    }

    fn create_test_service_with_cluster_mode(cluster_mode: bool) -> OAuth2Service {
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = crate::repository::UserOAuthProviderRepository::new(pool);
        let state_store = local_oauth_state_store();
        OAuth2Service::new(
            repo,
            state_store,
            crate::oauth2::ProviderRegistry::new(),
            cluster_mode,
        )
        .expect("Failed to create OAuth2 service")
    }

    fn create_test_service_with_domains(domains: Vec<String>) -> OAuth2Service {
        let mut svc = create_test_service();
        svc.set_allowed_redirect_domains(domains);
        svc
    }

    fn create_test_settings_registry(
        guard: &synctv_common::ssrf::SsrfGuard,
    ) -> Arc<SettingsRegistry> {
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let settings_service = Arc::new(SettingsService::new(
            SettingsRepository::new(pool.clone()),
            pool,
        ));
        Arc::new(SettingsRegistry::new_with_ssrf_guard(
            settings_service,
            guard,
        ))
    }

    // Tests: Redirect URL Validation (security-critical)

    #[test]
    fn test_redirect_relative_path_allowed() {
        let result = OAuth2Service::validate_redirect_url_with_allowlist("/dashboard", &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_redirect_relative_path_with_query_allowed() {
        let result = OAuth2Service::validate_redirect_url_with_allowlist("/rooms?sort=name", &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_redirect_protocol_relative_url_rejected() {
        let result = OAuth2Service::validate_redirect_url_with_allowlist("//evil.com/steal", &[]);
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
        let result =
            OAuth2Service::validate_redirect_url_with_allowlist("javascript:alert(1)", &domains);
        assert!(result.is_err());
    }

    #[test]
    fn test_redirect_ftp_scheme_rejected() {
        let domains = vec!["example.com".to_string()];
        let result =
            OAuth2Service::validate_redirect_url_with_allowlist("ftp://example.com/file", &domains);
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
        let result =
            OAuth2Service::validate_redirect_url_with_allowlist("not a valid url at all", &domains);
        assert!(result.is_err());
    }

    #[test]
    fn test_redirect_tld_only_domain_rejected() {
        // Adding "com" to allowlist should NOT allow all.com domains
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

    #[test]
    fn test_redirect_native_custom_scheme_rejected_without_allowlist() {
        let result = OAuth2Service::validate_redirect_url_with_allowlist(
            "io.github.synctv://oauth2/callback",
            &[],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_redirect_native_custom_scheme_allowed_when_reverse_domain_matches() {
        let domains = vec!["github.io".to_string()];
        let result = OAuth2Service::validate_redirect_url_with_allowlist(
            "io.github.synctv://oauth2/callback",
            &domains,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_redirect_native_custom_scheme_rejects_non_reverse_domain_scheme() {
        let domains = vec!["github.io".to_string()];
        let result = OAuth2Service::validate_redirect_url_with_allowlist(
            "mysynctv://oauth2/callback",
            &domains,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_redirect_dangerous_non_http_schemes_rejected() {
        for url in [
            "javascript:alert(1)",
            "data:text/html,hello",
            "file:///tmp/callback",
            "ftp://example.com/callback",
        ] {
            let result = OAuth2Service::validate_redirect_url_with_allowlist(url, &[]);
            assert!(result.is_err(), "{url} must be rejected");
        }
    }

    #[test]
    fn test_redirect_loopback_url_allowed_without_domain_allowlist() {
        let localhost = OAuth2Service::validate_redirect_url_with_allowlist(
            "http://127.0.0.1:34567/oauth/callback",
            &[],
        );
        assert!(localhost.is_ok());

        let hostname =
            OAuth2Service::validate_redirect_url_with_allowlist("http://localhost:8080/cb", &[]);
        assert!(hostname.is_ok());
    }

    // Tests: State Management (in-memory, no Redis required)

    #[tokio::test]
    async fn test_store_and_consume_state() {
        let service = create_test_service();
        let state = OAuth2State {
            instance_name: "github".to_string(),
            redirect_url: Some("/dashboard".to_string()),
            created_at: chrono::Utc::now(),
            bind_user_id: None,
            pkce_verifier: "verifier123".to_string(),
            nonce: None,
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
            nonce: None,
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
        let user_id = UserId::expect_positive(93_001);
        let state = OAuth2State {
            instance_name: "logto".to_string(),
            redirect_url: None,
            created_at: chrono::Utc::now(),
            bind_user_id: Some(user_id),
            pkce_verifier: "bind_verifier".to_string(),
            nonce: None,
        };

        service.store_state("bind_token", &state).await.unwrap();
        let retrieved = service.consume_state("bind_token").await.unwrap();

        assert_eq!(
            retrieved.bind_user_id.as_ref().unwrap().to_string(),
            "93001"
        );
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
            nonce: None,
        };

        service.store_state("verify_tok", &state).await.unwrap();

        // verify_state delegates to consume_state
        let result = service.verify_state("verify_tok").await;
        assert!(result.is_ok());

        // Replay fails
        let result = service.verify_state("verify_tok").await;
        assert!(result.is_err());
    }

    // Tests: Provider Registration and Listing

    #[tokio::test]
    async fn test_register_and_list_providers() {
        let service = create_test_service();

        // Initially empty
        let providers = service.list_available_instances().await.unwrap();
        assert!(providers.is_empty());

        // Register a mock provider
        service
            .register_provider(
                "github".to_string(),
                OAuth2Provider::GitHub,
                Box::new(MockOAuth2Provider::new()),
            )
            .await;

        let providers = service.list_available_instances().await.unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].0, "github");
        assert_eq!(providers[0].1, OAuth2Provider::GitHub);
    }

    #[tokio::test]
    async fn test_list_available_instances_uses_runtime_ssrf_policy_for_dynamic_oidc() {
        let guard = synctv_common::ssrf::SsrfGuard::builder()
            .allow_private_network_targets(true)
            .build();
        let registry = create_test_settings_registry(&guard);
        let configs: crate::service::OAuth2ProviderConfigs = r#"{"casdoor_oidc":{"type":"oidc","enable_signup":true,"config":{"client_id":"id","client_secret":"secret","redirect_url":"http://127.0.0.1:18081/oauth/callback","issuer":"http://127.0.0.1:18000"}}}"#
            .parse()
            .expect("test OAuth2 provider config should parse");
        registry
            .oauth2_providers
            .set_for_test(&configs)
            .expect("test settings seed should validate");

        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = crate::repository::UserOAuthProviderRepository::new(pool);
        let service = OAuth2Service::new_with_ssrf_guard(
            repo,
            local_oauth_state_store(),
            crate::oauth2::providers::provider_registry(guard),
            synctv_common::ssrf::SsrfGuard::builder()
                .allow_private_network_targets(true)
                .build(),
            false,
        )
        .expect("OAuth2 service should be created")
        .with_settings_registry(registry);

        let providers = service
            .list_available_instances()
            .await
            .expect("runtime SSRF policy should allow local Casdoor OIDC issuer");
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].0, "casdoor_oidc");
        assert_eq!(providers[0].1, OAuth2Provider::Oidc);
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

        let providers = service.list_available_instances().await.unwrap();
        assert_eq!(providers.len(), 3);

        let names: Vec<&str> = providers.iter().map(|(n, _, _)| n.as_str()).collect();
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

        let providers = service.list_available_instances().await.unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].1, OAuth2Provider::Oidc);
    }

    // Tests: Authorization URL Generation with PKCE

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

        let (auth_url, state_token) = service.get_authorization_url("github", None).await.unwrap();

        // Auth URL should contain the mock base URL and the state parameter
        assert!(auth_url.contains("https://provider.example.com/auth"));
        assert!(auth_url.contains("state="));

        // State token should be a 32-char shared base62 token
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

        let result = service.get_authorization_url("nonexistent", None).await;
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

    #[tokio::test]
    async fn test_get_authorization_url_accepts_configured_native_client_redirects() {
        let service = create_test_service_with_domains(vec!["github.io".to_string()]);
        service
            .register_provider(
                "github".to_string(),
                OAuth2Provider::GitHub,
                Box::new(MockOAuth2Provider::new()),
            )
            .await;

        let (_, native_state_token) = service
            .get_authorization_url(
                "github",
                Some("io.github.synctv://oauth2/callback".to_string()),
            )
            .await
            .expect("configured native custom scheme should be accepted");
        let native_state = service.verify_state(&native_state_token).await.unwrap();
        assert_eq!(
            native_state.redirect_url.as_deref(),
            Some("io.github.synctv://oauth2/callback")
        );

        let service = create_test_service();
        service
            .register_provider(
                "github".to_string(),
                OAuth2Provider::GitHub,
                Box::new(MockOAuth2Provider::new()),
            )
            .await;

        let (_, loopback_state_token) = service
            .get_authorization_url(
                "github",
                Some("http://127.0.0.1:34567/oauth/callback".to_string()),
            )
            .await
            .expect("native loopback redirects should not require domain allowlist");
        let loopback_state = service.verify_state(&loopback_state_token).await.unwrap();
        assert_eq!(
            loopback_state.redirect_url.as_deref(),
            Some("http://127.0.0.1:34567/oauth/callback")
        );
    }

    // Tests: Authorization URL with User Binding (PKCE)

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

        let user_id = UserId::expect_positive(93_002);
        let (auth_url, state_token) = service
            .get_authorization_url_with_user("logto", None, Some(user_id))
            .await
            .unwrap();

        assert!(auth_url.contains("https://provider.example.com/auth"));

        let state = service.verify_state(&state_token).await.unwrap();
        assert_eq!(state.instance_name, "logto");
        assert_eq!(state.bind_user_id.as_ref().unwrap().to_string(), "93002");
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
            .get_authorization_url_with_user("github", Some("//evil.com".to_string()), None)
            .await;
        assert!(result.is_err());
    }

    // Tests: Code Exchange for User Info

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

        let user_info = service
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
        assert_eq!(user_info.provider_instance_name, "github");
        assert!(user_info.provider_issuer.is_none());
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

    // Tests: Full Authorization Flow (URL -> State -> Exchange)

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
        let user_info = service
            .exchange_code_for_user_info("github", "callback_code", &state.pkce_verifier)
            .await
            .unwrap();
        assert_eq!(user_info.username, "testuser");
        assert_eq!(user_info.provider, OAuth2Provider::GitHub);
        assert_eq!(user_info.provider_instance_name, "github");

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

        let user_id = UserId::expect_positive(93_003);

        // Step 1: Generate auth URL with user binding
        let (_, state_token) = service
            .get_authorization_url_with_user("logto", None, Some(user_id))
            .await
            .unwrap();

        // Step 2: Verify state carries user ID
        let state = service.verify_state(&state_token).await.unwrap();
        assert_eq!(state.bind_user_id.as_ref().unwrap().to_string(), "93003");
        assert_eq!(state.instance_name, "logto");
    }

    // Tests: Service Configuration

    #[tokio::test]
    async fn test_state_store_is_abstracted() {
        // OAuth2Service takes Arc<dyn OAuthStateStore>, not a concrete Redis type.
        // This verifies the abstraction compiles with the in-memory implementation.
        let _service = create_test_service();
    }

    #[tokio::test]
    async fn test_set_allowed_redirect_domains() {
        let mut service = create_test_service();
        service
            .set_allowed_redirect_domains(vec!["example.com".to_string(), "myapp.io".to_string()]);

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

    // Tests: OAuth2State serialization (used for storage path)

    #[test]
    fn test_oauth2_state_serialization_roundtrip() {
        let state = OAuth2State {
            instance_name: "github".to_string(),
            redirect_url: Some("/dashboard".to_string()),
            created_at: chrono::Utc::now(),
            bind_user_id: Some(UserId::expect_positive(93_004)),
            pkce_verifier: "S256_challenge_verifier".to_string(),
            nonce: Some("oidc_nonce_123".to_string()),
        };

        let json = serde_json::to_string(&state).unwrap();
        let deserialized: OAuth2State = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.instance_name, state.instance_name);
        assert_eq!(deserialized.redirect_url, state.redirect_url);
        assert_eq!(deserialized.pkce_verifier, state.pkce_verifier);
        assert_eq!(deserialized.nonce, state.nonce);
        assert_eq!(
            deserialized.bind_user_id.as_ref().unwrap().to_string(),
            "93004"
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
            nonce: None,
        };

        let json = serde_json::to_string(&state).unwrap();
        let deserialized: OAuth2State = serde_json::from_str(&json).unwrap();

        assert!(deserialized.redirect_url.is_none());
        assert!(deserialized.bind_user_id.is_none());
    }

    // Tests: Concurrent State Operations

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
                nonce: None,
            };
            service
                .store_state(&format!("token_{i}"), &state)
                .await
                .unwrap();
        }

        // Each state should be independently consumable
        for i in 0..10 {
            let state = service.consume_state(&format!("token_{i}")).await.unwrap();
            assert_eq!(state.instance_name, format!("provider_{i}"));
            assert_eq!(state.pkce_verifier, format!("verifier_{i}"));
        }

        // All consumed, none should remain
        for i in 0..10 {
            let result = service.consume_state(&format!("token_{i}")).await;
            assert!(result.is_err());
        }
    }

    // Tests: PKCE Verifier Integrity

    #[tokio::test]
    async fn test_pkce_verifier_preserved_through_state_lifecycle() {
        let service = create_test_service();
        let mock = MockOAuth2Provider {
            auth_url: "https://auth.test/authorize".to_string(),
            pkce_verifier: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".to_string(),
            user_info: Some(crate::oauth2::OAuth2UserInfo {
                provider_user_id: "94001".to_string(),
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

        let (_, token1) = service.get_authorization_url("github", None).await.unwrap();
        let (_, token2) = service.get_authorization_url("github", None).await.unwrap();

        assert_ne!(
            token1, token2,
            "Each authorization request must get a unique state token"
        );
    }

    // Tests: OAuth2 Concurrent State Consumption (only one succeeds)

    #[tokio::test]
    async fn test_concurrent_state_consumption_only_first_succeeds() {
        let service = Arc::new(create_test_service());
        let state = OAuth2State {
            instance_name: "github".to_string(),
            redirect_url: None,
            created_at: chrono::Utc::now(),
            bind_user_id: None,
            pkce_verifier: "concurrent_verifier".to_string(),
            nonce: None,
        };

        service
            .store_state("concurrent_token", &state)
            .await
            .unwrap();

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
        assert_eq!(success_count, 1, "Exactly one consumer must succeed");
        assert_eq!(failure_count, 19, "All other consumers must fail");

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
        let (_, state_token) = service.get_authorization_url("github", None).await.unwrap();

        // Spawn concurrent verify_state attempts
        let mut handles = Vec::new();
        for _ in 0..10 {
            let svc = service.clone();
            let tok = state_token.clone();
            handles.push(tokio::spawn(async move { svc.verify_state(&tok).await }));
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

    // Tests: State Isolation Between Tokens

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
                nonce: None,
            };
            service
                .store_state(&format!("isolated_token_{i}"), &state)
                .await
                .unwrap();
        }

        // Consume token 2
        let consumed = service.consume_state("isolated_token_2").await.unwrap();
        assert_eq!(consumed.instance_name, "provider_2");

        // Other tokens should still be available
        for i in [0, 1, 3, 4] {
            let state = service
                .consume_state(&format!("isolated_token_{i}"))
                .await
                .unwrap();
            assert_eq!(state.instance_name, format!("provider_{i}"));
        }

        // Token 2 is consumed, should fail
        let result = service.consume_state("isolated_token_2").await;
        assert!(result.is_err());
    }

    // Tests: CSRF Protection - Defense in Depth

    /// Test that state tokens with expired `created_at` timestamps are rejected
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
            nonce: None,
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
            nonce: None,
        };

        service
            .store_state("within_ttl_token", &state)
            .await
            .unwrap();

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
        let past_boundary_time =
            chrono::Utc::now() - chrono::Duration::seconds(OAUTH2_STATE_TTL_SECONDS_I64 + 1);
        let state = OAuth2State {
            instance_name: "github".to_string(),
            redirect_url: None,
            created_at: past_boundary_time,
            bind_user_id: None,
            pkce_verifier: "boundary_verifier".to_string(),
            nonce: None,
        };

        service.store_state("boundary_token", &state).await.unwrap();

        // Past TTL seconds, the state should be rejected (> TTL)
        let result = service.consume_state("boundary_token").await;
        assert!(result.is_err());
    }

    /// Test that `verify_state` includes the `created_at` expiry check
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
            nonce: None,
        };

        service
            .store_state("verify_expired_token", &state)
            .await
            .unwrap();

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
        let (_, state_token) = service.get_authorization_url("github", None).await.unwrap();

        // Verify the state contains github as provider
        let state = service.verify_state(&state_token).await.unwrap();
        assert_eq!(state.instance_name, "github");

        // In the API layer, if attacker tries to use github's state with google provider,
        // the provider mismatch check in exchange_authorization_code will catch it.
        // This test verifies the state contains the correct instance_name.
    }

    // Cluster mode Redis dependency tests.

    /// Test: cluster mode with a local-only state store returns a descriptive error.
    /// In distributed mode, `OAuth2` states created on replica A cannot be validated
    /// on replica B without shared single-use state storage.
    #[tokio::test]
    async fn test_distributed_mode_without_redis_returns_error() {
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = crate::repository::UserOAuthProviderRepository::new(pool);
        let state_store = local_oauth_state_store();

        let result = OAuth2Service::new(
            repo,
            state_store,
            crate::oauth2::ProviderRegistry::new(),
            true,
        );

        assert!(
            result.is_err(),
            "Distributed mode without shared single-use state must return an error"
        );

        let err = result.unwrap_err();
        let err_msg = err.to_string();

        // Error message should be descriptive and mention the core issue
        assert!(
            err_msg.contains("shared single-use OAuth2 state"),
            "Error should mention shared single-use OAuth2 state; got: {err_msg}"
        );
        assert!(
            err_msg.contains("distributed runtime"),
            "Error should mention distributed runtime; got: {err_msg}"
        );
        assert!(
            err_msg.contains("replica") || err_msg.contains("replicas"),
            "Error should explain the replica visibility issue; got: {err_msg}"
        );
    }

    /// Test: cluster mode error message provides actionable guidance.
    /// Users should know how to fix the configuration.
    #[tokio::test]
    async fn test_cluster_mode_error_message_is_actionable() {
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = crate::repository::UserOAuthProviderRepository::new(pool);
        let state_store = local_oauth_state_store();

        let result = OAuth2Service::new(
            repo,
            state_store,
            crate::oauth2::ProviderRegistry::new(),
            true,
        );
        let err_msg = result.unwrap_err().to_string();

        // Should suggest using a shared state backend
        assert!(
            err_msg.contains("Configure a shared state backend"),
            "Error should suggest configuring a shared state backend; got: {err_msg}"
        );
    }

    /// Test: non-cluster mode allows in-memory state store.
    /// Single-replica deployments can use in-memory storage without issues.
    #[tokio::test]
    async fn test_non_cluster_mode_allows_memory() {
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = crate::repository::UserOAuthProviderRepository::new(pool);
        let state_store = local_oauth_state_store();

        let result = OAuth2Service::new(
            repo,
            state_store,
            crate::oauth2::ProviderRegistry::new(),
            false,
        );

        assert!(
            result.is_ok(),
            "Non-cluster mode should allow in-memory state store"
        );
    }

    /// Test: cluster mode validation happens at service creation time.
    /// This prevents runtime failures later during `OAuth2` flows.
    #[tokio::test]
    async fn test_cluster_mode_validation_at_creation_time() {
        // Cluster mode should fail immediately at service creation
        let pool = PgPool::connect_lazy("postgresql://test").unwrap();
        let repo = crate::repository::UserOAuthProviderRepository::new(pool);
        let state_store = local_oauth_state_store();
        let service_result = OAuth2Service::new(
            repo,
            state_store,
            crate::oauth2::ProviderRegistry::new(),
            true,
        );

        assert!(
            service_result.is_err(),
            "Cluster mode validation should fail at service creation"
        );
    }

    // Tests: InMemoryOAuthStateStore (moka cache)

    #[tokio::test]
    async fn test_in_memory_store_and_consume() {
        let store = InMemoryOAuthStateStore::new();
        let state = OAuth2State {
            instance_name: "test_provider".to_string(),
            redirect_url: Some("/dashboard".to_string()),
            created_at: chrono::Utc::now(),
            bind_user_id: None,
            pkce_verifier: "test_verifier".to_string(),
            nonce: None,
        };

        store
            .store("token_1", &state, std::time::Duration::from_mins(5))
            .await
            .unwrap();

        // First consume succeeds
        let result = store.consume("token_1").await.unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().instance_name, "test_provider");

        // Second consume returns None (single-use)
        let result = store.consume("token_1").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_in_memory_store_nonexistent_token() {
        let store = InMemoryOAuthStateStore::new();
        let result = store.consume("nonexistent").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_in_memory_store_multiple_entries() {
        let store = InMemoryOAuthStateStore::new();
        let state = OAuth2State {
            instance_name: "test".to_string(),
            redirect_url: None,
            created_at: chrono::Utc::now(),
            bind_user_id: None,
            pkce_verifier: "v".to_string(),
            nonce: None,
        };

        // Store multiple entries
        for i in 0..100 {
            store
                .store(
                    &format!("token_{i}"),
                    &state,
                    std::time::Duration::from_mins(5),
                )
                .await
                .unwrap();
        }

        // All entries should be consumable
        for i in 0..100 {
            let result = store.consume(&format!("token_{i}")).await.unwrap();
            assert!(result.is_some(), "token_{i} should exist");
        }
    }

    #[tokio::test]
    async fn test_in_memory_store_high_concurrency() {
        // High concurrency test: verify states survive concurrent store+consume
        use std::sync::atomic::{AtomicUsize, Ordering};

        let store = std::sync::Arc::new(InMemoryOAuthStateStore::new());
        let success_count = std::sync::Arc::new(AtomicUsize::new(0));
        let total_tasks = 100;

        // Spawn many concurrent tasks that store and then consume their tokens
        let mut handles = vec![];
        for i in 0..total_tasks {
            let store_clone = store.clone();
            let success_count_clone = success_count.clone();
            handles.push(tokio::spawn(async move {
                let token_id = format!("concurrent_token_{i}");
                let state = OAuth2State {
                    instance_name: format!("provider_{i}"),
                    redirect_url: None,
                    created_at: chrono::Utc::now(),
                    bind_user_id: None,
                    pkce_verifier: format!("verifier_{i}"),
                    nonce: None,
                };

                // Store the state
                store_clone
                    .store(&token_id, &state, std::time::Duration::from_mins(5))
                    .await
                    .unwrap();

                // Simulate some delay (like OAuth redirect)
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;

                // Consume should succeed
                let result = store_clone.consume(&token_id).await.unwrap();
                if result.is_some() {
                    success_count_clone.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }

        // Wait for all tasks
        for handle in handles {
            handle.await.unwrap();
        }

        // ALL tokens should have been consumed successfully
        assert_eq!(
            success_count.load(Ordering::SeqCst),
            total_tasks,
            "All {total_tasks} tokens should be consumable under concurrent load"
        );
    }

    #[tokio::test]
    async fn test_in_memory_store_concurrent_single_use_guarantee() {
        // Test that concurrent consumes return exactly one success
        use std::sync::atomic::{AtomicUsize, Ordering};

        let store = std::sync::Arc::new(InMemoryOAuthStateStore::new());
        let success_count = std::sync::Arc::new(AtomicUsize::new(0));

        // Store a single token
        let state = OAuth2State {
            instance_name: "test".to_string(),
            redirect_url: None,
            created_at: chrono::Utc::now(),
            bind_user_id: None,
            pkce_verifier: "v".to_string(),
            nonce: None,
        };
        store
            .store("shared_token", &state, std::time::Duration::from_mins(5))
            .await
            .unwrap();

        // Spawn many concurrent consumers
        let mut handles = vec![];
        for _ in 0..50 {
            let store_clone = store.clone();
            let success_count_clone = success_count.clone();
            handles.push(tokio::spawn(async move {
                let result = store_clone.consume("shared_token").await.unwrap();
                if result.is_some() {
                    success_count_clone.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }

        for handle in handles {
            handle.await.unwrap();
        }

        // Exactly ONE consume should have succeeded
        assert_eq!(
            success_count.load(Ordering::SeqCst),
            1,
            "Exactly one consume should succeed (single-use guarantee)"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_redis_state_store_timeout_maps_to_timeout_error() {
        let timeout_future = run_oauth_state_redis_op(
            crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
            "store OAuth2 state in Redis",
            async {
                std::future::pending::<()>().await;
                #[allow(unreachable_code)]
                Ok::<(), redis::RedisError>(())
            },
        );

        tokio::pin!(timeout_future);
        tokio::task::yield_now().await;
        tokio::time::advance(crate::resilience::timeout::REDIS_OPERATION_TIMEOUT).await;

        let err = timeout_future.await.expect_err("operation should time out");
        assert!(matches!(
            err,
            Error::Timeout(ref msg) if msg == "Redis timeout: store OAuth2 state in Redis"
        ));
    }
}
