//! Room member management API endpoints (room-scoped, requires room-level permissions)

use axum::{
    extract::{Path, State},
    Json,
};

use crate::http::validation::ProtoQuery;
use crate::http::{middleware::RequestMetadata, AppResult, AppState};
use crate::impls::EndpointRateLimitCategory;

/// Add a member to a room.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{roomId}/members",
        tag = "Room Member",
        params(
            ("roomId" = String, Path, description = "Room ID")
        ),
        request_body = synctv_proto::client::AddMemberRequest,
        responses(
            (status = 200, description = "Member added", body = synctv_proto::client::AddMemberResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Permission denied", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn add_member(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(req): Json<synctv_proto::client::AddMemberRequest>,
) -> AppResult<Json<synctv_proto::client::AddMemberResponse>> {
    let room_id = path.room_id;
    let request_meta = request_meta.0;
    let client_api = state.shared_api_runtime.client_api.clone();
    let resp = state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            move |authenticated| async move {
                client_api
                    .add_member(&authenticated.user_id, &room_id, req)
                    .await
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;
    Ok(Json(resp))
}

/// List room join reviews.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{roomId}/reviews/joins",
        tag = "Room Member",
        params(("roomId" = String, Path, description = "Room ID"), synctv_proto::client::ListRoomJoinReviewsRequest),
        responses(
            (status = 200, description = "Room join reviews", body = synctv_proto::client::ListRoomJoinReviewsResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(("bearer_auth" = []))
    )
)]
pub async fn list_room_join_reviews(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    ProtoQuery(req): ProtoQuery<synctv_proto::client::ListRoomJoinReviewsRequest>,
) -> AppResult<Json<synctv_proto::client::ListRoomJoinReviewsResponse>> {
    let synctv_proto::client::RoomPathRequest { room_id } = path;
    let request_meta = request_meta.0;
    let client_api = state.shared_api_runtime.client_api.clone();
    let resp = state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Read,
            move |authenticated| async move {
                client_api
                    .list_room_join_reviews(&authenticated.user_id, &room_id, req)
                    .await
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;
    Ok(Json(resp))
}

/// Approve a room join review.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{roomId}/reviews/joins/{requestId}/approve",
        tag = "Room Member",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("requestId" = String, Path, description = "Review request ID")
        ),
        responses(
            (status = 200, description = "Room join review approved", body = synctv_proto::client::ApproveRoomJoinReviewResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Permission denied", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Review not found", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(("bearer_auth" = []))
    )
)]
pub async fn approve_room_join_review(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomJoinReviewPathRequest>,
) -> AppResult<Json<synctv_proto::client::ApproveRoomJoinReviewResponse>> {
    let synctv_proto::client::RoomJoinReviewPathRequest {
        room_id,
        request_id,
    } = path;
    let req = synctv_proto::client::ApproveRoomJoinReviewRequest { request_id };
    let request_meta = request_meta.0;
    let client_api = state.shared_api_runtime.client_api.clone();
    let resp = state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            move |authenticated| async move {
                client_api
                    .approve_room_join_review(&authenticated.user_id, &room_id, req)
                    .await
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;
    Ok(Json(resp))
}

/// Reject a room join review.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{roomId}/reviews/joins/{requestId}/reject",
        tag = "Room Member",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("requestId" = String, Path, description = "Review request ID")
        ),
        request_body = synctv_proto::client::RejectRoomJoinReviewRequest,
        responses(
            (status = 200, description = "Room join review rejected", body = synctv_proto::client::RejectRoomJoinReviewResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Permission denied", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Review not found", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(("bearer_auth" = []))
    )
)]
pub async fn reject_room_join_review(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomJoinReviewPathRequest>,
    Json(mut req): Json<synctv_proto::client::RejectRoomJoinReviewRequest>,
) -> AppResult<Json<synctv_proto::client::RejectRoomJoinReviewResponse>> {
    let synctv_proto::client::RoomJoinReviewPathRequest {
        room_id,
        request_id,
    } = path;
    req.request_id = request_id;
    let request_meta = request_meta.0;
    let client_api = state.shared_api_runtime.client_api.clone();
    let resp = state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            move |authenticated| async move {
                client_api
                    .reject_room_join_review(&authenticated.user_id, &room_id, req)
                    .await
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;
    Ok(Json(resp))
}

/// Kick a member from a room
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/rooms/{roomId}/members/{userId}",
        tag = "Room Member",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("userId" = String, Path, description = "Target user ID")
        ),
        request_body = synctv_proto::client::KickMemberRequest,
        responses(
            (status = 200, description = "Member kicked", body = synctv_proto::client::KickMemberResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Permission denied", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Member not found", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn kick_member(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomMemberTargetPathRequest>,
    Json(mut req): Json<synctv_proto::client::KickMemberRequest>,
) -> AppResult<Json<synctv_proto::client::KickMemberResponse>> {
    let synctv_proto::client::RoomMemberTargetPathRequest { room_id, user_id } = path;
    req.user_id = user_id;
    let request_meta = request_meta.0;
    let client_api = state.shared_api_runtime.client_api.clone();
    let resp = state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            move |authenticated| async move {
                client_api
                    .kick_member(&authenticated.user_id, &room_id, req)
                    .await
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/rooms/{roomId}/members/{userId}",
        tag = "Room Member",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("userId" = String, Path, description = "Target user ID")
        ),
        request_body = synctv_proto::client::UpdateMemberPermissionsRequest,
        responses(
            (status = 200, description = "Member permissions updated", body = synctv_proto::client::UpdateMemberPermissionsResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Permission denied", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn set_member_permissions(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomMemberTargetPathRequest>,
    Json(mut req): Json<synctv_proto::client::UpdateMemberPermissionsRequest>,
) -> AppResult<Json<synctv_proto::client::UpdateMemberPermissionsResponse>> {
    let synctv_proto::client::RoomMemberTargetPathRequest { room_id, user_id } = path;
    req.user_id = user_id;
    let request_meta = request_meta.0;
    let client_api = state.shared_api_runtime.client_api.clone();
    let resp = state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            move |authenticated| async move {
                client_api
                    .update_member_permissions(&authenticated.user_id, &room_id, req)
                    .await
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;
    Ok(Json(resp))
}

#[cfg(test)]
mod tests {
    type TestResult<T = ()> = anyhow::Result<T>;

    #[test]
    fn test_room_member_target_path_request_deserializes_proto_field_names() -> TestResult {
        let req: synctv_proto::client::RoomMemberTargetPathRequest =
            serde_json::from_str(r#"{"roomId":"room_1","userId":"usr_1"}"#)?;

        assert_eq!(req.room_id, "room_1");
        assert_eq!(req.user_id, "usr_1");
        Ok(())
    }

    #[test]
    fn test_reject_room_join_review_request_overrides_path_request_id() -> TestResult {
        let mut req: synctv_proto::client::RejectRoomJoinReviewRequest =
            serde_json::from_str(r#"{"requestId":"rev_body","reason":"denied"}"#)?;
        req.request_id = "rev_1".to_string();
        assert_eq!(req.request_id, "rev_1");
        assert_eq!(req.reason, "denied");
        Ok(())
    }

    #[test]
    fn test_update_member_permissions_request_overrides_path_user_id() -> TestResult {
        let mut req: synctv_proto::client::UpdateMemberPermissionsRequest =
            serde_json::from_str(r#"{"userId":"usr_body","role":2,"addedPermissions":1}"#)?;
        req.user_id = "usr_1".to_string();
        assert_eq!(req.user_id, "usr_1");
        assert_eq!(req.role, 2);
        assert_eq!(req.added_permissions, 1);
        Ok(())
    }

    #[test]
    fn test_reject_room_join_review_request_rejects_oversized_reason() {
        let req = synctv_proto::client::RejectRoomJoinReviewRequest {
            request_id: "rev_1".to_string(),
            reason: "x".repeat(501),
        };

        assert!(synctv_proto::validate(&req).is_err());
    }
}
