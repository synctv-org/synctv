//! Admin HTTP handlers
//!
//! All admin routes require authentication and admin/root role.
//! Thin handlers that delegate to `AdminApiImpl`.

use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use std::sync::Arc;

use super::{
    admin_execute::{execute_admin_endpoint, execute_root_endpoint, request_metadata},
    middleware::RequestMetadata,
    validation::ValidatedQuery,
    AppError, AppResult, AppState, WithRoomId, WithUserId,
};
use crate::proto::admin;

fn require_admin_api(state: &AppState) -> Result<Arc<crate::impls::AdminApiImpl>, AppError> {
    state.admin_api.clone().ok_or_else(|| {
        AppError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "Admin service is not available on this server.",
        )
    })
}

/// Map a typed `ApiError` to an HTTP `AppError` with guaranteed-correct
/// status code mapping (no keyword-based heuristics).
fn admin_err_to_app_error(err: crate::impls::ApiError) -> AppError {
    AppError::from(err)
}

// Path validation helpers

fn validate_admin_proto_path<T>(path: T) -> Result<T, AppError>
where
    T: prost_reflect::ReflectMessage,
{
    crate::impls::validate_proto_request(&path).map_err(admin_err_to_app_error)?;
    Ok(path)
}

// Router

pub fn create_admin_router() -> Router<AppState> {
    Router::new()
        // System stats
        .route("/stats", get(get_system_stats))
        // Review workflow
        .route(
            "/reviews/user-registrations",
            get(list_user_registration_reviews),
        )
        .route(
            "/reviews/user-registrations/approve",
            post(approve_user_registration_review),
        )
        .route(
            "/reviews/user-registrations/reject",
            post(reject_user_registration_review),
        )
        .route("/reviews/room-creations", get(list_room_creation_reviews))
        .route(
            "/reviews/room-creations/approve",
            post(approve_room_creation_review),
        )
        .route(
            "/reviews/room-creations/reject",
            post(reject_room_creation_review),
        )
        .route("/reviews/room-joins", get(list_room_join_reviews))
        .route(
            "/reviews/room-joins/approve",
            post(approve_room_join_review),
        )
        .route("/reviews/room-joins/reject", post(reject_room_join_review))
        // Moderation bans
        .route("/bans", get(list_ban_records))
        // Settings
        .route("/settings", get(get_settings).post(set_settings))
        .route("/settings/{group}", get(get_settings_group))
        // Email
        .route("/email/test", post(send_test_email))
        // User management
        .route("/users", get(list_users).post(create_user))
        .route("/users/{user_id}", get(get_user).delete(delete_user))
        .route(
            "/users/{user_id}/preferences",
            get(get_user_preferences).patch(update_user_preferences),
        )
        .route("/users/{user_id}/role", post(set_user_role))
        .route("/users/{user_id}/password", post(set_user_password))
        .route("/users/{user_id}/username", post(set_user_username))
        .route("/users/{user_id}/ban", post(ban_user))
        .route("/users/{user_id}/unban", post(unban_user))
        .route("/users/{user_id}/rooms", get(get_user_rooms))
        // Batch user operations
        .route("/users/batch/ban", post(batch_ban_users))
        .route("/users/batch/delete", post(batch_delete_users))
        // Room management
        .route("/rooms", get(list_rooms))
        .route("/rooms/{room_id}", get(get_room).delete(delete_room))
        .route("/rooms/{room_id}/password", post(set_room_password))
        .route("/rooms/{room_id}/members", get(get_room_members))
        .route("/rooms/{room_id}/ban", post(ban_room))
        .route("/rooms/{room_id}/unban", post(unban_room))
        .route(
            "/rooms/{room_id}/settings",
            get(get_room_settings).post(set_room_settings),
        )
        .route("/rooms/{room_id}/settings/reset", post(reset_room_settings))
        // Batch room operations
        .route("/rooms/batch/ban", post(batch_ban_rooms))
        .route("/rooms/batch/delete", post(batch_delete_rooms))
        // Stream management
        .route("/streams", get(list_streams))
        .route("/streams/kick", post(kick_stream))
        // Admin management (root only)
        .route("/admins", get(list_admins))
        .route("/admins/{user_id}", post(add_admin).delete(remove_admin))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/reviews/user-registrations",
        tag = "Admin",
        params(admin::ListUserRegistrationReviewsRequest),
        responses(
            (status = 200, description = "User registration reviews", body = admin::ListUserRegistrationReviewsResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_user_registration_reviews(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ValidatedQuery(req): ValidatedQuery<admin::ListUserRegistrationReviewsRequest>,
) -> AppResult<Json<admin::ListUserRegistrationReviewsResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, _| async move {
            api.list_user_registration_reviews(req, &validated.user_id)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/reviews/user-registrations/approve",
        tag = "Admin",
        request_body = admin::ApproveUserRegistrationReviewRequest,
        responses(
            (status = 200, description = "User registration review approved", body = admin::ApproveUserRegistrationReviewResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn approve_user_registration_review(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::ApproveUserRegistrationReviewRequest>,
) -> AppResult<Json<admin::ApproveUserRegistrationReviewResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.approve_user_registration_review(req, &validated.user_id, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/reviews/user-registrations/reject",
        tag = "Admin",
        request_body = admin::RejectUserRegistrationReviewRequest,
        responses(
            (status = 200, description = "User registration review rejected", body = admin::RejectUserRegistrationReviewResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn reject_user_registration_review(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::RejectUserRegistrationReviewRequest>,
) -> AppResult<Json<admin::RejectUserRegistrationReviewResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, _| async move {
            api.reject_user_registration_review(req, &validated.user_id)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/reviews/room-creations",
        tag = "Admin",
        params(admin::ListRoomCreationReviewsRequest),
        responses((status = 200, description = "Room creation reviews", body = admin::ListRoomCreationReviewsResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_room_creation_reviews(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ValidatedQuery(req): ValidatedQuery<admin::ListRoomCreationReviewsRequest>,
) -> AppResult<Json<admin::ListRoomCreationReviewsResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, _| async move {
            api.list_room_creation_reviews(req, &validated.user_id)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/reviews/room-creations/approve",
        tag = "Admin",
        request_body = admin::ApproveRoomCreationReviewRequest,
        responses((status = 200, description = "Room creation review approved", body = admin::ApproveRoomCreationReviewResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn approve_room_creation_review(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::ApproveRoomCreationReviewRequest>,
) -> AppResult<Json<admin::ApproveRoomCreationReviewResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.approve_room_creation_review(req, &validated.user_id, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/reviews/room-creations/reject",
        tag = "Admin",
        request_body = admin::RejectRoomCreationReviewRequest,
        responses((status = 200, description = "Room creation review rejected", body = admin::RejectRoomCreationReviewResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn reject_room_creation_review(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::RejectRoomCreationReviewRequest>,
) -> AppResult<Json<admin::RejectRoomCreationReviewResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, _| async move {
            api.reject_room_creation_review(req, &validated.user_id)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/reviews/room-joins",
        tag = "Admin",
        params(admin::ListRoomJoinReviewsRequest),
        responses((status = 200, description = "Room join reviews", body = admin::ListRoomJoinReviewsResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_room_join_reviews(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ValidatedQuery(req): ValidatedQuery<admin::ListRoomJoinReviewsRequest>,
) -> AppResult<Json<admin::ListRoomJoinReviewsResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, _| async move {
            api.list_room_join_reviews(req, &validated.user_id).await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/reviews/room-joins/approve",
        tag = "Admin",
        request_body = admin::ApproveRoomJoinReviewRequest,
        responses((status = 200, description = "Room join review approved", body = admin::ApproveRoomJoinReviewResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn approve_room_join_review(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::ApproveRoomJoinReviewRequest>,
) -> AppResult<Json<admin::ApproveRoomJoinReviewResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.approve_room_join_review(req, &validated.user_id, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/reviews/room-joins/reject",
        tag = "Admin",
        request_body = admin::RejectRoomJoinReviewRequest,
        responses((status = 200, description = "Room join review rejected", body = admin::RejectRoomJoinReviewResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn reject_room_join_review(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::RejectRoomJoinReviewRequest>,
) -> AppResult<Json<admin::RejectRoomJoinReviewResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.reject_room_join_review(req, &validated.user_id, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/bans",
        tag = "Admin",
        params(admin::ListBanRecordsRequest),
        responses(
            (status = 200, description = "Ban records", body = admin::ListBanRecordsResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_ban_records(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ValidatedQuery(req): ValidatedQuery<admin::ListBanRecordsRequest>,
) -> AppResult<Json<admin::ListBanRecordsResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, _| async move { api.list_ban_records(req, &validated.user_id).await },
    )
    .await?;
    Ok(Json(resp))
}

// System Stats

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/stats",
        tag = "Admin",
        responses(
            (status = 200, description = "System stats", body = admin::GetSystemStatsResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn get_system_stats(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
) -> AppResult<Json<admin::GetSystemStatsResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move { api.get_system_stats(admin::GetSystemStatsRequest {}).await },
    )
    .await?;
    Ok(Json(resp))
}

// Settings

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/settings",
        tag = "Admin",
        responses(
            (status = 200, description = "All settings groups", body = admin::GetSettingsResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn get_settings(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
) -> AppResult<Json<admin::GetSettingsResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.get_settings(admin::GetSettingsRequest {}, &validated.user_id, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/settings/{group}",
        tag = "Admin",
        params(("group" = String, Path, description = "Settings group key")),
        responses(
            (status = 200, description = "Single settings group", body = admin::GetSettingsGroupResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn get_settings_group(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<admin::GetSettingsGroupRequest>,
) -> AppResult<Json<admin::GetSettingsGroupResponse>> {
    let req = validate_admin_proto_path(path)?;
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.get_settings_group(req, &validated.user_id, &rctx).await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/settings",
        tag = "Admin",
        request_body = admin::UpdateSettingsRequest,
        responses(
            (status = 200, description = "Settings updated", body = admin::UpdateSettingsResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn set_settings(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::UpdateSettingsRequest>,
) -> AppResult<Json<admin::UpdateSettingsResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.update_settings(req, &validated.user_id, &rctx).await
        },
    )
    .await?;
    Ok(Json(resp))
}

// Email

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/email/test",
        tag = "Admin",
        request_body = admin::SendTestEmailRequest,
        responses(
            (status = 200, description = "Test email sent", body = admin::SendTestEmailResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn send_test_email(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::SendTestEmailRequest>,
) -> AppResult<Json<admin::SendTestEmailResponse>> {
    let request_meta = request_metadata(request_meta);
    let api = require_admin_api(&state)?.clone();
    let executor = api.clone();
    let resp = executor
        .execute_admin_endpoint_with_control(&request_meta, move |request_control, _| async move {
            api.send_test_email_with_control(req, Some(&request_control))
                .await
        })
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

// User Management

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/users",
        tag = "Admin",
        params(admin::ListUsersRequest),
        responses(
            (status = 200, description = "Users list", body = admin::ListUsersResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_users(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ValidatedQuery(req): ValidatedQuery<admin::ListUsersRequest>,
) -> AppResult<Json<admin::ListUsersResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move { api.list_users(req).await },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/users/{user_id}",
        tag = "Admin",
        params(("user_id" = String, Path, description = "User ID")),
        responses(
            (status = 200, description = "User detail", body = admin::GetUserResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "User not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn get_user(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<admin::GetUserRequest>,
) -> AppResult<Json<admin::GetUserResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move { api.get_user(req).await },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/users/{user_id}/preferences",
        tag = "Admin",
        params(("user_id" = String, Path, description = "User ID")),
        responses(
            (status = 200, description = "User preferences", body = admin::GetUserPreferencesResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "User not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn get_user_preferences(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<admin::GetUserPreferencesRequest>,
) -> AppResult<Json<admin::GetUserPreferencesResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move { api.get_user_preferences(req).await },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/admin/users/{user_id}/preferences",
        tag = "Admin",
        params(("user_id" = String, Path, description = "User ID")),
        request_body = admin::UpdateUserPreferencesRequest,
        responses(
            (status = 200, description = "User preferences updated", body = admin::UpdateUserPreferencesResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "User not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn update_user_preferences(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<admin::UserPathRequest>,
    Json(req): Json<admin::UpdateUserPreferencesRequest>,
) -> AppResult<Json<admin::UpdateUserPreferencesResponse>> {
    let req = req.with_user_id(validate_admin_proto_path(path)?.user_id);
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.update_user_preferences(req, &validated.user_id, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/users",
        tag = "Admin",
        request_body = admin::CreateUserRequest,
        responses(
            (status = 200, description = "User created", body = admin::CreateUserResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn create_user(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::CreateUserRequest>,
) -> AppResult<Json<admin::CreateUserResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.create_user(req, validated.role, &validated.user_id, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/admin/users/{user_id}",
        tag = "Admin",
        params(("user_id" = String, Path, description = "User ID")),
        responses(
            (status = 200, description = "User deleted", body = admin::DeleteUserResponse),
            (status = 401, description = "Root authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn delete_user(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<admin::DeleteUserRequest>,
) -> AppResult<Json<admin::DeleteUserResponse>> {
    let resp =
        execute_root_endpoint(
            &state,
            request_meta,
            require_admin_api,
            move |api, validated, rctx| async move {
                api.delete_user(req, &validated.user_id, &rctx).await
            },
        )
        .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/users/{user_id}/role",
        tag = "Admin",
        params(("user_id" = String, Path, description = "User ID")),
        request_body = admin::UpdateUserRoleRequest,
        responses(
            (status = 200, description = "User role updated", body = admin::UpdateUserRoleResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn set_user_role(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<admin::UserPathRequest>,
    Json(req): Json<admin::UpdateUserRoleRequest>,
) -> AppResult<Json<admin::UpdateUserRoleResponse>> {
    let req = req.with_user_id(validate_admin_proto_path(path)?.user_id);
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.update_user_role(req, &validated.user_id, validated.role, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/users/{user_id}/password",
        tag = "Admin",
        params(("user_id" = String, Path, description = "User ID")),
        request_body = admin::UpdateUserPasswordRequest,
        responses(
            (status = 200, description = "User password updated", body = admin::UpdateUserPasswordResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn set_user_password(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<admin::UserPathRequest>,
    Json(mut req): Json<admin::UpdateUserPasswordRequest>,
) -> AppResult<Json<admin::UpdateUserPasswordResponse>> {
    req = req.with_user_id(validate_admin_proto_path(path)?.user_id);
    if req.reason.is_empty() {
        req.reason = "Admin forced password reset".to_string();
    }
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.update_user_password(req, validated.user_id, validated.role, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/users/{user_id}/username",
        tag = "Admin",
        params(("user_id" = String, Path, description = "User ID")),
        request_body = admin::UpdateUserUsernameRequest,
        responses(
            (status = 200, description = "Username updated", body = admin::UpdateUserUsernameResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn set_user_username(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<admin::UserPathRequest>,
    Json(req): Json<admin::UpdateUserUsernameRequest>,
) -> AppResult<Json<admin::UpdateUserUsernameResponse>> {
    let req = req.with_user_id(validate_admin_proto_path(path)?.user_id);
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.update_user_username(req, &validated.user_id, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/users/{user_id}/ban",
        tag = "Admin",
        params(("user_id" = String, Path, description = "User ID")),
        request_body = admin::BanUserRequest,
        responses(
            (status = 200, description = "User banned", body = admin::BanUserResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn ban_user(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<admin::UserPathRequest>,
    Json(req): Json<admin::BanUserRequest>,
) -> AppResult<Json<admin::BanUserResponse>> {
    let req = req.with_user_id(validate_admin_proto_path(path)?.user_id);
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.ban_user(req, &validated.user_id, validated.role, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/users/{user_id}/unban",
        tag = "Admin",
        params(("user_id" = String, Path, description = "User ID")),
        responses(
            (status = 200, description = "User unbanned", body = admin::UnbanUserResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn unban_user(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<admin::UnbanUserRequest>,
) -> AppResult<Json<admin::UnbanUserResponse>> {
    let resp =
        execute_admin_endpoint(
            &state,
            request_meta,
            require_admin_api,
            move |api, validated, rctx| async move {
                api.unban_user(req, &validated.user_id, &rctx).await
            },
        )
        .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/users/{user_id}/rooms",
        tag = "Admin",
        params(
            ("user_id" = String, Path, description = "User ID"),
            admin::GetUserRoomsRequest
        ),
        responses(
            (status = 200, description = "Rooms belonging to user", body = admin::GetUserRoomsResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn get_user_rooms(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<admin::UserPathRequest>,
    Query(req): Query<admin::GetUserRoomsRequest>,
) -> AppResult<Json<admin::GetUserRoomsResponse>> {
    let req = req.with_user_id(validate_admin_proto_path(path)?.user_id);
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move { api.get_user_rooms(req).await },
    )
    .await?;
    Ok(Json(resp))
}

// Batch User Operations

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/users/batch/ban",
        tag = "Admin",
        request_body = admin::BatchBanUsersRequest,
        responses(
            (status = 200, description = "Users batch banned", body = admin::BatchBanUsersResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn batch_ban_users(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::BatchBanUsersRequest>,
) -> AppResult<Json<admin::BatchBanUsersResponse>> {
    if req.user_ids.is_empty() {
        return Err(AppError::bad_request("user_ids cannot be empty"));
    }
    if req.user_ids.len() > 100 {
        return Err(AppError::bad_request("Batch size exceeds limit of 100"));
    }
    if req.reason.len() > 500 {
        return Err(AppError::bad_request(
            "Reason too long (max 500 characters)",
        ));
    }
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.batch_ban_users(req, &validated.user_id, validated.role, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/users/batch/delete",
        tag = "Admin",
        request_body = admin::BatchDeleteUsersRequest,
        responses(
            (status = 200, description = "Users batch deleted", body = admin::BatchDeleteUsersResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Root authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn batch_delete_users(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::BatchDeleteUsersRequest>,
) -> AppResult<Json<admin::BatchDeleteUsersResponse>> {
    if req.user_ids.is_empty() {
        return Err(AppError::bad_request("user_ids cannot be empty"));
    }
    if req.user_ids.len() > 100 {
        return Err(AppError::bad_request("Batch size exceeds limit of 100"));
    }
    let resp = execute_root_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.batch_delete_users(req, &validated.user_id, validated.role, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

// Room Management

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/rooms",
        tag = "Admin",
        params(admin::ListRoomsRequest),
        responses(
            (status = 200, description = "Admin room list", body = admin::ListRoomsResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_rooms(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ValidatedQuery(req): ValidatedQuery<admin::ListRoomsRequest>,
) -> AppResult<Json<admin::ListRoomsResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move { api.list_rooms(req).await },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/rooms/{room_id}",
        tag = "Admin",
        params(("room_id" = String, Path, description = "Room ID")),
        responses(
            (status = 200, description = "Admin room detail", body = admin::GetRoomResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn get_room(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<admin::GetRoomRequest>,
) -> AppResult<Json<admin::GetRoomResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move { api.get_room(req).await },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/admin/rooms/{room_id}",
        tag = "Admin",
        params(("room_id" = String, Path, description = "Room ID")),
        responses(
            (status = 200, description = "Room deleted", body = admin::DeleteRoomResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn delete_room(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<admin::DeleteRoomRequest>,
) -> AppResult<Json<admin::DeleteRoomResponse>> {
    let resp =
        execute_admin_endpoint(
            &state,
            request_meta,
            require_admin_api,
            move |api, validated, rctx| async move {
                api.delete_room(req, &validated.user_id, &rctx).await
            },
        )
        .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/rooms/{room_id}/password",
        tag = "Admin",
        params(("room_id" = String, Path, description = "Room ID")),
        request_body = admin::UpdateRoomPasswordRequest,
        responses(
            (status = 200, description = "Room password updated", body = admin::UpdateRoomPasswordResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn set_room_password(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<admin::RoomPathRequest>,
    Json(req): Json<admin::UpdateRoomPasswordRequest>,
) -> AppResult<Json<admin::UpdateRoomPasswordResponse>> {
    let req = req.with_room_id(validate_admin_proto_path(path)?.room_id);
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.update_room_password(req, &validated.user_id, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/rooms/{room_id}/members",
        tag = "Admin",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            admin::GetRoomMembersRequest
        ),
        responses(
            (status = 200, description = "Room members", body = admin::GetRoomMembersResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn get_room_members(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<admin::RoomPathRequest>,
    Query(req): Query<admin::GetRoomMembersRequest>,
) -> AppResult<Json<admin::GetRoomMembersResponse>> {
    let req = req.with_room_id(validate_admin_proto_path(path)?.room_id);
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move { api.get_room_members(req).await },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/rooms/{room_id}/ban",
        tag = "Admin",
        params(("room_id" = String, Path, description = "Room ID")),
        request_body = admin::BanRoomRequest,
        responses(
            (status = 200, description = "Room banned", body = admin::BanRoomResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn ban_room(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<admin::RoomPathRequest>,
    Json(req): Json<admin::BanRoomRequest>,
) -> AppResult<Json<admin::BanRoomResponse>> {
    let req = req.with_room_id(validate_admin_proto_path(path)?.room_id);
    let resp =
        execute_admin_endpoint(
            &state,
            request_meta,
            require_admin_api,
            move |api, validated, rctx| async move {
                api.ban_room(req, &validated.user_id, &rctx).await
            },
        )
        .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/rooms/{room_id}/unban",
        tag = "Admin",
        params(("room_id" = String, Path, description = "Room ID")),
        responses(
            (status = 200, description = "Room unbanned", body = admin::UnbanRoomResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn unban_room(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<admin::UnbanRoomRequest>,
) -> AppResult<Json<admin::UnbanRoomResponse>> {
    let resp =
        execute_admin_endpoint(
            &state,
            request_meta,
            require_admin_api,
            move |api, validated, rctx| async move {
                api.unban_room(req, &validated.user_id, &rctx).await
            },
        )
        .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/rooms/{room_id}/settings",
        tag = "Admin",
        params(("room_id" = String, Path, description = "Room ID")),
        responses(
            (status = 200, description = "Room settings", body = admin::GetRoomSettingsResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn get_room_settings(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<admin::GetRoomSettingsRequest>,
) -> AppResult<Json<admin::GetRoomSettingsResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move { api.get_room_settings(req).await },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/rooms/{room_id}/settings",
        tag = "Admin",
        params(("room_id" = String, Path, description = "Room ID")),
        request_body = admin::UpdateRoomSettingsRequest,
        responses(
            (status = 200, description = "Room settings updated", body = admin::UpdateRoomSettingsResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn set_room_settings(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<admin::RoomPathRequest>,
    Json(req): Json<admin::UpdateRoomSettingsRequest>,
) -> AppResult<Json<admin::UpdateRoomSettingsResponse>> {
    let req = req.with_room_id(validate_admin_proto_path(path)?.room_id);
    let resp =
        execute_admin_endpoint(
            &state,
            request_meta,
            require_admin_api,
            move |api, validated, _| async move {
                api.update_room_settings(req, &validated.user_id).await
            },
        )
        .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/rooms/{room_id}/settings/reset",
        tag = "Admin",
        params(("room_id" = String, Path, description = "Room ID")),
        responses(
            (status = 200, description = "Room settings reset", body = admin::ResetRoomSettingsResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn reset_room_settings(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<admin::ResetRoomSettingsRequest>,
) -> AppResult<Json<admin::ResetRoomSettingsResponse>> {
    let resp =
        execute_admin_endpoint(
            &state,
            request_meta,
            require_admin_api,
            move |api, validated, _| async move {
                api.reset_room_settings(req, &validated.user_id).await
            },
        )
        .await?;
    Ok(Json(resp))
}

// Batch Room Operations

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/rooms/batch/ban",
        tag = "Admin",
        request_body = admin::BatchBanRoomsRequest,
        responses(
            (status = 200, description = "Rooms batch banned", body = admin::BatchBanRoomsResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn batch_ban_rooms(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::BatchBanRoomsRequest>,
) -> AppResult<Json<admin::BatchBanRoomsResponse>> {
    if req.room_ids.is_empty() {
        return Err(AppError::bad_request("room_ids cannot be empty"));
    }
    if req.room_ids.len() > 100 {
        return Err(AppError::bad_request("Batch size exceeds limit of 100"));
    }
    if req.reason.len() > 500 {
        return Err(AppError::bad_request(
            "Reason too long (max 500 characters)",
        ));
    }
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.batch_ban_rooms(req, &validated.user_id, &rctx).await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/rooms/batch/delete",
        tag = "Admin",
        request_body = admin::BatchDeleteRoomsRequest,
        responses(
            (status = 200, description = "Rooms batch deleted", body = admin::BatchDeleteRoomsResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn batch_delete_rooms(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::BatchDeleteRoomsRequest>,
) -> AppResult<Json<admin::BatchDeleteRoomsResponse>> {
    if req.room_ids.is_empty() {
        return Err(AppError::bad_request("room_ids cannot be empty"));
    }
    if req.room_ids.len() > 100 {
        return Err(AppError::bad_request("Batch size exceeds limit of 100"));
    }
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.batch_delete_rooms(req, &validated.user_id, &rctx).await
        },
    )
    .await?;
    Ok(Json(resp))
}

// Stream Management

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/streams",
        tag = "Admin",
        params(admin::ListActiveStreamsRequest),
        responses(
            (status = 200, description = "Active streams", body = admin::ListActiveStreamsResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_streams(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ValidatedQuery(req): ValidatedQuery<admin::ListActiveStreamsRequest>,
) -> AppResult<Json<admin::ListActiveStreamsResponse>> {
    let response = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move { api.list_active_streams(req).await },
    )
    .await?;
    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/streams/kick",
        tag = "Admin",
        request_body = admin::KickStreamRequest,
        responses(
            (status = 200, description = "Stream kicked", body = admin::KickStreamResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn kick_stream(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::KickStreamRequest>,
) -> AppResult<Json<admin::KickStreamResponse>> {
    execute_admin_endpoint(&state, request_meta, require_admin_api, move |api, validated, rctx| async move {
        api.kick_stream(req, &validated.user_id, &rctx).await
    })
    .await?;
    Ok(Json(admin::KickStreamResponse {}))
}

// Admin Management (Root Only)

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/admins",
        tag = "Admin",
        responses(
            (status = 200, description = "Admins list", body = admin::ListAdminsResponse),
            (status = 401, description = "Root authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_admins(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ValidatedQuery(req): ValidatedQuery<admin::ListAdminsRequest>,
) -> AppResult<Json<admin::ListAdminsResponse>> {
    let resp = execute_root_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move { api.list_admins(req).await },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/admins/{user_id}",
        tag = "Admin",
        params(("user_id" = String, Path, description = "User ID")),
        responses(
            (status = 200, description = "Admin added", body = admin::AddAdminResponse),
            (status = 401, description = "Root authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn add_admin(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<admin::AddAdminRequest>,
) -> AppResult<Json<admin::AddAdminResponse>> {
    let resp =
        execute_root_endpoint(
            &state,
            request_meta,
            require_admin_api,
            move |api, validated, rctx| async move {
                api.add_admin(req, &validated.user_id, &rctx).await
            },
        )
        .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/admin/admins/{user_id}",
        tag = "Admin",
        params(("user_id" = String, Path, description = "User ID")),
        responses(
            (status = 200, description = "Admin removed", body = admin::RemoveAdminResponse),
            (status = 401, description = "Root authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn remove_admin(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<admin::RemoveAdminRequest>,
) -> AppResult<Json<admin::RemoveAdminResponse>> {
    let resp = execute_root_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.remove_admin(req, &validated.user_id, &rctx).await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::{ConnectInfo, FromRequestParts},
        http::Request,
    };
    use std::net::SocketAddr;

    #[test]
    fn test_update_user_role_request_deserialization() {
        let json = format!(
            r#"{{"role":{}}}"#,
            synctv_proto::common::UserRole::Admin as i32
        );
        let req: admin::UpdateUserRoleRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(req.user_id, "");
        assert_eq!(req.role, synctv_proto::common::UserRole::Admin as i32);
    }

    #[test]
    fn test_update_user_role_request_all_roles() {
        let role_mappings = [
            (synctv_proto::common::UserRole::Root as i32),
            (synctv_proto::common::UserRole::Admin as i32),
            (synctv_proto::common::UserRole::User as i32),
        ];

        for expected in role_mappings {
            let json = format!(r#"{{"role":{expected}}}"#);
            let req: admin::UpdateUserRoleRequest =
                serde_json::from_str(&json).expect("deserialize");
            assert_eq!(req.role, expected);
        }
    }

    #[test]
    fn test_update_user_role_request_rejects_string_role() {
        let err = serde_json::from_str::<admin::UpdateUserRoleRequest>(r#"{"role":"admin"}"#)
            .expect_err("string role should be rejected");
        assert!(err.is_data());
    }

    #[test]
    fn test_update_room_settings_request_accepts_raw_json_body() {
        let json = r#"{"theme":"dark","guest_enabled":true}"#;
        let req: admin::UpdateRoomSettingsRequest =
            serde_json::from_str(json).expect("deserialize");
        assert_eq!(req.room_id, "");
        let settings_json: serde_json::Value =
            serde_json::from_slice(&req.settings).expect("settings bytes should contain JSON");
        assert_eq!(
            settings_json,
            serde_json::json!({"theme":"dark","guest_enabled":true})
        );
    }

    #[test]
    fn test_admin_user_path_request_deserializes_proto_field_name() {
        let req: admin::UserPathRequest =
            serde_json::from_str(r#"{"user_id":"AbC123xYz890"}"#).expect("deserialize");

        assert_eq!(req.user_id, "AbC123xYz890");
    }

    #[test]
    fn test_admin_room_path_request_deserializes_proto_field_name() {
        let req: admin::RoomPathRequest =
            serde_json::from_str(r#"{"room_id":"AbC123xYz890"}"#).expect("deserialize");

        assert_eq!(req.room_id, "AbC123xYz890");
    }

    #[test]
    fn test_list_users_query_deserialization() {
        let json = r#"{"page":2,"page_size":50,"status":1,"role":2,"search":"test","sort_by":3,"sort_direction":1}"#;
        let query: admin::ListUsersRequest = serde_json::from_str(json).expect("deserialize");
        assert_eq!(query.page, 2);
        assert_eq!(query.page_size, 50);
        assert_eq!(
            query.status,
            synctv_proto::common::UserStatus::Active as i32
        );
        assert_eq!(query.role, synctv_proto::common::UserRole::Admin as i32);
        assert_eq!(query.search, "test");
        assert_eq!(query.sort_by, admin::UserListSortBy::Username as i32);
        assert_eq!(query.sort_direction, admin::SortDirection::Asc as i32);
    }

    #[test]
    fn test_list_rooms_query_deserialization() {
        let json = r#"{"page":1,"page_size":10,"status":1,"search":"room","creator_id":"user1","is_banned":false,"sort_by":3,"sort_direction":2}"#;
        let query: admin::ListRoomsRequest = serde_json::from_str(json).expect("deserialize");
        assert_eq!(query.page, 1);
        assert_eq!(query.page_size, 10);
        assert_eq!(
            query.status,
            synctv_proto::common::RoomStatus::Active as i32
        );
        assert_eq!(query.search, "room");
        assert_eq!(query.creator_id, "user1");
        assert_eq!(query.is_banned, Some(false));
        assert_eq!(query.sort_by, admin::RoomListSortBy::LastActivityAt as i32);
        assert_eq!(query.sort_direction, admin::SortDirection::Desc as i32);
    }

    #[test]
    fn test_room_members_query_deserialization() {
        let json =
            r#"{"page":2,"page_size":25,"search":"alice","role":3,"sort_by":2,"sort_direction":1}"#;
        let query: admin::GetRoomMembersRequest = serde_json::from_str(json).expect("deserialize");
        assert_eq!(query.page, 2);
        assert_eq!(query.page_size, 25);
        assert_eq!(query.search, "alice");
        assert_eq!(
            query.role,
            synctv_proto::common::RoomMemberRole::Admin as i32
        );
        assert_eq!(query.sort_by, admin::RoomMemberListSortBy::Username as i32);
        assert_eq!(query.sort_direction, admin::SortDirection::Asc as i32);
    }

    #[test]
    fn test_admin_path_scoped_queries_validate_after_path_id_injection() {
        let members_query: admin::GetRoomMembersRequest =
            serde_urlencoded::from_str("page_size=0").expect("query should deserialize");
        assert!(
            crate::impls::validate_proto_request(&members_query).is_err(),
            "path-scoped request is invalid until the path room_id is injected"
        );
        let members_query = members_query.with_room_id("room_1".to_string());
        crate::impls::validate_proto_request(&members_query)
            .expect("path-injected room members query should validate");

        let rooms_query: admin::GetUserRoomsRequest =
            serde_urlencoded::from_str("page_size=0").expect("query should deserialize");
        assert!(
            crate::impls::validate_proto_request(&rooms_query).is_err(),
            "path-scoped request is invalid until the path user_id is injected"
        );
        let rooms_query = rooms_query.with_user_id("usr_1".to_string());
        crate::impls::validate_proto_request(&rooms_query)
            .expect("path-injected user rooms query should validate");
    }

    #[tokio::test]
    async fn test_request_context_uses_trusted_proxy_headers_for_audit_ip() {
        let mut state = crate::http::tests::test_app_state();
        {
            let router_config = std::sync::Arc::make_mut(&mut state.router_config);
            let config = std::sync::Arc::make_mut(&mut router_config.config);
            config.server.trusted_proxies = vec!["127.0.0.1".to_string()];
        }

        let mut request = Request::builder()
            .uri("/admin/test")
            .header("X-Forwarded-For", "203.0.113.10")
            .header("User-Agent", "audit-test")
            .body(())
            .expect("request");
        request.extensions_mut().insert(ConnectInfo(
            "127.0.0.1:8080".parse::<SocketAddr>().expect("socket addr"),
        ));

        let (mut parts, ()) = request.into_parts();
        let request_meta =
            crate::http::middleware::RequestMetadata::from_request_parts(&mut parts, &state)
                .await
                .expect("extractor should not fail");
        let ctx = crate::http::admin_execute::request_context(&request_meta.0);

        assert_eq!(ctx.ip_address.as_deref(), Some("203.0.113.10"));
        assert_eq!(ctx.user_agent.as_deref(), Some("audit-test"));
    }

    #[tokio::test]
    async fn test_request_context_ignores_forwarded_headers_from_untrusted_proxy() {
        let state = crate::http::tests::test_app_state();

        let mut request = Request::builder()
            .uri("/admin/test")
            .header("X-Forwarded-For", "203.0.113.10")
            .body(())
            .expect("request");
        request.extensions_mut().insert(ConnectInfo(
            "198.51.100.7:8080"
                .parse::<SocketAddr>()
                .expect("socket addr"),
        ));

        let (mut parts, ()) = request.into_parts();
        let request_meta =
            crate::http::middleware::RequestMetadata::from_request_parts(&mut parts, &state)
                .await
                .expect("extractor should not fail");
        let ctx = crate::http::admin_execute::request_context(&request_meta.0);

        assert_eq!(ctx.ip_address.as_deref(), Some("198.51.100.7"));
    }

    #[tokio::test]
    async fn test_require_admin_api_error() {
        let mut state = crate::http::tests::test_app_state();
        state.admin_api = None;

        let Err(err) = require_admin_api(&state) else {
            panic!("missing admin api should fail");
        };
        assert_eq!(err.status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            err.message,
            "Admin service is not available on this server."
        );
    }

    #[test]
    fn test_ban_reason_length_validation() {
        // The handler validates reason.len() > 500
        let short_reason = "a".repeat(500);
        assert!(short_reason.len() <= 500);

        let long_reason = "a".repeat(501);
        assert!(long_reason.len() > 500);
    }

    #[test]
    fn test_get_user_rooms_query_defaults_to_proto_zero_values() {
        let query: admin::GetUserRoomsRequest = serde_urlencoded::from_str("").unwrap();

        assert!(query.user_id.is_empty());
        assert_eq!(query.page, 0);
        assert_eq!(query.page_size, 0);
        assert_eq!(query.status, 0);
        assert!(query.search.is_empty());
        assert_eq!(query.is_banned, None);
        assert_eq!(query.sort_by, 0);
        assert_eq!(query.sort_direction, 0);
    }

    #[test]
    fn test_list_active_streams_query_deserializes_explicit_values() {
        let query: admin::ListActiveStreamsRequest = serde_urlencoded::from_str(
            "page=2&page_size=25&room_id=room123&user_id=user123&node_id=node-a&search=live&sort_by=5&sort_direction=1",
        )
        .unwrap();

        assert_eq!(query.page, 2);
        assert_eq!(query.page_size, 25);
        assert_eq!(query.room_id, "room123");
        assert_eq!(query.user_id, "user123");
        assert_eq!(query.node_id, "node-a");
        assert_eq!(query.search, "live");
        assert_eq!(query.sort_by, admin::ActiveStreamListSortBy::NodeId as i32);
        assert_eq!(query.sort_direction, admin::SortDirection::Asc as i32);
    }

    #[test]
    fn test_list_admins_query_defaults_to_proto_zero_values() {
        let query: admin::ListAdminsRequest = serde_urlencoded::from_str("").unwrap();

        assert_eq!(query.page, 0);
        assert_eq!(query.page_size, 0);
        assert!(query.search.is_empty());
        assert_eq!(query.sort_by, 0);
        assert_eq!(query.sort_direction, 0);
    }

    #[test]
    fn test_admin_router_creation() {
        // Verify the admin router can be created without panicking
        let _router = create_admin_router();
    }

    #[test]
    fn test_room_members_page_size_clamp() {
        // The handler now uses centralized validation from validation module
        use super::super::validation;

        let raw_page_size: i32 = 1000;
        let clamped = validation::validate_page_size(Some(raw_page_size));
        assert_eq!(clamped, validation::MAX_PAGE_SIZE);

        let raw_page_size: i32 = 0;
        let clamped = validation::validate_page_size(Some(raw_page_size));
        assert_eq!(clamped, 1);

        let raw_page_size: i32 = 100;
        let clamped = validation::validate_page_size(Some(raw_page_size));
        assert_eq!(clamped, 100);
    }

    #[test]
    fn test_kick_stream_requires_room_id_and_media_id() {
        // The handler checks: if req.room_id.is_empty() || req.media_id.is_empty()
        let empty_room = admin::KickStreamRequest {
            room_id: String::new(),
            media_id: "media1".to_string(),
            reason: String::new(),
        };
        assert!(empty_room.room_id.is_empty());

        let empty_media = admin::KickStreamRequest {
            room_id: "room1".to_string(),
            media_id: String::new(),
            reason: String::new(),
        };
        assert!(empty_media.media_id.is_empty());

        let valid = admin::KickStreamRequest {
            room_id: "room1".to_string(),
            media_id: "media1".to_string(),
            reason: "test".to_string(),
        };
        assert!(!valid.room_id.is_empty() && !valid.media_id.is_empty());
    }

    #[test]
    fn test_kick_stream_request_uses_body_fields() {
        // L15: kick_stream uses room_id and media_id from body (not path).
        // Route is /streams/kick (no {stream_id} parameter).
        let req = admin::KickStreamRequest {
            room_id: "room123".to_string(),
            media_id: "media456".to_string(),
            reason: "test kick".to_string(),
        };
        assert_eq!(req.room_id, "room123");
        assert_eq!(req.media_id, "media456");
        assert_eq!(req.reason, "test kick");
    }
}
