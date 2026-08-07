use crate::{
    models::{PlaylistId, RoomId, RoomPermission, RoomPlaybackState, UserId},
    Error, Result,
};

use super::{
    apply_delete_entries_impact_in_tx, plan_clear_playlist_scope_in_tx, EntryDeletionImpact,
    RealtimeOutboxDeleteEntriesEventFactory, RoomService,
};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClearPlaylistResult {
    pub deleted_count: i64,
    pub deleted_playlists: usize,
    pub deleted_playlist_ids: Vec<PlaylistId>,
    pub deleted_media_ids: Vec<crate::models::MediaId>,
    pub playback_state: Option<RoomPlaybackState>,
}

impl RoomService {
    /// Clear media and child playlists in a playlist scope.
    ///
    /// The `CLEAR_MEDIA` permission check is performed inside the
    /// transaction so revocations cannot race with the clear operation.
    ///
    /// `playlist_id = None` clears the room-root scope. `Some(id)` clears the
    /// given playlist's contents while keeping the playlist itself.
    pub async fn clear_playlist(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playlist_id: Option<PlaylistId>,
    ) -> Result<ClearPlaylistResult> {
        self.clear_playlist_with_outbox(room_id, user_id, playlist_id, None)
            .await
    }

    pub async fn clear_playlist_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playlist_id: Option<PlaylistId>,
        outbox_event_factory: Option<RealtimeOutboxDeleteEntriesEventFactory>,
    ) -> Result<ClearPlaylistResult> {
        let mut tx = self.pool.begin().await?;
        self.ensure_actor_has_room_permission_now_tx(
            &mut tx,
            &room_id,
            &user_id,
            RoomPermission::CLEAR_MEDIA,
        )
        .await?;

        if let Some(playlist_id) = playlist_id {
            let exists = sqlx::query_scalar!(
                r#"SELECT EXISTS(
                    SELECT 1
                    FROM playlists
                    WHERE room_id = $1 AND id = $2
                ) AS "exists!""#,
                room_id.as_i64(),
                playlist_id.as_i64()
            )
            .fetch_one(&mut *tx)
            .await?;
            if !exists {
                return Err(Error::NotFound("Playlist not found".to_string()));
            }
        }

        let mut impact = plan_clear_playlist_scope_in_tx(&mut tx, &room_id, playlist_id).await?;
        apply_delete_entries_impact_in_tx(&mut tx, &room_id, &mut impact).await?;
        self.insert_delete_entries_outbox_events_tx(
            &mut tx,
            &impact,
            outbox_event_factory.as_ref(),
        )
        .await?;

        tx.commit().await?;

        self.finalize_entry_deletion_after_commit(&impact).await;
        if !self
            .notify_clear_playlist_after_commit(&room_id, &user_id, &impact)
            .await
        {
            return clear_playlist_result_from_impact(impact);
        }

        clear_playlist_result_from_impact(impact)
    }
}

fn clear_playlist_result_from_impact(impact: EntryDeletionImpact) -> Result<ClearPlaylistResult> {
    Ok(ClearPlaylistResult {
        deleted_count: deleted_count_to_i64(impact.deleted_media_ids.len(), "deleted media count")?,
        deleted_playlists: impact.deleted_playlist_ids.len(),
        deleted_playlist_ids: impact.deleted_playlist_ids,
        deleted_media_ids: impact.deleted_media_ids,
        playback_state: impact.playback_state,
    })
}

fn deleted_count_to_i64(value: usize, field: &'static str) -> Result<i64> {
    i64::try_from(value).map_err(|_| Error::Internal(format!("{field} exceeds i64::MAX")))
}
