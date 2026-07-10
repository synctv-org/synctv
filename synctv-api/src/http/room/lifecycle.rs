use axum::{
    extract::{Path, State},
    Json,
};

use super::execute::{
    execute_optional_user_endpoint, execute_public_endpoint, execute_room_actor_endpoint,
    execute_user_endpoint,
};
use crate::http::validation::ProtoQuery;
use crate::http::{middleware::RequestMetadata, AppResult, AppState};
use crate::impls::{EndpointRateLimitCategory, EndpointRateLimitScope};
use synctv_proto::client::{
    CheckRoomRequest, CheckRoomResponse, CreateRoomRequest, DeleteRoomResponse, GetHotRoomsRequest,
    GetHotRoomsResponse, GetRoomResponse, JoinRoomRequest, JoinRoomResponse, LeaveRoomResponse,
    ListRoomCategoriesRequest, ListRoomCategoriesResponse, ListRoomLabelsRequest,
    ListRoomLabelsResponse, ListRoomsRequest, ListRoomsResponse, Room, RoomPathRequest,
};

/// Create a new room
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms",
        tag = "Room",
        request_body = CreateRoomRequest,
        responses(
            (status = 200, description = "Room created", body = Room),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
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
) -> AppResult<Json<Room>> {
    tracing::info!(room_name = %req.name, "Creating new room");

    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomCreate,
        move |client_api, authenticated| async move {
            Box::pin(client_api.create_room(&authenticated.user_id, req)).await
        },
    )
    .await?;

    tracing::info!(room_id = response.id.as_str(), "Room created successfully");
    Ok(Json(response))
}

/// Get room information
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{roomId}",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "Room details", body = GetRoomResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Room not found", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_room(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<RoomPathRequest>,
) -> AppResult<Json<GetRoomResponse>> {
    let room_id = path.room_id;
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id.clone(),
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomGet,
        move |client_api, actor| async move { client_api.get_room_for_actor(&actor).await },
    )
    .await?;

    Ok(Json(response))
}

/// Join a room
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        put,
        path = "/api/rooms/{roomId}/members/@me",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID")
        ),
        request_body = JoinRoomRequest,
        responses(
            (status = 200, description = "Joined room", body = JoinRoomResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Permission denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Room not found", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn join_room(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<RoomPathRequest>,
    Json(mut req): Json<JoinRoomRequest>,
) -> AppResult<Json<JoinRoomResponse>> {
    let request_meta = request_meta.0;
    let room_id = path.room_id;
    req.room_id = room_id.clone();
    let client_ip = request_meta.client_ip.map(|ip| ip.to_string());
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_scoped_user_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Write,
            EndpointRateLimitScope::RoomJoin,
            move |request_control, authenticated| async move {
                Box::pin(client_api.join_room_with_control(
                    &authenticated.user_id,
                    &room_id,
                    req,
                    client_ip.as_deref(),
                    Some(&request_control),
                ))
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
        path = "/api/rooms/{roomId}/members/@me",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "Left room", body = LeaveRoomResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Room not found", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn leave_room(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<RoomPathRequest>,
) -> AppResult<Json<LeaveRoomResponse>> {
    let room_id = path.room_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomJoin,
        move |client_api, authenticated| async move {
            client_api
                .leave_room(&authenticated.user_id, &room_id)
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
        path = "/api/rooms/{roomId}",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "Room deleted", body = DeleteRoomResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Permission denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Room not found", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn delete_room(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<RoomPathRequest>,
) -> AppResult<Json<DeleteRoomResponse>> {
    let room_id = path.room_id;
    tracing::info!(room_id = %room_id, "Deleting room");
    let room_id_for_log = room_id.clone();

    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomCreate,
        move |client_api, authenticated| async move {
            client_api
                .delete_room(&authenticated.user_id, &room_id)
                .await
        },
    )
    .await?;

    tracing::info!(room_id = %room_id_for_log, "Room deleted successfully");
    Ok(Json(response))
}

/// Check if room exists (public endpoint)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{roomId}/check",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "Room availability and status; exists=false when the room is not found", body = CheckRoomResponse),
            (status = 400, description = "Invalid room ID", body = crate::openapi::GoogleRpcStatusSchema)
        )
    )
)]
pub async fn check_room(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<CheckRoomRequest>,
) -> AppResult<Json<CheckRoomResponse>> {
    let response = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomGet,
        move |client_api| async move { client_api.check_room(req).await },
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
            (status = 400, description = "Invalid query", body = crate::openapi::GoogleRpcStatusSchema)
        )
    )
)]
pub async fn list_or_get_rooms(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(req): ProtoQuery<ListRoomsRequest>,
) -> AppResult<Json<ListRoomsResponse>> {
    let response = execute_optional_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomList,
        move |client_api, authenticated| async move {
            let viewer_id = authenticated.map(|auth| auth.user_id);
            client_api.list_rooms(req, viewer_id).await
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
            (status = 200, description = "Hot rooms", body = GetHotRoomsResponse),
            (status = 400, description = "Invalid query", body = crate::openapi::GoogleRpcStatusSchema)
        )
    )
)]
pub async fn get_hot_rooms(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(req): ProtoQuery<GetHotRoomsRequest>,
) -> AppResult<Json<GetHotRoomsResponse>> {
    let response = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomList,
        move |client_api| async move { client_api.get_hot_rooms(req).await },
    )
    .await?;

    Ok(Json(response))
}

pub async fn list_room_categories(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(req): ProtoQuery<ListRoomCategoriesRequest>,
) -> AppResult<Json<ListRoomCategoriesResponse>> {
    let response = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomList,
        move |client_api| async move { client_api.list_room_categories(req).await },
    )
    .await?;

    Ok(Json(response))
}

pub async fn list_room_labels(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(req): ProtoQuery<ListRoomLabelsRequest>,
) -> AppResult<Json<ListRoomLabelsResponse>> {
    let response = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomList,
        move |client_api| async move { client_api.list_room_labels(req).await },
    )
    .await?;

    Ok(Json(response))
}
