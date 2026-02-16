//! `OAuth2` HTTP handlers
//!
//! Provides `OAuth2` endpoints for frontend-driven OAuth2 flow
//! Uses proto-generated types for request/response consistency with gRPC

use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;
use tracing::{debug, error, info};

use synctv_proto::client::{
    GetAuthorizationUrlResponse,
    GetAuthorizationUrlForBindResponse,
    ExchangeAuthorizationCodeRequest, ExchangeAuthorizationCodeResponse,
    ListAvailableProvidersResponse,
    UnlinkProviderResponse,
    GetLinkedProvidersResponse,
};

use super::{middleware::AuthUser, AppResult, AppState, error::map_api_error};

/// Query params for get authorization URL (converted to proto request)
#[derive(Debug, Deserialize)]
pub struct GetAuthUrlQuery {
    pub redirect_url: Option<String>,
}

/// Query params for unlink provider (converted to proto request)
#[derive(Debug, Deserialize)]
pub struct UnlinkProviderQuery {
    pub provider_user_id: Option<String>,
}

/// Get `OAuth2` authorization URL for login flow
///
/// GET /api/oauth2/:provider/authorize?redirect_url=<url>
pub async fn get_authorize_url(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(params): Query<GetAuthUrlQuery>,
) -> AppResult<Json<GetAuthorizationUrlResponse>> {
    let oauth2_api = state.oauth2_api.as_ref().ok_or_else(|| {
        super::AppError::bad_request("OAuth2 is not configured on this server")
    })?;

    let (authorization_url, state_token) = oauth2_api
        .get_authorization_url(&provider, params.redirect_url)
        .await
        .map_err(|e| {
            error!("Failed to get authorization URL: {}", e);
            map_api_error(e)
        })?;

    debug!("Generated OAuth2 authorization URL for provider: {}", provider);

    Ok(Json(GetAuthorizationUrlResponse {
        authorization_url,
        state: state_token,
    }))
}

/// Exchange authorization code for JWT token (frontend-driven flow)
///
/// POST /api/oauth2/:provider/exchange
/// Body: { "code": "xxx", "state": "xxx" }
pub async fn exchange_authorization_code(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Json(req): Json<ExchangeAuthorizationCodeRequest>,
) -> AppResult<Json<ExchangeAuthorizationCodeResponse>> {
    let oauth2_api = state.oauth2_api.as_ref().ok_or_else(|| {
        super::AppError::bad_request("OAuth2 is not configured on this server")
    })?;

    let result = oauth2_api
        .exchange_authorization_code(&provider, &req.code, &req.state)
        .await
        .map_err(|e| {
            error!("Failed to exchange authorization code: {}", e);
            map_api_error(e)
        })?;

    info!(
        "OAuth2 exchange successful for provider: {} (is_bind: {})",
        provider, result.is_bind
    );

    Ok(Json(ExchangeAuthorizationCodeResponse {
        access_token: result.access_token.unwrap_or_default(),
        refresh_token: result.refresh_token.unwrap_or_default(),
        expires_in: result.expires_in,
        user_info: result.user_info,
        redirect_url: result.redirect_url.unwrap_or_default(),
        is_bind: result.is_bind,
    }))
}

/// Get authorization URL for binding OAuth2 provider to authenticated user
///
/// GET /api/oauth2/:provider/bind?redirect_url=<url>
///
/// Requires authentication. The frontend then redirects to the OAuth2 provider,
/// receives code/state, and calls exchange endpoint which will bind the provider.
pub async fn get_bind_authorize_url(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(params): Query<GetAuthUrlQuery>,
) -> AppResult<Json<GetAuthorizationUrlForBindResponse>> {
    let oauth2_api = state.oauth2_api.as_ref().ok_or_else(|| {
        super::AppError::bad_request("OAuth2 is not configured on this server")
    })?;

    let (authorization_url, state_token) = oauth2_api
        .get_authorization_url_for_bind(&auth.user_id, &provider, params.redirect_url)
        .await
        .map_err(|e| {
            error!("Failed to get authorization URL for bind: {}", e);
            map_api_error(e)
        })?;

    debug!(
        "Generated OAuth2 bind URL for provider: {} (user: {})",
        provider,
        auth.user_id.as_str()
    );

    Ok(Json(GetAuthorizationUrlForBindResponse {
        authorization_url,
        state: state_token,
    }))
}

/// Unlink OAuth2 provider from authenticated user
///
/// DELETE /api/oauth2/:provider/unlink?provider_user_id=<optional>
pub async fn unlink_provider(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(params): Query<UnlinkProviderQuery>,
) -> AppResult<Json<UnlinkProviderResponse>> {
    let oauth2_api = state.oauth2_api.as_ref().ok_or_else(|| {
        super::AppError::bad_request("OAuth2 is not configured on this server")
    })?;

    let result = oauth2_api
        .unlink_provider(&auth.user_id, &provider, params.provider_user_id.as_deref())
        .await
        .map_err(|e| {
            error!("Failed to unlink OAuth2 provider: {}", e);
            map_api_error(e)
        })?;

    if !result.success {
        return Err(super::AppError::bad_request("No binding found for this provider"));
    }

    info!(
        "User {} unlinked OAuth2 provider: {}",
        auth.user_id.as_str(),
        provider
    );

    Ok(Json(UnlinkProviderResponse {
        success: result.success,
        removed_count: result.removed_count,
    }))
}

/// List all available OAuth2 provider instances
///
/// GET /api/oauth2/providers
///
/// Returns the configured OAuth2 provider instances that clients can use
/// for login or account binding. No authentication required.
pub async fn list_available_providers(
    State(state): State<AppState>,
) -> AppResult<Json<ListAvailableProvidersResponse>> {
    let oauth2_api = state.oauth2_api.as_ref().ok_or_else(|| {
        super::AppError::bad_request("OAuth2 is not configured on this server")
    })?;

    let providers = oauth2_api
        .list_available_providers()
        .await
        .map_err(|e| {
            error!("Failed to list available providers: {}", e);
            map_api_error(e)
        })?;

    let response = providers
        .into_iter()
        .map(|p| p.into())
        .collect();

    Ok(Json(ListAvailableProvidersResponse {
        providers: response,
    }))
}

/// Get linked OAuth2 providers for authenticated user
///
/// GET /api/oauth2/linked
///
/// Requires authentication.
pub async fn get_linked_providers(
    auth: AuthUser,
    State(state): State<AppState>,
) -> AppResult<Json<GetLinkedProvidersResponse>> {
    let oauth2_api = state.oauth2_api.as_ref().ok_or_else(|| {
        super::AppError::bad_request("OAuth2 is not configured on this server")
    })?;

    let providers = oauth2_api
        .get_linked_providers(&auth.user_id)
        .await
        .map_err(|e| {
            error!("Failed to get linked providers: {}", e);
            map_api_error(e)
        })?;

    let response = providers
        .into_iter()
        .map(|p| p.into())
        .collect();

    Ok(Json(GetLinkedProvidersResponse {
        providers: response,
    }))
}
