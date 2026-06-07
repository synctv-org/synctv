use axum::{
    extract::{Path, State},
    Json,
};

use super::execute::{execute_room_actor_endpoint, execute_user_endpoint};
use crate::http::validation::ProtoQuery;
use crate::http::{middleware::RequestMetadata, AppResult, AppState};
use crate::impls::{ApiError, EndpointRateLimitCategory, EndpointRateLimitScope};
use synctv_proto::client::{
    CreatePlaylistRequest, CreatePlaylistResponse, DeletePlaylistQuery, DeletePlaylistRequest,
    DeletePlaylistResponse, ListPlaylistsRequest, ListPlaylistsResponse, MovePlaylistResponse,
    UpdatePlaylistResponse,
};

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
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn create_playlist(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(req): Json<CreatePlaylistRequest>,
) -> AppResult<Json<CreatePlaylistResponse>> {
    let room_id = path.room_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, authenticated| async move {
            client_api
                .create_playlist(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

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
        request_body = synctv_proto::client::UpdatePlaylistRequest,
        responses(
            (status = 200, description = "Playlist updated", body = UpdatePlaylistResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn update_playlist(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPlaylistTargetPathRequest>,
    Json(mut req): Json<synctv_proto::client::UpdatePlaylistRequest>,
) -> AppResult<Json<UpdatePlaylistResponse>> {
    let synctv_proto::client::RoomPlaylistTargetPathRequest {
        room_id,
        playlist_id,
    } = path;
    req.playlist_id = playlist_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, authenticated| async move {
            client_api
                .update_playlist(&authenticated.user_id, &room_id, req)
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
        request_body = synctv_proto::http_serde::MovePlaylistRequestDef,
        responses(
            (status = 200, description = "Playlist moved", body = MovePlaylistResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn move_playlist(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPlaylistTargetPathRequest>,
    Json(req): Json<synctv_proto::http_serde::MovePlaylistRequestDef>,
) -> AppResult<Json<MovePlaylistResponse>> {
    let synctv_proto::client::RoomPlaylistTargetPathRequest {
        room_id,
        playlist_id,
    } = path;
    let mut req =
        synctv_proto::client::MovePlaylistRequest::try_from(req).map_err(ApiError::InvalidInput)?;
    req.playlist_id = playlist_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomPlaylist,
        move |client_api, authenticated| async move {
            client_api
                .move_playlist(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

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
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Playlist not found", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn delete_playlist(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPlaylistTargetPathRequest>,
    ProtoQuery(query): ProtoQuery<DeletePlaylistQuery>,
) -> AppResult<Json<DeletePlaylistResponse>> {
    let synctv_proto::client::RoomPlaylistTargetPathRequest {
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
        EndpointRateLimitScope::RoomPlaylist,
        move |client_api, authenticated| async move {
            client_api
                .delete_playlist(&authenticated.user_id, &room_id, req)
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
        path = "/api/rooms/{room_id}/playlists",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ListPlaylistsRequest
        ),
        responses(
            (status = 200, description = "Playlists in room", body = ListPlaylistsResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn list_playlists(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    ProtoQuery(req): ProtoQuery<ListPlaylistsRequest>,
) -> AppResult<Json<ListPlaylistsResponse>> {
    let room_id = path.room_id;
    let response =
        execute_room_actor_endpoint(
            &state,
            request_meta,
            room_id,
            EndpointRateLimitCategory::Read,
            EndpointRateLimitScope::RoomPlaylist,
            move |client_api, actor| async move {
                client_api.list_playlists_for_actor(&actor, req).await
            },
        )
        .await?;

    Ok(Json(response))
}
