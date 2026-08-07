use sqlx::{Postgres, Transaction};

use crate::{
    models::{RoomId, RoomRole, UserId},
    service::PermissionWriteFence,
    Error, Result,
};

use super::{
    cleanup_member_resources_in_tx, RealtimeOutboxMemberResourceCleanupEventFactory,
    RealtimeOutboxUserLeftEventFactory, RoomService, UserLeftOutboxSnapshot,
};

struct CompleteLeftMemberRemovalRequest<'a> {
    tx: Transaction<'a, Postgres>,
    fence: &'a PermissionWriteFence,
    room_id: RoomId,
    user_id: UserId,
    removed_version: i64,
    snapshot: UserLeftOutboxSnapshot,
    outbox_event_factory: Option<RealtimeOutboxUserLeftEventFactory>,
    cleanup_outbox_event_factory: Option<RealtimeOutboxMemberResourceCleanupEventFactory>,
}

impl RoomService {
    async fn complete_left_member_removal(
        &self,
        request: CompleteLeftMemberRemovalRequest<'_>,
    ) -> Result<()> {
        let CompleteLeftMemberRemovalRequest {
            mut tx,
            fence,
            room_id,
            user_id,
            removed_version,
            snapshot,
            outbox_event_factory,
            cleanup_outbox_event_factory,
        } = request;

        let cleanup = match cleanup_member_resources_in_tx(&mut tx, &room_id, &user_id).await {
            Ok(cleanup) => cleanup,
            Err(error) => {
                self.abort_permission_write(fence).await;
                return Err(error);
            }
        };
        if let Err(error) = self
            .insert_user_left_outbox_tx(&mut tx, &snapshot, outbox_event_factory.as_ref())
            .await
        {
            self.abort_permission_write(fence).await;
            return Err(error);
        }
        if let Err(error) = self
            .insert_member_resource_cleanup_outbox_tx(
                &mut tx,
                &cleanup,
                cleanup_outbox_event_factory.as_ref(),
            )
            .await
        {
            self.abort_permission_write(fence).await;
            return Err(error);
        }
        if let Err(error) = tx.commit().await {
            self.abort_permission_write(fence).await;
            return Err(error.into());
        }
        self.finalize_committed_permission_write_best_effort(
            fence,
            &room_id,
            &user_id,
            removed_version,
            "leave_room_with_outbox",
        )
        .await;

        self.permission_service
            .invalidate_removed_member_cache(&room_id, &user_id)
            .await;
        self.finalize_member_resource_cleanup_after_commit(&room_id, &user_id, &cleanup)
            .await;

        let username = snapshot.username;
        let subscriber_count = self
            .notification_service
            .notify_user_left(&room_id, &user_id, &username);
        tracing::debug!(
            room_id = %room_id,
            user_id = %user_id,
            subscriber_count,
            "User left notification dispatched"
        );

        tracing::info!(
            room_id = %room_id,
            user_id = %user_id,
            username = %username,
            deleted_playlists = cleanup.deleted_playlist_ids.len(),
            deleted_media = cleanup.deleted_media_ids.len(),
            "User left room"
        );

        Ok(())
    }

    /// Leave a room.
    ///
    /// Lifecycle rules:
    /// - the actor must currently be an active member of the room
    /// - the creator cannot leave and must transfer ownership or delete the room
    ///
    /// **Important for callers**: This method removes the membership record
    /// and sends an in-app notification. Active room connections and cluster
    /// disconnect events are handled by callers.
    pub async fn leave_room(&self, room_id: RoomId, user_id: UserId) -> Result<()> {
        self.leave_room_with_outbox(room_id, user_id, None, None)
            .await
    }

    pub async fn leave_room_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        outbox_event_factory: Option<RealtimeOutboxUserLeftEventFactory>,
        cleanup_outbox_event_factory: Option<RealtimeOutboxMemberResourceCleanupEventFactory>,
    ) -> Result<()> {
        tracing::info!(room_id = %room_id, user_id = %user_id, "User leaving room");

        let room = self
            .room_repo
            .get_by_id(&room_id)
            .await?
            .ok_or_else(|| Error::NotFound("Room not found".to_string()))?;

        let membership = self
            .member_repo
            .get(&room_id, &user_id)
            .await?
            .ok_or_else(|| Error::Authorization("You are not a member of this room".to_string()))?;

        if room.created_by == user_id {
            return Err(Error::Authorization(
                "Room creator cannot leave the room. Transfer ownership or delete the room instead."
                    .to_string(),
            ));
        }

        if membership.role == RoomRole::Creator {
            return Err(Error::Authorization(
                "Room creator cannot leave the room. Transfer ownership or delete the room instead."
                    .to_string(),
            ));
        }

        let snapshot = self.user_left_snapshot(room_id, user_id).await?;
        let mut tx = self.pool.begin().await?;
        let Some(observed_version) = self
            .member_repo
            .active_member_version_for_update_with_executor(&room_id, &user_id, &mut tx)
            .await?
        else {
            return Err(Error::NotFound(
                synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string(),
            ));
        };
        let fence = self
            .begin_permission_write(&room_id, &user_id, observed_version)
            .await?;
        let removed_version = match self
            .member_repo
            .remove_with_version_executor(&room_id, &user_id, &mut tx)
            .await
        {
            Ok(version) => version,
            Err(error) => {
                self.abort_permission_write(&fence).await;
                return Err(error);
            }
        };
        let Some(removed_version) = removed_version else {
            self.abort_permission_write(&fence).await;
            return Err(Error::NotFound(
                synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string(),
            ));
        };
        self.complete_left_member_removal(CompleteLeftMemberRemovalRequest {
            tx,
            fence: &fence,
            room_id,
            user_id,
            removed_version,
            snapshot,
            outbox_event_factory,
            cleanup_outbox_event_factory,
        })
        .await
    }
}
