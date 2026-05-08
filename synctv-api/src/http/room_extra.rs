//! Room member management API endpoints (room-scoped, requires room-level permissions)

use axum::{
    extract::{Path, State},
    Json,
};
use synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT;

use crate::http::{
    middleware::RequestMetadata, validation::ValidatedJson, AppResult, AppState, WithUserId,
};
use crate::impls::EndpointRateLimitCategory;

fn request_metadata(request_meta: RequestMetadata) -> crate::impls::RequestMetadata {
    request_meta.0.with_timeout(Some(HTTP_REQUEST_TIMEOUT))
}

pub type AddMemberBody = crate::proto::client::AddMemberRequest;
pub type BanMemberBody = crate::proto::client::BanMemberRequest;

#[derive(Debug, Default, serde::Deserialize, garde::Validate)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RejectRoomJoinReviewBody {
    #[serde(default)]
    #[garde(length(max = 500))]
    pub reason: String,
}

/// Add a member to a room.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/members",
        tag = "Room Member",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = crate::proto::client::AddMemberRequest,
        responses(
            (status = 200, description = "Member added", body = crate::proto::client::AddMemberResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Permission denied", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn add_member(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<AddMemberBody>,
) -> AppResult<Json<crate::proto::client::AddMemberResponse>> {
    crate::impls::validate_proto_request(&path).map_err(crate::http::error::map_api_error)?;
    let room_id = path.room_id;
    let request_meta = request_metadata(request_meta);
    let client_api = state.client_api.clone();
    let resp = state
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
        path = "/api/rooms/{room_id}/reviews/joins",
        tag = "Room Member",
        params(("room_id" = String, Path, description = "Room ID"), crate::proto::client::ListRoomJoinReviewsRequest),
        responses(
            (status = 200, description = "Room join reviews", body = crate::proto::client::ListRoomJoinReviewsResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub async fn list_room_join_reviews(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    crate::http::validation::ValidatedQuery(req): crate::http::validation::ValidatedQuery<
        crate::proto::client::ListRoomJoinReviewsRequest,
    >,
) -> AppResult<Json<crate::proto::client::ListRoomJoinReviewsResponse>> {
    crate::impls::validate_proto_request(&path).map_err(crate::http::error::map_api_error)?;
    let crate::proto::client::RoomPathRequest { room_id } = path;
    let request_meta = request_metadata(request_meta);
    let client_api = state.client_api.clone();
    let resp = state
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
        path = "/api/rooms/{room_id}/reviews/joins/{request_id}/approve",
        tag = "Room Member",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("request_id" = String, Path, description = "Review request ID")
        ),
        responses(
            (status = 200, description = "Room join review approved", body = crate::proto::client::ApproveRoomJoinReviewResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Permission denied", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Review not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub async fn approve_room_join_review(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomJoinReviewPathRequest>,
) -> AppResult<Json<crate::proto::client::ApproveRoomJoinReviewResponse>> {
    crate::impls::validate_proto_request(&path).map_err(crate::http::error::map_api_error)?;
    let crate::proto::client::RoomJoinReviewPathRequest {
        room_id,
        request_id,
    } = path;
    let req = crate::proto::client::ApproveRoomJoinReviewRequest { request_id };
    let request_meta = request_metadata(request_meta);
    let client_api = state.client_api.clone();
    let resp = state
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
        path = "/api/rooms/{room_id}/reviews/joins/{request_id}/reject",
        tag = "Room Member",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("request_id" = String, Path, description = "Review request ID")
        ),
        request_body = RejectRoomJoinReviewBody,
        responses(
            (status = 200, description = "Room join review rejected", body = crate::proto::client::RejectRoomJoinReviewResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Permission denied", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Review not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub async fn reject_room_join_review(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomJoinReviewPathRequest>,
    ValidatedJson(req): ValidatedJson<RejectRoomJoinReviewBody>,
) -> AppResult<Json<crate::proto::client::RejectRoomJoinReviewResponse>> {
    crate::impls::validate_proto_request(&path).map_err(crate::http::error::map_api_error)?;
    let crate::proto::client::RoomJoinReviewPathRequest {
        room_id,
        request_id,
    } = path;
    let req = crate::proto::client::RejectRoomJoinReviewRequest {
        request_id,
        reason: req.reason,
    };
    let request_meta = request_metadata(request_meta);
    let client_api = state.client_api.clone();
    let resp = state
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
        path = "/api/rooms/{room_id}/members/{user_id}",
        tag = "Room Member",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("user_id" = String, Path, description = "Target user ID")
        ),
        responses(
            (status = 200, description = "Member kicked", body = crate::proto::client::KickMemberResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Permission denied", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Member not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn kick_member(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomMemberTargetPathRequest>,
) -> AppResult<Json<crate::proto::client::KickMemberResponse>> {
    crate::impls::validate_proto_request(&path).map_err(crate::http::error::map_api_error)?;
    let crate::proto::client::RoomMemberTargetPathRequest { room_id, user_id } = path;
    let req = crate::proto::client::KickMemberRequest::default().with_user_id(user_id);
    let request_meta = request_metadata(request_meta);
    let client_api = state.client_api.clone();
    let resp = state
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
        path = "/api/rooms/{room_id}/members/{user_id}",
        tag = "Room Member",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("user_id" = String, Path, description = "Target user ID")
        ),
        request_body = crate::proto::client::UpdateMemberPermissionsRequest,
        responses(
            (status = 200, description = "Member permissions updated", body = crate::proto::client::UpdateMemberPermissionsResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Permission denied", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn set_member_permissions(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomMemberTargetPathRequest>,
    Json(req): Json<crate::proto::client::UpdateMemberPermissionsRequest>,
) -> AppResult<Json<crate::proto::client::UpdateMemberPermissionsResponse>> {
    crate::impls::validate_proto_request(&path).map_err(crate::http::error::map_api_error)?;
    let crate::proto::client::RoomMemberTargetPathRequest { room_id, user_id } = path;
    let req = req.with_user_id(user_id);
    let request_meta = request_metadata(request_meta);
    let client_api = state.client_api.clone();
    let resp = state
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

/// Ban a member from a room
/// POST /`api/rooms/:room_id/bans` with body: {`user_id`, reason}
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/bans",
        tag = "Room Member",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = crate::proto::client::BanMemberRequest,
        responses(
            (status = 200, description = "Member banned", body = crate::proto::client::BanMemberResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Permission denied", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn ban_member(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Json(req): Json<BanMemberBody>,
) -> AppResult<Json<crate::proto::client::BanMemberResponse>> {
    crate::impls::validate_proto_request(&path).map_err(crate::http::error::map_api_error)?;
    let room_id = path.room_id;
    let request_meta = request_metadata(request_meta);
    let client_api = state.client_api.clone();
    let resp = state
        .client_api
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            move |authenticated| async move {
                client_api
                    .ban_member(&authenticated.user_id, &room_id, req)
                    .await
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;

    Ok(Json(resp))
}

/// Unban a member from a room
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/rooms/{room_id}/bans/{user_id}",
        tag = "Room Member",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("user_id" = String, Path, description = "Target user ID")
        ),
        responses(
            (status = 200, description = "Member unbanned", body = crate::proto::client::UnbanMemberResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Permission denied", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn unban_member(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomMemberTargetPathRequest>,
) -> AppResult<Json<crate::proto::client::UnbanMemberResponse>> {
    crate::impls::validate_proto_request(&path).map_err(crate::http::error::map_api_error)?;
    let crate::proto::client::RoomMemberTargetPathRequest { room_id, user_id } = path;
    let req = crate::proto::client::UnbanMemberRequest::default().with_user_id(user_id);
    let request_meta = request_metadata(request_meta);
    let client_api = state.client_api.clone();
    let resp = state
        .client_api
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            move |authenticated| async move {
                client_api
                    .unban_member(&authenticated.user_id, &room_id, req)
                    .await
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;

    Ok(Json(resp))
}

#[cfg(test)]
mod tests {
    use super::RejectRoomJoinReviewBody;
    use garde::Validate;

    #[test]
    fn test_room_member_target_path_request_deserializes_proto_field_names() {
        let req: crate::proto::client::RoomMemberTargetPathRequest =
            serde_json::from_str(r#"{"room_id":"room_1","user_id":"usr_1"}"#)
                .expect("deserialize path request");

        assert_eq!(req.room_id, "room_1");
        assert_eq!(req.user_id, "usr_1");
    }

    #[test]
    fn test_reject_room_join_review_body_deserializes_reason_field() {
        let req: RejectRoomJoinReviewBody = serde_json::from_str(r#"{"reason":"denied"}"#)
            .expect("deserialize reject room join review body");

        assert_eq!(req.reason, "denied");
    }

    #[test]
    fn test_reject_room_join_review_body_rejects_oversized_reason() {
        let req = RejectRoomJoinReviewBody {
            reason: "x".repeat(501),
        };

        assert!(req.validate().is_err());
    }
}
