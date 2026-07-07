use std::sync::Arc;

use synctv_core::provider::PlaybackTransportServices;
use synctv_core::repository::UserProviderCredentialRepository;
use synctv_core::service::{
    AlistPlaybackProviderService, BilibiliPlaybackProviderService,
    DirectUrlPlaybackProviderService, EmbyPlaybackProviderService,
    LiveProxyPlaybackProviderService, RtmpPlaybackProviderService,
};

use crate::proxy_signature::ProxySigningKey;

/// Shared transport-agnostic API runtime derived from router configuration.
///
/// HTTP, gRPC, and management transports reuse these instances instead of
/// constructing parallel API impls, validators, caches, or provider stores.
#[derive(Clone)]
pub struct SharedApiRuntime {
    pub redis_runtime: Option<Arc<dyn synctv_core::RedisConnectionRuntime>>,
    pub rate_limit_config: Arc<synctv_core::RequestRateLimitConfig>,
    pub messaging_rate_limit_config: Arc<synctv_core::service::RateLimitConfig>,
    pub content_filter: Arc<synctv_core::service::ContentFilter>,
    pub heartbeat_schedule: crate::impls::HeartbeatSchedule,
    pub jwt_validator: Arc<synctv_core::service::JwtValidator>,
    pub security_pipeline: Arc<synctv_core::service::SecurityPipeline>,
    pub public_id_codec: Arc<crate::public_id::PublicIdCodec>,
    pub request_executor: Arc<crate::impls::RequestExecutor>,
    pub metrics_access_controller: Arc<crate::metrics_auth::MetricsAccessController>,
    pub client_api: Arc<crate::impls::ClientApiImpl>,
    pub admin_api: Option<Arc<crate::impls::AdminApiImpl>>,
    pub email_api: Option<Arc<crate::impls::EmailApiImpl>>,
    pub notification_api: Option<Arc<crate::impls::NotificationApiImpl>>,
    pub oauth2_api: Option<Arc<crate::impls::OAuth2ApiImpl>>,
    pub provider_common_api: Arc<crate::impls::ProviderCommonApiImpl>,
    pub bilibili_api: Arc<crate::impls::BilibiliApiImpl>,
    pub alist_api: Arc<crate::impls::AlistApiImpl>,
    pub emby_api: Arc<crate::impls::EmbyApiImpl>,
    pub user_provider_credential_repository: Arc<UserProviderCredentialRepository>,
    pub provider_access_service: Arc<dyn synctv_core::provider::ProviderAccessService>,
    pub provider_stores: Arc<dyn synctv_core::provider::ProviderStoreResolver>,
    pub playback_transport_services: Arc<PlaybackTransportServices>,
    pub alist_playback_provider_service: Arc<AlistPlaybackProviderService>,
    pub bilibili_playback_provider_service: Arc<BilibiliPlaybackProviderService>,
    pub direct_url_playback_provider_service: Arc<DirectUrlPlaybackProviderService>,
    pub emby_playback_provider_service: Arc<EmbyPlaybackProviderService>,
    pub rtmp_playback_provider_service: Arc<RtmpPlaybackProviderService>,
    pub live_proxy_playback_provider_service: Arc<LiveProxyPlaybackProviderService>,
    pub proxy_signing_key: Arc<ProxySigningKey>,
    pub webrtc_status: synctv_core::service::WebRtcRuntimeStatus,
    pub server_state_runtime: Arc<crate::status::ServerStateRuntime>,
}
