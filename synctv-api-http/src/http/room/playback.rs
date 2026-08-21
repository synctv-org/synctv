use axum::{
    extract::{Path, Query, State},
    response::sse::{Event, Sse},
    Json,
};
use std::convert::Infallible;

use super::execute::{execute_room_actor_endpoint_with_control, execute_user_endpoint};
use super::query::{
    build_get_playback_request, build_playback_client_profile_from_watch_query,
    parse_watch_delivery_mode, GetPlaybackQuery, WatchPlaybackQuery, WatchPlaybackStateQuery,
};
use super::watch::open_resource_watch_sse;
use crate::http::validation::ProtoQuery;
use crate::http::websocket::RealtimeTransportFormat;
use crate::http::{middleware::RequestMetadata, AppResult, AppState};
use synctv_api_common::impls::{EndpointRateLimitCategory, EndpointRateLimitScope};
use synctv_proto::client::{
    ClearPlaybackHistoryRequest, ClearPlaybackHistoryResponse, DeletePlaybackHistoryEntryRequest,
    DeletePlaybackHistoryEntryResponse, GetPlaybackResponse, ListPlaybackHistoryRequest,
    ListPlaybackHistoryResponse, PlayHistoryEntryRequest, PlayNextRequest, PlayPreviousRequest,
    PlaybackState, StartPlaybackRequest, StopPlaybackRequest, UpdatePlaybackStateRequest,
    WatchPlaybackRequest, WatchPlaybackStateRequest,
};

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{roomId}/playback/start",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID")
        ),
        request_body = StartPlaybackRequest,
        responses(
            (status = 200, description = "Current playback state", body = PlaybackState),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn start_playback(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(req): Json<StartPlaybackRequest>,
) -> AppResult<Json<PlaybackState>> {
    let room_id = path.room_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomPlayback,
        move |client_api, authenticated| async move {
            client_api
                .start_playback(&authenticated.user_id(), &room_id, req)
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
        path = "/api/rooms/{roomId}/playback/stop",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID")
        ),
        request_body = StopPlaybackRequest,
        responses(
            (status = 200, description = "Current playback state", body = PlaybackState),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn stop_playback(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(req): Json<StopPlaybackRequest>,
) -> AppResult<Json<PlaybackState>> {
    let room_id = path.room_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomPlayback,
        move |client_api, authenticated| async move {
            client_api
                .stop_playback(&authenticated.user_id(), &room_id, req)
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
        path = "/api/rooms/{roomId}/playback/next",
        tag = "Room",
        params(("roomId" = String, Path, description = "Room ID")),
        request_body = PlayNextRequest,
        responses((status = 200, description = "Current playback state", body = PlaybackState)),
        security(("bearer_auth" = []))
    )
)]
pub async fn play_next(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(req): Json<PlayNextRequest>,
) -> AppResult<Json<PlaybackState>> {
    let room_id = path.room_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomPlayback,
        move |client_api, authenticated| async move {
            client_api
                .play_next(&authenticated.user_id(), &room_id, req)
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
        path = "/api/rooms/{roomId}/playback/previous",
        tag = "Room",
        params(("roomId" = String, Path, description = "Room ID")),
        request_body = PlayPreviousRequest,
        responses((status = 200, description = "Current playback state", body = PlaybackState)),
        security(("bearer_auth" = []))
    )
)]
pub async fn play_previous(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(req): Json<PlayPreviousRequest>,
) -> AppResult<Json<PlaybackState>> {
    let room_id = path.room_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomPlayback,
        move |client_api, authenticated| async move {
            client_api
                .play_previous(&authenticated.user_id(), &room_id, req)
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
        path = "/api/rooms/{roomId}/playback/history",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("beforeEntryId" = Option<String>, Query, description = "Legacy newest-first pagination cursor"),
            ("cursorEntryId" = Option<String>, Query, description = "Pagination cursor for the selected sort direction"),
            ("limit" = Option<i32>, Query, description = "Page size, up to 100"),
            ("sortDirection" = Option<i32>, Query, description = "Sort direction enum value; defaults to descending")
        ),
        responses((status = 200, description = "Playback history", body = ListPlaybackHistoryResponse)),
        security(("bearer_auth" = []))
    )
)]
pub async fn list_playback_history(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    ProtoQuery(req): ProtoQuery<ListPlaybackHistoryRequest>,
) -> AppResult<Json<ListPlaybackHistoryResponse>> {
    let room_id = path.room_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomPlayback,
        move |client_api, authenticated| async move {
            client_api
                .list_playback_history(&authenticated.user_id(), &room_id, req)
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
        path = "/api/rooms/{roomId}/playback/history/{entryId}/play",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("entryId" = String, Path, description = "Playback history entry ID")
        ),
        responses((status = 200, description = "Current playback state", body = PlaybackState)),
        security(("bearer_auth" = []))
    )
)]
pub async fn play_history_entry(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path((room_id, entry_id)): Path<(String, String)>,
) -> AppResult<Json<PlaybackState>> {
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomPlayback,
        move |client_api, authenticated| async move {
            client_api
                .play_history_entry(
                    &authenticated.user_id(),
                    &room_id,
                    PlayHistoryEntryRequest {
                        entry_id,
                        client_operation_id: None,
                    },
                )
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
        path = "/api/rooms/{roomId}/playback/history/{entryId}",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("entryId" = String, Path, description = "Playback history entry ID")
        ),
        responses((status = 200, description = "Playback history deletion result", body = DeletePlaybackHistoryEntryResponse)),
        security(("bearer_auth" = []))
    )
)]
pub async fn delete_playback_history_entry(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path((room_id, entry_id)): Path<(String, String)>,
) -> AppResult<Json<DeletePlaybackHistoryEntryResponse>> {
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomPlayback,
        move |client_api, authenticated| async move {
            client_api
                .delete_playback_history_entry(
                    &authenticated.user_id(),
                    &room_id,
                    DeletePlaybackHistoryEntryRequest { entry_id },
                )
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
        path = "/api/rooms/{roomId}/playback/history",
        tag = "Room",
        params(("roomId" = String, Path, description = "Room ID")),
        responses((status = 200, description = "Playback history clear result", body = ClearPlaybackHistoryResponse)),
        security(("bearer_auth" = []))
    )
)]
pub async fn clear_playback_history(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
) -> AppResult<Json<ClearPlaybackHistoryResponse>> {
    let room_id = path.room_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomPlayback,
        move |client_api, authenticated| async move {
            client_api
                .clear_playback_history(
                    &authenticated.user_id(),
                    &room_id,
                    ClearPlaybackHistoryRequest {},
                )
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
        path = "/api/rooms/{roomId}/playback",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            GetPlaybackQuery
        ),
        responses(
            (status = 200, description = "Current playback state", body = GetPlaybackResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Room not found", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_playback(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    ProtoQuery(query): ProtoQuery<GetPlaybackQuery>,
) -> AppResult<Json<GetPlaybackResponse>> {
    let room_id = path.room_id;
    let req = build_get_playback_request(&query)?;
    let response = execute_room_actor_endpoint_with_control(
        &state,
        request_meta,
        room_id,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomPlayback,
        move |client_api, request_control, actor| async move {
            client_api
                .get_playback_for_actor(&actor, req, Some(&request_control))
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

pub async fn watch_playback_state(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Query(query): Query<WatchPlaybackStateQuery>,
) -> AppResult<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>> {
    let room_id = path.room_id;
    let format = RealtimeTransportFormat::parse(query.format.as_deref())?;
    if query.event_sequence.is_some_and(|sequence| sequence < 0) {
        return Err(super::super::AppError::bad_request(
            "Invalid eventSequence; expected a non-negative integer",
        ));
    }
    let request = WatchPlaybackStateRequest {
        delivery_mode: parse_watch_delivery_mode(query.delivery_mode)?,
        playback_state: Some(synctv_proto::client::ObservePlaybackState {
            event_sequence: query.event_sequence,
        }),
    };
    let observe = synctv_api_common::impls::messaging::watch_playback_state_observe(request)
        .map_err(super::super::AppError::bad_request)?;
    open_resource_watch_sse(state, request_meta, room_id, observe, format).await
}

pub async fn watch_playback(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Query(query): Query<WatchPlaybackQuery>,
) -> AppResult<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>> {
    let room_id = path.room_id;
    let format = RealtimeTransportFormat::parse(query.format.as_deref())?;
    let playback_client_profile = build_playback_client_profile_from_watch_query(&query)?;
    let request = WatchPlaybackRequest {
        delivery_mode: parse_watch_delivery_mode(query.delivery_mode)?,
        playback: Some(synctv_proto::client::ObservePlayback {
            playback_client_profile,
        }),
    };
    let observe = synctv_api_common::impls::messaging::watch_playback_observe(request)
        .map_err(super::super::AppError::bad_request)?;
    open_resource_watch_sse(state, request_meta, room_id, observe, format).await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/rooms/{roomId}/playback",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID")
        ),
        request_body = UpdatePlaybackStateRequest,
        responses(
            (status = 200, description = "Playback state updated", body = PlaybackState),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn update_playback_state(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(req): Json<UpdatePlaybackStateRequest>,
) -> AppResult<Json<PlaybackState>> {
    let room_id = path.room_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomPlayback,
        move |client_api, authenticated| async move {
            client_api
                .update_playback_state(&authenticated.user_id(), &room_id, req)
                .await
        },
    )
    .await?;
    Ok(Json(response))
}
