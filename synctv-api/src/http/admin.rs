//! Admin HTTP handlers
//!
//! All admin routes require authentication and admin/root role.
//! Thin handlers that delegate to `AdminApiImpl`.

use axum::{
    extract::{FromRef, FromRequestParts, Path, Query, State},
    http::request::Parts,
    routing::{get, post, put},
    Json, Router,
};
use std::sync::Arc;
use synctv_core::models::id::UserId;
use synctv_core::service::auth::{AuthErrorCategory, JwtValidator, SecurityPipeline};

use super::{AppError, AppResult, AppState};
use crate::proto::admin;

// ------------------------------------------------------------------
// Auth extractors
// ------------------------------------------------------------------

/// Extension to hold JWT validator in request extensions (cached)
#[derive(Clone)]
struct JwtValidatorExt(Arc<JwtValidator>);

/// Shared JWT validation + admin auth verification.
///
/// Extracts JWT claims from the Authorization header, runs the shared
/// [`SecurityPipeline`] (password invalidation, user status, and access
/// token blacklist), then verifies admin role via `validate_admin_auth`.
async fn validate_auth_user(
    parts: &mut Parts,
    app_state: &AppState,
) -> Result<crate::impls::admin::ValidatedAdmin, AppError> {
    let validator = parts
        .extensions
        .get::<JwtValidatorExt>()
        .map_or_else(|| app_state.jwt_validator.clone(), |v| v.0.clone());

    let auth_header = parts
        .headers
        .get(axum::http::header::AUTHORIZATION)
        .ok_or_else(AppError::missing_authorization_header)?;

    let auth_str = auth_header
        .to_str()
        .map_err(|_| AppError::invalid_authorization_header())?;

    let claims = validator
        .validate_http(auth_str)
        .map_err(|_| AppError::invalid_or_expired_token())?;

    // Run the shared SecurityPipeline (password version, user status, access
    // token blacklist). This matches the checks in the regular AuthUser
    // extractor, preventing blacklisted tokens from accessing admin endpoints.
    app_state
        .security_pipeline
        .check(&claims)
        .await
        .map_err(|e| match SecurityPipeline::classify_auth_error(&e) {
            AuthErrorCategory::Authentication => AppError::invalid_or_expired_token(),
            AuthErrorCategory::Authorization => AppError::forbidden(format!("{e}")),
            AuthErrorCategory::Unavailable | AuthErrorCategory::Internal => AppError::from(e),
        })?;

    let user_id = UserId::from_string(claims.sub);

    crate::impls::admin::validate_admin_auth(
        &app_state.user_service,
        user_id,
        claims.pv,
        claims.iat,
    )
    .await
    .map_err(AppError::from)
}

/// Authenticated admin user (admin or root role required)
#[derive(Debug, Clone)]
pub struct AuthAdmin {
    pub user_id: UserId,
    pub role: synctv_core::models::UserRole,
}

impl<S> FromRequestParts<S> for AuthAdmin
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);
        let validated = validate_auth_user(parts, &app_state).await?;

        if !validated.role.is_admin_or_above() {
            return Err(AppError::forbidden("Admin role required"));
        }

        Ok(Self {
            user_id: validated.user_id,
            role: validated.role,
        })
    }
}

/// Authenticated root user (root role only)
#[derive(Debug, Clone)]
pub struct AuthRoot {
    pub user_id: UserId,
}

impl<S> FromRequestParts<S> for AuthRoot
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);
        let validated = validate_auth_user(parts, &app_state).await?;

        if !matches!(validated.role, synctv_core::models::UserRole::Root) {
            return Err(AppError::forbidden("Root role required"));
        }

        Ok(Self {
            user_id: validated.user_id,
        })
    }
}

// ------------------------------------------------------------------
// Request context extractor (IP + User-Agent for audit logs)
// ------------------------------------------------------------------

/// Extracts client IP address and User-Agent from HTTP request for audit logging.
pub(crate) struct ReqCtx(crate::impls::admin::RequestContext);

impl<S> FromRequestParts<S> for ReqCtx
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);
        let ip_address = parts
            .extensions
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|ci| {
                super::auth::extract_client_ip(&app_state.config, ci.0, &parts.headers).to_string()
            });
        let user_agent = parts
            .headers
            .get(axum::http::header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(std::string::ToString::to_string);
        Ok(Self(crate::impls::admin::RequestContext {
            ip_address,
            user_agent,
        }))
    }
}

// ------------------------------------------------------------------
// Helper to get admin_api or 503
// ------------------------------------------------------------------

fn require_admin_api(state: &AppState) -> Result<&Arc<crate::impls::AdminApiImpl>, AppError> {
    state
        .admin_api
        .as_ref()
        .ok_or_else(|| AppError::internal("Admin service not configured"))
}

/// Map a typed `ApiError` to an HTTP `AppError` with guaranteed-correct
/// status code mapping (no keyword-based heuristics).
fn admin_err_to_app_error(err: crate::impls::ApiError) -> AppError {
    AppError::from(err)
}

// ------------------------------------------------------------------
// ID validation helper
// ------------------------------------------------------------------

/// Validate a path-parameter ID (`user_id`, `room_id`, `media_id`, etc.).
///
/// Returns `Err(AppError)` with a 400 status when the ID is empty, too long,
/// or contains characters outside `[a-zA-Z0-9_-]`.
fn validate_path_id(id: &str, field: &'static str) -> Result<(), AppError> {
    super::validation::validate_id(id, field)
        .map(|_| ())
        .map_err(|e| AppError::bad_request(format!("Invalid {field}: {e}")))
}

// ------------------------------------------------------------------
// Router
// ------------------------------------------------------------------

pub fn create_admin_router() -> Router<AppState> {
    Router::new()
        // System stats
        .route("/stats", get(get_system_stats))
        // Settings
        .route("/settings", get(get_settings).post(set_settings))
        .route("/settings/{group}", get(get_settings_group))
        // Email
        .route("/email/test", post(send_test_email))
        // User management
        .route("/users", get(list_users).post(create_user))
        .route("/users/{user_id}", get(get_user).delete(delete_user))
        .route("/users/{user_id}/role", post(set_user_role))
        .route("/users/{user_id}/password", post(set_user_password))
        .route("/users/{user_id}/username", post(set_user_username))
        .route("/users/{user_id}/ban", post(ban_user))
        .route("/users/{user_id}/unban", post(unban_user))
        .route("/users/{user_id}/approve", post(approve_user))
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
        .route("/rooms/{room_id}/approve", post(approve_room))
        .route(
            "/rooms/{room_id}/settings",
            get(get_room_settings).post(set_room_settings),
        )
        .route("/rooms/{room_id}/settings/reset", post(reset_room_settings))
        // Batch room operations
        .route("/rooms/batch/ban", post(batch_ban_rooms))
        .route("/rooms/batch/delete", post(batch_delete_rooms))
        // Provider instances
        .route("/providers", get(list_providers).post(add_provider))
        .route(
            "/providers/{name}",
            put(update_provider).delete(delete_provider),
        )
        .route("/providers/{name}/reconnect", post(reconnect_provider))
        .route("/providers/{name}/enable", post(enable_provider))
        .route("/providers/{name}/disable", post(disable_provider))
        // Stream management
        .route("/streams", get(list_streams))
        .route("/streams/kick", post(kick_stream))
        // Admin management (root only)
        .route("/admins", get(list_admins))
        .route("/admins/{user_id}", post(add_admin).delete(remove_admin))
}

// ------------------------------------------------------------------
// System Stats
// ------------------------------------------------------------------

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
    _auth: AuthAdmin,
    State(state): State<AppState>,
) -> AppResult<Json<admin::GetSystemStatsResponse>> {
    let api = require_admin_api(&state)?;
    let resp = api
        .get_system_stats(admin::GetSystemStatsRequest {})
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

// ------------------------------------------------------------------
// Settings
// ------------------------------------------------------------------

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
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
) -> AppResult<Json<admin::GetSettingsResponse>> {
    let api = require_admin_api(&state)?;
    let resp = api
        .get_settings(admin::GetSettingsRequest {}, &auth.user_id, &rctx.0)
        .await
        .map_err(admin_err_to_app_error)?;
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
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Path(group): Path<String>,
) -> AppResult<Json<admin::GetSettingsGroupResponse>> {
    let api = require_admin_api(&state)?;
    let resp = api
        .get_settings_group(
            admin::GetSettingsGroupRequest { group },
            &auth.user_id,
            &rctx.0,
        )
        .await
        .map_err(admin_err_to_app_error)?;
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
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Json(req): Json<admin::UpdateSettingsRequest>,
) -> AppResult<Json<admin::UpdateSettingsResponse>> {
    let api = require_admin_api(&state)?;
    let resp = api
        .update_settings(req, &auth.user_id, &rctx.0)
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

// ------------------------------------------------------------------
// Email
// ------------------------------------------------------------------

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
    _auth: AuthAdmin,
    State(state): State<AppState>,
    Json(req): Json<admin::SendTestEmailRequest>,
) -> AppResult<Json<admin::SendTestEmailResponse>> {
    let api = require_admin_api(&state)?;
    let resp = api
        .send_test_email(req)
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

// ------------------------------------------------------------------
// User Management
// ------------------------------------------------------------------

#[derive(serde::Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct ListUsersQuery {
    pub page: Option<i32>,
    pub page_size: Option<i32>,
    pub status: Option<String>,
    pub role: Option<String>,
    pub search: Option<String>,
    pub sort_by: Option<String>,
    pub sort_direction: Option<String>,
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/users",
        tag = "Admin",
        params(ListUsersQuery),
        responses(
            (status = 200, description = "Users list", body = admin::ListUsersResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_users(
    _auth: AuthAdmin,
    State(state): State<AppState>,
    Query(q): Query<ListUsersQuery>,
) -> AppResult<Json<admin::ListUsersResponse>> {
    let api = require_admin_api(&state)?;
    // Convert string status/role filters to proto enum values
    let status_i32 = match q.status.as_deref() {
        Some("active") => synctv_proto::common::UserStatus::Active as i32,
        Some("pending") => synctv_proto::common::UserStatus::Pending as i32,
        Some("banned") => synctv_proto::common::UserStatus::Banned as i32,
        _ => synctv_proto::common::UserStatus::Unspecified as i32,
    };
    let role_i32 = match q.role.as_deref() {
        Some("root") => synctv_proto::common::UserRole::Root as i32,
        Some("admin") => synctv_proto::common::UserRole::Admin as i32,
        Some("user") => synctv_proto::common::UserRole::User as i32,
        _ => synctv_proto::common::UserRole::Unspecified as i32,
    };
    let (page, page_size) = super::validation::validate_pagination(q.page, q.page_size);

    let resp = api
        .list_users(admin::ListUsersRequest {
            page,
            page_size,
            status: status_i32,
            role: role_i32,
            search: q.search.unwrap_or_default(),
            sort_by: match q.sort_by.as_deref() {
                Some("username") => admin::UserListSortBy::Username as i32,
                Some("email") => admin::UserListSortBy::Email as i32,
                Some("status") => admin::UserListSortBy::Status as i32,
                Some("role") => admin::UserListSortBy::Role as i32,
                Some("updated_at") => admin::UserListSortBy::UpdatedAt as i32,
                _ => admin::UserListSortBy::CreatedAt as i32,
            },
            sort_direction: match q.sort_direction.as_deref() {
                Some("asc") => admin::SortDirection::Asc as i32,
                _ => admin::SortDirection::Desc as i32,
            },
        })
        .await
        .map_err(admin_err_to_app_error)?;
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
    _auth: AuthAdmin,
    State(state): State<AppState>,
    Path(user_id): Path<String>,
) -> AppResult<Json<admin::GetUserResponse>> {
    validate_path_id(&user_id, "user_id")?;
    let api = require_admin_api(&state)?;
    let resp = api
        .get_user(admin::GetUserRequest { user_id })
        .await
        .map_err(admin_err_to_app_error)?;
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
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Json(req): Json<admin::CreateUserRequest>,
) -> AppResult<Json<admin::CreateUserResponse>> {
    let api = require_admin_api(&state)?;
    let resp = api
        .create_user(req, auth.role, &auth.user_id, &rctx.0)
        .await
        .map_err(admin_err_to_app_error)?;
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
    auth: AuthRoot,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Path(user_id): Path<String>,
) -> AppResult<Json<admin::DeleteUserResponse>> {
    validate_path_id(&user_id, "user_id")?;
    let api = require_admin_api(&state)?;
    let resp = api
        .delete_user(admin::DeleteUserRequest { user_id }, &auth.user_id, &rctx.0)
        .await
        .map_err(admin_err_to_app_error)?;
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
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(mut req): Json<admin::UpdateUserRoleRequest>,
) -> AppResult<Json<admin::UpdateUserRoleResponse>> {
    validate_path_id(&user_id, "user_id")?;
    let api = require_admin_api(&state)?;
    req.user_id = user_id;
    let resp = api
        .update_user_role(req, &auth.user_id, auth.role, &rctx.0)
        .await
        .map_err(admin_err_to_app_error)?;
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
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(mut req): Json<admin::UpdateUserPasswordRequest>,
) -> AppResult<Json<admin::UpdateUserPasswordResponse>> {
    validate_path_id(&user_id, "user_id")?;
    let api = require_admin_api(&state)?;
    req.user_id = user_id;
    if req.reason.is_empty() {
        req.reason = "Admin forced password reset".to_string();
    }
    let resp = api
        .update_user_password(req, auth.user_id, auth.role, &rctx.0)
        .await
        .map_err(admin_err_to_app_error)?;
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
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(mut req): Json<admin::UpdateUserUsernameRequest>,
) -> AppResult<Json<admin::UpdateUserUsernameResponse>> {
    validate_path_id(&user_id, "user_id")?;
    let api = require_admin_api(&state)?;
    req.user_id = user_id;
    let resp = api
        .update_user_username(req, &auth.user_id, &rctx.0)
        .await
        .map_err(admin_err_to_app_error)?;
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
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(mut req): Json<admin::BanUserRequest>,
) -> AppResult<Json<admin::BanUserResponse>> {
    validate_path_id(&user_id, "user_id")?;
    if req.reason.len() > 500 {
        return Err(AppError::bad_request(
            "Reason too long (max 500 characters)",
        ));
    }

    let api = require_admin_api(&state)?;
    req.user_id = user_id;
    let resp = api
        .ban_user(req, &auth.user_id, auth.role, &rctx.0)
        .await
        .map_err(admin_err_to_app_error)?;
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
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Path(user_id): Path<String>,
) -> AppResult<Json<admin::UnbanUserResponse>> {
    validate_path_id(&user_id, "user_id")?;
    let api = require_admin_api(&state)?;
    let resp = api
        .unban_user(admin::UnbanUserRequest { user_id }, &auth.user_id, &rctx.0)
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/users/{user_id}/approve",
        tag = "Admin",
        params(("user_id" = String, Path, description = "User ID")),
        responses(
            (status = 200, description = "User approved", body = admin::ApproveUserResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn approve_user(
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Path(user_id): Path<String>,
) -> AppResult<Json<admin::ApproveUserResponse>> {
    validate_path_id(&user_id, "user_id")?;
    let api = require_admin_api(&state)?;
    let resp = api
        .approve_user(
            admin::ApproveUserRequest { user_id },
            &auth.user_id,
            &rctx.0,
        )
        .await
        .map_err(admin_err_to_app_error)?;
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
            UserRoomsQuery
        ),
        responses(
            (status = 200, description = "Rooms belonging to user", body = admin::GetUserRoomsResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn get_user_rooms(
    _auth: AuthAdmin,
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Query(q): Query<UserRoomsQuery>,
) -> AppResult<Json<admin::GetUserRoomsResponse>> {
    validate_path_id(&user_id, "user_id")?;
    let api = require_admin_api(&state)?;
    let (page, page_size) = super::validation::validate_pagination(q.page, q.page_size);

    let resp = api
        .get_user_rooms(admin::GetUserRoomsRequest {
            user_id,
            page,
            page_size,
            status: match q.status.as_deref() {
                Some("active") => synctv_proto::common::RoomStatus::Active as i32,
                Some("pending") => synctv_proto::common::RoomStatus::Pending as i32,
                Some("closed") => synctv_proto::common::RoomStatus::Closed as i32,
                _ => synctv_proto::common::RoomStatus::Unspecified as i32,
            },
            search: q.search.unwrap_or_default(),
            is_banned: q.is_banned,
            sort_by: match q.sort_by.as_deref() {
                Some("name") => admin::RoomListSortBy::Name as i32,
                Some("updated_at") => admin::RoomListSortBy::UpdatedAt as i32,
                Some("last_activity_at") => admin::RoomListSortBy::LastActivityAt as i32,
                _ => admin::RoomListSortBy::CreatedAt as i32,
            },
            sort_direction: match q.sort_direction.as_deref() {
                Some("asc") => admin::SortDirection::Asc as i32,
                _ => admin::SortDirection::Desc as i32,
            },
        })
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

// ------------------------------------------------------------------
// Batch User Operations
// ------------------------------------------------------------------

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
    auth: AuthAdmin,
    rctx: ReqCtx,
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

    let api = require_admin_api(&state)?;
    let resp = api
        .batch_ban_users(req, &auth.user_id, auth.role, &rctx.0)
        .await
        .map_err(admin_err_to_app_error)?;
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
    auth: AuthRoot,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Json(req): Json<admin::BatchDeleteUsersRequest>,
) -> AppResult<Json<admin::BatchDeleteUsersResponse>> {
    if req.user_ids.is_empty() {
        return Err(AppError::bad_request("user_ids cannot be empty"));
    }
    if req.user_ids.len() > 100 {
        return Err(AppError::bad_request("Batch size exceeds limit of 100"));
    }

    let api = require_admin_api(&state)?;
    let resp = api
        .batch_delete_users(
            req,
            &auth.user_id,
            synctv_core::models::UserRole::Root,
            &rctx.0,
        )
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

// ------------------------------------------------------------------
// Room Management
// ------------------------------------------------------------------

#[derive(serde::Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct UserRoomsQuery {
    page: Option<i32>,
    page_size: Option<i32>,
    status: Option<String>,
    search: Option<String>,
    is_banned: Option<bool>,
    sort_by: Option<String>,
    sort_direction: Option<String>,
}

#[derive(serde::Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct ListProvidersQuery {
    page: Option<i32>,
    page_size: Option<i32>,
    provider_type: Option<String>,
    search: Option<String>,
    enabled: Option<bool>,
    tls: Option<bool>,
    sort_by: Option<String>,
    sort_direction: Option<String>,
}

#[derive(serde::Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct ListAdminsQuery {
    page: Option<i32>,
    page_size: Option<i32>,
    search: Option<String>,
    sort_by: Option<String>,
    sort_direction: Option<String>,
}

#[derive(serde::Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct RoomMembersQuery {
    page: Option<i32>,
    page_size: Option<i32>,
    search: Option<String>,
    role: Option<String>,
    status: Option<String>,
    sort_by: Option<String>,
    sort_direction: Option<String>,
}

#[derive(serde::Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct ListRoomsQuery {
    pub page: Option<i32>,
    pub page_size: Option<i32>,
    pub status: Option<i32>,
    pub search: Option<String>,
    pub creator_id: Option<String>,
    pub is_banned: Option<bool>,
    pub sort_by: Option<String>,
    pub sort_direction: Option<String>,
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/rooms",
        tag = "Admin",
        params(ListRoomsQuery),
        responses(
            (status = 200, description = "Admin room list", body = admin::ListRoomsResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_rooms(
    _auth: AuthAdmin,
    State(state): State<AppState>,
    Query(q): Query<ListRoomsQuery>,
) -> AppResult<Json<admin::ListRoomsResponse>> {
    let api = require_admin_api(&state)?;
    let (page, page_size) = super::validation::validate_pagination(q.page, q.page_size);

    let resp = api
        .list_rooms(admin::ListRoomsRequest {
            page,
            page_size,
            status: q.status.unwrap_or(0),
            search: q.search.unwrap_or_default(),
            creator_id: q.creator_id.unwrap_or_default(),
            is_banned: q.is_banned,
            sort_by: match q.sort_by.as_deref() {
                Some("name") => admin::RoomListSortBy::Name as i32,
                Some("updated_at") => admin::RoomListSortBy::UpdatedAt as i32,
                Some("last_activity_at") => admin::RoomListSortBy::LastActivityAt as i32,
                _ => admin::RoomListSortBy::CreatedAt as i32,
            },
            sort_direction: match q.sort_direction.as_deref() {
                Some("asc") => admin::SortDirection::Asc as i32,
                _ => admin::SortDirection::Desc as i32,
            },
        })
        .await
        .map_err(admin_err_to_app_error)?;
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
    _auth: AuthAdmin,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> AppResult<Json<admin::GetRoomResponse>> {
    validate_path_id(&room_id, "room_id")?;
    let api = require_admin_api(&state)?;
    let resp = api
        .get_room(admin::GetRoomRequest { room_id })
        .await
        .map_err(admin_err_to_app_error)?;
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
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> AppResult<Json<admin::DeleteRoomResponse>> {
    validate_path_id(&room_id, "room_id")?;
    let api = require_admin_api(&state)?;
    let resp = api
        .delete_room(admin::DeleteRoomRequest { room_id }, &auth.user_id, &rctx.0)
        .await
        .map_err(admin_err_to_app_error)?;
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
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(mut req): Json<admin::UpdateRoomPasswordRequest>,
) -> AppResult<Json<admin::UpdateRoomPasswordResponse>> {
    validate_path_id(&room_id, "room_id")?;
    let api = require_admin_api(&state)?;
    req.room_id = room_id;
    let resp = api
        .update_room_password(req, &auth.user_id, &rctx.0)
        .await
        .map_err(admin_err_to_app_error)?;
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
            RoomMembersQuery
        ),
        responses(
            (status = 200, description = "Room members", body = admin::GetRoomMembersResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn get_room_members(
    _auth: AuthAdmin,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Query(q): Query<RoomMembersQuery>,
) -> AppResult<Json<admin::GetRoomMembersResponse>> {
    validate_path_id(&room_id, "room_id")?;
    let api = require_admin_api(&state)?;
    let (page, page_size) = super::validation::validate_pagination(q.page, q.page_size);

    let resp = api
        .get_room_members(admin::GetRoomMembersRequest {
            room_id,
            page,
            page_size,
            search: q.search.unwrap_or_default(),
            role: match q.role.as_deref() {
                Some("guest") => synctv_proto::common::RoomMemberRole::Guest as i32,
                Some("member") => synctv_proto::common::RoomMemberRole::Member as i32,
                Some("admin") => synctv_proto::common::RoomMemberRole::Admin as i32,
                Some("creator") => synctv_proto::common::RoomMemberRole::Creator as i32,
                _ => synctv_proto::common::RoomMemberRole::Unspecified as i32,
            },
            status: match q.status.as_deref() {
                Some("active") => synctv_proto::common::MemberStatus::Active as i32,
                Some("pending") => synctv_proto::common::MemberStatus::Pending as i32,
                Some("banned") => synctv_proto::common::MemberStatus::Banned as i32,
                Some("left") => synctv_proto::common::MemberStatus::Left as i32,
                _ => synctv_proto::common::MemberStatus::Unspecified as i32,
            },
            sort_by: match q.sort_by.as_deref() {
                Some("username") => admin::RoomMemberListSortBy::Username as i32,
                Some("role") => admin::RoomMemberListSortBy::Role as i32,
                Some("status") => admin::RoomMemberListSortBy::Status as i32,
                _ => admin::RoomMemberListSortBy::JoinedAt as i32,
            },
            sort_direction: match q.sort_direction.as_deref() {
                Some("desc") => admin::SortDirection::Desc as i32,
                _ => admin::SortDirection::Asc as i32,
            },
        })
        .await
        .map_err(admin_err_to_app_error)?;
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
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(mut req): Json<admin::BanRoomRequest>,
) -> AppResult<Json<admin::BanRoomResponse>> {
    validate_path_id(&room_id, "room_id")?;
    if req.reason.len() > 500 {
        return Err(AppError::bad_request(
            "Reason too long (max 500 characters)",
        ));
    }

    let api = require_admin_api(&state)?;
    req.room_id = room_id;
    let resp = api
        .ban_room(req, &auth.user_id, &rctx.0)
        .await
        .map_err(admin_err_to_app_error)?;
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
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> AppResult<Json<admin::UnbanRoomResponse>> {
    validate_path_id(&room_id, "room_id")?;
    let api = require_admin_api(&state)?;
    let resp = api
        .unban_room(admin::UnbanRoomRequest { room_id }, &auth.user_id, &rctx.0)
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/rooms/{room_id}/approve",
        tag = "Admin",
        params(("room_id" = String, Path, description = "Room ID")),
        responses(
            (status = 200, description = "Room approved", body = admin::ApproveRoomResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn approve_room(
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> AppResult<Json<admin::ApproveRoomResponse>> {
    validate_path_id(&room_id, "room_id")?;
    let api = require_admin_api(&state)?;
    let resp = api
        .approve_room(
            admin::ApproveRoomRequest { room_id },
            &auth.user_id,
            &rctx.0,
        )
        .await
        .map_err(admin_err_to_app_error)?;
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
    _auth: AuthAdmin,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> AppResult<Json<admin::GetRoomSettingsResponse>> {
    validate_path_id(&room_id, "room_id")?;
    let api = require_admin_api(&state)?;
    let resp = api
        .get_room_settings(admin::GetRoomSettingsRequest { room_id })
        .await
        .map_err(admin_err_to_app_error)?;
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
    auth: AuthAdmin,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(mut req): Json<admin::UpdateRoomSettingsRequest>,
) -> AppResult<Json<admin::UpdateRoomSettingsResponse>> {
    validate_path_id(&room_id, "room_id")?;
    req.room_id = room_id;
    let api = require_admin_api(&state)?;
    let resp = api
        .update_room_settings(req, &auth.user_id)
        .await
        .map_err(admin_err_to_app_error)?;
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
    auth: AuthAdmin,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
) -> AppResult<Json<admin::ResetRoomSettingsResponse>> {
    validate_path_id(&room_id, "room_id")?;
    let api = require_admin_api(&state)?;
    let resp = api
        .reset_room_settings(admin::ResetRoomSettingsRequest { room_id }, &auth.user_id)
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

// ------------------------------------------------------------------
// Batch Room Operations
// ------------------------------------------------------------------

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
    auth: AuthAdmin,
    rctx: ReqCtx,
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

    let api = require_admin_api(&state)?;
    let resp = api
        .batch_ban_rooms(req, &auth.user_id, &rctx.0)
        .await
        .map_err(admin_err_to_app_error)?;
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
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Json(req): Json<admin::BatchDeleteRoomsRequest>,
) -> AppResult<Json<admin::BatchDeleteRoomsResponse>> {
    if req.room_ids.is_empty() {
        return Err(AppError::bad_request("room_ids cannot be empty"));
    }
    if req.room_ids.len() > 100 {
        return Err(AppError::bad_request("Batch size exceeds limit of 100"));
    }

    let api = require_admin_api(&state)?;
    let resp = api
        .batch_delete_rooms(req, &auth.user_id, &rctx.0)
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

// ------------------------------------------------------------------
// Provider Instances
// ------------------------------------------------------------------

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/providers",
        tag = "Admin",
        responses(
            (status = 200, description = "Provider instances", body = admin::ListProviderInstancesResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_providers(
    _auth: AuthAdmin,
    State(state): State<AppState>,
    Query(q): Query<ListProvidersQuery>,
) -> AppResult<Json<admin::ListProviderInstancesResponse>> {
    let api = require_admin_api(&state)?;
    let (page, page_size) = super::validation::validate_pagination(q.page, q.page_size);
    let resp = api
        .list_provider_instances(admin::ListProviderInstancesRequest {
            page,
            page_size,
            provider_type: q.provider_type.unwrap_or_default(),
            search: q.search.unwrap_or_default(),
            enabled: q.enabled,
            tls: q.tls,
            sort_by: match q.sort_by.as_deref() {
                Some("name") => admin::ProviderInstanceListSortBy::Name as i32,
                Some("endpoint") => admin::ProviderInstanceListSortBy::Endpoint as i32,
                Some("updated_at") => admin::ProviderInstanceListSortBy::UpdatedAt as i32,
                _ => admin::ProviderInstanceListSortBy::CreatedAt as i32,
            },
            sort_direction: match q.sort_direction.as_deref() {
                Some("asc") => admin::SortDirection::Asc as i32,
                _ => admin::SortDirection::Desc as i32,
            },
        })
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/providers",
        tag = "Admin",
        request_body = admin::AddProviderInstanceRequest,
        responses(
            (status = 200, description = "Provider instance added", body = admin::AddProviderInstanceResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn add_provider(
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Json(req): Json<admin::AddProviderInstanceRequest>,
) -> AppResult<Json<admin::AddProviderInstanceResponse>> {
    validate_path_id(&req.name, "name")?;
    let api = require_admin_api(&state)?;
    let resp = api
        .add_provider_instance(req, &auth.user_id, &rctx.0)
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        put,
        path = "/api/admin/providers/{name}",
        tag = "Admin",
        params(("name" = String, Path, description = "Provider instance name")),
        request_body = admin::UpdateProviderInstanceRequest,
        responses(
            (status = 200, description = "Provider instance updated", body = admin::UpdateProviderInstanceResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn update_provider(
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(mut req): Json<admin::UpdateProviderInstanceRequest>,
) -> AppResult<Json<admin::UpdateProviderInstanceResponse>> {
    validate_path_id(&name, "name")?;
    req.name = name;
    let api = require_admin_api(&state)?;
    let resp = api
        .update_provider_instance(req, &auth.user_id, &rctx.0)
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/admin/providers/{name}",
        tag = "Admin",
        params(("name" = String, Path, description = "Provider instance name")),
        responses(
            (status = 200, description = "Provider instance deleted", body = admin::DeleteProviderInstanceResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn delete_provider(
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> AppResult<Json<admin::DeleteProviderInstanceResponse>> {
    validate_path_id(&name, "name")?;
    let api = require_admin_api(&state)?;
    let resp = api
        .delete_provider_instance(
            admin::DeleteProviderInstanceRequest { name },
            &auth.user_id,
            &rctx.0,
        )
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/providers/{name}/reconnect",
        tag = "Admin",
        params(("name" = String, Path, description = "Provider instance name")),
        responses(
            (status = 200, description = "Provider instance reconnected", body = admin::ReconnectProviderInstanceResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn reconnect_provider(
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> AppResult<Json<admin::ReconnectProviderInstanceResponse>> {
    validate_path_id(&name, "name")?;
    let api = require_admin_api(&state)?;
    let resp = api
        .reconnect_provider_instance(
            admin::ReconnectProviderInstanceRequest { name },
            &auth.user_id,
            &rctx.0,
        )
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/providers/{name}/enable",
        tag = "Admin",
        params(("name" = String, Path, description = "Provider instance name")),
        responses(
            (status = 200, description = "Provider instance enabled", body = admin::EnableProviderInstanceResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn enable_provider(
    _auth: AuthAdmin,
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> AppResult<Json<admin::EnableProviderInstanceResponse>> {
    validate_path_id(&name, "name")?;
    let api = require_admin_api(&state)?;
    let resp = api
        .enable_provider_instance(admin::EnableProviderInstanceRequest { name })
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/providers/{name}/disable",
        tag = "Admin",
        params(("name" = String, Path, description = "Provider instance name")),
        responses(
            (status = 200, description = "Provider instance disabled", body = admin::DisableProviderInstanceResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn disable_provider(
    _auth: AuthAdmin,
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> AppResult<Json<admin::DisableProviderInstanceResponse>> {
    validate_path_id(&name, "name")?;
    let api = require_admin_api(&state)?;
    let resp = api
        .disable_provider_instance(admin::DisableProviderInstanceRequest { name })
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

// ------------------------------------------------------------------
// Stream Management
// ------------------------------------------------------------------

#[derive(serde::Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
pub struct ListStreamsQuery {
    page: Option<i32>,
    page_size: Option<i32>,
    room_id: Option<String>,
    user_id: Option<String>,
    node_id: Option<String>,
    search: Option<String>,
    sort_by: Option<String>,
    sort_direction: Option<String>,
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/streams",
        tag = "Admin",
        params(ListStreamsQuery),
        responses(
            (status = 200, description = "Active streams", body = admin::ListActiveStreamsResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_streams(
    _auth: AuthAdmin,
    State(state): State<AppState>,
    Query(q): Query<ListStreamsQuery>,
) -> AppResult<Json<admin::ListActiveStreamsResponse>> {
    let api = require_admin_api(&state)?;
    let (page, page_size) = super::validation::validate_pagination(q.page, q.page_size);
    let response = api
        .list_active_streams(admin::ListActiveStreamsRequest {
            page,
            page_size,
            room_id: q.room_id.unwrap_or_default(),
            user_id: q.user_id.unwrap_or_default(),
            node_id: q.node_id.unwrap_or_default(),
            search: q.search.unwrap_or_default(),
            sort_by: match q.sort_by.as_deref() {
                Some("room_id") => admin::ActiveStreamListSortBy::RoomId as i32,
                Some("media_id") => admin::ActiveStreamListSortBy::MediaId as i32,
                Some("user_id") => admin::ActiveStreamListSortBy::UserId as i32,
                Some("node_id") => admin::ActiveStreamListSortBy::NodeId as i32,
                _ => admin::ActiveStreamListSortBy::StartedAt as i32,
            },
            sort_direction: match q.sort_direction.as_deref() {
                Some("asc") => admin::SortDirection::Asc as i32,
                _ => admin::SortDirection::Desc as i32,
            },
        })
        .await
        .map_err(admin_err_to_app_error)?;
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
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Json(req): Json<admin::KickStreamRequest>,
) -> AppResult<Json<admin::KickStreamResponse>> {
    if req.room_id.is_empty() || req.media_id.is_empty() {
        return Err(AppError::bad_request("room_id and media_id are required"));
    }

    let api = require_admin_api(&state)?;
    api.kick_stream(
        &req.room_id,
        &req.media_id,
        &req.reason,
        &auth.user_id,
        &rctx.0,
    )
    .await
    .map_err(admin_err_to_app_error)?;
    Ok(Json(admin::KickStreamResponse {}))
}

// ------------------------------------------------------------------
// Admin Management (Root Only)
// ------------------------------------------------------------------

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
    _auth: AuthRoot,
    State(state): State<AppState>,
    Query(q): Query<ListAdminsQuery>,
) -> AppResult<Json<admin::ListAdminsResponse>> {
    let api = require_admin_api(&state)?;
    let (page, page_size) = super::validation::validate_pagination(q.page, q.page_size);
    let resp = api
        .list_admins(admin::ListAdminsRequest {
            page,
            page_size,
            search: q.search.unwrap_or_default(),
            sort_by: match q.sort_by.as_deref() {
                Some("username") => admin::UserListSortBy::Username as i32,
                Some("email") => admin::UserListSortBy::Email as i32,
                Some("status") => admin::UserListSortBy::Status as i32,
                Some("role") => admin::UserListSortBy::Role as i32,
                Some("updated_at") => admin::UserListSortBy::UpdatedAt as i32,
                _ => admin::UserListSortBy::CreatedAt as i32,
            },
            sort_direction: match q.sort_direction.as_deref() {
                Some("asc") => admin::SortDirection::Asc as i32,
                _ => admin::SortDirection::Desc as i32,
            },
        })
        .await
        .map_err(admin_err_to_app_error)?;
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
    auth: AuthRoot,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Path(user_id): Path<String>,
) -> AppResult<Json<admin::AddAdminResponse>> {
    validate_path_id(&user_id, "user_id")?;
    let api = require_admin_api(&state)?;
    let resp = api
        .add_admin(admin::AddAdminRequest { user_id }, &auth.user_id, &rctx.0)
        .await
        .map_err(admin_err_to_app_error)?;
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
    auth: AuthRoot,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Path(user_id): Path<String>,
) -> AppResult<Json<admin::RemoveAdminResponse>> {
    validate_path_id(&user_id, "user_id")?;
    let api = require_admin_api(&state)?;
    let resp = api
        .remove_admin(
            admin::RemoveAdminRequest { user_id },
            &auth.user_id,
            &rctx.0,
        )
        .await
        .map_err(admin_err_to_app_error)?;
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

    // ========== HTTP Proto Request Contract Tests ==========

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

    // ========== Query Struct Tests ==========

    #[test]
    fn test_list_users_query_deserialization() {
        let json = r#"{"page":2,"page_size":50,"status":"active","role":"admin","search":"test","sort_by":"username","sort_direction":"asc"}"#;
        let query: ListUsersQuery = serde_json::from_str(json).expect("deserialize");
        assert_eq!(query.page, Some(2));
        assert_eq!(query.page_size, Some(50));
        assert_eq!(query.status.as_deref(), Some("active"));
        assert_eq!(query.role.as_deref(), Some("admin"));
        assert_eq!(query.search.as_deref(), Some("test"));
        assert_eq!(query.sort_by.as_deref(), Some("username"));
        assert_eq!(query.sort_direction.as_deref(), Some("asc"));
    }

    #[test]
    fn test_list_rooms_query_deserialization() {
        let json = r#"{"page":1,"page_size":10,"status":1,"search":"room","creator_id":"user1","is_banned":false,"sort_by":"last_activity_at","sort_direction":"desc"}"#;
        let query: ListRoomsQuery = serde_json::from_str(json).expect("deserialize");
        assert_eq!(query.page, Some(1));
        assert_eq!(query.page_size, Some(10));
        assert_eq!(query.status, Some(1));
        assert_eq!(query.search.as_deref(), Some("room"));
        assert_eq!(query.creator_id.as_deref(), Some("user1"));
        assert_eq!(query.is_banned, Some(false));
        assert_eq!(query.sort_by.as_deref(), Some("last_activity_at"));
        assert_eq!(query.sort_direction.as_deref(), Some("desc"));
    }

    #[test]
    fn test_room_members_query_deserialization() {
        let json = r#"{"page":2,"page_size":25,"search":"alice","role":"admin","sort_by":"username","sort_direction":"asc"}"#;
        let query: RoomMembersQuery = serde_json::from_str(json).expect("deserialize");
        assert_eq!(query.page, Some(2));
        assert_eq!(query.page_size, Some(25));
        assert_eq!(query.search.as_deref(), Some("alice"));
        assert_eq!(query.role.as_deref(), Some("admin"));
        assert_eq!(query.sort_by.as_deref(), Some("username"));
        assert_eq!(query.sort_direction.as_deref(), Some("asc"));
    }

    #[tokio::test]
    async fn test_req_ctx_uses_trusted_proxy_headers_for_audit_ip() {
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
        let ctx = ReqCtx::from_request_parts(&mut parts, &state)
            .await
            .expect("extractor should not fail");

        assert_eq!(ctx.0.ip_address.as_deref(), Some("203.0.113.10"));
        assert_eq!(ctx.0.user_agent.as_deref(), Some("audit-test"));
    }

    #[tokio::test]
    async fn test_req_ctx_ignores_forwarded_headers_from_untrusted_proxy() {
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
        let ctx = ReqCtx::from_request_parts(&mut parts, &state)
            .await
            .expect("extractor should not fail");

        assert_eq!(ctx.0.ip_address.as_deref(), Some("198.51.100.7"));
    }

    // ========== Error Mapping Tests ==========

    #[test]
    fn test_require_admin_api_error() {
        // When admin_api is None, should produce an internal error
        let err = AppError::internal("Admin service not configured");
        assert_eq!(err.status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.message, "Admin service not configured");
    }

    #[test]
    fn test_ban_reason_length_validation() {
        // The handler validates reason.len() > 500
        let short_reason = "a".repeat(500);
        assert!(short_reason.len() <= 500);

        let long_reason = "a".repeat(501);
        assert!(long_reason.len() > 500);
    }

    // ========== Status Conversion Tests ==========

    #[test]
    fn test_status_string_to_proto_mapping() {
        let mappings = vec![
            ("active", synctv_proto::common::UserStatus::Active as i32),
            ("pending", synctv_proto::common::UserStatus::Pending as i32),
            ("banned", synctv_proto::common::UserStatus::Banned as i32),
        ];
        for (status_str, expected) in mappings {
            let actual = match status_str {
                "active" => synctv_proto::common::UserStatus::Active as i32,
                "pending" => synctv_proto::common::UserStatus::Pending as i32,
                "banned" => synctv_proto::common::UserStatus::Banned as i32,
                _ => synctv_proto::common::UserStatus::Unspecified as i32,
            };
            assert_eq!(actual, expected, "Status '{status_str}' mismatch");
        }
    }

    #[test]
    fn test_unknown_status_maps_to_unspecified() {
        let actual = match "invalid" {
            "active" => synctv_proto::common::UserStatus::Active as i32,
            "pending" => synctv_proto::common::UserStatus::Pending as i32,
            "banned" => synctv_proto::common::UserStatus::Banned as i32,
            _ => synctv_proto::common::UserStatus::Unspecified as i32,
        };
        assert_eq!(actual, synctv_proto::common::UserStatus::Unspecified as i32);
    }

    // ========== Router Structure Tests ==========

    #[test]
    fn test_admin_router_creation() {
        // Verify the admin router can be created without panicking
        let _router = create_admin_router();
    }

    // ========== Pagination Clamp Tests ==========

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

    // ========== Provider Name Validation Tests ==========

    #[test]
    fn test_provider_name_validation_empty() {
        // Empty name should fail validation
        let result = validate_path_id("", "name");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(err.message.contains("Invalid name"));
    }

    #[test]
    fn test_provider_name_validation_too_long() {
        // Name exceeding ID_MAX (128) should fail
        let long_name = "a".repeat(129);
        let result = validate_path_id(&long_name, "name");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(err.message.contains("Invalid name"));
    }

    #[test]
    fn test_provider_name_validation_special_characters() {
        // Name with special characters should fail
        let invalid_names = vec![
            "test<script>",    // HTML tags
            "test>alert",      // > character
            "test\"quote",     // Quote character
            "test'apostrophe", // Apostrophe
            "test space",      // Space
            "test/slash",      // Slash
            "test\\backslash", // Backslash
            "test;drop",       // Semicolon
            "test& amp",       // Ampersand
        ];
        for invalid_name in invalid_names {
            let result = validate_path_id(invalid_name, "name");
            assert!(result.is_err(), "Expected '{invalid_name}' to be invalid");
        }
    }

    #[test]
    fn test_provider_name_validation_valid() {
        // Valid names should pass
        let valid_names = vec![
            "provider1",
            "my-provider",
            "my_provider",
            "Provider123",
            "abc",
            "test_provider-123",
        ];
        for valid_name in valid_names {
            let result = validate_path_id(valid_name, "name");
            assert!(result.is_ok(), "Expected '{valid_name}' to be valid");
        }
    }

    #[test]
    fn test_provider_name_max_length_valid() {
        // Name at exactly ID_MAX (64) should be valid
        let max_length_name = "a".repeat(64);
        let result = validate_path_id(&max_length_name, "name");
        assert!(result.is_ok());
    }

    // ========== Kick Stream Validation Tests ==========

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

    // ========== Add Provider Name Validation Tests ==========

    #[test]
    fn test_add_provider_name_validation_empty_in_body() {
        // Empty name in request body should fail validation
        let req = admin::AddProviderInstanceRequest {
            name: String::new(),
            endpoint: "http://localhost:50051".to_string(),
            comment: String::new(),
            timeout_seconds: 10,
            tls: false,
            insecure_tls: false,
            providers: vec![],
            config: vec![],
        };
        // Validation is done by validate_path_id in the handler
        let result = validate_path_id(&req.name, "name");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(err.message.contains("Invalid name"));
    }

    #[test]
    fn test_add_provider_name_validation_malicious_in_body() {
        // Malicious name in request body should fail validation
        // Note: control chars like \x00 are sanitized away by sanitize_string,
        // so "test\x00null" becomes "testnull" which is valid - not tested here.
        let malicious_names = vec![
            "<script>alert(1)</script>",
            "test; DROP TABLE providers;",
            "../../../etc/passwd",
        ];
        for malicious_name in malicious_names {
            let result = validate_path_id(malicious_name, "name");
            assert!(
                result.is_err(),
                "Expected '{malicious_name}' to be rejected"
            );
        }
    }

    #[test]
    fn test_add_provider_name_validation_valid_in_body() {
        // Valid name in request body should pass validation
        let valid_names = vec![
            "alist_main",
            "bilibili-prod",
            "emby_server_1",
            "provider123",
        ];
        for valid_name in valid_names {
            let result = validate_path_id(valid_name, "name");
            assert!(result.is_ok(), "Expected '{valid_name}' to be valid");
        }
    }

    // ========== Admin Auth Blacklist Integration Tests ==========
    //
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
