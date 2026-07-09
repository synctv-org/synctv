use crate::{
    cache::{CacheDomain, ConsistencyCoordinator, VersionFenceReservation},
    models::RoomId,
    Result,
};

use super::RoomService;

impl RoomService {
    pub(super) async fn begin_room_settings_write_with(
        consistency: &ConsistencyCoordinator,
        room_id: &RoomId,
        db_version: i64,
    ) -> Result<Option<VersionFenceReservation>> {
        let domain = CacheDomain::RoomSettings { room_id: *room_id };
        consistency.begin_observed_write(&domain, db_version).await
    }

    pub(super) async fn commit_room_settings_write_with(
        consistency: &ConsistencyCoordinator,
        domain: &CacheDomain,
        reservation: Option<&VersionFenceReservation>,
        version: i64,
    ) -> Result<()> {
        consistency
            .commit_reserved_write(domain, reservation, version)
            .await?;
        Ok(())
    }

    pub(super) async fn abort_room_settings_write_with(
        consistency: &ConsistencyCoordinator,
        domain: &CacheDomain,
        reservation: Option<&VersionFenceReservation>,
    ) {
        consistency.abort_reserved_write(domain, reservation).await;
    }

    pub(super) async fn begin_room_settings_write(
        &self,
        room_id: &RoomId,
        db_version: i64,
    ) -> Result<Option<VersionFenceReservation>> {
        Self::begin_room_settings_write_with(&self.consistency, room_id, db_version).await
    }

    pub(super) async fn commit_room_settings_write(
        &self,
        domain: &CacheDomain,
        reservation: Option<&VersionFenceReservation>,
        version: i64,
    ) -> Result<()> {
        Self::commit_room_settings_write_with(&self.consistency, domain, reservation, version).await
    }

    pub(super) async fn finalize_committed_room_settings_write_best_effort(
        &self,
        domain: &CacheDomain,
        reservation: Option<&VersionFenceReservation>,
        version: i64,
        operation: &'static str,
    ) {
        if let Err(error) = self
            .commit_room_settings_write(domain, reservation, version)
            .await
        {
            tracing::warn!(
                error = %error,
                domain = %domain,
                version,
                operation,
                "Failed to finalize room settings fence after committed DB write"
            );
        }
    }

    pub(super) async fn abort_room_settings_write(
        &self,
        domain: &CacheDomain,
        reservation: Option<&VersionFenceReservation>,
    ) {
        Self::abort_room_settings_write_with(&self.consistency, domain, reservation).await;
    }
}
