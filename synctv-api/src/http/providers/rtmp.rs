use axum::{
    extract::{Path, State},
    response::Json,
    routing::{get, post},
    Router,
};
use synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT;

use crate::http::{error::map_api_error, middleware::RequestMetadata, AppResult, AppState};
use crate::impls::EndpointRateLimitCategory;
use crate::proto::providers::rtmp::{
    CreatePublishKeyRequest, CreatePublishKeyResponse, GetStreamInfoRequest, GetStreamInfoResponse,
};

pub fn rtmp_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/rooms/{room_id}/publish-key/{media_id}",
            post(generate_publish_key),
        )
        .route("/rooms/{room_id}/info/{media_id}", get(handle_stream_info))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/rtmp/rooms/{room_id}/publish-key/{media_id}",
        tag = "Provider",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("media_id" = String, Path, description = "Media ID")
        ),
        responses(
            (status = 200, description = "Publish key generated", body = CreatePublishKeyResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Permission denied", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Room or media not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn generate_publish_key(
    State(state): State<AppState>,
    Path(req): Path<CreatePublishKeyRequest>,
    request_meta: RequestMetadata,
) -> AppResult<Json<CreatePublishKeyResponse>> {
    let request_meta = request_meta.0.with_timeout(Some(HTTP_REQUEST_TIMEOUT));
    let client_api = state.shared_api_runtime.client_api.clone();
    let resp = state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Media,
            move |authenticated| async move {
                client_api
                    .create_publish_key(&authenticated.user_id, req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;

    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/providers/rtmp/rooms/{room_id}/info/{media_id}",
        tag = "Provider",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("media_id" = String, Path, description = "Media ID")
        ),
        responses(
            (status = 200, description = "Live stream information", body = GetStreamInfoResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Stream not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn handle_stream_info(
    request_meta: RequestMetadata,
    Path(req): Path<GetStreamInfoRequest>,
    State(state): State<AppState>,
) -> AppResult<Json<GetStreamInfoResponse>> {
    let request_meta = request_meta.0.with_timeout(Some(HTTP_REQUEST_TIMEOUT));
    let client_api = state.shared_api_runtime.client_api.clone();
    let room_id = req.room_id;
    let media_id = req.media_id;
    let resp = state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Read,
            move |authenticated| async move {
                client_api
                    .get_stream_info(&authenticated.user_id, room_id.as_str(), media_id.as_str())
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;

    Ok(Json(resp))
}
