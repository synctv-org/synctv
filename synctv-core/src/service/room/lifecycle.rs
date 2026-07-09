use std::sync::Arc;

use crate::{
    models::{AuditAction, AuditDetails, AuditTargetType, RoomId, RoomPermission, UserId},
    Error, Result,
};

use super::{
    permission_fence_guard::PermissionFenceGuard, soft_delete_room_and_cleanup_in_tx,
    NewRealtimeOutboxEvent, RoomService,
};

impl RoomService {
    /// Soft-delete a room.
    ///
    /// Sets the `deleted_at` timestamp on the room row. The room and its related
    /// data (members, playlists, media, chat messages, settings, playback state)
    /// remain in the database until the periodic `CleanupService` permanently
    /// purges rows whose `deleted_at` exceeds the configured retention period
    /// (default: 90 days). Permanent purge uses the same explicit cleanup path
    /// as normal room deletion before removing the room row itself.
    ///
    /// **Soft-delete lifecycle (optimized):**
    /// 1. This method sets `rooms.deleted_at = NOW()` (room becomes invisible to queries)
    /// 2. IMMEDIATELY deletes non-critical related data to free storage:
    /// - playlists and nested media via explicit subtree cleanup
    /// - `room_members`
    /// - `room_settings`
    /// - `room_playback_state`
    /// - `chat_messages`
    /// 3. Preserves only the room row (for audit) and `audit_logs` entries
    /// 4. `CleanupService::purge_soft_deleted_rooms()` eventually purges the room row
    ///    after `room_soft_delete_retention_days` (default: 90 days)
    ///
    /// Authorization model:
    /// - room creator can delete their own room
    /// - room members with DELETE_ROOM can delete the room
    /// - global admin/root can delete any room
    pub async fn delete_room(&self, room_id: RoomId, user_id: UserId) -> Result<()> {
        self.delete_room_with_outbox(room_id, user_id, None).await
    }

    pub async fn delete_room_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        outbox_event: Option<NewRealtimeOutboxEvent>,
    ) -> Result<()> {
        tracing::info!(room_id = %room_id, user_id = %user_id, "Soft-deleting room");

        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found or already deleted".to_string()))?;

        let actor = self.user_service.get_user(&user_id).await?;
        let is_global_admin = actor.role.is_admin_or_above();
        let is_creator = room.created_by == user_id;

        // Check authorization: creator, global admin, or member with delete permission
        if !is_creator && !is_global_admin {
            if self.member_repo.get(&room_id, &user_id).await?.is_none() {
                return Err(Error::Authorization(
                    "You are not a member of this room".to_string(),
                ));
            }
            self.permission_service
                .check_permission_no_cache(&room_id, &user_id, RoomPermission::DELETE_ROOM)
                .await?;
        }

        let mut tx = self.pool.begin().await?;
        let guard =
            PermissionFenceGuard::reserve(Arc::new(self.clone()), &room_id, &mut tx).await?;

        let impact = match soft_delete_room_and_cleanup_in_tx(&mut tx, &room_id).await {
            Ok(impact) => impact,
            Err(error) => {
                guard.abort().await;
                return Err(error);
            }
        };
        if let Err(error) = self
            .insert_realtime_outbox_tx(&mut tx, outbox_event.as_ref())
            .await
        {
            guard.abort().await;
            return Err(error);
        }

        // Commit transaction - all or nothing
        if let Err(error) = tx.commit().await {
            guard.abort().await;
            return Err(error.into());
        }

        if let Err(error) = guard.commit(&impact.removed_members).await {
            tracing::warn!(
                error = %error,
                room_id = %room_id,
                "Failed to finalize one or more room deletion permission fences after DB commit"
            );
        }

        self.invalidate_room_caches(&room_id).await;
        self.invalidate_removed_room_member_permission_caches(&impact.removed_members)
            .await;

        let subscriber_count = self.notification_service.notify_room_deleted(&room_id);
        super::outbox::log_if_no_local_subscribers(subscriber_count, &room_id, "Room deleted");

        tracing::info!(
            room_id = %room_id,
            user_id = %user_id,
            is_creator,
            is_global_admin,
            playlists_deleted = impact.deleted_playlist_ids.len(),
            media_deleted = impact.deleted_media_ids.len(),
            members_deleted = impact.members_deleted,
            settings_deleted = impact.settings_deleted,
            chat_deleted = impact.chat_deleted,
            "Room soft-deleted with immediate cleanup of related data (room row preserved for audit, will be purged by CleanupService after retention period)"
        );

        // Track room metrics
        crate::metrics::application::ROOMS_ACTIVE.dec();

        // Audit log (preserved - not deleted with room_creation data)
        self.audit_log(
            &user_id,
            &actor.username,
            AuditAction::RoomDeleted,
            AuditTargetType::Room,
            Some(room_id.to_string()),
            AuditDetails {
                reason: Some("Room deleted by user".to_string()),
                playlists_deleted: Some(impact.deleted_playlist_ids.len()),
                media_deleted: Some(impact.deleted_media_ids.len()),
                members_deleted: Some(impact.members_deleted),
                settings_deleted: Some(impact.settings_deleted),
                chat_deleted: Some(impact.chat_deleted),
                ..Default::default()
            },
        )
        .await;

        Ok(())
    }
}
