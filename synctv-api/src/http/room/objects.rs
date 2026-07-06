use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};

use super::execute::{execute_public_endpoint, execute_room_actor_endpoint, execute_user_endpoint};
use super::types::{
    ChatAttachmentObjectPath, ChatAttachmentObjectQuery, MediaCoverObjectPath,
    MediaCoverObjectQuery, MediaThumbnailObjectPath, MediaThumbnailObjectQuery,
    PlaylistCoverObjectPath, PlaylistCoverObjectQuery, RoomCoverObjectPath, RoomCoverObjectQuery,
};
use crate::http::{middleware::RequestMetadata, AppResult, AppState};
use crate::impls::{EndpointRateLimitCategory, EndpointRateLimitScope};
use synctv_proto::client::{
    CompleteChatAttachmentUploadSessionRequest, CompleteChatAttachmentUploadSessionResponse,
    CompleteMediaCoverUploadSessionRequest, CompleteMediaCoverUploadSessionResponse,
    CompleteMediaThumbnailUploadSessionRequest, CompleteMediaThumbnailUploadSessionResponse,
    CompletePlaylistCoverUploadSessionRequest, CompletePlaylistCoverUploadSessionResponse,
    CompleteRoomCoverUploadSessionRequest, CompleteRoomCoverUploadSessionResponse,
    CreateChatAttachmentUploadSessionRequest, CreateChatAttachmentUploadSessionResponse,
    CreateMediaCoverUploadSessionRequest, CreateMediaCoverUploadSessionResponse,
    CreateMediaThumbnailUploadSessionRequest, CreateMediaThumbnailUploadSessionResponse,
    CreatePlaylistCoverUploadSessionRequest, CreatePlaylistCoverUploadSessionResponse,
    CreateRoomCoverUploadSessionRequest, CreateRoomCoverUploadSessionResponse, GetRoomResponse,
    Media, Playlist, UpdateMediaCoverRequest, UpdateMediaThumbnailRequest,
    UpdatePlaylistCoverRequest, UpdateRoomCoverRequest, UploadChatAttachmentObjectRequest,
};

fn file_upload_range_to_proto(
    range: synctv_core::models::FileUploadRange,
) -> synctv_proto::client::FileUploadRange {
    synctv_proto::client::FileUploadRange {
        start: range.start,
        end_inclusive: range.end_inclusive,
        total_size: range.total_size,
    }
}

fn upload_response_headers(
    complete: bool,
    uploaded_size_bytes: i64,
    uploaded_parts: &[i32],
) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("x-synctv-upload-complete"),
        HeaderValue::from_static(if complete { "true" } else { "false" }),
    );
    if let Ok(value) = HeaderValue::from_str(&uploaded_size_bytes.to_string()) {
        headers.insert(
            HeaderName::from_static("x-synctv-uploaded-size-bytes"),
            value,
        );
    }
    let uploaded_parts = uploaded_parts
        .iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    if let Ok(value) = HeaderValue::from_str(&uploaded_parts) {
        headers.insert(HeaderName::from_static("x-synctv-uploaded-parts"), value);
    }
    headers
}

pub async fn create_media_cover_upload_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomMediaTargetPathRequest>,
    Json(mut req): Json<CreateMediaCoverUploadSessionRequest>,
) -> AppResult<Json<CreateMediaCoverUploadSessionResponse>> {
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
                .create_media_cover_upload_session(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn update_media_cover(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomMediaTargetPathRequest>,
    Json(mut req): Json<UpdateMediaCoverRequest>,
) -> AppResult<Json<Media>> {
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
                .update_media_cover(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn clear_media_cover(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomMediaTargetPathRequest>,
) -> AppResult<Json<Media>> {
    let synctv_proto::client::RoomMediaTargetPathRequest { room_id, media_id } = path;
    let req = synctv_proto::client::ClearMediaCoverRequest {
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
                .clear_media_cover(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn create_media_thumbnail_upload_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomMediaTargetPathRequest>,
    Json(mut req): Json<CreateMediaThumbnailUploadSessionRequest>,
) -> AppResult<Json<CreateMediaThumbnailUploadSessionResponse>> {
    let synctv_proto::client::RoomMediaTargetPathRequest { room_id, media_id } = path;
    req.room_id = room_id.clone();
    req.media_id = media_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::MediaThumbnail,
        move |client_api, authenticated| async move {
            client_api
                .create_media_thumbnail_upload_session(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn update_media_thumbnail(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomMediaTargetPathRequest>,
    Json(mut req): Json<UpdateMediaThumbnailRequest>,
) -> AppResult<Json<Media>> {
    let synctv_proto::client::RoomMediaTargetPathRequest { room_id, media_id } = path;
    req.room_id = room_id.clone();
    req.media_id = media_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::MediaThumbnail,
        move |client_api, authenticated| async move {
            client_api
                .update_media_thumbnail(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn clear_media_thumbnail(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomMediaTargetPathRequest>,
) -> AppResult<Json<Media>> {
    let synctv_proto::client::RoomMediaTargetPathRequest { room_id, media_id } = path;
    let req = synctv_proto::client::ClearMediaThumbnailRequest {
        room_id: room_id.clone(),
        media_id,
    };
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::MediaThumbnail,
        move |client_api, authenticated| async move {
            client_api
                .clear_media_thumbnail(&authenticated.user_id, &room_id, req)
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
) -> AppResult<Json<Playlist>> {
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
) -> AppResult<Json<Playlist>> {
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
        path = "/api/rooms/{roomId}/chat/attachments/upload-session",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID")
        ),
        request_body = CreateChatAttachmentUploadSessionRequest,
        responses(
            (status = 200, description = "Chat attachment upload session", body = CreateChatAttachmentUploadSessionResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Insufficient room permission", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn create_chat_attachment_upload_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(req): Json<CreateChatAttachmentUploadSessionRequest>,
) -> AppResult<Json<CreateChatAttachmentUploadSessionResponse>> {
    let room_id = path.room_id;
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomChat,
        move |client_api, actor| async move {
            client_api
                .create_chat_attachment_upload_session_for_actor(&actor, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn upload_chat_attachment_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<ChatAttachmentObjectPath>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<Response> {
    let upload_token = super::super::required_header_str(
        &headers,
        synctv_core::service::FILE_UPLOAD_TOKEN_HEADER,
        "Missing file upload token",
    )?;
    let content_type = super::super::optional_header_str(&headers, &header::CONTENT_TYPE)?;
    let encoded_object_key = path.encoded_object_key;
    let upload_token = upload_token.to_string();
    let content_type = content_type.map(str::to_string);
    let range = super::super::optional_content_range(&headers)?;
    let data = body.to_vec();
    let req = UploadChatAttachmentObjectRequest {
        room_id: String::new(),
        encoded_object_key,
        token: upload_token,
        content_type,
        data,
        content_range: range.map(file_upload_range_to_proto),
    };
    let response = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomChat,
        move |client_api| async move { client_api.upload_chat_attachment_object(req).await },
    )
    .await?;
    Ok((
        upload_response_headers(
            response.complete,
            response.uploaded_size_bytes,
            &response.uploaded_parts,
        ),
        StatusCode::NO_CONTENT,
    )
        .into_response())
}

pub async fn get_chat_attachment_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<ChatAttachmentObjectPath>,
    headers: HeaderMap,
    Query(query): Query<ChatAttachmentObjectQuery>,
) -> AppResult<Response> {
    let range = super::super::optional_file_range(&headers)?;
    let req = synctv_proto::client::GetChatAttachmentObjectRequest {
        room_id: String::new(),
        encoded_object_key: path.encoded_object_key,
        token: query.token,
        range: range.map(super::super::file_range_request_to_proto),
    };
    let download = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomChat,
        move |client_api| async move { client_api.get_chat_attachment_object(req).await },
    )
    .await?;
    super::super::file_object_download_response(
        download,
        Some("private, max-age=31536000, immutable"),
    )
}

pub async fn complete_chat_attachment_upload_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<ChatAttachmentObjectPath>,
    Json(mut req): Json<CompleteChatAttachmentUploadSessionRequest>,
) -> AppResult<Json<CompleteChatAttachmentUploadSessionResponse>> {
    req.encoded_object_key = path.encoded_object_key;
    let response = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomChat,
        move |client_api| async move {
            client_api
                .complete_chat_attachment_upload_session(req)
                .await
        },
    )
    .await?;
    Ok(Json(response))
}

pub async fn upload_media_cover_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<MediaCoverObjectPath>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<Response> {
    let upload_token = super::super::required_header_str(
        &headers,
        synctv_core::service::FILE_UPLOAD_TOKEN_HEADER,
        "Missing file upload token",
    )?;
    let content_type = super::super::optional_header_str(&headers, &header::CONTENT_TYPE)?;
    let range = super::super::optional_content_range(&headers)?;
    let req = synctv_proto::client::UploadMediaCoverObjectRequest {
        encoded_object_key: path.encoded_object_key,
        token: upload_token.to_string(),
        content_type: content_type.map(str::to_string),
        content_range: range.map(file_upload_range_to_proto),
        data: body.to_vec(),
    };
    let response = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::MediaCover,
        move |client_api| async move { client_api.upload_media_cover_object(req).await },
    )
    .await?;
    Ok((
        upload_response_headers(
            response.complete,
            response.uploaded_size_bytes,
            &response.uploaded_parts,
        ),
        StatusCode::NO_CONTENT,
    )
        .into_response())
}

pub async fn get_media_cover_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<MediaCoverObjectPath>,
    headers: HeaderMap,
    Query(query): Query<MediaCoverObjectQuery>,
) -> AppResult<Response> {
    let range = super::super::optional_file_range(&headers)?;
    let req = synctv_proto::client::GetMediaCoverObjectRequest {
        encoded_object_key: path.encoded_object_key,
        token: query.token,
        range: range.map(super::super::file_range_request_to_proto),
    };
    let download = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::MediaCover,
        move |client_api| async move { client_api.get_media_cover_object(req).await },
    )
    .await?;
    super::super::file_object_download_response(
        download,
        Some("private, max-age=31536000, immutable"),
    )
}

pub async fn complete_media_cover_upload_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<MediaCoverObjectPath>,
    Json(mut req): Json<CompleteMediaCoverUploadSessionRequest>,
) -> AppResult<Json<CompleteMediaCoverUploadSessionResponse>> {
    req.encoded_object_key = path.encoded_object_key;
    let response = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::MediaCover,
        move |client_api| async move { client_api.complete_media_cover_upload_session(req).await },
    )
    .await?;
    Ok(Json(response))
}

pub async fn upload_media_thumbnail_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<MediaThumbnailObjectPath>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<Response> {
    let upload_token = super::super::required_header_str(
        &headers,
        synctv_core::service::FILE_UPLOAD_TOKEN_HEADER,
        "Missing file upload token",
    )?;
    let content_type = super::super::optional_header_str(&headers, &header::CONTENT_TYPE)?;
    let range = super::super::optional_content_range(&headers)?;
    let req = synctv_proto::client::UploadMediaThumbnailObjectRequest {
        encoded_object_key: path.encoded_object_key,
        token: upload_token.to_string(),
        content_type: content_type.map(str::to_string),
        content_range: range.map(file_upload_range_to_proto),
        data: body.to_vec(),
    };
    let response = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::MediaThumbnail,
        move |client_api| async move { client_api.upload_media_thumbnail_object(req).await },
    )
    .await?;
    Ok((
        upload_response_headers(
            response.complete,
            response.uploaded_size_bytes,
            &response.uploaded_parts,
        ),
        StatusCode::NO_CONTENT,
    )
        .into_response())
}

pub async fn get_media_thumbnail_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<MediaThumbnailObjectPath>,
    headers: HeaderMap,
    Query(query): Query<MediaThumbnailObjectQuery>,
) -> AppResult<Response> {
    let range = super::super::optional_file_range(&headers)?;
    let req = synctv_proto::client::GetMediaThumbnailObjectRequest {
        encoded_object_key: path.encoded_object_key,
        token: query.token,
        range: range.map(super::super::file_range_request_to_proto),
    };
    let download = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::MediaThumbnail,
        move |client_api| async move { client_api.get_media_thumbnail_object(req).await },
    )
    .await?;
    super::super::file_object_download_response(
        download,
        Some("private, max-age=31536000, immutable"),
    )
}

pub async fn complete_media_thumbnail_upload_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<MediaThumbnailObjectPath>,
    Json(mut req): Json<CompleteMediaThumbnailUploadSessionRequest>,
) -> AppResult<Json<CompleteMediaThumbnailUploadSessionResponse>> {
    req.encoded_object_key = path.encoded_object_key;
    let response = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::MediaThumbnail,
        move |client_api| async move {
            client_api
                .complete_media_thumbnail_upload_session(req)
                .await
        },
    )
    .await?;
    Ok(Json(response))
}

pub async fn upload_room_cover_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<RoomCoverObjectPath>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<Response> {
    let upload_token = super::super::required_header_str(
        &headers,
        synctv_core::service::FILE_UPLOAD_TOKEN_HEADER,
        "Missing file upload token",
    )?;
    let content_type = super::super::optional_header_str(&headers, &header::CONTENT_TYPE)?;
    let range = super::super::optional_content_range(&headers)?;
    let req = synctv_proto::client::UploadRoomCoverObjectRequest {
        encoded_object_key: path.encoded_object_key,
        token: upload_token.to_string(),
        content_type: content_type.map(str::to_string),
        content_range: range.map(file_upload_range_to_proto),
        data: body.to_vec(),
    };
    let response = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomCover,
        move |client_api| async move { client_api.upload_room_cover_object(req).await },
    )
    .await?;
    Ok((
        upload_response_headers(
            response.complete,
            response.uploaded_size_bytes,
            &response.uploaded_parts,
        ),
        StatusCode::NO_CONTENT,
    )
        .into_response())
}

pub async fn get_room_cover_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<RoomCoverObjectPath>,
    headers: HeaderMap,
    Query(query): Query<RoomCoverObjectQuery>,
) -> AppResult<Response> {
    let range = super::super::optional_file_range(&headers)?;
    let req = synctv_proto::client::GetRoomCoverObjectRequest {
        encoded_object_key: path.encoded_object_key,
        token: query.token,
        range: range.map(super::super::file_range_request_to_proto),
    };
    let download = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomCover,
        move |client_api| async move { client_api.get_room_cover_object(req).await },
    )
    .await?;
    super::super::file_object_download_response(
        download,
        Some("private, max-age=31536000, immutable"),
    )
}

pub async fn complete_room_cover_upload_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<RoomCoverObjectPath>,
    Json(mut req): Json<CompleteRoomCoverUploadSessionRequest>,
) -> AppResult<Json<CompleteRoomCoverUploadSessionResponse>> {
    req.encoded_object_key = path.encoded_object_key;
    let response = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomCover,
        move |client_api| async move { client_api.complete_room_cover_upload_session(req).await },
    )
    .await?;
    Ok(Json(response))
}

pub async fn upload_playlist_cover_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<PlaylistCoverObjectPath>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<Response> {
    let upload_token = super::super::required_header_str(
        &headers,
        synctv_core::service::FILE_UPLOAD_TOKEN_HEADER,
        "Missing file upload token",
    )?;
    let content_type = super::super::optional_header_str(&headers, &header::CONTENT_TYPE)?;
    let range = super::super::optional_content_range(&headers)?;
    let req = synctv_proto::client::UploadPlaylistCoverObjectRequest {
        encoded_object_key: path.encoded_object_key,
        token: upload_token.to_string(),
        content_type: content_type.map(str::to_string),
        content_range: range.map(file_upload_range_to_proto),
        data: body.to_vec(),
    };
    let response = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::PlaylistCover,
        move |client_api| async move { client_api.upload_playlist_cover_object(req).await },
    )
    .await?;
    Ok((
        upload_response_headers(
            response.complete,
            response.uploaded_size_bytes,
            &response.uploaded_parts,
        ),
        StatusCode::NO_CONTENT,
    )
        .into_response())
}

pub async fn get_playlist_cover_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<PlaylistCoverObjectPath>,
    headers: HeaderMap,
    Query(query): Query<PlaylistCoverObjectQuery>,
) -> AppResult<Response> {
    let range = super::super::optional_file_range(&headers)?;
    let req = synctv_proto::client::GetPlaylistCoverObjectRequest {
        encoded_object_key: path.encoded_object_key,
        token: query.token,
        range: range.map(super::super::file_range_request_to_proto),
    };
    let download = execute_public_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::PlaylistCover,
        move |client_api| async move { client_api.get_playlist_cover_object(req).await },
    )
    .await?;
    super::super::file_object_download_response(
        download,
        Some("private, max-age=31536000, immutable"),
    )
}

pub async fn complete_playlist_cover_upload_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<PlaylistCoverObjectPath>,
    Json(mut req): Json<CompletePlaylistCoverUploadSessionRequest>,
) -> AppResult<Json<CompletePlaylistCoverUploadSessionResponse>> {
    req.encoded_object_key = path.encoded_object_key;
    let response =
        execute_public_endpoint(
            &state,
            request_meta,
            EndpointRateLimitCategory::Write,
            EndpointRateLimitScope::PlaylistCover,
            move |client_api| async move {
                client_api.complete_playlist_cover_upload_session(req).await
            },
        )
        .await?;
    Ok(Json(response))
}
