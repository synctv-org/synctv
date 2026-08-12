use axum::{
    extract::{Path, State},
    response::Json,
    routing::{get, post},
    Router,
};

use crate::http::{error::map_api_error, middleware::RequestMetadata, AppResult, AppState};
use synctv_api_common::impls::EndpointRateLimitCategory;
use synctv_proto::providers::rtmp::{
    CreatePublishKeyRequest, CreatePublishKeyResponse, GetStreamInfoRequest, GetStreamInfoResponse,
};

use super::common::provider_request_metadata;

pub(crate) fn rtmp_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/rooms/{roomId}/publish-key/{mediaId}",
            post(generate_publish_key),
        )
        .route("/rooms/{roomId}/info/{mediaId}", get(handle_stream_info))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/rtmp/rooms/{roomId}/publish-key/{mediaId}",
        tag = "Provider",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("mediaId" = String, Path, description = "Media ID")
        ),
        responses(
            (status = 200, description = "Publish key generated", body = CreatePublishKeyResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Permission denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Room or media not found", body = crate::openapi::GoogleRpcStatusSchema)
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
    let request_meta = provider_request_metadata(request_meta);
    let client_api = state.shared_api_runtime.client_api.clone();
    let resp = state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Media,
            move |authenticated| async move {
                client_api
                    .create_publish_key(&authenticated.user_id(), req)
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
        path = "/api/providers/rtmp/rooms/{roomId}/info/{mediaId}",
        tag = "Provider",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("mediaId" = String, Path, description = "Media ID")
        ),
        responses(
            (status = 200, description = "Live stream information", body = GetStreamInfoResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Stream not found", body = crate::openapi::GoogleRpcStatusSchema)
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
    let request_meta = provider_request_metadata(request_meta);
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
                    .get_stream_info(
                        &authenticated.user_id(),
                        room_id.as_str(),
                        media_id.as_str(),
                    )
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;

    Ok(Json(resp))
}
