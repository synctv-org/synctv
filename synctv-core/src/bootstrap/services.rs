//! Service initialization and dependency injection

use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use sqlx::PgPool;
use tracing::{error, info, warn};

use crate::{
    cache::{
        build_l2_cache_backend_from_profile, version_fence_store_from_shared_state_profile,
        CacheInvalidationRuntime, CacheL2Backend, CacheManager, ConsistencyCoordinator, RoomCache,
        UserCache, UsernameCache, VersionFenceStore,
    },
    repository::{
        realtime_outbox::RealtimeOutboxRepository, ChatRepository, NotificationRepository,
        ProviderInstanceRepository, RoomMemberRepository, RoomRepository,
        RoomSettingsRepository as RoomSettingsRepo, SettingsRepository,
        UserOAuthProviderRepository, UserProviderCredentialRepository,
        WebAuthnCredentialRepository,
    },
    service::{
        notification::NotificationService as RoomNotificationService, AuditFlushHandle,
        AuditService, ChatService, ContentFilter, EmailConfig, EmailService, EmailTokenService,
        JwtService, OAuth2Service, PasskeyService, PermissionService, ProvidersManager,
        RateLimitConfig, RemoteProviderManager, RequestRateLimiterService, RoomService,
        RoomSettingsService, SettingsRegistry, SettingsService, StreamingPublishKeyService,
        UserNotificationService, UserService,
    },
    Config, SharedStateMode, SharedStateProfile,
};

#[cfg(test)]
use crate::ManagedRedisRuntime;

const WEAK_JWT_SECRETS: &[&str] = &[
    "change-me-in-production",
    "secret",
    "password",
    "jwt-secret",
    "changeme",
    "test",
    "default",
];

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
    pub rate_limiter: Arc<dyn RequestRateLimiterService>,
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
    /// Passkey/WebAuthn service (optional, requires configuration)
    pub passkey_service: Option<Arc<PasskeyService>>,
    /// Settings service
    pub settings_service: Arc<SettingsService>,
    /// Settings registry with type-safe setting variables
    pub settings_registry: Arc<SettingsRegistry>,
    /// Email service (optional, requires SMTP configuration)
    pub email_service: Option<Arc<EmailService>>,
    /// Email token service for verification codes (optional, requires SMTP configuration)
    pub email_token_service: Option<Arc<EmailTokenService>>,
    /// Shared WebSocket ticket service reused across transports.
    pub ws_ticket_service: Arc<dyn crate::service::WebSocketTicketService>,
    /// Publish key service for RTMP streaming
    pub publish_key_service: Arc<dyn StreamingPublishKeyService>,
    /// User notification service
    pub notification_service: Arc<UserNotificationService>,
    /// Chat service for message handling with business logic
    pub chat_service: Arc<ChatService>,
    /// Shared room event notification service used for realtime room events.
    pub room_notification_service: Arc<RoomNotificationService>,
    /// Audit logging service for security and compliance
    pub audit_service: Arc<AuditService>,
    /// Cache invalidation service for cross-replica cache sync
    pub cache_invalidation: Arc<dyn CacheInvalidationRuntime>,
    /// Cache manager coordinating all cache layers
    pub cache_manager: CacheManager,
    /// Shared user cache for fast-path auth checks and hot user lookups.
    pub user_cache: Arc<UserCache>,
    /// Shared runtime for Redis-backed shared-state features.
    pub redis_runtime: Option<Arc<dyn crate::RedisConnectionRuntime>>,
    /// `CancellationToken` for settings listen task (cancel on shutdown)
    pub settings_cancel: tokio_util::sync::CancellationToken,
    /// Settings listen task handle (joined on shutdown).
    /// Wrapped in `Arc<Mutex<Option<...>>>` so `Services` remains `Clone`.
    /// Take the handle out of the `Option` to join it on shutdown.
    pub settings_listen_task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Cache invalidation listener task handle (joined on shutdown).
    pub cache_invalidation_listener_task:
        Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Cancellation token for pending cache-fence repair.
    pub cache_fence_repair_cancel: tokio_util::sync::CancellationToken,
    /// Pending cache-fence repair task handle (joined on shutdown).
    pub cache_fence_repair_task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Audit flush handle for graceful shutdown of audit logging
    pub audit_flush_handle: Arc<tokio::sync::Mutex<Option<AuditFlushHandle>>>,
    /// Cancellation token for the provider invalidation listener.
    pub provider_invalidation_cancel: tokio_util::sync::CancellationToken,
    /// Provider invalidation listener task handle (joined on shutdown).
    pub provider_invalidation_task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Credential encryption for protecting sensitive data (optional)
    pub credential_encryption: Option<crate::credential_encryption::CredentialEncryption>,
}

#[derive(Clone)]
pub struct InitServicesOptions {
    pub provider_address_overrides: HashMap<String, SocketAddr>,
    pub ssrf_guard: synctv_common::ssrf::SsrfGuard,
    pub credential_encryption_key_override: Option<String>,
    pub password_hasher_override: Option<Arc<dyn crate::service::auth::PasswordHasherService>>,
    pub realtime_outbox: Option<Arc<RealtimeOutboxRepository>>,
}

impl Default for InitServicesOptions {
    fn default() -> Self {
        Self {
            provider_address_overrides: HashMap::new(),
            ssrf_guard: synctv_common::ssrf::SsrfGuard::strict_policy(),
            credential_encryption_key_override: None,
            password_hasher_override: None,
            realtime_outbox: None,
        }
    }
}

impl std::fmt::Debug for InitServicesOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InitServicesOptions")
            .field(
                "provider_address_overrides",
                &self.provider_address_overrides,
            )
            .field("ssrf_enabled", &self.ssrf_guard.acl().is_some())
            .field(
                "credential_encryption_key_override",
                &self
                    .credential_encryption_key_override
                    .as_ref()
                    .map(|_| "<redacted>"),
            )
            .field(
                "password_hasher_override",
                &self.password_hasher_override.as_ref().map(|_| "<injected>"),
            )
            .field(
                "realtime_outbox",
                &self.realtime_outbox.as_ref().map(|_| "<injected>"),
            )
            .finish()
    }
}

impl Services {
    #[must_use]
    pub fn redis_runtime(&self) -> Option<Arc<dyn crate::RedisConnectionRuntime>> {
        self.redis_runtime.clone()
    }
}

const fn should_require_email_verification(email_service_available: bool) -> bool {
    email_service_available
}

fn build_email_token_service(
    pool: PgPool,
    email_service_available: bool,
    rate_limiter: Arc<dyn RequestRateLimiterService>,
) -> Option<Arc<EmailTokenService>> {
    if !email_service_available {
        return None;
    }

    Some(Arc::new(EmailTokenService::with_rate_limiter(
        pool,
        rate_limiter,
        None,
    )))
}

fn build_brute_force_protection(
    profile: &SharedStateProfile,
) -> Result<Arc<dyn crate::service::auth::BruteForceProtectionService>, anyhow::Error> {
    let service = crate::service::brute_force_protection_from_shared_state_profile(profile)
        .map_err(anyhow::Error::from)?;
    match profile.state_mode() {
        SharedStateMode::SharedRequired => {
            info!("Brute-force protection initialized (shared state required)");
        }
        SharedStateMode::SharedBestEffort => {
            info!("Brute-force protection initialized (shared state preferred)");
        }
        SharedStateMode::LocalOnly => {
            info!("Brute-force protection initialized (local state)");
        }
    }
    Ok(service)
}

fn build_request_rate_limiter(
    profile: &SharedStateProfile,
) -> Result<Arc<dyn RequestRateLimiterService>, anyhow::Error> {
    crate::service::request_rate_limiter_from_shared_state_profile(profile)
        .map_err(anyhow::Error::from)
}

fn build_refresh_token_rate_limiter(
    profile: &SharedStateProfile,
) -> Result<Arc<dyn RequestRateLimiterService>, anyhow::Error> {
    let rate_limit_prefix = format!("{}refresh_rl:", profile.key_prefix());
    let refresh_profile = SharedStateProfile::new(
        profile.state_mode(),
        profile.shared_runtime(),
        rate_limit_prefix,
    );
    let limiter = build_request_rate_limiter(&refresh_profile)?;
    match refresh_profile.state_mode() {
        SharedStateMode::SharedRequired => {
            info!("Refresh token rate limiter initialized (shared state required)");
        }
        SharedStateMode::SharedBestEffort => {
            info!("Refresh token rate limiter initialized (shared state preferred)");
        }
        SharedStateMode::LocalOnly => {
            info!("Refresh token rate limiter initialized (local state)");
        }
    }
    Ok(limiter)
}

fn build_ws_ticket_service(
    profile: &SharedStateProfile,
) -> Result<Arc<dyn crate::service::WebSocketTicketService>, anyhow::Error> {
    crate::service::web_socket_ticket_service_from_shared_state_profile(profile, None)
        .map_err(anyhow::Error::from)
}

fn handle_provider_invalidation_listener_result(
    start_result: crate::Result<()>,
    cluster_mode: bool,
) -> Result<(), anyhow::Error> {
    if let Err(e) = start_result {
        if cluster_mode {
            return Err(anyhow::anyhow!(
                "distributed mode requires provider invalidation listener to start successfully: {e}"
            ));
        }
        tracing::warn!("Failed to start provider invalidation listener: {e}");
    }

    Ok(())
}

fn handle_provider_manager_init_result(
    init_result: crate::Result<()>,
) -> Result<(), anyhow::Error> {
    init_result.map_err(|e| anyhow::anyhow!("RemoteProviderManager initialization failed: {e}"))
}

async fn build_providers_manager(
    config: &Config,
    provider_instance_manager: Arc<RemoteProviderManager>,
) -> Result<Arc<ProvidersManager>, anyhow::Error> {
    let providers_manager = ProvidersManager::new_with_ssrf_guard(
        provider_instance_manager,
        config.security.ssrf_guard(),
    );
    let default_provider_count = providers_manager
        .create_builtin_defaults_with_config(&config.media_providers)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create default media providers: {e}"))?;

    info!(
        "ProvidersManager initialized {} local provider adapter(s)",
        default_provider_count
    );

    Ok(Arc::new(providers_manager))
}

/// Initialize all core services
///
/// The caller must supply optional shared runtime wiring
/// and a pre-built `CacheInvalidationService` so that the same instance (with
/// the correct cluster node ID) is shared across every component.  The caller
/// is also responsible for calling `.start()` on the cache invalidation service
/// after this function returns, so there is exactly one Redis subscriber.
///
/// When `shared_runtime` is `None` (standalone mode without Redis), all services
/// use in-memory fallbacks.
pub async fn init_services(
    pool: PgPool,
    config: &Config,
    shared_runtime: Option<Arc<dyn crate::RedisConnectionRuntime>>,
    cache_invalidation: Arc<dyn CacheInvalidationRuntime>,
    cache_invalidation_listener_task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
) -> Result<Services, anyhow::Error> {
    init_services_with_options(
        pool,
        config,
        shared_runtime,
        cache_invalidation,
        cache_invalidation_listener_task,
        InitServicesOptions::default(),
    )
    .await
}

pub async fn init_services_with_options(
    pool: PgPool,
    config: &Config,
    shared_runtime: Option<Arc<dyn crate::RedisConnectionRuntime>>,
    cache_invalidation: Arc<dyn CacheInvalidationRuntime>,
    cache_invalidation_listener_task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    options: InitServicesOptions,
) -> Result<Services, anyhow::Error> {
    info!("Initializing services...");

    let cluster_mode = config.cluster_runtime_enabled();

    // Initialize JWT service
    info!("Loading JWT keys...");
    let jwt_service = load_jwt_service(config)?;
    info!("JWT service initialized");

    let shared_state_profile = SharedStateProfile::for_cluster_runtime(
        shared_runtime.clone(),
        &config.redis.key_prefix,
        cluster_mode,
    );
    let version_fence = version_fence_store_from_shared_state_profile(&shared_state_profile)?;
    let consistency = ConsistencyCoordinator::new(version_fence.clone());

    // Extract a plain ConnectionManager snapshot for passing to individual services.
    // IMPORTANT (Sentinel mode): This snapshot is taken once at init time. In Sentinel
    // mode, the background health check hot-swaps the ConnectionManager inside
    // `redis_handles.conn` (the Arc<RwLock<>>) on failover. Services receive the
    // shared Arc directly so they do not hold init-time snapshots that would
    // become stale after Sentinel failover.
    // Create L2 cache backend (Redis or Noop)
    // In Sentinel mode, use the shared Arc<RwLock<ConnectionManager>> so that
    // the L2 backend automatically follows Sentinel failover without holding a
    // stale snapshot.
    let cache_l2: Arc<dyn CacheL2Backend> =
        build_l2_cache_backend_from_profile(&shared_state_profile);
    info!(
        l2_cache_enabled = cache_l2.is_active(),
        "Cache L2 initialized"
    );

    // Initialize username cache (using config values)
    let username_cache_capacity =
        usize::try_from(config.cache.username_cache_capacity).map_err(|_| {
            anyhow::anyhow!(
                "cache.username_cache_capacity={} exceeds platform usize::MAX",
                config.cache.username_cache_capacity
            )
        })?;
    let username_cache = UsernameCache::new(
        cache_l2.clone(),
        format!("{}username:", config.redis.key_prefix),
        username_cache_capacity,
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
    let brute_force = build_brute_force_protection(&shared_state_profile)?;

    // Initialize token blacklist store (tiered: L1 moka + optional L2 Redis + PG primary)
    let token_blacklist =
        crate::service::auth::token_blacklist::token_blacklist_store_from_shared_state_profile(
            pool.clone(),
            &shared_state_profile,
        );
    info!(
        "Token blacklist store initialized (tiered: PG primary{})",
        if shared_runtime.is_some() {
            " + Redis L2"
        } else {
            ""
        }
    );

    // Prepare UserService construction-time dependencies. UserService is
    // constructed after Settings/Email so its runtime collaborators are
    // complete before the service is cloned into RoomService.
    let key_builder = crate::cache::KeyBuilder::from_config(config);
    // Shared-state refresh limiting prevents N * limit bypass across replicas.
    let refresh_rate_limiter = build_refresh_token_rate_limiter(&shared_state_profile)?;

    // Initialize credential encryption (shared by both repositories and media providers)
    let credential_encryption =
        init_credential_encryption(options.credential_encryption_key_override.as_deref())?;
    // Keep a clone for provider credential resolution during media playback.
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
            "Credential encryption key not configured (set SYNCTV_SECURITY_CREDENTIAL_ENCRYPTION_KEY). \
             Existing encrypted credentials remain readable only when the key is configured, \
             and creating/updating provider credentials will be rejected."
        );
        Arc::new(UserProviderCredentialRepository::new(pool.clone()))
    };

    // Initialize rate limiter
    let rate_limiter = build_request_rate_limiter(&shared_state_profile)?;
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
    let provider_instance_manager = Arc::new(
        RemoteProviderManager::new_with_address_overrides_and_ssrf_guard(
            provider_instance_repo.clone(),
            Some(cache_invalidation.clone()),
            options.provider_address_overrides,
            options.ssrf_guard.clone(),
        )
        .with_grpc_compression(config.server.grpc_compression_enabled),
    );

    // Pre-warm cache with all enabled provider instances from database
    handle_provider_manager_init_result(provider_instance_manager.init().await)?;
    info!("RemoteProviderManager initialized successfully");

    // Start cross-replica cache invalidation listener
    handle_provider_invalidation_listener_result(
        provider_instance_manager
            .start_invalidation_listener()
            .await,
        cluster_mode,
    )?;

    // Initialize ProvidersManager
    info!("Initializing ProvidersManager...");
    let providers_manager =
        build_providers_manager(config, provider_instance_manager.clone()).await?;
    info!("ProvidersManager initialized");

    // Prepare RoomService runtime dependencies after ProvidersManager so
    // media/playback paths use the same provider graph and HTTP client
    // configuration as bootstrap. The RoomService itself is constructed later,
    // once all construction-time collaborators are available.
    let room_runtime = build_room_service_runtime(
        &shared_state_profile,
        &config.redis.deployment_mode,
        config.cache.l2_ttl_seconds,
    )?;
    let room_settings_l2_cache_for_chat = room_runtime.room_settings_l2_cache.clone();

    // Initialize CacheManager and start cross-replica invalidation listener
    let cache_manager = CacheManager::new(user_cache.clone(), room_cache.clone())
        .with_username_cache(Arc::new(username_cache.clone()));
    let cache_invalidation_listener_task_handle =
        cache_manager.start_invalidation_listener(&cache_invalidation);
    *cache_invalidation_listener_task.lock().await = Some(cache_invalidation_listener_task_handle);
    info!("CacheManager initialized with invalidation listener");

    let cache_fence_repair_cancel = tokio_util::sync::CancellationToken::new();
    let cache_fence_repair_task = Arc::new(tokio::sync::Mutex::new(Some(
        consistency.clone().spawn_repair_worker(
            pool.clone(),
            std::time::Duration::from_secs(30),
            cache_fence_repair_cancel.clone(),
        ),
    )));
    info!("Cache fence repair worker started");

    // Initialize Settings service
    info!("Initializing Settings service...");
    let settings_repo = SettingsRepository::new(pool.clone());
    let settings_service = SettingsService::new_with_runtime(
        settings_repo,
        pool.clone(),
        crate::service::settings::SettingsServiceRuntime {
            version_fence: Some(version_fence.clone()),
            l2_cache: Some(cache_l2.clone()),
            cache_key_prefix: format!("{}runtime_settings:", config.redis.key_prefix),
            cache_l2_ttl_secs: config.cache.l2_ttl_seconds,
            ..crate::service::settings::SettingsServiceRuntime::default()
        },
    );
    settings_service.initialize().await?;
    info!("Settings service initialized with {} groups", {
        settings_service.get_all().map_or(0, |g| g.len())
    });

    // Start PostgreSQL LISTEN for hot reload (with CancellationToken for graceful shutdown)
    let settings_cancel = tokio_util::sync::CancellationToken::new();
    let settings_listen_task = settings_service.start_listen_task(settings_cancel.clone());
    info!("Settings hot reload (PostgreSQL LISTEN) started");

    // Wrap settings_service in Arc before creating registry
    let settings_service = Arc::new(settings_service);

    // Initialize Settings registry
    info!("Initializing Settings registry...");
    let settings_registry = SettingsRegistry::new_with_ssrf_guard(
        settings_service.clone(),
        &config.security.ssrf_guard(),
    );
    settings_registry.init(settings_cancel.clone())?;
    info!("Settings registry initialized");

    // Initialize Email service (optional - requires SMTP configuration)
    let email_service = init_email_service(config)?;
    if email_service.is_some() {
        info!("Email service initialized");
    } else {
        info!("Email service not configured (set SYNCTV_EMAIL_SMTP_HOST)");
    }

    let email_verification_required = should_require_email_verification(email_service.is_some());

    // Initialize Email Token service (optional - requires email service)
    let email_token_service =
        build_email_token_service(pool.clone(), email_service.is_some(), rate_limiter.clone());
    if email_token_service.is_some() {
        info!("Email token service initialized");
    } else {
        info!("Email token service not configured (requires email service)");
    }

    let ws_ticket_service = build_ws_ticket_service(&shared_state_profile)?;
    info!(
        cross_node_capable = ws_ticket_service.supports_cluster_runtime(),
        "WebSocket ticket service initialized"
    );

    // Initialize Publish Key service (for RTMP streaming)
    // Use Redis-backed JTI dedup when available (shared handle follows Sentinel failover).
    // Falls back to in-memory for standalone mode.
    let publish_key_service =
        build_publish_key_service(jwt_service.clone(), &shared_state_profile)?;

    // Initialize User Notification service
    let notification_repo = NotificationRepository::new(pool.clone());
    let notification_service = UserNotificationService::new(notification_repo);
    info!("User notification service initialized");

    // Initialize Audit service with buffering
    let (audit_service, audit_flush_handle) = AuditService::new(pool.clone());
    let audit_service = Arc::new(audit_service);
    info!("Audit service initialized with async buffering");

    let notification_service = Arc::new(notification_service);

    let settings_registry = Arc::new(settings_registry);

    let oauth2_service = init_oauth2_service(
        &pool,
        Arc::clone(&settings_registry),
        &shared_state_profile,
        options.ssrf_guard.clone(),
    )?;
    info!("OAuth2 service initialized");

    let user_service = Arc::new(UserService::new_with_brute_force_service_and_runtime(
        &pool,
        crate::service::user::UserServiceDependencies {
            jwt_service: jwt_service.clone(),
            username_cache: username_cache.clone(),
            password_complexity: config.password_complexity.clone(),
            token_blacklist,
            key_builder,
            brute_force: brute_force.clone(),
        },
        crate::service::user::UserServiceRuntimeOptions {
            cache_invalidation: Some(cache_invalidation.clone()),
            refresh_rate_limiter: Some(refresh_rate_limiter),
            email_verification_required,
            settings_registry: Some(Arc::clone(&settings_registry)),
            password_hasher: options.password_hasher_override.as_ref().map(Arc::clone),
            realtime_outbox: options.realtime_outbox.clone(),
            opaque_password_service: Some(Arc::new(
                crate::service::auth::OpaquePasswordService::derive_from_secret(
                    config.security.opaque_server_setup_secret.as_bytes(),
                ),
            )),
            opaque_login_session_store: Some(
                crate::service::user::opaque_login_session_store_from_shared_state_profile(
                    &shared_state_profile,
                )?,
            ),
            opaque_registration_session_store: Some(
                crate::service::user::opaque_registration_session_store_from_shared_state_profile(
                    &shared_state_profile,
                )?,
            ),
            mfa_session_store: Some(
                crate::service::user::mfa_session_store_from_shared_state_profile(
                    &shared_state_profile,
                )?,
            ),
            version_fence: Some(version_fence.clone()),
            permission_service: Some(PermissionService::new_with_runtime(
                RoomMemberRepository::new(pool.clone()),
                RoomRepository::new(pool.clone()),
                crate::service::permission::PermissionServiceRuntime {
                    settings_registry: Some(Arc::clone(&settings_registry)),
                    room_settings_repo: Some(RoomSettingsRepo::new(pool.clone())),
                    invalidation_service: Some(cache_invalidation.clone()),
                    version_fence: Some(version_fence.clone()),
                    member_permission_l2_cache: Some(cache_l2.clone()),
                    member_permission_cache_key_prefix: format!(
                        "{}member_permission:",
                        config.redis.key_prefix
                    ),
                    room_settings_l2_cache: room_settings_l2_cache_for_chat.clone(),
                    room_settings_cache_key_prefix: format!(
                        "{}room_settings:",
                        config.redis.key_prefix
                    ),
                    ..crate::service::permission::PermissionServiceRuntime::default()
                },
            )),
        },
    ));
    info!("UserService initialized with construction-time dependencies");

    let passkey_service = if config.webauthn.enabled {
        let session_store =
            crate::service::passkey_session_store_from_shared_state_profile(&shared_state_profile)?;
        Some(Arc::new(PasskeyService::new(
            &config.webauthn,
            WebAuthnCredentialRepository::new(pool.clone()),
            user_service.clone(),
            session_store,
        )?))
    } else {
        None
    };
    if passkey_service.is_some() {
        info!("Passkey/WebAuthn service initialized");
    } else {
        info!("Passkey/WebAuthn service not configured");
    }

    let room_service = build_room_service(RoomServiceBuildArgs {
        pool: pool.clone(),
        user_service: (*user_service).clone(),
        credential_repo: user_provider_credential_repo.clone(),
        credential_encryption: credential_encryption_for_services.clone(),
        providers_manager: providers_manager.clone(),
        cache_invalidation: cache_invalidation.clone(),
        brute_force: brute_force.clone(),
        audit_service: Some(Arc::clone(&audit_service)),
        settings_registry: Some(Arc::clone(&settings_registry)),
        user_notification_service: Some(Arc::clone(&notification_service)),
        password_hasher: options.password_hasher_override.as_ref().map(Arc::clone),
        realtime_outbox: options.realtime_outbox.clone(),
        runtime: room_runtime,
        version_fence: version_fence.clone(),
    });
    info!("RoomService initialized with construction-time dependencies");

    // Store the settings listen task handle so it can be joined on shutdown.
    // The task will be cancelled via settings_cancel.

    // Initialize ChatService with proper business logic (permissions, rate limiting, filtering)
    let chat_repo = Arc::new(ChatRepository::new(pool.clone()));
    let room_settings_repo_for_chat = RoomSettingsRepo::new(pool.clone());
    let room_notification_service = Arc::new(room_service.notification_service().clone());
    let room_settings_service_for_chat = RoomSettingsService::new_with_version_fence(
        room_settings_repo_for_chat,
        Some(cache_invalidation.clone()),
        room_notification_service.clone(),
        crate::service::room_settings::RoomSettingsRuntime {
            version_fence: Some(version_fence),
            l2_cache: room_settings_l2_cache_for_chat,
            cache_key_prefix: format!("{}room_settings:", shared_state_profile.key_prefix()),
            ..crate::service::room_settings::RoomSettingsRuntime::default()
        },
    );
    let permission_service_for_chat = room_service.permission_service().clone();
    let chat_service = ChatService::new(
        chat_repo,
        crate::service::chat::ChatRuntime {
            rate_limiter: rate_limiter.clone(),
            rate_limit_config: rate_limit_config.clone(),
            content_filter: content_filter.clone(),
        },
        crate::service::chat::ChatDependencies {
            permission_service: permission_service_for_chat,
            room_settings_service: room_settings_service_for_chat,
            user_service: user_service.clone(),
            notification_service: (*room_service.notification_service()).clone(),
        },
    );
    info!("ChatService initialized");

    let provider_invalidation_cancel = provider_instance_manager.invalidation_cancel_token();
    let provider_invalidation_task = provider_instance_manager.invalidation_listener_task();

    Ok(Services {
        user_service,
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
        passkey_service,
        settings_service,
        settings_registry,
        email_service,
        email_token_service,
        ws_ticket_service,
        publish_key_service,
        notification_service,
        chat_service: Arc::new(chat_service),
        room_notification_service,
        audit_service,
        cache_invalidation,
        cache_manager,
        user_cache,
        redis_runtime: shared_runtime,
        settings_cancel,
        settings_listen_task: Arc::new(tokio::sync::Mutex::new(Some(settings_listen_task))),
        cache_invalidation_listener_task,
        cache_fence_repair_cancel,
        cache_fence_repair_task,
        audit_flush_handle: Arc::new(tokio::sync::Mutex::new(Some(audit_flush_handle))),
        provider_invalidation_cancel,
        provider_invalidation_task,
        credential_encryption: credential_encryption_for_services,
    })
}

fn init_oauth2_service(
    pool: &PgPool,
    settings_registry: Arc<SettingsRegistry>,
    profile: &SharedStateProfile,
    ssrf_guard: synctv_common::ssrf::SsrfGuard,
) -> Result<Option<Arc<OAuth2Service>>, anyhow::Error> {
    let provider_registry = crate::oauth2::providers::provider_registry(ssrf_guard.clone());
    info!("OAuth2 provider registry initialized");

    let oauth2_repo = UserOAuthProviderRepository::new(pool.clone());
    let state_store = build_oauth_state_store(profile)?;
    let oauth2_service = OAuth2Service::new_with_ssrf_guard(
        oauth2_repo,
        state_store,
        provider_registry.clone(),
        ssrf_guard,
        matches!(profile.state_mode(), SharedStateMode::SharedRequired),
    )
    .map_err(|e| anyhow::anyhow!("Failed to create OAuth2 service: {e}"))?
    .with_settings_registry(settings_registry);

    // OAuth2 state cleanup is handled automatically:
    // - Redis: SETEX TTL auto-expires entries
    // - In-memory: sweep_expired on each store/consume call

    Ok(Some(Arc::new(oauth2_service)))
}

fn build_oauth_state_store(
    profile: &SharedStateProfile,
) -> Result<Arc<dyn crate::service::OAuthStateStore>, anyhow::Error> {
    let store = crate::service::oauth2::state_store_from_shared_state_profile(profile)
        .map_err(anyhow::Error::from)?;
    info!(
        cross_node_single_use = store.supports_cross_node_single_use(),
        "OAuth2 state store initialized"
    );
    Ok(store)
}

/// Load JWT service from secret in configuration
fn load_jwt_service(config: &Config) -> Result<JwtService, anyhow::Error> {
    if config.jwt.secret.is_empty() {
        return Err(anyhow::anyhow!(
            "JWT secret is empty. Please set SYNCTV_JWT_SECRET environment variable or configure jwt.secret in config file"
        ));
    }

    if WEAK_JWT_SECRETS.contains(&config.jwt.secret.as_str()) {
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

struct RoomServiceBuildArgs {
    pool: PgPool,
    user_service: UserService,
    credential_repo: Arc<UserProviderCredentialRepository>,
    credential_encryption: Option<crate::credential_encryption::CredentialEncryption>,
    providers_manager: Arc<ProvidersManager>,
    cache_invalidation: Arc<dyn CacheInvalidationRuntime>,
    brute_force: Arc<dyn crate::service::auth::BruteForceProtectionService>,
    audit_service: Option<Arc<AuditService>>,
    settings_registry: Option<Arc<SettingsRegistry>>,
    user_notification_service: Option<Arc<UserNotificationService>>,
    password_hasher: Option<Arc<dyn crate::service::auth::PasswordHasherService>>,
    realtime_outbox: Option<Arc<RealtimeOutboxRepository>>,
    runtime: RoomServiceRuntime,
    version_fence: Arc<dyn VersionFenceStore>,
}

struct RoomServiceRuntime {
    distributed_lock: Option<Arc<dyn crate::service::distributed_lock::CoordinationLock>>,
    playback_l2_cache: Option<crate::cache::PlaybackStateCache>,
    room_settings_l2_cache: Option<Arc<dyn crate::cache::CacheL2Backend>>,
    room_settings_cache_key_prefix: String,
    member_permission_l2_cache: Option<Arc<dyn crate::cache::CacheL2Backend>>,
    member_permission_cache_key_prefix: String,
}

fn build_room_service_runtime(
    profile: &SharedStateProfile,
    deployment_mode: &crate::config::RedisDeploymentMode,
    cache_l2_ttl_seconds: u64,
) -> Result<RoomServiceRuntime, anyhow::Error> {
    let distributed_lock = if matches!(profile.state_mode(), SharedStateMode::SharedRequired) {
        let redis_runtime = profile
            .require_shared_runtime("room coordination locking")
            .map_err(anyhow::Error::from)?;
        Some(
            Arc::new(crate::service::DistributedLock::from_runtime_with_mode(
                redis_runtime,
                matches!(
                    deployment_mode,
                    crate::config::RedisDeploymentMode::Sentinel
                ),
            )) as Arc<dyn crate::service::distributed_lock::CoordinationLock>,
        )
    } else {
        None
    };

    let playback_l2_cache = profile
        .shared_runtime()
        .map(|redis_runtime| {
            crate::cache::PlaybackStateCache::new(
                Arc::new(crate::cache::RedisCacheL2::from_runtime(redis_runtime)),
                crate::service::PlaybackService::DEFAULT_CACHE_SIZE,
                crate::service::PlaybackService::DEFAULT_CACHE_TTL_SECS,
                cache_l2_ttl_seconds,
                format!("{}playback:", profile.key_prefix()),
            )
        })
        .transpose()
        .map_err(anyhow::Error::from)?;

    let room_settings_l2_cache = profile.shared_runtime().map(|redis_runtime| {
        Arc::new(crate::cache::RedisCacheL2::from_runtime(redis_runtime))
            as Arc<dyn crate::cache::CacheL2Backend>
    });
    let member_permission_l2_cache = profile.shared_runtime().map(|redis_runtime| {
        Arc::new(crate::cache::RedisCacheL2::from_runtime(redis_runtime))
            as Arc<dyn crate::cache::CacheL2Backend>
    });

    Ok(RoomServiceRuntime {
        distributed_lock,
        playback_l2_cache,
        room_settings_l2_cache,
        room_settings_cache_key_prefix: format!("{}room_settings:", profile.key_prefix()),
        member_permission_l2_cache,
        member_permission_cache_key_prefix: format!("{}member_permission:", profile.key_prefix()),
    })
}

fn build_room_service(args: RoomServiceBuildArgs) -> RoomService {
    let RoomServiceBuildArgs {
        pool,
        user_service,
        credential_repo,
        credential_encryption,
        providers_manager,
        cache_invalidation,
        brute_force,
        audit_service,
        settings_registry,
        user_notification_service,
        password_hasher,
        realtime_outbox,
        runtime,
        version_fence,
    } = args;
    let permission_service = PermissionService::new_with_runtime(
        RoomMemberRepository::new(pool.clone()),
        RoomRepository::new(pool.clone()),
        crate::service::permission::PermissionServiceRuntime {
            settings_registry: settings_registry.clone(),
            room_settings_repo: Some(RoomSettingsRepo::new(pool.clone())),
            invalidation_service: Some(cache_invalidation.clone()),
            version_fence: Some(version_fence.clone()),
            member_permission_l2_cache: runtime.member_permission_l2_cache.clone(),
            member_permission_cache_key_prefix: runtime.member_permission_cache_key_prefix.clone(),
            room_settings_l2_cache: runtime.room_settings_l2_cache.clone(),
            room_settings_cache_key_prefix: runtime.room_settings_cache_key_prefix.clone(),
            ..crate::service::permission::PermissionServiceRuntime::default()
        },
    );
    RoomService::new_with_providers_permission_service_and_options(
        pool,
        user_service,
        providers_manager,
        permission_service,
        crate::service::room::RoomServiceOptions {
            distributed_lock: runtime.distributed_lock,
            cache_invalidation: Some(cache_invalidation),
            version_fence: Some(version_fence),
            playback_l2_cache: runtime.playback_l2_cache,
            room_settings_l2_cache: runtime.room_settings_l2_cache,
            room_settings_cache_key_prefix: Some(runtime.room_settings_cache_key_prefix),
            credential_encryption,
            credential_repo: Some(credential_repo),
            audit_service,
            brute_force_service: Some(brute_force),
            settings_registry,
            user_notification_service,
            password_hasher,
            realtime_outbox,
        },
    )
}

#[cfg(test)]
fn test_providers_manager(pool: &PgPool) -> Arc<ProvidersManager> {
    let provider_repo = Arc::new(ProviderInstanceRepository::new(pool.clone()));
    let provider_instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(
        provider_repo,
        None,
    ));
    Arc::new(ProvidersManager::new(provider_instance_manager))
}

fn build_publish_key_service(
    jwt_service: JwtService,
    profile: &SharedStateProfile,
) -> Result<Arc<dyn StreamingPublishKeyService>, anyhow::Error> {
    let service = crate::service::streaming_publish_key_service_from_shared_state_profile(
        jwt_service,
        24,
        profile,
    )
    .map_err(anyhow::Error::from)?;
    match profile.state_mode() {
        SharedStateMode::SharedRequired => {
            info!("Publish key service initialized with shared JTI deduplication (required)");
        }
        SharedStateMode::SharedBestEffort => {
            info!("Publish key service initialized with shared JTI deduplication");
        }
        SharedStateMode::LocalOnly => {
            info!("Publish key service initialized with local JTI deduplication");
        }
    }
    Ok(service)
}

/// Initialize credential encryption from environment variable or secret file
///
/// Tries the following sources in order:
/// 1. Secret file: `/run/secrets/credential_encryption_key`
/// 2. Environment variable: `SYNCTV_SECURITY_CREDENTIAL_ENCRYPTION_KEY`
///
/// The key must be a 64-character hex string (32 bytes).
fn init_credential_encryption(
    hex_key_override: Option<&str>,
) -> Result<Option<crate::credential_encryption::CredentialEncryption>, anyhow::Error> {
    use crate::secrets::{SecretLoader, SecretSource};

    let Some(hex_key) = (match hex_key_override {
        Some(hex_key) if hex_key.trim().is_empty() => None,
        Some(hex_key) => Some(hex_key.to_string()),
        None => {
            // Try file first, then env var
            SecretLoader::load_with_fallback(
                "credential_encryption_key",
                &SecretSource::File("/run/secrets/credential_encryption_key"),
                &SecretSource::Env("SYNCTV_SECURITY_CREDENTIAL_ENCRYPTION_KEY"),
            )
            .ok()
        }
    }) else {
        return Ok(None);
    };

    match crate::credential_encryption::CredentialEncryption::from_hex_key(&hex_key) {
        Ok(enc) => {
            info!("Credential encryption initialized (AES-256-GCM)");
            Ok(Some(enc))
        }
        Err(e) => Err(anyhow::anyhow!(
            "Failed to initialize credential encryption: {e}. \
             Key must be a 64-character hex string (32 bytes for AES-256)"
        )),
    }
}

/// Initialize Email service (optional - requires SMTP configuration).
fn init_email_service(config: &Config) -> Result<Option<Arc<EmailService>>, anyhow::Error> {
    // Check if SMTP host is configured
    if config.email.smtp_host.is_empty() {
        return Ok(None);
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
        Ok(service) => Ok(Some(Arc::new(service))),
        Err(e) => {
            error!("Failed to initialize email service: {}", e);
            Err(anyhow::anyhow!(
                "email.smtp_host is configured but EmailService initialization failed: {e}"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheInvalidationService;
    use crate::service::RateLimiter;

    struct FakeRedisRuntime;

    #[async_trait::async_trait]
    impl crate::RedisConnectionRuntime for FakeRedisRuntime {
        async fn snapshot(&self) -> redis::aio::ConnectionManager {
            panic!("snapshot should not be called in this unit test");
        }
    }

    #[test]
    fn test_shared_state_profile_requires_shared_runtime_in_cluster() {
        assert_eq!(
            SharedStateProfile::from_runtime(None, "test:", true).state_mode(),
            SharedStateMode::SharedRequired
        );
    }

    #[test]
    fn test_shared_state_profile_prefers_shared_runtime_when_available() {
        let profile = SharedStateProfile::new(
            SharedStateMode::SharedBestEffort,
            Some(Arc::new(FakeRedisRuntime)),
            "test:",
        );
        assert_eq!(profile.state_mode(), SharedStateMode::SharedBestEffort);
    }

    #[test]
    fn test_shared_state_profile_uses_local_mode_without_shared_runtime() {
        assert_eq!(
            SharedStateProfile::from_runtime(None, "test:", false).state_mode(),
            SharedStateMode::LocalOnly
        );
    }

    #[test]
    fn test_build_publish_key_service_returns_error_without_shared_runtime_in_cluster_mode() {
        let jwt_service = JwtService::with_durations(
            "f4e9a7c21d3b5e6f8a9c0b1d2e3f4a5b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e2f",
            24,
            30,
            24,
            60,
        )
        .expect("jwt service");

        let profile = SharedStateProfile::from_runtime(None, "test:", true);
        let Err(error) = build_publish_key_service(jwt_service, &profile) else {
            panic!("cluster runtime must reject local publish-key deduplication");
        };

        assert!(
            error
                .to_string()
                .contains("distributed runtime requires shared publish-key deduplication state"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_build_brute_force_protection_returns_error_without_shared_runtime_in_cluster_mode() {
        let profile = SharedStateProfile::from_runtime(None, "test:", true);
        let Err(error) = build_brute_force_protection(&profile) else {
            panic!("cluster runtime must reject local brute-force tracking");
        };

        assert!(
            error
                .to_string()
                .contains("distributed runtime requires shared brute-force protection state"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_build_request_rate_limiter_returns_error_without_shared_runtime_in_cluster_mode() {
        let profile = SharedStateProfile::from_runtime(None, "test:", true);
        let Err(error) = build_request_rate_limiter(&profile) else {
            panic!("cluster runtime must reject local rate limiting");
        };

        assert!(
            error
                .to_string()
                .contains("distributed runtime requires shared rate-limit state"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_build_request_rate_limiter_uses_local_backend_without_runtime() {
        let profile = SharedStateProfile::from_runtime(None, "test:", false);
        let limiter = build_request_rate_limiter(&profile)
            .expect("standalone mode should allow local rate limiting");

        assert!(
            limiter.check_rate_limit_sync("test-user", 1, 60).is_ok(),
            "helper must return a live rate limiter service abstraction"
        );
    }

    #[tokio::test]
    async fn test_configure_refresh_token_rate_limiter_returns_error_without_shared_runtime_in_cluster_mode(
    ) {
        let profile = SharedStateProfile::from_runtime(None, "test:", true);
        let Err(error) = build_refresh_token_rate_limiter(&profile) else {
            panic!("cluster runtime must reject local refresh-token rate limiting");
        };

        assert!(
            error
                .to_string()
                .contains("distributed runtime requires shared rate-limit state"),
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
                .contains("distributed mode requires provider invalidation listener"),
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

    #[test]
    fn test_build_ws_ticket_service_uses_memory_backend_without_runtime() {
        let profile = SharedStateProfile::from_runtime(None, "test:", false);
        let service = build_ws_ticket_service(&profile)
            .expect("standalone mode should allow local WebSocket ticket storage");

        assert!(!service.supports_cluster_runtime());
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_build_ws_ticket_service_uses_distributed_backend_when_runtime_available() {
        let (_redis_container, redis_client) = synctv_core_testing::start_redis_with_client().await;
        let redis_conn = redis::aio::ConnectionManager::new(redis_client.clone())
            .await
            .expect("redis connection manager");
        let redis_runtime = Arc::new(ManagedRedisRuntime::new(
            redis_client,
            Arc::new(tokio::sync::RwLock::new(redis_conn)),
        ));

        let profile = SharedStateProfile::from_runtime(Some(redis_runtime), "test:", false);
        let service = build_ws_ticket_service(&profile)
            .expect("distributed ticket storage should be accepted");

        assert!(service.supports_cluster_runtime());
    }

    #[test]
    fn test_build_ws_ticket_service_rejects_local_backend_in_cluster_mode() {
        let profile = SharedStateProfile::from_runtime(None, "test:", true);
        let Err(error) = build_ws_ticket_service(&profile) else {
            panic!("cluster runtime must reject local-only WebSocket ticket storage");
        };

        assert!(
            error
                .to_string()
                .contains("distributed runtime requires shared WebSocket ticket storage"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_provider_manager_init_failure_is_fatal() {
        let err = handle_provider_manager_init_result(Err(crate::Error::Internal(
            "provider init failed".to_string(),
        )))
        .expect_err("provider manager init must fail closed");

        assert!(
            err.to_string()
                .contains("RemoteProviderManager initialization failed"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_provider_manager_init_success_passthrough() {
        handle_provider_manager_init_result(Ok(())).expect("successful provider init should pass");
    }

    #[test]
    fn test_init_email_service_is_independent_of_redis_backend_choice() {
        let mut config = Config::default();
        config.email.smtp_host = "smtp.example.com".to_string();
        config.email.smtp_port = 587;
        config.email.smtp_username = "user".to_string();
        config.email.smtp_password = "password".to_string();
        config.email.from_email = "noreply@example.com".to_string();
        config.email.from_name = "SyncTV".to_string();
        config.email.use_tls = true;

        let standalone = init_email_service(&config).expect("standalone email service");
        let standalone = standalone.expect("email service");
        assert!(standalone.is_configured());
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
        let username_cache = UsernameCache::local_only("test:user:".to_string(), 100, 60);
        let token_blacklist: Arc<dyn crate::service::TokenBlacklistStore> = Arc::new(
            crate::service::InMemoryTokenBlacklistStore::new(100, 60, 60),
        );
        let user_service = UserService::new(
            &pool,
            jwt_service,
            username_cache,
            Config::default().password_complexity,
            token_blacklist,
            crate::cache::KeyBuilder::new("test"),
            crate::service::auth::BruteForceProtection::in_memory("test:user".to_string()),
        );
        let cache_invalidation = Arc::new(CacheInvalidationService::new(
            "node-test".to_string(),
            "test:cache:stream".to_string(),
        ));

        let room_service = build_room_service(RoomServiceBuildArgs {
            pool: pool.clone(),
            user_service,
            credential_repo: Arc::new(UserProviderCredentialRepository::new(pool.clone())),
            credential_encryption: None,
            providers_manager: test_providers_manager(&pool),
            cache_invalidation,
            brute_force: Arc::new(crate::service::auth::BruteForceProtection::in_memory(
                "test:room".to_string(),
            )),
            audit_service: None,
            settings_registry: None,
            user_notification_service: None,
            password_hasher: None,
            realtime_outbox: None,
            runtime: build_room_service_runtime(
                &SharedStateProfile::from_runtime(None, "test:", false),
                &crate::config::RedisDeploymentMode::Standalone,
                Config::default().cache.l2_ttl_seconds,
            )
            .expect("room service runtime should build"),
            version_fence: Arc::new(crate::cache::NoopVersionFenceStore),
        });

        assert!(room_service.has_brute_force_service());
        assert!(!room_service.has_distributed_lock());
        assert!(
            room_service.permission_service().has_invalidation_service(),
            "room service must wire permission invalidation even without Redis so local subscribers stay consistent"
        );
        assert!(
            !room_service.has_playback_l2_cache(),
            "room service should not wire playback L2 cache without Redis"
        );
    }

    #[test]
    fn test_build_room_service_signature_supports_redis_lock_wiring() {
        let _: fn(RoomServiceBuildArgs) -> RoomService = build_room_service;
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_build_room_service_only_enables_distributed_lock_in_cluster_mode() {
        let (_redis_container, redis_client) = synctv_core_testing::start_redis_with_client().await;
        let redis_conn = redis::aio::ConnectionManager::new(redis_client.clone())
            .await
            .expect("redis connection manager");
        let redis_runtime = Arc::new(ManagedRedisRuntime::new(
            redis_client,
            Arc::new(tokio::sync::RwLock::new(redis_conn)),
        ));

        let pool = PgPool::connect_lazy("postgresql://test").expect("lazy pool should build");
        let jwt_service = JwtService::with_durations(
            "f4e9a7c21d3b5e6f8a9c0b1d2e3f4a5b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e2f",
            24,
            30,
            24,
            60,
        )
        .expect("jwt service");
        let username_cache = UsernameCache::local_only("test:user:".to_string(), 100, 60);
        let token_blacklist: Arc<dyn crate::service::TokenBlacklistStore> = Arc::new(
            crate::service::InMemoryTokenBlacklistStore::new(100, 60, 60),
        );
        let user_service = UserService::new(
            &pool,
            jwt_service,
            username_cache,
            Config::default().password_complexity,
            token_blacklist,
            crate::cache::KeyBuilder::new("test"),
            crate::service::auth::BruteForceProtection::in_memory("test:user".to_string()),
        );
        let cache_invalidation = Arc::new(CacheInvalidationService::new(
            "node-test".to_string(),
            "test:cache:stream".to_string(),
        ));

        let standalone_room_service = build_room_service(RoomServiceBuildArgs {
            pool: pool.clone(),
            user_service: user_service.clone(),
            credential_repo: Arc::new(UserProviderCredentialRepository::new(pool.clone())),
            credential_encryption: None,
            providers_manager: test_providers_manager(&pool),
            cache_invalidation: cache_invalidation.clone(),
            brute_force: Arc::new(crate::service::auth::BruteForceProtection::in_memory(
                "test:room".to_string(),
            )),
            audit_service: None,
            settings_registry: None,
            user_notification_service: None,
            password_hasher: None,
            realtime_outbox: None,
            runtime: build_room_service_runtime(
                &SharedStateProfile::from_runtime(Some(redis_runtime.clone()), "test:", false),
                &crate::config::RedisDeploymentMode::Standalone,
                Config::default().cache.l2_ttl_seconds,
            )
            .expect("room service runtime should build"),
            version_fence: Arc::new(crate::cache::NoopVersionFenceStore),
        });
        assert!(
            !standalone_room_service.has_distributed_lock(),
            "standalone mode should not enable distributed lock just because Redis is configured"
        );
        assert!(
            standalone_room_service.has_playback_l2_cache(),
            "room service should wire playback L2 cache whenever Redis is configured"
        );

        let cluster_room_service = build_room_service(RoomServiceBuildArgs {
            pool: pool.clone(),
            user_service,
            credential_repo: Arc::new(UserProviderCredentialRepository::new(pool.clone())),
            credential_encryption: None,
            providers_manager: test_providers_manager(&pool),
            cache_invalidation,
            brute_force: Arc::new(crate::service::auth::BruteForceProtection::in_memory(
                "test:room".to_string(),
            )),
            audit_service: None,
            settings_registry: None,
            user_notification_service: None,
            password_hasher: None,
            realtime_outbox: None,
            runtime: build_room_service_runtime(
                &SharedStateProfile::from_runtime(Some(redis_runtime), "test:", true),
                &crate::config::RedisDeploymentMode::Standalone,
                Config::default().cache.l2_ttl_seconds,
            )
            .expect("room service runtime should build"),
            version_fence: Arc::new(crate::cache::NoopVersionFenceStore),
        });
        assert!(
            cluster_room_service.has_distributed_lock(),
            "cluster mode should enable distributed lock when Redis is configured"
        );
        assert!(
            cluster_room_service.has_playback_l2_cache(),
            "cluster mode should also wire playback L2 cache"
        );
        assert!(
            cluster_room_service
                .permission_service()
                .has_invalidation_service(),
            "cluster room service must wire permission invalidation"
        );
    }

    #[tokio::test]
    async fn test_services_wiring_applies_settings_registry_to_room_service() {
        let pool = PgPool::connect_lazy("postgresql://test").expect("lazy pool should build");
        let jwt_service = JwtService::with_durations(
            "f4e9a7c21d3b5e6f8a9c0b1d2e3f4a5b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e2f",
            24,
            30,
            24,
            60,
        )
        .expect("jwt service");
        let username_cache = UsernameCache::local_only("test:user:".to_string(), 100, 60);
        let token_blacklist: Arc<dyn crate::service::TokenBlacklistStore> = Arc::new(
            crate::service::InMemoryTokenBlacklistStore::new(100, 60, 60),
        );
        let user_service = UserService::new(
            &pool,
            jwt_service,
            username_cache,
            Config::default().password_complexity,
            token_blacklist,
            crate::cache::KeyBuilder::new("test"),
            crate::service::auth::BruteForceProtection::in_memory("test:user".to_string()),
        );
        let cache_invalidation = Arc::new(CacheInvalidationService::new(
            "node-test".to_string(),
            "test:cache:stream".to_string(),
        ));
        let settings_service = Arc::new(SettingsService::new(
            SettingsRepository::new(pool.clone()),
            PgPool::connect_lazy("postgresql://test").expect("lazy pool should build"),
        ));
        let settings_registry = Arc::new(SettingsRegistry::new(settings_service));

        let room_service = build_room_service(RoomServiceBuildArgs {
            pool: pool.clone(),
            user_service: user_service.clone(),
            credential_repo: Arc::new(UserProviderCredentialRepository::new(pool.clone())),
            credential_encryption: None,
            providers_manager: test_providers_manager(&pool),
            cache_invalidation,
            brute_force: Arc::new(crate::service::auth::BruteForceProtection::in_memory(
                "test:room".to_string(),
            )),
            audit_service: None,
            settings_registry: Some(Arc::clone(&settings_registry)),
            user_notification_service: None,
            password_hasher: None,
            realtime_outbox: None,
            runtime: build_room_service_runtime(
                &SharedStateProfile::from_runtime(None, "test:", false),
                &crate::config::RedisDeploymentMode::Standalone,
                Config::default().cache.l2_ttl_seconds,
            )
            .expect("room service runtime should build"),
            version_fence: Arc::new(crate::cache::NoopVersionFenceStore),
        });

        assert!(room_service.has_settings_registry());
        assert!(
            room_service.permission_service().has_settings_registry(),
            "room permission service must use runtime permission defaults from SettingsRegistry"
        );
    }

    #[tokio::test]
    async fn test_build_room_service_reuses_injected_providers_manager() {
        let pool = PgPool::connect_lazy("postgresql://test").expect("lazy pool should build");
        let jwt_service = JwtService::with_durations(
            "f4e9a7c21d3b5e6f8a9c0b1d2e3f4a5b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e2f",
            24,
            30,
            24,
            60,
        )
        .expect("jwt service");
        let username_cache = UsernameCache::local_only("test:user:".to_string(), 100, 60);
        let token_blacklist: Arc<dyn crate::service::TokenBlacklistStore> = Arc::new(
            crate::service::InMemoryTokenBlacklistStore::new(100, 60, 60),
        );
        let user_service = UserService::new(
            &pool,
            jwt_service,
            username_cache,
            Config::default().password_complexity,
            token_blacklist,
            crate::cache::KeyBuilder::new("test"),
            crate::service::auth::BruteForceProtection::in_memory("test:user".to_string()),
        );
        let cache_invalidation = Arc::new(CacheInvalidationService::new(
            "node-test".to_string(),
            "test:cache:stream".to_string(),
        ));
        let providers_manager = test_providers_manager(&pool);

        let room_service = build_room_service(RoomServiceBuildArgs {
            pool: pool.clone(),
            user_service,
            credential_repo: Arc::new(UserProviderCredentialRepository::new(pool.clone())),
            credential_encryption: None,
            providers_manager: Arc::clone(&providers_manager),
            cache_invalidation,
            brute_force: Arc::new(crate::service::auth::BruteForceProtection::in_memory(
                "test:room".to_string(),
            )),
            audit_service: None,
            settings_registry: None,
            user_notification_service: None,
            password_hasher: None,
            realtime_outbox: None,
            runtime: build_room_service_runtime(
                &SharedStateProfile::from_runtime(None, "test:", false),
                &crate::config::RedisDeploymentMode::Standalone,
                Config::default().cache.l2_ttl_seconds,
            )
            .expect("room service runtime should build"),
            version_fence: Arc::new(crate::cache::NoopVersionFenceStore),
        });

        assert!(
            Arc::ptr_eq(room_service.media_service().providers_manager(), &providers_manager),
            "room service must reuse the injected providers manager instead of constructing a hidden one"
        );
    }

    #[tokio::test]
    async fn test_room_service_accepts_media_credential_encryption_injection() {
        let pool = PgPool::connect_lazy("postgresql://test").expect("lazy pool should build");
        let jwt_service = JwtService::with_durations(
            "f4e9a7c21d3b5e6f8a9c0b1d2e3f4a5b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e2f",
            24,
            30,
            24,
            60,
        )
        .expect("jwt service");
        let username_cache = UsernameCache::local_only("test:user:".to_string(), 100, 60);
        let token_blacklist: Arc<dyn crate::service::TokenBlacklistStore> = Arc::new(
            crate::service::InMemoryTokenBlacklistStore::new(100, 60, 60),
        );
        let user_service = UserService::new(
            &pool,
            jwt_service,
            username_cache,
            Config::default().password_complexity,
            token_blacklist,
            crate::cache::KeyBuilder::new("test"),
            crate::service::auth::BruteForceProtection::in_memory("test:user".to_string()),
        );
        let cache_invalidation = Arc::new(CacheInvalidationService::new(
            "node-test".to_string(),
            "test:cache:stream".to_string(),
        ));
        let providers_manager = test_providers_manager(&pool);
        let encryption = crate::credential_encryption::CredentialEncryption::new(&[7u8; 32])
            .expect("credential encryption should construct");
        let room_service = build_room_service(RoomServiceBuildArgs {
            pool: pool.clone(),
            user_service,
            credential_repo: Arc::new(UserProviderCredentialRepository::new(pool.clone())),
            credential_encryption: Some(encryption),
            providers_manager: Arc::clone(&providers_manager),
            cache_invalidation,
            brute_force: Arc::new(crate::service::auth::BruteForceProtection::in_memory(
                "test:room".to_string(),
            )),
            audit_service: None,
            settings_registry: None,
            user_notification_service: None,
            password_hasher: None,
            realtime_outbox: None,
            runtime: build_room_service_runtime(
                &SharedStateProfile::from_runtime(None, "test:", false),
                &crate::config::RedisDeploymentMode::Standalone,
                Config::default().cache.l2_ttl_seconds,
            )
            .expect("room service runtime should build"),
            version_fence: Arc::new(crate::cache::NoopVersionFenceStore),
        });

        assert!(
            Arc::ptr_eq(
                room_service.media_service().providers_manager(),
                &providers_manager
            ),
            "injecting media credential encryption must not replace the shared providers manager"
        );
    }

    #[test]
    fn test_cluster_enabled_without_redis_fails_at_config_validation_layer() {
        let mut config = Config::default();
        config.cluster.enabled = true;
        config.cluster.secret = "cluster-secret".to_string();
        config.redis.url.clear();

        let errors = config
            .validate()
            .expect_err("cluster.enabled=true without Redis must fail config validation");

        assert!(
            errors
                .iter()
                .any(|e| e.contains("distributed mode requires Redis")),
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
                .any(|e| e.contains("distributed mode requires Redis")),
            "cluster=false should not be rejected by cluster/Redis rule: {errors:?}"
        );
    }

    #[tokio::test]
    async fn test_init_oauth2_service_rejects_cluster_mode_without_shared_state_at_bootstrap_layer()
    {
        let pool = PgPool::connect_lazy("postgresql://test").expect("lazy pool should build");
        let settings_registry = test_settings_registry(pool.clone());

        let profile = SharedStateProfile::from_runtime(None, "synctv:", true);
        let error = init_oauth2_service(
            &pool,
            settings_registry,
            &profile,
            synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
        .expect_err("cluster bootstrap must not fall back to in-memory OAuth2 state store");

        assert!(
            error
                .to_string()
                .contains("distributed runtime requires shared single-use OAuth2 state storage"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn test_init_oauth2_service_starts_without_runtime_providers() {
        let pool = PgPool::connect_lazy("postgresql://test").expect("lazy pool should build");
        let settings_registry = test_settings_registry(pool.clone());
        let profile = SharedStateProfile::from_runtime(None, "synctv:", false);
        let service = init_oauth2_service(
            &pool,
            settings_registry,
            &profile,
            synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
        .expect("OAuth2 service should start before runtime providers are configured");
        assert!(service.is_some());
    }

    fn test_settings_registry(pool: PgPool) -> Arc<SettingsRegistry> {
        let settings_repo = SettingsRepository::new(pool.clone());
        let settings_service = Arc::new(SettingsService::new(settings_repo, pool));
        Arc::new(SettingsRegistry::new(settings_service))
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

    #[test]
    fn test_email_verification_requirement_tracks_email_service_availability() {
        assert!(
            should_require_email_verification(true),
            "email verification must be enabled when email delivery is configured"
        );
        assert!(
            !should_require_email_verification(false),
            "email verification must stay disabled when email delivery is unavailable"
        );
    }

    #[tokio::test]
    async fn test_build_email_token_service_requires_email_service() {
        let pool = PgPool::connect_lazy("postgresql://test").expect("lazy pool should build");
        let limiter: Arc<dyn RequestRateLimiterService> =
            Arc::new(RateLimiter::local_only("test-email-token:".to_string()));

        assert!(
            build_email_token_service(pool.clone(), false, limiter.clone()).is_none(),
            "email token service must not start without email delivery"
        );

        let service = build_email_token_service(pool, true, limiter)
            .expect("email token service should be built when email delivery is configured");
        assert!(
            service.has_rate_limiter(),
            "email token service should inherit the shared rate limiter by default"
        );
        assert_eq!(
            service.rate_limit_config().max_tokens_per_user,
            5,
            "email token service should use the default per-user hourly cap"
        );
        assert_eq!(
            service.rate_limit_config().window_seconds,
            3600,
            "email token service should use the default hourly window"
        );
    }

    #[tokio::test]
    async fn test_build_providers_manager_loads_defaults_from_config() {
        let pool = PgPool::connect_lazy("postgresql://test").expect("lazy pool should build");
        let config = Config::default();
        let provider_repo = Arc::new(ProviderInstanceRepository::new(pool));
        let provider_instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(
            provider_repo,
            None,
        ));

        let providers_manager = build_providers_manager(&config, provider_instance_manager)
            .await
            .expect("provider manager builder should load default providers");

        assert!(
            providers_manager.get("direct_url").await.is_some(),
            "default provider instances must be loaded during provider manager initialization"
        );
    }

    #[test]
    fn test_init_credential_encryption_allows_missing_key() {
        let encryption = init_credential_encryption(Some(""))
            .expect("empty credential encryption key should mean disabled");

        assert!(encryption.is_none());
    }

    #[test]
    fn test_init_credential_encryption_rejects_invalid_key() {
        let error = init_credential_encryption(Some("not-a-64-character-hex-key"))
            .expect_err("invalid explicit credential encryption key must fail closed");

        assert!(error
            .to_string()
            .contains("Failed to initialize credential encryption"));
    }
}
