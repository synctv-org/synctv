use axum::{
    extract::{Path, State},
    Json,
};

use super::execute::{execute_room_actor_endpoint, execute_user_endpoint};
use crate::http::validation::ProtoQuery;
use crate::http::{middleware::RequestMetadata, AppResult, AppState};
use synctv_api_common::impls::{EndpointRateLimitCategory, EndpointRateLimitScope};
use synctv_proto::client::{
    AddMediaBatchRequest, AddMediaRequest, ClearPlaylistRequest, ClearPlaylistResponse,
    DeleteEntriesRequest, DeleteEntriesResponse, DeleteMediaQuery, DeleteMediaRequest,
    DeleteMediaResponse, EditMediaRequest, ListPlaylistItemsRequest, Media, MoveMediaRequest,
    MoveMediaResponse,
};

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{roomId}/media",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID")
        ),
        request_body = AddMediaRequest,
        responses(
            (status = 200, description = "Media added", body = Media),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn add_media(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(req): Json<AddMediaRequest>,
) -> AppResult<Json<Media>> {
    let room_id = path.room_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, authenticated| async move {
            client_api
                .add_media(&authenticated.user_id(), &room_id, req)
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
        path = "/api/rooms/{roomId}/media/{mediaId}",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("mediaId" = String, Path, description = "Media ID"),
            ("force" = Option<bool>, Query, description = "Force delete")
        ),
        responses(
            (status = 200, description = "Media deleted", body = DeleteMediaResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Media not found", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn delete_media(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomMediaTargetPathRequest>,
    ProtoQuery(query): ProtoQuery<DeleteMediaQuery>,
) -> AppResult<Json<DeleteMediaResponse>> {
    let synctv_proto::client::RoomMediaTargetPathRequest { room_id, media_id } = path;
    let proto_req = DeleteMediaRequest {
        media_id,
        force: query.force,
    };
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, authenticated| async move {
            client_api
                .delete_media(&authenticated.user_id(), &room_id, proto_req)
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
        path = "/api/rooms/{roomId}/entries",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID")
        ),
        request_body = DeleteEntriesRequest,
        responses(
            (status = 200, description = "Entries deleted", body = DeleteEntriesResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn delete_entries(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(req): Json<DeleteEntriesRequest>,
) -> AppResult<Json<DeleteEntriesResponse>> {
    let room_id = path.room_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, authenticated| async move {
            client_api
                .delete_entries(&authenticated.user_id(), &room_id, req)
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
        path = "/api/rooms/{roomId}/media/move",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID")
        ),
        request_body = MoveMediaRequest,
        responses(
            (status = 200, description = "Media moved", body = MoveMediaResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn move_media(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(req): Json<MoveMediaRequest>,
) -> AppResult<Json<MoveMediaResponse>> {
    let room_id = path.room_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, authenticated| async move {
            client_api
                .move_media(&authenticated.user_id(), &room_id, req)
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
        path = "/api/rooms/{roomId}/media/list",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID")
        ),
        request_body = ListPlaylistItemsRequest,
        responses(
            (status = 200, description = "Playlist items", body = synctv_proto::client::ListPlaylistItemsResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn list_playlist_items(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(req): Json<ListPlaylistItemsRequest>,
) -> AppResult<Json<synctv_proto::client::ListPlaylistItemsResponse>> {
    let room_id = path.room_id;
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomPlaylist,
        move |client_api, actor| async move {
            client_api.list_playlist_items_for_actor(&actor, req).await
        },
    )
    .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{roomId}/media/batch",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID")
        ),
        request_body = AddMediaBatchRequest,
        responses(
            (status = 200, description = "Batch media added", body = synctv_proto::client::AddMediaBatchResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn push_media_batch(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(req): Json<AddMediaBatchRequest>,
) -> AppResult<Json<synctv_proto::client::AddMediaBatchResponse>> {
    let room_id = path.room_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, authenticated| async move {
            client_api
                .add_media_batch(&authenticated.user_id(), &room_id, req)
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
        path = "/api/rooms/{roomId}/media/{mediaId}",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("mediaId" = String, Path, description = "Media ID")
        ),
        request_body = EditMediaRequest,
        responses(
            (status = 200, description = "Media updated", body = Media),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn edit_media(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomMediaTargetPathRequest>,
    Json(mut req): Json<EditMediaRequest>,
) -> AppResult<Json<Media>> {
    let synctv_proto::client::RoomMediaTargetPathRequest { room_id, media_id } = path;
    req.media_id = media_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, authenticated| async move {
            client_api
                .edit_media(&authenticated.user_id(), &room_id, req)
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
        path = "/api/rooms/{roomId}/media",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID")
        ),
        request_body = ClearPlaylistRequest,
        responses(
            (status = 200, description = "Playlist cleared", body = ClearPlaylistResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn clear_playlist(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(req): Json<ClearPlaylistRequest>,
) -> AppResult<Json<ClearPlaylistResponse>> {
    let room_id = path.room_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, authenticated| async move {
            client_api
                .clear_playlist(&authenticated.user_id(), &room_id, req)
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
        path = "/api/rooms/{roomId}/media/{mediaId}",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("mediaId" = String, Path, description = "Media ID")
        ),
        responses(
            (status = 200, description = "Media details", body = synctv_proto::client::Media),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Media not found", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_media(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomMediaTargetPathRequest>,
) -> AppResult<Json<synctv_proto::client::Media>> {
    let synctv_proto::client::RoomMediaTargetPathRequest { room_id, media_id } = path;
    let media =
        execute_room_actor_endpoint(
            &state,
            request_meta,
            room_id,
            EndpointRateLimitCategory::Read,
            EndpointRateLimitScope::RoomGet,
            move |client_api, actor| async move {
                client_api.get_media_for_actor(&actor, &media_id).await
            },
        )
        .await?;

    Ok(Json(media))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{roomId}/playlists/{playlistId}",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("playlistId" = String, Path, description = "Playlist ID")
        ),
        responses(
            (status = 200, description = "Playlist details", body = synctv_proto::client::GetPlaylistResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Playlist not found", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_playlist(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPlaylistTargetPathRequest>,
) -> AppResult<Json<synctv_proto::client::GetPlaylistResponse>> {
    let synctv_proto::client::RoomPlaylistTargetPathRequest {
        room_id,
        playlist_id,
    } = path;
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomGet,
        move |client_api, actor| async move {
            client_api
                .get_playlist_for_actor(&actor, &playlist_id)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}
