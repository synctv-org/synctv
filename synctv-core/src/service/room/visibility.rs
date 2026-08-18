use crate::{
    models::{Room, RoomId, RoomPermission, UserId},
    service::GuestKickReason,
    Error, Result,
};

use super::RoomService;

impl RoomService {
    pub async fn update_room_visibility(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        is_public: bool,
    ) -> Result<Room> {
        self.permission_service
            .check_permission_no_cache(room_id, user_id, RoomPermission::MANAGE_ROOM_SETTINGS)
            .await?;

        self.update_room_visibility_unchecked(room_id, is_public)
            .await
    }

    /// Update visibility from an already authenticated management/system plane.
    pub async fn admin_update_room_visibility(
        &self,
        room_id: &RoomId,
        is_public: bool,
    ) -> Result<Room> {
        self.update_room_visibility_unchecked(room_id, is_public)
            .await
    }

    async fn update_room_visibility_unchecked(
        &self,
        room_id: &RoomId,
        is_public: bool,
    ) -> Result<Room> {
        let mut tx = self.pool.begin().await?;
        let mut room = self
            .room_repo
            .get_by_id_for_update_with_executor(room_id, &mut *tx)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;
        let was_public = room.is_public;

        if was_public == is_public {
            tx.commit().await?;
            return Ok(room);
        }

        let old_version = room.version;
        room.is_public = is_public;
        let category_id = room.category.as_ref().map(|category| category.id);
        let updated = self
            .room_repo
            .update_with_taxonomy_executor(&room, category_id, old_version, &mut *tx)
            .await?;
        tx.commit().await?;

        self.notify_room_invalidation(room_id).await;
        if was_public && !is_public {
            if let Err(error) = self
                .revoke_all_guest_access(room_id, GuestKickReason::RoomMadePrivate)
                .await
            {
                tracing::warn!(
                    room_id = %room_id,
                    error = %error,
                    "Failed to revoke guest access after room visibility change"
                );
            }
        }

        Ok(updated)
    }
}
