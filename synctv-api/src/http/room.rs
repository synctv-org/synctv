// Room management HTTP handlers
// Thin transport layer: delegates all business logic to the impls layer.
// Request and response types are proto-generated structs.

use axum::{
    extract::{Path, RawQuery, State},
    Json,
};
use futures::future::BoxFuture;
use futures::FutureExt;
use std::future::Future;
use synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT;

use super::validation::ValidatedQuery;
use super::{middleware::RequestMetadata, AppResult, AppState, WithMediaId, WithPlaylistId};
use crate::impls::EndpointRateLimitCategory;
use crate::proto::client::{
    AddMediaBatchRequest, AddMediaRequest, AddMediaResponse, CheckRoomResponse,
    ClearPlaylistResponse, CreatePlaylistRequest, CreatePlaylistResponse, CreateRoomRequest,
    CreateRoomResponse, DeleteEntriesRequest, DeleteEntriesResponse, DeleteMediaQuery,
    DeleteMediaRequest, DeleteMediaResponse, DeletePlaylistQuery, DeletePlaylistRequest,
    DeletePlaylistResponse, DeleteRoomResponse, EditMediaRequest, EditMediaResponse,
    GetChatHistoryRequest, GetChatHistoryResponse, GetHotRoomsRequest, GetHotRoomsResponse,
    GetPlaybackRequest, GetPlaybackResponse, GetRoomMembersRequest, GetRoomMembersResponse,
    GetRoomResponse, JoinRoomRequest, JoinRoomResponse, LeaveRoomResponse,
    ListPlaylistItemsRequest, ListPlaylistsRequest, ListPlaylistsResponse, ListRoomsRequest,
    ListRoomsResponse, MoveMediaRequest, MoveMediaResponse, MovePlaylistRequest,
    MovePlaylistResponse, ResetRoomSettingsResponse, SetRoomPasswordRequest,
    SetRoomPasswordResponse, StartPlaybackRequest, StartPlaybackResponse, StopPlaybackRequest,
    StopPlaybackResponse, TransferRoomOwnershipRequest, TransferRoomOwnershipResponse,
    UpdatePlaybackRequest, UpdatePlaylistRequest, UpdatePlaylistResponse,
    UpdateRoomSettingsRequest, UpdateRoomSettingsResponse,
};

pub type JoinRoomBody = JoinRoomRequest;
pub type SetRoomPasswordBody = SetRoomPasswordRequest;
pub type UpdateRoomSettingsBody = UpdateRoomSettingsRequest;
pub type TransferRoomOwnershipBody = TransferRoomOwnershipRequest;
pub type StartPlaybackBody = StartPlaybackRequest;
pub type StopPlaybackBody = StopPlaybackRequest;
pub type AddMediaBody = AddMediaRequest;
pub type DeleteEntriesBody = DeleteEntriesRequest;

#[cfg(test)]
fn parse_optional_query_i32(
    params: &std::collections::HashMap<String, String>,
    key: &str,
) -> AppResult<Option<i32>> {
    params
        .get(key)
        .map(|value| {
            value.parse::<i32>().map_err(|_| {
                super::AppError::bad_request(format!(
                    "Invalid {key} query parameter '{value}'. Expected an integer"
                ))
            })
        })
        .transpose()
}

#[cfg(test)]
fn parse_optional_query_bool(
    params: &std::collections::HashMap<String, String>,
    key: &str,
) -> AppResult<Option<bool>> {
    params
        .get(key)
        .map(|value| {
            value.parse::<bool>().map_err(|_| {
                super::AppError::bad_request(format!(
                    "Invalid {key} query parameter '{value}'. Expected true or false"
                ))
            })
        })
        .transpose()
}

pub type AddMediaBatchBody = AddMediaBatchRequest;
pub type EditMediaBody = EditMediaRequest;
pub type CreatePlaylistBody = CreatePlaylistRequest;
pub type UpdatePlaylistBody = UpdatePlaylistRequest;
pub type MovePlaylistBody = MovePlaylistRequest;

fn validate_room_path(
    path: crate::proto::client::RoomPathRequest,
) -> Result<String, super::AppError> {
    crate::impls::validate_proto_request(&path).map_err(super::error::map_api_error)?;
    Ok(path.room_id)
}

fn request_metadata(request_meta: RequestMetadata) -> crate::impls::RequestMetadata {
    request_meta.0.with_timeout(Some(HTTP_REQUEST_TIMEOUT))
}

fn execute_public_endpoint<'a, T, F, Fut>(
    state: &'a AppState,
    request_meta: RequestMetadata,
    category: EndpointRateLimitCategory,
    operation: F,
) -> BoxFuture<'a, Result<T, super::AppError>>
where
    T: Send + 'a,
    F: FnOnce(std::sync::Arc<crate::impls::ClientApiImpl>) -> Fut + Send + 'a,
    Fut: Future<Output = Result<T, crate::impls::ApiError>> + Send + 'a,
{
    async move {
        let request_meta = request_metadata(request_meta);
        let executor = state.client_api.clone();
        let client_api = state.client_api.clone();
        executor
            .execute_public_endpoint(&request_meta, category, move || operation(client_api))
            .await
            .map_err(super::error::map_api_error)
    }
    .boxed()
}

fn execute_user_endpoint<'a, T, F, Fut>(
    state: &'a AppState,
    request_meta: RequestMetadata,
    category: EndpointRateLimitCategory,
    operation: F,
) -> BoxFuture<'a, Result<T, super::AppError>>
where
    T: Send + 'a,
    F: FnOnce(
            std::sync::Arc<crate::impls::ClientApiImpl>,
            synctv_core::service::AuthenticatedToken,
        ) -> Fut
        + Send
        + 'a,
    Fut: Future<Output = Result<T, crate::impls::ApiError>> + Send + 'a,
{
    async move {
        let request_meta = request_metadata(request_meta);
        let executor = state.client_api.clone();
        let client_api = state.client_api.clone();
        executor
            .execute_user_endpoint(&request_meta, category, move |authenticated| {
                operation(client_api, authenticated)
            })
            .await
            .map_err(super::error::map_api_error)
    }
    .boxed()
}

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
pub async fn create_room(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<CreateRoomRequest>,
) -> AppResult<Json<CreateRoomResponse>> {
    tracing::info!(room_name = %req.name, "Creating new room");

    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        move |client_api, authenticated| async move {
            client_api
                .create_room(authenticated.user_id.as_str(), req)
                .await
        },
    )
    .await?;

    tracing::info!(
        room_id = response
            .room
            .as_ref()
            .map_or("unknown", |room| room.id.as_str()),
        "Room created successfully"
    );
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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
) -> AppResult<Json<GetRoomResponse>> {
    let room_id = validate_room_path(path)?;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |client_api, authenticated| async move {
            client_api
                .get_room(authenticated.user_id.as_str(), &room_id)
                .await
        },
    )
    .await?;

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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(mut req): Json<JoinRoomBody>,
) -> AppResult<Json<JoinRoomResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let room_id = validate_room_path(path)?;
    req.room_id = room_id.clone();
    let client_ip = request_meta.client_ip.map(|ip| ip.to_string());
    let executor = state.client_api.clone();
    let client_api = state.client_api.clone();
    let response = executor
        .execute_user_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Write,
            move |request_control, authenticated| async move {
                client_api
                    .join_room_with_control(
                        authenticated.user_id.as_str(),
                        &room_id,
                        req,
                        client_ip.as_deref(),
                        Some(&request_control),
                    )
                    .await
            },
        )
        .await?;

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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
) -> AppResult<Json<LeaveRoomResponse>> {
    let room_id = validate_room_path(path)?;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        move |client_api, authenticated| async move {
            client_api
                .leave_room(authenticated.user_id.as_str(), &room_id)
                .await
        },
    )
    .await?;

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
pub async fn delete_room(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
) -> AppResult<Json<DeleteRoomResponse>> {
    let room_id = validate_room_path(path)?;
    tracing::info!(room_id = %room_id, "Deleting room");
    let room_id_for_log = room_id.clone();

    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        move |client_api, authenticated| async move {
            client_api
                .delete_room(authenticated.user_id.as_str(), &room_id)
                .await
        },
    )
    .await?;

    tracing::info!(room_id = %room_id_for_log, "Room deleted successfully");
    Ok(Json(response))
}

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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<AddMediaBody>,
) -> AppResult<Json<AddMediaResponse>> {
    let room_id = validate_room_path(path)?;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        move |client_api, authenticated| async move {
            client_api
                .add_media(authenticated.user_id.as_str(), &room_id, req)
                .await
        },
    )
    .await?;

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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomMediaTargetPathRequest>,
    ValidatedQuery(query): ValidatedQuery<DeleteMediaQuery>,
) -> AppResult<Json<DeleteMediaResponse>> {
    crate::impls::validate_proto_request(&path).map_err(super::error::map_api_error)?;
    let crate::proto::client::RoomMediaTargetPathRequest { room_id, media_id } = path;
    let proto_req = DeleteMediaRequest {
        media_id,
        force: query.force,
    };
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        move |client_api, authenticated| async move {
            client_api
                .delete_media(authenticated.user_id.as_str(), &room_id, proto_req)
                .await
        },
    )
    .await?;

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
pub async fn delete_entries(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<DeleteEntriesBody>,
) -> AppResult<Json<DeleteEntriesResponse>> {
    let room_id = validate_room_path(path)?;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        move |client_api, authenticated| async move {
            client_api
                .delete_entries(authenticated.user_id.as_str(), &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

/// Move a media item relative to a sibling.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/media/move",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = MoveMediaRequest,
        responses(
            (status = 200, description = "Media moved", body = MoveMediaResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn move_media(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<MoveMediaRequest>,
) -> AppResult<Json<MoveMediaResponse>> {
    let room_id = validate_room_path(path)?;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        move |client_api, authenticated| async move {
            client_api
                .move_media(authenticated.user_id.as_str(), &room_id, req)
                .await
        },
    )
    .await?;

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
        request_body = ListPlaylistItemsRequest,
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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<ListPlaylistItemsRequest>,
) -> AppResult<Json<crate::proto::client::ListPlaylistItemsResponse>> {
    let room_id = validate_room_path(path)?;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |client_api, authenticated| async move {
            client_api
                .list_playlist_items(authenticated.user_id.as_str(), &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<StartPlaybackBody>,
) -> AppResult<Json<StartPlaybackResponse>> {
    let room_id = validate_room_path(path)?;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        move |client_api, authenticated| async move {
            client_api
                .start_playback(authenticated.user_id.as_str(), &room_id, req)
                .await
        },
    )
    .await?;

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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<StopPlaybackBody>,
) -> AppResult<Json<StopPlaybackResponse>> {
    let room_id = validate_room_path(path)?;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        move |client_api, authenticated| async move {
            client_api
                .stop_playback(authenticated.user_id.as_str(), &room_id, req)
                .await
        },
    )
    .await?;

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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
) -> AppResult<Json<GetPlaybackResponse>> {
    let room_id = validate_room_path(path)?;
    let request_meta = request_metadata(request_meta);
    let executor = state.client_api.clone();
    let client_api = state.client_api.clone();
    let response = executor
        .execute_user_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Read,
            move |request_control, authenticated| async move {
                client_api
                    .get_playback_with_context(
                        authenticated.user_id.as_str(),
                        &room_id,
                        GetPlaybackRequest {},
                        &request_control,
                    )
                    .await
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
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
            GetRoomMembersRequest
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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    ValidatedQuery(req): ValidatedQuery<GetRoomMembersRequest>,
) -> AppResult<Json<GetRoomMembersResponse>> {
    let room_id = validate_room_path(path)?;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |client_api, authenticated| async move {
            client_api
                .get_room_members(authenticated.user_id.as_str(), &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<crate::proto::client::CheckRoomRequest>,
) -> AppResult<Json<CheckRoomResponse>> {
    let response = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |client_api| async move { client_api.check_room(req).await },
    )
    .await?;

    Ok(Json(response))
}

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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<SetRoomPasswordBody>,
) -> AppResult<Json<SetRoomPasswordResponse>> {
    let room_id = validate_room_path(path)?;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        move |client_api, authenticated| async move {
            client_api
                .set_room_password(authenticated.user_id.as_str(), &room_id, req)
                .await
        },
    )
    .await?;

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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
) -> AppResult<Json<crate::proto::client::GetRoomSettingsResponse>> {
    let room_id = validate_room_path(path)?;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |client_api, authenticated| async move {
            client_api
                .get_room_settings(authenticated.user_id.as_str(), &room_id)
                .await
        },
    )
    .await?;

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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<AddMediaBatchBody>,
) -> AppResult<Json<crate::proto::client::AddMediaBatchResponse>> {
    let room_id = validate_room_path(path)?;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        move |client_api, authenticated| async move {
            client_api
                .add_media_batch(authenticated.user_id.as_str(), &room_id, req)
                .await
        },
    )
    .await?;

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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomMediaTargetPathRequest>,
    Json(req): Json<EditMediaBody>,
) -> AppResult<Json<EditMediaResponse>> {
    crate::impls::validate_proto_request(&path).map_err(super::error::map_api_error)?;
    let crate::proto::client::RoomMediaTargetPathRequest { room_id, media_id } = path;
    let req = req.with_media_id(media_id);
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        move |client_api, authenticated| async move {
            client_api
                .edit_media(authenticated.user_id.as_str(), &room_id, req)
                .await
        },
    )
    .await?;

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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
) -> AppResult<Json<ClearPlaylistResponse>> {
    let room_id = validate_room_path(path)?;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        move |client_api, authenticated| async move {
            client_api
                .clear_playlist(authenticated.user_id.as_str(), &room_id)
                .await
        },
    )
    .await?;

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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomMediaTargetPathRequest>,
) -> AppResult<Json<crate::proto::client::Media>> {
    crate::impls::validate_proto_request(&path).map_err(super::error::map_api_error)?;
    let crate::proto::client::RoomMediaTargetPathRequest { room_id, media_id } = path;
    let media = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |client_api, authenticated| async move {
            client_api
                .get_media(authenticated.user_id.as_str(), &room_id, &media_id)
                .await
        },
    )
    .await?;

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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPlaylistTargetPathRequest>,
) -> AppResult<Json<crate::proto::client::GetPlaylistResponse>> {
    crate::impls::validate_proto_request(&path).map_err(super::error::map_api_error)?;
    let crate::proto::client::RoomPlaylistTargetPathRequest {
        room_id,
        playlist_id,
    } = path;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |client_api, authenticated| async move {
            client_api
                .get_playlist(authenticated.user_id.as_str(), &room_id, &playlist_id)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

/// Unified handler for listing rooms (with query params) or getting single room by ID
/// GET /api/rooms (list) or GET /api/rooms?id=xxx (single)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms",
        tag = "Room",
        params(ListRoomsRequest),
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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ValidatedQuery(req): ValidatedQuery<ListRoomsRequest>,
) -> AppResult<Json<ListRoomsResponse>> {
    let response = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |client_api| async move { client_api.list_rooms(req).await },
    )
    .await?;

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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<UpdateRoomSettingsBody>,
) -> AppResult<Json<UpdateRoomSettingsResponse>> {
    let room_id = validate_room_path(path)?;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        move |client_api, authenticated| async move {
            client_api
                .update_room_settings(authenticated.user_id.as_str(), &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/owner",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = TransferRoomOwnershipRequest,
        responses(
            (status = 200, description = "Room ownership transferred", body = TransferRoomOwnershipResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Permission denied", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn transfer_room_ownership(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<TransferRoomOwnershipBody>,
) -> AppResult<Json<TransferRoomOwnershipResponse>> {
    let room_id = validate_room_path(path)?;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        move |client_api, authenticated| async move {
            client_api
                .transfer_room_ownership(authenticated.user_id.as_str(), &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<UpdatePlaybackRequest>,
) -> AppResult<Json<GetPlaybackResponse>> {
    let room_id = validate_room_path(path)?;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        move |client_api, authenticated| async move {
            client_api
                .update_playback(authenticated.user_id.as_str(), &room_id, req)
                .await
        },
    )
    .await?;
    Ok(Json(response))
}

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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
) -> AppResult<Json<ResetRoomSettingsResponse>> {
    let room_id = validate_room_path(path)?;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        move |client_api, authenticated| async move {
            client_api
                .reset_room_settings(authenticated.user_id.as_str(), &room_id)
                .await
        },
    )
    .await?;

    Ok(Json(response))
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
            GetChatHistoryRequest
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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    RawQuery(raw_query): RawQuery,
    ValidatedQuery(mut req): ValidatedQuery<GetChatHistoryRequest>,
) -> AppResult<Json<GetChatHistoryResponse>> {
    let room_id = validate_room_path(path)?;
    validate_chat_history_query(raw_query.as_deref())?;
    req.limit = if req.limit == 0 {
        50
    } else {
        req.limit.clamp(1, 100)
    };
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |client_api, authenticated| async move {
            client_api
                .get_chat_history(authenticated.user_id.as_str(), &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

fn validate_chat_history_query(raw_query: Option<&str>) -> AppResult<()> {
    let Some(raw_query) = raw_query else {
        return Ok(());
    };

    if url::form_urlencoded::parse(raw_query.as_bytes()).any(|(key, _)| key == "before") {
        return Err(super::AppError::bad_request(
            "The 'before' query parameter is no longer supported; use 'cursor' instead",
        ));
    }

    Ok(())
}

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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<CreatePlaylistBody>,
) -> AppResult<Json<CreatePlaylistResponse>> {
    let room_id = validate_room_path(path)?;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        move |client_api, authenticated| async move {
            client_api
                .create_playlist(authenticated.user_id.as_str(), &room_id, req)
                .await
        },
    )
    .await?;

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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPlaylistTargetPathRequest>,
    Json(req): Json<UpdatePlaylistBody>,
) -> AppResult<Json<UpdatePlaylistResponse>> {
    crate::impls::validate_proto_request(&path).map_err(super::error::map_api_error)?;
    let crate::proto::client::RoomPlaylistTargetPathRequest {
        room_id,
        playlist_id,
    } = path;
    let req = req.with_playlist_id(playlist_id);
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        move |client_api, authenticated| async move {
            client_api
                .update_playlist(authenticated.user_id.as_str(), &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/playlists/{playlist_id}/move",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("playlist_id" = String, Path, description = "Playlist ID")
        ),
        request_body = MovePlaylistRequest,
        responses(
            (status = 200, description = "Playlist moved", body = MovePlaylistResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn move_playlist(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPlaylistTargetPathRequest>,
    Json(req): Json<MovePlaylistBody>,
) -> AppResult<Json<MovePlaylistResponse>> {
    crate::impls::validate_proto_request(&path).map_err(super::error::map_api_error)?;
    let crate::proto::client::RoomPlaylistTargetPathRequest {
        room_id,
        playlist_id,
    } = path;
    let req = req.with_playlist_id(playlist_id);
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        move |client_api, authenticated| async move {
            client_api
                .move_playlist(authenticated.user_id.as_str(), &room_id, req)
                .await
        },
    )
    .await?;

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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPlaylistTargetPathRequest>,
    ValidatedQuery(query): ValidatedQuery<DeletePlaylistQuery>,
) -> AppResult<Json<DeletePlaylistResponse>> {
    crate::impls::validate_proto_request(&path).map_err(super::error::map_api_error)?;
    let crate::proto::client::RoomPlaylistTargetPathRequest {
        room_id,
        playlist_id,
    } = path;
    let req = DeletePlaylistRequest {
        playlist_id,
        force: query.force,
    };
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        move |client_api, authenticated| async move {
            client_api
                .delete_playlist(authenticated.user_id.as_str(), &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

/// List playlists in a room
/// GET /`api/rooms/:room_id/playlists`
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/playlists",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ListPlaylistsRequest
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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    ValidatedQuery(req): ValidatedQuery<ListPlaylistsRequest>,
) -> AppResult<Json<ListPlaylistsResponse>> {
    let room_id = validate_room_path(path)?;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |client_api, authenticated| async move {
            client_api
                .list_playlists(authenticated.user_id.as_str(), &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

/// Get hot rooms (sorted by online count)
/// GET /api/rooms/hot
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/hot",
        tag = "Room",
        params(GetHotRoomsRequest),
        responses(
            (status = 200, description = "Hot rooms", body = GetHotRoomsResponse)
        )
    )
)]
pub async fn get_hot_rooms(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ValidatedQuery(mut req): ValidatedQuery<GetHotRoomsRequest>,
) -> AppResult<Json<GetHotRoomsResponse>> {
    req.limit = if req.limit == 0 {
        10
    } else {
        req.limit.min(50)
    };
    let response = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |client_api| async move { client_api.get_hot_rooms(req).await },
    )
    .await?;

    Ok(Json(response))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        parse_optional_query_bool, parse_optional_query_i32, validate_chat_history_query,
        AddMediaBatchBody, CreatePlaylistBody, DeleteEntriesBody, UpdatePlaybackRequest,
    };
    use crate::proto::client::{
        DeleteMediaQuery, DeletePlaylistQuery, GetChatHistoryRequest, GetRoomMembersRequest,
        ListPlaylistItemsRequest, ListPlaylistsRequest, ListRoomsRequest, MoveMediaRequest,
    };

    #[test]
    fn test_update_playback_request_deserialize_state_only() {
        let json = r#"{"state":1}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            req.state,
            crate::proto::client::PlaybackPatchState::Playing as i32
        );
        assert!(req.position.is_none());
        assert!(req.speed.is_none());
        assert!(req.media_id.is_empty());
    }

    #[test]
    fn test_update_playback_request_deserialize_position_only() {
        let json = r#"{"position": 42.5}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            req.state,
            crate::proto::client::PlaybackPatchState::Unspecified as i32
        );
        assert!((req.position.unwrap() - 42.5).abs() < f64::EPSILON);
        assert!(req.speed.is_none());
        assert!(req.media_id.is_empty());
    }

    #[test]
    fn test_update_playback_request_deserialize_speed_only() {
        let json = r#"{"speed": 2.0}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            req.state,
            crate::proto::client::PlaybackPatchState::Unspecified as i32
        );
        assert!(req.position.is_none());
        assert!((req.speed.unwrap() - 2.0).abs() < f64::EPSILON);
        assert!(req.media_id.is_empty());
    }

    #[test]
    fn test_update_playback_request_deserialize_media_id_only() {
        let json = r#"{"media_id": "media_abc123"}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            req.state,
            crate::proto::client::PlaybackPatchState::Unspecified as i32
        );
        assert!(req.position.is_none());
        assert!(req.speed.is_none());
        assert_eq!(req.media_id, "media_abc123");
        assert!(req.playlist_id.is_empty());
        assert!(req.target.is_empty());
    }

    #[test]
    fn test_members_query_params_deserialize_sorting_and_filters() {
        let json =
            r#"{"page":2,"page_size":25,"search":"alice","role":3,"sort_by":2,"sort_direction":1}"#;
        let query: GetRoomMembersRequest = serde_json::from_str(json).expect("deserialize");
        assert_eq!(query.page, 2);
        assert_eq!(query.page_size, 25);
        assert_eq!(query.search, "alice");
        assert_eq!(
            query.role,
            Some(synctv_proto::common::RoomMemberRole::Admin as i32)
        );
        assert_eq!(
            query.sort_by,
            crate::proto::client::RoomMemberListSortBy::Username as i32
        );
        assert_eq!(
            query.sort_direction,
            crate::proto::client::SortDirection::Asc as i32
        );
    }

    #[test]
    fn test_scalar_query_parsers_reject_invalid_values() {
        let mut params = HashMap::new();
        params.insert("page".to_string(), "abc".to_string());
        assert!(parse_optional_query_i32(&params, "page").is_err());

        let mut params = HashMap::new();
        params.insert("dynamic_only".to_string(), "sometimes".to_string());
        assert!(parse_optional_query_bool(&params, "dynamic_only").is_err());

        assert!(serde_urlencoded::from_str::<DeleteMediaQuery>("force=definitely").is_err());
        assert!(serde_urlencoded::from_str::<DeletePlaylistQuery>("force=definitely").is_err());
    }

    #[test]
    fn test_list_rooms_query_deserializes_proto_defaults() {
        let query: ListRoomsRequest = serde_urlencoded::from_str("").unwrap();

        assert_eq!(query.page, 0);
        assert_eq!(query.page_size, 0);
        assert!(query.search.is_empty());
        assert_eq!(query.sort_by, 0);
        assert_eq!(query.sort_direction, 0);
    }

    #[test]
    fn test_list_rooms_query_deserializes_explicit_values() {
        let query: ListRoomsRequest = serde_urlencoded::from_str(
            "page=2&page_size=25&search=room&sort_by=4&sort_direction=1",
        )
        .unwrap();

        assert_eq!(query.page, 2);
        assert_eq!(query.page_size, 25);
        assert_eq!(query.search, "room");
        assert_eq!(
            query.sort_by,
            crate::proto::client::RoomListSortBy::Name as i32
        );
        assert_eq!(
            query.sort_direction,
            crate::proto::client::SortDirection::Asc as i32
        );
    }

    #[test]
    fn test_check_room_path_deserializes_proto_field_name() {
        let req: crate::proto::client::CheckRoomRequest =
            serde_json::from_str(r#"{"room_id":"AbC123xYz890"}"#).unwrap();

        assert_eq!(req.room_id, "AbC123xYz890");
    }

    #[test]
    fn test_room_path_request_deserializes_proto_field_name() {
        let req: crate::proto::client::RoomPathRequest =
            serde_json::from_str(r#"{"room_id":"AbC123xYz890"}"#).unwrap();

        assert_eq!(req.room_id, "AbC123xYz890");
    }

    #[test]
    fn test_room_media_target_path_request_deserializes_proto_field_names() {
        let req: crate::proto::client::RoomMediaTargetPathRequest =
            serde_json::from_str(r#"{"room_id":"AbC123xYz890","media_id":"ZyX098wVu765"}"#)
                .unwrap();

        assert_eq!(req.room_id, "AbC123xYz890");
        assert_eq!(req.media_id, "ZyX098wVu765");
    }

    #[test]
    fn test_room_playlist_target_path_request_deserializes_proto_field_names() {
        let req: crate::proto::client::RoomPlaylistTargetPathRequest =
            serde_json::from_str(r#"{"room_id":"AbC123xYz890","playlist_id":"ZyX098wVu765"}"#)
                .unwrap();

        assert_eq!(req.room_id, "AbC123xYz890");
        assert_eq!(req.playlist_id, "ZyX098wVu765");
    }

    #[test]
    fn test_list_playlists_query_deserializes_proto_defaults() {
        let query: ListPlaylistsRequest = serde_urlencoded::from_str("").unwrap();

        assert_eq!(query.page, 0);
        assert_eq!(query.page_size, 0);
        assert_eq!(query.sort_by, 0);
        assert_eq!(query.sort_direction, 0);
        assert_eq!(query.availability, 0);
    }

    #[test]
    fn test_list_playlists_query_deserializes_explicit_values() {
        let query: ListPlaylistsRequest = serde_urlencoded::from_str(
            "page=2&page_size=25&sort_by=4&sort_direction=2&availability=2",
        )
        .unwrap();

        assert_eq!(query.page, 2);
        assert_eq!(query.page_size, 25);
        assert_eq!(
            query.sort_by,
            crate::proto::client::PlaylistListSortBy::UpdatedAt as i32
        );
        assert_eq!(
            query.sort_direction,
            crate::proto::client::SortDirection::Desc as i32
        );
        assert_eq!(
            query.availability,
            crate::proto::client::ResourceAvailabilityFilter::Unavailable as i32
        );
    }

    #[test]
    fn test_chat_history_parser_rejects_invalid_limit() {
        let mut params = HashMap::new();
        params.insert("limit".to_string(), "many".to_string());
        assert!(serde_urlencoded::from_str::<GetChatHistoryRequest>("limit=many").is_err());
    }

    #[test]
    fn test_update_playback_request_deserialize_dynamic_target() {
        let json = r#"{"playlist_id": "pl1", "target": {"item_id": "provider-item-123"}}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.playlist_id, "pl1");
        let target: serde_json::Value = serde_json::from_slice(&req.target).unwrap();
        assert_eq!(target, serde_json::json!({"item_id": "provider-item-123"}));
        assert!(req.media_id.is_empty());
        assert_eq!(
            req.state,
            crate::proto::client::PlaybackPatchState::Unspecified as i32
        );
        assert!(req.position.is_none());
        assert!(req.speed.is_none());
    }

    #[test]
    fn test_update_playback_empty_switch_ids_are_treated_as_clear_request() {
        let json = r#"{"media_id":"","playlist_id":""}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert!(req.media_id.is_empty());
        assert!(req.playlist_id.is_empty());
        assert!(req.target.is_empty());
    }

    #[test]
    fn test_update_playback_omitted_switch_fields_are_not_switch_request() {
        let json = r#"{"state":2}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert!(req.media_id.is_empty());
        assert!(req.playlist_id.is_empty());
        assert!(req.target.is_empty());
    }

    #[test]
    fn test_list_playlist_items_body_deserialize_room_root() {
        let json = r"{}";
        let req: ListPlaylistItemsRequest = serde_json::from_str(json).unwrap();
        assert!(req.playlist_id.is_empty());
        assert!(req.target.is_empty());
        assert_eq!(req.page, 0);
        assert_eq!(req.page_size, 0);
        assert_eq!(req.availability, 0);
    }

    #[test]
    fn test_list_playlist_items_body_deserialize_dynamic_target() {
        let json =
            r#"{"playlist_id":"pl1","target":{"cursor":"season-1"},"page":2,"page_size":25}"#;
        let req: ListPlaylistItemsRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.playlist_id, "pl1");
        let target: serde_json::Value = serde_json::from_slice(&req.target).unwrap();
        assert_eq!(target, serde_json::json!({"cursor":"season-1"}));
        assert_eq!(req.page, 2);
        assert_eq!(req.page_size, 25);
        assert_eq!(req.availability, 0);
    }

    #[test]
    fn test_update_playback_request_deserialize_with_version() {
        let json = r#"{"state": 1, "version": 42}"#;
        let req: UpdatePlaybackRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            req.state,
            crate::proto::client::PlaybackPatchState::Playing as i32
        );
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
    fn test_move_media_request_deserializes_anchor_fields_without_wrapper() {
        let json = r#"{
            "media_ids": ["media-1"],
            "before_media_id": "media-2"
        }"#;
        let req: MoveMediaRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.media_ids, vec!["media-1".to_string()]);
        assert_eq!(req.before_media_id.as_deref(), Some("media-2"));
        assert!(req.after_media_id.is_none());
    }

    #[test]
    fn test_parse_chat_history_request_rejects_before_param() {
        let err = validate_chat_history_query(Some("limit=20&before=1710000000"))
            .expect_err("before must be rejected");

        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(
            err.message.contains("no longer supported"),
            "unexpected message: {}",
            err.message
        );
    }

    #[test]
    fn test_parse_chat_history_request_accepts_cursor_only() {
        validate_chat_history_query(Some(
            "limit=20&cursor=2026-03-31T12%3A00%3A00%2B00%3A00%7Cmsg_123",
        ))
        .expect("cursor-only request");
        let req: GetChatHistoryRequest = serde_urlencoded::from_str(
            "limit=20&cursor=2026-03-31T12%3A00%3A00%2B00%3A00%7Cmsg_123",
        )
        .expect("deserialize cursor request");

        assert_eq!(req.limit, 20);
        assert_eq!(req.cursor, "2026-03-31T12:00:00+00:00|msg_123");
    }

    #[test]
    fn test_delete_force_query_deserialization_accepts_bool_only() {
        let query: DeleteMediaQuery = serde_urlencoded::from_str("force=true").unwrap();
        assert!(query.force);

        let query: DeletePlaylistQuery = serde_urlencoded::from_str("force=false").unwrap();
        assert!(!query.force);

        assert!(serde_urlencoded::from_str::<DeleteMediaQuery>("force=1").is_err());
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

    #[test]
    fn test_move_playlist_body_deserializes_without_path_playlist_id() {
        let body: crate::proto::client::MovePlaylistRequest =
            serde_json::from_str(r#"{"before_playlist_id":"playlist-2"}"#).expect("deserialize");

        assert!(body.playlist_id.is_empty());
        assert_eq!(
            body.anchor,
            Some(
                crate::proto::client::move_playlist_request::Anchor::BeforePlaylistId(
                    "playlist-2".to_string()
                )
            )
        );
    }
}
