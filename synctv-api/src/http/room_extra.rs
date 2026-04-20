//! Room member management API endpoints (room-scoped, requires room-level permissions)

use axum::{
    extract::{Path, State},
    Json,
};
use synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT;

use crate::http::{middleware::RequestMetadata, AppResult, AppState, WithUserId};
use crate::impls::EndpointRateLimitCategory;

fn request_metadata(request_meta: RequestMetadata) -> crate::impls::RequestMetadata {
    request_meta.0.with_timeout(Some(HTTP_REQUEST_TIMEOUT))
}

pub type AddMemberBody = crate::proto::client::AddMemberRequest;
pub type RejectMemberBody = crate::proto::client::RejectMemberRequest;
pub type BanMemberBody = crate::proto::client::BanMemberRequest;

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
                    .add_member(authenticated.user_id.as_str(), &room_id, req)
                    .await
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;
    Ok(Json(resp))
}

/// Approve a pending member.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/members/{user_id}/approve",
        tag = "Room Member",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("user_id" = String, Path, description = "Target user ID")
        ),
        responses(
            (status = 200, description = "Member approved", body = crate::proto::client::ApproveMemberResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Permission denied", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Pending member not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn approve_member(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomMemberTargetPathRequest>,
) -> AppResult<Json<crate::proto::client::ApproveMemberResponse>> {
    crate::impls::validate_proto_request(&path).map_err(crate::http::error::map_api_error)?;
    let crate::proto::client::RoomMemberTargetPathRequest { room_id, user_id } = path;
    let req = crate::proto::client::ApproveMemberRequest::default().with_user_id(user_id);
    let request_meta = request_metadata(request_meta);
    let client_api = state.client_api.clone();
    let resp = state
        .client_api
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            move |authenticated| async move {
                client_api
                    .approve_member(authenticated.user_id.as_str(), &room_id, req)
                    .await
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;
    Ok(Json(resp))
}

/// Reject a pending member.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{room_id}/members/{user_id}/reject",
        tag = "Room Member",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("user_id" = String, Path, description = "Target user ID")
        ),
        request_body = crate::proto::client::RejectMemberRequest,
        responses(
            (status = 200, description = "Member rejected", body = crate::proto::client::RejectMemberResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Permission denied", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Pending member not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn reject_member(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<crate::proto::client::RoomMemberTargetPathRequest>,
    Json(req): Json<RejectMemberBody>,
) -> AppResult<Json<crate::proto::client::RejectMemberResponse>> {
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
                    .reject_member(authenticated.user_id.as_str(), &room_id, req)
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
                    .kick_member(authenticated.user_id.as_str(), &room_id, req)
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
                    .update_member_permissions(authenticated.user_id.as_str(), &room_id, req)
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
                    .ban_member(authenticated.user_id.as_str(), &room_id, req)
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
                    .unban_member(authenticated.user_id.as_str(), &room_id, req)
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
    fn test_room_member_target_path_request_deserializes_proto_field_names() {
        let req: crate::proto::client::RoomMemberTargetPathRequest =
            serde_json::from_str(r#"{"room_id":"AbC123xYz890","user_id":"ZyX098wVu765"}"#)
                .expect("deserialize path request");

        assert_eq!(req.room_id, "AbC123xYz890");
        assert_eq!(req.user_id, "ZyX098wVu765");
    }

    #[test]
    fn test_reject_member_request_deserializes_reason_field() {
        let req: crate::proto::client::RejectMemberRequest =
            serde_json::from_str(r#"{"user_id":"AbC123xYz890","reason":"denied"}"#)
                .expect("deserialize reject request");

        assert_eq!(req.user_id, "AbC123xYz890");
        assert_eq!(req.reason, "denied");
    }
}
