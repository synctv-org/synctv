use std::time::Duration;

use synctv_core::{
    models::RoomPermission,
    service::{PlaybackStatePatch, PlaybackUpdateRequest},
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
    pub(crate) async fn handle_playback_progress(
        &self,
        report: &synctv_proto::client::PlaybackProgressReport,
    ) -> Result<(), String> {
        if report.position < 0.0 {
            return Err("Playback position must be non-negative".to_string());
        }

        self.check_realtime_permission(RoomPermission::PLAY_CONTROL)
            .await
            .map_err(|e| e.to_string())?;
        if self.principal.is_guest() {
            return Err("Guests cannot update canonical playback progress".to_string());
        }

        let playback_service = self.room_service.playback_service();
        let state = playback_service
            .get_state(&self.room_id)
            .await
            .map_err(|e| e.to_string())?;
        let expected_source = crate::impls::client::build_playback_source_expectation(
            report.expected_media_id.clone(),
            report.expected_playlist_id.clone(),
            report.expected_target_hash.clone(),
            &self.public_id_codec,
        )
        .map_err(|error| error.to_string())?;
        if expected_source
            .as_ref()
            .is_some_and(|expected_source| !expected_source.matches(&state))
        {
            tracing::debug!(
                user_id = %self.user_id,
                room_id = %self.room_id,
                "Playback progress report ignored: playback source changed"
            );
            return Ok(());
        }

        if state.is_playing && report.is_playing {
            let elapsed_ms = chrono::Utc::now()
                .signed_duration_since(state.updated_at)
                .num_milliseconds();
            let elapsed_secs = if elapsed_ms <= 0 {
                0.0
            } else {
                let elapsed_ms = u64::try_from(elapsed_ms)
                    .map_err(|_| "playback progress elapsed time exceeds u64::MAX".to_string())?;
                Duration::from_millis(elapsed_ms).as_secs_f64()
            };
            let expected_position = state.position + (elapsed_secs * state.speed);
            let drift = (report.position - expected_position).abs();

            if drift > PLAYBACK_PROGRESS_MAX_DRIFT_SECONDS {
                tracing::warn!(
                    user_id = %self.user_id,
                    room_id = %self.room_id,
                    reported = report.position,
                    expected = expected_position,
                    drift = drift,
                    "Playback progress report ignored: drift exceeds {} seconds",
                    PLAYBACK_PROGRESS_MAX_DRIFT_SECONDS
                );
                return Ok(());
            }

            let should_write = {
                let guard = self.last_progress_write.lock().await;
                should_persist_playback_progress(*guard, report.position)
            };

            if should_write {
                let prepared_fanout = self.playback_fanout.prepare_state_changed_outbox_fanout(
                    PlaybackFanoutActor::new(self.user_id, &self.username),
                );
                let mut request = PlaybackUpdateRequest::new(
                    self.room_id,
                    self.user_id,
                    PlaybackStatePatch::new(None, Some(report.position), None),
                )
                .with_expected_version(Some(state.version))
                .with_outbox(Some(prepared_fanout.outbox_factory()));
                if let Some(expected_source) = expected_source {
                    request = request.with_expected_source(expected_source);
                }
                let update_result = playback_service.update_playback_state(request).await;

                let updated_state = match update_result {
                    Ok(updated_state) => updated_state,
                    Err(CoreError::OptimisticLockConflict) => {
                        tracing::debug!(
                            room_id = %self.room_id,
                            "Playback progress report ignored: playback state changed concurrently"
                        );
                        return Ok(());
                    }
                    Err(e) => {
                        return Err(format!(
                            "Failed to update playback state from progress report: {e}"
                        ));
                    }
                };

                prepared_fanout.publish_after_outbox_commit();
                {
                    let mut guard = self.last_progress_write.lock().await;
                    *guard = Some((report.position, tokio::time::Instant::now()));
                }

                self.playback_service
                    .report_provider_playback_progress(
                        &updated_state,
                        report.position,
                        !report.is_playing,
                        false,
                    )
                    .await;
            }
        }

        Ok(())
    }

    pub(crate) async fn handle_playback_update(
        &self,
        update: &synctv_proto::client::UpdatePlaybackRequest,
    ) -> Result<(), String> {
        self.check_realtime_permission(RoomPermission::PLAY_CONTROL)
            .await
            .map_err(|e| e.to_string())?;
        if self.principal.is_guest() {
            return Err("Guests cannot control playback".to_string());
        }
        let command =
            crate::impls::client::build_update_playback(update.clone(), &self.public_id_codec)
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

        let crate::impls::client::PlaybackUpdateCommand::Patch {
            playing,
            position,
            speed,
            version,
            expected_source,
        } = command;
        let playback_service = self.room_service.playback_service();
        let prepared_fanout =
            self.playback_fanout
                .prepare_state_changed_outbox_fanout(PlaybackFanoutActor::new(
                    self.user_id,
                    &self.username,
                ));
        let mut request = PlaybackUpdateRequest::new(
            self.room_id,
            self.user_id,
            PlaybackStatePatch::new(playing, position, speed),
        )
        .with_expected_version(version)
        .with_outbox(Some(prepared_fanout.outbox_factory()));
        if let Some(expected_source) = expected_source {
            request = request.with_expected_source(expected_source);
        }
        let state = playback_service
            .update_playback_state(request)
            .await
            .map_err(|e| e.to_string())?;
        prepared_fanout.publish_after_outbox_commit();
        self.playback_service
            .handle_provider_lifecycle_transition(Some(&previous_state), &state)
            .await;

        Ok(())
    }
}
