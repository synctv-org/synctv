use crate::{
    models::{MediaId, PlaylistId, ProviderTarget, RoomId, RoomPlaybackState, UserId},
    Error, Result,
};

use super::{AuthorizedAdminActor, RoomService};

impl RoomService {
    /// Start playback from the management plane.
    pub async fn admin_start_playback(
        &self,
        room_id: RoomId,
        admin_user_id: UserId,
        media_id: Option<MediaId>,
        playlist_id: Option<PlaylistId>,
        target: Option<ProviderTarget>,
    ) -> Result<RoomPlaybackState> {
        let actor = self.load_authorized_admin_actor(&admin_user_id).await?;
        self.admin_start_playback_as(room_id, &actor, media_id, playlist_id, target)
            .await
    }

    pub async fn admin_start_playback_as(
        &self,
        room_id: RoomId,
        actor: &AuthorizedAdminActor,
        media_id: Option<MediaId>,
        playlist_id: Option<PlaylistId>,
        target: Option<ProviderTarget>,
    ) -> Result<RoomPlaybackState> {
        self.admin_start_playback_as_with_outbox(
            room_id,
            actor,
            media_id,
            playlist_id,
            target,
            None,
        )
        .await
    }

    pub async fn admin_start_playback_as_with_outbox(
        &self,
        room_id: RoomId,
        actor: &AuthorizedAdminActor,
        media_id: Option<MediaId>,
        playlist_id: Option<PlaylistId>,
        target: Option<ProviderTarget>,
        outbox_event_factory: Option<crate::service::RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<RoomPlaybackState> {
        self.playback_service
            .admin_switch_with_outbox(
                room_id,
                *actor.user_id(),
                media_id,
                playlist_id,
                target,
                outbox_event_factory,
            )
            .await
    }

    /// Stop playback from the management plane.
    pub async fn admin_stop_playback(
        &self,
        room_id: RoomId,
        admin_user_id: UserId,
    ) -> Result<RoomPlaybackState> {
        let actor = self.load_authorized_admin_actor(&admin_user_id).await?;
        self.admin_stop_playback_as(room_id, &actor).await
    }

    pub async fn admin_stop_playback_as(
        &self,
        room_id: RoomId,
        actor: &AuthorizedAdminActor,
    ) -> Result<RoomPlaybackState> {
        self.admin_stop_playback_as_with_outbox(room_id, actor, None)
            .await
    }

    pub async fn admin_stop_playback_as_with_outbox(
        &self,
        room_id: RoomId,
        actor: &AuthorizedAdminActor,
        outbox_event_factory: Option<crate::service::RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<RoomPlaybackState> {
        self.playback_service
            .admin_reset_with_outbox(room_id, *actor.user_id(), outbox_event_factory)
            .await
    }

    pub async fn admin_update_playback_as_request(
        &self,
        actor: &AuthorizedAdminActor,
        request: crate::service::PlaybackStateUpdateRequest,
    ) -> Result<RoomPlaybackState> {
        if request.actor_user_id != *actor.user_id() {
            return Err(Error::Authorization(
                "Playback state update actor does not match authorized admin actor".to_string(),
            ));
        }
        self.playback_service
            .admin_update_playback_state(request)
            .await
    }
}
