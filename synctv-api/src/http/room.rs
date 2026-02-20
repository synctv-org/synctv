// Room management HTTP handlers
//
// Thin transport layer: delegates all business logic to the impls layer.
// Request and response types are proto-generated structs.

use axum::{
    extract::{Path, Query, State},
    Json,
};

use super::{middleware::AuthUser, AppResult, AppState};
use crate::proto::client::{
    CreateRoomResponse, CreateRoomRequest, GetRoomResponse,
    JoinRoomResponse, JoinRoomRequest, LeaveRoomResponse,
    DeleteRoomResponse,
    AddMediaResponse, AddMediaRequest, DeleteMediaResponse, DeleteMediaRequest,
    ListPlaylistResponse, SwapMediaResponse, SwapMediaRequest,
    StartPlaybackResponse, StartPlaybackRequest,
    StopPlaybackResponse, StopPlaybackRequest,
    GetPlaybackResponse, GetPlaybackRequest,
    GetRoomMembersResponse, CheckRoomResponse, ListRoomsResponse, ListRoomsRequest,
    UpdateRoomSettingsRequest, UpdateRoomSettingsResponse,
    ResetRoomSettingsResponse,
    SetRoomPasswordRequest, SetRoomPasswordResponse,
    CheckRoomPasswordRequest, CheckRoomPasswordResponse,
    EditMediaRequest, EditMediaResponse, ClearPlaylistResponse,
    AddMediaBatchRequest, DeleteMediaBatchRequest, DeleteMediaBatchResponse,
    ReorderMediaBatchRequest, ReorderMediaBatchResponse, MediaReorderUpdate,
    GetChatHistoryResponse,
    CreatePlaylistRequest, CreatePlaylistResponse,
    UpdatePlaylistRequest, UpdatePlaylistResponse,
    DeletePlaylistRequest, DeletePlaylistResponse,
    ListPlaylistsResponse,
    GetHotRoomsResponse,
};

// ==================== Room Management Endpoints ====================

/// Create a new room
#[tracing::instrument(name = "http_create_room", skip(state), fields(user_id = %auth.user_id))]
pub async fn create_room(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<CreateRoomRequest>,
) -> AppResult<Json<CreateRoomResponse>> {
    tracing::info!(user_id = %auth.user_id, room_name = %req.name, "Creating new room");

    let response = state
        .client_api
        .create_room(&auth.user_id.to_string(), req)
        .await
        .map_err(|e| {
            tracing::error!(user_id = %auth.user_id, error = %e, "Failed to create room");
            super::error::map_api_error(e)
        })?;

    let room_id = response.room.as_ref().map_or("unknown", |r| r.id.as_str());
    tracing::info!(user_id = %auth.user_id, room_id = %room_id, "Room created successfully");
    Ok(Json(response))
}

/// Get room information
pub async fn get_room(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> AppResult<Json<GetRoomResponse>> {
    let response = state
        .client_api
        .get_room(&auth.user_id.to_string(), &room_id)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Join a room
pub async fn join_room(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<JoinRoomRequest>,
) -> AppResult<Json<JoinRoomResponse>> {
    let response = state
        .client_api
        .join_room(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Leave a room
pub async fn leave_room(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> AppResult<Json<LeaveRoomResponse>> {
    let response = state
        .client_api
        .leave_room(&auth.user_id.to_string(), &room_id)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Delete a room
#[tracing::instrument(name = "http_delete_room", skip(state), fields(user_id = %auth.user_id, room_id = %room_id))]
pub async fn delete_room(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> AppResult<Json<DeleteRoomResponse>> {
    tracing::info!(user_id = %auth.user_id, room_id = %room_id, "Deleting room");

    let response = state
        .client_api
        .delete_room(&auth.user_id.to_string(), &room_id)
        .await
        .map_err(|e| {
            tracing::error!(user_id = %auth.user_id, room_id = %room_id, error = %e, "Failed to delete room");
            super::error::map_api_error(e)
        })?;

    tracing::info!(user_id = %auth.user_id, room_id = %room_id, "Room deleted successfully");
    Ok(Json(response))
}

// ==================== Media Management Endpoints ====================

/// Add media to playlist
pub async fn add_media(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<AddMediaRequest>,
) -> AppResult<Json<AddMediaResponse>> {
    let response = state
        .client_api
        .add_media(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Delete media from playlist
pub async fn delete_media(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((room_id, media_id)): Path<(String, String)>,
) -> AppResult<Json<DeleteMediaResponse>> {
    let proto_req = DeleteMediaRequest { media_id };
    let response = state
        .client_api
        .delete_media(&auth.user_id.to_string(), &room_id, proto_req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Bulk delete media from playlist
#[tracing::instrument(name = "http_delete_media_batch", skip(state, req), fields(user_id = %auth.user_id, room_id = %room_id))]
pub async fn delete_media_batch(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<DeleteMediaBatchRequest>,
) -> AppResult<Json<DeleteMediaBatchResponse>> {
    let response = state
        .client_api
        .delete_media_batch(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(|e| {
            tracing::error!(user_id = %auth.user_id, room_id = %room_id, error = %e, "Failed to delete media batch");
            super::error::map_api_error(e)
        })?;

    Ok(Json(response))
}

/// Bulk reorder media items in playlist
#[tracing::instrument(name = "http_reorder_media_batch", skip(state, req), fields(user_id = %auth.user_id, room_id = %room_id))]
pub async fn reorder_media_batch(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<ReorderMediaBatchRequest>,
) -> AppResult<Json<ReorderMediaBatchResponse>> {
    let response = state
        .client_api
        .reorder_media_batch(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(|e| {
            tracing::error!(user_id = %auth.user_id, room_id = %room_id, error = %e, "Failed to reorder media batch");
            super::error::map_api_error(e)
        })?;

    Ok(Json(response))
}

/// List media items in room
pub async fn list_media(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> AppResult<Json<ListPlaylistResponse>> {
    let response = state
        .client_api
        .list_media(&auth.user_id.to_string(), &room_id)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Swap media items
pub async fn swap_media_items(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<SwapMediaRequest>,
) -> AppResult<Json<SwapMediaResponse>> {
    let response = state
        .client_api
        .swap_media(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

// ==================== Playback Control Endpoints ====================

/// Play (resume playback)
/// POST /`api/rooms/{room_id}/playback/start` - Start playback of a specific media
pub async fn start_playback(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<StartPlaybackRequest>,
) -> AppResult<Json<StartPlaybackResponse>> {
    let response = state
        .client_api
        .start_playback(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// POST /`api/rooms/{room_id}/playback/stop` - Stop current playback
pub async fn stop_playback(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<StopPlaybackRequest>,
) -> AppResult<Json<StopPlaybackResponse>> {
    let response = state
        .client_api
        .stop_playback(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// GET /`api/rooms/{room_id}/playback` - Get current playback state and complete playback information
pub async fn get_playback(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> AppResult<Json<GetPlaybackResponse>> {
    let response = state
        .client_api
        .get_playback(&auth.user_id.to_string(), &room_id, GetPlaybackRequest {})
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

// ==================== Room Members Endpoints ====================

/// Get room members
pub async fn get_room_members(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> AppResult<Json<GetRoomMembersResponse>> {
    let response = state
        .client_api
        .get_room_members(&auth.user_id.to_string(), &room_id)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

// ==================== Room Discovery & Public Endpoints ====================

/// Check if room exists (public endpoint)
pub async fn check_room(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> AppResult<Json<CheckRoomResponse>> {
    let req = crate::proto::client::CheckRoomRequest { room_id };
    let response = state
        .client_api
        .check_room(req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// List rooms (requires authentication to prevent anonymous enumeration)
pub async fn list_rooms(
    _auth: AuthUser,
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> AppResult<Json<ListRoomsResponse>> {
    let page: i32 = params.get("page").and_then(|v| v.parse().ok()).unwrap_or(1);
    let page_size: i32 = params.get("page_size").and_then(|v| v.parse().ok()).unwrap_or(50);
    let search = params.get("search").cloned().unwrap_or_default();

    let proto_req = ListRoomsRequest {
        page,
        page_size,
        search,
    };
    let response = state
        .client_api
        .list_rooms(proto_req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

// ==================== Room Settings Endpoints ====================

/// Set room password
pub async fn set_room_password(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<SetRoomPasswordRequest>,
) -> AppResult<Json<SetRoomPasswordResponse>> {
    let response = state
        .client_api
        .set_room_password(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Check room password (requires authentication to prevent brute force)
pub async fn check_password(
    _auth: AuthUser,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    connect_info: axum::extract::ConnectInfo<std::net::SocketAddr>,
    Path(room_id): Path<String>,
    Json(req): Json<CheckRoomPasswordRequest>,
) -> AppResult<Json<CheckRoomPasswordResponse>> {
    let client_ip = super::auth::extract_client_ip(&state.config, connect_info.0, &headers)
        .to_string();

    let response = state
        .client_api
        .check_room_password(&room_id, req, &client_ip)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Get room settings (requires authentication and room membership)
pub async fn get_room_settings(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> AppResult<Json<crate::proto::client::GetRoomSettingsResponse>> {
    let response = state
        .client_api
        .get_room_settings(&auth.user_id.to_string(), &room_id)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Push multiple media items to playlist
pub async fn push_media_batch(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<AddMediaBatchRequest>,
) -> AppResult<Json<crate::proto::client::AddMediaBatchResponse>> {
    let response = state
        .client_api
        .add_media_batch(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Edit media
pub async fn edit_media(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((room_id, media_id)): Path<(String, String)>,
    Json(mut req): Json<EditMediaRequest>,
) -> AppResult<Json<EditMediaResponse>> {
    req.media_id = media_id;
    let response = state
        .client_api
        .edit_media(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Clear playlist
pub async fn clear_playlist(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> AppResult<Json<ClearPlaylistResponse>> {
    let response = state
        .client_api
        .clear_playlist(&auth.user_id.to_string(), &room_id)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// GET /`api/rooms/:room_id/media/:media_id` - Get media record from database
pub async fn get_media(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((room_id, media_id)): Path<(String, String)>,
) -> AppResult<Json<crate::proto::client::Media>> {
    let media = state
        .client_api
        .get_media(auth.user_id.as_str(), &room_id, &media_id)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(media))
}

/// GET /`api/rooms/:room_id/playlists/:playlist_id` - Get single playlist info
pub async fn get_playlist(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((room_id, playlist_id)): Path<(String, String)>,
) -> AppResult<Json<crate::proto::client::GetPlaylistResponse>> {
    let response = state
        .client_api
        .get_playlist(auth.user_id.as_str(), &room_id, &playlist_id)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

// ==================== New RESTful Endpoints ====================

/// Unified handler for listing rooms (with query params) or getting single room by ID
/// GET /api/rooms (list) or GET /api/rooms?id=xxx (single)
pub async fn list_or_get_rooms(
    _auth: Option<AuthUser>,
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> AppResult<Json<ListRoomsResponse>> {
    // List rooms with optional filtering
    let search = params.get("search").cloned().unwrap_or_default();
    let limit: i32 = params.get("limit").and_then(|s| s.parse().ok()).unwrap_or(50).clamp(1, 100);
    let offset: i32 = params.get("offset").and_then(|s| s.parse().ok()).unwrap_or(0).max(0);

    let request = ListRoomsRequest {
        page: (offset / limit) + 1,
        page_size: limit,
        search,
    };

    let response = state
        .client_api
        .list_rooms(request)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Unified handler for updating room settings via PATCH
/// PATCH /`api/rooms/:room_id/settings`
///
/// PATCH semantics: only specified fields are updated; unspecified fields retain
/// their current values. Current settings are fetched first, then merged.
pub async fn update_room_settings(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<UpdateRoomSettingsRequest>,
) -> AppResult<Json<UpdateRoomSettingsResponse>> {
    let response = state
        .client_api
        .update_room_settings(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// HTTP-specific: Update playback request for PATCH endpoint
/// Dispatches to individual proto operations (play/pause/seek/speed/switch)
#[derive(serde::Deserialize)]
pub struct UpdatePlaybackRequest {
    /// "playing" or "paused"
    #[serde(default)]
    pub state: Option<String>,
    /// Seek position in seconds
    #[serde(default)]
    pub position: Option<f64>,
    /// Playback speed multiplier
    #[serde(default)]
    pub speed: Option<f64>,
    /// Switch to media ID
    #[serde(default)]
    pub media_id: Option<String>,
    /// Playlist context when switching media
    #[serde(default)]
    pub playlist_id: Option<String>,
    /// Expected version for optimistic locking (CAS).
    /// If provided, the update will only succeed if the current playback state
    /// version matches this value, preventing last-writer-wins conflicts.
    #[serde(default)]
    pub version: Option<i64>,
}

/// Unified handler for updating playback state via PATCH
/// PATCH /`api/rooms/:room_id/playback`
/// Supports: state (play/pause), position (seek), speed, `media_id` (switch), `playlist_id`
///
/// Applies ALL provided fields atomically via `PlaybackService::update_multiple()`.
pub async fn update_playback(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<UpdatePlaybackRequest>,
) -> AppResult<Json<GetPlaybackResponse>> {
    use synctv_core::models::{MediaId, PlaylistId, RoomId, UserId};

    let user_id = auth.user_id.to_string();

    // Validate that at least one field is provided
    if req.state.is_none() && req.position.is_none() && req.speed.is_none() && req.media_id.is_none() {
        return Err(super::AppError::bad_request(
            "No valid playback update field provided (state, position, speed, or media_id)"
        ));
    }

    // Translate "playing"/"paused" to bool
    let playing = match req.state.as_deref() {
        Some("playing") => Some(true),
        Some("paused") => Some(false),
        Some(_) => return Err(super::AppError::bad_request("Invalid state value, use 'playing' or 'paused'")),
        None => None,
    };

    let media_id = req.media_id.map(MediaId::from_string);
    let playlist_id = req.playlist_id.map(|pid| {
        if pid.is_empty() { None } else { Some(PlaylistId::from_string(pid)) }
    });

    let rid = RoomId::from_string(room_id.clone());
    let uid = UserId::from_string(user_id.clone());

    // Apply all fields atomically in a single DB update.
    // When a version is provided, the update uses optimistic locking (CAS)
    // to prevent concurrent modification conflicts.
    state.room_service.playback_service()
        .update_multiple_with_version(rid, uid, playing, req.position, req.speed, media_id, playlist_id, req.version)
        .await?;

    // Return final playback state and playback info
    let pb = state.client_api
        .get_playback(&user_id, &room_id, GetPlaybackRequest {})
        .await.map_err(super::error::map_api_error)?;
    Ok(Json(pb))
}

/// HTTP-specific: Media batch update request for PATCH endpoint
/// Dispatches to reorder or swap proto operations
#[derive(serde::Deserialize)]
pub struct UpdateMediaBatchRequest {
    /// Reorder operations: list of {`media_id`, position}
    #[serde(default)]
    pub reorder: Option<Vec<MediaReorderUpdate>>,
    /// Swap operation: {`media_id1`, `media_id2`}
    #[serde(default)]
    pub swap: Option<SwapMediaRequest>,
}

/// HTTP-specific: batch operation response
#[derive(serde::Serialize)]
pub struct BatchOperationResponse {
    pub success: bool,
}

/// Unified handler for media batch operations via PATCH
/// PATCH /`api/rooms/:room_id/media`
/// Supports: reorder, swap operations
pub async fn update_media_batch(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<UpdateMediaBatchRequest>,
) -> AppResult<Json<BatchOperationResponse>> {
    let user_id = auth.user_id.to_string();

    // Check for reorder operation
    if let Some(updates) = req.reorder {
        let proto_req = ReorderMediaBatchRequest { updates };
        let response = state.client_api
            .reorder_media_batch(&user_id, &room_id, proto_req)
            .await
            .map_err(super::error::map_api_error)?;

        return Ok(Json(BatchOperationResponse { success: response.success }));
    }

    // Check for swap operation
    if let Some(swap_req) = req.swap {
        let response = state.client_api
            .swap_media(&user_id, &room_id, swap_req)
            .await
            .map_err(super::error::map_api_error)?;

        return Ok(Json(BatchOperationResponse { success: response.success }));
    }

    Err(super::AppError::bad_request(
        "No valid batch operation provided (reorder or swap)"
    ))
}

// ==================== Room Settings Reset ====================

/// Reset room settings to defaults
/// POST /`api/rooms/:room_id/settings/reset`
pub async fn reset_room_settings(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> AppResult<Json<ResetRoomSettingsResponse>> {
    let response = state
        .client_api
        .reset_room_settings(&auth.user_id.to_string(), &room_id)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

// ==================== Chat History ====================

/// Get chat history for a room
/// GET /`api/rooms/:room_id/chat/history`
pub async fn get_chat_history(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> AppResult<Json<GetChatHistoryResponse>> {
    let limit = params.get("limit").and_then(|v| v.parse().ok()).unwrap_or(50i32).clamp(1, 100);
    let before = params.get("before").and_then(|v| v.parse().ok()).unwrap_or(0i64);

    // Read cursor for keyset pagination (takes precedence over before timestamp)
    let cursor = params.get("cursor").cloned().unwrap_or_default();
    let req = crate::proto::client::GetChatHistoryRequest { limit, before, cursor };
    let response = state
        .client_api
        .get_chat_history(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

// ==================== Playlist CRUD ====================

/// Create a playlist
/// POST /`api/rooms/:room_id/playlists`
pub async fn create_playlist(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<CreatePlaylistRequest>,
) -> AppResult<Json<CreatePlaylistResponse>> {
    let response = state
        .client_api
        .create_playlist(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Update a playlist
/// PATCH /`api/rooms/:room_id/playlists/:playlist_id`
pub async fn update_playlist(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((room_id, playlist_id)): Path<(String, String)>,
    Json(mut req): Json<UpdatePlaylistRequest>,
) -> AppResult<Json<UpdatePlaylistResponse>> {
    req.playlist_id = playlist_id;
    let response = state
        .client_api
        .update_playlist(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Delete a playlist
/// DELETE /`api/rooms/:room_id/playlists/:playlist_id`
pub async fn delete_playlist(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((room_id, playlist_id)): Path<(String, String)>,
) -> AppResult<Json<DeletePlaylistResponse>> {
    let req = DeletePlaylistRequest { playlist_id };
    let response = state
        .client_api
        .delete_playlist(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// List playlists in a room
/// GET /`api/rooms/:room_id/playlists`
pub async fn list_playlists(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> AppResult<Json<ListPlaylistsResponse>> {
    let parent_id = params.get("parent_id").cloned().unwrap_or_default();
    let page: i32 = params.get("page").and_then(|v| v.parse().ok()).unwrap_or(1);
    let page_size: i32 = params.get("page_size").and_then(|v| v.parse().ok()).unwrap_or(50);
    let req = crate::proto::client::ListPlaylistsRequest { parent_id, page, page_size };
    let response = state
        .client_api
        .list_playlists(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

// ==================== Public: Hot Rooms ====================

/// Get hot rooms (sorted by online count)
/// GET /api/rooms/hot
pub async fn get_hot_rooms(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> AppResult<Json<GetHotRoomsResponse>> {
    let limit = params.get("limit").and_then(|v| v.parse().ok()).unwrap_or(10i32).min(50);
    let req = crate::proto::client::GetHotRoomsRequest { limit };
    let response = state
        .client_api
        .get_hot_rooms(req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

#[cfg(test)]
mod tests {
    use super::UpdatePlaybackRequest;

    #[test]
    fn test_update_playback_request_deserialize_state_only() {
        let json = r#"{"state": "playing"}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.state.as_deref(), Some("playing"));
        assert!(req.position.is_none());
        assert!(req.speed.is_none());
        assert!(req.media_id.is_none());
    }

    #[test]
    fn test_update_playback_request_deserialize_position_only() {
        let json = r#"{"position": 42.5}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert!(req.state.is_none());
        assert!((req.position.unwrap() - 42.5).abs() < f64::EPSILON);
        assert!(req.speed.is_none());
        assert!(req.media_id.is_none());
    }

    #[test]
    fn test_update_playback_request_deserialize_speed_only() {
        let json = r#"{"speed": 2.0}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert!(req.state.is_none());
        assert!(req.position.is_none());
        assert!((req.speed.unwrap() - 2.0).abs() < f64::EPSILON);
        assert!(req.media_id.is_none());
    }

    #[test]
    fn test_update_playback_request_deserialize_media_id_only() {
        let json = r#"{"media_id": "media_abc123"}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert!(req.state.is_none());
        assert!(req.position.is_none());
        assert!(req.speed.is_none());
        assert_eq!(req.media_id.as_deref(), Some("media_abc123"));
    }

    #[test]
    fn test_update_playback_request_deserialize_combined() {
        let json = r#"{"state": "paused", "position": 10.0, "speed": 1.5, "media_id": "m1", "playlist_id": "pl1"}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.state.as_deref(), Some("paused"));
        assert!((req.position.unwrap() - 10.0).abs() < f64::EPSILON);
        assert!((req.speed.unwrap() - 1.5).abs() < f64::EPSILON);
        assert_eq!(req.media_id.as_deref(), Some("m1"));
        assert_eq!(req.playlist_id.as_deref(), Some("pl1"));
    }

    #[test]
    fn test_update_playback_request_deserialize_empty() {
        let json = r#"{}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert!(req.state.is_none());
        assert!(req.position.is_none());
        assert!(req.speed.is_none());
        assert!(req.media_id.is_none());
        assert!(req.playlist_id.is_none());
    }
}
