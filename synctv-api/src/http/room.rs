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

use super::validation::{StrictQuery, ValidatedQuery};
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
    ListPlaylistItemsRequest, ListPlaylistsRequest, ListPlaylistsResponse, ListRoomStreamsRequest,
    ListRoomStreamsResponse, ListRoomsRequest, ListRoomsResponse, MoveMediaRequest,
    MoveMediaResponse, MovePlaylistRequest, MovePlaylistResponse, ResetRoomSettingsResponse,
    SetRoomPasswordRequest, SetRoomPasswordResponse, StartPlaybackRequest, StartPlaybackResponse,
    StopPlaybackRequest, StopPlaybackResponse, TransferRoomOwnershipRequest,
    TransferRoomOwnershipResponse, UpdatePlayback, UpdatePlaylistRequest, UpdatePlaylistResponse,
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

#[derive(Debug, Default, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct GetPlaybackQuery {
    pub delivery_preference: Option<String>,
    pub max_streaming_bitrate: Option<i64>,
    pub max_audio_channels: Option<i32>,
    pub video_codecs: Option<String>,
    pub containers: Option<String>,
    pub audio_capability: Option<String>,
    pub subtitle_preference: Option<String>,
}

fn parse_delivery_preference(
    value: Option<&str>,
) -> Result<crate::proto::client::PlaybackDeliveryPreference, super::AppError> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(crate::proto::client::PlaybackDeliveryPreference::Unspecified),
        Some("auto") => Ok(crate::proto::client::PlaybackDeliveryPreference::Auto),
        Some("direct_play") => Ok(crate::proto::client::PlaybackDeliveryPreference::DirectPlay),
        Some("transcode") => Ok(crate::proto::client::PlaybackDeliveryPreference::Transcode),
        Some(other) => Err(super::AppError::bad_request(format!(
            "Invalid delivery_preference '{other}'. Expected auto, direct_play, or transcode"
        ))),
    }
}

fn parse_subtitle_preference(
    value: Option<&str>,
) -> Result<crate::proto::client::PlaybackSubtitlePreference, super::AppError> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(crate::proto::client::PlaybackSubtitlePreference::Unspecified),
        Some("external") => Ok(crate::proto::client::PlaybackSubtitlePreference::External),
        Some("embedded_or_external") => {
            Ok(crate::proto::client::PlaybackSubtitlePreference::EmbeddedOrExternal)
        }
        Some("none") => Ok(crate::proto::client::PlaybackSubtitlePreference::None),
        Some(other) => Err(super::AppError::bad_request(format!(
            "Invalid subtitle_preference '{other}'. Expected external, embedded_or_external, or none"
        ))),
    }
}

fn parse_video_codecs(value: Option<&str>) -> Result<Vec<i32>, super::AppError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(Vec::new());
    };

    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|codec| match codec {
            "h264" => Ok(crate::proto::client::PlaybackVideoCodec::H264 as i32),
            "hevc" => Ok(crate::proto::client::PlaybackVideoCodec::Hevc as i32),
            "vp9" => Ok(crate::proto::client::PlaybackVideoCodec::Vp9 as i32),
            "av1" => Ok(crate::proto::client::PlaybackVideoCodec::Av1 as i32),
            other => Err(super::AppError::bad_request(format!(
                "Invalid video codec '{other}'. Expected h264, hevc, vp9, or av1"
            ))),
        })
        .collect()
}

fn parse_containers(value: Option<&str>) -> Result<Vec<i32>, super::AppError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(Vec::new());
    };

    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|container| match container {
            "mp4" => Ok(crate::proto::client::PlaybackContainer::Mp4 as i32),
            "mkv" => Ok(crate::proto::client::PlaybackContainer::Mkv as i32),
            "webm" => Ok(crate::proto::client::PlaybackContainer::Webm as i32),
            other => Err(super::AppError::bad_request(format!(
                "Invalid container '{other}'. Expected mp4, mkv, or webm"
            ))),
        })
        .collect()
}

fn parse_audio_capability(
    value: Option<&str>,
) -> Result<crate::proto::client::PlaybackAudioCapability, super::AppError> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(crate::proto::client::PlaybackAudioCapability::Unspecified),
        Some("stereo") => Ok(crate::proto::client::PlaybackAudioCapability::Stereo),
        Some("surround") => Ok(crate::proto::client::PlaybackAudioCapability::Surround),
        Some("lossless_surround") => {
            Ok(crate::proto::client::PlaybackAudioCapability::LosslessSurround)
        }
        Some(other) => Err(super::AppError::bad_request(format!(
            "Invalid audio_capability '{other}'. Expected stereo, surround, or lossless_surround"
        ))),
    }
}

fn build_get_playback_request(query: &GetPlaybackQuery) -> AppResult<GetPlaybackRequest> {
    let has_profile = query.delivery_preference.is_some()
        || query.max_streaming_bitrate.is_some()
        || query.max_audio_channels.is_some()
        || query.video_codecs.is_some()
        || query.containers.is_some()
        || query.audio_capability.is_some()
        || query.subtitle_preference.is_some();

    let playback_client_profile = if has_profile {
        Some(crate::proto::client::PlaybackClientProfile {
            delivery_preference: parse_delivery_preference(query.delivery_preference.as_deref())?
                as i32,
            max_streaming_bitrate: query.max_streaming_bitrate,
            max_audio_channels: query.max_audio_channels,
            supported_video_codecs: parse_video_codecs(query.video_codecs.as_deref())?,
            supported_containers: parse_containers(query.containers.as_deref())?,
            audio_capability: parse_audio_capability(query.audio_capability.as_deref())? as i32,
            subtitle_preference: parse_subtitle_preference(query.subtitle_preference.as_deref())?
                as i32,
        })
    } else {
        None
    };

    let request = GetPlaybackRequest {
        playback_client_profile,
    };
    crate::impls::validate_proto_request(&request).map_err(super::error::map_api_error)?;
    Ok(request)
}

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
            client_api.create_room(&authenticated.user_id, req).await
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
            client_api.get_room(&authenticated.user_id, &room_id).await
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
                        &authenticated.user_id,
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
                .delete_room(&authenticated.user_id, &room_id)
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
                .add_media(&authenticated.user_id, &room_id, req)
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
                .delete_media(&authenticated.user_id, &room_id, proto_req)
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
                .delete_entries(&authenticated.user_id, &room_id, req)
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
                .move_media(&authenticated.user_id, &room_id, req)
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
                .list_playlist_items(&authenticated.user_id, &room_id, req)
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
                .start_playback(&authenticated.user_id, &room_id, req)
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
                .stop_playback(&authenticated.user_id, &room_id, req)
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
            ("room_id" = String, Path, description = "Room ID"),
            GetPlaybackQuery
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
    StrictQuery(query): StrictQuery<GetPlaybackQuery>,
) -> AppResult<Json<GetPlaybackResponse>> {
    let room_id = validate_room_path(path)?;
    let req = build_get_playback_request(&query)?;
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
                        &authenticated.user_id,
                        &room_id,
                        req,
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
                .get_room_members(&authenticated.user_id, &room_id, req)
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
        path = "/api/rooms/{room_id}/streams",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ListRoomStreamsRequest
        ),
        responses(
            (status = 200, description = "Active room live streams", body = ListRoomStreamsResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Room not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn list_room_streams(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    ValidatedQuery(req): ValidatedQuery<ListRoomStreamsRequest>,
) -> AppResult<Json<ListRoomStreamsResponse>> {
    let room_id = validate_room_path(path)?;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |client_api, authenticated| async move {
            client_api
                .list_room_streams(&authenticated.user_id, &room_id, req)
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
                .set_room_password(&authenticated.user_id, &room_id, req)
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
                .get_room_settings(&authenticated.user_id, &room_id)
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
                .add_media_batch(&authenticated.user_id, &room_id, req)
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
                .edit_media(&authenticated.user_id, &room_id, req)
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
                .clear_playlist(&authenticated.user_id, &room_id)
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
                .get_media(&authenticated.user_id, &room_id, &media_id)
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
                .get_playlist(&authenticated.user_id, &room_id, &playlist_id)
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
                .update_room_settings(&authenticated.user_id, &room_id, req)
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
                .transfer_room_ownership(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

/// Unified handler for updating playback state via PATCH
/// PATCH /`api/rooms/:room_id/playback`
/// Supports play/pause/seek/speed state updates. Playback target changes are
/// handled by start/stop endpoints.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/rooms/{room_id}/playback",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = UpdatePlayback,
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
    Json(req): Json<UpdatePlayback>,
) -> AppResult<Json<GetPlaybackResponse>> {
    let room_id = validate_room_path(path)?;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        move |client_api, authenticated| async move {
            client_api
                .update_playback(&authenticated.user_id, &room_id, req)
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
                .reset_room_settings(&authenticated.user_id, &room_id)
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
                .get_chat_history(&authenticated.user_id, &room_id, req)
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
                .create_playlist(&authenticated.user_id, &room_id, req)
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
                .move_playlist(&authenticated.user_id, &room_id, req)
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
                .delete_playlist(&authenticated.user_id, &room_id, req)
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
                .list_playlists(&authenticated.user_id, &room_id, req)
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
        build_get_playback_request, parse_optional_query_bool, parse_optional_query_i32,
        validate_chat_history_query, AddMediaBatchBody, CreatePlaylistBody, DeleteEntriesBody,
        GetPlaybackQuery, UpdatePlayback,
    };
    use crate::proto::client::{
        DeleteMediaQuery, DeletePlaylistQuery, GetChatHistoryRequest, GetRoomMembersRequest,
        ListPlaylistItemsRequest, ListPlaylistsRequest, ListRoomsRequest, MoveMediaRequest,
    };

    #[test]
    fn test_update_playback_deserialize_playing_update() {
        let json = r#"{"type":1,"playing":true}"#;
        let req: UpdatePlayback = serde_json::from_str(json).unwrap();
        assert_eq!(
            req.r#type,
            crate::proto::client::PlaybackUpdateType::Play as i32
        );
        assert_eq!(req.playing, Some(true));
        assert!(req.position.is_none());
        assert!(req.speed.is_none());
    }

    #[test]
    fn test_update_playback_deserialize_seek_update() {
        let json = r#"{"type":3,"position": 42.5}"#;
        let req: UpdatePlayback = serde_json::from_str(json).unwrap();
        assert_eq!(
            req.r#type,
            crate::proto::client::PlaybackUpdateType::Seek as i32
        );
        assert!((req.position.unwrap() - 42.5).abs() < f64::EPSILON);
        assert!(req.speed.is_none());
    }

    #[test]
    fn test_update_playback_deserialize_speed_update() {
        let json = r#"{"type":4,"speed": 2.0}"#;
        let req: UpdatePlayback = serde_json::from_str(json).unwrap();
        assert_eq!(
            req.r#type,
            crate::proto::client::PlaybackUpdateType::Speed as i32
        );
        assert!(req.position.is_none());
        assert!((req.speed.unwrap() - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_update_playback_deserialize_full_state() {
        let json = r#"{"type":3,"playing":false,"position":42.5,"speed":1.25,"version":9}"#;
        let req: UpdatePlayback = serde_json::from_str(json).unwrap();
        assert_eq!(
            req.r#type,
            crate::proto::client::PlaybackUpdateType::Seek as i32
        );
        assert_eq!(req.playing, Some(false));
        assert_eq!(req.position, Some(42.5));
        assert_eq!(req.speed, Some(1.25));
        assert_eq!(req.version, Some(9));
    }

    #[test]
    fn test_build_get_playback_request_parses_generic_profile_query() {
        let request = build_get_playback_request(&GetPlaybackQuery {
            delivery_preference: Some("transcode".to_string()),
            max_streaming_bitrate: Some(8_000_000),
            max_audio_channels: Some(2),
            video_codecs: Some("h264,av1".to_string()),
            containers: Some("mp4,webm".to_string()),
            audio_capability: Some("surround".to_string()),
            subtitle_preference: Some("embedded_or_external".to_string()),
        })
        .expect("playback query should parse");

        let profile = request
            .playback_client_profile
            .expect("query should produce playback client profile");
        assert_eq!(
            profile.delivery_preference,
            crate::proto::client::PlaybackDeliveryPreference::Transcode as i32
        );
        assert_eq!(profile.max_streaming_bitrate, Some(8_000_000));
        assert_eq!(profile.max_audio_channels, Some(2));
        assert_eq!(
            profile.supported_video_codecs,
            vec![
                crate::proto::client::PlaybackVideoCodec::H264 as i32,
                crate::proto::client::PlaybackVideoCodec::Av1 as i32,
            ]
        );
        assert_eq!(
            profile.supported_containers,
            vec![
                crate::proto::client::PlaybackContainer::Mp4 as i32,
                crate::proto::client::PlaybackContainer::Webm as i32,
            ]
        );
        assert_eq!(
            profile.audio_capability,
            crate::proto::client::PlaybackAudioCapability::Surround as i32
        );
        assert_eq!(
            profile.subtitle_preference,
            crate::proto::client::PlaybackSubtitlePreference::EmbeddedOrExternal as i32
        );
    }

    #[test]
    fn test_build_get_playback_request_omits_profile_when_query_is_empty() {
        let request = build_get_playback_request(&GetPlaybackQuery::default())
            .expect("empty query should be valid");

        assert!(request.playback_client_profile.is_none());
    }

    #[test]
    fn test_build_get_playback_request_rejects_invalid_video_codec() {
        let error = build_get_playback_request(&GetPlaybackQuery {
            delivery_preference: None,
            max_streaming_bitrate: None,
            max_audio_channels: None,
            video_codecs: Some("h264,divx".to_string()),
            containers: None,
            audio_capability: None,
            subtitle_preference: None,
        })
        .expect_err("unknown codec must be rejected");

        assert!(error.message.contains("video codec"), "{error:?}");
    }

    #[test]
    fn test_build_get_playback_request_rejects_invalid_delivery_preference() {
        let error = build_get_playback_request(&GetPlaybackQuery {
            delivery_preference: Some("download".to_string()),
            max_streaming_bitrate: None,
            max_audio_channels: None,
            video_codecs: None,
            containers: None,
            audio_capability: None,
            subtitle_preference: None,
        })
        .expect_err("unknown delivery preference must be rejected");

        assert!(error.message.contains("delivery_preference"), "{error:?}");
    }

    #[test]
    fn test_build_get_playback_request_rejects_invalid_container() {
        let error = build_get_playback_request(&GetPlaybackQuery {
            delivery_preference: None,
            max_streaming_bitrate: None,
            max_audio_channels: None,
            video_codecs: None,
            containers: Some("mp4,avi".to_string()),
            audio_capability: None,
            subtitle_preference: None,
        })
        .expect_err("unknown container must be rejected");

        assert!(error.message.contains("container"), "{error:?}");
    }

    #[test]
    fn test_build_get_playback_request_rejects_invalid_audio_capability() {
        let error = build_get_playback_request(&GetPlaybackQuery {
            delivery_preference: None,
            max_streaming_bitrate: None,
            max_audio_channels: None,
            video_codecs: None,
            containers: None,
            audio_capability: Some("mono".to_string()),
            subtitle_preference: None,
        })
        .expect_err("unknown audio capability must be rejected");

        assert!(error.message.contains("audio_capability"), "{error:?}");
    }

    #[test]
    fn test_build_get_playback_request_rejects_invalid_subtitle_preference() {
        let error = build_get_playback_request(&GetPlaybackQuery {
            delivery_preference: None,
            max_streaming_bitrate: None,
            max_audio_channels: None,
            video_codecs: None,
            containers: None,
            audio_capability: None,
            subtitle_preference: Some("burn_in".to_string()),
        })
        .expect_err("unknown subtitle preference must be rejected");

        assert!(error.message.contains("subtitle_preference"), "{error:?}");
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
            serde_json::from_str(r#"{"room_id":"room_1"}"#).unwrap();

        assert_eq!(req.room_id, "room_1");
    }

    #[test]
    fn test_room_path_request_deserializes_proto_field_name() {
        let req: crate::proto::client::RoomPathRequest =
            serde_json::from_str(r#"{"room_id":"room_1"}"#).unwrap();

        assert_eq!(req.room_id, "room_1");
    }

    #[test]
    fn test_room_media_target_path_request_deserializes_proto_field_names() {
        let req: crate::proto::client::RoomMediaTargetPathRequest =
            serde_json::from_str(r#"{"room_id":"room_1","media_id":"med_1"}"#).unwrap();

        assert_eq!(req.room_id, "room_1");
        assert_eq!(req.media_id, "med_1");
    }

    #[test]
    fn test_room_playlist_target_path_request_deserializes_proto_field_names() {
        let req: crate::proto::client::RoomPlaylistTargetPathRequest =
            serde_json::from_str(r#"{"room_id":"room_1","playlist_id":"pl_1"}"#).unwrap();

        assert_eq!(req.room_id, "room_1");
        assert_eq!(req.playlist_id, "pl_1");
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
        let json = r#"{"type": 1, "version": 42}"#;
        let req: UpdatePlayback = serde_json::from_str(json).unwrap();
        assert_eq!(
            req.r#type,
            crate::proto::client::PlaybackUpdateType::Play as i32
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
