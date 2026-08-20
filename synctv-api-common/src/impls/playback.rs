use async_trait::async_trait;
use synctv_core::models::{PlaybackKind, RoomId, RoomPlaybackState};
use synctv_core::provider::ProviderCredentialDependency;

use crate::impls::client::RoomActor;
use crate::impls::ApiError;

#[async_trait]
pub trait PlaybackService: Send + Sync {
    async fn room_playback_state(&self, room_id: &RoomId) -> Result<RoomPlaybackState, ApiError>;

    async fn get_playback_for_actor(
        &self,
        actor: &RoomActor,
        room_id: &RoomId,
        state: &RoomPlaybackState,
        playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
    ) -> Result<synctv_proto::client::Playback, ApiError>;

    async fn playback_credential_dependencies(
        &self,
        _actor: &RoomActor,
        _room_id: &RoomId,
        _state: &RoomPlaybackState,
    ) -> Result<Vec<ProviderCredentialDependency>, ApiError> {
        Ok(Vec::new())
    }

    async fn handle_provider_lifecycle_transition(
        &self,
        _previous: Option<&RoomPlaybackState>,
        _current: &RoomPlaybackState,
    ) {
    }

    async fn report_provider_playback_progress(
        &self,
        _state: &RoomPlaybackState,
        _position: f64,
        _is_paused: bool,
        _force: bool,
    ) {
    }

    async fn reap_provider_playback_sessions(&self, _force: bool) {}

    async fn refresh_observed_playback_metadata_and_auto_advance(
        &self,
        _room_id: &RoomId,
        _state: &RoomPlaybackState,
    ) {
    }
}

pub fn playback_expires_at(playback: &synctv_proto::client::Playback) -> Option<i64> {
    playback
        .playback_infos
        .values()
        .flat_map(|info| info.medias.iter().filter_map(|media| media.expire_at))
        .min()
}

pub const fn playback_generation_error_allows_state_only(error: &ApiError) -> bool {
    matches!(
        error,
        ApiError::ServiceUnavailable(_) | ApiError::Timeout(_)
    )
}

pub fn playback_snapshot_error_indicates_stale_state<T>(
    state: &RoomPlaybackState,
    result: &Result<T, ApiError>,
) -> bool {
    if state.playing_media_id.is_none() && state.playing_playlist_id.is_none() {
        return false;
    }

    match result {
        Err(ApiError::Authorization(_)) => true,
        Err(ApiError::NotFound(message)) => matches!(
            message.as_str(),
            "Media not found" | "Playlist not found" | "Dynamic playlist item not found"
        ),
        _ => false,
    }
}

pub fn normalized_provider_duration(
    playback_kind: Option<PlaybackKind>,
    duration_seconds: Option<f64>,
) -> Option<f64> {
    playback_kind
        .unwrap_or(PlaybackKind::Regular)
        .supports_duration()
        .then_some(duration_seconds)
        .flatten()
        .filter(|duration| duration.is_finite() && *duration > 0.0)
}
