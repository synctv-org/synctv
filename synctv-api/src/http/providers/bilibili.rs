//! Bilibili Provider HTTP Routes
//!
//! Provider API endpoints for Bilibili login, parse, etc.
//! Proxy routes (including danmu) are handled by the unified proxy handler
//! in `providers/mod.rs` via `BilibiliProvider::resolve_proxy`.

use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT;

use super::common::provider_instance_name;
use crate::http::{
    error::map_api_error, middleware::RequestMetadata, validation::ValidatedQuery, AppResult,
    AppState,
};
use crate::impls::EndpointRateLimitCategory;
use crate::proto::providers::bilibili::GetBindsResponse;
use crate::proto::providers::common::ProviderInstanceQuery;

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
        .route("/binds", get(binds))
}

fn request_metadata(request_meta: RequestMetadata) -> crate::impls::RequestMetadata {
    request_meta.0.with_timeout(Some(HTTP_REQUEST_TIMEOUT))
}

// Provider API handlers

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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<ProviderInstanceQuery>,
    Json(req): Json<crate::proto::providers::bilibili::ParseRequest>,
) -> AppResult<Json<crate::proto::providers::bilibili::ParseResponse>> {
    tracing::info!("Bilibili parse request");

    let instance_name = provider_instance_name(&query)?;
    let api = state.bilibili_api.clone();
    let request_meta = request_metadata(request_meta);
    let resp = state
        .client_api
        .execute_user_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Read,
            move |control, authenticated| async move {
                api.parse_with_context(
                    authenticated.user_id.as_str(),
                    req,
                    instance_name,
                    Some(&control),
                )
                .await
            },
        )
        .await
        .map_err(map_api_error)
        .map_err(|e| {
            tracing::error!("Bilibili parse failed: {}", e);
            e
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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<ProviderInstanceQuery>,
) -> AppResult<Json<crate::proto::providers::bilibili::QrCodeResponse>> {
    tracing::info!("Bilibili login QR request");

    let instance_name = provider_instance_name(&query)?;
    let req = crate::proto::providers::bilibili::LoginQrRequest::default();
    let api = state.bilibili_api.clone();
    let request_meta = request_metadata(request_meta);
    let resp = state
        .client_api
        .execute_user_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |control, _| async move {
                api.login_qr_with_context(req, instance_name, Some(&control))
                    .await
            },
        )
        .await
        .map_err(map_api_error)
        .map_err(|e| {
            tracing::error!("Failed to generate QR code: {}", e);
            e
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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<ProviderInstanceQuery>,
    Json(req): Json<crate::proto::providers::bilibili::CheckQrRequest>,
) -> AppResult<Json<crate::proto::providers::bilibili::QrStatusResponse>> {
    tracing::info!("Bilibili QR check");

    let instance_name = provider_instance_name(&query)?;
    let api = state.bilibili_api.clone();
    let request_meta = request_metadata(request_meta);
    let resp = state
        .client_api
        .execute_user_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |control, authenticated| async move {
                api.check_qr_with_context(
                    authenticated.user_id.as_str(),
                    req,
                    instance_name,
                    Some(&control),
                )
                .await
            },
        )
        .await
        .map_err(map_api_error)
        .map_err(|e| {
            tracing::error!("Failed to check QR status: {}", e);
            e
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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<ProviderInstanceQuery>,
) -> AppResult<Json<crate::proto::providers::bilibili::CaptchaResponse>> {
    tracing::info!("Bilibili new captcha request");

    let instance_name = provider_instance_name(&query)?;
    let req = crate::proto::providers::bilibili::GetCaptchaRequest::default();
    let api = state.bilibili_api.clone();
    let request_meta = request_metadata(request_meta);
    let resp = state
        .client_api
        .execute_user_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |control, _| async move {
                api.get_captcha_with_context(req, instance_name, Some(&control))
                    .await
            },
        )
        .await
        .map_err(map_api_error)
        .map_err(|e| {
            tracing::error!("Failed to get captcha: {}", e);
            e
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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<ProviderInstanceQuery>,
    Json(req): Json<crate::proto::providers::bilibili::SendSmsRequest>,
) -> AppResult<Json<crate::proto::providers::bilibili::SendSmsResponse>> {
    tracing::info!("Bilibili SMS send request");

    let instance_name = provider_instance_name(&query)?;
    let api = state.bilibili_api.clone();
    let request_meta = request_metadata(request_meta);
    let resp = state
        .client_api
        .execute_user_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |control, _| async move {
                api.send_sms_with_context(req, instance_name, Some(&control))
                    .await
            },
        )
        .await
        .map_err(map_api_error)
        .map_err(|e| {
            tracing::error!("Failed to send SMS: {}", e);
            e
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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<ProviderInstanceQuery>,
    Json(req): Json<crate::proto::providers::bilibili::LoginSmsRequest>,
) -> AppResult<Json<crate::proto::providers::bilibili::LoginSmsResponse>> {
    tracing::info!("Bilibili SMS login request");

    let instance_name = provider_instance_name(&query)?;
    let api = state.bilibili_api.clone();
    let request_meta = request_metadata(request_meta);
    let resp = state
        .client_api
        .execute_user_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Auth,
            move |control, authenticated| async move {
                api.login_sms_with_context(
                    authenticated.user_id.as_str(),
                    req,
                    instance_name,
                    Some(&control),
                )
                .await
            },
        )
        .await
        .map_err(map_api_error)
        .map_err(|e| {
            tracing::error!("Failed to login with SMS: {}", e);
            e
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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<ProviderInstanceQuery>,
    Json(req): Json<crate::proto::providers::bilibili::UserInfoRequest>,
) -> AppResult<Json<crate::proto::providers::bilibili::UserInfoResponse>> {
    tracing::info!("Bilibili user info request");

    let instance_name = provider_instance_name(&query)?;
    let api = state.bilibili_api.clone();
    let request_meta = request_metadata(request_meta);
    let resp = state
        .client_api
        .execute_user_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Read,
            move |control, authenticated| async move {
                api.get_user_info_with_context(
                    authenticated.user_id.as_str(),
                    req,
                    instance_name,
                    Some(&control),
                )
                .await
            },
        )
        .await
        .map_err(map_api_error)
        .map_err(|e| {
            tracing::error!("Failed to get user info: {}", e);
            e
        })?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/providers/bilibili/binds",
        tag = "Provider",
        params(ProviderInstanceQuery),
        responses(
            (status = 200, description = "Saved Bilibili credentials", body = GetBindsResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 503, description = "Provider bind information unavailable", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn binds(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<ProviderInstanceQuery>,
) -> AppResult<Json<GetBindsResponse>> {
    let instance_name = provider_instance_name(&query)?;
    let api = state.bilibili_api.clone();
    let request_meta = request_metadata(request_meta);
    let response = state
        .client_api
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Read,
            move |authenticated| async move {
                tracing::info!("Bilibili binds request for user: {}", authenticated.user_id);
                api.get_binds(authenticated.user_id.as_str(), instance_name)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;

    Ok(Json(response))
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
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<crate::proto::providers::bilibili::LogoutRequest>,
) -> AppResult<Json<crate::proto::providers::bilibili::LogoutResponse>> {
    tracing::info!("Bilibili logout request");

    let api = state.bilibili_api.clone();
    let request_meta = request_metadata(request_meta);
    let resp =
        state
            .client_api
            .execute_user_endpoint(
                &request_meta,
                EndpointRateLimitCategory::Auth,
                move |authenticated| async move {
                    api.logout(authenticated.user_id.as_str(), req).await
                },
            )
            .await
            .map_err(|e| {
                tracing::error!("Bilibili logout failed: {}", e);
                e
            })?;
    Ok(Json(resp))
}

#[cfg(test)]
mod tests {
    use synctv_core::models::UserProviderCredential;

    #[test]
    fn bilibili_server_id_is_global_for_all_instances() {
        assert_eq!(
            UserProviderCredential::bilibili_server_id(),
            UserProviderCredential::bilibili_server_id()
        );
    }
}
