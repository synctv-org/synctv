//! Bilibili Provider HTTP Routes
//!
//! Provider API endpoints for Bilibili login, parse, etc.
//! Proxy routes (including danmu) are handled by the unified proxy handler
//! in `providers/mod.rs` via `BilibiliProvider::resolve_proxy`.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;

use crate::http::{middleware::AuthUser, provider_common::InstanceQuery, AppError, AppState};

fn user_info_server_id(instance_name: Option<&str>) -> String {
    synctv_core::models::UserProviderCredential::bilibili_server_id(instance_name)
}

/// Bilibili endpoints that authenticate, issue challenges, or mutate stored credentials.
pub fn bilibili_auth_routes() -> Router<AppState> {
    Router::new()
        .route("/login/qr/generate", post(login_qr))
        .route("/login/qr/check", post(qr_check))
        .route("/login/captcha", post(new_captcha))
        .route("/login/sms/send", post(sms_send))
        .route("/login/sms/login", post(sms_login))
        .route("/logout", post(logout))
}

/// Bilibili read/query endpoints.
pub fn bilibili_read_routes() -> Router<AppState> {
    Router::new()
        .route("/parse", post(parse))
        .route("/me", get(user_info))
}

// ------------------------------------------------------------------
// Provider API handlers
// ------------------------------------------------------------------

/// Parse Bilibili URL (uses stored cookies)
async fn parse(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
    Json(req): Json<crate::proto::providers::bilibili::ParseRequest>,
) -> axum::response::Response {
    tracing::info!("Bilibili parse request");

    let api = &state.bilibili_api;

    match api
        .parse(&auth.user_id.to_string(), req, query.as_deref())
        .await
    {
        Ok(resp) => (StatusCode::OK, Json(json!(resp))).into_response(),
        Err(e) => {
            tracing::error!("Bilibili parse failed: {}", e);
            AppError::from(e).into_response()
        }
    }
}

/// Generate Bilibili QR code for login
async fn login_qr(
    _auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
) -> axum::response::Response {
    tracing::info!("Bilibili login QR request");

    let api = &state.bilibili_api;
    let req = crate::proto::providers::bilibili::LoginQrRequest::default();

    match api.login_qr(req, query.as_deref()).await {
        Ok(resp) => (StatusCode::OK, Json(json!(resp))).into_response(),
        Err(e) => {
            tracing::error!("Failed to generate QR code: {}", e);
            AppError::from(e).into_response()
        }
    }
}

/// Check Bilibili QR code login status (persists cookies on success)
async fn qr_check(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
    Json(req): Json<crate::proto::providers::bilibili::CheckQrRequest>,
) -> axum::response::Response {
    tracing::info!("Bilibili QR check");

    let api = &state.bilibili_api;

    match api
        .check_qr(&auth.user_id.to_string(), req, query.as_deref())
        .await
    {
        Ok(resp) => (StatusCode::OK, Json(json!(resp))).into_response(),
        Err(e) => {
            tracing::error!("Failed to check QR status: {}", e);
            AppError::from(e).into_response()
        }
    }
}

/// Get captcha for SMS login
async fn new_captcha(
    _auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
) -> axum::response::Response {
    tracing::info!("Bilibili new captcha request");

    let api = &state.bilibili_api;
    let req = crate::proto::providers::bilibili::GetCaptchaRequest::default();

    match api.get_captcha(req, query.as_deref()).await {
        Ok(resp) => (StatusCode::OK, Json(json!(resp))).into_response(),
        Err(e) => {
            tracing::error!("Failed to get captcha: {}", e);
            AppError::from(e).into_response()
        }
    }
}

/// Send SMS verification code
async fn sms_send(
    _auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
    Json(req): Json<crate::proto::providers::bilibili::SendSmsRequest>,
) -> axum::response::Response {
    tracing::info!("Bilibili SMS send request");

    let api = &state.bilibili_api;

    match api.send_sms(req, query.as_deref()).await {
        Ok(resp) => (StatusCode::OK, Json(json!(resp))).into_response(),
        Err(e) => {
            tracing::error!("Failed to send SMS: {}", e);
            AppError::from(e).into_response()
        }
    }
}

/// Login with SMS code (persists cookies on success)
async fn sms_login(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
    Json(req): Json<crate::proto::providers::bilibili::LoginSmsRequest>,
) -> axum::response::Response {
    tracing::info!("Bilibili SMS login request");

    let api = &state.bilibili_api;

    match api
        .login_sms(&auth.user_id.to_string(), req, query.as_deref())
        .await
    {
        Ok(resp) => (StatusCode::OK, Json(json!(resp))).into_response(),
        Err(e) => {
            tracing::error!("Failed to login with SMS: {}", e);
            AppError::from(e).into_response()
        }
    }
}

/// Get Bilibili user info (uses stored cookies)
async fn user_info(
    auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
) -> impl IntoResponse {
    tracing::info!("Bilibili user info request");

    let api = &state.bilibili_api;
    let req = crate::proto::providers::bilibili::UserInfoRequest {
        server_id: user_info_server_id(query.as_deref()),
        instance_name: query.instance_name.clone().unwrap_or_default(),
    };

    match api
        .get_user_info(&auth.user_id.to_string(), req, query.as_deref())
        .await
    {
        Ok(resp) => (StatusCode::OK, Json(json!(resp))).into_response(),
        Err(e) => {
            tracing::error!("Failed to get user info: {}", e);
            AppError::from(e).into_response()
        }
    }
}

/// Logout (delete stored credential)
async fn logout(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<crate::proto::providers::bilibili::LogoutRequest>,
) -> impl IntoResponse {
    tracing::info!("Bilibili logout request");

    let api = &state.bilibili_api;

    match api.logout(&auth.user_id.to_string(), req).await {
        Ok(resp) => (StatusCode::OK, Json(json!(resp))).into_response(),
        Err(e) => {
            tracing::error!("Bilibili logout failed: {}", e);
            AppError::from(e).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::user_info_server_id;
    use synctv_core::models::UserProviderCredential;

    #[test]
    fn user_info_server_id_defaults_without_instance_name() {
        assert_eq!(
            user_info_server_id(None),
            UserProviderCredential::BILIBILI_SERVER_ID
        );
        assert_eq!(
            user_info_server_id(Some("   ")),
            UserProviderCredential::BILIBILI_SERVER_ID
        );
    }

    #[test]
    fn user_info_server_id_scopes_to_requested_instance() {
        let scoped = user_info_server_id(Some("bili-main"));
        assert_eq!(
            scoped,
            UserProviderCredential::bilibili_server_id(Some("bili-main"))
        );
        assert_ne!(scoped, UserProviderCredential::BILIBILI_SERVER_ID);
    }
}
