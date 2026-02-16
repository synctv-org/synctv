//! Media/Playlist management HTTP API
//!
//! Handles playlist browsing operations.
//!
//! Uses gRPC proto types for all requests/responses to maintain consistency.
//! Delegates to `ClientApiImpl` for shared business logic.

use axum::{
    extract::{Path, Query, State},
    response::{IntoResponse, Json},
};

use crate::http::{AppState, AppResult, middleware::AuthUser};

/// Get current playing media for a room
///
/// Requires authentication and room membership.
/// Delegates to `ClientApiImpl::get_playing_media()` for consistent behavior.
#[axum::debug_handler]
pub async fn get_playing_media(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> AppResult<impl IntoResponse> {
    let response = state
        .client_api
        .get_playing_media(&auth.user_id.to_string(), &room_id)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Get playlist for a room
///
/// Delegates to `ClientApiImpl::get_playlist()` for consistent behavior with gRPC.
#[axum::debug_handler]
pub async fn get_playlist(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> AppResult<impl IntoResponse> {
    let response = state
        .client_api
        .get_playlist(auth.user_id.as_str(), &room_id)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// List dynamic playlist items
///
/// GET /`api/rooms/:room_id/playlists/:playlist_id/items`
#[axum::debug_handler]
pub async fn list_playlist_items(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((room_id, playlist_id)): Path<(String, String)>,
    Query(params): Query<ListPlaylistItemsQuery>,
) -> AppResult<impl IntoResponse> {
    let req = crate::proto::client::ListPlaylistItemsRequest {
        playlist_id: playlist_id.clone(),
        relative_path: params.relative_path.unwrap_or_default(),
        page: params.page.unwrap_or(0).max(0),
        page_size: params.page_size.unwrap_or(50).clamp(1, 100),
    };

    let response = state
        .client_api
        .list_playlist_items(auth.user_id.as_str(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Set current playing media
///
/// Delegates to `ClientApiImpl::set_current_media()` for consistent behavior with gRPC.
#[axum::debug_handler]
pub async fn set_playing_media(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((room_id, media_id)): Path<(String, String)>,
) -> AppResult<impl IntoResponse> {
    let req = crate::proto::client::SetCurrentMediaRequest {
        playlist_id: String::new(), // Use current playlist
        media_id,
    };

    let response = state
        .client_api
        .set_current_media(auth.user_id.as_str(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

// ============== Request/Response Types ==============

#[derive(Debug, serde::Deserialize)]
pub struct ListPlaylistItemsQuery {
    pub relative_path: Option<String>,
    pub page: Option<i32>,
    pub page_size: Option<i32>,
}
