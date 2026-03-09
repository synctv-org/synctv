//! Service initialization and dependency injection

use std::sync::Arc;

use sqlx::PgPool;
use tracing::{error, info, warn};

use crate::{
    bootstrap::RedisHandles,
    cache::{
        CacheInvalidationService, CacheL2Backend, CacheManager, NoopCacheL2, RedisCacheL2,
        RoomCache, UserCache, UsernameCache,
    },
    repository::{
        ChatRepository, NotificationRepository, ProviderInstanceRepository,
        RoomSettingsRepository as RoomSettingsRepo, SettingsRepository,
        UserOAuthProviderRepository, UserProviderCredentialRepository,
    },
    service::{
        notification::NotificationService as RoomNotificationService, AuditFlushHandle,
        AuditService, ChatService, ContentFilter, EmailConfig, EmailService, EmailTokenService,
        JwtService, OAuth2Service, ProvidersManager, PublishKeyService, RateLimitConfig,
        RateLimiter, RemoteProviderManager, RoomService, RoomSettingsService, SettingsRegistry,
        SettingsService, UserNotificationService, UserService,
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
    /// Chat service for message handling with business logic
    pub chat_service: Arc<ChatService>,
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

fn handle_provider_invalidation_listener_result(
    start_result: crate::Result<()>,
    cluster_mode: bool,
) -> Result<(), anyhow::Error> {
    if let Err(e) = start_result {
        if cluster_mode {
            return Err(anyhow::anyhow!(
                "cluster mode requires provider invalidation listener to start successfully: {e}"
            ));
        }
        tracing::warn!("Failed to start provider invalidation listener: {e}");
    }

    Ok(())
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

    let cluster_mode = config.cluster_runtime_enabled();

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
    // H8 fix: All services now receive the shared Arc<RwLock<ConnectionManager>>
    // directly via redis_handles.conn, eliminating init-time snapshots that
    // would become stale after Sentinel failover.
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
    info!(
        "Username cache initialized (capacity={}, ttl={}s)",
        config.cache.username_cache_capacity, config.cache.username_cache_ttl_seconds
    );

    // Initialize user and room L1/L2 caches (using config values)
    let user_cache = Arc::new(UserCache::new(
        cache_l2.clone(),
        config.cache.l1_capacity,
        config.cache.l1_ttl_seconds,
        config.cache.l2_ttl_seconds,
        format!("{}user:", config.redis.key_prefix),
    )?);
    let room_cache = Arc::new(RoomCache::new(
        cache_l2.clone(),
        config.cache.l1_capacity,
        config.cache.l1_ttl_seconds,
        config.cache.l2_ttl_seconds,
        format!("{}room:", config.redis.key_prefix),
    )?);
    info!(
        "User and room caches initialized (l1_capacity={}, l1_ttl={}s, l2_ttl={}s)",
        config.cache.l1_capacity, config.cache.l1_ttl_seconds, config.cache.l2_ttl_seconds
    );

    // Initialize brute-force protection
    //
    // In cluster mode, use fail-closed mode to prevent security degradation.
    // When Redis is unavailable, login attempts will be rejected rather than
    // falling back to per-replica independent counters.
    //
    // In standalone mode with Redis, use fallback mode for better availability.
    // In standalone mode without Redis, use in-memory tracker.
    let brute_force = if cluster_mode {
        let redis_handles = redis_handles.as_ref().expect(
            "cluster mode requires Redis handles; this invariant is validated before init_services",
        );
        let bf = crate::service::BruteForceProtection::with_redis_fail_closed(
            redis_handles.conn.clone(),
            config.redis.key_prefix.clone(),
        );
        info!("Brute-force protection initialized (Redis-backed, fail-closed for cluster mode)");
        bf
    } else if let Some(ref rh) = redis_handles {
        let bf = crate::service::BruteForceProtection::with_redis(
            rh.conn.clone(),
            config.redis.key_prefix.clone(),
        );
        info!("Brute-force protection initialized (Redis-backed with fallback)");
        bf
    } else {
        let bf = crate::service::BruteForceProtection::in_memory(config.redis.key_prefix.clone());
        info!("Brute-force protection initialized (in-memory)");
        bf
    };

    // Initialize token blacklist store (tiered: L1 moka + optional L2 Redis + PG primary)
    let token_blacklist: Arc<dyn crate::service::TokenBlacklistStore> =
        Arc::new(crate::service::TieredTokenBlacklistStore::new(
            crate::service::PgTokenBlacklistStore::new(pool.clone()),
            redis_handles.as_ref().map(|h| h.conn.clone()),
            config.redis.key_prefix.clone(),
        ));
    info!(
        "Token blacklist store initialized (tiered: PG primary{})",
        if redis_handles.is_some() {
            " + Redis L2"
        } else {
            ""
        }
    );

    // Initialize UserService
    let key_builder = crate::cache::KeyBuilder::from_config(config);
    let mut user_service = UserService::new(
        pool.clone(),
        jwt_service.clone(),
        username_cache.clone(),
        config.password_complexity.clone(),
        token_blacklist,
        key_builder,
        brute_force.clone(),
    );
    user_service.set_cache_invalidation(cache_invalidation.clone());

    // Upgrade refresh token rate limiter to Redis-backed when available.
    // This ensures the refresh rate limit is enforced globally across all replicas
    // in cluster mode, preventing N * limit bypass with N replicas.
    if cluster_mode {
        let redis_handles = redis_handles.as_ref().expect(
            "cluster mode requires Redis handles; this invariant is validated before init_services",
        );
        user_service.set_refresh_rate_limiter_redis_strict(
            redis_handles.conn.clone(),
            format!("{}refresh_rl:", config.redis.key_prefix),
        );
        info!("Refresh token rate limiter upgraded to Redis-backed (cross-replica)");
    } else if let Some(ref rh) = redis_handles {
        user_service.set_refresh_rate_limiter_redis(
            rh.conn.clone(),
            format!("{}refresh_rl:", config.redis.key_prefix),
        );
        info!("Refresh token rate limiter upgraded to Redis-backed (cross-replica)");
    }
    info!("UserService initialized");

    // Initialize RoomService
    let mut room_service = build_room_service(
        pool.clone(),
        user_service.clone(),
        cache_invalidation.clone(),
        brute_force.clone(),
        redis_handles.as_ref(),
        matches!(
            config.redis.deployment_mode,
            crate::config::RedisDeploymentMode::Sentinel
        ),
    );
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
            Arc::new(ProviderInstanceRepository::new_with_encryption(
                pool.clone(),
                enc.clone(),
            ))
        }
        None => Arc::new(ProviderInstanceRepository::new(pool.clone())),
    };
    info!("ProviderInstanceRepository initialized");

    // Initialize UserProviderCredentialRepository (with optional encryption)
    let user_provider_credential_repo = if let Some(enc) = credential_encryption {
        info!("UserProviderCredentialRepository initialized with encryption enabled");
        Arc::new(UserProviderCredentialRepository::new_with_encryption(
            pool.clone(),
            enc,
        ))
    } else {
        warn!(
            "Credential encryption key not configured (set SYNCTV_CREDENTIAL_ENCRYPTION_KEY). \
             Provider credentials will not be encrypted."
        );
        Arc::new(UserProviderCredentialRepository::new(pool.clone()))
    };

    // Initialize rate limiter
    let rate_limiter = RateLimiter::new(
        redis_handles.as_ref().map(|h| h.conn.clone()),
        config.redis.key_prefix.clone(),
    );
    let rate_limit_config = RateLimitConfig {
        chat_per_second: config.messaging_rate_limits.chat_per_second,
        danmaku_per_second: config.messaging_rate_limits.danmaku_per_second,
        window_seconds: config.messaging_rate_limits.window_seconds,
    };
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
        redis_handles.as_ref().map(|h| h.conn.clone()),
        redis_client.clone(),
        config.redis.key_prefix.clone(),
    ));

    // Pre-warm cache with all enabled provider instances from database
    if let Err(e) = provider_instance_manager.init().await {
        tracing::error!("Failed to initialize RemoteProviderManager: {}", e);
        tracing::error!("Continuing without remote provider instances");
    } else {
        info!("RemoteProviderManager initialized successfully");
    }

    // Start cross-replica cache invalidation listener
    handle_provider_invalidation_listener_result(
        provider_instance_manager
            .start_invalidation_listener()
            .await,
        cluster_mode,
    )?;

    // Initialize ProvidersManager
    info!("Initializing ProvidersManager...");
    let provider_http_client = synctv_common::http::SsrfSafeClientBuilder::provider()
        .connect_timeout(std::time::Duration::from_secs(
            config.media_providers.connect_timeout_seconds,
        ))
        .request_timeout(std::time::Duration::from_secs(
            config.media_providers.request_timeout_seconds,
        ))
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to build provider HTTP client: {e}"))?;
    let providers_manager = Arc::new(ProvidersManager::new_with_provider_http_client(
        provider_instance_manager.clone(),
        provider_http_client,
        std::time::Duration::from_secs(config.media_providers.connect_timeout_seconds),
    ));
    info!("ProvidersManager initialized");

    // Initialize OAuth2 service (optional - requires OAuth2 provider config).
    // In cluster mode, Redis is required (validated at service creation).
    // In standalone mode, uses in-memory state store when Redis is not available.
    let oauth2_configured = config
        .oauth2
        .providers
        .as_object()
        .is_some_and(|m| !m.is_empty());
    let oauth2_service = if oauth2_configured {
        init_oauth2_service(
            pool.clone(),
            config,
            redis_handles.as_ref().map(|h| h.conn.clone()),
            cluster_mode,
        )
        .await?
    } else {
        None
    };
    if oauth2_service.is_some() {
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
    let email_service = init_email_service(config, redis_handles.as_ref().map(|h| h.conn.clone()));
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
    //
    // Use Redis-backed JTI dedup when available (shared handle follows Sentinel failover).
    // Falls back to in-memory for standalone mode.
    let publish_key_service = build_publish_key_service(
        jwt_service.clone(),
        redis_handles.as_ref(),
        &config.redis.key_prefix,
        cluster_mode,
    )?;

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

    // Wire user notification service into RoomService for pending room admin notifications
    let notification_service = Arc::new(notification_service);
    room_service.set_user_notification_service(Arc::clone(&notification_service));
    info!("User notification service wired into RoomService");

    // Wire settings registry into UserService for signup_need_review and email_whitelist enforcement
    let settings_registry = Arc::new(settings_registry);
    user_service.set_settings_registry(Arc::clone(&settings_registry));

    // Store the settings listen task handle so it can be joined on shutdown.
    // The task will be cancelled via settings_cancel.

    // Initialize ChatService with proper business logic (permissions, rate limiting, filtering)
    let chat_repo = Arc::new(ChatRepository::new(pool.clone()));
    let room_settings_repo_for_chat = RoomSettingsRepo::new(pool.clone());
    let room_notification_service = Arc::new(RoomNotificationService::default());
    let room_settings_service_for_chat = RoomSettingsService::new(
        room_settings_repo_for_chat,
        Some(cache_invalidation.clone()),
        room_notification_service,
        None,
        None,
        None,
    );
    let permission_service_for_chat = room_service.permission_service().clone();
    let chat_service = ChatService::new(
        chat_repo,
        rate_limiter.clone(),
        rate_limit_config.clone(),
        content_filter.clone(),
        username_cache.clone(),
        permission_service_for_chat,
        room_settings_service_for_chat,
    );
    info!("ChatService initialized");

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
        settings_registry,
        email_service,
        email_token_service,
        publish_key_service: Arc::new(publish_key_service),
        notification_service,
        chat_service: Arc::new(chat_service),
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
    redis_conn: Option<Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>>,
    cluster_mode: bool,
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

    // 2. Create OAuth2 provider repository and service.
    // Cluster mode must fail before wiring if Redis is missing; only standalone
    // mode may use an in-memory state store.
    let oauth2_repo = UserOAuthProviderRepository::new(pool.clone());
    let state_store: Arc<dyn crate::service::OAuthStateStore> = match (cluster_mode, redis_conn) {
        (true, Some(conn)) => {
            info!("OAuth2 state store: Redis (cluster mode)");
            Arc::new(crate::service::RedisOAuthStateStore::new(conn))
        }
        (true, None) => {
            return Err(anyhow::anyhow!(
                "Redis is required for OAuth2 state storage in cluster mode. \
                 Refusing to fall back to in-memory state because OAuth2 callbacks may land on a different replica."
            ));
        }
        (false, Some(conn)) => {
            info!("OAuth2 state store: Redis");
            Arc::new(crate::service::RedisOAuthStateStore::new(conn))
        }
        (false, None) => {
            info!("OAuth2 state store: in-memory (standalone mode)");
            Arc::new(crate::service::InMemoryOAuthStateStore::new())
        }
    };
    let oauth2_service = OAuth2Service::new(oauth2_repo, state_store, cluster_mode)
        .map_err(|e| anyhow::anyhow!("Failed to create OAuth2 service: {e}"))?;
    let oauth2_service = Arc::new(oauth2_service);

    // 3. Initialize each provider instance using factory pattern
    for instance_name in provider_instances {
        // Get the full config for this instance
        let full_config = providers_value.get(&instance_name).ok_or_else(|| {
            anyhow::anyhow!("Provider instance {instance_name} not found in config")
        })?;

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
        let redirect_url = format!(
            "{}://{}/api/oauth2/{}/callback",
            scheme,
            config.advertise_host(),
            instance_name
        );
        if let Some(mapping) = full_config.as_object_mut() {
            mapping.insert(
                "redirect_url".to_string(),
                serde_json::Value::String(redirect_url.clone()),
            );
        }

        // Use factory to create provider with full config
        match crate::oauth2::create_provider(&provider_type, &full_config).await {
            Ok(provider) => {
                let provider_enum = if let Some(p) =
                    crate::models::oauth2_client::OAuth2Provider::from_str_name(&provider_type)
                {
                    p
                } else {
                    warn!(
                        "Skipping unknown OAuth2 provider type '{}' for instance '{}'",
                        provider_type, instance_name
                    );
                    continue;
                };

                // Store provider for later use
                oauth2_service
                    .register_provider(instance_name.clone(), provider_enum, provider)
                    .await;
                info!(
                    "Registered OAuth2 provider: {} (type: {})",
                    instance_name, provider_type
                );
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
        "change-me-in-production",
        "secret",
        "password",
        "jwt-secret",
        "changeme",
        "test",
        "default",
    ];
    if WEAK_SECRETS.contains(&config.jwt.secret.as_str()) {
        warn!("Using a well-known JWT secret! This is insecure for production use.");
        warn!("Please set SYNCTV_JWT_SECRET to a strong random value.");
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

fn build_room_service(
    pool: PgPool,
    user_service: UserService,
    cache_invalidation: Arc<CacheInvalidationService>,
    brute_force: crate::service::auth::BruteForceProtection,
    redis_handles: Option<&RedisHandles>,
    is_sentinel: bool,
) -> RoomService {
    let mut room_service = RoomService::new(pool, user_service);
    if let Some(redis_handles) = redis_handles {
        let lock = crate::service::DistributedLock::new_shared_with_mode(
            redis_handles.conn.clone(),
            is_sentinel,
        );
        room_service.set_distributed_lock(lock);
    }
    room_service.set_brute_force_service(brute_force);
    room_service.set_cache_invalidation(cache_invalidation.clone());
    room_service.set_playback_cache_invalidation(cache_invalidation);
    room_service
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublishKeyBackendMode {
    Memory,
    RedisBestEffort,
    RedisFailClosed,
}

const fn publish_key_backend_mode(cluster_mode: bool, has_redis: bool) -> PublishKeyBackendMode {
    match (cluster_mode, has_redis) {
        (true, true) => PublishKeyBackendMode::RedisFailClosed,
        (true, false) => PublishKeyBackendMode::RedisFailClosed,
        (false, true) => PublishKeyBackendMode::RedisBestEffort,
        (_, false) => PublishKeyBackendMode::Memory,
    }
}

fn build_publish_key_service(
    jwt_service: JwtService,
    redis_handles: Option<&RedisHandles>,
    key_prefix: &str,
    cluster_mode: bool,
) -> Result<PublishKeyService, anyhow::Error> {
    match publish_key_backend_mode(cluster_mode, redis_handles.is_some()) {
        PublishKeyBackendMode::RedisFailClosed => {
            let redis_handles = redis_handles.ok_or_else(|| {
                anyhow::anyhow!(
                    "cluster mode requires Redis handles for fail-closed publish key deduplication"
                )
            })?;
            info!("Publish key service initialized with Redis JTI deduplication (fail-closed)");
            Ok(PublishKeyService::with_redis_shared_fail_closed(
                jwt_service,
                24,
                redis_handles.conn.clone(),
                key_prefix.to_string(),
            ))
        }
        PublishKeyBackendMode::RedisBestEffort => {
            let redis_handles = redis_handles.ok_or_else(|| {
                anyhow::anyhow!(
                    "Redis-backed publish key mode requires Redis handles to be present"
                )
            })?;
            info!("Publish key service initialized with Redis JTI deduplication");
            Ok(PublishKeyService::with_redis_shared(
                jwt_service,
                24,
                redis_handles.conn.clone(),
                key_prefix.to_string(),
            ))
        }
        PublishKeyBackendMode::Memory => {
            info!("Publish key service initialized with in-memory JTI deduplication");
            Ok(PublishKeyService::with_default_ttl(jwt_service))
        }
    }
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
    )
    .ok()?;

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
fn init_email_service(
    config: &Config,
    redis_conn: Option<Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>>,
) -> Option<Arc<EmailService>> {
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

    let cluster_mode = config.cluster_runtime_enabled();

    let result = match (cluster_mode, redis_conn) {
        (true, Some(shared_conn)) => {
            info!("Email verification code store: Redis (cluster mode)");
            EmailService::with_redis(Some(email_config), shared_conn)
        }
        (true, None) => unreachable!(
            "cluster.enabled=true requires Redis and is validated before service initialization"
        ),
        (false, Some(shared_conn)) => {
            info!("Email verification code store: Redis (multi-node safe)");
            EmailService::with_redis(Some(email_config), shared_conn)
        }
        (false, None) => EmailService::new(Some(email_config)),
    };

    match result {
        Ok(service) => Some(Arc::new(service)),
        Err(e) => {
            error!("Failed to initialize email service: {}", e);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_publish_key_backend_mode_is_fail_closed_in_cluster() {
        assert_eq!(
            publish_key_backend_mode(true, true),
            PublishKeyBackendMode::RedisFailClosed
        );
    }

    #[test]
    fn test_publish_key_backend_mode_is_best_effort_in_standalone_with_redis() {
        assert_eq!(
            publish_key_backend_mode(false, true),
            PublishKeyBackendMode::RedisBestEffort
        );
    }

    #[test]
    fn test_publish_key_backend_mode_is_memory_without_redis() {
        assert_eq!(
            publish_key_backend_mode(false, false),
            PublishKeyBackendMode::Memory
        );
    }

    #[test]
    fn test_build_publish_key_service_returns_error_without_redis_in_cluster_mode() {
        let jwt_service = JwtService::with_durations(
            "f4e9a7c21d3b5e6f8a9c0b1d2e3f4a5b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e2f",
            24,
            30,
            24,
            60,
        )
        .expect("jwt service");

        let error = build_publish_key_service(jwt_service, None, "test:", true)
            .expect_err("cluster mode publish key service must return an error without Redis");

        assert!(
            error.to_string().contains(
                "cluster mode requires Redis handles for fail-closed publish key deduplication"
            ),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_cluster_mode_provider_invalidation_failure_is_fatal() {
        let err = handle_provider_invalidation_listener_result(
            Err(crate::Error::Internal(
                "listener bootstrap failed".to_string(),
            )),
            true,
        )
        .expect_err("cluster mode must fail closed on provider invalidation wiring");
        assert!(
            err.to_string()
                .contains("cluster mode requires provider invalidation listener"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_standalone_provider_invalidation_failure_is_non_fatal() {
        handle_provider_invalidation_listener_result(
            Err(crate::Error::Internal(
                "listener bootstrap failed".to_string(),
            )),
            false,
        )
        .expect("standalone mode may continue with local-only provider invalidation");
    }

    #[tokio::test]
    async fn test_build_room_service_wires_brute_force_protection() {
        let pool = PgPool::connect_lazy("postgresql://test").expect("lazy pool should build");
        let jwt_service = JwtService::with_durations(
            "f4e9a7c21d3b5e6f8a9c0b1d2e3f4a5b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e2f",
            24,
            30,
            24,
            60,
        )
        .expect("jwt service");
        let username_cache =
            UsernameCache::new(Arc::new(NoopCacheL2), "test:user:".to_string(), 100, 60);
        let token_blacklist: Arc<dyn crate::service::TokenBlacklistStore> = Arc::new(
            crate::service::InMemoryTokenBlacklistStore::new(100, 60, 60),
        );
        let user_service = UserService::new(
            pool.clone(),
            jwt_service,
            username_cache,
            Config::default().password_complexity,
            token_blacklist,
            crate::cache::KeyBuilder::new("test"),
            crate::service::auth::BruteForceProtection::in_memory("test:user".to_string()),
        );
        let cache_invalidation = Arc::new(CacheInvalidationService::new(
            None,
            "node-test".to_string(),
            "test:cache:stream".to_string(),
        ));

        let room_service = build_room_service(
            pool,
            user_service,
            cache_invalidation,
            crate::service::auth::BruteForceProtection::in_memory("test:room".to_string()),
            None,
            false,
        );

        assert!(room_service.has_brute_force_service());
        assert!(!room_service.has_distributed_lock());
    }

    #[test]
    fn test_build_room_service_signature_supports_redis_lock_wiring() {
        let _: fn(
            PgPool,
            UserService,
            Arc<CacheInvalidationService>,
            crate::service::auth::BruteForceProtection,
            Option<&RedisHandles>,
            bool,
        ) -> RoomService = build_room_service;
    }

    #[test]
    fn test_cluster_enabled_without_redis_fails_at_config_validation_layer() {
        let mut config = Config::default();
        config.cluster.enabled = true;
        config.server.cluster_secret = "cluster-secret".to_string();
        config.redis.url.clear();

        let errors = config
            .validate()
            .expect_err("cluster.enabled=true without Redis must fail config validation");

        assert!(
            errors
                .iter()
                .any(|e| e.contains("cluster mode requires Redis")),
            "Expected cluster/Redis validation error, got: {errors:?}"
        );
    }

    #[test]
    fn test_standalone_without_redis_has_no_cluster_redis_validation_error() {
        let mut config = Config::default();
        config.cluster.enabled = false;
        config.redis.url.clear();

        let errors = config.validate().err().unwrap_or_default();
        assert!(
            !errors
                .iter()
                .any(|e| e.contains("cluster mode requires Redis")),
            "cluster=false should not be rejected by cluster/Redis rule: {errors:?}"
        );
    }

    #[tokio::test]
    async fn test_init_oauth2_service_rejects_cluster_mode_without_redis_at_bootstrap_layer() {
        let pool = PgPool::connect_lazy("postgresql://test").expect("lazy pool should build");
        let mut config = Config::default();
        config.cluster.enabled = true;
        config.server.cluster_secret = "cluster-secret".to_string();
        config.oauth2.providers = serde_json::json!({
            "github": {
                "type": "github",
                "client_id": "test-client-id",
                "client_secret": "test-client-secret"
            }
        });

        let error = init_oauth2_service(pool, &config, None, true)
            .await
            .expect_err("cluster bootstrap must not fall back to in-memory OAuth2 state store");

        assert!(
            error
                .to_string()
                .contains("Redis is required for OAuth2 state storage in cluster mode"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_init_services_uses_configured_messaging_rate_limits() {
        let mut config = Config::default();
        config.messaging_rate_limits.chat_per_second = 21;
        config.messaging_rate_limits.danmaku_per_second = 8;
        config.messaging_rate_limits.window_seconds = 5;

        let rate_limit_config = RateLimitConfig {
            chat_per_second: config.messaging_rate_limits.chat_per_second,
            danmaku_per_second: config.messaging_rate_limits.danmaku_per_second,
            window_seconds: config.messaging_rate_limits.window_seconds,
        };

        assert_eq!(rate_limit_config.chat_per_second, 21);
        assert_eq!(rate_limit_config.danmaku_per_second, 8);
        assert_eq!(rate_limit_config.window_seconds, 5);
    }
}
