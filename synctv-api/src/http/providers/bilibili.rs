//! Bilibili Provider HTTP Routes
//!
//! Provider API endpoints for Bilibili login, parse, etc.
//! Proxy routes (including danmu) are handled by the unified proxy handler
//! in `providers/mod.rs` via `BilibiliProvider::resolve_proxy`.

use axum::{
    extract::State,
    routing::post,
    Json, Router,
};

use crate::http::{
    middleware::AuthUser, provider_common::provider_instance_name, validation::ValidatedQuery,
    AppError, AppResult, AppState,
};
use crate::proto::client::ProviderInstanceQuery;

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
        .route("/me", post(user_info))
}

// ------------------------------------------------------------------
// Provider API handlers
// ------------------------------------------------------------------

/// Parse Bilibili URL (uses stored cookies)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/bilibili/parse",
        tag = "Provider",
        params(ProviderInstanceQuery),
        request_body = crate::proto::providers::bilibili::ParseRequest,
        responses(
            (status = 200, description = "Bilibili media parsed", body = crate::proto::providers::bilibili::ParseResponse),
            (status = 400, description = "Invalid parse request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn parse(
    auth: AuthUser,
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<ProviderInstanceQuery>,
    Json(req): Json<crate::proto::providers::bilibili::ParseRequest>,
) -> AppResult<Json<crate::proto::providers::bilibili::ParseResponse>> {
    tracing::info!("Bilibili parse request");

    let instance_name = provider_instance_name(&query)?;
    let api = &state.bilibili_api;
    let resp = api
        .parse(&auth.user_id.to_string(), req, instance_name)
        .await
        .map_err(|e| {
            tracing::error!("Bilibili parse failed: {}", e);
            AppError::from(e)
        })?;
    Ok(Json(resp))
}

/// Generate Bilibili QR code for login
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/bilibili/login/qr/generate",
        tag = "Provider",
        params(ProviderInstanceQuery),
        responses(
            (status = 200, description = "Bilibili login QR code generated", body = crate::proto::providers::bilibili::QrCodeResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn login_qr(
    _auth: AuthUser,
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<ProviderInstanceQuery>,
) -> AppResult<Json<crate::proto::providers::bilibili::QrCodeResponse>> {
    tracing::info!("Bilibili login QR request");

    let instance_name = provider_instance_name(&query)?;
    let api = &state.bilibili_api;
    let req = crate::proto::providers::bilibili::LoginQrRequest::default();

    let resp = api.login_qr(req, instance_name).await.map_err(|e| {
        tracing::error!("Failed to generate QR code: {}", e);
        AppError::from(e)
    })?;
    Ok(Json(resp))
}

/// Check Bilibili QR code login status (persists cookies on success)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/bilibili/login/qr/check",
        tag = "Provider",
        params(ProviderInstanceQuery),
        request_body = crate::proto::providers::bilibili::CheckQrRequest,
        responses(
            (status = 200, description = "Bilibili QR login status", body = crate::proto::providers::bilibili::QrStatusResponse),
            (status = 400, description = "Invalid QR check request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn qr_check(
    auth: AuthUser,
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<ProviderInstanceQuery>,
    Json(req): Json<crate::proto::providers::bilibili::CheckQrRequest>,
) -> AppResult<Json<crate::proto::providers::bilibili::QrStatusResponse>> {
    tracing::info!("Bilibili QR check");

    let instance_name = provider_instance_name(&query)?;
    let api = &state.bilibili_api;

    let resp = api
        .check_qr(&auth.user_id.to_string(), req, instance_name)
        .await
        .map_err(|e| {
            tracing::error!("Failed to check QR status: {}", e);
            AppError::from(e)
        })?;
    Ok(Json(resp))
}

/// Get captcha for SMS login
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/bilibili/login/captcha",
        tag = "Provider",
        params(ProviderInstanceQuery),
        responses(
            (status = 200, description = "Bilibili captcha challenge", body = crate::proto::providers::bilibili::CaptchaResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn new_captcha(
    _auth: AuthUser,
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<ProviderInstanceQuery>,
) -> AppResult<Json<crate::proto::providers::bilibili::CaptchaResponse>> {
    tracing::info!("Bilibili new captcha request");

    let instance_name = provider_instance_name(&query)?;
    let api = &state.bilibili_api;
    let req = crate::proto::providers::bilibili::GetCaptchaRequest::default();

    let resp = api.get_captcha(req, instance_name).await.map_err(|e| {
        tracing::error!("Failed to get captcha: {}", e);
        AppError::from(e)
    })?;
    Ok(Json(resp))
}

/// Send SMS verification code
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/bilibili/login/sms/send",
        tag = "Provider",
        params(ProviderInstanceQuery),
        request_body = crate::proto::providers::bilibili::SendSmsRequest,
        responses(
            (status = 200, description = "Bilibili SMS sent", body = crate::proto::providers::bilibili::SendSmsResponse),
            (status = 400, description = "Invalid SMS request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn sms_send(
    _auth: AuthUser,
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<ProviderInstanceQuery>,
    Json(req): Json<crate::proto::providers::bilibili::SendSmsRequest>,
) -> AppResult<Json<crate::proto::providers::bilibili::SendSmsResponse>> {
    tracing::info!("Bilibili SMS send request");

    let instance_name = provider_instance_name(&query)?;
    let api = &state.bilibili_api;

    let resp = api.send_sms(req, instance_name).await.map_err(|e| {
        tracing::error!("Failed to send SMS: {}", e);
        AppError::from(e)
    })?;
    Ok(Json(resp))
}

/// Login with SMS code (persists cookies on success)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/bilibili/login/sms/login",
        tag = "Provider",
        params(ProviderInstanceQuery),
        request_body = crate::proto::providers::bilibili::LoginSmsRequest,
        responses(
            (status = 200, description = "Bilibili SMS login succeeded", body = crate::proto::providers::bilibili::LoginSmsResponse),
            (status = 400, description = "Invalid SMS login request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn sms_login(
    auth: AuthUser,
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<ProviderInstanceQuery>,
    Json(req): Json<crate::proto::providers::bilibili::LoginSmsRequest>,
) -> AppResult<Json<crate::proto::providers::bilibili::LoginSmsResponse>> {
    tracing::info!("Bilibili SMS login request");

    let instance_name = provider_instance_name(&query)?;
    let api = &state.bilibili_api;

    let resp = api
        .login_sms(&auth.user_id.to_string(), req, instance_name)
        .await
        .map_err(|e| {
            tracing::error!("Failed to login with SMS: {}", e);
            AppError::from(e)
        })?;
    Ok(Json(resp))
}

/// Get Bilibili user info (uses stored cookies)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/bilibili/me",
        tag = "Provider",
        params(ProviderInstanceQuery),
        request_body = crate::proto::providers::bilibili::UserInfoRequest,
        responses(
            (status = 200, description = "Bilibili account info", body = crate::proto::providers::bilibili::UserInfoResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn user_info(
    auth: AuthUser,
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<ProviderInstanceQuery>,
    Json(req): Json<crate::proto::providers::bilibili::UserInfoRequest>,
) -> AppResult<Json<crate::proto::providers::bilibili::UserInfoResponse>> {
    tracing::info!("Bilibili user info request");

    let instance_name = provider_instance_name(&query)?;
    let api = &state.bilibili_api;

    let resp = api
        .get_user_info(&auth.user_id.to_string(), req, instance_name)
        .await
        .map_err(|e| {
            tracing::error!("Failed to get user info: {}", e);
            AppError::from(e)
        })?;
    Ok(Json(resp))
}

/// Logout (delete stored credential)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/bilibili/logout",
        tag = "Provider",
        request_body = crate::proto::providers::bilibili::LogoutRequest,
        responses(
            (status = 200, description = "Bilibili credential removed", body = crate::proto::providers::bilibili::LogoutResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn logout(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<crate::proto::providers::bilibili::LogoutRequest>,
) -> AppResult<Json<crate::proto::providers::bilibili::LogoutResponse>> {
    tracing::info!("Bilibili logout request");

    let api = &state.bilibili_api;

    let resp = api
        .logout(&auth.user_id.to_string(), req)
        .await
        .map_err(|e| {
            tracing::error!("Bilibili logout failed: {}", e);
            AppError::from(e)
        })?;
    Ok(Json(resp))
}

#[cfg(test)]
mod tests {
    use synctv_core::models::UserProviderCredential;

    #[test]
    fn bilibili_server_id_scopes_to_requested_instance() {
        let scoped = UserProviderCredential::bilibili_server_id(Some("bili-main"));
        assert_ne!(scoped, UserProviderCredential::bilibili_server_id(None));
    }
}
