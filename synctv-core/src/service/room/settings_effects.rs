use crate::{
    models::{RoomId, RoomSettings, UserId},
    Result,
};

use super::RoomService;

impl RoomService {
    pub(super) async fn finalize_room_settings_update(
        &self,
        room_id: &RoomId,
        previous_settings: &RoomSettings,
        updated_settings: &RoomSettings,
        version: i64,
        actor_user_id: Option<&UserId>,
        actor_username: &str,
    ) -> Result<crate::cache::RoomSettingsSnapshot> {
        self.run_post_apply_hooks_for_settings_update(room_id, previous_settings, updated_settings)
            .await;
        self.room_settings_service.invalidate_local(room_id).await;
        self.permission_service.invalidate_room_cache(room_id).await;
        self.notify_room_invalidation(room_id).await;
        self.notify_room_settings_invalidation(room_id).await;

        let subscriber_count = self.notification_service.notify_settings_updated(
            room_id,
            actor_user_id,
            actor_username,
            updated_settings.clone(),
            version,
        );
        super::outbox::log_if_no_local_subscribers(
            subscriber_count,
            room_id,
            "Room settings updated",
        );

        Ok(crate::cache::RoomSettingsSnapshot {
            settings: updated_settings.clone(),
            version,
        })
    }

    async fn run_post_apply_hooks_for_settings_update(
        &self,
        room_id: &RoomId,
        previous_settings: &RoomSettings,
        updated_settings: &RoomSettings,
    ) {
        use crate::service::GuestKickReason;

        let guest_kick_reason =
            if previous_settings.allow_guest_join.0 && !updated_settings.allow_guest_join.0 {
                Some(GuestKickReason::RoomGuestModeDisabled)
            } else {
                None
            };

        if let Some(reason) = guest_kick_reason {
            if let Err(e) = self.revoke_all_guest_access(room_id, reason).await {
                tracing::warn!(
                    room_id = %room_id,
                    error = %e,
                    "Failed to revoke guest access after settings change"
                );
            }
        }
    }
}
