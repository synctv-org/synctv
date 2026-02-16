//! User management HTTP handlers
//
// This layer now uses proto types and delegates to the impls layer for business logic

use axum::{
    extract::{Path, Query, State},
    Json,
};
use axum::http::HeaderMap;

use super::{middleware::AuthUser, AppResult, AppState};
use crate::proto::client::{
    LogoutResponse, GetProfileResponse, SetUsernameRequest,
    SetPasswordRequest, ListParticipatedRoomsResponse,
    DeleteRoomResponse,
    ListCreatedRoomsResponse,
};

/// Typed request for PATCH /api/user
#[derive(serde::Deserialize)]
pub struct UpdateUserRequest {
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub old_password: Option<String>,
}

/// Optional body for logout containing the refresh token.
#[derive(serde::Deserialize, Default)]
pub struct LogoutBody {
    #[serde(default)]
    pub refresh_token: Option<String>,
}

/// Logout user
///
/// Extracts the Bearer token from the Authorization header and an optional
/// refresh token from the JSON body. Both tokens are blacklisted server-side.
pub async fn logout(
    _auth: AuthUser,
    headers: HeaderMap,
    State(state): State<AppState>,
    body: Option<Json<LogoutBody>>,
) -> AppResult<Json<LogoutResponse>> {
    let access_token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer ").or_else(|| s.strip_prefix("bearer ")))
        .unwrap_or("");

    let refresh_token = body.and_then(|b| b.0.refresh_token);
    let response = state.client_api.logout(access_token, refresh_token.as_deref()).await;
    Ok(Json(response))
}

/// Get current user info
pub async fn get_me(
    auth: AuthUser,
    State(state): State<AppState>,
) -> AppResult<Json<GetProfileResponse>> {
    let response = state
        .client_api
        .get_profile(&auth.user_id.to_string())
        .await
        .map_err(super::error::impls_err_to_app_error)?;

    Ok(Json(response))
}

/// Update user (unified endpoint for username and password via PATCH)
pub async fn update_user(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<UpdateUserRequest>,
) -> AppResult<Json<serde_json::Value>> {
    // Check if username update is requested
    if let Some(ref username) = req.username {
        if username.is_empty() {
            return Err(super::AppError::bad_request("Username cannot be empty"));
        }

        let set_username_req = SetUsernameRequest {
            new_username: username.clone(),
        };

        let response = state
            .client_api
            .set_username(&auth.user_id.to_string(), set_username_req)
            .await
            .map_err(super::error::impls_err_to_app_error)?;

        let new_username = response.user.as_ref().map_or_else(|| username.clone(), |u| u.username.clone());

        return Ok(Json(serde_json::json!({
            "message": "Username updated successfully",
            "username": new_username
        })));
    }

    // Check if password update is requested
    if let Some(ref password) = req.password {
        // Old password is required to prevent unauthorized password changes
        // from stolen session tokens.
        let old_password = req.old_password
            .as_deref()
            .ok_or_else(|| super::AppError::bad_request(
                "old_password is required when changing password"
            ))?
            .to_string();

        let set_password_req = SetPasswordRequest {
            old_password,
            new_password: password.clone(),
        };

        let _response = state
            .client_api
            .set_password(&auth.user_id.to_string(), set_password_req)
            .await
            .map_err(super::error::impls_err_to_app_error)?;

        return Ok(Json(serde_json::json!({
            "message": "Password updated successfully"
        })));
    }

    Err(super::AppError::bad_request("No valid update fields provided (username or password)"))
}

/// Get user's joined rooms (paginated)
pub async fn get_joined_rooms(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> AppResult<Json<ListParticipatedRoomsResponse>> {
    let page = params.get("page").and_then(|v| v.parse().ok()).unwrap_or(1i32);
    let page_size = params.get("page_size").and_then(|v| v.parse().ok()).unwrap_or(20i32);

    let response = state
        .client_api
        .get_joined_rooms(&auth.user_id.to_string(), page, page_size)
        .await
        .map_err(super::error::impls_err_to_app_error)?;

    Ok(Json(response))
}

/// Delete a room (user's own room)
pub async fn delete_my_room(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> AppResult<Json<DeleteRoomResponse>> {
    // Ownership/permission check is performed inside client_api.delete_room()
    // via room_service.delete_room() -> check_permission(DELETE_ROOM)
    let response = state
        .client_api
        .delete_room(&auth.user_id.to_string(), &room_id)
        .await
        .map_err(super::error::impls_err_to_app_error)?;

    Ok(Json(response))
}

/// List rooms created by this user
/// GET /api/user/rooms/created
pub async fn list_created_rooms(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> AppResult<Json<ListCreatedRoomsResponse>> {
    let page = params.get("page").and_then(|v| v.parse().ok()).unwrap_or(1i32);
    let page_size = params.get("page_size").and_then(|v| v.parse().ok()).unwrap_or(10i32);

    let req = crate::proto::client::ListCreatedRoomsRequest { page, page_size };
    let response = state
        .client_api
        .list_created_rooms(&auth.user_id.to_string(), req)
        .await
        .map_err(super::error::impls_err_to_app_error)?;

    Ok(Json(response))
}
