use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};

use super::execute::{execute_public_endpoint, execute_room_actor_endpoint, execute_user_endpoint};
use super::types::{
    ChatImageObjectPath, ChatImageObjectQuery, PlaylistCoverObjectPath, PlaylistCoverObjectQuery,
    RoomCoverObjectPath, RoomCoverObjectQuery, VideoCoverObjectPath, VideoCoverObjectQuery,
};
use crate::http::{middleware::RequestMetadata, AppResult, AppState};
use crate::impls::{EndpointRateLimitCategory, EndpointRateLimitScope};
use synctv_proto::client::{
    CreateChatImageUploadSessionRequest, CreateChatImageUploadSessionResponse,
    CreatePlaylistCoverUploadSessionRequest, CreatePlaylistCoverUploadSessionResponse,
    CreateRoomCoverUploadSessionRequest, CreateRoomCoverUploadSessionResponse,
    CreateVideoCoverUploadSessionRequest, CreateVideoCoverUploadSessionResponse, EditMediaResponse,
    GetRoomResponse, UpdatePlaylistCoverRequest, UpdatePlaylistResponse, UpdateRoomCoverRequest,
    UpdateVideoCoverRequest,
};

pub async fn create_video_cover_upload_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomMediaTargetPathRequest>,
    Json(mut req): Json<CreateVideoCoverUploadSessionRequest>,
) -> AppResult<Json<CreateVideoCoverUploadSessionResponse>> {
    let synctv_proto::client::RoomMediaTargetPathRequest { room_id, media_id } = path;
    req.room_id = room_id.clone();
    req.media_id = media_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::MediaCover,
        move |client_api, authenticated| async move {
            client_api
                .create_video_cover_upload_session(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn update_video_cover(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomMediaTargetPathRequest>,
    Json(mut req): Json<UpdateVideoCoverRequest>,
) -> AppResult<Json<EditMediaResponse>> {
    let synctv_proto::client::RoomMediaTargetPathRequest { room_id, media_id } = path;
    req.room_id = room_id.clone();
    req.media_id = media_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::MediaCover,
        move |client_api, authenticated| async move {
            client_api
                .update_video_cover(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn clear_video_cover(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomMediaTargetPathRequest>,
) -> AppResult<Json<EditMediaResponse>> {
    let synctv_proto::client::RoomMediaTargetPathRequest { room_id, media_id } = path;
    let req = synctv_proto::client::ClearVideoCoverRequest {
        room_id: room_id.clone(),
        media_id,
    };
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::MediaCover,
        move |client_api, authenticated| async move {
            client_api
                .clear_video_cover(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn create_room_cover_upload_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(mut req): Json<CreateRoomCoverUploadSessionRequest>,
) -> AppResult<Json<CreateRoomCoverUploadSessionResponse>> {
    req.room_id = room_id.clone();
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomCover,
        move |client_api, authenticated| async move {
            client_api
                .create_room_cover_upload_session(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn update_room_cover(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(mut req): Json<UpdateRoomCoverRequest>,
) -> AppResult<Json<GetRoomResponse>> {
    req.room_id = room_id.clone();
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomCover,
        move |client_api, authenticated| async move {
            client_api
                .update_room_cover(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn clear_room_cover(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> AppResult<Json<GetRoomResponse>> {
    let req = synctv_proto::client::ClearRoomCoverRequest {
        room_id: room_id.clone(),
    };
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomCover,
        move |client_api, authenticated| async move {
            client_api
                .clear_room_cover(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn create_playlist_cover_upload_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPlaylistTargetPathRequest>,
    Json(mut req): Json<CreatePlaylistCoverUploadSessionRequest>,
) -> AppResult<Json<CreatePlaylistCoverUploadSessionResponse>> {
    let synctv_proto::client::RoomPlaylistTargetPathRequest {
        room_id,
        playlist_id,
    } = path;
    req.room_id = room_id.clone();
    req.playlist_id = playlist_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::PlaylistCover,
        move |client_api, authenticated| async move {
            client_api
                .create_playlist_cover_upload_session(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn update_playlist_cover(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPlaylistTargetPathRequest>,
    Json(mut req): Json<UpdatePlaylistCoverRequest>,
) -> AppResult<Json<UpdatePlaylistResponse>> {
    let synctv_proto::client::RoomPlaylistTargetPathRequest {
        room_id,
        playlist_id,
    } = path;
    req.room_id = room_id.clone();
    req.playlist_id = playlist_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::PlaylistCover,
        move |client_api, authenticated| async move {
            client_api
                .update_playlist_cover(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn clear_playlist_cover(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPlaylistTargetPathRequest>,
) -> AppResult<Json<UpdatePlaylistResponse>> {
    let synctv_proto::client::RoomPlaylistTargetPathRequest {
        room_id,
        playlist_id,
    } = path;
    let req = synctv_proto::client::ClearPlaylistCoverRequest {
        room_id: room_id.clone(),
        playlist_id,
    };
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::PlaylistCover,
        move |client_api, authenticated| async move {
            client_api
                .clear_playlist_cover(&authenticated.user_id, &room_id, req)
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
        path = "/api/rooms/{room_id}/chat/images/upload-session",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = CreateChatImageUploadSessionRequest,
        responses(
            (status = 200, description = "Chat image upload session", body = CreateChatImageUploadSessionResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Insufficient room permission", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn create_chat_image_upload_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(req): Json<CreateChatImageUploadSessionRequest>,
) -> AppResult<Json<CreateChatImageUploadSessionResponse>> {
    let room_id = path.room_id;
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomChat,
        move |client_api, actor| async move {
            client_api
                .create_chat_image_upload_session_for_actor(&actor, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn upload_chat_image_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<ChatImageObjectPath>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<StatusCode> {
    let upload_token = super::super::required_header_str(
        &headers,
        synctv_core::service::file_storage::FILE_UPLOAD_TOKEN_HEADER,
        "Missing file upload token",
    )?;
    let content_type = super::super::optional_header_str(&headers, &header::CONTENT_TYPE)?;
    let encoded_object_key = path.encoded_object_key;
    let upload_token = upload_token.to_string();
    let content_type = content_type.map(str::to_string);
    let data = body.to_vec();
    execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomChat,
        move |client_api| async move {
            let chat_service = client_api.chat_service.as_ref().ok_or_else(|| {
                crate::impls::ApiError::ServiceUnavailable(
                    "Chat service is unavailable".to_string(),
                )
            })?;
            chat_service
                .store_image_upload_object(
                    &encoded_object_key,
                    &upload_token,
                    content_type.as_deref(),
                    data,
                )
                .await
                .map(|_| ())
                .map_err(crate::impls::ApiError::from)
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn get_chat_image_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<ChatImageObjectPath>,
    Query(query): Query<ChatImageObjectQuery>,
) -> AppResult<Response> {
    let encoded_object_key = path.encoded_object_key;
    let token = query.token;
    let blob = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomChat,
        move |client_api| async move {
            let chat_service = client_api.chat_service.as_ref().ok_or_else(|| {
                crate::impls::ApiError::ServiceUnavailable(
                    "Chat service is unavailable".to_string(),
                )
            })?;
            chat_service
                .get_image_object(&encoded_object_key, &token)
                .await
                .map_err(crate::impls::ApiError::from)
        },
    )
    .await?;
    let headers = [
        (header::CONTENT_TYPE, blob.mime_type),
        (
            header::CACHE_CONTROL,
            "private, max-age=31536000, immutable".to_string(),
        ),
    ];
    Ok((headers, blob.data).into_response())
}

pub async fn upload_video_cover_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<VideoCoverObjectPath>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<StatusCode> {
    let upload_token = super::super::required_header_str(
        &headers,
        synctv_core::service::file_storage::FILE_UPLOAD_TOKEN_HEADER,
        "Missing file upload token",
    )?;
    let content_type = super::super::optional_header_str(&headers, &header::CONTENT_TYPE)?;
    let req = synctv_proto::client::UploadVideoCoverObjectRequest {
        encoded_object_key: path.encoded_object_key,
        token: upload_token.to_string(),
        content_type: content_type.map(str::to_string),
        data: body.to_vec(),
    };
    execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::MediaCover,
        move |client_api| async move { client_api.upload_video_cover_object(req).await },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn get_video_cover_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<VideoCoverObjectPath>,
    Query(query): Query<VideoCoverObjectQuery>,
) -> AppResult<Response> {
    let req = synctv_proto::client::GetVideoCoverObjectRequest {
        encoded_object_key: path.encoded_object_key,
        token: query.token,
    };
    let blob = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomChat,
        move |client_api| async move { client_api.get_video_cover_object(req).await },
    )
    .await?;
    let headers = [
        (header::CONTENT_TYPE, blob.mime_type),
        (
            header::CACHE_CONTROL,
            "private, max-age=31536000, immutable".to_string(),
        ),
    ];
    Ok((headers, blob.data).into_response())
}

pub async fn upload_room_cover_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<RoomCoverObjectPath>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<StatusCode> {
    let upload_token = super::super::required_header_str(
        &headers,
        synctv_core::service::file_storage::FILE_UPLOAD_TOKEN_HEADER,
        "Missing file upload token",
    )?;
    let content_type = super::super::optional_header_str(&headers, &header::CONTENT_TYPE)?;
    let req = synctv_proto::client::UploadRoomCoverObjectRequest {
        encoded_object_key: path.encoded_object_key,
        token: upload_token.to_string(),
        content_type: content_type.map(str::to_string),
        data: body.to_vec(),
    };
    execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomCover,
        move |client_api| async move { client_api.upload_room_cover_object(req).await },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn get_room_cover_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<RoomCoverObjectPath>,
    Query(query): Query<RoomCoverObjectQuery>,
) -> AppResult<Response> {
    let req = synctv_proto::client::GetRoomCoverObjectRequest {
        encoded_object_key: path.encoded_object_key,
        token: query.token,
    };
    let blob = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomCover,
        move |client_api| async move { client_api.get_room_cover_object(req).await },
    )
    .await?;
    let headers = [
        (header::CONTENT_TYPE, blob.mime_type),
        (
            header::CACHE_CONTROL,
            "private, max-age=31536000, immutable".to_string(),
        ),
    ];
    Ok((headers, blob.data).into_response())
}

pub async fn upload_playlist_cover_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<PlaylistCoverObjectPath>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<StatusCode> {
    let upload_token = super::super::required_header_str(
        &headers,
        synctv_core::service::file_storage::FILE_UPLOAD_TOKEN_HEADER,
        "Missing file upload token",
    )?;
    let content_type = super::super::optional_header_str(&headers, &header::CONTENT_TYPE)?;
    let req = synctv_proto::client::UploadPlaylistCoverObjectRequest {
        encoded_object_key: path.encoded_object_key,
        token: upload_token.to_string(),
        content_type: content_type.map(str::to_string),
        data: body.to_vec(),
    };
    execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::PlaylistCover,
        move |client_api| async move { client_api.upload_playlist_cover_object(req).await },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn get_playlist_cover_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<PlaylistCoverObjectPath>,
    Query(query): Query<PlaylistCoverObjectQuery>,
) -> AppResult<Response> {
    let req = synctv_proto::client::GetPlaylistCoverObjectRequest {
        encoded_object_key: path.encoded_object_key,
        token: query.token,
    };
    let blob = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::MediaCover,
        move |client_api| async move { client_api.get_playlist_cover_object(req).await },
    )
    .await?;
    let headers = [
        (header::CONTENT_TYPE, blob.mime_type),
        (
            header::CACHE_CONTROL,
            "private, max-age=31536000, immutable".to_string(),
        ),
    ];
    Ok((headers, blob.data).into_response())
}
