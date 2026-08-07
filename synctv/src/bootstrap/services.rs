//! Service initialization and dependency injection

use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use sqlx::PgPool;
use tracing::{info, warn};

use super::redis::RedisConnectionOptions;
use synctv_core::{
    cache::{
        build_l2_cache_backend, version_fence_store_from_shared_state_profile,
        CacheInvalidationRuntime, CacheL2Backend, CacheManager, ConsistencyCoordinator, KeyBuilder,
        RoomCache, UserCache, UsernameCache, VersionFenceStore,
    },
    provider::{
        AlistProvider, CachedProviderAccessService, ProviderAccessService, ProviderStoreRegistry,
        ProviderStoreResolver,
    },
    repository::{
        realtime_outbox::RealtimeOutboxRepository, ChatRepository, FileStorageRepository,
        NotificationRepository, ProviderInstanceRepository, RoomMemberRepository, RoomRepository,
        RoomSettingsRepository as RoomSettingsRepo, SettingsRepository,
        UserOAuthProviderRepository, UserProviderCredentialRepository,
        WebAuthnCredentialRepository,
    },
    service::{
        AuditFlushHandle, AuditService, ChatService, ContentFilter,
        DatabaseFileStorageCompressionConfig, DatabaseFileStorageService,
        DisabledFileStorageService, EmailOutboxService, EmailService, EmailTokenRateLimitConfig,
        EmailTokenService, FileStorageBackendRegistry, FileStorageService, JwtService,
        MediaProvidersOptions, NotificationService as RoomNotificationService, OAuth2Service,
        OAuth2ServiceRuntime, PasskeyService, PasskeyServiceOptions, PermissionService,
        PgTokenBlacklistStore, ProvidersManager, RateLimitConfig, RemoteProviderManager,
        RemoteProviderManagerOptions, RequestRateLimiterService, RoomService, RoomSettingsRuntime,
        RoomSettingsService, RuntimeEmailConfigProvider, RuntimeSettingsStore,
        S3CompatibleFileStorageService, S3FileStorageConfig, SettingsService,
        SettingsServiceRuntime, StreamingPublishKeyService, TieredTokenBlacklistStore,
        UserNotificationService, UserService,
    },
    validation::PasswordComplexityOptions,
    RedisDeploymentMode, SharedStateMode, SharedStateProfile,
};

#[cfg(test)]
use synctv_core::ManagedRedisRuntime;

const WEAK_JWT_SECRETS: &[&str] = &[
    "change-me-in-production",
    "secret",
    "password",
    "jwt-secret",
    "changeme",
    "test",
    "default",
];

#[derive(Debug, Clone, Default)]
pub struct SsrfOptions {
    pub enabled: bool,
    pub allow_private_network_targets: bool,
    pub allowed_hosts: Vec<String>,
    pub allowed_ip_ranges: Vec<String>,
}

#[derive(Clone, Default)]
pub struct SecurityOptions {
    pub email_outbox_encryption_key: String,
    pub opaque_server_setup_secret: String,
    pub login_discovery_key: String,
    pub ssrf: SsrfOptions,
}

impl std::fmt::Debug for SecurityOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecurityOptions")
            .field("email_outbox_encryption_key", &"<redacted>")
            .field("opaque_server_setup_secret", &"<redacted>")
            .field("login_discovery_key", &"<redacted>")
            .field("ssrf", &self.ssrf)
            .finish()
    }
}

impl SecurityOptions {
    #[must_use]
    pub fn ssrf_guard(&self) -> synctv_common::ssrf::SsrfGuard {
        if !self.ssrf.enabled {
            tracing::warn!(
                "SSRF protection is disabled by configuration. Proxy routes will forward \
                 requests to any host including private network addresses. \
                 Do not use this setting in production."
            );
            return synctv_common::ssrf::SsrfGuard::disabled();
        }

        let mut builder = synctv_common::ssrf::SsrfGuard::builder();
        if self.ssrf.allow_private_network_targets {
            builder = builder.allow_private_network_targets(true);
        }
        for host in &self.ssrf.allowed_hosts {
            builder = builder.extra_allowed_host(host.clone());
        }
        for range in &self.ssrf.allowed_ip_ranges {
            if let Ok(range) = range.parse() {
                builder = builder.extra_allowed_ip_range(range);
            }
        }
        builder.build()
    }
}

#[derive(Clone)]
pub struct JwtOptions {
    pub secret: String,
    pub access_token_duration_hours: u64,
    pub refresh_token_duration_days: u64,
    pub guest_token_duration_hours: u64,
    pub clock_skew_leeway_secs: u64,
}

impl std::fmt::Debug for JwtOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwtOptions")
            .field("secret", &"<redacted>")
            .field(
                "access_token_duration_hours",
                &self.access_token_duration_hours,
            )
            .field(
                "refresh_token_duration_days",
                &self.refresh_token_duration_days,
            )
            .field(
                "guest_token_duration_hours",
                &self.guest_token_duration_hours,
            )
            .field("clock_skew_leeway_secs", &self.clock_skew_leeway_secs)
            .finish()
    }
}

impl Default for JwtOptions {
    fn default() -> Self {
        Self {
            secret: "change-me-in-production".to_string(),
            access_token_duration_hours: 1,
            refresh_token_duration_days: 30,
            guest_token_duration_hours: 4,
            clock_skew_leeway_secs: 60,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CacheOptions {
    pub l1_capacity: u64,
    pub l1_ttl_seconds: u64,
    pub l2_ttl_seconds: u64,
    pub username_cache_capacity: u64,
    pub username_cache_ttl_seconds: u64,
}

impl Default for CacheOptions {
    fn default() -> Self {
        Self {
            l1_capacity: 5000,
            l1_ttl_seconds: 300,
            l2_ttl_seconds: 300,
            username_cache_capacity: 10_000,
            username_cache_ttl_seconds: 3600,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MessagingRateLimitOptions {
    pub chat_per_second: u32,
    pub window_seconds: u64,
}

impl Default for MessagingRateLimitOptions {
    fn default() -> Self {
        Self {
            chat_per_second: 10,
            window_seconds: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FileStorageDatabaseCompressionOption {
    None,
    Lz4,
    #[default]
    Zstd,
}

impl From<FileStorageDatabaseCompressionOption> for synctv_core::models::FileBlobCompression {
    fn from(value: FileStorageDatabaseCompressionOption) -> Self {
        match value {
            FileStorageDatabaseCompressionOption::None => Self::None,
            FileStorageDatabaseCompressionOption::Lz4 => Self::Lz4,
            FileStorageDatabaseCompressionOption::Zstd => Self::Zstd,
        }
    }
}

#[derive(Clone, Debug)]
pub struct FileStorageDatabaseOptions {
    pub compression: FileStorageDatabaseCompressionOption,
    pub compression_min_size_bytes: i64,
    pub compression_min_savings_percent: u8,
}

impl Default for FileStorageDatabaseOptions {
    fn default() -> Self {
        Self {
            compression: FileStorageDatabaseCompressionOption::Zstd,
            compression_min_size_bytes: 4096,
            compression_min_savings_percent: 10,
        }
    }
}

#[derive(Clone)]
pub struct FileStorageS3Options {
    pub endpoint: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub bucket: String,
    pub region: String,
    pub base_path: String,
    pub public_base_url: Option<String>,
    pub upload_expires_seconds: i64,
}

impl Default for FileStorageS3Options {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            bucket: String::new(),
            region: "auto".to_string(),
            base_path: "files/".to_string(),
            public_base_url: None,
            upload_expires_seconds: 900,
        }
    }
}

impl std::fmt::Debug for FileStorageS3Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileStorageS3Options")
            .field("endpoint", &self.endpoint)
            .field("access_key_id", &"<redacted>")
            .field("secret_access_key", &"<redacted>")
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("base_path", &self.base_path)
            .field("public_base_url", &self.public_base_url)
            .field("upload_expires_seconds", &self.upload_expires_seconds)
            .finish()
    }
}

#[derive(Clone, Debug, Default)]
pub enum FileStorageBackendOptions {
    #[default]
    Disabled,
    Database(FileStorageDatabaseOptions),
    S3(FileStorageS3Options),
}

#[derive(Clone, Debug)]
pub struct FileStorageOptions {
    pub upload_token_secret: String,
    pub default_backend: String,
    pub chat_attachments_backend: String,
    pub user_avatars_backend: String,
    pub media_covers_backend: String,
    pub room_covers_backend: String,
    pub playlist_covers_backend: String,
    pub backends: HashMap<String, FileStorageBackendOptions>,
}

impl Default for FileStorageOptions {
    fn default() -> Self {
        Self {
            upload_token_secret: String::new(),
            default_backend: "disabled".to_string(),
            chat_attachments_backend: String::new(),
            user_avatars_backend: String::new(),
            media_covers_backend: String::new(),
            room_covers_backend: String::new(),
            playlist_covers_backend: String::new(),
            backends: HashMap::new(),
        }
    }
}

impl FileStorageOptions {
    #[must_use]
    pub fn backend_for_chat_attachments(&self) -> &str {
        self.selected_backend_or_default(&self.chat_attachments_backend)
    }

    #[must_use]
    pub fn backend_for_user_avatars(&self) -> &str {
        self.selected_backend_or_default(&self.user_avatars_backend)
    }

    #[must_use]
    pub fn backend_for_media_covers(&self) -> &str {
        self.selected_backend_or_default(&self.media_covers_backend)
    }

    #[must_use]
    pub fn backend_for_room_covers(&self) -> &str {
        self.selected_backend_or_default(&self.room_covers_backend)
    }

    #[must_use]
    pub fn backend_for_playlist_covers(&self) -> &str {
        self.selected_backend_or_default(&self.playlist_covers_backend)
    }

    fn selected_backend_or_default<'a>(&'a self, selected: &'a str) -> &'a str {
        let selected = selected.trim();
        if selected.is_empty() {
            self.default_backend.trim()
        } else {
            selected
        }
    }
}

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
    /// Content filter for chat
    pub content_filter: ContentFilter,
    /// Provider instance manager
    pub provider_instance_manager: Arc<RemoteProviderManager>,
    /// User provider credential repository
    pub user_provider_credential_repo: Arc<UserProviderCredentialRepository>,
    /// Shared provider cache/lock stores.
    pub provider_stores: Arc<dyn ProviderStoreResolver>,
    /// Providers manager
    pub providers_manager: Arc<ProvidersManager>,
    /// `OAuth2` service (optional, requires configuration)
    pub oauth2_service: Option<Arc<OAuth2Service>>,
    /// Passkey/WebAuthn service (optional, requires configuration)
    pub passkey_service: Option<Arc<PasskeyService>>,
    /// Settings service
    pub settings_service: Arc<SettingsService>,
    /// Runtime settings store with type-safe setting variables
    pub runtime_settings_store: Arc<RuntimeSettingsStore>,
    /// Email service backed by runtime SMTP settings.
    pub email_service: Option<Arc<EmailService>>,
    /// Email token service for bind, login, and password reset codes.
    pub email_token_service: Option<Arc<EmailTokenService>>,
    /// Durable email delivery outbox shared by API writers and cluster workers.
    pub email_outbox_service: Arc<EmailOutboxService>,
    /// Shared WebSocket ticket service reused across transports.
    pub ws_ticket_service: Arc<dyn synctv_core::service::WebSocketTicketService>,
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
    /// Shared user cache for fast-path auth checks and hot user lookups.
    pub user_cache: Arc<UserCache>,
    /// Shared runtime for Redis-backed shared-state features.
    pub redis_runtime: Option<Arc<dyn synctv_core::RedisConnectionRuntime>>,
    /// `CancellationToken` for settings listen task (cancel on shutdown)
    pub settings_cancel: tokio_util::sync::CancellationToken,
    /// Settings listen task handle (joined on shutdown).
    /// Wrapped in `Arc<Mutex<Option<...>>>` so `Services` remains `Clone`.
    /// Take the handle out of the `Option` to join it on shutdown.
    pub settings_listen_task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
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
    pub credential_encryption: Option<synctv_core::credential_encryption::CredentialEncryption>,
}

#[derive(Clone)]
pub struct CoreServicesOptions {
    pub security: SecurityOptions,
    pub media_providers: MediaProvidersOptions,
    pub cluster_runtime_enabled: bool,
    pub redis: RedisConnectionOptions,
    pub cache: CacheOptions,
    pub messaging_rate_limits: MessagingRateLimitOptions,
    pub transport_compression_enabled: bool,
    pub file_storage: FileStorageOptions,
    pub jwt: JwtOptions,
    pub password_complexity: PasswordComplexityOptions,
    pub passkey: PasskeyServiceOptions,
}

impl Default for CoreServicesOptions {
    fn default() -> Self {
        Self {
            security: SecurityOptions::default(),
            media_providers: MediaProvidersOptions::default(),
            cluster_runtime_enabled: false,
            redis: RedisConnectionOptions::default(),
            cache: CacheOptions::default(),
            messaging_rate_limits: MessagingRateLimitOptions::default(),
            transport_compression_enabled: true,
            file_storage: FileStorageOptions::default(),
            jwt: JwtOptions::default(),
            password_complexity: PasswordComplexityOptions::default(),
            passkey: PasskeyServiceOptions::default(),
        }
    }
}

#[derive(Clone)]
pub struct InitServicesOptions {
    pub clock: Arc<dyn synctv_core::Clock>,
    pub provider_address_overrides: HashMap<String, SocketAddr>,
    pub ssrf_guard: synctv_common::ssrf::SsrfGuard,
    pub credential_encryption_key: Option<String>,
    pub totp_encryption_key: Option<String>,
    pub realtime_outbox: Option<Arc<RealtimeOutboxRepository>>,
    pub read_pool: Option<PgPool>,
}

impl Default for InitServicesOptions {
    fn default() -> Self {
        Self {
            clock: Arc::new(synctv_core::SystemClock),
            provider_address_overrides: HashMap::new(),
            ssrf_guard: synctv_common::ssrf::SsrfGuard::strict_policy(),
            credential_encryption_key: None,
            totp_encryption_key: None,
            realtime_outbox: None,
            read_pool: None,
        }
    }
}

impl std::fmt::Debug for InitServicesOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InitServicesOptions")
            .field("clock", &"Clock")
            .field(
                "provider_address_overrides",
                &self.provider_address_overrides,
            )
            .field("ssrf_enabled", &self.ssrf_guard.acl().is_some())
            .field(
                "credential_encryption_key",
                &self
                    .credential_encryption_key
                    .as_ref()
                    .map(|_| "<redacted>"),
            )
            .field(
                "totp_encryption_key",
                &self.totp_encryption_key.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "realtime_outbox",
                &self.realtime_outbox.as_ref().map(|_| "<injected>"),
            )
            .field("read_pool", &self.read_pool.as_ref().map(|_| "<injected>"))
            .finish()
    }
}

impl Services {
    #[must_use]
    pub fn redis_runtime(&self) -> Option<Arc<dyn synctv_core::RedisConnectionRuntime>> {
        self.redis_runtime.clone()
    }
}

fn build_email_token_service(
    pool: PgPool,
    rate_limiter: Arc<dyn RequestRateLimiterService>,
    clock: Arc<dyn synctv_core::Clock>,
) -> Arc<EmailTokenService> {
    Arc::new(EmailTokenService::new_with_runtime(
        pool,
        rate_limiter,
        Some(EmailTokenRateLimitConfig::default()),
        clock,
    ))
}

fn build_brute_force_protection(
    profile: &SharedStateProfile,
) -> Result<Arc<dyn synctv_core::service::BruteForceProtectionService>, anyhow::Error> {
    let service: Arc<dyn synctv_core::service::BruteForceProtectionService> = Arc::new(
        synctv_core::service::BruteForceProtection::from_shared_state_profile(profile)
            .map_err(anyhow::Error::from)?,
    );
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
    Ok(Arc::new(
        synctv_core::service::RateLimiter::from_shared_state_profile(profile)
            .map_err(anyhow::Error::from)?,
    ))
}

fn build_refresh_token_rate_limiter(
    profile: &SharedStateProfile,
) -> Result<Arc<dyn RequestRateLimiterService>, anyhow::Error> {
    let rate_limit_prefix = KeyBuilder::new(profile.key_prefix()).namespace_prefix("refresh_rl");
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
) -> Result<Arc<dyn synctv_core::service::WebSocketTicketService>, anyhow::Error> {
    Ok(Arc::new(
        synctv_core::service::WsTicketService::from_shared_state_profile(profile, None)
            .map_err(anyhow::Error::from)?,
    ))
}

fn handle_provider_invalidation_listener_result(
    start_result: synctv_core::Result<()>,
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
    init_result: synctv_core::Result<()>,
) -> Result<(), anyhow::Error> {
    init_result.map_err(|e| anyhow::anyhow!("RemoteProviderManager initialization failed: {e}"))
}

async fn build_providers_manager(
    options: &CoreServicesOptions,
    provider_instance_manager: Arc<RemoteProviderManager>,
) -> Result<Arc<ProvidersManager>, anyhow::Error> {
    let providers_manager = ProvidersManager::new_with_ssrf_guard(
        provider_instance_manager,
        options.security.ssrf_guard(),
    )
    .map_err(|e| anyhow::anyhow!("Failed to build provider HTTP client: {e}"))?;
    let default_provider_count = providers_manager
        .create_builtin_defaults_with_options(&options.media_providers)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create default media providers: {e}"))?;

    info!(
        "ProvidersManager initialized {} local provider adapter(s)",
        default_provider_count
    );

    Ok(Arc::new(providers_manager))
}

pub async fn init_services_with_options(
    pool: PgPool,
    service_options: &CoreServicesOptions,
    shared_runtime: Option<Arc<dyn synctv_core::RedisConnectionRuntime>>,
    cache_invalidation: Arc<dyn CacheInvalidationRuntime>,
    cache_invalidation_listener_task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    runtime_options: InitServicesOptions,
) -> Result<Services, anyhow::Error> {
    info!("Initializing services...");
    let read_pool = runtime_options
        .read_pool
        .clone()
        .unwrap_or_else(|| pool.clone());

    let cluster_mode = service_options.cluster_runtime_enabled;

    // Initialize JWT service
    info!("Loading JWT keys...");
    let jwt_service = load_jwt_service(&service_options.jwt, runtime_options.clock.clone())?;
    info!("JWT service initialized");

    let shared_state_profile = SharedStateProfile::for_cluster_runtime(
        shared_runtime.clone(),
        &service_options.redis.key_prefix,
        cluster_mode,
    );
    let key_builder = KeyBuilder::new(service_options.redis.key_prefix.clone());
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
        build_l2_cache_backend(shared_state_profile.shared_runtime());
    info!(
        l2_cache_enabled = cache_l2.is_active(),
        "Cache L2 initialized"
    );

    // Initialize username cache (using config values)
    let username_cache_capacity = usize::try_from(service_options.cache.username_cache_capacity)
        .map_err(|_| {
            anyhow::anyhow!(
                "cache.username_cache_capacity={} exceeds platform usize::MAX",
                service_options.cache.username_cache_capacity
            )
        })?;
    let username_cache = UsernameCache::new_with_invalidation(
        cache_l2.clone(),
        key_builder.namespace_prefix("username"),
        username_cache_capacity,
        service_options.cache.username_cache_ttl_seconds,
        Some(cache_invalidation.clone()),
    );
    info!(
        "Username cache initialized (capacity={}, ttl={}s)",
        service_options.cache.username_cache_capacity,
        service_options.cache.username_cache_ttl_seconds
    );

    // Initialize user and room L1/L2 caches (using config values)
    let user_cache = Arc::new(UserCache::new(
        cache_l2.clone(),
        service_options.cache.l1_capacity,
        service_options.cache.l1_ttl_seconds,
        service_options.cache.l2_ttl_seconds,
        key_builder.namespace_prefix("user"),
    ));
    let room_cache = Arc::new(RoomCache::new(
        cache_l2.clone(),
        service_options.cache.l1_capacity,
        service_options.cache.l1_ttl_seconds,
        service_options.cache.l2_ttl_seconds,
        key_builder.namespace_prefix("room"),
    ));
    info!(
        "User and room caches initialized (l1_capacity={}, l1_ttl={}s, l2_ttl={}s)",
        service_options.cache.l1_capacity,
        service_options.cache.l1_ttl_seconds,
        service_options.cache.l2_ttl_seconds
    );

    // Initialize brute-force protection
    let brute_force = build_brute_force_protection(&shared_state_profile)?;

    // Initialize token blacklist store (tiered: L1 moka + optional L2 Redis + PG primary)
    let token_blacklist: Arc<dyn synctv_core::service::TokenBlacklistStore> =
        Arc::new(TieredTokenBlacklistStore::from_shared_state_profile(
            PgTokenBlacklistStore::new(pool.clone()),
            &shared_state_profile,
        ));
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
    // Shared-state refresh limiting prevents N * limit bypass across replicas.
    let refresh_rate_limiter = build_refresh_token_rate_limiter(&shared_state_profile)?;

    // Initialize credential encryption (shared by both repositories and media providers)
    let credential_encryption = init_credential_encryption(
        runtime_options.credential_encryption_key.as_deref(),
        "provider-data",
    )?;
    let totp_encryption = init_credential_encryption(
        runtime_options.totp_encryption_key.as_deref(),
        "totp-secret",
    )?;
    // Keep a clone for provider credential resolution during media playback.
    let credential_encryption_for_services = credential_encryption.clone();

    // Initialize ProviderInstanceRepository (with optional encryption for jwt_secret/custom_ca)
    let provider_instance_repo = match &credential_encryption {
        Some(enc) => {
            info!("ProviderInstanceRepository initialized with encryption enabled");
            Arc::new(
                ProviderInstanceRepository::new_with_encryption_and_read_pool(
                    pool.clone(),
                    read_pool.clone(),
                    enc.clone(),
                ),
            )
        }
        None => Arc::new(ProviderInstanceRepository::new_with_read_pool(
            pool.clone(),
            read_pool.clone(),
        )),
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
            "Credential encryption key not provided. \
             Existing encrypted credentials remain readable only when the key is configured, \
             and creating/updating provider credentials will be rejected."
        );
        Arc::new(UserProviderCredentialRepository::new(pool.clone()))
    };

    // Initialize rate limiter
    let rate_limiter = build_request_rate_limiter(&shared_state_profile)?;
    let rate_limit_config = RateLimitConfig {
        chat_per_second: service_options.messaging_rate_limits.chat_per_second,
        window_seconds: service_options.messaging_rate_limits.window_seconds,
    };
    info!(
        "Rate limiter initialized (chat: {}/s)",
        rate_limit_config.chat_per_second
    );

    // Initialize content filter
    let content_filter = ContentFilter::new();
    info!(
        "Content filter initialized (max chat: {} chars)",
        content_filter.max_chat_length
    );

    // Initialize RemoteProviderManager (with Redis for cross-replica cache invalidation when available)
    info!("Initializing RemoteProviderManager...");
    let provider_instance_manager = Arc::new(RemoteProviderManager::new_with_options(
        provider_instance_repo.clone(),
        Some(cache_invalidation.clone()),
        RemoteProviderManagerOptions {
            address_overrides: runtime_options.provider_address_overrides,
            ssrf_guard: runtime_options.ssrf_guard.clone(),
            transport_compression_enabled: service_options.transport_compression_enabled,
        },
    ));

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
        build_providers_manager(service_options, provider_instance_manager.clone()).await?;
    info!("ProvidersManager initialized");
    let provider_access_http_client =
        synctv_media_providers::build_provider_http_client(service_options.security.ssrf_guard())
            .map_err(|error| anyhow::anyhow!("Failed to build provider HTTP client: {error}"))?;
    let provider_stores: Arc<dyn ProviderStoreResolver> =
        Arc::new(ProviderStoreRegistry::from_runtime(
            shared_state_profile.shared_runtime(),
            shared_state_profile.key_prefix().to_string(),
        ));
    let provider_access_service: Arc<dyn ProviderAccessService> = Arc::new(
        CachedProviderAccessService::new(
            user_provider_credential_repo.clone(),
            Arc::new(AlistProvider::with_client_manager(
                provider_instance_manager.clone(),
                Arc::new(
                    synctv_core::provider::ProviderClientManager::new_with_provider_http_client(
                        provider_access_http_client,
                    ),
                ),
            )),
        )
        .with_store(provider_stores.load("credentials"))
        .with_credential_encryption(credential_encryption_for_services.clone()),
    );

    // Prepare RoomService runtime dependencies after ProvidersManager so
    // media/playback paths use the same provider graph and HTTP client
    // configuration as bootstrap. The RoomService itself is constructed later,
    // once all construction-time collaborators are available.
    let room_runtime = build_room_service_runtime(
        &shared_state_profile,
        &service_options.redis.deployment_mode,
        service_options.cache.l2_ttl_seconds,
    )?;
    let room_settings_l2_cache_for_chat = room_runtime.room_settings_l2_cache.clone();

    // Initialize CacheManager and start cross-replica invalidation listener
    let cache_manager = CacheManager::new(
        user_cache.clone(),
        room_cache.clone(),
        Some(Arc::new(username_cache.clone())),
    );
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
        SettingsServiceRuntime {
            version_fence: version_fence.clone(),
            l2_cache: cache_l2.clone(),
            cache_key_prefix: key_builder.namespace_prefix("runtime_settings"),
            cache_max_capacity: 512,
            cache_ttl_secs: 300,
            cache_l2_ttl_secs: service_options.cache.l2_ttl_seconds,
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

    // Initialize Runtime settings store
    info!("Initializing Runtime settings store...");
    let runtime_settings_store = RuntimeSettingsStore::new_with_ssrf_guard(
        settings_service.clone(),
        &service_options.security.ssrf_guard(),
    );
    runtime_settings_store.init(settings_cancel.clone())?;
    info!("Runtime settings store initialized");
    let runtime_settings_store = Arc::new(runtime_settings_store);

    // Initialize Email service. SMTP connection details live in runtime settings
    // and are read lazily when mail is sent.
    let email_config_provider = RuntimeEmailConfigProvider::new(&runtime_settings_store);
    let email_service = Some(Arc::new(EmailService::new(Arc::new(
        email_config_provider,
    ))?));
    info!("Email service initialized with runtime settings");

    // Initialize Email Token service. Delivery is gated by EmailService runtime settings when mail is sent.
    let email_token_service = Some(build_email_token_service(
        pool.clone(),
        rate_limiter.clone(),
        runtime_options.clock.clone(),
    ));
    info!("Email token service initialized");
    let email_outbox_service = Arc::new(EmailOutboxService::new(
        pool.clone(),
        &service_options.security.email_outbox_encryption_key,
    )?);
    info!("Durable email outbox initialized");

    let ws_ticket_service = build_ws_ticket_service(&shared_state_profile)?;
    info!(
        cross_node_capable = ws_ticket_service.supports_cluster_runtime(),
        "WebSocket ticket service initialized"
    );

    // Initialize Publish Key service (for RTMP streaming)
    // Use Redis-backed JTI dedup when available (shared handle follows Sentinel failover).
    // Falls back to in-memory for standalone mode.
    let publish_key_service = build_publish_key_service(
        jwt_service.clone(),
        runtime_options.clock.clone(),
        &shared_state_profile,
    )?;

    // Initialize User Notification service
    let notification_repo = NotificationRepository::new(pool.clone());
    let notification_service = UserNotificationService::new(notification_repo);
    info!("User notification service initialized");

    // Initialize Audit service with buffering
    let (audit_service, audit_flush_handle) = AuditService::new(pool.clone());
    let audit_service = Arc::new(audit_service);
    info!("Audit service initialized with async buffering");

    let notification_service = Arc::new(notification_service);

    let file_storage_repo = Arc::new(FileStorageRepository::new(pool.clone()));
    let file_upload_token_secret = service_options.file_storage.upload_token_secret.clone();
    let mut file_storage_backends: HashMap<String, Arc<dyn FileStorageService>> = HashMap::new();
    file_storage_backends.insert("disabled".to_string(), Arc::new(DisabledFileStorageService));
    for (name, backend_config) in &service_options.file_storage.backends {
        let service: Arc<dyn FileStorageService> = match backend_config {
            FileStorageBackendOptions::Disabled => Arc::new(DisabledFileStorageService),
            FileStorageBackendOptions::Database(database) => {
                Arc::new(DatabaseFileStorageService::new_with_compression_config(
                    name.clone(),
                    file_storage_repo.clone(),
                    file_upload_token_secret.clone(),
                    DatabaseFileStorageCompressionConfig {
                        algorithm: database.compression.into(),
                        min_size_bytes: database.compression_min_size_bytes,
                        min_savings_percent: database.compression_min_savings_percent,
                    },
                ))
            }
            FileStorageBackendOptions::S3(s3) => {
                let file_storage = S3CompatibleFileStorageService::new_with_repository(
                    S3FileStorageConfig {
                        endpoint: s3.endpoint.clone(),
                        access_key_id: s3.access_key_id.clone(),
                        secret_access_key: s3.secret_access_key.clone(),
                        bucket: s3.bucket.clone(),
                        region: s3.region.clone(),
                        base_path: s3.base_path.clone(),
                        public_base_url: s3.public_base_url.clone(),
                        upload_expires_seconds: s3.upload_expires_seconds,
                        storage_backend: name.clone(),
                        upload_token_secret: file_upload_token_secret.clone(),
                    },
                    Some(file_storage_repo.clone()),
                )
                .map_err(|error| {
                    anyhow::anyhow!("failed to initialize file storage backend '{name}': {error}")
                })?;
                Arc::new(file_storage)
            }
        };
        file_storage_backends.insert(name.clone(), service);
    }
    let file_storage_registry = FileStorageBackendRegistry::new(file_storage_backends);
    let user_avatar_file_storage: Arc<dyn FileStorageService> = Arc::new(
        file_storage_registry
            .routed(
                service_options
                    .file_storage
                    .backend_for_user_avatars()
                    .to_string(),
            )
            .map_err(|error| anyhow::anyhow!("failed to route user avatar storage: {error}"))?,
    );
    let media_cover_file_storage: Arc<dyn FileStorageService> = Arc::new(
        file_storage_registry
            .routed(
                service_options
                    .file_storage
                    .backend_for_media_covers()
                    .to_string(),
            )
            .map_err(|error| anyhow::anyhow!("failed to route media cover storage: {error}"))?,
    );
    let room_cover_file_storage: Arc<dyn FileStorageService> = Arc::new(
        file_storage_registry
            .routed(
                service_options
                    .file_storage
                    .backend_for_room_covers()
                    .to_string(),
            )
            .map_err(|error| anyhow::anyhow!("failed to route room cover storage: {error}"))?,
    );
    let playlist_cover_file_storage: Arc<dyn FileStorageService> = Arc::new(
        file_storage_registry
            .routed(
                service_options
                    .file_storage
                    .backend_for_playlist_covers()
                    .to_string(),
            )
            .map_err(|error| anyhow::anyhow!("failed to route playlist cover storage: {error}"))?,
    );

    let opaque_password_service = Arc::new(
        synctv_core::service::OpaquePasswordService::derive_from_secret(
            service_options
                .security
                .opaque_server_setup_secret
                .as_bytes(),
        ),
    );

    let user_permission_service = PermissionService::new_with_runtime(
        RoomMemberRepository::new(pool.clone()),
        RoomRepository::new(pool.clone()),
        synctv_core::service::PermissionServiceRuntime {
            runtime_settings_store: Some(Arc::clone(&runtime_settings_store)),
            cache_size: PermissionService::DEFAULT_CACHE_SIZE,
            cache_ttl_secs: PermissionService::DEFAULT_CACHE_TTL_SECS,
            room_settings_repo: Some(RoomSettingsRepo::new(pool.clone())),
            invalidation_service: Some(cache_invalidation.clone()),
            version_fence: version_fence.clone(),
            member_permission_l2_cache: cache_l2.clone(),
            member_permission_cache_key_prefix: key_builder.namespace_prefix("member_permission"),
            room_settings_l2_cache: room_settings_l2_cache_for_chat.clone(),
            room_settings_cache_key_prefix: key_builder.namespace_prefix("room_settings"),
        },
    )?;

    let user_service = Arc::new(UserService::new_with_brute_force_service_and_runtime(
        &pool,
        synctv_core::service::UserServiceDependencies {
            jwt_service: jwt_service.clone(),
            username_cache: username_cache.clone(),
            token_blacklist,
            key_builder: key_builder.clone(),
            brute_force: brute_force.clone(),
            password_complexity: service_options.password_complexity.clone(),
        },
        synctv_core::service::UserServiceRuntimeOptions {
            cache_invalidation: Some(cache_invalidation.clone()),
            refresh_rate_limiter,
            refresh_rate_limit_config: synctv_core::service::RefreshRateLimitConfig::default(),
            runtime_settings_store: Some(Arc::clone(&runtime_settings_store)),
            password_registration_policy_override: None,
            realtime_outbox: runtime_options.realtime_outbox.clone(),
            opaque_password_service: opaque_password_service.clone(),
            login_discovery_key:
                synctv_core::service::UserServiceRuntimeOptions::derive_login_discovery_key(
                    service_options.security.login_discovery_key.as_bytes(),
                ),
            login_session_store:
                synctv_core::service::login_session_store_from_shared_state_profile(
                    &shared_state_profile,
                )?,
            opaque_registration_session_store:
                synctv_core::service::opaque_registration_session_store_from_shared_state_profile(
                    &shared_state_profile,
                )?,
            mfa_session_store: synctv_core::service::mfa_session_store_from_shared_state_profile(
                &shared_state_profile,
            )?,
            sensitive_verification_session_store:
                synctv_core::service::sensitive_verification_session_store_from_shared_state_profile(
                    &shared_state_profile,
                )?,
            version_fence: version_fence.clone(),
            permission_service: Some(user_permission_service),
            file_storage_service: Some(user_avatar_file_storage),
            read_pool: Some(read_pool.clone()),
            credential_encryption: totp_encryption,
        },
    ));
    info!("UserService initialized with construction-time dependencies");

    let oauth2_service = init_oauth2_service(
        &pool,
        Arc::clone(&runtime_settings_store),
        Arc::clone(&user_service),
        &shared_state_profile,
        runtime_options.ssrf_guard.clone(),
    )?;
    info!("OAuth2 service initialized");

    let passkey_service = if service_options.passkey.enabled {
        let session_store = synctv_core::service::passkey_session_store_from_shared_state_profile(
            &shared_state_profile,
        )?;
        Some(Arc::new(PasskeyService::new(
            &service_options.passkey,
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
        clock: runtime_options.clock.clone(),
        pool: pool.clone(),
        read_pool: Some(read_pool.clone()),
        user_service: (*user_service).clone(),
        credential_repo: credential_encryption_for_services
            .as_ref()
            .map(|_| user_provider_credential_repo.clone()),
        credential_encryption: credential_encryption_for_services.clone(),
        providers_manager: providers_manager.clone(),
        provider_access_service: provider_access_service.clone(),
        cache_invalidation: cache_invalidation.clone(),
        brute_force: brute_force.clone(),
        audit_service: Some(Arc::clone(&audit_service)),
        runtime_settings_store: Arc::clone(&runtime_settings_store),
        user_notification_service: Some(Arc::clone(&notification_service)),
        opaque_password_service,
        room_opaque_password_registration_session_store:
            synctv_core::service::room_opaque_password_registration_session_store_from_shared_state_profile(
                &shared_state_profile,
            )?,
        room_opaque_password_login_session_store:
            synctv_core::service::room_opaque_password_login_session_store_from_shared_state_profile(
                &shared_state_profile,
            )?,
        realtime_outbox: runtime_options.realtime_outbox.clone(),
        media_file_storage_service: Some(media_cover_file_storage),
        room_file_storage_service: Some(room_cover_file_storage),
        playlist_file_storage_service: Some(playlist_cover_file_storage),
        provider_stores: provider_stores.clone(),
        runtime: room_runtime,
        version_fence: version_fence.clone(),
    })?;
    info!("RoomService initialized with construction-time dependencies");

    // Store the settings listen task handle so it can be joined on shutdown.
    // The task will be cancelled via settings_cancel.

    // Initialize ChatService with proper business logic (permissions, rate limiting, filtering)
    let chat_repo = Arc::new(ChatRepository::new_with_read_pool(
        pool.clone(),
        read_pool.clone(),
    ));
    let room_settings_repo_for_chat = RoomSettingsRepo::new(pool.clone());
    let room_notification_service = Arc::new(room_service.notification_service().clone());
    let room_settings_service_for_chat = RoomSettingsService::new_with_version_fence(
        room_settings_repo_for_chat,
        Some(cache_invalidation.clone()),
        room_notification_service.clone(),
        RoomSettingsRuntime {
            cache_ttl_secs: Some(300),
            cache_max_capacity: Some(10_000),
            version_fence,
            l2_cache: room_settings_l2_cache_for_chat,
            cache_key_prefix: key_builder.namespace_prefix("room_settings"),
        },
    );
    let permission_service_for_chat = room_service.permission_service().clone();
    let chat_file_storage = file_storage_registry
        .routed(
            service_options
                .file_storage
                .backend_for_chat_attachments()
                .to_string(),
        )
        .map_err(|error| anyhow::anyhow!("failed to route chat attachment storage: {error}"))?;
    let chat_service = ChatService::new(
        chat_repo.clone(),
        synctv_core::service::ChatRuntime {
            clock: runtime_options.clock.clone(),
            rate_limiter: rate_limiter.clone(),
            rate_limit_config: rate_limit_config.clone(),
            content_filter: content_filter.clone(),
        },
        synctv_core::service::ChatDependencies {
            permission_service: permission_service_for_chat,
            room_settings_service: room_settings_service_for_chat,
            user_service: user_service.clone(),
            file_storage_service: Arc::new(chat_file_storage),
            audit_service: Some(audit_service.clone()),
            notification_service: (*room_service.notification_service()).clone(),
            runtime_settings_store: Some(runtime_settings_store.clone()),
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
        user_provider_credential_repo,
        provider_stores,
        providers_manager,
        oauth2_service,
        passkey_service,
        settings_service,
        runtime_settings_store,
        email_service,
        email_token_service,
        email_outbox_service,
        ws_ticket_service,
        publish_key_service,
        notification_service,
        chat_service: Arc::new(chat_service),
        room_notification_service,
        audit_service,
        user_cache,
        redis_runtime: shared_runtime,
        settings_cancel,
        settings_listen_task: Arc::new(tokio::sync::Mutex::new(Some(settings_listen_task))),
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
    runtime_settings_store: Arc<RuntimeSettingsStore>,
    user_service: Arc<UserService>,
    profile: &SharedStateProfile,
    ssrf_guard: synctv_common::ssrf::SsrfGuard,
) -> Result<Option<Arc<OAuth2Service>>, anyhow::Error> {
    let provider_registry = synctv_core::oauth2::providers::provider_registry(ssrf_guard.clone());
    info!("OAuth2 provider registry initialized");

    let oauth2_repo = UserOAuthProviderRepository::new(pool.clone());
    let state_store = build_oauth_state_store(profile)?;
    let oauth2_service = OAuth2Service::new_with_runtime(
        oauth2_repo,
        state_store,
        provider_registry.clone(),
        ssrf_guard,
        matches!(profile.state_mode(), SharedStateMode::SharedRequired),
        OAuth2ServiceRuntime {
            runtime_settings_store: Some(runtime_settings_store),
            user_service: Some(user_service),
        },
    )
    .map_err(|e| anyhow::anyhow!("Failed to create OAuth2 service: {e}"))?;

    // OAuth2 state cleanup is handled automatically:
    // - Redis: SETEX TTL auto-expires entries
    // - In-memory: sweep_expired on each store/consume call

    Ok(Some(Arc::new(oauth2_service)))
}

fn build_oauth_state_store(
    profile: &SharedStateProfile,
) -> Result<Arc<dyn synctv_core::service::OAuthStateStore>, anyhow::Error> {
    let store = synctv_core::service::state_store_from_shared_state_profile(profile)
        .map_err(anyhow::Error::from)?;
    info!(
        cross_node_single_use = store.supports_cross_node_single_use(),
        "OAuth2 state store initialized"
    );
    Ok(store)
}

/// Load JWT service from explicit initialization parameters.
fn load_jwt_service(
    options: &JwtOptions,
    clock: Arc<dyn synctv_core::Clock>,
) -> Result<JwtService, anyhow::Error> {
    if options.secret.is_empty() {
        return Err(anyhow::anyhow!("JWT secret is empty"));
    }

    if WEAK_JWT_SECRETS.contains(&options.secret.as_str()) {
        warn!("Using a well-known JWT secret! This is insecure for production use.");
        warn!("Use a strong random JWT secret.");
    }

    JwtService::with_options(synctv_core::service::JwtServiceOptions {
        secret: options.secret.clone(),
        access_token_duration_hours: options.access_token_duration_hours,
        refresh_token_duration_days: options.refresh_token_duration_days,
        guest_token_duration_hours: options.guest_token_duration_hours,
        clock_skew_leeway_secs: options.clock_skew_leeway_secs,
        issuer: None,
        audience: None,
        clock,
    })
    .map_err(|e| anyhow::anyhow!("Failed to initialize JWT service: {e}"))
}

struct RoomServiceBuildArgs {
    clock: Arc<dyn synctv_core::Clock>,
    pool: PgPool,
    read_pool: Option<PgPool>,
    user_service: UserService,
    credential_repo: Option<Arc<UserProviderCredentialRepository>>,
    credential_encryption: Option<synctv_core::credential_encryption::CredentialEncryption>,
    provider_access_service: Arc<dyn ProviderAccessService>,
    providers_manager: Arc<ProvidersManager>,
    cache_invalidation: Arc<dyn CacheInvalidationRuntime>,
    brute_force: Arc<dyn synctv_core::service::BruteForceProtectionService>,
    audit_service: Option<Arc<AuditService>>,
    runtime_settings_store: Arc<RuntimeSettingsStore>,
    user_notification_service: Option<Arc<UserNotificationService>>,
    opaque_password_service: Arc<synctv_core::service::OpaquePasswordService>,
    room_opaque_password_registration_session_store:
        Arc<dyn synctv_core::service::RoomOpaquePasswordRegistrationSessionStore>,
    room_opaque_password_login_session_store:
        Arc<dyn synctv_core::service::RoomOpaquePasswordLoginSessionStore>,
    realtime_outbox: Option<Arc<RealtimeOutboxRepository>>,
    media_file_storage_service: Option<Arc<dyn FileStorageService>>,
    room_file_storage_service: Option<Arc<dyn FileStorageService>>,
    playlist_file_storage_service: Option<Arc<dyn FileStorageService>>,
    provider_stores: Arc<dyn ProviderStoreResolver>,
    runtime: RoomServiceRuntime,
    version_fence: Arc<dyn VersionFenceStore>,
}

struct RoomServiceRuntime {
    distributed_lock: Option<Arc<dyn synctv_core::service::CoordinationLock>>,
    playback_l2_cache: synctv_core::cache::PlaybackStateCache,
    room_settings_l2_cache: Arc<dyn synctv_core::cache::CacheL2Backend>,
    room_settings_cache_key_prefix: String,
    member_permission_l2_cache: Arc<dyn synctv_core::cache::CacheL2Backend>,
    member_permission_cache_key_prefix: String,
}

fn cache_l2_backend_for_profile(
    profile: &SharedStateProfile,
    purpose: &'static str,
) -> Result<Arc<dyn synctv_core::cache::CacheL2Backend>, anyhow::Error> {
    match profile.state_mode() {
        SharedStateMode::SharedRequired => {
            let redis_runtime = profile
                .require_shared_runtime(purpose)
                .map_err(anyhow::Error::from)?;
            Ok(Arc::new(synctv_core::cache::RedisCacheL2::from_runtime(
                redis_runtime,
            )))
        }
        SharedStateMode::SharedBestEffort => {
            let redis_runtime = profile
                .shared_runtime()
                .expect("SharedBestEffort profile must carry a shared runtime");
            Ok(Arc::new(synctv_core::cache::RedisCacheL2::from_runtime(
                redis_runtime,
            )))
        }
        SharedStateMode::LocalOnly => Ok(Arc::new(synctv_core::cache::NoopCacheL2)),
    }
}

fn build_room_service_runtime(
    profile: &SharedStateProfile,
    deployment_mode: &RedisDeploymentMode,
    cache_l2_ttl_seconds: u64,
) -> Result<RoomServiceRuntime, anyhow::Error> {
    let distributed_lock = if matches!(profile.state_mode(), SharedStateMode::SharedRequired) {
        let redis_runtime = profile
            .require_shared_runtime("room coordination locking")
            .map_err(anyhow::Error::from)?;
        Some(Arc::new(
            synctv_core::service::DistributedLock::from_runtime_with_mode(
                redis_runtime,
                matches!(deployment_mode, RedisDeploymentMode::Sentinel),
            ),
        ) as Arc<dyn synctv_core::service::CoordinationLock>)
    } else {
        None
    };

    let playback_l2_backend = cache_l2_backend_for_profile(profile, "playback cache L2")?;
    let playback_l2_cache = synctv_core::cache::PlaybackStateCache::new(
        playback_l2_backend,
        synctv_core::service::PlaybackService::DEFAULT_CACHE_SIZE,
        synctv_core::service::PlaybackService::DEFAULT_CACHE_TTL_SECS,
        cache_l2_ttl_seconds,
        KeyBuilder::new(profile.key_prefix()).namespace_prefix("playback"),
    );

    let room_settings_l2_cache = cache_l2_backend_for_profile(profile, "room settings cache L2")?;
    let member_permission_l2_cache =
        cache_l2_backend_for_profile(profile, "member permission cache L2")?;

    Ok(RoomServiceRuntime {
        distributed_lock,
        playback_l2_cache,
        room_settings_l2_cache,
        room_settings_cache_key_prefix: KeyBuilder::new(profile.key_prefix())
            .namespace_prefix("room_settings"),
        member_permission_l2_cache,
        member_permission_cache_key_prefix: KeyBuilder::new(profile.key_prefix())
            .namespace_prefix("member_permission"),
    })
}

fn build_room_service(args: RoomServiceBuildArgs) -> anyhow::Result<RoomService> {
    let RoomServiceBuildArgs {
        clock,
        pool,
        read_pool,
        user_service,
        credential_repo,
        credential_encryption,
        provider_access_service,
        providers_manager,
        cache_invalidation,
        brute_force,
        audit_service,
        runtime_settings_store,
        user_notification_service,
        opaque_password_service,
        room_opaque_password_registration_session_store,
        room_opaque_password_login_session_store,
        realtime_outbox,
        media_file_storage_service,
        room_file_storage_service,
        playlist_file_storage_service,
        provider_stores,
        runtime,
        version_fence,
    } = args;
    let permission_service = PermissionService::new_with_runtime(
        RoomMemberRepository::new(pool.clone()),
        RoomRepository::new(pool.clone()),
        synctv_core::service::PermissionServiceRuntime {
            runtime_settings_store: Some(runtime_settings_store.clone()),
            cache_size: PermissionService::DEFAULT_CACHE_SIZE,
            cache_ttl_secs: PermissionService::DEFAULT_CACHE_TTL_SECS,
            room_settings_repo: Some(RoomSettingsRepo::new(pool.clone())),
            invalidation_service: Some(cache_invalidation.clone()),
            version_fence: version_fence.clone(),
            member_permission_l2_cache: runtime.member_permission_l2_cache.clone(),
            member_permission_cache_key_prefix: runtime.member_permission_cache_key_prefix.clone(),
            room_settings_l2_cache: runtime.room_settings_l2_cache.clone(),
            room_settings_cache_key_prefix: runtime.room_settings_cache_key_prefix.clone(),
        },
    )?;
    Ok(
        RoomService::new_with_providers_permission_service_and_options(
            pool,
            user_service,
            providers_manager,
            permission_service,
            synctv_core::service::RoomServiceOptions {
                clock,
                read_pool,
                distributed_lock: runtime.distributed_lock,
                cache_invalidation: Some(cache_invalidation),
                version_fence,
                playback_l2_cache: runtime.playback_l2_cache,
                room_settings_l2_cache: runtime.room_settings_l2_cache,
                room_settings_cache_key_prefix: runtime.room_settings_cache_key_prefix,
                member_permission_l2_cache: runtime.member_permission_l2_cache,
                member_permission_cache_key_prefix: runtime.member_permission_cache_key_prefix,
                credential_encryption,
                credential_repo,
                provider_access_service: Some(provider_access_service),
                provider_stores: Some(provider_stores),
                audit_service,
                brute_force_service: Some(brute_force),
                runtime_settings_store: Some(runtime_settings_store),
                user_notification_service,
                opaque_password_service,
                opaque_password_registration_session_store:
                    room_opaque_password_registration_session_store,
                opaque_password_login_session_store: room_opaque_password_login_session_store,
                realtime_outbox,
                media_file_storage_service,
                room_file_storage_service,
                playlist_file_storage_service,
            },
        ),
    )
}

#[cfg(test)]
fn test_providers_manager() -> anyhow::Result<Arc<ProvidersManager>> {
    let provider_instance_manager = synctv_core::service::empty_provider_instance_manager();
    Ok(Arc::new(ProvidersManager::new(provider_instance_manager)?))
}

#[cfg(test)]
fn test_provider_stores() -> Arc<dyn ProviderStoreResolver> {
    Arc::new(ProviderStoreRegistry::local_only("test:provider:"))
}

fn build_publish_key_service(
    jwt_service: JwtService,
    clock: Arc<dyn synctv_core::Clock>,
    profile: &SharedStateProfile,
) -> Result<Arc<dyn StreamingPublishKeyService>, anyhow::Error> {
    let service: Arc<dyn StreamingPublishKeyService> = Arc::new(
        synctv_core::service::PublishKeyService::from_shared_state_profile(
            jwt_service,
            clock,
            24,
            profile,
        )
        .map_err(anyhow::Error::from)?,
    );
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

fn init_credential_encryption(
    hex_key: Option<&str>,
    domain: &'static str,
) -> Result<Option<synctv_core::credential_encryption::CredentialEncryption>, anyhow::Error> {
    let Some(hex_key) = (match hex_key {
        Some(hex_key) if hex_key.trim().is_empty() => None,
        Some(hex_key) => Some(hex_key.to_string()),
        None => None,
    }) else {
        return Ok(None);
    };

    match synctv_core::credential_encryption::CredentialEncryption::from_hex_key_with_domain(
        &hex_key, domain,
    ) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use synctv_core::cache::CacheInvalidationService;
    use synctv_core::models::{UserId, UserProviderCredential};
    use synctv_core::service::RateLimiter;
    use synctv_core_testing::{failing_redis_runtime, TestResultExt};

    #[test]
    fn security_options_debug_redacts_secrets() {
        let options = SecurityOptions {
            email_outbox_encryption_key: "outbox-secret-value".to_string(),
            opaque_server_setup_secret: "opaque-secret-value".to_string(),
            login_discovery_key: "discovery-secret-value".to_string(),
            ssrf: SsrfOptions::default(),
        };

        let debug = format!("{options:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("outbox-secret-value"));
        assert!(!debug.contains("opaque-secret-value"));
        assert!(!debug.contains("discovery-secret-value"));
    }

    fn test_user_service(pool: &PgPool) -> UserService {
        let jwt_service = JwtService::with_durations(
            "f4e9a7c21d3b5e6f8a9c0b1d2e3f4a5b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e2f",
            24,
            30,
            24,
            60,
        )
        .checked("jwt service");
        let username_cache = UsernameCache::local_only("test:user:".to_string(), 100, 60);
        let token_blacklist: Arc<dyn synctv_core::service::TokenBlacklistStore> = Arc::new(
            synctv_core::service::InMemoryTokenBlacklistStore::new(100, 60, 60),
        );

        UserService::new_for_tests(
            pool,
            jwt_service,
            username_cache,
            token_blacklist,
            synctv_core::cache::KeyBuilder::new("test"),
            synctv_core::service::BruteForceProtection::in_memory("test:user".to_string()),
        )
    }

    fn test_cache_invalidation() -> Arc<CacheInvalidationService> {
        Arc::new(CacheInvalidationService::new(
            "node-test".to_string(),
            "test:cache:stream".to_string(),
        ))
    }

    fn test_runtime_settings_store(pool: &PgPool) -> Arc<RuntimeSettingsStore> {
        Arc::new(RuntimeSettingsStore::new(Arc::new(SettingsService::new(
            SettingsRepository::new(pool.clone()),
            pool.clone(),
        ))))
    }

    fn test_room_opaque_registration_store(
    ) -> Arc<dyn synctv_core::service::RoomOpaquePasswordRegistrationSessionStore> {
        synctv_core::service::local_room_opaque_password_registration_session_store()
    }

    fn test_room_opaque_login_store(
    ) -> Arc<dyn synctv_core::service::RoomOpaquePasswordLoginSessionStore> {
        synctv_core::service::local_room_opaque_password_login_session_store()
    }

    fn test_room_brute_force() -> Arc<dyn synctv_core::service::BruteForceProtectionService> {
        Arc::new(synctv_core::service::BruteForceProtection::in_memory(
            "test:room".to_string(),
        ))
    }

    struct EmptyProviderCredentialReader;

    #[async_trait]
    impl synctv_core::provider::ProviderCredentialReader for EmptyProviderCredentialReader {
        async fn get_by_provider_and_server(
            &self,
            _user_id: UserId,
            _provider: &str,
            _server_id: &str,
        ) -> synctv_core::Result<Option<UserProviderCredential>> {
            Ok(None)
        }
    }

    fn test_provider_access_service() -> Arc<dyn ProviderAccessService> {
        Arc::new(CachedProviderAccessService::new(
            Arc::new(EmptyProviderCredentialReader),
            Arc::new(
                AlistProvider::new(synctv_core::service::empty_provider_instance_manager())
                    .checked("provider should build"),
            ),
        ))
    }

    #[test]
    fn test_shared_state_profile_requires_shared_runtime_in_cluster() {
        assert_eq!(
            SharedStateProfile::for_cluster_runtime(None, "test:", true).state_mode(),
            SharedStateMode::SharedRequired
        );
    }

    #[test]
    fn test_shared_state_profile_prefers_shared_runtime_when_available() {
        let profile = SharedStateProfile::new(
            SharedStateMode::SharedBestEffort,
            Some(failing_redis_runtime()),
            "test:",
        );
        assert_eq!(profile.state_mode(), SharedStateMode::SharedBestEffort);
    }

    #[test]
    fn test_shared_state_profile_uses_local_mode_without_shared_runtime() {
        assert_eq!(
            SharedStateProfile::for_cluster_runtime(None, "test:", false).state_mode(),
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
        .checked("jwt service");

        let profile = SharedStateProfile::for_cluster_runtime(None, "test:", true);
        let Err(error) =
            build_publish_key_service(jwt_service, Arc::new(synctv_core::SystemClock), &profile)
        else {
            std::panic::panic_any("cluster runtime must reject local publish-key deduplication");
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
        let profile = SharedStateProfile::for_cluster_runtime(None, "test:", true);
        let Err(error) = build_brute_force_protection(&profile) else {
            std::panic::panic_any("cluster runtime must reject local brute-force tracking");
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
        let profile = SharedStateProfile::for_cluster_runtime(None, "test:", true);
        let Err(error) = build_request_rate_limiter(&profile) else {
            std::panic::panic_any("cluster runtime must reject local rate limiting");
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
        let profile = SharedStateProfile::for_cluster_runtime(None, "test:", false);
        let limiter = build_request_rate_limiter(&profile)
            .checked("standalone mode should allow local rate limiting");

        assert!(
            limiter.check_rate_limit_sync("test-user", 1, 60).is_ok(),
            "helper must return a live rate limiter service abstraction"
        );
    }

    #[tokio::test]
    async fn test_configure_refresh_token_rate_limiter_returns_error_without_shared_runtime_in_cluster_mode(
    ) {
        let profile = SharedStateProfile::for_cluster_runtime(None, "test:", true);
        let Err(error) = build_refresh_token_rate_limiter(&profile) else {
            std::panic::panic_any("cluster runtime must reject local refresh-token rate limiting");
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
            Err(synctv_core::Error::Internal(
                "listener bootstrap failed".to_string(),
            )),
            true,
        )
        .failed("cluster mode must fail closed on provider invalidation wiring");
        assert!(
            err.to_string()
                .contains("distributed mode requires provider invalidation listener"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_standalone_provider_invalidation_failure_is_non_fatal() {
        handle_provider_invalidation_listener_result(
            Err(synctv_core::Error::Internal(
                "listener bootstrap failed".to_string(),
            )),
            false,
        )
        .checked("standalone mode may continue with local-only provider invalidation");
    }

    #[test]
    fn test_build_ws_ticket_service_uses_memory_backend_without_runtime() {
        let profile = SharedStateProfile::for_cluster_runtime(None, "test:", false);
        let service = build_ws_ticket_service(&profile)
            .checked("standalone mode should allow local WebSocket ticket storage");

        assert!(!service.supports_cluster_runtime());
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_build_ws_ticket_service_uses_distributed_backend_when_runtime_available() {
        let (_redis_container, redis_client) = synctv_core_testing::start_redis_with_client().await;
        let redis_conn = redis::aio::ConnectionManager::new(redis_client.clone())
            .await
            .checked("redis connection manager");
        let redis_runtime = Arc::new(ManagedRedisRuntime::new(
            redis_client,
            Arc::new(tokio::sync::RwLock::new(redis_conn)),
        ));

        let profile = SharedStateProfile::for_cluster_runtime(Some(redis_runtime), "test:", false);
        let service = build_ws_ticket_service(&profile)
            .checked("distributed ticket storage should be accepted");

        assert!(service.supports_cluster_runtime());
    }

    #[test]
    fn test_build_ws_ticket_service_rejects_local_backend_in_cluster_mode() {
        let profile = SharedStateProfile::for_cluster_runtime(None, "test:", true);
        let Err(error) = build_ws_ticket_service(&profile) else {
            std::panic::panic_any(
                "cluster runtime must reject local-only WebSocket ticket storage",
            );
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
        let err = handle_provider_manager_init_result(Err(synctv_core::Error::Internal(
            "provider init failed".to_string(),
        )))
        .failed("provider manager init must fail closed");

        assert!(
            err.to_string()
                .contains("RemoteProviderManager initialization failed"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn test_build_room_service_wires_brute_force_protection() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let user_service = test_user_service(&pool);
        let cache_invalidation = test_cache_invalidation();

        let room_service = build_room_service(RoomServiceBuildArgs {
            clock: Arc::new(synctv_core::SystemClock),
            pool: pool.clone(),
            read_pool: None,
            user_service,
            credential_repo: None,
            credential_encryption: None,
            provider_access_service: test_provider_access_service(),
            providers_manager: test_providers_manager().checked("providers manager should build"),
            cache_invalidation,
            brute_force: test_room_brute_force(),
            audit_service: None,
            runtime_settings_store: test_runtime_settings_store(&pool),
            user_notification_service: None,
            opaque_password_service: Arc::new(
                synctv_core::service::OpaquePasswordService::new_ephemeral_for_process(),
            ),
            room_opaque_password_registration_session_store: test_room_opaque_registration_store(),
            room_opaque_password_login_session_store: test_room_opaque_login_store(),
            realtime_outbox: None,
            media_file_storage_service: None,
            room_file_storage_service: None,
            playlist_file_storage_service: None,
            provider_stores: test_provider_stores(),
            runtime: build_room_service_runtime(
                &SharedStateProfile::for_cluster_runtime(None, "test:", false),
                &RedisDeploymentMode::Standalone,
                CacheOptions::default().l2_ttl_seconds,
            )
            .checked("room service runtime should build"),
            version_fence: Arc::new(synctv_core::cache::LocalVersionFenceStore::new()),
        })
        .checked("room service should build");

        assert!(room_service.has_brute_force_service());
        assert!(!room_service.has_distributed_lock());
        assert!(
            room_service.permission_service().has_invalidation_service(),
            "room service must wire permission invalidation even without Redis so local subscribers stay consistent"
        );
        assert!(
            room_service.has_playback_l2_cache(),
            "room service should wire playback L2 cache with the local runtime"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_build_room_service_only_enables_distributed_lock_in_cluster_mode() {
        let (_redis_container, redis_client) = synctv_core_testing::start_redis_with_client().await;
        let redis_conn = redis::aio::ConnectionManager::new(redis_client.clone())
            .await
            .checked("redis connection manager");
        let redis_runtime = Arc::new(ManagedRedisRuntime::new(
            redis_client,
            Arc::new(tokio::sync::RwLock::new(redis_conn)),
        ));

        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let user_service = test_user_service(&pool);
        let cache_invalidation = test_cache_invalidation();

        let standalone_room_service = build_room_service(RoomServiceBuildArgs {
            clock: Arc::new(synctv_core::SystemClock),
            pool: pool.clone(),
            read_pool: None,
            user_service: user_service.clone(),
            credential_repo: None,
            credential_encryption: None,
            provider_access_service: test_provider_access_service(),
            providers_manager: test_providers_manager().checked("providers manager should build"),
            cache_invalidation: cache_invalidation.clone(),
            brute_force: test_room_brute_force(),
            audit_service: None,
            runtime_settings_store: test_runtime_settings_store(&pool),
            user_notification_service: None,
            opaque_password_service: Arc::new(
                synctv_core::service::OpaquePasswordService::new_ephemeral_for_process(),
            ),
            room_opaque_password_registration_session_store: test_room_opaque_registration_store(),
            room_opaque_password_login_session_store: test_room_opaque_login_store(),
            realtime_outbox: None,
            media_file_storage_service: None,
            room_file_storage_service: None,
            playlist_file_storage_service: None,
            provider_stores: test_provider_stores(),
            runtime: build_room_service_runtime(
                &SharedStateProfile::for_cluster_runtime(
                    Some(redis_runtime.clone()),
                    "test:",
                    false,
                ),
                &RedisDeploymentMode::Standalone,
                CacheOptions::default().l2_ttl_seconds,
            )
            .checked("room service runtime should build"),
            version_fence: Arc::new(synctv_core::cache::LocalVersionFenceStore::new()),
        })
        .checked("room service should build");
        assert!(
            !standalone_room_service.has_distributed_lock(),
            "standalone mode should not enable distributed lock just because Redis is configured"
        );
        assert!(
            standalone_room_service.has_playback_l2_cache(),
            "room service should wire playback L2 cache whenever Redis is configured"
        );

        let cluster_room_service = build_room_service(RoomServiceBuildArgs {
            clock: Arc::new(synctv_core::SystemClock),
            pool: pool.clone(),
            read_pool: None,
            user_service,
            credential_repo: None,
            credential_encryption: None,
            provider_access_service: test_provider_access_service(),
            providers_manager: test_providers_manager().checked("providers manager should build"),
            cache_invalidation,
            brute_force: test_room_brute_force(),
            audit_service: None,
            runtime_settings_store: test_runtime_settings_store(&pool),
            user_notification_service: None,
            opaque_password_service: Arc::new(
                synctv_core::service::OpaquePasswordService::new_ephemeral_for_process(),
            ),
            room_opaque_password_registration_session_store: test_room_opaque_registration_store(),
            room_opaque_password_login_session_store: test_room_opaque_login_store(),
            realtime_outbox: None,
            media_file_storage_service: None,
            room_file_storage_service: None,
            playlist_file_storage_service: None,
            provider_stores: test_provider_stores(),
            runtime: build_room_service_runtime(
                &SharedStateProfile::for_cluster_runtime(Some(redis_runtime), "test:", true),
                &RedisDeploymentMode::Standalone,
                CacheOptions::default().l2_ttl_seconds,
            )
            .checked("room service runtime should build"),
            version_fence: Arc::new(synctv_core::cache::LocalVersionFenceStore::new()),
        })
        .checked("room service should build");
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
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn test_services_wiring_applies_runtime_settings_store_to_room_service() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let user_service = test_user_service(&pool);
        let cache_invalidation = test_cache_invalidation();
        let settings_service = Arc::new(SettingsService::new(
            SettingsRepository::new(pool.clone()),
            pool.clone(),
        ));
        let runtime_settings_store = Arc::new(RuntimeSettingsStore::new(settings_service));

        let room_service = build_room_service(RoomServiceBuildArgs {
            clock: Arc::new(synctv_core::SystemClock),
            pool: pool.clone(),
            read_pool: None,
            user_service: user_service.clone(),
            credential_repo: None,
            credential_encryption: None,
            provider_access_service: test_provider_access_service(),
            providers_manager: test_providers_manager().checked("providers manager should build"),
            cache_invalidation,
            brute_force: test_room_brute_force(),
            audit_service: None,
            runtime_settings_store: Arc::clone(&runtime_settings_store),
            user_notification_service: None,
            opaque_password_service: Arc::new(
                synctv_core::service::OpaquePasswordService::new_ephemeral_for_process(),
            ),
            room_opaque_password_registration_session_store: test_room_opaque_registration_store(),
            room_opaque_password_login_session_store: test_room_opaque_login_store(),
            realtime_outbox: None,
            media_file_storage_service: None,
            room_file_storage_service: None,
            playlist_file_storage_service: None,
            provider_stores: test_provider_stores(),
            runtime: build_room_service_runtime(
                &SharedStateProfile::for_cluster_runtime(None, "test:", false),
                &RedisDeploymentMode::Standalone,
                CacheOptions::default().l2_ttl_seconds,
            )
            .checked("room service runtime should build"),
            version_fence: Arc::new(synctv_core::cache::LocalVersionFenceStore::new()),
        })
        .checked("room service should build");

        assert!(room_service.has_runtime_settings_store());
        assert!(
            room_service.permission_service().has_runtime_settings_store(),
            "room permission service must use runtime permission defaults from RuntimeSettingsStore"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn test_build_room_service_reuses_injected_providers_manager() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let user_service = test_user_service(&pool);
        let cache_invalidation = test_cache_invalidation();
        let providers_manager = test_providers_manager().checked("providers manager should build");

        let room_service = build_room_service(RoomServiceBuildArgs {
            clock: Arc::new(synctv_core::SystemClock),
            pool: pool.clone(),
            read_pool: None,
            user_service,
            credential_repo: None,
            credential_encryption: None,
            provider_access_service: test_provider_access_service(),
            providers_manager: Arc::clone(&providers_manager),
            cache_invalidation,
            brute_force: test_room_brute_force(),
            audit_service: None,
            runtime_settings_store: test_runtime_settings_store(&pool),
            user_notification_service: None,
            opaque_password_service: Arc::new(
                synctv_core::service::OpaquePasswordService::new_ephemeral_for_process(),
            ),
            room_opaque_password_registration_session_store: test_room_opaque_registration_store(),
            room_opaque_password_login_session_store: test_room_opaque_login_store(),
            realtime_outbox: None,
            media_file_storage_service: None,
            room_file_storage_service: None,
            playlist_file_storage_service: None,
            provider_stores: test_provider_stores(),
            runtime: build_room_service_runtime(
                &SharedStateProfile::for_cluster_runtime(None, "test:", false),
                &RedisDeploymentMode::Standalone,
                CacheOptions::default().l2_ttl_seconds,
            )
            .checked("room service runtime should build"),
            version_fence: Arc::new(synctv_core::cache::LocalVersionFenceStore::new()),
        })
        .checked("room service should build");

        assert!(
            Arc::ptr_eq(
                room_service.media_service().providers_manager(),
                &providers_manager
            ),
            "room service must reuse the injected providers manager instead of constructing a hidden one"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn test_room_service_accepts_media_credential_encryption_injection() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let user_service = test_user_service(&pool);
        let cache_invalidation = test_cache_invalidation();
        let providers_manager = test_providers_manager().checked("providers manager should build");
        let encryption = synctv_core::credential_encryption::CredentialEncryption::new(&[7u8; 32])
            .checked("credential encryption should construct");
        let room_service = build_room_service(RoomServiceBuildArgs {
            clock: Arc::new(synctv_core::SystemClock),
            pool: pool.clone(),
            read_pool: None,
            user_service,
            credential_repo: Some(Arc::new(
                UserProviderCredentialRepository::new_with_encryption(
                    pool.clone(),
                    encryption.clone(),
                ),
            )),
            credential_encryption: Some(encryption),
            provider_access_service: test_provider_access_service(),
            providers_manager: Arc::clone(&providers_manager),
            cache_invalidation,
            brute_force: test_room_brute_force(),
            audit_service: None,
            runtime_settings_store: test_runtime_settings_store(&pool),
            user_notification_service: None,
            opaque_password_service: Arc::new(
                synctv_core::service::OpaquePasswordService::new_ephemeral_for_process(),
            ),
            room_opaque_password_registration_session_store: test_room_opaque_registration_store(),
            room_opaque_password_login_session_store: test_room_opaque_login_store(),
            realtime_outbox: None,
            media_file_storage_service: None,
            room_file_storage_service: None,
            playlist_file_storage_service: None,
            provider_stores: test_provider_stores(),
            runtime: build_room_service_runtime(
                &SharedStateProfile::for_cluster_runtime(None, "test:", false),
                &RedisDeploymentMode::Standalone,
                CacheOptions::default().l2_ttl_seconds,
            )
            .checked("room service runtime should build"),
            version_fence: Arc::new(synctv_core::cache::LocalVersionFenceStore::new()),
        })
        .checked("room service should build");

        assert!(
            Arc::ptr_eq(
                room_service.media_service().providers_manager(),
                &providers_manager
            ),
            "injecting media credential encryption must not replace the shared providers manager"
        );
    }

    #[tokio::test]
    async fn test_init_oauth2_service_rejects_cluster_mode_without_shared_state_at_bootstrap_layer()
    {
        let profile = SharedStateProfile::for_cluster_runtime(None, "synctv:", true);
        let Err(error) = build_oauth_state_store(&profile) else {
            std::panic::panic_any(
                "cluster bootstrap must not fall back to in-memory OAuth2 state store",
            );
        };

        assert!(
            error
                .to_string()
                .contains("distributed runtime requires shared single-use OAuth2 state storage"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn test_init_oauth2_service_starts_without_runtime_providers() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let runtime_settings_store = test_runtime_settings_store(&pool);
        let profile = SharedStateProfile::for_cluster_runtime(None, "synctv:", false);
        let service = init_oauth2_service(
            &pool,
            runtime_settings_store,
            Arc::new(test_user_service(&pool)),
            &profile,
            synctv_common::ssrf::SsrfGuard::strict_policy(),
        )
        .checked("OAuth2 service should start before runtime providers are configured");
        assert!(service.is_some());
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn test_build_email_token_service_uses_shared_rate_limiter() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let limiter: Arc<dyn RequestRateLimiterService> =
            Arc::new(RateLimiter::local_only("test-email-token:".to_string()));

        let service = build_email_token_service(pool, limiter, Arc::new(synctv_core::SystemClock));
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
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn test_build_providers_manager_loads_defaults_from_options() {
        let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
        let options = CoreServicesOptions::default();
        let provider_repo = Arc::new(ProviderInstanceRepository::new(pool));
        let provider_instance_manager = Arc::new(RemoteProviderManager::new_with_invalidation(
            provider_repo,
            None,
        ));

        let providers_manager = build_providers_manager(&options, provider_instance_manager)
            .await
            .checked("provider manager builder should load default providers");

        assert!(
            providers_manager.get("direct_url").await.is_some(),
            "default provider instances must be loaded during provider manager initialization"
        );
    }

    #[test]
    fn test_init_credential_encryption_rejects_invalid_key() {
        let error = init_credential_encryption(Some("not-a-64-character-hex-key"), "test")
            .failed("invalid explicit credential encryption key must fail closed");

        assert!(error
            .to_string()
            .contains("Failed to initialize credential encryption"));
    }
}
