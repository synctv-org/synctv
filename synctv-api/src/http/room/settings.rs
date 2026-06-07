use axum::{
    extract::{Path, State},
    Json,
};
use synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT;

use super::execute::{execute_room_actor_endpoint, execute_user_endpoint};
use crate::http::{middleware::RequestMetadata, AppResult, AppState};
use crate::impls::{EndpointRateLimitCategory, EndpointRateLimitScope};
use synctv_proto::client::{
    ClearRoomPasswordRequest, FinishRoomPasswordLoginRequest,
    FinishRoomPasswordRegistrationRequest, JoinRoomResponse, ResetRoomSettingsResponse,
    SetRoomPasswordResponse, StartRoomPasswordLoginRequest, StartRoomPasswordLoginResponse,
    StartRoomPasswordRegistrationRequest, StartRoomPasswordRegistrationResponse,
    TransferRoomOwnershipRequest, TransferRoomOwnershipResponse, UpdateRoomSettingsResponse,
};

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/password/opaque/login/start",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = StartRoomPasswordLoginRequest,
        responses(
            (status = 200, description = "Room password login challenge created", body = StartRoomPasswordLoginResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Permission denied", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn start_room_password_login(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(mut req): Json<StartRoomPasswordLoginRequest>,
) -> AppResult<Json<StartRoomPasswordLoginResponse>> {
    let request_meta = request_meta.0.with_timeout(Some(HTTP_REQUEST_TIMEOUT));
    req.room_id = path.room_id;
    let client_ip = request_meta.client_ip.map(|ip| ip.to_string());
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_scoped_user_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Write,
            EndpointRateLimitScope::RoomJoin,
            move |request_control, authenticated| async move {
                client_api
                    .start_room_password_login_with_control(
                        &authenticated.user_id,
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

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/password/opaque/login/finish",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = FinishRoomPasswordLoginRequest,
        responses(
            (status = 200, description = "Joined room", body = JoinRoomResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Permission denied", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn finish_room_password_login(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(req): Json<FinishRoomPasswordLoginRequest>,
) -> AppResult<Json<JoinRoomResponse>> {
    let request_meta = request_meta.0.with_timeout(Some(HTTP_REQUEST_TIMEOUT));
    let room_id = path.room_id;
    let client_ip = request_meta.client_ip.map(|ip| ip.to_string());
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_scoped_user_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Write,
            EndpointRateLimitScope::RoomJoin,
            move |_request_control, authenticated| async move {
                client_api
                    .finish_room_password_login_with_control(
                        &authenticated.user_id,
                        Some(&room_id),
                        req,
                        client_ip.as_deref(),
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
        patch,
        path = "/api/rooms/{room_id}/password/opaque/registration/start",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = StartRoomPasswordRegistrationRequest,
        responses(
            (status = 200, description = "Room password registration challenge created", body = StartRoomPasswordRegistrationResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn start_room_password_registration(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(req): Json<StartRoomPasswordRegistrationRequest>,
) -> AppResult<Json<StartRoomPasswordRegistrationResponse>> {
    let room_id = path.room_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomPassword,
        move |client_api, authenticated| async move {
            client_api
                .start_room_password_registration(&authenticated.user_id, &room_id, req)
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
        path = "/api/rooms/{room_id}/password/opaque/registration/finish",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = FinishRoomPasswordRegistrationRequest,
        responses(
            (status = 200, description = "Room password updated", body = SetRoomPasswordResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn finish_room_password_registration(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(req): Json<FinishRoomPasswordRegistrationRequest>,
) -> AppResult<Json<SetRoomPasswordResponse>> {
    let room_id = path.room_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomPassword,
        move |client_api, authenticated| async move {
            client_api
                .finish_room_password_registration(&authenticated.user_id, &room_id, req)
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
        path = "/api/rooms/{room_id}/password",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "Room password cleared", body = SetRoomPasswordResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn clear_room_password(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
) -> AppResult<Json<SetRoomPasswordResponse>> {
    let room_id = path.room_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomPassword,
        move |client_api, authenticated| async move {
            client_api
                .clear_room_password(
                    &authenticated.user_id,
                    &room_id,
                    ClearRoomPasswordRequest {},
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
        path = "/api/rooms/{room_id}/settings",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "Room settings", body = synctv_proto::client::GetRoomSettingsResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Room not found", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_room_settings(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
) -> AppResult<Json<synctv_proto::client::GetRoomSettingsResponse>> {
    let room_id = path.room_id;
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomSettings,
        move |client_api, actor| async move { client_api.get_room_settings_for_actor(&actor).await },
    )
    .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/rooms/{room_id}/settings",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = synctv_proto::http_serde::ClientUpdateRoomSettingsRequestDef,
        responses(
            (status = 200, description = "Room settings updated", body = UpdateRoomSettingsResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn update_room_settings(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(req): Json<synctv_proto::http_serde::ClientUpdateRoomSettingsRequestDef>,
) -> AppResult<Json<UpdateRoomSettingsResponse>> {
    let room_id = path.room_id;
    let req = synctv_proto::client::UpdateRoomSettingsRequest::from(req);
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomSettings,
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
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Permission denied", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn transfer_room_ownership(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(req): Json<TransferRoomOwnershipRequest>,
) -> AppResult<Json<TransferRoomOwnershipResponse>> {
    let room_id = path.room_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomSettings,
        move |client_api, authenticated| async move {
            client_api
                .transfer_room_ownership(&authenticated.user_id, &room_id, req)
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
        path = "/api/rooms/{room_id}/settings/reset",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "Room settings reset", body = ResetRoomSettingsResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn reset_room_settings(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
) -> AppResult<Json<ResetRoomSettingsResponse>> {
    let room_id = path.room_id;
    let response = execute_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomSettings,
        move |client_api, authenticated| async move {
            client_api
                .reset_room_settings(&authenticated.user_id, &room_id)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}
