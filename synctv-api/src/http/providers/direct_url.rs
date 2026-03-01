//! `DirectURL` Provider HTTP Proxy Routes
//!
//! Proxies direct URL media that has playback info stored in `source_config`.

use std::collections::HashMap;

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    routing::get,
    Router,
};

use crate::http::{error::AppResult, middleware::AuthUser, AppState};
use crate::impls::provider::resolve_media_from_playlist;
use synctv_core::models::{MediaId, RoomId};

/// Build `DirectURL` HTTP routes (proxy only, no provider API)
pub fn direct_url_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/proxy/{room_id}/{media_id}",
            get(proxy_stream).options(super::proxy_options_preflight),
        )
        .route("/proxy/{room_id}/{media_id}/m3u8", get(proxy_m3u8))
}

// ------------------------------------------------------------------
// Proxy handlers
// ------------------------------------------------------------------

/// Resolve playback URL from a direct-URL media item's `source_config`.
async fn resolve_direct_playback(
    auth: &AuthUser,
    room_id: &RoomId,
    media_id: &MediaId,
    state: &AppState,
) -> Result<(String, HashMap<String, String>), crate::http::AppError> {
    let media = resolve_media_from_playlist(&auth.user_id, room_id, media_id, &state.room_service)
        .await
        .map_err(crate::http::error::map_api_error)?;

    let playback_result = media
        .get_playback_result()
        .ok_or_else(|| anyhow::anyhow!("Media does not have playback info"))?;

    let default_mode = &playback_result.default_mode;
    let playback_info = playback_result
        .playback_infos
        .get(default_mode)
        .ok_or_else(|| anyhow::anyhow!("Default playback mode not found"))?;

    let playback_url = playback_info
        .urls
        .first()
        .ok_or_else(|| anyhow::anyhow!("No URLs found in playback info"))?;

    Ok((playback_url.url.clone(), playback_url.headers.clone()))
}

/// GET /`proxy/:room_id/:media_id` - Proxy direct URL video stream
async fn proxy_stream(
    auth: AuthUser,
    Path((room_id, media_id)): Path<(String, String)>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<axum::response::Response> {
    let room_id = RoomId::from_string(room_id);
    let media_id = MediaId::from_string(media_id);

    let (url, provider_headers) =
        resolve_direct_playback(&auth, &room_id, &media_id, &state).await?;

    tracing::debug!("Proxying direct URL media: {}", url);

    let cfg = synctv_proxy::ProxyConfig {
        url: &url,
        provider_headers: &provider_headers,
        client_headers: &headers,
    };

    synctv_proxy::proxy_fetch_and_forward(cfg, &synctv_proxy::NoopMetrics)
        .await
        .map_err(Into::into)
}

/// GET /`proxy/:room_id/:media_id/m3u8` - Proxy direct URL M3U8
async fn proxy_m3u8(
    auth: AuthUser,
    Path((room_id, media_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> AppResult<axum::response::Response> {
    let room_id_parsed = RoomId::from_string(room_id.clone());
    let media_id_parsed = MediaId::from_string(media_id.clone());

    let (url, provider_headers) =
        resolve_direct_playback(&auth, &room_id_parsed, &media_id_parsed, &state).await?;

    let proxy_base = format!("/api/providers/direct_url/proxy/{room_id}/{media_id}");

    synctv_proxy::proxy_m3u8_and_rewrite(&url, &provider_headers, &proxy_base)
        .await
        .map_err(Into::into)
}
