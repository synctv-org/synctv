use std::sync::Arc;

use crate::{
    models::{MediaId, PlaylistId, ProviderTarget, RoomId, RoomPlaybackState, UserId},
    repository::realtime_outbox::NewRealtimeOutboxEvent,
    Error, Result,
};

pub type RealtimeOutboxPlaybackStateEventFactory =
    Arc<dyn Fn(&RoomPlaybackState) -> Result<NewRealtimeOutboxEvent> + Send + Sync>;

#[derive(Debug, Clone)]
pub struct SwitchPlaybackTarget {
    pub media_id: Option<MediaId>,
    pub playlist_id: Option<PlaylistId>,
    pub target: Option<ProviderTarget>,
}

#[derive(Debug, Clone)]
pub struct PlaybackSourceExpectation {
    pub media_id: Option<MediaId>,
    pub playlist_id: Option<PlaylistId>,
    pub target_hash: String,
}

impl PlaybackSourceExpectation {
    pub fn matches(&self, state: &RoomPlaybackState) -> Result<bool> {
        Ok(self.media_id == state.playing_media_id
            && self.playlist_id == state.playing_playlist_id
            && self.target_hash.eq_ignore_ascii_case(&state.target_hash()?))
    }
}

#[derive(Debug, Clone, Default)]
pub struct PlaybackStatePatch {
    pub playing: Option<bool>,
    pub position: Option<f64>,
    pub speed: Option<f64>,
}

impl PlaybackStatePatch {
    #[must_use]
    pub const fn new(playing: Option<bool>, position: Option<f64>, speed: Option<f64>) -> Self {
        Self {
            playing,
            position,
            speed,
        }
    }
}

#[derive(Clone)]
pub struct PlaybackStateUpdateRequest {
    pub room_id: RoomId,
    pub actor_user_id: UserId,
    pub patch: PlaybackStatePatch,
    pub expected_version: Option<i64>,
    pub expected_source: Option<PlaybackSourceExpectation>,
    pub client_time_millis: Option<i64>,
    pub outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
}

impl PlaybackStateUpdateRequest {
    #[must_use]
    pub const fn new(room_id: RoomId, actor_user_id: UserId, patch: PlaybackStatePatch) -> Self {
        Self {
            room_id,
            actor_user_id,
            patch,
            expected_version: None,
            expected_source: None,
            client_time_millis: None,
            outbox_event_factory: None,
        }
    }

    #[must_use]
    pub const fn with_expected_version(mut self, expected_version: Option<i64>) -> Self {
        self.expected_version = expected_version;
        self
    }

    #[must_use]
    pub fn with_expected_source(mut self, expected_source: PlaybackSourceExpectation) -> Self {
        self.expected_source = Some(expected_source);
        self
    }

    #[must_use]
    pub const fn with_client_time_millis(mut self, client_time_millis: Option<i64>) -> Self {
        self.client_time_millis = client_time_millis;
        self
    }

    #[must_use]
    pub fn with_outbox(
        mut self,
        outbox_event_factory: Option<RealtimeOutboxPlaybackStateEventFactory>,
    ) -> Self {
        self.outbox_event_factory = outbox_event_factory;
        self
    }
}

#[derive(Debug)]
pub(super) enum NextTarget {
    Static(crate::models::Media),
    Dynamic {
        playlist_id: PlaylistId,
        media_name: String,
        target: ProviderTarget,
    },
}

#[derive(Debug, Clone)]
pub struct SeekResponse {
    pub state: RoomPlaybackState,
    pub seek_applied: bool,
    pub message: Option<String>,
}

impl SeekResponse {
    #[must_use]
    pub const fn success(state: RoomPlaybackState) -> Self {
        Self {
            state,
            seek_applied: true,
            message: None,
        }
    }

    #[must_use]
    pub fn degraded(state: RoomPlaybackState, message: impl Into<String>) -> Self {
        Self {
            state,
            seek_applied: false,
            message: Some(message.into()),
        }
    }
}

pub(super) const MAX_PLAYBACK_POSITION_SECONDS: f64 = 86_400.0;
const MIN_PLAYBACK_SPEED: f64 = 0.25;
const MAX_PLAYBACK_SPEED: f64 = 4.0;

pub(super) fn validate_seek_position(position: f64) -> Result<()> {
    if !position.is_finite() {
        return Err(Error::InvalidInput(
            "Seek position must be a finite number".to_string(),
        ));
    }
    if position < 0.0 {
        return Err(Error::InvalidInput(
            "Seek position must be non-negative".to_string(),
        ));
    }
    if position > MAX_PLAYBACK_POSITION_SECONDS {
        return Err(Error::InvalidInput(
            "Seek position exceeds maximum (24 hours)".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn validate_playback_speed_value(speed: f64) -> Result<()> {
    if !speed.is_finite() {
        return Err(Error::InvalidInput(
            "Speed must be a finite number".to_string(),
        ));
    }
    if !(MIN_PLAYBACK_SPEED..=MAX_PLAYBACK_SPEED).contains(&speed) {
        return Err(Error::InvalidInput(format!(
            "Speed must be between {MIN_PLAYBACK_SPEED} and {MAX_PLAYBACK_SPEED}"
        )));
    }
    Ok(())
}

pub(super) fn validate_position_update_source(state: &RoomPlaybackState) -> Result<()> {
    if state.playing_media_id.is_none() && state.playing_playlist_id.is_none() {
        return Err(Error::InvalidInput(
            "playback position update requires a current playback source".to_string(),
        ));
    }

    Ok(())
}

pub(super) fn validate_switch_target(target: &SwitchPlaybackTarget) -> Result<()> {
    match (&target.media_id, &target.playlist_id, &target.target) {
        (None, None, Some(_)) => Err(Error::InvalidInput(
            "target must be omitted when clearing playback".to_string(),
        )),
        (Some(_), _, Some(_)) => Err(Error::InvalidInput(
            "target must be omitted when switching to a static media item".to_string(),
        )),
        (None, Some(_), None) => Err(Error::InvalidInput(
            "target is required when switching to a dynamic playlist item".to_string(),
        )),
        _ => Ok(()),
    }
}

fn playback_source_is_set(state: &RoomPlaybackState) -> bool {
    state.playing_media_id.is_some() || state.playing_playlist_id.is_some()
}

fn playback_source_changed(before: &RoomPlaybackState, after: &RoomPlaybackState) -> bool {
    before.playing_media_id != after.playing_media_id
        || before.playing_playlist_id != after.playing_playlist_id
        || before.target != after.target
}

pub(super) fn previous_progress_position_for_source_transition(
    before: &RoomPlaybackState,
    after: &RoomPlaybackState,
) -> Option<f64> {
    if playback_source_is_set(before) && playback_source_changed(before, after) {
        Some(before.computed_position())
    } else {
        None
    }
}
