//! User management HTTP handlers
//
// This layer now uses proto types and delegates to the impls layer for business logic

use axum::{
    extract::{Path, Query, State},
    Json,
};

use super::{middleware::AuthUser, AppResult, AppState};
use crate::proto::client::{
    GetProfileResponse, SetUsernameRequest,
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

/// Get current user info
pub async fn get_me(
    auth: AuthUser,
    State(state): State<AppState>,
) -> AppResult<Json<GetProfileResponse>> {
    let response = state
        .client_api
        .get_profile(&auth.user_id.to_string())
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Update user (unified endpoint for username and password via PATCH)
pub async fn update_user(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<UpdateUserRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let mut updated_fields = Vec::new();
    let mut result = serde_json::Map::new();

    // Process username update if requested
    if let Some(ref username) = req.username {
        let set_username_req = SetUsernameRequest {
            new_username: username.clone(),
        };

        let response = state
            .client_api
            .set_username(&auth.user_id.to_string(), set_username_req)
            .await
            .map_err(super::error::map_api_error)?;

        let new_username = response.user.as_ref().map_or_else(|| username.clone(), |u| u.username.clone());
        result.insert("username".to_string(), serde_json::Value::String(new_username));
        updated_fields.push("username");
    }

    // Process password update if requested
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
            .map_err(super::error::map_api_error)?;

        updated_fields.push("password");
    }

    if updated_fields.is_empty() {
        return Err(super::AppError::bad_request("No valid update fields provided (username or password)"));
    }

    result.insert(
        "message".to_string(),
        serde_json::Value::String(format!("{} updated successfully", updated_fields.join(" and "))),
    );
    Ok(Json(serde_json::Value::Object(result)))
}

/// Get user's joined rooms (paginated)
pub async fn get_joined_rooms(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> AppResult<Json<ListParticipatedRoomsResponse>> {
    let page_opt = params.get("page").and_then(|v| v.parse().ok());
    let page_size_opt = params.get("page_size").and_then(|v| v.parse().ok());
    let (page, page_size) = super::validation::validate_pagination(page_opt, page_size_opt);

    let response = state
        .client_api
        .get_joined_rooms(&auth.user_id.to_string(), page, page_size)
        .await
        .map_err(super::error::map_api_error)?;

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
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Delete the current user's own account (soft-delete)
///
/// Sets `deleted_at = NOW()` on the user row and cleans up `OAuth2` mappings.
/// The current token will return 401 on the next request because the security
/// pipeline checks for deleted users.
pub async fn delete_me(
    auth: AuthUser,
    State(state): State<AppState>,
) -> AppResult<axum::http::StatusCode> {
    state
        .user_service
        .delete_self(&auth.user_id)
        .await
        .map_err(super::AppError::from)?;

    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// List rooms created by this user
/// GET /api/user/rooms/created
pub async fn list_created_rooms(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> AppResult<Json<ListCreatedRoomsResponse>> {
    let page_opt = params.get("page").and_then(|v| v.parse().ok());
    let page_size_opt = params.get("page_size").and_then(|v| v.parse().ok());
    let (page, page_size) = super::validation::validate_pagination(page_opt, page_size_opt);

    let req = crate::proto::client::ListCreatedRoomsRequest { page, page_size };
    let response = state
        .client_api
        .list_created_rooms(&auth.user_id.to_string(), req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}
