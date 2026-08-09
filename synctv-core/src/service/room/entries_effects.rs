use crate::models::{RoomId, UserId};

use super::{outbox::log_if_no_local_subscribers, EntryDeletionImpact, RoomService};

impl RoomService {
    pub(super) async fn finalize_entry_deletion_after_commit(&self, impact: &EntryDeletionImpact) {
        if let Some(state) = impact.playback_state.clone() {
            self.broadcast_playback_reset_after_entry_deletion(state)
                .await;
        }
        // Soft-deleted media still owns its file references during the
        // recovery window. Hard-purge cleanup marks those references expired
        // after the database rows are permanently removed.
    }

    pub(super) async fn notify_user_entry_deletion_after_commit(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        impact: &EntryDeletionImpact,
    ) -> bool {
        if impact.deleted_media_ids.is_empty() && impact.deleted_playlist_ids.is_empty() {
            return true;
        }

        let actor_username = match self.resolve_actor_username(user_id).await {
            Ok(username) => username,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    room_id = %room_id,
                    user_id = %user_id,
                    "Skipped delete entries notifications because actor username lookup failed"
                );
                return false;
            }
        };
        self.notify_entry_deletion_after_commit(
            room_id,
            Some(user_id),
            &actor_username,
            impact,
            "Media removed",
            "Playlist deleted",
        );
        true
    }

    pub(super) fn notify_admin_entry_deletion_after_commit(
        &self,
        room_id: &RoomId,
        admin_user_id: &UserId,
        admin_username: &str,
        impact: &EntryDeletionImpact,
    ) {
        if impact.deleted_media_ids.is_empty() && impact.deleted_playlist_ids.is_empty() {
            return;
        }

        self.notify_entry_deletion_after_commit(
            room_id,
            Some(admin_user_id),
            admin_username,
            impact,
            "Media removed",
            "Playlist deleted",
        );
    }

    pub(super) async fn notify_clear_playlist_after_commit(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        impact: &EntryDeletionImpact,
    ) -> bool {
        let actor_username = match self.resolve_actor_username(user_id).await {
            Ok(username) => username,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    room_id = %room_id,
                    user_id = %user_id,
                    "Skipped clear playlist notifications because actor username lookup failed"
                );
                return false;
            }
        };
        self.notify_entry_deletion_after_commit(
            room_id,
            Some(user_id),
            &actor_username,
            impact,
            "Media removed event after clear_playlist",
            "Playlist deleted after clear_playlist",
        );
        true
    }

    fn notify_entry_deletion_after_commit(
        &self,
        room_id: &RoomId,
        actor_user_id: Option<&UserId>,
        actor_username: &str,
        impact: &EntryDeletionImpact,
        media_label: &'static str,
        playlist_label: &'static str,
    ) {
        for media_id in &impact.deleted_media_ids {
            let subscriber_count = self.notification_service.notify_media_removed(
                room_id,
                actor_user_id,
                actor_username,
                *media_id,
            );
            log_if_no_local_subscribers(subscriber_count, room_id, media_label);
        }
        for playlist_id in &impact.deleted_playlist_ids {
            let subscriber_count = self.notification_service.notify_playlist_deleted(
                room_id,
                actor_user_id,
                actor_username,
                *playlist_id,
            );
            log_if_no_local_subscribers(subscriber_count, room_id, playlist_label);
        }
    }
}
