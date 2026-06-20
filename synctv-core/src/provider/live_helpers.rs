//! Shared helpers for live playback providers (RTMP and LiveProxy).

use super::error::ProviderError;
use super::playback_transport::PlaybackTransportAction;
use super::store::VersionedPlayback;
use crate::models::{MediaId, RoomId, TypedId};
use crate::proxy_signature::ProxyUrlClaims;
use crate::PublicIdCodec;

/// Extract a typed ID from versioned playback metadata.
///
/// Supports both numeric (i64/u64) and string (public ID) formats.
pub(super) fn metadata_typed_id<T>(
    versioned: &VersionedPlayback,
    field: &'static str,
    parse_public_id: impl FnOnce(&str) -> Result<T, ProviderError>,
) -> Result<T, ProviderError>
where
    T: TypedId,
{
    let value = versioned
        .result
        .metadata
        .get(field)
        .ok_or_else(|| ProviderError::ApiError(format!("Live playback missing {field}")))?;

    if let Some(id) = value.as_i64() {
        return T::try_from(id).map_err(|error| {
            ProviderError::InvalidConfig(format!(
                "Invalid {field} in live playback metadata: {error}"
            ))
        });
    }

    if let Some(id) = value.as_u64() {
        let id = i64::try_from(id).map_err(|_| {
            ProviderError::InvalidConfig(format!(
                "Invalid {field} in live playback metadata: exceeds i64"
            ))
        })?;
        return T::try_from(id).map_err(|error| {
            ProviderError::InvalidConfig(format!(
                "Invalid {field} in live playback metadata: {error}"
            ))
        });
    }

    let value = value.as_str().ok_or_else(|| {
        ProviderError::InvalidConfig(format!(
            "Invalid {field} in live playback metadata: expected public ID string or numeric ID"
        ))
    })?;

    parse_public_id(value)
}

/// Extract room_id and media_id from versioned playback metadata.
pub(super) fn live_ids_from_metadata(
    versioned: &VersionedPlayback,
    public_id_codec: &PublicIdCodec,
    context_label: &str,
) -> Result<(RoomId, MediaId), ProviderError> {
    let room_id = metadata_typed_id(versioned, "room_id", |room_id| {
        super::playback_transport::parse_playback_room_id(public_id_codec, room_id, context_label)
    })?;
    let media_id = metadata_typed_id(versioned, "media_id", |media_id| {
        super::playback_transport::parse_playback_media_id(public_id_codec, media_id, context_label)
    })?;
    Ok((room_id, media_id))
}

/// Build a FLV stream playback transport action.
pub(super) fn build_flv_action(
    provider_name: &str,
    versioned: &VersionedPlayback,
    claims: &ProxyUrlClaims,
    public_id_codec: &PublicIdCodec,
    context_label: &str,
) -> Result<PlaybackTransportAction, ProviderError> {
    let (room_id, media_id) = live_ids_from_metadata(versioned, public_id_codec, context_label)?;
    Ok(PlaybackTransportAction::LiveFlv {
        provider_name: provider_name.to_string(),
        room_id,
        media_id,
        user_id: super::playback_transport::parse_playback_user_id(
            public_id_codec,
            &claims.user_id,
            &format!("{context_label} proxy claims"),
        )?,
        expires_at: claims.expires_at,
    })
}

/// Build an HLS playlist playback transport action.
pub(super) fn build_hls_playlist_action(
    provider_name: &str,
    versioned: &VersionedPlayback,
    public_id_codec: &PublicIdCodec,
    context_label: &str,
) -> Result<PlaybackTransportAction, ProviderError> {
    let (room_id, media_id) = live_ids_from_metadata(versioned, public_id_codec, context_label)?;
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
    public_id_codec: &PublicIdCodec,
    context_label: &str,
) -> Result<PlaybackTransportAction, ProviderError> {
    let (room_id, media_id) = live_ids_from_metadata(versioned, public_id_codec, context_label)?;
    Ok(PlaybackTransportAction::LiveHlsSegment {
        provider_name: provider_name.to_string(),
        room_id,
        media_id,
        segment_name: segment_name.to_string(),
        disguised_as_png: segment_name.ends_with(".png"),
    })
}
