use std::sync::Arc;

use crate::{
    models::{AuditAction, AuditDetails, AuditTargetType, RoomId, UserId},
    Error, Result,
};

use super::{
    permission_fence_guard::PermissionFenceGuard, soft_delete_room_and_cleanup_in_tx,
    AuthorizedAdminActor, NewRealtimeOutboxEvent, RoomService,
};

impl RoomService {
    /// Maximum number of items allowed in a batch operation.
    pub const BATCH_SIZE_LIMIT: usize = 100;

    /// Delete a room from the admin plane.
    pub async fn admin_delete_room(&self, room_id: &RoomId, admin_user_id: &UserId) -> Result<()> {
        let actor = self.load_authorized_admin_actor(admin_user_id).await?;
        self.admin_delete_room_as(room_id, &actor).await
    }

    pub async fn admin_delete_room_as(
        &self,
        room_id: &RoomId,
        actor: &AuthorizedAdminActor,
    ) -> Result<()> {
        self.admin_delete_room_as_with_outbox(room_id, actor, None)
            .await
    }

    pub async fn admin_delete_room_as_with_outbox(
        &self,
        room_id: &RoomId,
        actor: &AuthorizedAdminActor,
        outbox_event: Option<NewRealtimeOutboxEvent>,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let guard = PermissionFenceGuard::reserve(Arc::new(self.clone()), room_id, &mut tx).await?;

        let impact = match soft_delete_room_and_cleanup_in_tx(&mut tx, room_id, "admin").await {
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
        if let Err(error) = tx.commit().await {
            guard.abort().await;
            return Err(error.into());
        }

        if let Err(error) = guard.commit(&impact.removed_members).await {
            tracing::warn!(
                error = %error,
                room_id = %room_id,
                "Failed to finalize one or more admin room deletion permission fences after DB commit"
            );
        }

        self.invalidate_room_caches(room_id).await;
        self.invalidate_removed_room_member_permission_caches(&impact.removed_members)
            .await;

        let subscriber_count = self.notification_service.notify_room_deleted(room_id);
        super::outbox::log_if_no_local_subscribers(subscriber_count, room_id, "Room deleted");

        crate::metrics::application::ROOMS_ACTIVE.dec();

        self.write_audit_event(
            actor.user_id(),
            &actor.user_id().to_string(),
            AuditAction::RoomDeleted,
            AuditTargetType::Room,
            Some(room_id.to_string()),
            AuditDetails {
                reason: Some("Room deleted by admin".to_string()),
                playlists_deleted: Some(impact.deleted_playlist_ids.len()),
                media_deleted: Some(impact.deleted_media_ids.len()),
                members_deleted: Some(impact.members_deleted),
                settings_deleted: Some(impact.settings_deleted),
                chat_deleted: Some(impact.chat_deleted),
                ..Default::default()
            },
        )
        .await?;

        Ok(())
    }

    /// Delete an orphaned room whose creator has been deleted or banned.
    pub async fn admin_delete_orphaned_room(
        &self,
        room_id: &RoomId,
        admin_user_id: &UserId,
    ) -> Result<()> {
        let actor = self.load_authorized_admin_actor(admin_user_id).await?;
        self.admin_delete_orphaned_room_as(room_id, &actor).await
    }

    pub async fn admin_delete_orphaned_room_as(
        &self,
        room_id: &RoomId,
        actor: &AuthorizedAdminActor,
    ) -> Result<()> {
        let admin_user_id = actor.user_id();
        tracing::info!(room_id = %room_id, admin_user_id = %admin_user_id, "Admin deleting orphaned room");

        let room = self
            .room_repo
            .get_by_id(room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        if room.deleted_at.is_some() {
            return Err(Error::InvalidInput("Room is already deleted".to_string()));
        }

        let creator_orphaned = sqlx::query_scalar!(
            "SELECT NOT EXISTS (
                SELECT 1
                FROM users u
                WHERE u.id = $1
                  AND u.deleted_at IS NULL
                  AND NOT EXISTS (
                      SELECT 1 FROM user_bans ub
                      WHERE ub.user_id = u.id
                        AND ub.revoked_at IS NULL
                        AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
                  )
            ) AS \"orphaned!\"",
            room.created_by as UserId,
        )
        .fetch_one(&self.pool)
        .await?;

        if !creator_orphaned {
            return Err(Error::InvalidInput(
                "Room is not orphaned: creator still exists and is active. Use delete_room instead.".to_string()
            ));
        }

        tracing::info!(
            room_id = %room_id,
            creator_id = %room.created_by,
            "Confirmed room is orphaned, proceeding with admin deletion"
        );

        let mut tx = self.pool.begin().await?;
        let guard = PermissionFenceGuard::reserve(Arc::new(self.clone()), room_id, &mut tx).await?;

        let impact = match soft_delete_room_and_cleanup_in_tx(&mut tx, room_id, "admin").await {
            Ok(impact) => impact,
            Err(error) => {
                guard.abort().await;
                return Err(error);
            }
        };
        if let Err(error) = tx.commit().await {
            guard.abort().await;
            return Err(error.into());
        }

        if let Err(error) = guard.commit(&impact.removed_members).await {
            tracing::warn!(
                error = %error,
                room_id = %room_id,
                "Failed to finalize one or more orphaned room deletion permission fences after DB commit"
            );
        }

        self.invalidate_room_caches(room_id).await;
        self.invalidate_removed_room_member_permission_caches(&impact.removed_members)
            .await;

        let subscriber_count = self.notification_service.notify_room_deleted(room_id);
        super::outbox::log_if_no_local_subscribers(subscriber_count, room_id, "Room deleted");

        crate::metrics::application::ROOMS_ACTIVE.dec();

        self.write_audit_event(
            actor.user_id(),
            &actor.user_id().to_string(),
            AuditAction::RoomDeleted,
            AuditTargetType::Room,
            Some(room_id.to_string()),
            AuditDetails {
                reason: Some("Orphaned room deleted by admin (creator deleted/banned)".to_string()),
                creator_id: Some(room.created_by.to_string()),
                playlists_deleted: Some(impact.deleted_playlist_ids.len()),
                media_deleted: Some(impact.deleted_media_ids.len()),
                members_deleted: Some(impact.members_deleted),
                settings_deleted: Some(impact.settings_deleted),
                chat_deleted: Some(impact.chat_deleted),
                ..Default::default()
            },
        )
        .await?;

        tracing::info!(room_id = %room_id, "Orphaned room deleted successfully");

        Ok(())
    }

    /// Batch delete multiple rooms.
    pub async fn batch_delete_rooms(
        &self,
        room_ids: &[RoomId],
        admin_user_id: &UserId,
    ) -> crate::Result<Vec<(RoomId, crate::Result<()>)>> {
        if room_ids.is_empty() {
            return Err(Error::InvalidInput("room_ids cannot be empty".to_string()));
        }
        if room_ids.len() > Self::BATCH_SIZE_LIMIT {
            return Err(Error::InvalidInput(format!(
                "Batch size {} exceeds limit of {}",
                room_ids.len(),
                Self::BATCH_SIZE_LIMIT
            )));
        }

        let mut results = Vec::with_capacity(room_ids.len());

        for room_id in room_ids {
            let result = self.admin_delete_room(room_id, admin_user_id).await;
            results.push((*room_id, result));
        }

        Ok(results)
    }
}
