// Room management HTTP handlers
//
// Thin transport layer: delegates all business logic to the impls layer.
// Request and response types are proto-generated structs.

use axum::{
    extract::{Path, Query, State},
    Json,
};

use super::validation::{
    validate_id, validate_playback_position, validate_playback_speed, ValidationError,
};
use super::{middleware::AuthUser, AppResult, AppState};
use crate::proto::client::{
    AddMediaBatchRequest, AddMediaRequest, AddMediaResponse, CheckRoomPasswordRequest,
    CheckRoomPasswordResponse, CheckRoomResponse, ClearPlaylistResponse, CreatePlaylistRequest,
    CreatePlaylistResponse, CreateRoomRequest, CreateRoomResponse, DeleteEntriesRequest,
    DeleteEntriesResponse, DeleteMediaRequest, DeleteMediaResponse, DeletePlaylistRequest,
    DeletePlaylistResponse, DeleteRoomResponse, EditMediaRequest, EditMediaResponse,
    GetChatHistoryResponse, GetHotRoomsResponse, GetPlaybackRequest, GetPlaybackResponse,
    GetRoomMembersResponse, GetRoomResponse, JoinRoomRequest, JoinRoomResponse, LeaveRoomResponse,
    ListPlaylistsResponse, ListRoomsRequest, ListRoomsResponse, MediaReorderUpdate,
    ReorderMediaBatchRequest, ReorderMediaBatchResponse, ResetRoomSettingsResponse,
    SetRoomPasswordRequest, SetRoomPasswordResponse, StartPlaybackRequest, StartPlaybackResponse,
    StopPlaybackRequest, StopPlaybackResponse, SwapMediaRequest, SwapMediaResponse,
    UpdatePlaylistRequest, UpdatePlaylistResponse, UpdateRoomSettingsRequest,
    UpdateRoomSettingsResponse,
};

pub type JoinRoomBody = JoinRoomRequest;
pub type SetRoomPasswordBody = SetRoomPasswordRequest;
pub type CheckRoomPasswordBody = CheckRoomPasswordRequest;
pub type UpdateRoomSettingsBody = UpdateRoomSettingsRequest;
pub type StartPlaybackBody = StartPlaybackRequest;
pub type StopPlaybackBody = StopPlaybackRequest;
pub type AddMediaBody = AddMediaRequest;
pub type DeleteEntriesBody = DeleteEntriesRequest;

fn parse_force_query(params: &std::collections::HashMap<String, String>) -> bool {
    params.get("force").is_some_and(|value| value == "true")
}

pub type ListPlaylistItemsBody = crate::proto::client::ListPlaylistItemsRequest;
pub type ReorderMediaBatchBody = ReorderMediaBatchRequest;
pub type SwapMediaBody = SwapMediaRequest;
pub type AddMediaBatchBody = AddMediaBatchRequest;
pub type EditMediaBody = EditMediaRequest;
pub type CreatePlaylistBody = CreatePlaylistRequest;
pub type UpdatePlaylistBody = UpdatePlaylistRequest;

fn map_validation_error(err: ValidationError) -> super::AppError {
    match err {
        ValidationError::TooLong { field, .. } => {
            super::AppError::validation_failed(field, "value is too long")
        }
        ValidationError::TooShort { field, .. } => {
            super::AppError::validation_failed(field, "value is too short")
        }
        ValidationError::InvalidFormat { field } => {
            super::AppError::validation_failed(field, "contains invalid characters")
        }
        ValidationError::InvalidValue(message) => super::AppError::bad_request(message),
        ValidationError::Required(field) => {
            super::AppError::validation_failed(field, "field is required")
        }
        ValidationError::SecurityRisk => {
            super::AppError::bad_request("Potential security issue detected in input")
        }
    }
}

// ==================== Room Management Endpoints ====================

/// Create a new room
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms",
        tag = "Room",
        request_body = CreateRoomRequest,
        responses(
            (status = 200, description = "Room created", body = CreateRoomResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
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
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "Room details", body = GetRoomResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Room not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
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
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        put,
        path = "/api/rooms/{room_id}/members/@me",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = JoinRoomRequest,
        responses(
            (status = 200, description = "Joined room", body = JoinRoomResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Permission denied", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Room not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn join_room(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(mut req): Json<JoinRoomBody>,
) -> AppResult<Json<JoinRoomResponse>> {
    req.room_id = room_id.clone();
    let response = state
        .client_api
        .join_room(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Leave a room
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/rooms/{room_id}/members/@me",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "Left room", body = LeaveRoomResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Room not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
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
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/rooms/{room_id}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "Room deleted", body = DeleteRoomResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Permission denied", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Room not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
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
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/media",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = AddMediaRequest,
        responses(
            (status = 200, description = "Media added", body = AddMediaResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn add_media(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<AddMediaBody>,
) -> AppResult<Json<AddMediaResponse>> {
    let response = state
        .client_api
        .add_media(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Delete media from playlist
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/rooms/{room_id}/media/{media_id}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("media_id" = String, Path, description = "Media ID"),
            ("force" = Option<bool>, Query, description = "Force delete")
        ),
        responses(
            (status = 200, description = "Media deleted", body = DeleteMediaResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Media not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn delete_media(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((room_id, media_id)): Path<(String, String)>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> AppResult<Json<DeleteMediaResponse>> {
    let force = parse_force_query(&params);
    let proto_req = DeleteMediaRequest { media_id, force };
    let response = state
        .client_api
        .delete_media(&auth.user_id.to_string(), &room_id, proto_req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Delete a mixed set of playlist and media entries.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/rooms/{room_id}/entries",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = DeleteEntriesRequest,
        responses(
            (status = 200, description = "Entries deleted", body = DeleteEntriesResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
#[tracing::instrument(name = "http_delete_entries", skip(state, req), fields(user_id = %auth.user_id, room_id = %room_id))]
pub async fn delete_entries(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<DeleteEntriesBody>,
) -> AppResult<Json<DeleteEntriesResponse>> {
    let response = state
        .client_api
        .delete_entries(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(|e| {
            tracing::error!(user_id = %auth.user_id, room_id = %room_id, error = %e, "Failed to delete entries");
            super::error::map_api_error(e)
        })?;

    Ok(Json(response))
}

/// Bulk reorder media items in playlist
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/media/reorder",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = ReorderMediaBatchRequest,
        responses(
            (status = 200, description = "Media reordered", body = ReorderMediaBatchResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
#[tracing::instrument(name = "http_reorder_media_batch", skip(state, req), fields(user_id = %auth.user_id, room_id = %room_id))]
pub async fn reorder_media_batch(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<ReorderMediaBatchBody>,
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

/// List items for a room root, static playlist, or dynamic playlist target.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/media/list",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = crate::proto::client::ListPlaylistItemsRequest,
        responses(
            (status = 200, description = "Playlist items", body = crate::proto::client::ListPlaylistItemsResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn list_playlist_items(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(mut req): Json<ListPlaylistItemsBody>,
) -> AppResult<Json<crate::proto::client::ListPlaylistItemsResponse>> {
    req.page = super::validation::validate_page((req.page != 0).then_some(req.page));
    req.page_size =
        super::validation::validate_page_size((req.page_size != 0).then_some(req.page_size));

    let response = state
        .client_api
        .list_playlist_items(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Swap media items
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/media/swap",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = SwapMediaRequest,
        responses(
            (status = 200, description = "Media swapped", body = SwapMediaResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn swap_media_items(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<SwapMediaBody>,
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
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/playback/start",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = StartPlaybackRequest,
        responses(
            (status = 200, description = "Playback started", body = StartPlaybackResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn start_playback(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<StartPlaybackBody>,
) -> AppResult<Json<StartPlaybackResponse>> {
    let response = state
        .client_api
        .start_playback(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// POST /`api/rooms/{room_id}/playback/stop` - Stop current playback
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/playback/stop",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = StopPlaybackRequest,
        responses(
            (status = 200, description = "Playback stopped", body = StopPlaybackResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn stop_playback(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<StopPlaybackBody>,
) -> AppResult<Json<StopPlaybackResponse>> {
    let response = state
        .client_api
        .stop_playback(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// GET /`api/rooms/{room_id}/playback` - Get current playback state and complete playback information
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/playback",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "Current playback state", body = GetPlaybackResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Room not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
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

/// Pagination query for room members.
#[derive(serde::Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct MembersQueryParams {
    page: Option<i32>,
    page_size: Option<i32>,
    search: Option<String>,
    role: Option<String>,
    status: Option<String>,
    sort_by: Option<String>,
    sort_direction: Option<String>,
}

/// Get room members (E8: with pagination)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/members",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            MembersQueryParams
        ),
        responses(
            (status = 200, description = "Room members", body = GetRoomMembersResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Room not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_room_members(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Query(q): Query<MembersQueryParams>,
) -> AppResult<Json<GetRoomMembersResponse>> {
    let (page, page_size) = super::validation::validate_pagination(q.page, q.page_size);
    let response = state
        .client_api
        .get_room_members(
            &auth.user_id.to_string(),
            &room_id,
            crate::proto::client::GetRoomMembersRequest {
                page,
                page_size,
                search: q.search.unwrap_or_default(),
                role: match q.role.as_deref() {
                    Some("guest") => Some(synctv_proto::common::RoomMemberRole::Guest as i32),
                    Some("member") => Some(synctv_proto::common::RoomMemberRole::Member as i32),
                    Some("admin") => Some(synctv_proto::common::RoomMemberRole::Admin as i32),
                    Some("creator") => Some(synctv_proto::common::RoomMemberRole::Creator as i32),
                    _ => None,
                },
                status: match q.status.as_deref() {
                    Some("active") => Some(synctv_proto::common::MemberStatus::Active as i32),
                    Some("pending") => Some(synctv_proto::common::MemberStatus::Pending as i32),
                    Some("banned") => Some(synctv_proto::common::MemberStatus::Banned as i32),
                    Some("left") => Some(synctv_proto::common::MemberStatus::Left as i32),
                    _ => None,
                },
                sort_by: match q.sort_by.as_deref() {
                    Some("username") => crate::proto::client::RoomMemberListSortBy::Username as i32,
                    Some("role") => crate::proto::client::RoomMemberListSortBy::Role as i32,
                    Some("status") => crate::proto::client::RoomMemberListSortBy::Status as i32,
                    _ => crate::proto::client::RoomMemberListSortBy::JoinedAt as i32,
                },
                sort_direction: match q.sort_direction.as_deref() {
                    Some("desc") => crate::proto::client::SortDirection::Desc as i32,
                    _ => crate::proto::client::SortDirection::Asc as i32,
                },
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

// ==================== Room Discovery & Public Endpoints ====================

/// Check if room exists (public endpoint)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/check",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "Room availability and status", body = CheckRoomResponse),
            (status = 404, description = "Room not found", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
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

/// Maximum allowed search query length to prevent abuse.
const LIST_ROOMS_MAX_SEARCH_LENGTH: usize = 100;

#[derive(serde::Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct ListRoomsQueryParams {
    pub search: Option<String>,
    pub limit: Option<i32>,
    pub offset: Option<i32>,
}

/// List rooms (requires authentication to prevent anonymous enumeration)
pub async fn list_rooms(
    _auth: AuthUser,
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> AppResult<Json<ListRoomsResponse>> {
    // Parse and validate page and page_size parameters using centralized validation
    let page_opt = params.get("page").and_then(|v| v.parse().ok());
    let page_size_opt = params.get("page_size").and_then(|v| v.parse().ok());
    let (page, page_size) = super::validation::validate_pagination(page_opt, page_size_opt);

    // Validate search parameter length
    let search = params.get("search").cloned().unwrap_or_default();
    if search.len() > LIST_ROOMS_MAX_SEARCH_LENGTH {
        return Err(super::error::AppError::bad_request(format!(
            "search query must not exceed {LIST_ROOMS_MAX_SEARCH_LENGTH} characters"
        )));
    }

    let proto_req = ListRoomsRequest {
        page,
        page_size,
        search,
        sort_by: match params.get("sort_by").map(String::as_str) {
            Some("name") => crate::proto::client::RoomListSortBy::Name as i32,
            Some("updated_at") => crate::proto::client::RoomListSortBy::UpdatedAt as i32,
            Some("last_activity_at") => crate::proto::client::RoomListSortBy::LastActivityAt as i32,
            _ => crate::proto::client::RoomListSortBy::CreatedAt as i32,
        },
        sort_direction: match params.get("sort_direction").map(String::as_str) {
            Some("asc") => crate::proto::client::SortDirection::Asc as i32,
            _ => crate::proto::client::SortDirection::Desc as i32,
        },
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
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/rooms/{room_id}/password",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = SetRoomPasswordRequest,
        responses(
            (status = 200, description = "Room password updated", body = SetRoomPasswordResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn set_room_password(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<SetRoomPasswordBody>,
) -> AppResult<Json<SetRoomPasswordResponse>> {
    let response = state
        .client_api
        .set_room_password(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Check room password (requires authentication to prevent brute force)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/password/verify",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = CheckRoomPasswordRequest,
        responses(
            (status = 200, description = "Password verification result", body = CheckRoomPasswordResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn check_password(
    _auth: AuthUser,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    connect_info: axum::extract::ConnectInfo<std::net::SocketAddr>,
    Path(room_id): Path<String>,
    Json(mut req): Json<CheckRoomPasswordBody>,
) -> AppResult<Json<CheckRoomPasswordResponse>> {
    let client_ip =
        super::auth::extract_client_ip(&state.config, connect_info.0, &headers).to_string();
    req.room_id = room_id.clone();

    let response = state
        .client_api
        .check_room_password(&room_id, req, &client_ip)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Get room settings (requires authentication and room membership)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/settings",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "Room settings", body = crate::proto::client::GetRoomSettingsResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Room not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
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
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/media/batch",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = AddMediaBatchRequest,
        responses(
            (status = 200, description = "Batch media added", body = crate::proto::client::AddMediaBatchResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn push_media_batch(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<AddMediaBatchBody>,
) -> AppResult<Json<crate::proto::client::AddMediaBatchResponse>> {
    let response = state
        .client_api
        .add_media_batch(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Edit media
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/rooms/{room_id}/media/{media_id}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("media_id" = String, Path, description = "Media ID")
        ),
        request_body = EditMediaRequest,
        responses(
            (status = 200, description = "Media updated", body = EditMediaResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn edit_media(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((room_id, media_id)): Path<(String, String)>,
    Json(mut req): Json<EditMediaBody>,
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
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/rooms/{room_id}/media",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "Playlist cleared", body = ClearPlaylistResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
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
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/media/{media_id}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("media_id" = String, Path, description = "Media ID")
        ),
        responses(
            (status = 200, description = "Media details", body = crate::proto::client::Media),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Media not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
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
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/playlists/{playlist_id}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("playlist_id" = String, Path, description = "Playlist ID")
        ),
        responses(
            (status = 200, description = "Playlist details", body = crate::proto::client::GetPlaylistResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Playlist not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
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
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms",
        tag = "Room",
        params(ListRoomsQueryParams),
        responses(
            (status = 200, description = "Rooms list", body = ListRoomsResponse),
            (status = 400, description = "Invalid query", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn list_or_get_rooms(
    _auth: AuthUser,
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> AppResult<Json<ListRoomsResponse>> {
    // List rooms with optional filtering
    let search = params.get("search").cloned().unwrap_or_default();
    let limit: i32 = params
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(50)
        .clamp(1, 100);
    let offset: i32 = params
        .get("offset")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
        .max(0);

    // R-8: Validate that offset is aligned to limit to prevent incorrect page
    // calculations. Non-aligned offsets would silently round down and return
    // the wrong page of results.
    if offset % limit != 0 {
        return Err(super::AppError::bad_request(format!(
            "offset ({offset}) must be a multiple of limit ({limit})"
        )));
    }

    let request = ListRoomsRequest {
        page: (offset / limit) + 1,
        page_size: limit,
        search,
        sort_by: match params.get("sort_by").map(String::as_str) {
            Some("name") => crate::proto::client::RoomListSortBy::Name as i32,
            Some("updated_at") => crate::proto::client::RoomListSortBy::UpdatedAt as i32,
            Some("last_activity_at") => crate::proto::client::RoomListSortBy::LastActivityAt as i32,
            _ => crate::proto::client::RoomListSortBy::CreatedAt as i32,
        },
        sort_direction: match params.get("sort_direction").map(String::as_str) {
            Some("asc") => crate::proto::client::SortDirection::Asc as i32,
            _ => crate::proto::client::SortDirection::Desc as i32,
        },
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
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/rooms/{room_id}/settings",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = UpdateRoomSettingsRequest,
        responses(
            (status = 200, description = "Room settings updated", body = UpdateRoomSettingsResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn update_room_settings(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<UpdateRoomSettingsBody>,
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
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
    /// Switch to dynamic playlist item
    #[serde(default)]
    pub playlist_id: Option<String>,
    /// Provider-facing playback target payload
    #[serde(default)]
    pub target: Option<serde_json::Value>,
    /// Expected version for optimistic locking (CAS).
    /// If provided, the update will only succeed if the current playback state
    /// version matches this value, preventing last-writer-wins conflicts.
    #[serde(default)]
    pub version: Option<i64>,
}

fn is_switch_request(req: &UpdatePlaybackRequest) -> bool {
    req.media_id.is_some() || req.playlist_id.is_some() || req.target.is_some()
}

fn normalize_switch_id(value: Option<&str>) -> Option<String> {
    value.and_then(|raw| {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

/// Unified handler for updating playback state via PATCH
/// PATCH /`api/rooms/:room_id/playback`
/// Supports either:
/// - play/pause/seek/speed updates
/// - playback target switch (`media_id` or `playlist_id` + `target`)
///
/// Target switches intentionally use `PlaybackService::switch()` and cannot be
/// mixed with other playback state updates.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/rooms/{room_id}/playback",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = UpdatePlaybackRequest,
        responses(
            (status = 200, description = "Playback updated", body = GetPlaybackResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn update_playback(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<UpdatePlaybackRequest>,
) -> AppResult<Json<GetPlaybackResponse>> {
    use synctv_core::models::{MediaId, PlaylistId, RoomId, UserId};

    let user_id = auth.user_id.to_string();

    // Validate that at least one field is provided
    let target_requested = is_switch_request(&req);

    if req.state.is_none() && req.position.is_none() && req.speed.is_none() && !target_requested {
        return Err(super::AppError::bad_request(
            "No valid playback update field provided (state, position, speed, media_id, or playlist_id)",
        ));
    }

    // Translate "playing"/"paused" to bool
    let playing = match req.state.as_deref() {
        Some("playing") => Some(true),
        Some("paused") => Some(false),
        Some(_) => {
            return Err(super::AppError::bad_request(
                "Invalid state value, use 'playing' or 'paused'",
            ))
        }
        None => None,
    };

    if let Some(position) = req.position {
        validate_playback_position(position).map_err(map_validation_error)?;
    }
    if let Some(speed) = req.speed {
        validate_playback_speed(speed).map_err(map_validation_error)?;
    }
    if let Some(media_id) = normalize_switch_id(req.media_id.as_deref()) {
        validate_id(&media_id, "media_id").map_err(map_validation_error)?;
    }
    if let Some(playlist_id) = normalize_switch_id(req.playlist_id.as_deref()) {
        validate_id(&playlist_id, "playlist_id").map_err(map_validation_error)?;
    }

    let rid = RoomId::from_string(room_id.clone());
    let uid = UserId::from_string(user_id.clone());

    if target_requested {
        if req.state.is_some()
            || req.position.is_some()
            || req.speed.is_some()
            || req.version.is_some()
        {
            return Err(super::AppError::bad_request(
                "Target switch requests cannot be combined with play/pause/seek/speed/version updates",
            ));
        }

        let media_id = normalize_switch_id(req.media_id.as_deref()).map(MediaId::from_string);
        let playlist_id =
            normalize_switch_id(req.playlist_id.as_deref()).map(PlaylistId::from_string);
        let target = req
            .target
            .map(|value| {
                serde_json::to_vec(&value).map_err(|e| {
                    super::AppError::bad_request(format!("Invalid target payload: {e}"))
                })
            })
            .transpose()?
            .unwrap_or_default();

        state
            .room_service
            .playback_service()
            .switch(rid, uid, media_id, playlist_id, target)
            .await?;
    } else {
        // Apply all non-target fields atomically in a single DB update.
        // When a version is provided, the update uses optimistic locking (CAS)
        // to prevent concurrent modification conflicts.
        state
            .room_service
            .playback_service()
            .update_multiple_with_version(rid, uid, playing, req.position, req.speed, req.version)
            .await?;
    }

    // Return final playback state and playback info
    let pb = state
        .client_api
        .get_playback(&user_id, &room_id, GetPlaybackRequest {})
        .await
        .map_err(super::error::map_api_error)?;
    Ok(Json(pb))
}

/// HTTP-specific: Media batch update request for PATCH endpoint
/// Dispatches to reorder or swap proto operations
#[derive(serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpdateMediaBatchRequest {
    /// Reorder operations: list of {`media_id`, position}
    #[serde(default)]
    pub reorder: Option<Vec<MediaReorderUpdate>>,
    /// Swap operation: {`media_id1`, `media_id2`}
    #[serde(default)]
    pub swap: Option<SwapMediaBody>,
}

/// HTTP-specific: batch operation response
#[derive(serde::Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct BatchOperationResponse {
    pub success: bool,
}

/// Unified handler for media batch operations via PATCH
/// PATCH /`api/rooms/:room_id/media`
/// Supports: reorder, swap operations
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/rooms/{room_id}/media",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = UpdateMediaBatchRequest,
        responses(
            (status = 200, description = "Batch media operation applied", body = BatchOperationResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
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
        let response = state
            .client_api
            .reorder_media_batch(&user_id, &room_id, proto_req)
            .await
            .map_err(super::error::map_api_error)?;

        return Ok(Json(BatchOperationResponse {
            success: response.success,
        }));
    }

    // Check for swap operation
    if let Some(swap_req) = req.swap {
        let proto_req = SwapMediaRequest {
            media_id1: swap_req.media_id1,
            media_id2: swap_req.media_id2,
        };
        let response = state
            .client_api
            .swap_media(&user_id, &room_id, proto_req)
            .await
            .map_err(super::error::map_api_error)?;

        return Ok(Json(BatchOperationResponse {
            success: response.success,
        }));
    }

    Err(super::AppError::bad_request(
        "No valid batch operation provided (reorder or swap)",
    ))
}

// ==================== Room Settings Reset ====================

/// Reset room settings to defaults
/// POST /`api/rooms/:room_id/settings/reset`
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/settings/reset",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "Room settings reset", body = ResetRoomSettingsResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
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

#[derive(serde::Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct ChatHistoryQueryParams {
    pub limit: Option<i32>,
    pub cursor: Option<String>,
}

/// Get chat history for a room
/// GET /`api/rooms/:room_id/chat/history`
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/chat/history",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ChatHistoryQueryParams
        ),
        responses(
            (status = 200, description = "Chat history", body = GetChatHistoryResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_chat_history(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> AppResult<Json<GetChatHistoryResponse>> {
    let req = parse_chat_history_request_params(&params)?;
    let response = state
        .client_api
        .get_chat_history(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

fn parse_chat_history_request_params(
    params: &std::collections::HashMap<String, String>,
) -> AppResult<crate::proto::client::GetChatHistoryRequest> {
    if params.contains_key("before") {
        return Err(super::AppError::bad_request(
            "The 'before' query parameter is no longer supported; use 'cursor' instead",
        ));
    }

    let limit = params
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(50i32)
        .clamp(1, 100);

    Ok(crate::proto::client::GetChatHistoryRequest {
        limit,
        cursor: params.get("cursor").cloned().unwrap_or_default(),
    })
}

// ==================== Playlist CRUD ====================

/// Create a playlist
/// POST /`api/rooms/:room_id/playlists`
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/playlists",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = CreatePlaylistRequest,
        responses(
            (status = 200, description = "Playlist created", body = CreatePlaylistResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn create_playlist(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<CreatePlaylistBody>,
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
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/rooms/{room_id}/playlists/{playlist_id}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("playlist_id" = String, Path, description = "Playlist ID")
        ),
        request_body = UpdatePlaylistRequest,
        responses(
            (status = 200, description = "Playlist updated", body = UpdatePlaylistResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn update_playlist(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((room_id, playlist_id)): Path<(String, String)>,
    Json(mut req): Json<UpdatePlaylistBody>,
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
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/rooms/{room_id}/playlists/{playlist_id}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("playlist_id" = String, Path, description = "Playlist ID"),
            ("force" = Option<bool>, Query, description = "Force delete")
        ),
        responses(
            (status = 200, description = "Playlist deleted", body = DeletePlaylistResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Playlist not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn delete_playlist(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((room_id, playlist_id)): Path<(String, String)>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> AppResult<Json<DeletePlaylistResponse>> {
    let force = parse_force_query(&params);
    let req = DeletePlaylistRequest { playlist_id, force };
    let response = state
        .client_api
        .delete_playlist(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// List playlists in a room
/// GET /`api/rooms/:room_id/playlists`
#[derive(serde::Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct ListPlaylistsQueryParams {
    pub parent_id: Option<String>,
    pub page: Option<i32>,
    pub page_size: Option<i32>,
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/playlists",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ListPlaylistsQueryParams
        ),
        responses(
            (status = 200, description = "Playlists in room", body = ListPlaylistsResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn list_playlists(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> AppResult<Json<ListPlaylistsResponse>> {
    let parent_id = params.get("parent_id").cloned().unwrap_or_default();
    let page_opt = params.get("page").and_then(|v| v.parse().ok());
    let page_size_opt = params.get("page_size").and_then(|v| v.parse().ok());
    let (page, page_size) = super::validation::validate_pagination(page_opt, page_size_opt);
    let req = crate::proto::client::ListPlaylistsRequest {
        parent_id,
        page,
        page_size,
        search: params.get("search").cloned().unwrap_or_default(),
        source_provider: params.get("source_provider").cloned().unwrap_or_default(),
        provider_instance_name: params
            .get("provider_instance_name")
            .cloned()
            .unwrap_or_default(),
        dynamic_only: params
            .get("dynamic_only")
            .and_then(|value| value.parse::<bool>().ok()),
        sort_by: match params.get("sort_by").map(String::as_str) {
            Some("name") => crate::proto::client::PlaylistListSortBy::Name as i32,
            Some("created_at") => crate::proto::client::PlaylistListSortBy::CreatedAt as i32,
            Some("updated_at") => crate::proto::client::PlaylistListSortBy::UpdatedAt as i32,
            _ => crate::proto::client::PlaylistListSortBy::Position as i32,
        },
        sort_direction: match params.get("sort_direction").map(String::as_str) {
            Some("desc") => crate::proto::client::SortDirection::Desc as i32,
            _ => crate::proto::client::SortDirection::Asc as i32,
        },
    };
    let response = state
        .client_api
        .list_playlists(&auth.user_id.to_string(), &room_id, req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

// ==================== Public: Hot Rooms ====================

#[derive(serde::Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct HotRoomsQueryParams {
    pub limit: Option<i32>,
}

/// Get hot rooms (sorted by online count)
/// GET /api/rooms/hot
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/hot",
        tag = "Room",
        params(HotRoomsQueryParams),
        responses(
            (status = 200, description = "Hot rooms", body = GetHotRoomsResponse)
        )
    )
)]
pub async fn get_hot_rooms(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> AppResult<Json<GetHotRoomsResponse>> {
    let limit = params
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(10i32)
        .min(50);
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
    use std::collections::HashMap;

    use super::{
        is_switch_request, normalize_switch_id, parse_chat_history_request_params,
        parse_force_query, AddMediaBatchBody, CreatePlaylistBody, DeleteEntriesBody,
        ListPlaylistItemsBody, MembersQueryParams, UpdateMediaBatchRequest, UpdatePlaybackRequest,
    };

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
        assert!(req.playlist_id.is_none());
        assert!(req.target.is_none());
    }

    #[test]
    fn test_members_query_params_deserialize_sorting_and_filters() {
        let json = r#"{"page":2,"page_size":25,"search":"alice","role":"admin","sort_by":"username","sort_direction":"asc"}"#;
        let query: MembersQueryParams = serde_json::from_str(json).expect("deserialize");
        assert_eq!(query.page, Some(2));
        assert_eq!(query.page_size, Some(25));
        assert_eq!(query.search.as_deref(), Some("alice"));
        assert_eq!(query.role.as_deref(), Some("admin"));
        assert_eq!(query.sort_by.as_deref(), Some("username"));
        assert_eq!(query.sort_direction.as_deref(), Some("asc"));
    }

    #[test]
    fn test_update_playback_request_deserialize_dynamic_target() {
        let json = r#"{"playlist_id": "pl1", "target": {"item_id": "provider-item-123"}}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.playlist_id.as_deref(), Some("pl1"));
        assert_eq!(
            req.target,
            Some(serde_json::json!({"item_id": "provider-item-123"}))
        );
        assert!(req.media_id.is_none());
        assert!(req.state.is_none());
        assert!(req.position.is_none());
        assert!(req.speed.is_none());
    }

    #[test]
    fn test_update_playback_empty_switch_ids_are_treated_as_clear_request() {
        let json = r#"{"media_id":"","playlist_id":""}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert!(is_switch_request(&req));
        assert_eq!(normalize_switch_id(req.media_id.as_deref()), None);
        assert_eq!(normalize_switch_id(req.playlist_id.as_deref()), None);
    }

    #[test]
    fn test_update_playback_omitted_switch_fields_are_not_switch_request() {
        let json = r#"{"state":"paused"}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert!(!is_switch_request(&req));
    }

    #[test]
    fn test_list_playlist_items_body_deserialize_room_root() {
        let json = r#"{}"#;
        let req: ListPlaylistItemsBody = serde_json::from_str(json).unwrap();
        assert!(req.playlist_id.is_empty());
        assert!(req.target.is_empty());
        assert_eq!(req.page, 0);
        assert_eq!(req.page_size, 0);
    }

    #[test]
    fn test_list_playlist_items_body_deserialize_dynamic_target() {
        let json =
            r#"{"playlist_id":"pl1","target":{"cursor":"season-1"},"page":2,"page_size":25}"#;
        let req: ListPlaylistItemsBody = serde_json::from_str(json).unwrap();
        assert_eq!(req.playlist_id, "pl1");
        let target: serde_json::Value = serde_json::from_slice(&req.target).unwrap();
        assert_eq!(target, serde_json::json!({"cursor":"season-1"}));
        assert_eq!(req.page, 2);
        assert_eq!(req.page_size, 25);
    }

    #[test]
    fn test_update_playback_request_deserialize_with_version() {
        let json = r#"{"state": "playing", "version": 42}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.state.as_deref(), Some("playing"));
        assert_eq!(req.version, Some(42));
    }

    #[test]
    fn test_add_media_batch_body_deserializes_without_room_id_in_nested_items() {
        let json = r#"{
            "items": [
                {
                    "playlist_id": "playlist-1",
                    "provider": "yt-dlp",
                    "provider_instance_name": "default",
                    "source_config": [1, 2, 3],
                    "title": "Example"
                }
            ]
        }"#;
        let body: AddMediaBatchBody = serde_json::from_str(json).unwrap();
        assert_eq!(body.items.len(), 1);
    }

    #[test]
    fn test_update_media_batch_request_deserializes_swap_without_room_id() {
        let json = r#"{
            "swap": {
                "media_id1": "media-1",
                "media_id2": "media-2"
            }
        }"#;
        let req: UpdateMediaBatchRequest = serde_json::from_str(json).unwrap();
        let swap = req.swap.expect("swap operation should deserialize");
        assert_eq!(swap.media_id1, "media-1");
        assert_eq!(swap.media_id2, "media-2");
    }

    #[test]
    fn test_parse_chat_history_request_rejects_before_param() {
        let params = HashMap::from([
            ("limit".to_string(), "20".to_string()),
            ("before".to_string(), "1710000000".to_string()),
        ]);

        let err = parse_chat_history_request_params(&params).expect_err("before must be rejected");

        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(
            err.message.contains("no longer supported"),
            "unexpected message: {}",
            err.message
        );
    }

    #[test]
    fn test_parse_chat_history_request_accepts_cursor_only() {
        let params = HashMap::from([
            ("limit".to_string(), "20".to_string()),
            (
                "cursor".to_string(),
                "2026-03-31T12:00:00+00:00|msg_123".to_string(),
            ),
        ]);

        let req = parse_chat_history_request_params(&params).expect("cursor-only request");

        assert_eq!(req.limit, 20);
        assert_eq!(req.cursor, "2026-03-31T12:00:00+00:00|msg_123");
    }

    #[test]
    fn test_parse_force_query_accepts_true_only() {
        let params = HashMap::from([("force".to_string(), "true".to_string())]);
        assert!(parse_force_query(&params));

        let params = HashMap::from([("force".to_string(), "false".to_string())]);
        assert!(!parse_force_query(&params));

        let params = HashMap::from([("force".to_string(), "1".to_string())]);
        assert!(!parse_force_query(&params));
    }

    #[test]
    fn test_delete_entries_body_deserializes_force_true() {
        let body: DeleteEntriesBody = serde_json::from_str(
            r#"{"playlist_ids":["playlist-1"],"media_ids":["media-1"],"force":true}"#,
        )
        .unwrap();

        assert_eq!(body.playlist_ids, vec!["playlist-1"]);
        assert_eq!(body.media_ids, vec!["media-1"]);
        assert!(body.force);
    }

    #[test]
    fn test_create_playlist_body_deserializes_dynamic_fields() {
        let body: CreatePlaylistBody = serde_json::from_str(
            r#"{
                "name":"Dynamic Folder",
                "parent_id":"playlist-root",
                "source_provider":"alist",
                "source_config":{"path":"/tv"},
                "provider_instance_name":"alist-main"
            }"#,
        )
        .unwrap();

        assert_eq!(body.name, "Dynamic Folder");
        assert_eq!(body.parent_id, "playlist-root");
        assert_eq!(body.source_provider, "alist");
        let source_config: serde_json::Value = serde_json::from_slice(&body.source_config).unwrap();
        assert_eq!(source_config, serde_json::json!({"path":"/tv"}));
        assert_eq!(body.provider_instance_name, "alist-main");
    }
}
