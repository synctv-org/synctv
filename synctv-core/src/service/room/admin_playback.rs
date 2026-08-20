use crate::{
    models::{RoomId, RoomPlaybackState, UserId},
    service::SwitchPlaybackTarget,
    Error, Result,
};

use super::{AuthorizedAdminActor, RoomService};

impl RoomService {
    pub async fn admin_start_playback_as_with_outbox(
        &self,
        room_id: RoomId,
        actor: &AuthorizedAdminActor,
        recorded_actor_user_id: Option<UserId>,
        target: SwitchPlaybackTarget,
        outbox_event_factory: Option<crate::service::RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Result<RoomPlaybackState> {
        self.playback_service
            .admin_switch_with_outbox(
                room_id,
                *actor.user_id(),
                recorded_actor_user_id,
                target,
                outbox_event_factory,
            )
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
