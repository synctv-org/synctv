use std::sync::Arc;

use crate::service::SystemStatsService;
use crate::service::{OnlinePresenceService, PresenceOverview, RemoteProviderManager, RoomService};
use crate::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceAdditionalState {
    pub active_streams: i64,
    pub open_reports: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceState {
    pub total_users: i64,
    pub active_users: i64,
    pub banned_users: i64,
    pub total_rooms: i64,
    pub active_rooms: i64,
    pub banned_rooms: i64,
    pub total_media: i64,
    pub provider_instances: i64,
    pub additional_state: ServiceAdditionalState,
    pub presence: PresenceOverview,
}

pub struct ServiceStateServiceDependencies {
    pub system_stats_service: Arc<SystemStatsService>,
    pub provider_instance_manager: Arc<RemoteProviderManager>,
    pub room_service: Arc<RoomService>,
    pub presence_service: Arc<OnlinePresenceService>,
}

#[derive(Clone)]
pub struct ServiceStateService {
    system_stats_service: Arc<SystemStatsService>,
    provider_instance_manager: Arc<RemoteProviderManager>,
    room_service: Arc<RoomService>,
    presence_service: Arc<OnlinePresenceService>,
}

impl ServiceStateService {
    #[must_use]
    pub fn new(deps: ServiceStateServiceDependencies) -> Self {
        Self {
            system_stats_service: deps.system_stats_service,
            provider_instance_manager: deps.provider_instance_manager,
            room_service: deps.room_service,
            presence_service: deps.presence_service,
        }
    }

    pub async fn get_service_state(&self) -> Result<ServiceState> {
        let (stats_res, provider_instances_res, total_media_res, presence_res) = tokio::join!(
            self.system_stats_service.get_system_stats(),
            self.provider_instance_manager.get_all_instances(),
            self.room_service.media_service().count_all_media(),
            self.presence_service.overview(),
        );

        let stats = stats_res?;
        let provider_instances = i64::try_from(provider_instances_res?.len()).unwrap_or(i64::MAX);
        let total_media = total_media_res?;
        let presence = presence_res?;

        Ok(ServiceState {
            total_users: stats.total_users,
            active_users: stats.active_users,
            banned_users: stats.banned_users,
            total_rooms: stats.total_rooms,
            active_rooms: stats.active_rooms,
            banned_rooms: stats.banned_rooms,
            total_media,
            provider_instances,
            additional_state: ServiceAdditionalState {
                active_streams: 0,
                open_reports: 0,
            },
            presence,
        })
    }
}
