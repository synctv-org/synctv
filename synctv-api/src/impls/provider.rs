//! Provider business logic implementation
//!
//! This module handles all provider-related business logic:
//! - Playback generation (caching is internal to providers via ProviderStore)
//! - Media resolution and playback URL extraction

use std::collections::HashMap;
use synctv_core::models::{Media, MediaId, RoomId, UserId};
use synctv_core::provider::{
    MediaProvider, PlaybackResult as ProviderPlaybackResult, ProviderContext,
};
use synctv_core::service::RoomService;

use super::ApiError;

// ------------------------------------------------------------------
// Shared playback resolution helpers
// ------------------------------------------------------------------

/// Verify room membership and look up a specific media item by ID.
///
/// This is the common first phase shared by all provider proxy handlers.
/// Uses a direct media lookup by primary key instead of fetching the entire
/// playlist and scanning linearly, which is O(1) vs O(n) in playlist size.
pub async fn resolve_media_from_playlist(
    user_id: &UserId,
    room_id: &RoomId,
    media_id: &MediaId,
    room_service: &RoomService,
) -> Result<Media, ApiError> {
    room_service
        .check_membership(room_id, user_id)
        .await
        .map_err(|_| ApiError::Authorization("Not a member of this room".to_string()))?;

    let media = room_service
        .media_service()
        .get_media(media_id)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to get media: {e}")))?
        .ok_or_else(|| ApiError::NotFound("Media not found in playlist".to_string()))?;

    // Verify the media belongs to the requested room
    if media.room_id != *room_id {
        return Err(ApiError::NotFound(
            "Media not found in playlist".to_string(),
        ));
    }

    Ok(media)
}

/// Resolve a playback URL and headers from a `MediaProvider`.
///
/// Performs the full flow: membership check -> playlist lookup -> find media ->
/// `generate_playback` -> extract first URL + headers from the default mode.
///
/// Used by alist and emby proxy handlers.
pub async fn resolve_provider_playback_url(
    user_id: &UserId,
    room_id: &RoomId,
    media_id: &MediaId,
    provider: &dyn MediaProvider,
    room_service: &RoomService,
    credential_encryption: Option<&synctv_core::service::CredentialEncryption>,
    store: Option<&std::sync::Arc<dyn synctv_core::provider::store::ProviderStore>>,
) -> Result<(String, HashMap<String, String>), ApiError> {
    let media = resolve_media_from_playlist(user_id, room_id, media_id, room_service).await?;

    let mut ctx = ProviderContext::new("synctv")
        .with_user_id(user_id.as_str())
        .with_room_id(room_id.as_str());
    if let Some(enc) = credential_encryption {
        ctx = ctx.with_credential_encryption(enc);
    }
    if let Some(store) = store {
        ctx = ctx.with_store(store.clone());
    }

    let playback_result = provider
        .generate_playback(&ctx, &media.source_config)
        .await
        .map_err(|e| {
            ApiError::Internal(format!("{} generate_playback failed: {e}", provider.name()))
        })?;

    let default_mode = &playback_result.default_mode;
    let playback_info = playback_result
        .playback_infos
        .get(default_mode)
        .ok_or_else(|| ApiError::Internal("Default playback mode not found".to_string()))?;

    let url = playback_info
        .urls
        .first()
        .ok_or_else(|| ApiError::Internal("No URLs in playback info".to_string()))?;

    Ok((url.clone(), playback_info.headers.clone()))
}

/// Resolve the full `PlaybackResult` from a `MediaProvider`.
///
/// Performs the full flow: membership check -> playlist lookup -> find media ->
/// `generate_playback`.
///
/// Used by bilibili proxy handlers that need access to the complete result
/// (DASH data, multiple modes, subtitles).
pub async fn resolve_provider_playback_result(
    user_id: &UserId,
    room_id: &RoomId,
    media_id: &MediaId,
    provider: &dyn MediaProvider,
    room_service: &RoomService,
    credential_encryption: Option<&synctv_core::service::CredentialEncryption>,
    store: Option<&std::sync::Arc<dyn synctv_core::provider::store::ProviderStore>>,
) -> Result<ProviderPlaybackResult, ApiError> {
    let media = resolve_media_from_playlist(user_id, room_id, media_id, room_service).await?;

    let mut ctx = ProviderContext::new("synctv")
        .with_user_id(user_id.as_str())
        .with_room_id(room_id.as_str());
    if let Some(enc) = credential_encryption {
        ctx = ctx.with_credential_encryption(enc);
    }
    if let Some(store) = store {
        ctx = ctx.with_store(store.clone());
    }

    provider
        .generate_playback(&ctx, &media.source_config)
        .await
        .map_err(|e| {
            ApiError::Internal(format!("{} generate_playback failed: {e}", provider.name()))
        })
}
