//! Room member management API endpoints (room-scoped, requires room-level permissions)

use axum::{
    extract::{Path, State},
    Json,
};
use serde::Deserialize;

use crate::http::{AppState, AppResult, middleware::AuthUser};

/// Kick a member from a room
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

/// Set member permissions / role
#[derive(Debug, Deserialize)]
pub struct SetMemberPermissionsRequest {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub added_permissions: u64,
    #[serde(default)]
    pub removed_permissions: u64,
    #[serde(default)]
    pub admin_added_permissions: u64,
    #[serde(default)]
    pub admin_removed_permissions: u64,
}

pub async fn set_member_permissions(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((room_id, target_user_id)): Path<(String, String)>,
    Json(req): Json<SetMemberPermissionsRequest>,
) -> AppResult<Json<crate::proto::client::UpdateMemberPermissionsResponse>> {
    let resp = state
        .client_api
        .update_member_permissions(
            auth.user_id.as_str(),
            &room_id,
            crate::proto::client::UpdateMemberPermissionsRequest {
                user_id: target_user_id,
                role: match req.role.as_str() {
                    "creator" => synctv_proto::common::RoomMemberRole::Creator as i32,
                    "admin" => synctv_proto::common::RoomMemberRole::Admin as i32,
                    "member" => synctv_proto::common::RoomMemberRole::Member as i32,
                    "guest" => synctv_proto::common::RoomMemberRole::Guest as i32,
                    _ => synctv_proto::common::RoomMemberRole::Unspecified as i32,
                },
                added_permissions: req.added_permissions,
                removed_permissions: req.removed_permissions,
                admin_added_permissions: req.admin_added_permissions,
                admin_removed_permissions: req.admin_removed_permissions,
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;
    Ok(Json(resp))
}

/// Ban a member from a room
/// POST /`api/rooms/:room_id/bans` with body: {`user_id`, reason}
pub async fn ban_member(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<crate::proto::client::BanMemberRequest>,
) -> AppResult<Json<crate::proto::client::BanMemberResponse>> {
    let resp = state
        .client_api
        .ban_member(auth.user_id.as_str(), &room_id, req)
        .await
        .map_err(crate::http::error::map_api_error)?;

    Ok(Json(resp))
}

/// Unban a member from a room
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
