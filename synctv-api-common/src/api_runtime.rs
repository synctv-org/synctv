use std::sync::Arc;

use synctv_core::provider::PlaybackTransportServices;
use synctv_core::service::{
    AcFunPlaybackProviderService, AlistPlaybackProviderService, BilibiliPlaybackProviderService,
    CctvPlaybackProviderService, DirectUrlPlaybackProviderService, DouyinPlaybackProviderService,
    DouyuPlaybackProviderService, EmbyPlaybackProviderService, FnosPlaybackProviderService,
    HuyaPlaybackProviderService, LiveProxyPlaybackProviderService,
    NextcloudPlaybackProviderService, QnapPlaybackProviderService, RtmpPlaybackProviderService,
    SeafilePlaybackProviderService, SynologyPlaybackProviderService, TikTokPlaybackProviderService,
    TrueNasPlaybackProviderService, TwitchPlaybackProviderService, YoutubePlaybackProviderService,
};

use crate::proxy_signature::ProxySigningKey;
use crate::server_settings::ApiServerSettings;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RateLimitScopeStrategy {
    #[default]
    FixedWindow,
    Disabled,
}

impl RateLimitScopeStrategy {
    #[must_use]
    pub const fn enabled(self) -> bool {
        matches!(self, Self::FixedWindow)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RateLimitScopeRule {
    pub max_requests: Option<u32>,
    pub window_seconds: Option<u64>,
    pub strategy: RateLimitScopeStrategy,
}

#[derive(Debug, Clone)]
pub struct RequestRateLimitSettings {
    pub auth_max_requests: u32,
    pub auth_window_seconds: u64,
    pub write_max_requests: u32,
    pub write_window_seconds: u64,
    pub read_max_requests: u32,
    pub read_window_seconds: u64,
    pub media_max_requests: u32,
    pub media_window_seconds: u64,
    pub admin_max_requests: u32,
    pub admin_window_seconds: u64,
    pub streaming_max_requests: u32,
    pub streaming_window_seconds: u64,
    pub websocket_max_requests: u32,
    pub websocket_window_seconds: u64,
    pub scopes: std::collections::HashMap<String, RateLimitScopeRule>,
}

impl Default for RequestRateLimitSettings {
    fn default() -> Self {
        Self {
            auth_max_requests: 5,
            auth_window_seconds: 60,
            write_max_requests: 120,
            write_window_seconds: 60,
            read_max_requests: 600,
            read_window_seconds: 60,
            media_max_requests: 120,
            media_window_seconds: 60,
            admin_max_requests: 180,
            admin_window_seconds: 60,
            streaming_max_requests: 1200,
            streaming_window_seconds: 60,
            websocket_max_requests: 60,
            websocket_window_seconds: 60,
            scopes: std::collections::HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MetricsAuthMode {
    #[default]
    BearerToken,
    Basic,
    Kubernetes,
}

#[derive(Debug, Clone)]
pub struct MetricsKubernetesAuthSettings {
    pub audience: String,
    pub authentication_cache_ttl_seconds: u64,
    pub authorization_cache_ttl_seconds: u64,
}

impl Default for MetricsKubernetesAuthSettings {
    fn default() -> Self {
        Self {
            audience: String::new(),
            authentication_cache_ttl_seconds: 60,
            authorization_cache_ttl_seconds: 60,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MetricsAuthSettings {
    pub mode: MetricsAuthMode,
    pub bearer_token: String,
    pub basic_username: String,
    pub basic_password: String,
    pub kubernetes: MetricsKubernetesAuthSettings,
}

impl Default for MetricsAuthSettings {
    fn default() -> Self {
        Self {
            mode: MetricsAuthMode::BearerToken,
            bearer_token: String::new(),
            basic_username: String::new(),
            basic_password: String::new(),
            kubernetes: MetricsKubernetesAuthSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct MetricsRuntimeSettings {
    pub enabled: bool,
    pub auth: MetricsAuthSettings,
}

#[derive(Debug, Clone)]
pub struct LivestreamRuntimeSettings {
    pub rtmp_port: u16,
    pub public_rtmp_host: String,
    pub flv_max_connection_duration_seconds: u64,
    pub flv_write_timeout_seconds: u64,
}

impl Default for LivestreamRuntimeSettings {
    fn default() -> Self {
        Self {
            rtmp_port: 1935,
            public_rtmp_host: String::new(),
            flv_max_connection_duration_seconds: 86400,
            flv_write_timeout_seconds: 30,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct WebRtcRuntimeSettings {
    pub filter_private_ice_candidates: bool,
}

#[derive(Debug, Clone)]
pub struct ProxySliceCacheRuntimeSettings {
    pub enabled: bool,
    pub slice_size_bytes: usize,
    pub max_cache_size_bytes: u64,
    pub segment_ttl_seconds: u64,
    pub stale_max_age_seconds: u64,
    pub stale_while_revalidate: bool,
    pub file_backend_enabled: bool,
    pub file_cache_dir: String,
    pub eviction_interval_seconds: u64,
    pub watermark_ratio: f64,
}

impl Default for ProxySliceCacheRuntimeSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            slice_size_bytes: 2 * 1024 * 1024,
            max_cache_size_bytes: 512 * 1024 * 1024,
            segment_ttl_seconds: 300,
            stale_max_age_seconds: 60,
            stale_while_revalidate: true,
            file_backend_enabled: false,
            file_cache_dir: String::new(),
            eviction_interval_seconds: 60,
            watermark_ratio: 0.875,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ClusterRuntimeSettings {
    pub secret: String,
}

#[derive(Debug, Clone)]
pub struct RedisRuntimeSettings {
    pub key_prefix: String,
}

impl Default for RedisRuntimeSettings {
    fn default() -> Self {
        Self {
            key_prefix: "synctv:".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConnectionLimitSettings {
    pub ws_message_rate_limit_per_second: u32,
}

impl Default for ConnectionLimitSettings {
    fn default() -> Self {
        Self {
            ws_message_rate_limit_per_second: 50,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ApiRuntimeSettings {
    pub server: ApiServerSettings,
    pub request_rate_limits: RequestRateLimitSettings,
    pub metrics: MetricsRuntimeSettings,
    pub cluster_enabled: bool,
    pub cluster_secret_configured: bool,
    pub livestream: LivestreamRuntimeSettings,
    pub webrtc: WebRtcRuntimeSettings,
    pub proxy_slice_cache: ProxySliceCacheRuntimeSettings,
    pub cluster: ClusterRuntimeSettings,
    pub redis: RedisRuntimeSettings,
    pub connection_limits: ConnectionLimitSettings,
    pub server_state: synctv_core::service::ServerStateRuntimeParams,
}

impl Default for ApiRuntimeSettings {
    fn default() -> Self {
        Self {
            server: ApiServerSettings::default(),
            request_rate_limits: RequestRateLimitSettings::default(),
            metrics: MetricsRuntimeSettings::default(),
            cluster_enabled: false,
            cluster_secret_configured: false,
            livestream: LivestreamRuntimeSettings::default(),
            webrtc: WebRtcRuntimeSettings::default(),
            proxy_slice_cache: ProxySliceCacheRuntimeSettings::default(),
            cluster: ClusterRuntimeSettings::default(),
            redis: RedisRuntimeSettings::default(),
            connection_limits: ConnectionLimitSettings::default(),
            server_state: synctv_core::service::ServerStateRuntimeParams {
                cluster_enabled: false,
                advertise_api_address: String::new(),
                cluster: synctv_core::service::ServerStateClusterOptions::default(),
                database: synctv_core::service::ServerStateDatabaseOptions::default(),
                redis: synctv_core::service::ServerStateRedisOptions::default(),
                livestream: synctv_core::service::ServerStateLivestreamOptions::default(),
            },
        }
    }
}

impl ApiRuntimeSettings {
    #[must_use]
    pub const fn cluster_runtime_enabled(&self) -> bool {
        self.cluster_enabled
    }

    #[must_use]
    pub fn api_address(&self) -> String {
        self.server.bind_address.clone()
    }

    #[must_use]
    pub fn public_rtmp_host(&self) -> String {
        if self.livestream.public_rtmp_host.is_empty() {
            "127.0.0.1".to_string()
        } else {
            self.livestream.public_rtmp_host.clone()
        }
    }
}

/// Shared transport-agnostic API runtime derived from runtime settings.
///
/// HTTP, gRPC, and management transports reuse these instances instead of
/// constructing parallel API impls, validators, caches, or provider stores.
#[derive(Clone)]
pub struct SharedApiRuntime {
    pub redis_runtime: Option<Arc<dyn synctv_core::RedisConnectionRuntime>>,
    pub rate_limit_config: Arc<RequestRateLimitSettings>,
    pub messaging_rate_limit_config: Arc<synctv_core::service::RateLimitConfig>,
    pub content_filter: Arc<synctv_core::service::ContentFilter>,
    pub heartbeat_schedule: crate::impls::HeartbeatSchedule,
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
    pub provider_access_service: Arc<dyn synctv_core::provider::ProviderAccessService>,
    pub provider_stores: Arc<dyn synctv_core::provider::ProviderStoreResolver>,
    pub playback_transport_services: Arc<PlaybackTransportServices>,
    pub alist_playback_provider_service: Arc<AlistPlaybackProviderService>,
    pub bilibili_playback_provider_service: Arc<BilibiliPlaybackProviderService>,
    pub direct_url_playback_provider_service: Arc<DirectUrlPlaybackProviderService>,
    pub emby_playback_provider_service: Arc<EmbyPlaybackProviderService>,
    pub rtmp_playback_provider_service: Arc<RtmpPlaybackProviderService>,
    pub live_proxy_playback_provider_service: Arc<LiveProxyPlaybackProviderService>,
    pub twitch_playback_provider_service: Arc<TwitchPlaybackProviderService>,
    pub youtube_playback_provider_service: Arc<YoutubePlaybackProviderService>,
    pub huya_playback_provider_service: Arc<HuyaPlaybackProviderService>,
    pub douyu_playback_provider_service: Arc<DouyuPlaybackProviderService>,
    pub douyin_playback_provider_service: Arc<DouyinPlaybackProviderService>,
    pub tiktok_playback_provider_service: Arc<TikTokPlaybackProviderService>,
    pub acfun_playback_provider_service: Arc<AcFunPlaybackProviderService>,
    pub cctv_playback_provider_service: Arc<CctvPlaybackProviderService>,
    pub fnos_playback_provider_service: Arc<FnosPlaybackProviderService>,
    pub qnap_playback_provider_service: Arc<QnapPlaybackProviderService>,
    pub synology_playback_provider_service: Arc<SynologyPlaybackProviderService>,
    pub nextcloud_playback_provider_service: Arc<NextcloudPlaybackProviderService>,
    pub seafile_playback_provider_service: Arc<SeafilePlaybackProviderService>,
    pub truenas_playback_provider_service: Arc<TrueNasPlaybackProviderService>,
    pub proxy_signing_key: Arc<ProxySigningKey>,
    pub webrtc_status: synctv_core::service::WebRtcRuntimeStatus,
    pub server_state_runtime: Arc<crate::status::ServerStateRuntime>,
    pub slice_cache_management_runtime: Arc<crate::status::SliceCacheManagementRuntime>,
}
