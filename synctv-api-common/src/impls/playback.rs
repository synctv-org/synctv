use async_trait::async_trait;
use synctv_core::models::{RoomId, RoomPlaybackState, UserId};
use synctv_core::provider::ProviderCredentialDependency;

use crate::impls::ApiError;

#[async_trait]
pub trait PlaybackService: Send + Sync {
    async fn room_playback_state(&self, room_id: &RoomId) -> Result<RoomPlaybackState, ApiError>;

    async fn get_playback(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
        state: &RoomPlaybackState,
        playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
    ) -> Result<synctv_proto::client::Playback, ApiError>;

    async fn playback_credential_dependencies(
        &self,
        _user_id: &UserId,
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

pub fn normalized_provider_duration(
    is_live: Option<bool>,
    duration_seconds: Option<f64>,
) -> Option<f64> {
    let is_live = is_live.unwrap_or(false);
    (!is_live)
        .then_some(duration_seconds)
        .flatten()
        .filter(|duration| duration.is_finite() && *duration > 0.0)
}
