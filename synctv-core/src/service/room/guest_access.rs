use chrono::Duration;

use crate::{
    models::{RoomId, RoomPermissionSet, RoomRole},
    service::{RoomService, RuntimeSettingsStore},
    Error, Result,
};

impl RoomService {
    /// Check if guests are allowed to access a room_creation
    pub async fn check_guest_allowed(
        &self,
        room_id: &RoomId,
        runtime_settings_store: Option<&RuntimeSettingsStore>,
    ) -> Result<()> {
        if let Some(registry) = runtime_settings_store {
            let enable_guest = registry.user.enable_guest.get()?;
            if !enable_guest {
                tracing::debug!(room_id = %room_id, "Guest access denied: global guest mode disabled");
                return Err(Error::Authorization(
                    "Guest mode is disabled globally".to_string(),
                ));
            }
        } else {
            tracing::debug!(room_id = %room_id, "Guest access denied: runtime settings store unavailable (fail-closed)");
            return Err(Error::Authorization(
                "Guest mode is not available".to_string(),
            ));
        }

        let room_settings = self.room_settings_repo.get(room_id).await?;
        if !room_settings.allow_guest_join.0 {
            tracing::debug!(room_id = %room_id, "Guest access denied: room guest mode disabled");
            return Err(Error::Authorization(
                "Guest access is not allowed in this room".to_string(),
            ));
        }

        let password_enabled = self
            .room_password_repo
            .get_state(room_id)
            .await?
            .is_some_and(|state| state.enabled);
        if password_enabled {
            tracing::debug!(room_id = %room_id, "Guest access denied: room has password");
            return Err(Error::Authorization(
                "Guests cannot join password-protected rooms. Please create an account and join as a member.".to_string(),
            ));
        }

        tracing::debug!(room_id = %room_id, "Guest access allowed");
        Ok(())
    }

    /// Return the effective room permissions for guests.
    pub async fn get_guest_permissions(&self, room_id: &RoomId) -> Result<RoomPermissionSet> {
        let settings = self.get_room_settings(room_id).await?;
        Ok(self
            .permission_service
            .effective_permission_calculator()
            .role_default(&RoomRole::Guest, &settings))
    }

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
        reason: crate::service::GuestKickReason,
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
