use crate::{
    models::{MediaId, RoomId, RoomPlaybackState, UserId},
    repository::room_member::RemovedRoomMember,
    service::{room::MemberResourceCleanupResult, RoomService},
};

impl RoomService {
    pub(super) async fn invalidate_removed_room_member_permission_caches(
        &self,
        removed_members: &[RemovedRoomMember],
    ) {
        for member in removed_members {
            self.permission_service
                .invalidate_removed_member_cache(&member.room_id, &member.user_id)
                .await;
        }
    }

    pub(super) async fn broadcast_playback_reset_after_entry_deletion(
        &self,
        state: RoomPlaybackState,
    ) {
        self.playback_service
            .broadcast_playback_reset_after_force_delete(state)
            .await;
    }

    /// Invalidate room cache locally and broadcast to other replicas.
    pub(super) async fn notify_room_invalidation(&self, room_id: &RoomId) {
        if let Some(ref service) = self.cache_invalidation {
            if let Err(e) = service.invalidate_and_broadcast_room(room_id).await {
                tracing::warn!(
                    error = %e,
                    room_id = %room_id,
                    "Failed to broadcast room cache invalidation"
                );
            }
        }
    }

    /// Broadcast room settings cache invalidation to other replicas.
    pub(super) async fn notify_room_settings_invalidation(&self, room_id: &RoomId) {
        if let Some(ref service) = self.cache_invalidation {
            if let Err(e) = service
                .invalidate_and_broadcast_room_settings(room_id)
                .await
            {
                tracing::warn!(
                    error = %e,
                    room_id = %room_id,
                    "Failed to broadcast room settings cache invalidation"
                );
            }
        }
    }

    /// Invalidate all caches associated with a room_creation after a transaction commits.
    pub(super) async fn invalidate_room_caches(&self, room_id: &RoomId) {
        self.notify_room_invalidation(room_id).await;
        self.permission_service.invalidate_room_cache(room_id).await;
        self.playback_service
            .invalidate_playback_cache(room_id)
            .await;
    }

    /// Run best-effort post-commit side effects after a room_creation has already been
    /// deleted transactionally elsewhere.
    pub async fn finalize_deleted_room_after_commit(&self, room_id: &RoomId) {
        self.invalidate_room_caches(room_id).await;
        let subscriber_count = self.notification_service.notify_room_deleted(room_id);
        if subscriber_count == 0 {
            tracing::debug!(
                room_id = %room_id,
                "Room deleted event after commit had no local subscribers"
            );
        }
    }

    /// Run best-effort post-commit side effects after a room_creation became unusable
    /// because its creator account is no longer active.
    pub async fn finalize_room_owner_inactive_after_commit(&self, room_id: &RoomId) {
        self.invalidate_room_caches(room_id).await;
    }

    /// Run best-effort post-commit side effects after entry deletions have
    /// already committed.
    pub async fn finalize_entry_deletions_after_commit(
        &self,
        room_id: &RoomId,
        deleted_media_ids: &[MediaId],
        playback_state: Option<&RoomPlaybackState>,
    ) {
        self.invalidate_room_caches(room_id).await;

        if let Some(state) = playback_state {
            self.broadcast_playback_reset_after_entry_deletion(state.clone())
                .await;
        }

        for media_id in deleted_media_ids {
            let subscriber_count = self
                .notification_service
                .notify_media_removed(room_id, None, "", *media_id);
            if subscriber_count == 0 {
                tracing::debug!(
                    room_id = %room_id,
                    media_id = %media_id,
                    "Media removed event after user cleanup had no local subscribers"
                );
            }
        }
    }

    pub(super) async fn finalize_member_resource_cleanup_after_commit(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        cleanup: &MemberResourceCleanupResult,
    ) {
        if cleanup.is_empty() {
            return;
        }

        self.finalize_entry_deletions_after_commit(
            room_id,
            &cleanup.deleted_media_ids,
            cleanup.playback_state.as_ref(),
        )
        .await;

        let username = match self.resolve_actor_username(user_id).await {
            Ok(username) => username,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    room_id = %room_id,
                    user_id = %user_id,
                    "Skipped member resource cleanup notifications because actor username lookup failed"
                );
                return;
            }
        };
        for playlist_id in &cleanup.deleted_playlist_ids {
            let subscriber_count = self.notification_service.notify_playlist_deleted(
                room_id,
                Some(user_id),
                &username,
                *playlist_id,
            );
            if subscriber_count == 0 {
                tracing::debug!(
                    room_id = %room_id,
                    playlist_id = %playlist_id,
                    "Playlist deleted event after member resource cleanup had no local subscribers"
                );
            }
        }
    }
}
