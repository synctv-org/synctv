use super::{AdminApiImpl, ApiError};

impl AdminApiImpl {
    pub async fn get_service_state(
        &self,
        _req: synctv_proto::admin::GetServiceStateRequest,
    ) -> Result<synctv_proto::admin::GetServiceStateResponse, ApiError> {
        let service = synctv_core::service::ServiceStateService::new(
            synctv_core::service::ServiceStateServiceDependencies {
                system_stats_service: self.system_stats_service.clone(),
                provider_instance_manager: self.provider_instance_manager.clone(),
                room_service: self.room_service.clone(),
                presence_service: self.presence_service.clone(),
            },
        );
        let state = service.get_service_state().await.map_err(ApiError::from)?;

        let presence = crate::impls::client::convert::presence_overview_to_proto(&state.presence)?;

        Ok(synctv_proto::admin::GetServiceStateResponse {
            total_users: state.total_users,
            active_users: state.active_users,
            banned_users: state.banned_users,
            total_rooms: state.total_rooms,
            active_rooms: state.active_rooms,
            banned_rooms: state.banned_rooms,
            total_media: state.total_media,
            provider_instances: state.provider_instances,
            additional_state: Some(synctv_proto::admin::ServiceAdditionalState {
                active_streams: state.additional_state.active_streams,
                open_reports: state.additional_state.open_reports,
            }),
            presence: Some(presence),
        })
    }
}
