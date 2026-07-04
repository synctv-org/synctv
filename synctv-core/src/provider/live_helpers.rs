//! Shared helpers for live playback providers (RTMP and LiveProxy).

use super::error::ProviderError;
use super::playback_transport::{LiveFlvAccess, PlaybackTransportAction};
use super::store::VersionedPlayback;
use crate::models::{MediaId, RoomId};

/// Extract room_id and media_id from versioned playback metadata.
pub(super) fn live_ids_from_metadata(
    versioned: &VersionedPlayback,
) -> Result<(RoomId, MediaId), ProviderError> {
    let room_id = versioned
        .result
        .metadata
        .room_id
        .ok_or_else(|| ProviderError::ApiError("Live playback missing room_id".to_string()))?;
    let media_id = versioned
        .result
        .metadata
        .media_id
        .ok_or_else(|| ProviderError::ApiError("Live playback missing media_id".to_string()))?;
    Ok((room_id, media_id))
}

/// Build a FLV stream playback transport action.
pub(super) fn build_flv_action(
    provider_name: &str,
    versioned: &VersionedPlayback,
    access: LiveFlvAccess,
) -> Result<PlaybackTransportAction, ProviderError> {
    let (room_id, media_id) = live_ids_from_metadata(versioned)?;
    Ok(PlaybackTransportAction::LiveFlv {
        provider_name: provider_name.to_string(),
        room_id,
        media_id,
        user_id: access.user_id,
        expires_at: access.expires_at,
    })
}

/// Build an HLS playlist playback transport action.
pub(super) fn build_hls_playlist_action(
    provider_name: &str,
    versioned: &VersionedPlayback,
) -> Result<PlaybackTransportAction, ProviderError> {
    let (room_id, media_id) = live_ids_from_metadata(versioned)?;
    Ok(PlaybackTransportAction::LiveHlsPlaylist {
        provider_name: provider_name.to_string(),
        room_id,
        media_id,
        version: versioned.version.clone(),
    })
}

/// Build an HLS segment playback transport action.
pub(super) fn build_hls_segment_action(
    provider_name: &str,
    versioned: &VersionedPlayback,
    segment_name: &str,
) -> Result<PlaybackTransportAction, ProviderError> {
    let (room_id, media_id) = live_ids_from_metadata(versioned)?;
    Ok(PlaybackTransportAction::LiveHlsSegment {
        provider_name: provider_name.to_string(),
        room_id,
        media_id,
        segment_name: segment_name.to_string(),
        disguised_as_png: segment_name.ends_with(".png"),
    })
}
