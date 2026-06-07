use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::sse::{Event, Sse},
    Json,
};
use std::convert::Infallible;

use super::execute::{execute_room_actor_endpoint_with_control, execute_user_endpoint};
use super::query::{
    build_get_playback_request, build_playback_client_profile_from_watch_query,
    parse_watch_delivery_mode, watch_after_event_sequence, GetPlaybackQuery, WatchPlaybackQuery,
    WatchQuery,
};
use super::watch::open_resource_watch_sse;
use crate::http::validation::ProtoQuery;
use crate::http::websocket::RealtimeTransportFormat;
use crate::http::{middleware::RequestMetadata, AppResult, AppState};
use crate::impls::{EndpointRateLimitCategory, EndpointRateLimitScope};
use synctv_proto::client::{
    GetPlaybackResponse, StartPlaybackRequest, StartPlaybackResponse, StopPlaybackRequest,
    StopPlaybackResponse, UpdatePlaybackRequest, WatchPlaybackRequest, WatchPlaybackStateRequest,
};

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
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse)
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
) -> AppResult<Json<StartPlaybackResponse>> {
    let room_id = path.room_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomPlayback,
        move |client_api, authenticated| async move {
            client_api
                .start_playback(&authenticated.user_id, &room_id, req)
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
        path = "/api/rooms/{room_id}/playback/stop",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = StopPlaybackRequest,
        responses(
            (status = 200, description = "Playback stopped", body = StopPlaybackResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse)
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
) -> AppResult<Json<StopPlaybackResponse>> {
    let room_id = path.room_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomPlayback,
        move |client_api, authenticated| async move {
            client_api
                .stop_playback(&authenticated.user_id, &room_id, req)
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
        path = "/api/rooms/{room_id}/playback",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            GetPlaybackQuery
        ),
        responses(
            (status = 200, description = "Current playback state", body = GetPlaybackResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Room not found", body = synctv_proto::client::ApiErrorResponse)
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
    headers: HeaderMap,
    Query(query): Query<WatchQuery>,
) -> AppResult<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>> {
    let room_id = path.room_id;
    let format = RealtimeTransportFormat::parse(query.format.as_deref())?;
    let after_event_sequence = watch_after_event_sequence(&headers, query.after_event_sequence)?;
    let request = WatchPlaybackStateRequest {
        delivery_mode: parse_watch_delivery_mode(query.delivery_mode.as_deref())?,
        playback_state: Some(synctv_proto::client::ObservePlaybackState {
            after_event_sequence,
        }),
    };
    let observe = crate::impls::messaging::watch_playback_state_observe(request)
        .map_err(super::super::AppError::bad_request)?;
    open_resource_watch_sse(state, request_meta, room_id, observe, format).await
}

pub async fn watch_playback(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    headers: HeaderMap,
    Query(query): Query<WatchPlaybackQuery>,
) -> AppResult<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>> {
    let room_id = path.room_id;
    let format = RealtimeTransportFormat::parse(query.format.as_deref())?;
    let _ = headers;
    let playback_client_profile = build_playback_client_profile_from_watch_query(&query)?;
    let request = WatchPlaybackRequest {
        delivery_mode: parse_watch_delivery_mode(query.delivery_mode.as_deref())?,
        playback: Some(synctv_proto::client::ObservePlayback {
            playback_client_profile,
        }),
    };
    let observe = crate::impls::messaging::watch_playback_observe(request)
        .map_err(super::super::AppError::bad_request)?;
    open_resource_watch_sse(state, request_meta, room_id, observe, format).await
}

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
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn update_playback(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(req): Json<UpdatePlaybackRequest>,
) -> AppResult<Json<GetPlaybackResponse>> {
    let room_id = path.room_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomPlayback,
        move |client_api, authenticated| async move {
            client_api
                .update_playback(&authenticated.user_id, &room_id, req)
                .await
        },
    )
    .await?;
    Ok(Json(response))
}
