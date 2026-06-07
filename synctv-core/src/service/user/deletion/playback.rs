use crate::{
    cache::{CacheDomain, VersionFenceReservation},
    models::RoomId,
    Result,
};

use super::{UserDeletedRoomImpact, UserService};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PendingPlaybackResetFence {
    pub(super) room_id: RoomId,
    pub(super) reservation: Option<VersionFenceReservation>,
}

impl UserService {
    fn playback_domain(room_id: &RoomId) -> CacheDomain {
        CacheDomain::Playback { room_id: *room_id }
    }

    pub(super) async fn begin_playback_reset_write(
        &self,
        room_id: &RoomId,
        db_version: i64,
    ) -> Result<Option<VersionFenceReservation>> {
        self.consistency
            .begin_observed_write(&Self::playback_domain(room_id), db_version)
            .await
    }

    pub(super) async fn abort_playback_reset_fence(
        &self,
        room_id: &RoomId,
        reservation: Option<&VersionFenceReservation>,
    ) {
        self.consistency
            .abort_reserved_write(&Self::playback_domain(room_id), reservation)
            .await;
    }

    pub(super) async fn abort_playback_reset_fence_option(
        &self,
        fence: Option<&PendingPlaybackResetFence>,
    ) {
        let Some(fence) = fence else {
            return;
        };
        self.abort_playback_reset_fence(&fence.room_id, fence.reservation.as_ref())
            .await;
    }

    pub(super) async fn commit_playback_reset_fences(
        &self,
        impacts: &[UserDeletedRoomImpact],
    ) -> Result<()> {
        let mut first_error = None;
        for impact in impacts {
            let (Some(state), Some(fence)) = (&impact.playback_state, &impact.playback_fence)
            else {
                continue;
            };

            if let Err(error) = self
                .consistency
                .commit_reserved_write(
                    &Self::playback_domain(&fence.room_id),
                    fence.reservation.as_ref(),
                    state.version,
                )
                .await
            {
                tracing::warn!(
                    error = %error,
                    room_id = %fence.room_id,
                    version = state.version,
                    "Failed to finalize playback reset fence after committed user deletion"
                );
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }

        if let Some(error) = first_error {
            Err(error)
        } else {
            Ok(())
        }
    }

    pub(super) async fn abort_playback_reset_fences(&self, impacts: &[UserDeletedRoomImpact]) {
        for impact in impacts {
            self.abort_playback_reset_fence_option(impact.playback_fence.as_ref())
                .await;
        }
    }
}
