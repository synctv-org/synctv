use std::time::Duration;

use synctv_core::{
    models::RoomPermission,
    service::{PlaybackStatePatch, PlaybackStateUpdateRequest},
    Error as CoreError,
};

use crate::playback_fanout::PlaybackFanoutActor;

use super::StreamMessageHandler;

/// Minimum position change (in seconds) required to trigger a DB write
/// for playback progress reports. Reports with smaller position deltas
/// are acknowledged but not persisted, reducing write amplification.
const PROGRESS_MIN_POSITION_DELTA: f64 = 1.0;

/// Minimum elapsed wall-clock time (in seconds) between DB writes for
/// playback progress reports, regardless of position delta.
const PROGRESS_MIN_ELAPSED_SECS: f64 = 5.0;

const PLAYBACK_PROGRESS_MAX_DRIFT_SECONDS: f64 = 30.0;

pub(crate) fn should_persist_playback_progress(
    last_write: Option<(f64, tokio::time::Instant)>,
    position: f64,
) -> bool {
    match last_write {
        Some((last_pos, last_time)) => {
            let pos_delta = (position - last_pos).abs();
            let elapsed = last_time.elapsed().as_secs_f64();
            pos_delta > PROGRESS_MIN_POSITION_DELTA || elapsed > PROGRESS_MIN_ELAPSED_SECS
        }
        None => true,
    }
}

impl StreamMessageHandler {
    pub(crate) async fn handle_playback_source_update(
        &self,
        update: &synctv_proto::client::UpdatePlaybackRequest,
    ) -> Result<(), String> {
        self.check_realtime_permission(RoomPermission::PLAY_CONTROL)
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
        let prepared_fanout =
            self.playback_fanout
                .prepare_state_changed_outbox_fanout(PlaybackFanoutActor::new(
                    self.user_id,
                    &self.username,
                ));
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

    pub(crate) async fn handle_playback_state_update(
        &self,
        update: &synctv_proto::client::UpdatePlaybackStateRequest,
    ) -> Result<(), String> {
        self.check_realtime_permission(RoomPermission::PLAY_CONTROL)
            .await
            .map_err(|e| e.to_string())?;
        if self.principal.is_guest() {
            return Err("Guests cannot control playback".to_string());
        }
        let command = crate::impls::client::build_playback_state_update(
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

        let crate::impls::client::PlaybackStateUpdateCommand::Patch {
            playing,
            position,
            speed,
            version,
            expected_source,
        } = command;
        let playback_service = self.room_service.playback_service();
        let is_progress_update = matches!(
            synctv_proto::client::PlaybackUpdateType::try_from(update.r#type),
            Ok(synctv_proto::client::PlaybackUpdateType::Play
                | synctv_proto::client::PlaybackUpdateType::Seek)
        ) && position.is_some()
            && speed.is_none();
        if let (true, Some(position)) = (is_progress_update, position) {
            let current_state = playback_service
                .get_state(&self.room_id)
                .await
                .map_err(|e| e.to_string())?;
            if current_state.is_playing && playing.unwrap_or(true) {
                let elapsed_ms = chrono::Utc::now()
                    .signed_duration_since(current_state.updated_at)
                    .num_milliseconds();
                let elapsed_secs = if elapsed_ms <= 0 {
                    0.0
                } else {
                    let elapsed_ms = u64::try_from(elapsed_ms).map_err(|_| {
                        "playback state update elapsed time exceeds u64::MAX".to_string()
                    })?;
                    Duration::from_millis(elapsed_ms).as_secs_f64()
                };
                let expected_position =
                    current_state.position + (elapsed_secs * current_state.speed);
                let drift = (position - expected_position).abs();

                if drift > PLAYBACK_PROGRESS_MAX_DRIFT_SECONDS {
                    tracing::warn!(
                        user_id = %self.user_id,
                        room_id = %self.room_id,
                        reported = position,
                        expected = expected_position,
                        drift = drift,
                        "Playback state update ignored: drift exceeds {} seconds",
                        PLAYBACK_PROGRESS_MAX_DRIFT_SECONDS
                    );
                    return Ok(());
                }

                let should_write = {
                    let guard = self.last_progress_write.lock().await;
                    should_persist_playback_progress(*guard, position)
                };
                if !should_write {
                    return Ok(());
                }
            }
        }
        let prepared_fanout =
            self.playback_fanout
                .prepare_state_changed_outbox_fanout(PlaybackFanoutActor::new(
                    self.user_id,
                    &self.username,
                ));
        let mut request = PlaybackStateUpdateRequest::new(
            self.room_id,
            self.user_id,
            PlaybackStatePatch::new(playing, position, speed),
        )
        .with_expected_version(version)
        .with_outbox(Some(prepared_fanout.outbox_factory()));
        if let Some(expected_source) = expected_source {
            request = request.with_expected_source(expected_source);
        }
        let state = playback_service.update_playback_state(request).await;
        let state = match state {
            Ok(state) => state,
            Err(CoreError::OptimisticLockConflict) if is_progress_update => {
                tracing::debug!(
                    room_id = %self.room_id,
                    "Playback state update ignored: playback state changed concurrently"
                );
                return Ok(());
            }
            Err(CoreError::InvalidInput(message))
                if is_progress_update
                    && message == "live playback does not accept position updates" =>
            {
                tracing::debug!(
                    room_id = %self.room_id,
                    "Playback progress update ignored for live playback"
                );
                return Ok(());
            }
            Err(error) => return Err(error.to_string()),
        };
        prepared_fanout.publish_after_outbox_commit();
        if let (true, Some(position)) = (is_progress_update, position) {
            let mut guard = self.last_progress_write.lock().await;
            *guard = Some((position, tokio::time::Instant::now()));
        }
        self.playback_service
            .handle_provider_lifecycle_transition(Some(&previous_state), &state)
            .await;

        Ok(())
    }
}
