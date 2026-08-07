use std::sync::Arc;

use synctv_core::provider::PlaybackTransportServices;
use synctv_core::service::{
    AcFunPlaybackProviderService, AlistPlaybackProviderService, BilibiliPlaybackProviderService,
    CctvPlaybackProviderService, DirectUrlPlaybackProviderService, DouyuPlaybackProviderService,
    EmbyPlaybackProviderService, FnosPlaybackProviderService, HuyaPlaybackProviderService,
    LiveProxyPlaybackProviderService, NextcloudPlaybackProviderService,
    QnapPlaybackProviderService, RoomService, RtmpPlaybackProviderService,
    SynologyPlaybackProviderService, TrueNasPlaybackProviderService, TwitchPlaybackProviderService,
    UserService,
};
use synctv_livestream::LiveStreamingInfrastructure;
use synctv_realtime::fanout::{RealtimeEventService, RealtimeFanoutService};
use synctv_realtime::sync::ConnectionRuntime;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::api_runtime::SharedApiRuntime;
use crate::proxy_signature::{MediaSwarmSigningKey, ProxySigningKey};

/// Options for creating the API transports.
#[derive(Clone)]
pub struct RouterOptions {
    pub runtime_settings: Arc<crate::ApiRuntimeSettings>,
    pub user_service: Arc<UserService>,
    /// Eventually consistent PostgreSQL pool for read-only API views.
    pub read_pool: Option<sqlx::PgPool>,
    pub user_cache: Arc<synctv_core::cache::UserCache>,
    pub room_service: Arc<RoomService>,
    pub content_filter: synctv_core::service::ContentFilter,
    pub provider_access_service: Arc<dyn synctv_core::provider::ProviderAccessService>,
    pub event_service: Arc<dyn RealtimeEventService>,
    pub connection_manager: Arc<dyn ConnectionRuntime>,
    pub presence_service: Arc<synctv_core::service::OnlinePresenceService>,
    pub jwt_service: synctv_core::service::JwtService,
    pub jwt_validator: Arc<synctv_core::service::JwtValidator>,
    pub security_pipeline: Arc<synctv_core::service::SecurityPipeline>,
    pub public_id_codec: Arc<synctv_adapter::PublicIdCodec>,
    pub request_executor: Arc<crate::impls::RequestExecutor>,
    pub metrics_access_controller: Arc<crate::metrics_auth::MetricsAccessController>,
    pub client_api: Arc<crate::impls::ClientApiImpl>,
    pub admin_api: Option<Arc<crate::impls::AdminApiImpl>>,
    pub email_api: Option<Arc<crate::impls::EmailApiImpl>>,
    pub notification_api: Option<Arc<crate::impls::NotificationApiImpl>>,
    pub oauth2_api: Option<Arc<crate::impls::OAuth2ApiImpl>>,
    pub realtime_fanout_service: Arc<dyn RealtimeFanoutService>,
    pub oauth2_service: Option<Arc<synctv_core::service::OAuth2Service>>,
    pub passkey_service: Option<Arc<synctv_core::service::PasskeyService>>,
    pub settings_service: Option<Arc<synctv_core::service::SettingsService>>,
    pub runtime_settings_store: Option<Arc<synctv_core::service::RuntimeSettingsStore>>,
    pub email_service: Option<Arc<synctv_core::service::EmailService>>,
    pub email_token_service: Option<Arc<synctv_core::service::EmailTokenService>>,
    pub publish_key_service: Option<Arc<dyn synctv_core::service::StreamingPublishKeyService>>,
    pub notification_service: Option<Arc<synctv_core::service::UserNotificationService>>,
    pub chat_service: Option<Arc<synctv_core::service::ChatService>>,
    pub audit_service: Arc<synctv_core::service::AuditService>,
    pub live_streaming_infrastructure: Option<Arc<LiveStreamingInfrastructure>>,
    pub cluster_client: Option<Arc<synctv_cluster::grpc::ClusterClient>>,
    pub rate_limiter: Arc<dyn synctv_core::service::RequestRateLimiterService>,
    /// WebSocket ticket service for secure WebSocket authentication (HTTP only)
    pub ws_ticket_service: Arc<dyn synctv_core::service::WebSocketTicketService>,
    /// Shared runtime for playback caching and other shared-state lookups.
    pub redis_runtime: Option<Arc<dyn synctv_core::RedisConnectionRuntime>>,
    /// Shared provider playback store registry reused across transports.
    pub shared_provider_stores: Arc<dyn synctv_core::provider::ProviderStoreResolver>,
    pub playback_transport_services: Arc<PlaybackTransportServices>,
    pub alist_playback_provider_service: Arc<AlistPlaybackProviderService>,
    pub bilibili_playback_provider_service: Arc<BilibiliPlaybackProviderService>,
    pub direct_url_playback_provider_service: Arc<DirectUrlPlaybackProviderService>,
    pub emby_playback_provider_service: Arc<EmbyPlaybackProviderService>,
    pub rtmp_playback_provider_service: Arc<RtmpPlaybackProviderService>,
    pub live_proxy_playback_provider_service: Arc<LiveProxyPlaybackProviderService>,
    pub twitch_playback_provider_service: Arc<TwitchPlaybackProviderService>,
    pub youtube_playback_provider_service:
        Arc<synctv_core::service::YoutubePlaybackProviderService>,
    pub douyin_playback_provider_service: Arc<synctv_core::service::DouyinPlaybackProviderService>,
    pub tiktok_playback_provider_service: Arc<synctv_core::service::TikTokPlaybackProviderService>,
    pub huya_playback_provider_service: Arc<HuyaPlaybackProviderService>,
    pub douyu_playback_provider_service: Arc<DouyuPlaybackProviderService>,
    pub acfun_playback_provider_service: Arc<AcFunPlaybackProviderService>,
    pub cctv_playback_provider_service: Arc<CctvPlaybackProviderService>,
    pub fnos_playback_provider_service: Arc<FnosPlaybackProviderService>,
    pub qnap_playback_provider_service: Arc<QnapPlaybackProviderService>,
    pub synology_playback_provider_service: Arc<SynologyPlaybackProviderService>,
    pub nextcloud_playback_provider_service: Arc<NextcloudPlaybackProviderService>,
    pub seafile_playback_provider_service:
        Arc<synctv_core::service::SeafilePlaybackProviderService>,
    pub truenas_playback_provider_service: Arc<TrueNasPlaybackProviderService>,
    pub provider_common_api: Arc<crate::providers::ProviderCommonApiImpl>,
    pub bilibili_api: Arc<crate::providers::BilibiliApiImpl>,
    pub alist_api: Arc<crate::providers::AlistApiImpl>,
    pub emby_api: Arc<crate::providers::EmbyApiImpl>,
    pub cloudreve_api: Arc<crate::providers::CloudreveApiImpl>,
    pub twitch_api: Arc<crate::providers::TwitchApiImpl>,
    pub youtube_api: Arc<crate::providers::YoutubeApiImpl>,
    pub douyin_api: Arc<crate::providers::DouyinApiImpl>,
    pub tiktok_api: Arc<crate::providers::TikTokApiImpl>,
    pub fnos_api: Arc<crate::providers::FnosApiImpl>,
    pub qnap_api: Arc<crate::providers::QnapApiImpl>,
    pub synology_api: Arc<crate::providers::SynologyApiImpl>,
    pub nextcloud_api: Arc<crate::providers::NextcloudApiImpl>,
    pub seafile_api: Arc<crate::providers::SeafileApiImpl>,
    pub truenas_api: Arc<crate::providers::TrueNasApiImpl>,
    /// Shared proxy signing key reused across transports.
    pub shared_proxy_signing_key: Arc<ProxySigningKey>,
    /// Signing key for WebRTC media swarm announcements.
    pub media_swarm_signing_key: Arc<MediaSwarmSigningKey>,
    /// Resolved built-in STUN URL (e.g. "stun:203.0.113.1:3478") from a successfully started
    /// STUN server. When `None`, the built-in STUN entry is omitted from ICE server lists.
    pub builtin_stun_url: Option<String>,
    /// Structured WebRTC/STUN runtime state exposed through health and ICE bootstrap responses.
    pub webrtc_status: synctv_core::service::WebRtcRuntimeStatus,
    /// Credential encryption for provider credential resolution
    pub credential_encryption: Option<synctv_core::credential_encryption::CredentialEncryption>,
    /// Shared proxy slice cache instance managed by the runtime.
    pub proxy_slice_cache: Arc<synctv_proxy::slice_cache::SliceCache>,
    /// Global SSRF policy used by proxy handlers.
    pub ssrf_guard: synctv_common::ssrf::SsrfGuard,
    /// Shared outbound HTTP client used by proxy handlers and cache fills.
    pub proxy_http_client: reqwest::Client,
    /// Rate limit configuration for WebSocket chat messaging.
    /// This is separate from the HTTP request rate limit config used by the
    /// shared request execution path.
    pub messaging_rate_limit_config: synctv_core::service::RateLimitConfig,
    /// Heartbeat/cache timing for real-time messaging. Production defaults are
    /// conservative; tests may inject a shorter schedule.
    pub heartbeat_schedule: crate::impls::HeartbeatSchedule,
    /// Providers manager for playback generation and provider HTTP APIs.
    pub providers_manager: Arc<synctv_core::service::ProvidersManager>,
    pub playback_duration_probe: Option<Arc<synctv_core::service::PlaybackDurationProbeService>>,
}

#[derive(Clone)]
pub struct AppState {
    /// Common service construction options (shared cheaply via `Arc`).
    pub router_options: Arc<RouterOptions>,
    /// Shared transport-agnostic runtime reused across HTTP, gRPC, and management.
    pub shared_api_runtime: Arc<SharedApiRuntime>,
    pub metrics_access_controller: Arc<crate::metrics_auth::MetricsAccessController>,
    #[cfg(any(test, feature = "test-support"))]
    test_database_leases: Arc<std::sync::Mutex<Vec<synctv_core_testing::TestDatabase>>>,
}

pub struct ProxyCacheLifecycleRuntime {
    pub cancel: CancellationToken,
    pub handle: JoinHandle<()>,
}

impl std::ops::Deref for AppState {
    type Target = RouterOptions;
    fn deref(&self) -> &RouterOptions {
        &self.router_options
    }
}

impl AppState {
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn with_test_database_leases(
        mut self,
        leases: Vec<synctv_core_testing::TestDatabase>,
    ) -> Self {
        self.test_database_leases = Arc::new(std::sync::Mutex::new(leases));
        self
    }

    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn with_shared_test_database_leases(
        mut self,
        leases: Arc<std::sync::Mutex<Vec<synctv_core_testing::TestDatabase>>>,
    ) -> Self {
        self.test_database_leases = leases;
        self
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn test_database_leases(
        &self,
    ) -> Arc<std::sync::Mutex<Vec<synctv_core_testing::TestDatabase>>> {
        Arc::clone(&self.test_database_leases)
    }

    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn with_added_test_database_lease(self, lease: synctv_core_testing::TestDatabase) -> Self {
        match self.test_database_leases.lock() {
            Ok(mut leases) => leases.push(lease),
            Err(error) => {
                tracing::warn!(error = %error, "test database lease lock was poisoned");
            }
        }
        self
    }

    /// Resolve a fresh Redis `ConnectionManager` clone from the shared `RwLock`.
    ///
    /// Returns `None` when Redis is not configured.
    pub async fn resolve_redis_conn(&self) -> Option<redis::aio::ConnectionManager> {
        match &self.shared_api_runtime.redis_runtime {
            Some(runtime) => match runtime.snapshot().await {
                Ok(conn) => Some(conn),
                Err(error) => {
                    tracing::warn!(error = %error, "Redis connection snapshot failed");
                    None
                }
            },
            None => None,
        }
    }
}

/// Create shared `AppState` once so multiple transports can reuse the same impl instances.
pub fn create_app_state_from_options(options: RouterOptions) -> anyhow::Result<AppState> {
    build_app_state(options)
}

/// Build `AppState` from `RouterOptions`, creating the shared API implementation layers.
pub fn build_app_state(options: RouterOptions) -> anyhow::Result<AppState> {
    let shared_api_runtime = Arc::new(build_shared_api_runtime(&options)?);

    Ok(AppState {
        router_options: Arc::new(options),
        shared_api_runtime: shared_api_runtime.clone(),
        metrics_access_controller: shared_api_runtime.metrics_access_controller.clone(),
        #[cfg(any(test, feature = "test-support"))]
        test_database_leases: Arc::new(std::sync::Mutex::new(Vec::new())),
    })
}

pub fn build_shared_api_runtime(options: &RouterOptions) -> anyhow::Result<SharedApiRuntime> {
    let redis_runtime = options.redis_runtime.clone();
    let proxy_signing_key = options.shared_proxy_signing_key.clone();
    let media_swarm_signing_key = options.media_swarm_signing_key.clone();
    let provider_stores = options.shared_provider_stores.clone();
    let provider_access_service = options.provider_access_service.clone();
    let security_pipeline = options.security_pipeline.clone();
    let jwt_validator = options.jwt_validator.clone();
    let public_id_codec = options.public_id_codec.clone();
    let request_executor = options.request_executor.clone();

    // Create shared RateLimitConfig from the runtime settings.
    let rate_limit_config = Arc::new(options.runtime_settings.request_rate_limits.clone());

    // Create shared messaging rate limit config for WebSocket chat messages.
    let messaging_rate_limit_config = Arc::new(options.messaging_rate_limit_config.clone());

    Ok(SharedApiRuntime {
        redis_runtime,
        rate_limit_config,
        messaging_rate_limit_config,
        content_filter: Arc::new(options.content_filter.clone()),
        heartbeat_schedule: options.heartbeat_schedule,
        jwt_validator,
        security_pipeline,
        public_id_codec,
        request_executor,
        metrics_access_controller: options.metrics_access_controller.clone(),
        client_api: options.client_api.clone(),
        admin_api: options.admin_api.clone(),
        email_api: options.email_api.clone(),
        notification_api: options.notification_api.clone(),
        oauth2_api: options.oauth2_api.clone(),
        provider_common_api: options.provider_common_api.clone(),
        bilibili_api: options.bilibili_api.clone(),
        alist_api: options.alist_api.clone(),
        emby_api: options.emby_api.clone(),
        cloudreve_api: options.cloudreve_api.clone(),
        twitch_api: options.twitch_api.clone(),
        youtube_api: options.youtube_api.clone(),
        douyin_api: options.douyin_api.clone(),
        tiktok_api: options.tiktok_api.clone(),
        fnos_api: options.fnos_api.clone(),
        qnap_api: options.qnap_api.clone(),
        synology_api: options.synology_api.clone(),
        nextcloud_api: options.nextcloud_api.clone(),
        seafile_api: options.seafile_api.clone(),
        truenas_api: options.truenas_api.clone(),
        provider_access_service,
        provider_stores,
        playback_transport_services: options.playback_transport_services.clone(),
        alist_playback_provider_service: options.alist_playback_provider_service.clone(),
        bilibili_playback_provider_service: options.bilibili_playback_provider_service.clone(),
        direct_url_playback_provider_service: options.direct_url_playback_provider_service.clone(),
        emby_playback_provider_service: options.emby_playback_provider_service.clone(),
        rtmp_playback_provider_service: options.rtmp_playback_provider_service.clone(),
        live_proxy_playback_provider_service: options.live_proxy_playback_provider_service.clone(),
        twitch_playback_provider_service: options.twitch_playback_provider_service.clone(),
        youtube_playback_provider_service: options.youtube_playback_provider_service.clone(),
        douyin_playback_provider_service: options.douyin_playback_provider_service.clone(),
        tiktok_playback_provider_service: options.tiktok_playback_provider_service.clone(),
        huya_playback_provider_service: options.huya_playback_provider_service.clone(),
        douyu_playback_provider_service: options.douyu_playback_provider_service.clone(),
        acfun_playback_provider_service: options.acfun_playback_provider_service.clone(),
        cctv_playback_provider_service: options.cctv_playback_provider_service.clone(),
        fnos_playback_provider_service: options.fnos_playback_provider_service.clone(),
        qnap_playback_provider_service: options.qnap_playback_provider_service.clone(),
        synology_playback_provider_service: options.synology_playback_provider_service.clone(),
        nextcloud_playback_provider_service: options.nextcloud_playback_provider_service.clone(),
        seafile_playback_provider_service: options.seafile_playback_provider_service.clone(),
        truenas_playback_provider_service: options.truenas_playback_provider_service.clone(),
        proxy_signing_key,
        media_swarm_signing_key,
        webrtc_status: options.webrtc_status.clone(),
        server_state_runtime: Arc::new(crate::status::server_state_runtime_from_router_options(
            options,
        )),
        slice_cache_management_runtime: Arc::new(
            crate::status::slice_cache_management_runtime_from_router_options(options),
        ),
    })
}
