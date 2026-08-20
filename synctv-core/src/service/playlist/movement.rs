use crate::{
    models::{Playlist, PlaylistId, RoomId, UserId},
    Error, Result,
};

use super::{PlaylistService, RealtimeOutboxPlaylistEventFactory};

#[derive(Debug, Clone)]
pub struct MovePlaylistRequest {
    pub playlist_id: PlaylistId,
    pub before_playlist_id: Option<PlaylistId>,
    pub after_playlist_id: Option<PlaylistId>,
}

impl PlaylistService {
    pub async fn move_playlist(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: MovePlaylistRequest,
    ) -> Result<Playlist> {
        self.move_playlist_with_outbox(room_id, user_id, request, None)
            .await
    }

    pub async fn move_playlist_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: MovePlaylistRequest,
        outbox_event_factory: Option<RealtimeOutboxPlaylistEventFactory>,
    ) -> Result<Playlist> {
        self.move_playlist_internal(room_id, user_id, request, false, outbox_event_factory)
            .await
    }

    pub async fn admin_move_playlist_with_outbox(
        &self,
        room_id: RoomId,
        actor_user_id: UserId,
        request: MovePlaylistRequest,
        outbox_event_factory: Option<RealtimeOutboxPlaylistEventFactory>,
    ) -> Result<Playlist> {
        self.move_playlist_internal(room_id, actor_user_id, request, true, outbox_event_factory)
            .await
    }

    async fn move_playlist_internal(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: MovePlaylistRequest,
        bypass_room_permissions: bool,
        outbox_event_factory: Option<RealtimeOutboxPlaylistEventFactory>,
    ) -> Result<Playlist> {
        if !bypass_room_permissions {
            self.permission_service
                .check_permission(
                    &room_id,
                    &user_id,
                    crate::models::RoomPermission::REORDER_MEDIA,
                )
                .await?;
        }

        let has_before = request.before_playlist_id.is_some();
        let has_after = request.after_playlist_id.is_some();
        if has_before == has_after {
            return Err(Error::InvalidInput(
                "Exactly one of before_playlist_id or after_playlist_id must be set".to_string(),
            ));
        }

        let mut tx = self.playlist_repo.pool().begin().await?;
        let moved = self
            .playlist_repo
            .move_with_tx(
                &room_id,
                &request.playlist_id,
                request.before_playlist_id.as_ref(),
                request.after_playlist_id.as_ref(),
                &mut tx,
            )
            .await?;

        self.insert_playlist_outbox_tx(&mut tx, &moved, outbox_event_factory.as_ref())
            .await?;
        tx.commit().await?;
        Ok(moved)
    }
}
