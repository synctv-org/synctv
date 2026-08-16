use axum::{
    extract::{Path, State},
    Json,
};

use super::execute::execute_user_endpoint;
use super::types::RoomStreamPath;
use crate::http::validation::ProtoQuery;
use crate::http::{middleware::RequestMetadata, AppResult, AppState};
use synctv_api_common::impls::{EndpointRateLimitCategory, EndpointRateLimitScope};
use synctv_proto::client::{
    CreateRoomPublishKeyRequest, CreateRoomPublishKeyResponse, GetRoomStreamInfoRequest,
    GetRoomStreamInfoResponse, KickRoomStreamRequest, KickRoomStreamResponse,
    ListRoomStreamsRequest, ListRoomStreamsResponse,
};

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{roomId}/streams",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ListRoomStreamsRequest
        ),
        responses(
            (status = 200, description = "Active room live streams", body = ListRoomStreamsResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Room not found", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn list_room_streams(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    ProtoQuery(req): ProtoQuery<ListRoomStreamsRequest>,
) -> AppResult<Json<ListRoomStreamsResponse>> {
    let room_id = path.room_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, authenticated| async move {
            client_api
                .list_room_streams(&authenticated.user_id(), &room_id, req)
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
        path = "/api/rooms/{roomId}/streams/{mediaId}/publish-key",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("mediaId" = String, Path, description = "Media ID")
        ),
        responses(
            (status = 200, description = "Room stream publish key generated", body = CreateRoomPublishKeyResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Permission denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Room or media not found", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn create_room_publish_key(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<RoomStreamPath>,
) -> AppResult<Json<CreateRoomPublishKeyResponse>> {
    let room_id = path.room_id;
    let req = CreateRoomPublishKeyRequest {
        media_id: path.media_id,
    };
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, authenticated| async move {
            client_api
                .create_room_publish_key(&authenticated.user_id(), &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{roomId}/streams/{mediaId}",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("mediaId" = String, Path, description = "Media ID")
        ),
        responses(
            (status = 200, description = "Room live stream information", body = GetRoomStreamInfoResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Stream not found", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_room_stream_info(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<RoomStreamPath>,
) -> AppResult<Json<GetRoomStreamInfoResponse>> {
    let room_id = path.room_id;
    let req = GetRoomStreamInfoRequest {
        media_id: path.media_id,
    };
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, authenticated| async move {
            client_api
                .get_room_stream_info(&authenticated.user_id(), &room_id, req)
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
        path = "/api/rooms/{roomId}/streams/{mediaId}/kick",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("mediaId" = String, Path, description = "Media ID")
        ),
        request_body = KickRoomStreamRequest,
        responses(
            (status = 200, description = "Room live stream kicked", body = KickRoomStreamResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Permission denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Stream not found", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn kick_room_stream(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<RoomStreamPath>,
    Json(mut req): Json<KickRoomStreamRequest>,
) -> AppResult<Json<KickRoomStreamResponse>> {
    let room_id = path.room_id;
    req.media_id = path.media_id;
    execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, authenticated| async move {
            client_api
                .kick_room_stream(&authenticated.user_id(), &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(KickRoomStreamResponse {}))
}
