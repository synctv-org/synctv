//! Admin API Implementation

use std::sync::Arc;
use synctv_core::models::UserId;
use synctv_core::service::{
    AuditService, BanRecordService, EmailService, RemoteProviderManager, ReviewService,
    RoomService, SettingsRegistry, SettingsService, UserService,
};
use synctv_livestream::api::LiveStreamingInfrastructure;

#[cfg(test)]
use synctv_core::models::MediaId;
#[cfg(test)]
use synctv_core::models::SortDirection as CoreSortDirection;

use super::client::convert::{
    json_to_vec, playback_client_profile_from_proto, provider_playback_info_to_model,
    sign_local_bilibili_danmaku_urls, try_bilibili_live_danmaku_for_static_media,
    try_members_to_proto, try_playback_state_to_proto, try_playback_to_proto, user_status_to_proto,
};
use super::client::user_notification_preferences_to_proto;
use super::ApiError;
use crate::fanout::{default_room_settings_fanout_service, RoomSettingsFanoutService};
use crate::impls::client::media::prepare_delete_entries_outbox_fanout;

use crate::impls::client::playback_lifecycle::ProviderPlaybackLifecycleApi;
use crate::impls::playback::playback_expires_at;
use crate::impls::RequestExecutor;
use crate::media_fanout::{default_media_fanout_service, MediaFanoutService};
use crate::membership_event_fanout::{
    default_membership_event_fanout_service, MembershipEventFanoutService,
};
use crate::playback_fanout::{
    default_playback_fanout_service, PlaybackFanoutActor, PlaybackFanoutService,
};
use crate::playlist_fanout::{default_playlist_fanout_service, PlaylistFanoutService};
use crate::realtime_fanout::RealtimeFanoutService;
use crate::realtime_lifecycle::{default_realtime_lifecycle_service, RealtimeLifecycleService};
use crate::room_cache_fanout::{default_room_cache_fanout_service, RoomCacheFanoutService};
use crate::room_lifecycle_fanout::{
    default_room_lifecycle_fanout_service_with_realtime, RoomLifecycleFanoutService,
};
use crate::runtime::{
    LocalNoopRealtimeEventService, RealtimeConnectionService, RealtimeEventService,
};

mod audit;
mod auth;
mod batch;
mod common;
mod lifecycle;
mod livestream;
mod mapping;
mod media;
mod playback;
mod query;
mod response;
mod reviews;
mod rooms;
mod settings;
mod stats;
mod users;

pub use auth::{validate_admin_auth, ValidatedAdmin};
use common::*;
use lifecycle::*;
use mapping::*;
use query::*;
use response::*;

/// HTTP request context for audit logging (IP address and User-Agent).
#[derive(Debug, Clone, Default)]
pub struct RequestContext {
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
}

pub const LOCAL_MANAGEMENT_ACTOR_USER_ID: UserId = UserId::MAX;

#[derive(Clone)]
pub struct AdminApiConfig {
    pub room_service: Arc<RoomService>,
    pub user_service: Arc<UserService>,
    pub settings_service: Arc<SettingsService>,
    pub settings_registry: Option<Arc<SettingsRegistry>>,
    pub email_service: Arc<EmailService>,
    pub connection_service: Arc<dyn RealtimeConnectionService>,
    pub provider_instance_manager: Arc<RemoteProviderManager>,
    pub live_streaming_infrastructure: Option<Arc<LiveStreamingInfrastructure>>,
    pub publish_key_service: Option<Arc<dyn synctv_core::service::StreamingPublishKeyService>>,
    pub config: Arc<synctv_core::Config>,
    pub audit_service: Arc<AuditService>,
    pub public_id_codec: Arc<crate::PublicIdCodec>,
}

pub struct AdminApiRuntime {
    pub realtime_fanout: Arc<dyn RealtimeFanoutService>,
    pub realtime_event_service: Arc<dyn RealtimeEventService>,
    pub provider_stores: Option<Arc<dyn synctv_core::provider::store::ProviderStoreResolver>>,
    pub provider_access_service: Option<Arc<dyn synctv_core::provider::ProviderAccessService>>,
    pub request_executor: Option<Arc<RequestExecutor>>,
}

impl AdminApiRuntime {
    #[must_use]
    pub fn test_disabled() -> Self {
        let realtime_event_service = Arc::new(LocalNoopRealtimeEventService::new());
        Self {
            realtime_fanout: crate::realtime_fanout::local_realtime_fanout_service(
                realtime_event_service.clone(),
            ),
            realtime_event_service,
            provider_stores: None,
            provider_access_service: None,
            request_executor: None,
        }
    }
}

/// Admin API implementation
#[derive(Clone)]
pub struct AdminApiImpl {
    pub room_service: Arc<RoomService>,
    pub user_service: Arc<UserService>,
    pub review_service: Arc<ReviewService>,
    pub ban_record_service: Arc<BanRecordService>,
    pub settings_service: Arc<SettingsService>,
    pub settings_registry: Option<Arc<SettingsRegistry>>,
    pub email_service: Arc<EmailService>,
    pub connection_service: Arc<dyn RealtimeConnectionService>,
    pub provider_instance_manager: Arc<RemoteProviderManager>,
    pub live_streaming_infrastructure: Option<Arc<LiveStreamingInfrastructure>>,
    pub publish_key_service: Option<Arc<dyn synctv_core::service::StreamingPublishKeyService>>,
    pub config: Arc<synctv_core::Config>,
    pub realtime_fanout: Arc<dyn RealtimeFanoutService>,
    pub room_settings_fanout: Arc<dyn RoomSettingsFanoutService>,
    pub membership_event_fanout: Arc<dyn MembershipEventFanoutService>,
    pub media_fanout: Arc<dyn MediaFanoutService>,
    pub playback_fanout: Arc<dyn PlaybackFanoutService>,
    pub playlist_fanout: Arc<dyn PlaylistFanoutService>,
    pub room_cache_fanout: Arc<dyn RoomCacheFanoutService>,
    pub realtime_lifecycle: Arc<dyn RealtimeLifecycleService>,
    pub room_lifecycle_fanout: Arc<dyn RoomLifecycleFanoutService>,
    pub realtime_event_service: Arc<dyn RealtimeEventService>,
    pub audit_service: Arc<AuditService>,
    pub provider_stores: Option<Arc<dyn synctv_core::provider::store::ProviderStoreResolver>>,
    pub provider_access_service: Option<Arc<dyn synctv_core::provider::ProviderAccessService>>,
    pub public_id_codec: Arc<crate::PublicIdCodec>,
    pub request_executor: Option<Arc<RequestExecutor>>,
}

impl AdminApiImpl {
    #[must_use]
    pub fn new_with_runtime(config: AdminApiConfig, runtime: AdminApiRuntime) -> Self {
        let AdminApiConfig {
            room_service,
            user_service,
            settings_service,
            settings_registry,
            email_service,
            connection_service,
            provider_instance_manager,
            live_streaming_infrastructure,
            publish_key_service,
            config,
            audit_service,
            public_id_codec,
        } = config;

        let review_service = Arc::new(ReviewService::new(user_service.pool().clone()));
        let ban_record_service = Arc::new(BanRecordService::new(user_service.pool().clone()));
        let realtime_fanout = runtime.realtime_fanout;
        let realtime_event_service = runtime.realtime_event_service;
        let room_settings_fanout = default_room_settings_fanout_service(realtime_fanout.clone());
        let membership_event_fanout = default_membership_event_fanout_service(
            realtime_fanout.clone(),
            realtime_event_service.clone(),
        );
        let media_fanout = default_media_fanout_service(realtime_fanout.clone());
        let playback_fanout = default_playback_fanout_service(realtime_fanout.clone());
        let playlist_fanout = default_playlist_fanout_service(realtime_fanout.clone());
        let room_cache_fanout = default_room_cache_fanout_service(realtime_fanout.clone());
        let realtime_lifecycle = default_realtime_lifecycle_service(
            connection_service.clone(),
            live_streaming_infrastructure.clone(),
            realtime_fanout.clone(),
        );
        let room_lifecycle_fanout = default_room_lifecycle_fanout_service_with_realtime(
            realtime_fanout.clone(),
            realtime_event_service.clone(),
        );
        Self {
            room_service,
            user_service,
            review_service,
            ban_record_service,
            settings_service,
            settings_registry,
            email_service,
            connection_service,
            provider_instance_manager,
            live_streaming_infrastructure,
            publish_key_service,
            config,
            realtime_fanout,
            room_settings_fanout,
            membership_event_fanout,
            media_fanout,
            playback_fanout,
            playlist_fanout,
            room_cache_fanout,
            realtime_lifecycle,
            room_lifecycle_fanout,
            realtime_event_service,
            audit_service,
            provider_stores: runtime.provider_stores,
            provider_access_service: runtime.provider_access_service,
            public_id_codec,
            request_executor: runtime.request_executor,
        }
    }
}

#[cfg(test)]
mod tests;
