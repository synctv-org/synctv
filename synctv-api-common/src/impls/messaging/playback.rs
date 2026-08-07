use synctv_core::{
    models::RoomPermission,
    service::{PlaybackStatePatch, PlaybackStateUpdateRequest},
};

use crate::playback_fanout::PlaybackFanoutActor;

use super::StreamMessageHandler;

impl StreamMessageHandler {
    pub async fn handle_playback_source_update(
        &self,
        update: &synctv_proto::client::UpdatePlaybackRequest,
    ) -> Result<(), String> {
        crate::impls::validate_proto_request(update).map_err(|error| error.to_string())?;
        self.check_realtime_permission(RoomPermission::CONTROL_PLAYBACK_STATE)
            .await
            .map_err(|e| e.to_string())?;
        if self.principal.is_guest() {
            return Err("Guests cannot control playback".to_string());
        }
        let target = crate::impls::client::build_start_playback_request(
            synctv_proto::client::StartPlaybackRequest {
                media_id: update.media_id.clone(),
                playlist_id: update.playlist_id.clone(),
                target: update.target.clone(),
                client_operation_id: update.client_operation_id.clone(),
            },
            &self.public_id_codec,
        )
        .map_err(|error| error.to_string())?;
        let previous_state = self
            .room_service
            .playback_service()
            .get_state(&self.room_id)
            .await
            .map_err(|error| {
                format!(
                    "Failed to load previous playback state for provider lifecycle transition: {error}"
                )
            })?;
        let prepared_fanout = self.playback_fanout.prepare_state_changed_outbox_fanout(
            PlaybackFanoutActor::new(self.user_id, &self.username)
                .with_client_operation_id(update.client_operation_id.as_deref()),
        );
        let state = if target.media_id.is_none() && target.playlist_id.is_none() {
            self.room_service
                .playback_service()
                .reset_with_outbox(
                    self.room_id,
                    self.user_id,
                    Some(prepared_fanout.outbox_factory_with_source_changed(true)),
                )
                .await
                .map_err(|e| e.to_string())?
        } else {
            self.room_service
                .playback_service()
                .switch_with_outbox(
                    self.room_id,
                    self.user_id,
                    target.media_id,
                    target.playlist_id,
                    target.target,
                    Some(prepared_fanout.outbox_factory_with_source_changed(true)),
                )
                .await
                .map_err(|e| e.to_string())?
        };
        prepared_fanout.publish_after_outbox_commit();
        self.playback_service
            .handle_provider_lifecycle_transition(Some(&previous_state), &state)
            .await;
        Ok(())
    }

    pub async fn handle_playback_state_update(
        &self,
        update: &synctv_proto::client::UpdatePlaybackStateRequest,
    ) -> Result<(), String> {
        self.check_realtime_permission(RoomPermission::CONTROL_PLAYBACK_STATE)
            .await
            .map_err(|e| e.to_string())?;
        if self.principal.is_guest() {
            return Err("Guests cannot control playback".to_string());
        }
        let update_parts = crate::impls::client::build_playback_state_update(
            update.clone(),
            &self.public_id_codec,
        )
        .map_err(|error| error.to_string())?;
        let previous_state = self
            .room_service
            .playback_service()
            .get_state(&self.room_id)
            .await
            .map_err(|error| {
                format!(
                    "Failed to load previous playback state for provider lifecycle transition: {error}"
                )
            })?;

        let playing = update_parts.playing;
        let position = update_parts.position;
        let speed = update_parts.speed;
        let version = update_parts.version;
        let expected_source = update_parts.expected_source;
        let client_operation_id = update_parts.client_operation_id;
        let client_time_millis = update_parts.client_time_millis;
        let playback_service = self.room_service.playback_service();
        let prepared_fanout = self.playback_fanout.prepare_state_changed_outbox_fanout(
            PlaybackFanoutActor::new(self.user_id, &self.username)
                .with_client_operation_id(client_operation_id.as_deref()),
        );
        let mut request = PlaybackStateUpdateRequest::new(
            self.room_id,
            self.user_id,
            PlaybackStatePatch::new(playing, position, speed),
        )
        .with_expected_version(version)
        .with_client_time_millis(client_time_millis)
        .with_outbox(Some(prepared_fanout.outbox_factory()));
        if let Some(expected_source) = expected_source {
            request = request.with_expected_source(expected_source);
        }
        let state = playback_service
            .update_playback_state(request)
            .await
            .map_err(|error| error.to_string())?;
        prepared_fanout.publish_after_outbox_commit();
        self.playback_service
            .handle_provider_lifecycle_transition(Some(&previous_state), &state)
            .await;

        Ok(())
    }
}
