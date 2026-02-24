//! Service initialization and dependency injection

use std::sync::Arc;

use sqlx::PgPool;
use tracing::{error, info, warn};

use crate::{
    bootstrap::RedisHandles,
    cache::{
        CacheInvalidationService, CacheL2Backend, CacheManager,
        NoopCacheL2, RedisCacheL2, RoomCache, UserCache, UsernameCache,
    },
    repository::{UserOAuthProviderRepository, ProviderInstanceRepository, UserProviderCredentialRepository, SettingsRepository, NotificationRepository},
    service::{
        ContentFilter, JwtService, OAuth2Service, RemoteProviderManager, RateLimitConfig,
        RateLimiter, UserService, RoomService, ProvidersManager,
        SettingsService, SettingsRegistry, EmailService, EmailTokenService, EmailConfig, PublishKeyService, UserNotificationService,
        AuditService, AuditFlushHandle,
    },
    Config,
};

/// Container for all initialized services
#[derive(Clone)]
pub struct Services {
    /// User authentication and management service
    pub user_service: Arc<UserService>,
    /// Room management service
    pub room_service: Arc<RoomService>,
    /// JWT token service
    pub jwt_service: JwtService,
    /// Rate limiter (uses Redis when available)
    pub rate_limiter: RateLimiter,
    /// Rate limit configuration
    pub rate_limit_config: RateLimitConfig,
    /// Content filter for chat and danmaku
    pub content_filter: ContentFilter,
    /// Provider instance manager
    pub provider_instance_manager: Arc<RemoteProviderManager>,
    /// Provider instances repository
    pub provider_instance_repo: Arc<ProviderInstanceRepository>,
    /// User provider credential repository
    pub user_provider_credential_repo: Arc<UserProviderCredentialRepository>,
    /// Providers manager
    pub providers_manager: Arc<ProvidersManager>,
    /// `OAuth2` service (optional, requires configuration)
    pub oauth2_service: Option<Arc<OAuth2Service>>,
    /// Settings service
    pub settings_service: Arc<SettingsService>,
    /// Settings registry with type-safe setting variables
    pub settings_registry: Arc<SettingsRegistry>,
    /// Email service (optional, requires SMTP configuration)
    pub email_service: Option<Arc<EmailService>>,
    /// Email token service for verification codes (optional, requires SMTP configuration)
    pub email_token_service: Option<Arc<EmailTokenService>>,
    /// Publish key service for RTMP streaming
    pub publish_key_service: Arc<PublishKeyService>,
    /// User notification service
    pub notification_service: Arc<UserNotificationService>,
    /// Audit logging service for security and compliance
    pub audit_service: Arc<AuditService>,
    /// Cache invalidation service for cross-replica cache sync
    pub cache_invalidation: Arc<CacheInvalidationService>,
    /// Cache manager coordinating all cache layers
    pub cache_manager: CacheManager,
    /// Shared Redis connection (optional in standalone mode).
    ///
    /// In Sentinel mode, the background health check hot-swaps the inner
    /// `ConnectionManager` on failover. In Standalone mode the inner
    /// `ConnectionManager` handles transient reconnections transparently.
    ///
    /// Use `.read().await.clone()` to obtain a working connection handle.
    pub redis_conn: Option<Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>>,
    /// Shared Redis client (for operations that need a `Client`, e.g. Pub/Sub).
    pub redis_client: Option<redis::Client>,
    /// `CancellationToken` for settings listen task (cancel on shutdown)
    pub settings_cancel: tokio_util::sync::CancellationToken,
    /// Settings listen task handle (joined on shutdown).
    /// Wrapped in `Arc<Mutex<Option<...>>>` so `Services` remains `Clone`.
    /// Take the handle out of the `Option` to join it on shutdown.
    pub settings_listen_task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Audit flush handle for graceful shutdown of audit logging
    pub audit_flush_handle: Arc<tokio::sync::Mutex<Option<AuditFlushHandle>>>,
    /// Credential encryption for protecting sensitive data (optional)
    pub credential_encryption: Option<crate::service::CredentialEncryption>,
}

impl Services {
    /// Return a plain `ConnectionManager` snapshot from the shared connection.
    ///
    /// Returns `None` when Redis is not configured (standalone mode without Redis).
    pub async fn redis_conn_snapshot(&self) -> Option<redis::aio::ConnectionManager> {
        match &self.redis_conn {
            Some(conn) => Some(conn.read().await.clone()),
            None => None,
        }
    }
}

/// Initialize all core services
///
/// The caller must supply optional `RedisHandles` (created by `init_redis`)
/// and a pre-built `CacheInvalidationService` so that the same instance (with
/// the correct cluster node ID) is shared across every component.  The caller
/// is also responsible for calling `.start()` on the cache invalidation service
/// after this function returns, so there is exactly one Redis subscriber.
///
/// When `redis_handles` is `None` (standalone mode without Redis), all services
/// use in-memory fallbacks.
pub async fn init_services(
    pool: PgPool,
    config: &Config,
    redis_handles: Option<RedisHandles>,
    cache_invalidation: Arc<CacheInvalidationService>,
) -> Result<Services, anyhow::Error> {
    info!("Initializing services...");

    // Initialize JWT service
    info!("Loading JWT keys...");
    let jwt_service = load_jwt_service(config)?;
    info!("JWT service initialized");

    // Extract a plain ConnectionManager snapshot for passing to individual services.
    //
    // IMPORTANT (Sentinel mode): This snapshot is taken once at init time. In Sentinel
    // mode, the background health check hot-swaps the ConnectionManager inside
    // `redis_handles.conn` (the Arc<RwLock<>>) on failover. However, services that
    // store this snapshot will keep using the old ConnectionManager until they are
    // recreated. ConnectionManager handles *transient* reconnection internally, but
    // it cannot discover a NEW master after Sentinel failover.
    //
    // For long-lived operations in Sentinel deployments, callers should prefer
    // `Services::redis_conn_snapshot()` to obtain a fresh handle that reflects the
    // latest Sentinel failover, rather than caching this init-time snapshot.
    let redis_conn_plain: Option<redis::aio::ConnectionManager> = match &redis_handles {
        Some(h) => Some(h.conn_snapshot().await),
        None => None,
    };
    let redis_client: Option<redis::Client> = redis_handles.as_ref().map(|h| h.client.clone());

    // Create L2 cache backend (Redis or Noop)
    //
    // In Sentinel mode, use the shared Arc<RwLock<ConnectionManager>> so that
    // the L2 backend automatically follows Sentinel failover without holding a
    // stale snapshot.
    let cache_l2: Arc<dyn CacheL2Backend> = if let Some(ref rh) = redis_handles {
        Arc::new(RedisCacheL2::new_shared(rh.conn.clone()))
    } else {
        Arc::new(NoopCacheL2)
    };
    info!("Cache L2 backend: {}", cache_l2.backend_name());

    // Initialize username cache (using config values)
    let username_cache = UsernameCache::new(
        cache_l2.clone(),
        format!("{}username:", config.redis.key_prefix),
        config.cache.username_cache_capacity as usize,
        config.cache.username_cache_ttl_seconds,
    )
    .with_invalidation_service(cache_invalidation.clone());
    info!("Username cache initialized (capacity={}, ttl={}s)",
        config.cache.username_cache_capacity, config.cache.username_cache_ttl_seconds);

    // Initialize user and room L1/L2 caches (using config values)
    let user_cache = Arc::new(
        UserCache::new(
            cache_l2.clone(),
            config.cache.l1_capacity,
            config.cache.l1_ttl_seconds,
            config.cache.l2_ttl_seconds,
            format!("{}user:", config.redis.key_prefix),
        )?
    );
    let room_cache = Arc::new(
        RoomCache::new(
            cache_l2.clone(),
            config.cache.l1_capacity,
            config.cache.l1_ttl_seconds,
            config.cache.l2_ttl_seconds,
            format!("{}room:", config.redis.key_prefix),
        )?
    );
    info!("User and room caches initialized (l1_capacity={}, l1_ttl={}s, l2_ttl={}s)",
        config.cache.l1_capacity, config.cache.l1_ttl_seconds, config.cache.l2_ttl_seconds);

    // Determine if cluster mode is active (used for startup warnings below)
    let cluster_mode = config.cluster.enabled || !config.server.cluster_secret.is_empty();

    // Initialize brute-force protection
    //
    // In cluster mode, use fail-closed mode to prevent security degradation.
    // When Redis is unavailable, login attempts will be rejected rather than
    // falling back to per-replica independent counters.
    //
    // In standalone mode with Redis, use fallback mode for better availability.
    // In standalone mode without Redis, use in-memory tracker.
    let brute_force = if let Some(ref conn) = redis_conn_plain {
        if cluster_mode {
            let bf = crate::service::BruteForceProtection::with_redis_fail_closed(
                conn.clone(),
                config.redis.key_prefix.clone(),
            );
            info!("Brute-force protection initialized (Redis-backed, fail-closed for cluster mode)");
            bf
        } else {
            let bf = crate::service::BruteForceProtection::with_redis(
                conn.clone(),
                config.redis.key_prefix.clone(),
            );
            info!("Brute-force protection initialized (Redis-backed with fallback)");
            bf
        }
    } else {
        // No Redis available
        if cluster_mode {
            // This should have been caught by config validation, but double-check
            error!(
                "CRITICAL: Cluster mode is enabled but Redis is not available. \
                 Brute-force protection will use in-memory counters, which are NOT \
                 shared across replicas. This is a security risk. Configure Redis \
                 immediately."
            );
        }
        let bf = crate::service::BruteForceProtection::in_memory(
            config.redis.key_prefix.clone(),
        );
        if cluster_mode {
            warn!(
                "Brute-force protection is using in-memory counters but cluster mode is active. \
                 Login attempt counters will NOT be shared across replicas, reducing brute-force \
                 protection effectiveness. Configure Redis to fix this."
            );
        }
        info!("Brute-force protection initialized (in-memory)");
        bf
    };

    // Initialize token blacklist store (tiered: L1 moka + optional L2 Redis + PG primary)
    let token_blacklist: Arc<dyn crate::service::TokenBlacklistStore> = Arc::new(
        crate::service::TieredTokenBlacklistStore::new(
            crate::service::PgTokenBlacklistStore::new(pool.clone()),
            redis_conn_plain.clone(),
            config.redis.key_prefix.clone(),
        )
    );
    info!("Token blacklist store initialized (tiered: PG primary{})", if redis_conn_plain.is_some() { " + Redis L2" } else { "" });

    // Initialize UserService
    let key_builder = crate::cache::KeyBuilder::from_config(config);
    let mut user_service = UserService::new(
        pool.clone(),
        jwt_service.clone(),
        username_cache.clone(),
        config.password_complexity.clone(),
        token_blacklist,
        key_builder,
        brute_force,
    );
    user_service.set_cache_invalidation(cache_invalidation.clone());
    info!("UserService initialized");

    // Initialize RoomService
    let mut room_service = RoomService::new(pool.clone(), user_service.clone());
    room_service.set_cache_invalidation(cache_invalidation.clone());
    room_service.set_playback_cache_invalidation(cache_invalidation.clone());
    info!("RoomService initialized");

    // Initialize CacheManager and start cross-replica invalidation listener
    let cache_manager = CacheManager::new(user_cache.clone(), room_cache.clone())
        .with_username_cache(Arc::new(username_cache.clone()));
    cache_manager.start_invalidation_listener(&cache_invalidation);
    info!("CacheManager initialized with invalidation listener");

    // Initialize credential encryption (shared by both repositories and media providers)
    let credential_encryption = init_credential_encryption();
    // Keep a clone for use by media providers (source_config cookie encryption)
    let credential_encryption_for_services = credential_encryption.clone();

    // Initialize ProviderInstanceRepository (with optional encryption for jwt_secret/custom_ca)
    let provider_instance_repo = match &credential_encryption {
        Some(enc) => {
            info!("ProviderInstanceRepository initialized with encryption enabled");
            Arc::new(ProviderInstanceRepository::new_with_encryption(pool.clone(), enc.clone()))
        }
        None => {
            Arc::new(ProviderInstanceRepository::new(pool.clone()))
        }
    };
    info!("ProviderInstanceRepository initialized");

    // Initialize UserProviderCredentialRepository (with optional encryption)
    let user_provider_credential_repo = if let Some(enc) = credential_encryption {
        info!("UserProviderCredentialRepository initialized with encryption enabled");
        Arc::new(UserProviderCredentialRepository::new_with_encryption(pool.clone(), enc))
    } else {
        warn!(
            "Credential encryption key not configured (set SYNCTV_CREDENTIAL_ENCRYPTION_KEY). \
             Provider credentials will be stored in plaintext."
        );
        Arc::new(UserProviderCredentialRepository::new(pool.clone()))
    };

    // Initialize rate limiter
    let rate_limiter = RateLimiter::new(redis_conn_plain.clone(), config.redis.key_prefix.clone());
    let rate_limit_config = RateLimitConfig::default();
    info!(
        "Rate limiter initialized (chat: {}/s, danmaku: {}/s)",
        rate_limit_config.chat_per_second, rate_limit_config.danmaku_per_second
    );

    // Initialize content filter
    let content_filter = ContentFilter::new();
    info!(
        "Content filter initialized (max chat: {} chars, max danmaku: {} chars)",
        content_filter.max_chat_length, content_filter.max_danmaku_length
    );

    // Initialize RemoteProviderManager (with Redis for cross-replica cache invalidation when available)
    info!("Initializing RemoteProviderManager...");
    let provider_instance_manager = Arc::new(RemoteProviderManager::new(
        provider_instance_repo.clone(),
        redis_conn_plain.clone(),
        redis_client.clone(),
    ));

    // Pre-warm cache with all enabled provider instances from database
    if let Err(e) = provider_instance_manager.init().await {
        tracing::error!("Failed to initialize RemoteProviderManager: {}", e);
        tracing::error!("Continuing without remote provider instances");
    } else {
        info!("RemoteProviderManager initialized successfully");
    }

    // Start cross-replica cache invalidation listener
    if let Err(e) = provider_instance_manager.start_invalidation_listener().await {
        tracing::warn!("Failed to start provider invalidation listener: {e}");
    }

    // Initialize ProvidersManager
    info!("Initializing ProvidersManager...");
    let providers_manager = Arc::new(ProvidersManager::new(
        provider_instance_manager.clone(),
    ));
    info!("ProvidersManager initialized");

    // Initialize OAuth2 service (optional - requires OAuth2 provider config).
    // In cluster mode, Redis is required (validated at config level).
    // In standalone mode, uses in-memory state store when Redis is not available.
    let oauth2_configured = config.oauth2.providers.as_object()
        .is_some_and(|m| !m.is_empty());
    let oauth2_service = if oauth2_configured {
        init_oauth2_service(pool.clone(), config, redis_conn_plain.clone()).await?
    } else {
        None
    };
    if oauth2_service.is_some() {
        if redis_conn_plain.is_none() && cluster_mode {
            warn!(
                "OAuth2 is configured but Redis is unavailable. OAuth2 state (CSRF tokens) \
                 is stored in-memory and will NOT be shared across replicas. Users may \
                 experience login failures if the callback hits a different replica. \
                 Configure Redis to fix this."
            );
        }
        info!("OAuth2 service initialized");
    } else {
        info!("OAuth2 service not configured (no OAuth2 providers in config)");
    }

    // Initialize Settings service
    info!("Initializing Settings service...");
    let settings_repo = SettingsRepository::new(pool.clone());
    let settings_service = SettingsService::new(settings_repo, pool.clone());
    settings_service.initialize().await?;
    info!("Settings service initialized with {} groups", {
        settings_service.get_all().await.map_or(0, |g| g.len())
    });

    // Start PostgreSQL LISTEN for hot reload (with CancellationToken for graceful shutdown)
    let settings_cancel = tokio_util::sync::CancellationToken::new();
    let settings_listen_task = settings_service.start_listen_task(settings_cancel.clone());
    info!("Settings hot reload (PostgreSQL LISTEN) started");

    // Wrap settings_service in Arc before creating registry
    let settings_service = Arc::new(settings_service);

    // Initialize Settings registry
    info!("Initializing Settings registry...");
    let settings_registry = SettingsRegistry::new(settings_service.clone());
    settings_registry.init(settings_cancel.clone()).await?;
    info!("Settings registry initialized");

    // Initialize Email service (optional - requires SMTP configuration)
    let email_service = init_email_service(config, redis_client.as_ref());
    if email_service.is_some() {
        info!("Email service initialized");
    } else {
        info!("Email service not configured (set SYNCTV_EMAIL_SMTP_HOST)");
    }

    // Initialize Email Token service (optional - requires email service)
    let email_token_service = if email_service.is_some() {
        Some(Arc::new(EmailTokenService::new(pool.clone())))
    } else {
        None
    };
    if email_token_service.is_some() {
        info!("Email token service initialized");
    } else {
        info!("Email token service not configured (requires email service)");
    }

    // Initialize Publish Key service (for RTMP streaming)
    let publish_key_service = if let Some(ref conn) = redis_conn_plain {
        info!("Publish key service initialized with Redis-backed JTI deduplication");
        PublishKeyService::with_redis(jwt_service.clone(), 24, conn.clone(), config.redis.key_prefix.clone())
    } else {
        info!("Publish key service initialized with in-memory JTI deduplication");
        PublishKeyService::with_default_ttl(jwt_service.clone())
    };

    // Initialize User Notification service
    let notification_repo = NotificationRepository::new(pool.clone());
    let notification_service = UserNotificationService::new(notification_repo);
    info!("User notification service initialized");

    // Initialize Audit service with buffering
    let (audit_service, audit_flush_handle) = AuditService::new(pool.clone());
    let audit_service = Arc::new(audit_service);
    info!("Audit service initialized with async buffering");

    // Wire audit service into RoomService (propagates to MemberService internally)
    room_service.set_audit_service(Arc::clone(&audit_service));
    info!("Audit service wired into RoomService and MemberService");

    // Store the settings listen task handle so it can be joined on shutdown.
    // The task will be cancelled via settings_cancel.

    Ok(Services {
        user_service: Arc::new(user_service),
        room_service: Arc::new(room_service),
        jwt_service,
        rate_limiter,
        rate_limit_config,
        content_filter,
        provider_instance_manager,
        provider_instance_repo,
        user_provider_credential_repo,
        providers_manager,
        oauth2_service,
        settings_service,
        settings_registry: Arc::new(settings_registry),
        email_service,
        email_token_service,
        publish_key_service: Arc::new(publish_key_service),
        notification_service: Arc::new(notification_service),
        audit_service,
        cache_invalidation,
        cache_manager,
        redis_conn: redis_handles.as_ref().map(|h| h.conn.clone()),
        redis_client: redis_handles.as_ref().map(|h| h.client.clone()),
        settings_cancel,
        settings_listen_task: Arc::new(tokio::sync::Mutex::new(Some(settings_listen_task))),
        audit_flush_handle: Arc::new(tokio::sync::Mutex::new(Some(audit_flush_handle))),
        credential_encryption: credential_encryption_for_services,
    })
}

/// Initialize `OAuth2` service with modular provider system
///
/// Uses factory pattern to create providers from configuration.
/// `OAuth2` configuration is part of the main config file.
async fn init_oauth2_service(
    pool: PgPool,
    config: &Config,
    redis_conn: Option<redis::aio::ConnectionManager>,
) -> Result<Option<Arc<OAuth2Service>>, anyhow::Error> {
    // 0. Initialize provider registry (register all factory functions)
    crate::oauth2::providers::init_providers();
    info!("OAuth2 provider registry initialized");

    // 1. Get OAuth2 provider configurations from main config
    let providers_value = &config.oauth2.providers;

    // Extract provider instance names from the JSON mapping
    let provider_instances = if let Some(mapping) = providers_value.as_object() {
        mapping.keys().cloned().collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    if provider_instances.is_empty() {
        info!("No OAuth2 providers configured");
        return Ok(None);
    }

    // 2. Create OAuth2 provider repository and service
    //    Use Redis state store when available, in-memory otherwise.
    let oauth2_repo = UserOAuthProviderRepository::new(pool.clone());
    let state_store: Arc<dyn crate::service::OAuthStateStore> = if let Some(conn) = redis_conn {
        info!("OAuth2 state store: Redis");
        Arc::new(crate::service::RedisOAuthStateStore::new(conn))
    } else {
        info!("OAuth2 state store: in-memory (standalone mode)");
        Arc::new(crate::service::InMemoryOAuthStateStore::new())
    };
    let oauth2_service = OAuth2Service::new(oauth2_repo, state_store);
    let oauth2_service = Arc::new(oauth2_service);

    // 3. Initialize each provider instance using factory pattern
    for instance_name in provider_instances {
        // Get the full config for this instance
        let full_config = providers_value.get(&instance_name)
            .ok_or_else(|| anyhow::anyhow!("Provider instance {instance_name} not found in config"))?;

        // Get provider type from config (check for explicit "type" field)
        let provider_type = full_config
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or(&instance_name)
            .to_string();

        // Create a mutable config for adding redirect_url
        let mut full_config = full_config.clone();

        // Add redirect_url to config (merge it in)
        // Use configured scheme (http/https) to support reverse proxy TLS termination
        let scheme = &config.oauth2.redirect_scheme;
        let redirect_url = format!("{}://{}/api/oauth2/{}/callback", scheme, config.advertise_host(), instance_name);
        if let Some(mapping) = full_config.as_object_mut() {
            mapping.insert(
                "redirect_url".to_string(),
                serde_json::Value::String(redirect_url.clone())
            );
        }

        // Use factory to create provider with full config
        match crate::oauth2::create_provider(&provider_type, &full_config).await {
            Ok(provider) => {
                let provider_enum = if let Some(p) = crate::models::oauth2_client::OAuth2Provider::from_str_name(&provider_type) { p } else {
                    warn!(
                        "Skipping unknown OAuth2 provider type '{}' for instance '{}'",
                        provider_type, instance_name
                    );
                    continue;
                };

                // Store provider for later use
                oauth2_service.register_provider(instance_name.clone(), provider_enum, provider).await;
                info!("Registered OAuth2 provider: {} (type: {})", instance_name, provider_type);
            }
            Err(e) => {
                warn!("Failed to create OAuth2 provider {}: {}", instance_name, e);
            }
        }
    }

    // OAuth2 state cleanup is handled automatically:
    // - Redis: SETEX TTL auto-expires entries
    // - In-memory: sweep_expired on each store/consume call

    Ok(Some(oauth2_service))
}


/// Load JWT service from secret in configuration
fn load_jwt_service(config: &Config) -> Result<JwtService, anyhow::Error> {
    if config.jwt.secret.is_empty() {
        return Err(anyhow::anyhow!(
            "JWT secret is empty. Please set SYNCTV_JWT_SECRET environment variable or configure jwt.secret in config file"
        ));
    }

    const WEAK_SECRETS: &[&str] = &[
        "change-me-in-production", "secret", "password", "jwt-secret",
        "changeme", "test", "default",
    ];
    if WEAK_SECRETS.contains(&config.jwt.secret.as_str()) {
        warn!("Using a well-known JWT secret! This is insecure for production use.");
        warn!("Please set SYNCTV_JWT_SECRET to a strong random value.");
    }
    if config.jwt.secret.len() < 32 {
        return Err(anyhow::anyhow!(
            "JWT secret is too short ({} chars). Minimum 32 characters required for security. \
             Set SYNCTV_JWT_SECRET to a strong random value.",
            config.jwt.secret.len()
        ));
    }

    JwtService::with_durations(
        &config.jwt.secret,
        config.jwt.access_token_duration_hours,
        config.jwt.refresh_token_duration_days,
        config.jwt.guest_token_duration_hours,
        config.jwt.clock_skew_leeway_secs,
    )
    .map_err(|e| anyhow::anyhow!("Failed to initialize JWT service: {e}"))
}

/// Initialize credential encryption from environment variable or secret file
///
/// Tries the following sources in order:
/// 1. Secret file: `/run/secrets/credential_encryption_key`
/// 2. Environment variable: `SYNCTV_CREDENTIAL_ENCRYPTION_KEY`
///
/// The key must be a 64-character hex string (32 bytes).
fn init_credential_encryption() -> Option<crate::service::CredentialEncryption> {
    use crate::secrets::{SecretLoader, SecretSource};

    // Try file first, then env var
    let hex_key = SecretLoader::load_with_fallback(
        "credential_encryption_key",
        SecretSource::File("/run/secrets/credential_encryption_key"),
        SecretSource::Env("SYNCTV_CREDENTIAL_ENCRYPTION_KEY"),
    ).ok()?;

    match crate::service::CredentialEncryption::from_hex_key(&hex_key) {
        Ok(enc) => {
            info!("Credential encryption initialized (AES-256-GCM)");
            Some(enc)
        }
        Err(e) => {
            error!("Failed to initialize credential encryption: {}", e);
            error!("Key must be a 64-character hex string (32 bytes for AES-256)");
            None
        }
    }
}

/// Initialize Email service (optional - requires SMTP configuration)
///
/// When `redis_client` is provided, uses Redis-backed verification code storage
/// for multi-node safety. Otherwise falls back to in-memory storage.
fn init_email_service(config: &Config, redis_client: Option<&redis::Client>) -> Option<Arc<EmailService>> {
    // Check if SMTP host is configured
    if config.email.smtp_host.is_empty() {
        return None;
    }

    let email_config = EmailConfig {
        smtp_host: config.email.smtp_host.clone(),
        smtp_port: config.email.smtp_port,
        smtp_username: config.email.smtp_username.clone(),
        smtp_password: config.email.smtp_password.clone(),
        from_email: config.email.from_email.clone(),
        from_name: config.email.from_name.clone(),
        use_tls: config.email.use_tls,
    };

    let cluster_mode = config.cluster.enabled || !config.server.cluster_secret.is_empty();

    let result = if let Some(client) = redis_client {
        info!("Email verification code store: Redis (multi-node safe)");
        EmailService::with_redis(Some(email_config), Arc::new(client.clone()))
    } else {
        if cluster_mode {
            warn!(
                "Email verification codes are stored in-memory but cluster mode is active. \
                 Verification codes will NOT be shared across replicas. \
                 Configure Redis to fix this."
            );
        }
        EmailService::new(Some(email_config))
    };

    match result {
        Ok(service) => Some(Arc::new(service)),
        Err(e) => {
            error!("Failed to initialize email service: {}", e);
            None
        }
    }
}
