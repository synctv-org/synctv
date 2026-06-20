use chrono::Duration;

use crate::{
    models::{RoomId, RoomRole},
    service::RoomService,
    Error, Result,
};

impl RoomService {
    async fn remove_guest_role_members(&self, room_id: &RoomId) -> Result<()> {
        let members = self.member_service.list_members(room_id).await?;

        for member in members {
            if member.role == RoomRole::Guest {
                self.member_service
                    .delete_active_membership(*room_id, member.user_id)
                    .await?;
            }
        }

        Ok(())
    }

    pub(super) async fn revoke_all_guest_access(
        &self,
        room_id: &RoomId,
        reason: crate::service::notification::GuestKickReason,
    ) -> Result<()> {
        self.remove_guest_role_members(room_id).await?;
        self.bump_room_guest_version(room_id).await?;
        let subscriber_count = self.notification_service.kick_all_guests(room_id, reason);
        super::outbox::log_if_no_local_subscribers(subscriber_count, room_id, "Guest kick");
        Ok(())
    }

    async fn bump_room_guest_version(&self, room_id: &RoomId) -> Result<i64> {
        let current = self.get_room_guest_version(room_id).await?;
        let next = current
            .checked_add(1)
            .ok_or_else(|| Error::Internal("Room guest version overflowed".to_string()))?;

        let key = self
            .user_service
            .key_builder()
            .room_guest_version(&room_id.to_string());
        self.user_service
            .token_blacklist_store()
            .set_version(&key, next, Self::room_guest_version_ttl_secs())
            .await?;

        Ok(next)
    }

    const fn room_guest_version_ttl_secs() -> u64 {
        Duration::hours(4).num_seconds().cast_unsigned()
    }
}
