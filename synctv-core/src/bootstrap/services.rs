//! Service initialization and dependency injection

use std::sync::Arc;

use sqlx::PgPool;
use tracing::{error, info, warn};

use crate::{
    cache::{
        CacheInvalidationService, CacheManager,
        RoomCache, UserCache, UsernameCache,
    },
    repository::{UserOAuthProviderRepository, ProviderInstanceRepository, UserProviderCredentialRepository, SettingsRepository, NotificationRepository},
    service::{
        ContentFilter, JwtService, OAuth2Service, RemoteProviderManager, RateLimitConfig,
        RateLimiter, TokenBlacklistService, UserService, RoomService, ProvidersManager,
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
    /// Token blacklist (uses Redis)
    pub token_blacklist: TokenBlacklistService,
    /// Rate limiter (uses Redis)
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
    /// Shared Redis connection (optional, None if Redis not configured)
    pub redis_conn: Option<redis::aio::ConnectionManager>,
    /// CancellationToken for settings listen task (cancel on shutdown)
    pub settings_cancel: tokio_util::sync::CancellationToken,
    /// Settings listen task handle (joined on shutdown).
    /// Wrapped in `Arc<Mutex<Option<...>>>` so `Services` remains `Clone`.
    /// Take the handle out of the `Option` to join it on shutdown.
    pub settings_listen_task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Audit flush handle for graceful shutdown of audit logging
    pub audit_flush_handle: Arc<tokio::sync::Mutex<Option<AuditFlushHandle>>>,
}

/// Initialize all core services
///
/// The caller must supply a pre-built `CacheInvalidationService` so that the
/// same instance (with the correct cluster node ID) is shared across every
/// component.  The caller is also responsible for calling `.start()` on it
/// after this function returns, so there is exactly one Redis subscriber.
pub async fn init_services(
    pool: PgPool,
    config: &Config,
    cache_invalidation: Arc<CacheInvalidationService>,
) -> Result<Services, anyhow::Error> {
    info!("Initializing services...");

    // Initialize JWT service
    info!("Loading JWT keys...");
    let jwt_service = load_jwt_service(config)?;
    info!("JWT service initialized");

    // Initialize shared Redis connection (used by token blacklist, rate limiter, and username cache)
    let (redis_conn, redis_client) = if config.redis.url.is_empty() {
        (None, None)
    } else {
        use crate::config::RedisDeploymentMode;

        // Check for cluster mode early (not yet fully supported)
        if config.redis.deployment_mode == RedisDeploymentMode::Cluster {
            return Err(anyhow::anyhow!(
                "Redis cluster mode requires additional refactoring to support ConnectionManager. \
                 Please use standalone or sentinel mode for now."
            ).into());
        }

        let client = match config.redis.deployment_mode {
            RedisDeploymentMode::Standalone => {
                info!("Initializing Redis in standalone mode");
                redis::Client::open(config.redis.url.clone())?
            }
            RedisDeploymentMode::Sentinel => {
                info!("Initializing Redis in sentinel mode");
                let master_name = config.redis.sentinel_master_name.as_ref()
                    .ok_or_else(|| anyhow::anyhow!("sentinel_master_name is required for sentinel mode"))?;

                if config.redis.sentinel_addresses.is_empty() {
                    return Err(anyhow::anyhow!("sentinel_addresses cannot be empty for sentinel mode").into());
                }

                // Use Sentinel to discover the current master address, then create a
                // regular Client pointing at it.  This gives us a ConnectionManager
                // that reconnects automatically, though it won't follow master failover
                // on its own (a proper SentinelClient integration is tracked separately).
                let mut sentinel = redis::sentinel::Sentinel::build(
                    config.redis.sentinel_addresses.iter().map(String::as_str).collect::<Vec<_>>(),
                )?;

                let node_info: Option<&redis::sentinel::SentinelNodeConnectionInfo> = None;
                sentinel
                    .async_master_for(master_name.as_str(), node_info)
                    .await
                    .map_err(|e| anyhow::anyhow!("Sentinel master discovery failed: {e}"))?
            }
            RedisDeploymentMode::Cluster => {
                unreachable!("Cluster mode was already checked and rejected");
            }
        };

        let conn = redis::aio::ConnectionManager::new(client.clone()).await?;
        (Some(conn), Some(client))
    };

    // Initialize token blacklist service
    let token_blacklist = TokenBlacklistService::new(redis_conn.clone(), config.redis.key_prefix.clone());
    if token_blacklist.uses_redis() {
        info!("Token blacklist service initialized with Redis");
    } else {
        warn!("Token blacklist using in-memory fallback (no Redis) — revocations are per-instance only");
    }

    // Initialize username cache
    let username_cache = UsernameCache::new(
        redis_conn.clone(),
        format!("{}username:", config.redis.key_prefix),
        1000, // Cache up to 1000 usernames in memory
        3600, // Cache for 1 hour in Redis
    );
    info!("Username cache initialized");

    // Initialize user and room L1/L2 caches
    let user_cache = Arc::new(
        UserCache::new(
            redis_conn.clone(),
            500,  // L1 max capacity
            5,    // L1 TTL minutes
            300,  // L2 TTL seconds (5 min)
            format!("{}user:", config.redis.key_prefix),
        )?
    );
    let room_cache = Arc::new(
        RoomCache::new(
            redis_conn.clone(),
            500,  // L1 max capacity
            5,    // L1 TTL minutes
            300,  // L2 TTL seconds (5 min)
            format!("{}room:", config.redis.key_prefix),
        )?
    );
    info!("User and room caches initialized");

    // Initialize UserService
    let mut user_service = UserService::new(pool.clone(), jwt_service.clone(), token_blacklist.clone(), username_cache.clone(), config.password_complexity.clone());
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

    // Initialize ProviderInstanceRepository
    let provider_instance_repo = Arc::new(ProviderInstanceRepository::new(pool.clone()));
    info!("ProviderInstanceRepository initialized");

    // Initialize UserProviderCredentialRepository (with optional encryption)
    let credential_encryption = init_credential_encryption();
    let user_provider_credential_repo = match credential_encryption {
        Some(enc) => {
            info!("UserProviderCredentialRepository initialized with encryption enabled");
            Arc::new(UserProviderCredentialRepository::new_with_encryption(pool.clone(), enc))
        }
        None => {
            warn!(
                "Credential encryption key not configured (set SYNCTV_CREDENTIAL_ENCRYPTION_KEY). \
                 Provider credentials will be stored in plaintext."
            );
            Arc::new(UserProviderCredentialRepository::new(pool.clone()))
        }
    };

    // Initialize rate limiter
    let rate_limiter = RateLimiter::new(redis_conn.clone(), config.redis.key_prefix.clone());
    let rate_limit_config = RateLimitConfig::default();
    if redis_conn.is_some() {
        info!(
            "Rate limiter initialized (chat: {}/s, danmaku: {}/s)",
            rate_limit_config.chat_per_second, rate_limit_config.danmaku_per_second
        );
    } else {
        warn!(
            "Rate limiting using in-memory fallback (no Redis) — limits are per-instance only"
        );
    }

    // Initialize content filter
    let content_filter = ContentFilter::new();
    info!(
        "Content filter initialized (max chat: {} chars, max danmaku: {} chars)",
        content_filter.max_chat_length, content_filter.max_danmaku_length
    );

    // Initialize RemoteProviderManager (with Redis for cross-replica cache invalidation)
    info!("Initializing RemoteProviderManager...");
    let provider_instance_manager = Arc::new(RemoteProviderManager::new(
        provider_instance_repo.clone(),
        redis_conn.clone(),
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

    // Initialize OAuth2 service (optional - requires OAuth2_* env vars)
    let oauth2_service = init_oauth2_service(pool.clone(), config, redis_conn.clone()).await?;
    if oauth2_service.is_some() {
        info!("OAuth2 service initialized");
    } else {
        info!("OAuth2 service not configured (set SYNCTV_OAUTH2_ENCRYPTION_KEY)");
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
    settings_registry.init().await?;
    info!("Settings registry initialized");

    // Initialize Email service (optional - requires SMTP configuration)
    let email_service = init_email_service(config);
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
    let publish_key_service = PublishKeyService::with_default_ttl(jwt_service.clone());
    info!("Publish key service initialized");

    // Initialize User Notification service
    let notification_repo = NotificationRepository::new(pool.clone());
    let notification_service = UserNotificationService::new(notification_repo);
    info!("User notification service initialized");

    // Initialize Audit service with buffering
    let (audit_service, audit_flush_handle) = AuditService::new(pool.clone());
    info!("Audit service initialized with async buffering");

    // Store the settings listen task handle so it can be joined on shutdown.
    // The task will be cancelled via settings_cancel.

    Ok(Services {
        user_service: Arc::new(user_service),
        room_service: Arc::new(room_service),
        jwt_service,
        token_blacklist,
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
        audit_service: Arc::new(audit_service),
        cache_invalidation,
        cache_manager,
        redis_conn,
        settings_cancel,
        settings_listen_task: Arc::new(tokio::sync::Mutex::new(Some(settings_listen_task))),
        audit_flush_handle: Arc::new(tokio::sync::Mutex::new(Some(audit_flush_handle))),
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
    let oauth2_repo = UserOAuthProviderRepository::new(pool.clone());
    let oauth2_service = if let Some(conn) = redis_conn {
        info!("OAuth2 service using Redis for state storage (multi-replica safe)");
        OAuth2Service::with_redis(oauth2_repo, conn)
    } else {
        OAuth2Service::new(oauth2_repo)
    };
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
                let provider_enum = match crate::models::oauth2_client::OAuth2Provider::from_str_name(&provider_type) {
                    Some(p) => p,
                    None => {
                        warn!(
                            "Skipping unknown OAuth2 provider type '{}' for instance '{}'",
                            provider_type, instance_name
                        );
                        continue;
                    }
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

    // OAuth2 state cleanup is handled automatically by moka cache TTL and Redis SETEX.
    // No background task needed.

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
fn init_email_service(config: &Config) -> Option<Arc<EmailService>> {
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

    match EmailService::new(Some(email_config)) {
        Ok(service) => Some(Arc::new(service)),
        Err(e) => {
            error!("Failed to initialize email service: {}", e);
            None
        }
    }
}
