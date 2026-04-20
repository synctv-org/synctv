//! Publish key API endpoints
//!
//! HTTP endpoints for generating RTMP publish keys for live streaming.
//! Streaming is scoped to individual media items, not rooms.
//!
//! Uses proto-generated types for response to ensure type consistency
//! with gRPC handlers.

use axum::{
    extract::{Path, State},
    response::Json,
    routing::post,
    Router,
};
use synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT;

use crate::http::{middleware::RequestMetadata, AppResult, AppState, WithId};
use crate::impls::EndpointRateLimitCategory;
use crate::proto::client::{CreatePublishKeyResponse, RoomMediaTargetPathRequest};

/// Create publish key routes
pub fn create_publish_key_router() -> Router<AppState> {
    Router::new().route(
        "/api/rooms/{room_id}/movies/{media_id}/live/publish-key",
        post(generate_publish_key),
    )
}

/// Generate a publish key for RTMP streaming
///
/// POST /api/rooms/:room_id/movies/:media_id/live/publish-key
/// Requires authentication
///
/// Generates a JWT token for a specific media item.
/// Stream name format: {`room_id}/{media_id`}
///
/// Based on synctv-go implementation:
/// - Endpoint: POST /api/room/movie/:movieId/live/publishKey
/// - Multiple concurrent streams per room (one per media item)
/// - Each media item can have independent RTMP stream
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/movies/{media_id}/live/publish-key",
        tag = "Streaming",
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
pub async fn generate_publish_key(
    State(state): State<AppState>,
    Path(path): Path<RoomMediaTargetPathRequest>,
    request_meta: RequestMetadata,
) -> AppResult<Json<CreatePublishKeyResponse>> {
    crate::impls::validate_proto_request(&path).map_err(crate::http::error::map_api_error)?;
    let RoomMediaTargetPathRequest { room_id, media_id } = path;
    let request_meta = request_meta.0.with_timeout(Some(HTTP_REQUEST_TIMEOUT));
    let client_api = state.client_api.clone();

    // Delegate to shared ClientApiImpl (handles permission check, key generation, RTMP URL)
    let req = crate::proto::client::CreatePublishKeyRequest::default().with_id(media_id);
    let resp = state
        .client_api
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Media,
            move |authenticated| async move {
                client_api
                    .create_publish_key(authenticated.user_id.as_str(), &room_id, req)
                    .await
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;

    Ok(Json(resp))
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_room_media_target_path_request_deserializes_proto_field_names() {
        let req: crate::proto::client::RoomMediaTargetPathRequest =
            serde_json::from_str(r#"{"room_id":"AbC123xYz890","media_id":"ZyX098wVu765"}"#)
                .expect("deserialize path request");

        assert_eq!(req.room_id, "AbC123xYz890");
        assert_eq!(req.media_id, "ZyX098wVu765");
    }
}
