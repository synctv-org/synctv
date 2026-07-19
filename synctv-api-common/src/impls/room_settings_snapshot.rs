use std::sync::Arc;

use synctv_core::{models::RoomId, service::RoomService};

use crate::impls::ApiError;

#[derive(Debug, Clone)]
pub struct RoomSettingsSnapshot {
    pub settings: synctv_core::models::RoomSettings,
    pub version: i64,
}

#[async_trait::async_trait]
pub trait RoomSettingsSnapshotService: Send + Sync {
    async fn get_room_settings_snapshot(
        &self,
        room_id: &RoomId,
    ) -> Result<RoomSettingsSnapshot, ApiError>;
}

#[derive(Clone)]
pub struct DefaultRoomSettingsSnapshotService {
    room_service: Arc<RoomService>,
}

impl DefaultRoomSettingsSnapshotService {
    #[must_use]
    pub fn new(room_service: Arc<RoomService>) -> Self {
        Self { room_service }
    }
}

#[async_trait::async_trait]
impl RoomSettingsSnapshotService for DefaultRoomSettingsSnapshotService {
    async fn get_room_settings_snapshot(
        &self,
        room_id: &RoomId,
    ) -> Result<RoomSettingsSnapshot, ApiError> {
        let (settings, version) = self
            .room_service
            .get_room_settings_with_version(room_id)
            .await
            .map_err(ApiError::from)?;
        Ok(RoomSettingsSnapshot { settings, version })
    }
}

#[must_use]
pub fn default_room_settings_snapshot_service(
    room_service: Arc<RoomService>,
) -> Arc<dyn RoomSettingsSnapshotService> {
    Arc::new(DefaultRoomSettingsSnapshotService::new(room_service))
}
