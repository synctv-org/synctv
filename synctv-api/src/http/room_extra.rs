//! Room member management API endpoints (room-scoped, requires room-level permissions)

use axum::{
    extract::{Path, State},
    Json,
};

use crate::http::{middleware::AuthUser, AppResult, AppState};

pub type BanMemberBody = crate::proto::client::BanMemberRequest;

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
    auth: AuthUser,
    State(state): State<AppState>,
    Path((room_id, target_user_id)): Path<(String, String)>,
) -> AppResult<Json<crate::proto::client::KickMemberResponse>> {
    let resp = state
        .client_api
        .kick_member(
            auth.user_id.as_str(),
            &room_id,
            crate::proto::client::KickMemberRequest {
                user_id: target_user_id,
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
    auth: AuthUser,
    State(state): State<AppState>,
    Path((room_id, target_user_id)): Path<(String, String)>,
    Json(mut req): Json<crate::proto::client::UpdateMemberPermissionsRequest>,
) -> AppResult<Json<crate::proto::client::UpdateMemberPermissionsResponse>> {
    req.user_id = target_user_id;
    let resp = state
        .client_api
        .update_member_permissions(auth.user_id.as_str(), &room_id, req)
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
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<BanMemberBody>,
) -> AppResult<Json<crate::proto::client::BanMemberResponse>> {
    let resp = state
        .client_api
        .ban_member(auth.user_id.as_str(), &room_id, req)
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
    auth: AuthUser,
    State(state): State<AppState>,
    Path((room_id, target_user_id)): Path<(String, String)>,
) -> AppResult<Json<crate::proto::client::UnbanMemberResponse>> {
    let resp = state
        .client_api
        .unban_member(
            auth.user_id.as_str(),
            &room_id,
            crate::proto::client::UnbanMemberRequest {
                user_id: target_user_id,
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;

    Ok(Json(resp))
}
