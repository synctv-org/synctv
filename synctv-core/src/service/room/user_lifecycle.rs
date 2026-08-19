use crate::{
    models::{User, UserId},
    repository::realtime_outbox::NewRealtimeOutboxEvent,
    service::{RealtimeOutboxPlaybackStateEventFactory, RoomService},
    Error, Result,
};

impl RoomService {
    /// Atomically ban a user and stop playback of media or dynamic playlists
    /// that depend on that user's room-resource access.
    pub async fn ban_user_and_reset_owned_playback_with_outbox(
        &self,
        user_id: &UserId,
        banned_by: Option<&UserId>,
        reason: Option<String>,
        playback_outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
        lifecycle_outbox_events: &[NewRealtimeOutboxEvent],
    ) -> Result<User> {
        let mut tx = self.pool.begin().await?;

        self.user_service
            .repository
            .get_by_id_for_update_with_executor(user_id, &mut *tx)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))?;
        self.user_service
            .repository
            .insert_ban_with_executor(user_id, banned_by, reason, &mut *tx)
            .await?;

        let pending_playback_reset = self
            .playback_service
            .prepare_creator_playback_reset_in_tx(
                user_id,
                playback_outbox_event_factory.as_ref(),
                &mut tx,
            )
            .await?;

        let updated = match self
            .user_service
            .repository
            .get_by_id_for_update_with_executor(user_id, &mut *tx)
            .await
        {
            Ok(Some(user)) => user,
            Ok(None) => {
                self.playback_service
                    .abort_creator_playback_reset(&pending_playback_reset)
                    .await;
                return Err(Error::NotFound(format!("User {user_id} not found")));
            }
            Err(error) => {
                self.playback_service
                    .abort_creator_playback_reset(&pending_playback_reset)
                    .await;
                return Err(error);
            }
        };

        if let Err(error) = self
            .insert_realtime_outbox_events_tx(&mut tx, lifecycle_outbox_events)
            .await
        {
            self.playback_service
                .abort_creator_playback_reset(&pending_playback_reset)
                .await;
            return Err(error);
        }

        if let Err(error) = tx.commit().await {
            self.playback_service
                .abort_creator_playback_reset(&pending_playback_reset)
                .await;
            return Err(error.into());
        }

        self.playback_service
            .finalize_creator_playback_reset_after_commit(pending_playback_reset)
            .await;
        self.user_service.notify_user_invalidation(user_id).await;

        Ok(updated)
    }
}
