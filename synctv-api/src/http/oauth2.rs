//! `OAuth2` HTTP handlers
//!
//! Provides `OAuth2` login endpoints for GitHub, Google, Microsoft, Discord

use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

use synctv_core::models::{
    oauth2_client::OAuth2CallbackRequest,
    id::UserId,
    user::User,
    settings::{groups, server},
};
use synctv_core::service::JwtService;

use super::{middleware::AuthUser, AppResult, AppState};

/// `OAuth2` authorization request query params
#[derive(Debug, Deserialize)]
pub struct OAuth2AuthQuery {
    pub redirect: Option<String>,
}

/// `OAuth2` authorization URL response
#[derive(Debug, Serialize)]
pub struct AuthUrlResponse {
    pub url: String,
    pub state: String,
}

/// `OAuth2` callback response (JSON)
#[derive(Debug, Serialize)]
pub struct OAuth2CallbackJsonResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Get `OAuth2` authorization URL
///
/// GET /api/oauth2/:instance/authorize?redirect=<url>
pub async fn get_authorize_url(
    State(state): State<AppState>,
    Path(instance_name): Path<String>,
    Query(params): Query<OAuth2AuthQuery>,
) -> AppResult<Json<AuthUrlResponse>> {
    // Check if OAuth2 service exists
    let oauth2_service = state.oauth2_service.as_ref().ok_or_else(|| {
        super::AppError::bad_request("OAuth2 is not configured on this server")
    })?;

    // Get authorization URL
    let (url, state_token) = oauth2_service
        .get_authorization_url(&instance_name, params.redirect)
        .await
        .map_err(|e| {
            error!("Failed to get authorization URL: {}", e);
            super::AppError::internal_server_error("Failed to get authorization URL")
        })?;

    debug!("Generated OAuth2 authorization URL for {}", instance_name);

    Ok(Json(AuthUrlResponse { url, state: state_token }))
}

/// `OAuth2` callback handler
///
/// GET /api/oauth2/:instance/callback?code=xxx&state=xxx
pub async fn oauth2_callback_get(
    State(state): State<AppState>,
    Path(instance_name): Path<String>,
    Query(params): Query<OAuth2CallbackRequest>,
) -> AppResult<Json<OAuth2CallbackJsonResponse>> {
    handle_oauth2_callback(state, instance_name, params, None).await
}

/// `OAuth2` callback handler (POST version)
///
/// POST /api/oauth2/:instance/callback
/// Body: { "code": "xxx", "state": "xxx" }
pub async fn oauth2_callback_post(
    State(state): State<AppState>,
    Path(instance_name): Path<String>,
    Json(req): Json<OAuth2CallbackRequest>,
) -> AppResult<Json<OAuth2CallbackJsonResponse>> {
    handle_oauth2_callback(state, instance_name, req, None).await
}

/// Handle `OAuth2` callback (shared logic)
async fn handle_oauth2_callback(
    state: AppState,
    instance_name: String,
    req: OAuth2CallbackRequest,
    expected_user_id: Option<UserId>,
) -> AppResult<Json<OAuth2CallbackJsonResponse>> {
    // Check if OAuth2 service exists
    let oauth2_service = state.oauth2_service.as_ref().ok_or_else(|| {
        super::AppError::bad_request("OAuth2 is not configured on this server")
    })?;

    // Verify state
    let oauth_state = oauth2_service.verify_state(&req.state).await.map_err(|e| {
        warn!("OAuth2 state verification failed: {}", e);
        super::AppError::bad_request("Invalid or expired OAuth2 state")
    })?;

    // Verify instance matches
    if oauth_state.instance_name != instance_name {
        return Err(super::AppError::bad_request("Instance mismatch"));
    }

    // Exchange code for user info (with PKCE verifier from stored state)
    let (user_info, _provider_type) = oauth2_service
        .exchange_code_for_user_info(&instance_name, &req.code, &oauth_state.pkce_verifier)
        .await
        .map_err(|e| {
            error!("Failed to exchange OAuth2 code: {}", e);
            super::AppError::bad_request("Failed to exchange authorization code")
        })?;

    debug!(
        "Got OAuth2 user info: {} provider {}",
        user_info.username,
        user_info.provider.as_str()
    );

    // Use bind_user_id from OAuth state if present, or fall back to expected_user_id parameter
    let effective_user_id = oauth_state.bind_user_id.clone().or(expected_user_id);

    // Find or create user
    let user_id = match effective_user_id {
        // Binding to existing user
        Some(uid) => uid,
        // Login flow - find or create user
        None => {
            // Check if user exists with this OAuth2 provider
            if let Some(uid) = oauth2_service
                .find_user_by_provider(&user_info.provider, &user_info.provider_user_id)
                .await
                .map_err(|e| {
                    error!("Failed to query OAuth2 user: {}", e);
                    super::AppError::internal_server_error("Database error")
                })?
            {
                info!(
                    "Found existing user {} via OAuth2 provider {}",
                    uid.as_str(),
                    user_info.provider.as_str()
                );
                uid
            } else {
                // User doesn't exist - check if signup is enabled via settings
                let settings_service = state.settings_service.as_ref()
                    .ok_or_else(|| super::AppError::internal_server_error("Settings service not available"))?;

                let server_settings = settings_service.get(groups::SERVER).await
                    .map_err(|_| super::AppError::internal_server_error("Failed to get settings"))?;

                let signup_enabled = server_settings.parse_json()
                    .ok()
                    .and_then(|json| json.get(server::SIGNUP_ENABLED).cloned())
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true); // Default to true if not set

                if !signup_enabled {
                    return Err(super::AppError::bad_request("User registration is disabled"));
                }

                let new_user = state
                    .user_service
                    .create_or_load_by_oauth2(
                        &user_info.provider,
                        &user_info.provider_user_id,
                        &user_info.username,
                        user_info.email.as_deref(),
                    )
                    .await
                    .map_err(|e| {
                        error!("Failed to create user via OAuth2: {}", e);
                        super::AppError::internal_server_error("Failed to create user")
                    })?;

                info!(
                    "Created new user {} via OAuth2 provider {}",
                    new_user.id.as_str(),
                    user_info.provider.as_str()
                );
                new_user.id
            }
        }
    };

    // Save OAuth2 provider mapping
    oauth2_service
        .upsert_user_provider(&user_id, &user_info.provider, &user_info.provider_user_id, &user_info)
        .await
        .map_err(|e| {
            error!("Failed to save OAuth2 mapping: {}", e);
            super::AppError::internal_server_error("Failed to save OAuth2 mapping")
        })?;

    // Generate JWT tokens
    let user = state.user_service.get_user(&user_id).await.map_err(|e| {
        error!("Failed to get user: {}", e);
        super::AppError::internal_server_error("Failed to get user info")
    })?;

    // Check if user is deleted or banned (generic message to prevent user enumeration)
    if user.is_deleted() || user.status == synctv_core::models::UserStatus::Banned {
        return Ok(Json(OAuth2CallbackJsonResponse {
            token: None,
            redirect: oauth_state.redirect_url,
            message: Some("Authentication failed".to_string()),
        }));
    }

    // Generate tokens
    let (access_token, _refresh_token) = generate_tokens(&state.jwt_service, &user).await?;

    info!(
        "OAuth2 login successful for user {} via {}",
        user_id.as_str(),
        user_info.provider.as_str()
    );

    Ok(Json(OAuth2CallbackJsonResponse {
        token: Some(access_token),
        redirect: oauth_state.redirect_url,
        message: None,
    }))
}

/// Bind `OAuth2` provider to authenticated user
///
/// POST /api/oauth2/:instance/bind
///
/// Requires authentication. The authenticated user's ID is stored in the OAuth state
/// so the callback can associate the provider with the correct user.
pub async fn bind_provider(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(instance_name): Path<String>,
    Json(req): Json<OAuth2AuthQuery>,
) -> AppResult<Json<AuthUrlResponse>> {
    // Check if OAuth2 service exists
    let oauth2_service = state.oauth2_service.as_ref().ok_or_else(|| {
        super::AppError::bad_request("OAuth2 is not configured on this server")
    })?;

    // Get authorization URL (bind flow stores user_id in state for the callback)
    let (url, state_token) = oauth2_service
        .get_authorization_url_with_user(&instance_name, req.redirect, Some(auth.user_id.clone()))
        .await
        .map_err(|e| {
            error!("Failed to get authorization URL: {}", e);
            super::AppError::internal_server_error("Failed to get authorization URL")
        })?;

    debug!(
        "Generated OAuth2 bind URL for {} (user: {})",
        instance_name,
        auth.user_id.as_str(),
    );

    Ok(Json(AuthUrlResponse { url, state: state_token }))
}

/// Unbind `OAuth2` provider from authenticated user
///
/// DELETE /api/oauth2/:instance/bind
pub async fn unbind_provider(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(instance_name): Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    let oauth2_service = state.oauth2_service.as_ref().ok_or_else(|| {
        super::AppError::bad_request("OAuth2 is not configured on this server")
    })?;

    // Parse provider from instance name
    let provider = synctv_core::models::oauth2_client::OAuth2Provider::from_str_name(&instance_name)
        .ok_or_else(|| super::AppError::bad_request(format!("Unknown OAuth2 provider: {instance_name}")))?;

    // Unbind all bindings for this provider
    let deleted = oauth2_service
        .unlink_provider_all(&auth.user_id, &provider)
        .await
        .map_err(|e| {
            error!("Failed to unbind OAuth2 provider: {}", e);
            super::AppError::internal_server_error("Failed to unbind provider")
        })?;

    if !deleted {
        return Err(super::AppError::bad_request("No binding found for this provider"));
    }

    info!(
        "User {} unbound OAuth2 provider {}",
        auth.user_id.as_str(),
        instance_name,
    );

    Ok(Json(serde_json::json!({
        "success": true,
        "provider": instance_name,
    })))
}

/// Get list of available `OAuth2` provider instances
///
/// GET /api/oauth2/providers
///
/// Returns the configured `OAuth2` provider instances that clients can use
/// for login or account binding. No authentication required.
///
/// Response: JSON array of objects, each with:
/// - `name`: Instance name (e.g., "github", "logto1") used in `OAuth2` URLs
/// - `type`: Provider type (e.g., "github", "google", "logto", "oidc")
pub async fn list_providers(
    State(state): State<AppState>,
) -> AppResult<Json<Vec<serde_json::Value>>> {
    let oauth2_service = state.oauth2_service.as_ref().ok_or_else(|| {
        super::AppError::bad_request("OAuth2 is not configured on this server")
    })?;

    let instances = oauth2_service.list_available_instances().await;

    let providers: Vec<serde_json::Value> = instances
        .into_iter()
        .map(|(name, provider_type)| {
            serde_json::json!({
                "name": name,
                "type": provider_type.as_str(),
            })
        })
        .collect();

    Ok(Json(providers))
}

/// Generate JWT tokens for user
async fn generate_tokens(
    jwt_service: &JwtService,
    user: &User,
) -> Result<(String, String), super::AppError> {
    use synctv_core::service::TokenType;

    let access_token = jwt_service
        .sign_token(&user.id, TokenType::Access)
        .map_err(|e| {
            error!("Failed to sign access token: {}", e);
            super::AppError::internal_server_error("Failed to generate access token")
        })?;

    let refresh_token = jwt_service
        .sign_token(&user.id, TokenType::Refresh)
        .map_err(|e| {
            error!("Failed to sign refresh token: {}", e);
            super::AppError::internal_server_error("Failed to generate refresh token")
        })?;

    Ok((access_token, refresh_token))
}
