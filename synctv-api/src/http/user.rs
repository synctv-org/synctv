//! User management HTTP handlers
//
// This layer now uses proto types and delegates to the impls layer for business logic

use axum::{
    extract::{Path, Query, State},
    Json,
};

use super::{middleware::AuthUser, AppResult, AppState};
use crate::proto::client::{
    DeleteRoomResponse, GetProfileResponse, ListCreatedRoomsResponse, ListParticipatedRoomsResponse,
};

/// Typed request for PATCH /api/user
#[derive(serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpdateUserRequest {
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub old_password: Option<String>,
}

#[derive(serde::Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpdateUserResponseDoc {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

/// Get current user info
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/user",
        tag = "User",
        responses(
            (status = 200, description = "Current user profile", body = GetProfileResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
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
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/user",
        tag = "User",
        request_body = UpdateUserRequest,
        responses(
            (status = 200, description = "User profile updated", body = UpdateUserResponseDoc),
            (status = 400, description = "Invalid update request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn update_user(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<UpdateUserRequest>,
) -> AppResult<Json<UpdateUserResponseDoc>> {
    let UpdateUserRequest {
        username,
        password,
        old_password,
    } = req;

    let mut updated_fields = Vec::new();
    if username.is_some() {
        updated_fields.push("username");
    }
    if password.is_some() {
        updated_fields.push("password");
    }
    if updated_fields.is_empty() {
        return Err(super::AppError::bad_request(
            "No valid update fields provided (username or password)",
        ));
    }

    let response = state
        .client_api
        .update_profile(
            &auth.user_id.to_string(),
            username.clone(),
            old_password,
            password,
        )
        .await
        .map_err(super::error::map_api_error)?;

    let username = if let Some(user) = response.user {
        Some(user.username)
    } else {
        username
    };
    Ok(Json(UpdateUserResponseDoc {
        message: format!("{} updated successfully", updated_fields.join(" and ")),
        username,
    }))
}

/// Get user's joined rooms (paginated)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/user/rooms",
        tag = "User",
        params(
            ("page" = Option<i32>, Query, description = "Page number"),
            ("page_size" = Option<i32>, Query, description = "Page size"),
            ("search" = Option<String>, Query, description = "Search by room name or description"),
            ("status" = Option<String>, Query, description = "Filter by room status: active, pending, closed"),
            ("is_banned" = Option<bool>, Query, description = "Filter by room ban state"),
            ("sort_by" = Option<String>, Query, description = "Sort by: joined_at, created_at, updated_at, last_activity_at, name"),
            ("sort_direction" = Option<String>, Query, description = "Sort direction: asc or desc")
        ),
        responses(
            (status = 200, description = "Joined rooms", body = ListParticipatedRoomsResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_joined_rooms(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> AppResult<Json<ListParticipatedRoomsResponse>> {
    let page_opt = params.get("page").and_then(|v| v.parse().ok());
    let page_size_opt = params.get("page_size").and_then(|v| v.parse().ok());
    let (page, page_size) = super::validation::validate_pagination(page_opt, page_size_opt);

    let req = crate::proto::client::ListParticipatedRoomsRequest {
        page,
        page_size,
        search: params.get("search").cloned().unwrap_or_default(),
        status: match params.get("status").map(String::as_str) {
            Some("active") => synctv_proto::common::RoomStatus::Active as i32,
            Some("pending") => synctv_proto::common::RoomStatus::Pending as i32,
            Some("closed") => synctv_proto::common::RoomStatus::Closed as i32,
            _ => synctv_proto::common::RoomStatus::Unspecified as i32,
        },
        is_banned: params
            .get("is_banned")
            .and_then(|value| value.parse::<bool>().ok()),
        sort_by: match params.get("sort_by").map(String::as_str) {
            Some("name") => crate::proto::client::RelatedRoomListSortBy::Name as i32,
            Some("created_at") => crate::proto::client::RelatedRoomListSortBy::CreatedAt as i32,
            Some("updated_at") => crate::proto::client::RelatedRoomListSortBy::UpdatedAt as i32,
            Some("last_activity_at") => {
                crate::proto::client::RelatedRoomListSortBy::LastActivityAt as i32
            }
            _ => crate::proto::client::RelatedRoomListSortBy::JoinedAt as i32,
        },
        sort_direction: match params.get("sort_direction").map(String::as_str) {
            Some("asc") => crate::proto::client::SortDirection::Asc as i32,
            _ => crate::proto::client::SortDirection::Desc as i32,
        },
    };

    let response = state
        .client_api
        .get_joined_rooms(&auth.user_id.to_string(), req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Delete a room (user's own room)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/user/rooms/{room_id}",
        tag = "User",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "Room deleted", body = DeleteRoomResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Permission denied", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Room not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
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
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/user/me",
        tag = "User",
        responses(
            (status = 204, description = "Current user deleted"),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
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
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/user/rooms/created",
        tag = "User",
        params(
            ("page" = Option<i32>, Query, description = "Page number"),
            ("page_size" = Option<i32>, Query, description = "Page size")
        ),
        responses(
            (status = 200, description = "Rooms created by the current user", body = ListCreatedRoomsResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn list_created_rooms(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> AppResult<Json<ListCreatedRoomsResponse>> {
    let page_opt = params.get("page").and_then(|v| v.parse().ok());
    let page_size_opt = params.get("page_size").and_then(|v| v.parse().ok());
    let (page, page_size) = super::validation::validate_pagination(page_opt, page_size_opt);

    let req = crate::proto::client::ListCreatedRoomsRequest {
        page,
        page_size,
        search: params.get("search").cloned().unwrap_or_default(),
        status: match params.get("status").map(String::as_str) {
            Some("active") => synctv_proto::common::RoomStatus::Active as i32,
            Some("pending") => synctv_proto::common::RoomStatus::Pending as i32,
            Some("closed") => synctv_proto::common::RoomStatus::Closed as i32,
            _ => synctv_proto::common::RoomStatus::Unspecified as i32,
        },
        is_banned: params
            .get("is_banned")
            .and_then(|value| value.parse::<bool>().ok()),
        sort_by: match params.get("sort_by").map(String::as_str) {
            Some("name") => crate::proto::client::RoomListSortBy::Name as i32,
            Some("updated_at") => crate::proto::client::RoomListSortBy::UpdatedAt as i32,
            Some("last_activity_at") => crate::proto::client::RoomListSortBy::LastActivityAt as i32,
            _ => crate::proto::client::RoomListSortBy::CreatedAt as i32,
        },
        sort_direction: match params.get("sort_direction").map(String::as_str) {
            Some("asc") => crate::proto::client::SortDirection::Asc as i32,
            _ => crate::proto::client::SortDirection::Desc as i32,
        },
    };
    let response = state
        .client_api
        .list_created_rooms(&auth.user_id.to_string(), req)
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}
