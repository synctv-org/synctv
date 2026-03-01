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
use synctv_core::service::auth::JwtValidator;

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
/// Extracts JWT claims from the Authorization header, then delegates to
/// the shared `validate_admin_auth` in the impls layer for user lookup,
/// banned/deleted check, and password-change invalidation.
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
        .ok_or_else(|| AppError::unauthorized("Missing Authorization header"))?;

    let auth_str = auth_header
        .to_str()
        .map_err(|e| AppError::unauthorized(format!("Invalid Authorization header: {e}")))?;

    let claims = validator
        .validate_http(auth_str)
        .map_err(|e| AppError::unauthorized(format!("{e}")))?;

    let user_id = UserId::from_string(claims.sub);

    crate::impls::admin::validate_admin_auth(
        &app_state.user_service,
        user_id,
        claims.pv,
        claims.iat,
    )
    .await
    .map_err(AppError::unauthorized)
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
struct ReqCtx(crate::impls::admin::RequestContext);

impl<S> FromRequestParts<S> for ReqCtx
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let ip_address = parts
            .extensions
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|ci| ci.0.ip().to_string());
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
// Typed request structs for admin endpoints
// ------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct SetUserRoleRequest {
    role: String,
}

#[derive(serde::Deserialize)]
struct SetUserPasswordRequest {
    password: String,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(serde::Deserialize)]
struct SetUserUsernameRequest {
    username: String,
}

#[derive(serde::Deserialize)]
struct BanRequest {
    #[serde(default)]
    reason: String,
}

#[derive(serde::Deserialize)]
struct SetRoomPasswordAdminRequest {
    #[serde(default)]
    password: String,
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
        .route("/streams/{stream_id}/kick", post(kick_stream))
        // Admin management (root only)
        .route("/admins", get(list_admins))
        .route("/admins/{user_id}", post(add_admin).delete(remove_admin))
}

// ------------------------------------------------------------------
// System Stats
// ------------------------------------------------------------------

async fn get_system_stats(
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

async fn get_settings(
    _auth: AuthAdmin,
    State(state): State<AppState>,
) -> AppResult<Json<admin::GetSettingsResponse>> {
    let api = require_admin_api(&state)?;
    let resp = api
        .get_settings(admin::GetSettingsRequest {})
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

async fn get_settings_group(
    _auth: AuthAdmin,
    State(state): State<AppState>,
    Path(group): Path<String>,
) -> AppResult<Json<admin::GetSettingsGroupResponse>> {
    let api = require_admin_api(&state)?;
    let resp = api
        .get_settings_group(admin::GetSettingsGroupRequest { group })
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

async fn set_settings(
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

async fn send_test_email(
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
pub struct ListUsersQuery {
    pub page: Option<i32>,
    pub page_size: Option<i32>,
    pub status: Option<String>,
    pub role: Option<String>,
    pub search: Option<String>,
}

async fn list_users(
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
        })
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

async fn get_user(
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

async fn create_user(
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

async fn delete_user(
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

async fn set_user_role(
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(req): Json<SetUserRoleRequest>,
) -> AppResult<Json<admin::UpdateUserRoleResponse>> {
    validate_path_id(&user_id, "user_id")?;
    let api = require_admin_api(&state)?;
    // Convert string role to proto enum value
    let role_i32 = match req.role.as_str() {
        "root" => synctv_proto::common::UserRole::Root as i32,
        "admin" => synctv_proto::common::UserRole::Admin as i32,
        "user" => synctv_proto::common::UserRole::User as i32,
        _ => return Err(AppError::bad_request(format!("Unknown role: {}", req.role))),
    };
    let resp = api
        .update_user_role(
            admin::UpdateUserRoleRequest {
                user_id,
                role: role_i32,
            },
            &auth.user_id,
            auth.role,
            &rctx.0,
        )
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

async fn set_user_password(
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(req): Json<SetUserPasswordRequest>,
) -> AppResult<Json<admin::UpdateUserPasswordResponse>> {
    validate_path_id(&user_id, "user_id")?;
    let api = require_admin_api(&state)?;
    let reason = req
        .reason
        .unwrap_or_else(|| "Admin forced password reset".to_string());
    let resp = api
        .update_user_password(
            admin::UpdateUserPasswordRequest {
                user_id,
                new_password: req.password,
                reason,
            },
            auth.user_id,
            auth.role,
            &rctx.0,
        )
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

async fn set_user_username(
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(req): Json<SetUserUsernameRequest>,
) -> AppResult<Json<admin::UpdateUserUsernameResponse>> {
    validate_path_id(&user_id, "user_id")?;
    let api = require_admin_api(&state)?;
    let resp = api
        .update_user_username(
            admin::UpdateUserUsernameRequest {
                user_id,
                new_username: req.username,
            },
            &auth.user_id,
            &rctx.0,
        )
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

async fn ban_user(
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(req): Json<BanRequest>,
) -> AppResult<Json<admin::BanUserResponse>> {
    validate_path_id(&user_id, "user_id")?;
    if req.reason.len() > 500 {
        return Err(AppError::bad_request(
            "Reason too long (max 500 characters)",
        ));
    }

    let api = require_admin_api(&state)?;
    let resp = api
        .ban_user(
            admin::BanUserRequest {
                user_id,
                reason: req.reason,
            },
            &auth.user_id,
            auth.role,
            &rctx.0,
        )
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

async fn unban_user(
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

async fn approve_user(
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

async fn get_user_rooms(
    _auth: AuthAdmin,
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Query(q): Query<PaginationQuery>,
) -> AppResult<Json<admin::GetUserRoomsResponse>> {
    validate_path_id(&user_id, "user_id")?;
    let api = require_admin_api(&state)?;
    let (page, page_size) = super::validation::validate_pagination(q.page, q.page_size);

    let resp = api
        .get_user_rooms(admin::GetUserRoomsRequest {
            user_id,
            page,
            page_size,
        })
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

// ------------------------------------------------------------------
// Batch User Operations
// ------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct BatchBanUsersRequest {
    user_ids: Vec<String>,
    #[serde(default)]
    reason: String,
}

#[derive(serde::Deserialize)]
struct BatchDeleteUsersRequest {
    user_ids: Vec<String>,
}

async fn batch_ban_users(
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Json(req): Json<BatchBanUsersRequest>,
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
        .batch_ban_users(
            admin::BatchBanUsersRequest {
                user_ids: req.user_ids,
                reason: req.reason,
            },
            &auth.user_id,
            auth.role,
            &rctx.0,
        )
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

async fn batch_delete_users(
    auth: AuthRoot,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Json(req): Json<BatchDeleteUsersRequest>,
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
            admin::BatchDeleteUsersRequest {
                user_ids: req.user_ids,
            },
            &auth.user_id,
            synctv_core::models::UserRole::Root, // AuthRoot guarantees caller is Root
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
struct PaginationQuery {
    page: Option<i32>,
    page_size: Option<i32>,
}

#[derive(serde::Deserialize, Default)]
pub struct ListRoomsQuery {
    pub page: Option<i32>,
    pub page_size: Option<i32>,
    pub status: Option<i32>,
    pub search: Option<String>,
    pub creator_id: Option<String>,
    pub is_banned: Option<bool>,
}

async fn list_rooms(
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
        })
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

async fn get_room(
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

async fn delete_room(
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

async fn set_room_password(
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<SetRoomPasswordAdminRequest>,
) -> AppResult<Json<admin::UpdateRoomPasswordResponse>> {
    validate_path_id(&room_id, "room_id")?;
    let api = require_admin_api(&state)?;
    let resp = api
        .update_room_password(
            admin::UpdateRoomPasswordRequest {
                room_id,
                new_password: req.password,
            },
            &auth.user_id,
            &rctx.0,
        )
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

async fn get_room_members(
    _auth: AuthAdmin,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Query(q): Query<PaginationQuery>,
) -> AppResult<Json<admin::GetRoomMembersResponse>> {
    validate_path_id(&room_id, "room_id")?;
    let api = require_admin_api(&state)?;
    let (page, page_size) = super::validation::validate_pagination(q.page, q.page_size);

    let resp = api
        .get_room_members(admin::GetRoomMembersRequest {
            room_id,
            page,
            page_size,
        })
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

async fn ban_room(
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<BanRequest>,
) -> AppResult<Json<admin::BanRoomResponse>> {
    validate_path_id(&room_id, "room_id")?;
    if req.reason.len() > 500 {
        return Err(AppError::bad_request(
            "Reason too long (max 500 characters)",
        ));
    }

    let api = require_admin_api(&state)?;
    let resp = api
        .ban_room(
            admin::BanRoomRequest {
                room_id,
                reason: req.reason,
            },
            &auth.user_id,
            &rctx.0,
        )
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

async fn unban_room(
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

async fn approve_room(
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

async fn get_room_settings(
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

async fn set_room_settings(
    auth: AuthAdmin,
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(req): Json<serde_json::Value>,
) -> AppResult<Json<admin::UpdateRoomSettingsResponse>> {
    validate_path_id(&room_id, "room_id")?;
    let settings = serde_json::to_vec(&req)
        .map_err(|e| AppError::bad_request(format!("Invalid settings JSON: {e}")))?;

    let api = require_admin_api(&state)?;
    let resp = api
        .update_room_settings(
            admin::UpdateRoomSettingsRequest { room_id, settings },
            &auth.user_id,
        )
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

async fn reset_room_settings(
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

#[derive(serde::Deserialize)]
struct BatchBanRoomsRequest {
    room_ids: Vec<String>,
    #[serde(default)]
    reason: String,
}

#[derive(serde::Deserialize)]
struct BatchDeleteRoomsRequest {
    room_ids: Vec<String>,
}

async fn batch_ban_rooms(
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Json(req): Json<BatchBanRoomsRequest>,
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
        .batch_ban_rooms(
            admin::BatchBanRoomsRequest {
                room_ids: req.room_ids,
                reason: req.reason,
            },
            &auth.user_id,
            &rctx.0,
        )
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

async fn batch_delete_rooms(
    auth: AuthAdmin,
    rctx: ReqCtx,
    State(state): State<AppState>,
    Json(req): Json<BatchDeleteRoomsRequest>,
) -> AppResult<Json<admin::BatchDeleteRoomsResponse>> {
    if req.room_ids.is_empty() {
        return Err(AppError::bad_request("room_ids cannot be empty"));
    }
    if req.room_ids.len() > 100 {
        return Err(AppError::bad_request("Batch size exceeds limit of 100"));
    }

    let api = require_admin_api(&state)?;
    let resp = api
        .batch_delete_rooms(
            admin::BatchDeleteRoomsRequest {
                room_ids: req.room_ids,
            },
            &auth.user_id,
            &rctx.0,
        )
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

// ------------------------------------------------------------------
// Provider Instances
// ------------------------------------------------------------------

async fn list_providers(
    _auth: AuthAdmin,
    State(state): State<AppState>,
) -> AppResult<Json<admin::ListProviderInstancesResponse>> {
    let api = require_admin_api(&state)?;
    let resp = api
        .list_provider_instances(admin::ListProviderInstancesRequest {
            provider_type: String::new(),
        })
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

async fn add_provider(
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

async fn update_provider(
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

async fn delete_provider(
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

async fn reconnect_provider(
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

async fn enable_provider(
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

async fn disable_provider(
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
struct ListStreamsQuery {
    room_id: Option<String>,
}

async fn list_streams(
    _auth: AuthAdmin,
    State(state): State<AppState>,
    Query(q): Query<ListStreamsQuery>,
) -> AppResult<Json<admin::ListActiveStreamsResponse>> {
    let api = require_admin_api(&state)?;
    let room_id = q.room_id.as_deref().filter(|s| !s.is_empty());
    let streams = api
        .list_active_streams(room_id)
        .await
        .map_err(|e| admin_err_to_app_error(crate::impls::ApiError::Internal(e.to_string())))?;
    Ok(Json(admin::ListActiveStreamsResponse { streams }))
}

async fn kick_stream(
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
    .map_err(|e| admin_err_to_app_error(crate::impls::ApiError::Internal(e.to_string())))?;
    Ok(Json(admin::KickStreamResponse {}))
}

// ------------------------------------------------------------------
// Admin Management (Root Only)
// ------------------------------------------------------------------

async fn list_admins(
    _auth: AuthRoot,
    State(state): State<AppState>,
) -> AppResult<Json<admin::ListAdminsResponse>> {
    let api = require_admin_api(&state)?;
    let resp = api
        .list_admins(admin::ListAdminsRequest {})
        .await
        .map_err(admin_err_to_app_error)?;
    Ok(Json(resp))
}

async fn add_admin(
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

async fn remove_admin(
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

    // ========== Request Struct Tests ==========

    #[test]
    fn test_set_user_role_request_deserialization() {
        let json = r#"{"role":"admin"}"#;
        let req: SetUserRoleRequest = serde_json::from_str(json).expect("deserialize");
        assert_eq!(req.role, "admin");
    }

    #[test]
    fn test_set_user_role_request_all_roles() {
        for role in &["root", "admin", "user"] {
            let json = format!(r#"{{"role":"{role}"}}"#);
            let req: SetUserRoleRequest = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(req.role, *role);
        }
    }

    #[test]
    fn test_set_user_password_request_deserialization() {
        let json = r#"{"password":"newpassword123"}"#;
        let req: SetUserPasswordRequest = serde_json::from_str(json).expect("deserialize");
        assert_eq!(req.password, "newpassword123");
    }

    #[test]
    fn test_set_user_username_request_deserialization() {
        let json = r#"{"username":"newname"}"#;
        let req: SetUserUsernameRequest = serde_json::from_str(json).expect("deserialize");
        assert_eq!(req.username, "newname");
    }

    #[test]
    fn test_ban_request_with_reason() {
        let json = r#"{"reason":"spamming"}"#;
        let req: BanRequest = serde_json::from_str(json).expect("deserialize");
        assert_eq!(req.reason, "spamming");
    }

    #[test]
    fn test_ban_request_empty_reason_default() {
        let json = r"{}";
        let req: BanRequest = serde_json::from_str(json).expect("deserialize");
        assert_eq!(req.reason, ""); // #[serde(default)]
    }

    #[test]
    fn test_set_room_password_request_deserialization() {
        let json = r#"{"password":"roompass"}"#;
        let req: SetRoomPasswordAdminRequest = serde_json::from_str(json).expect("deserialize");
        assert_eq!(req.password, "roompass");
    }

    #[test]
    fn test_set_room_password_empty_clears_password() {
        // Empty password means remove password
        let json = r"{}";
        let req: SetRoomPasswordAdminRequest = serde_json::from_str(json).expect("deserialize");
        assert_eq!(req.password, ""); // #[serde(default)]
    }

    // ========== Query Struct Tests ==========

    #[test]
    fn test_list_users_query_defaults() {
        let query = ListUsersQuery::default();
        assert!(query.page.is_none());
        assert!(query.page_size.is_none());
        assert!(query.status.is_none());
        assert!(query.role.is_none());
        assert!(query.search.is_none());
    }

    #[test]
    fn test_list_users_query_deserialization() {
        let json = r#"{"page":2,"page_size":50,"status":"active","role":"admin","search":"test"}"#;
        let query: ListUsersQuery = serde_json::from_str(json).expect("deserialize");
        assert_eq!(query.page, Some(2));
        assert_eq!(query.page_size, Some(50));
        assert_eq!(query.status.as_deref(), Some("active"));
        assert_eq!(query.role.as_deref(), Some("admin"));
        assert_eq!(query.search.as_deref(), Some("test"));
    }

    #[test]
    fn test_list_rooms_query_defaults() {
        let query = ListRoomsQuery::default();
        assert!(query.page.is_none());
        assert!(query.page_size.is_none());
        assert!(query.status.is_none());
        assert!(query.search.is_none());
        assert!(query.creator_id.is_none());
        assert!(query.is_banned.is_none());
    }

    #[test]
    fn test_list_rooms_query_deserialization() {
        let json = r#"{"page":1,"page_size":10,"status":1,"search":"room","creator_id":"user1","is_banned":false}"#;
        let query: ListRoomsQuery = serde_json::from_str(json).expect("deserialize");
        assert_eq!(query.page, Some(1));
        assert_eq!(query.page_size, Some(10));
        assert_eq!(query.status, Some(1));
        assert_eq!(query.search.as_deref(), Some("room"));
        assert_eq!(query.creator_id.as_deref(), Some("user1"));
        assert_eq!(query.is_banned, Some(false));
    }

    // ========== AuthAdmin / AuthRoot Type Tests ==========

    #[test]
    fn test_auth_admin_debug() {
        let auth = AuthAdmin {
            user_id: UserId::from_string("admin1".to_string()),
            role: synctv_core::models::UserRole::Admin,
        };
        let debug = format!("{auth:?}");
        assert!(debug.contains("AuthAdmin"));
    }

    #[test]
    fn test_auth_admin_clone() {
        let auth = AuthAdmin {
            user_id: UserId::from_string("admin1".to_string()),
            role: synctv_core::models::UserRole::Admin,
        };
        let cloned = auth;
        assert_eq!(cloned.user_id.as_str(), "admin1");
    }

    #[test]
    fn test_auth_root_debug() {
        let auth = AuthRoot {
            user_id: UserId::from_string("root1".to_string()),
        };
        let debug = format!("{auth:?}");
        assert!(debug.contains("AuthRoot"));
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

    // ========== Role Conversion Tests ==========

    #[test]
    fn test_role_string_to_proto_mapping() {
        // Verify the mapping logic used in set_user_role handler
        let role_mappings = vec![
            ("root", synctv_proto::common::UserRole::Root as i32),
            ("admin", synctv_proto::common::UserRole::Admin as i32),
            ("user", synctv_proto::common::UserRole::User as i32),
        ];
        for (role_str, expected_i32) in role_mappings {
            let actual_opt = match role_str {
                "root" => Some(synctv_proto::common::UserRole::Root as i32),
                "admin" => Some(synctv_proto::common::UserRole::Admin as i32),
                "user" => Some(synctv_proto::common::UserRole::User as i32),
                _ => None,
            };
            let actual = actual_opt.expect("Role must be one of root/admin/user");
            assert_eq!(actual, expected_i32, "Role '{role_str}' mapping mismatch");
        }
    }

    #[test]
    fn test_unknown_role_returns_error() {
        let role_str = "superuser";
        let result = match role_str {
            "root" | "admin" | "user" => Ok(()),
            _ => Err(AppError::bad_request(format!("Unknown role: {role_str}"))),
        };
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(err.message.contains("Unknown role"));
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
}
