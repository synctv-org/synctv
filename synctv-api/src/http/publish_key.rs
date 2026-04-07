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

use crate::http::{middleware::AuthUser, AppResult, AppState, WithId};
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
#[axum::debug_handler]
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
    auth_user: AuthUser,
) -> AppResult<Json<CreatePublishKeyResponse>> {
    let user_id_str = auth_user.user_id.to_string();
    crate::impls::validate_proto_request(&path).map_err(crate::http::error::map_api_error)?;
    let RoomMediaTargetPathRequest { room_id, media_id } = path;

    // Delegate to shared ClientApiImpl (handles permission check, key generation, RTMP URL)
    let req = crate::proto::client::CreatePublishKeyRequest::default().with_id(media_id);
    let resp = state
        .client_api
        .create_publish_key(&user_id_str, &room_id, req)
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
