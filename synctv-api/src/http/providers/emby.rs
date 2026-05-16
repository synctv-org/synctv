//! Emby Provider HTTP Routes
//!
//! Provider API endpoints for Emby login, listing, etc.
//! Playback proxy routes are handled by the unified proxy handler in
//! `providers/mod.rs`, while thumbnail fetches use an authenticated route that
//! resolves Emby credentials server-side.

use axum::{
    extract::{Path, Query, RawQuery, State},
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use futures::FutureExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use synctv_core::models::ProviderCredential;
use synctv_core::provider::proxy::ProxyAction;
use synctv_core::proxy_signature::{ProxySigningKey, ProxyUrlClaims};

use crate::http::{
    error::map_api_error, middleware::RequestMetadata, validation::ProtoQuery, AppError, AppResult,
    AppState,
};
use crate::impls::ApiError;
use crate::impls::EndpointRateLimitCategory;
use crate::proto::providers::common::ProviderInstanceQuery;
use crate::proto::providers::emby::GetBindsResponse;

use super::common::{
    apply_provider_instance_name, execute_provider_user_endpoint,
    execute_provider_user_endpoint_with_control, provider_instance_name, provider_request_metadata,
};

const DEFAULT_THUMBNAIL_HEIGHT: u32 = 300;
const MAX_THUMBNAIL_DIMENSION: u32 = 1920;
const THUMBNAIL_ROUTE_PREFIX: &str = "/api/providers/emby/thumbnail/";
const THUMBNAIL_SIGNATURE_PROVIDER: &str = "emby-thumbnail";

#[derive(Debug, Deserialize)]
pub(crate) struct ThumbnailQuery {
    server_id: String,
    #[serde(default)]
    credential_owner_id: Option<String>,
    #[serde(default)]
    max_height: Option<u32>,
    #[serde(default)]
    max_width: Option<u32>,
}

#[derive(Clone, Copy)]
struct ThumbnailSignatureScope<'a> {
    item_id: &'a str,
    server_id: &'a str,
    credential_owner_id: &'a str,
    max_height: u32,
    max_width: u32,
}

fn clamp_thumbnail_dimension(value: Option<u32>, default: u32) -> u32 {
    value.unwrap_or(default).min(MAX_THUMBNAIL_DIMENSION)
}

fn resolve_thumbnail_query(
    query: &ThumbnailQuery,
) -> Result<(&str, Option<&str>, u32, u32), AppError> {
    let server_id = query.server_id.trim();
    if server_id.is_empty() {
        return Err(AppError::bad_request("server_id must not be empty"));
    }
    let credential_owner_id = query.credential_owner_id.as_deref().map(str::trim);

    Ok((
        server_id,
        credential_owner_id.filter(|owner_id| !owner_id.is_empty()),
        clamp_thumbnail_dimension(query.max_height, DEFAULT_THUMBNAIL_HEIGHT),
        clamp_thumbnail_dimension(query.max_width, 0),
    ))
}

fn build_thumbnail_proxy_action(
    item_id: &str,
    host: &str,
    api_key: &str,
    max_height: u32,
    max_width: u32,
) -> Result<ProxyAction, ApiError> {
    let thumbnail_path = if max_width > 0 {
        format!(
            "/Items/{item_id}/Images/Primary?maxHeight={max_height}&maxWidth={max_width}&quality=90"
        )
    } else {
        format!("/Items/{item_id}/Images/Primary?maxHeight={max_height}&quality=90")
    };

    let url = synctv_core::provider::emby::emby_server_url(host, &thumbnail_path)
        .map_err(|error| ApiError::Internal(error.to_string()))?;

    Ok(ProxyAction::FetchAndForward {
        url,
        headers: std::collections::HashMap::from([(
            "X-Emby-Token".to_string(),
            api_key.to_string(),
        )]),
        range_header: None,
    })
}

fn build_thumbnail_proxy_action_from_credential(
    item_id: &str,
    credential: &ProviderCredential,
    max_height: u32,
    max_width: u32,
) -> Result<ProxyAction, ApiError> {
    if item_id.trim().is_empty() {
        return Err(ApiError::InvalidInput(
            "item_id must not be empty".to_string(),
        ));
    }

    match credential {
        ProviderCredential::Emby { host, api_key, .. } => {
            build_thumbnail_proxy_action(item_id.trim(), host, api_key, max_height, max_width)
        }
        _ => Err(ApiError::Internal(
            "Stored credential is not an Emby credential".to_string(),
        )),
    }
}

fn thumbnail_signature_version(scope: ThumbnailSignatureScope<'_>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(scope.item_id.as_bytes());
    hasher.update([0]);
    hasher.update(scope.server_id.as_bytes());
    hasher.update([0]);
    hasher.update(scope.credential_owner_id.as_bytes());
    hasher.update([0]);
    hasher.update(scope.max_height.to_be_bytes());
    hasher.update(scope.max_width.to_be_bytes());
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    encoded
}

fn build_signed_thumbnail_query(
    signing_key: &ProxySigningKey,
    room_id: &str,
    user_id: &str,
    scope: ThumbnailSignatureScope<'_>,
    expires_at: i64,
) -> String {
    let claims = ProxyUrlClaims {
        provider: THUMBNAIL_SIGNATURE_PROVIDER.to_string(),
        version: thumbnail_signature_version(scope),
        room_id: room_id.to_string(),
        user_id: user_id.to_string(),
        expires_at,
        target_url: None,
    };
    signing_key.build_signed_query(&claims)
}

fn thumbnail_signature_present(raw_query: &str) -> bool {
    raw_query.split('&').any(|pair| pair.starts_with("sig="))
}

fn verify_signed_thumbnail_access(
    signing_key: &ProxySigningKey,
    auth_user_id: &str,
    raw_query: &str,
    scope: ThumbnailSignatureScope<'_>,
) -> Result<String, AppError> {
    let claims = signing_key
        .parse_and_verify_query(
            raw_query,
            THUMBNAIL_SIGNATURE_PROVIDER,
            &thumbnail_signature_version(scope),
        )
        .map_err(|_| AppError::unauthorized("Invalid thumbnail signature"))?;

    if claims.user_id != auth_user_id {
        return Err(AppError::forbidden(
            "Thumbnail URL is not valid for this user",
        ));
    }

    Ok(claims.room_id)
}

fn authorize_thumbnail_request(
    signing_key: &ProxySigningKey,
    auth_user_id: &str,
    public_auth_user_id: &str,
    raw_query: &str,
    credential_owner_id: Option<&str>,
    scope: ThumbnailSignatureScope<'_>,
) -> Result<Option<String>, AppError> {
    let credential_owner_id = credential_owner_id.unwrap_or(auth_user_id);
    if !thumbnail_signature_present(raw_query) && credential_owner_id == auth_user_id {
        return Ok(None);
    }

    let scope = ThumbnailSignatureScope {
        credential_owner_id,
        ..scope
    };
    verify_signed_thumbnail_access(signing_key, public_auth_user_id, raw_query, scope).map(Some)
}

fn app_error_to_thumbnail_api_error(error: AppError) -> ApiError {
    match error.status {
        axum::http::StatusCode::UNAUTHORIZED => ApiError::Authentication(error.message),
        axum::http::StatusCode::FORBIDDEN => ApiError::Authorization(error.message),
        axum::http::StatusCode::BAD_REQUEST => ApiError::InvalidInput(error.message),
        axum::http::StatusCode::NOT_FOUND => ApiError::NotFound(error.message),
        axum::http::StatusCode::REQUEST_TIMEOUT => ApiError::Timeout(error.message),
        axum::http::StatusCode::TOO_MANY_REQUESTS => ApiError::RateLimited(error.message),
        axum::http::StatusCode::SERVICE_UNAVAILABLE => ApiError::ServiceUnavailable(error.message),
        _ if error.status.is_server_error() => ApiError::Internal(error.message),
        _ => ApiError::InvalidInput(error.message),
    }
}

pub(crate) fn sign_emby_thumbnail_url(
    thumbnail_url: &str,
    room_id: &str,
    user_id: &str,
    signing_key: Option<&ProxySigningKey>,
) -> Result<String, String> {
    let Some(signing_key) = signing_key else {
        return Ok(thumbnail_url.to_string());
    };
    let Some(path_with_item) = thumbnail_url.strip_prefix(THUMBNAIL_ROUTE_PREFIX) else {
        return Ok(thumbnail_url.to_string());
    };
    let Some((item_id, raw_query)) = path_with_item.split_once('?') else {
        return Ok(thumbnail_url.to_string());
    };
    if thumbnail_signature_present(raw_query) {
        return Ok(thumbnail_url.to_string());
    }

    let mut server_id = None;
    let mut credential_owner_id = None;
    let mut max_height = None;
    let mut max_width = None;
    for (key, value) in url::form_urlencoded::parse(raw_query.as_bytes()) {
        match key.as_ref() {
            "server_id" => server_id = Some(value.into_owned()),
            "credential_owner_id" => credential_owner_id = Some(value.into_owned()),
            "max_height" => {
                max_height = Some(value.parse().map_err(|error| {
                    format!("Failed to parse Emby thumbnail max_height: {error}")
                })?);
            }
            "max_width" => {
                max_width = Some(value.parse().map_err(|error| {
                    format!("Failed to parse Emby thumbnail max_width: {error}")
                })?);
            }
            _ => {}
        }
    }
    let query = ThumbnailQuery {
        server_id: server_id.unwrap_or_default(),
        credential_owner_id,
        max_height,
        max_width,
    };
    let (server_id, credential_owner_id, max_height, max_width) =
        resolve_thumbnail_query(&query).map_err(|error| error.message)?;
    let credential_owner_id = credential_owner_id.unwrap_or(user_id);
    let item_id = percent_encoding::percent_decode_str(item_id)
        .decode_utf8()
        .map_err(|error| format!("Failed to decode Emby thumbnail item_id: {error}"))?
        .into_owned();
    let scope = ThumbnailSignatureScope {
        item_id: &item_id,
        server_id,
        credential_owner_id,
        max_height,
        max_width,
    };
    let expires_at = chrono::Utc::now().timestamp()
        + synctv_core::proxy_signature::ProxySigningKey::default_expiry_secs();
    let signed_query =
        build_signed_thumbnail_query(signing_key, room_id, user_id, scope, expires_at);

    Ok(format!("{thumbnail_url}&{signed_query}"))
}

/// Emby endpoints that perform authentication or credential mutation.
pub fn emby_auth_routes() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/logout", post(logout))
}

/// Emby read/query endpoints.
pub fn emby_read_routes() -> Router<AppState> {
    Router::new()
        .route("/list", post(list))
        .route("/me", post(me))
        .route("/binds", get(binds))
        .route("/thumbnail/{item_id}", get(thumbnail))
}

// Existing provider API handlers

/// Login to Emby/Jellyfin (validate API key and persist credential)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/emby/login",
        tag = "Provider",
        params(ProviderInstanceQuery),
        request_body = crate::proto::providers::emby::LoginRequest,
        responses(
            (status = 200, description = "Emby login succeeded", body = crate::proto::providers::emby::LoginResponse),
            (status = 400, description = "Invalid login request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Provider access denied", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Provider resource not found", body = crate::openapi::ErrorResponseDoc),
            (status = 408, description = "Provider request timed out", body = crate::openapi::ErrorResponseDoc),
            (status = 409, description = "Provider request conflict", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn login(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(query): ProtoQuery<ProviderInstanceQuery>,
    Json(mut req): Json<crate::proto::providers::emby::LoginRequest>,
) -> AppResult<Json<crate::proto::providers::emby::LoginResponse>> {
    tracing::info!("Emby login request");

    let instance_name = apply_provider_instance_name(&mut req.instance_name, &query)?;
    let api = state.shared_api_runtime.emby_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Auth,
        move |control, authenticated| {
            async move {
                api.login_with_context(
                    &authenticated.user_id,
                    req,
                    instance_name.as_deref(),
                    Some(&control),
                )
                .await
            }
            .boxed()
        },
    )
    .await
    .map_err(|e| {
        tracing::error!("Emby login failed: {}", e);
        e
    })
}

/// List Emby library items
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/emby/list",
        tag = "Provider",
        params(ProviderInstanceQuery),
        request_body = crate::proto::providers::emby::ListRequest,
        responses(
            (status = 200, description = "Emby library listing", body = crate::proto::providers::emby::ListResponse),
            (status = 400, description = "Invalid list request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Provider access denied", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Provider resource not found", body = crate::openapi::ErrorResponseDoc),
            (status = 408, description = "Provider request timed out", body = crate::openapi::ErrorResponseDoc),
            (status = 409, description = "Provider request conflict", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn list(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(query): ProtoQuery<ProviderInstanceQuery>,
    Json(mut req): Json<crate::proto::providers::emby::ListRequest>,
) -> AppResult<Json<crate::proto::providers::emby::ListResponse>> {
    tracing::info!("Emby list request");

    let instance_name = apply_provider_instance_name(&mut req.instance_name, &query)?;
    let api = state.shared_api_runtime.emby_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |control, authenticated| {
            async move {
                api.list_with_context(
                    &authenticated.user_id,
                    req,
                    instance_name.as_deref(),
                    Some(&control),
                )
                .await
            }
            .boxed()
        },
    )
    .await
    .map_err(|e| {
        tracing::error!("Emby list failed: {}", e);
        e
    })
}

/// Get Emby user info
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/emby/me",
        tag = "Provider",
        params(ProviderInstanceQuery),
        request_body = crate::proto::providers::emby::GetMeRequest,
        responses(
            (status = 200, description = "Emby account info", body = crate::proto::providers::emby::GetMeResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Provider access denied", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Provider resource not found", body = crate::openapi::ErrorResponseDoc),
            (status = 408, description = "Provider request timed out", body = crate::openapi::ErrorResponseDoc),
            (status = 409, description = "Provider request conflict", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn me(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(query): ProtoQuery<ProviderInstanceQuery>,
    Json(mut req): Json<crate::proto::providers::emby::GetMeRequest>,
) -> AppResult<Json<crate::proto::providers::emby::GetMeResponse>> {
    tracing::info!("Emby me request");

    let instance_name = apply_provider_instance_name(&mut req.instance_name, &query)?;
    let api = state.shared_api_runtime.emby_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |control, authenticated| {
            async move {
                api.get_me_with_context(
                    &authenticated.user_id,
                    req,
                    instance_name.as_deref(),
                    Some(&control),
                )
                .await
            }
            .boxed()
        },
    )
    .await
    .map_err(|e| {
        tracing::error!("Emby me failed: {}", e);
        e
    })
}

/// Logout from Emby (delete stored credential)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/emby/logout",
        tag = "Provider",
        params(ProviderInstanceQuery),
        request_body = crate::proto::providers::emby::LogoutRequest,
        responses(
            (status = 200, description = "Emby credential removed", body = crate::proto::providers::emby::LogoutResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Provider access denied", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Provider resource not found", body = crate::openapi::ErrorResponseDoc),
            (status = 408, description = "Provider request timed out", body = crate::openapi::ErrorResponseDoc),
            (status = 409, description = "Provider request conflict", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn logout(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(query): ProtoQuery<ProviderInstanceQuery>,
    Json(mut req): Json<crate::proto::providers::emby::LogoutRequest>,
) -> AppResult<Json<crate::proto::providers::emby::LogoutResponse>> {
    tracing::info!("Emby logout request");

    apply_provider_instance_name(&mut req.instance_name, &query)?;
    let api = state.shared_api_runtime.emby_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Auth,
        move |authenticated| async move { api.logout(&authenticated.user_id, req).await }.boxed(),
    )
    .await
    .map_err(|e| {
        tracing::error!("Emby logout failed: {}", e);
        e
    })
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/providers/emby/binds",
        tag = "Provider",
        params(ProviderInstanceQuery),
        responses(
            (status = 200, description = "Saved Emby credentials", body = GetBindsResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 400, description = "Invalid provider instance query", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Provider access denied", body = crate::openapi::ErrorResponseDoc),
            (status = 408, description = "Provider bind request timed out", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc),
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
    ProtoQuery(query): ProtoQuery<ProviderInstanceQuery>,
) -> AppResult<Json<GetBindsResponse>> {
    let instance_name = provider_instance_name(&query)?.map(str::to_owned);
    let api = state.shared_api_runtime.emby_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |authenticated| {
            async move {
                tracing::info!("Emby binds request for user: {}", authenticated.user_id);
                api.get_binds(&authenticated.user_id, instance_name.as_deref())
                    .await
            }
            .boxed()
        },
    )
    .await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/providers/emby/thumbnail/{item_id}",
        tag = "Provider",
        params(
            ("item_id" = String, Path, description = "Emby item ID"),
            ("server_id" = String, Query, description = "Saved Emby credential server ID"),
            ("credential_owner_id" = Option<String>, Query, description = "Original credential owner for shared Emby media"),
            ("max_height" = Option<u32>, Query, description = "Maximum thumbnail height"),
            ("max_width" = Option<u32>, Query, description = "Maximum thumbnail width")
        ),
        responses(
            (status = 200, description = "Proxied Emby thumbnail"),
            (status = 400, description = "Invalid thumbnail request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Provider access denied", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Emby credential not found", body = crate::openapi::ErrorResponseDoc),
            (status = 408, description = "Thumbnail request timed out", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited", body = crate::openapi::ErrorResponseDoc),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn thumbnail(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(item_id): Path<String>,
    Query(query): Query<ThumbnailQuery>,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
) -> AppResult<axum::response::Response> {
    let (server_id, credential_owner_id, max_height, max_width) = resolve_thumbnail_query(&query)?;
    let raw_query = raw_query.as_deref().unwrap_or("");
    let operation_state = state.clone();
    let request_meta = provider_request_metadata(request_meta);
    let action = state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Read,
            move |authenticated| async move {
                let state = operation_state;
                let auth_user_id_key = authenticated.user_id.to_string();
                let public_auth_user_id = state
                    .shared_api_runtime
                    .public_id_codec
                    .encode_user_id(authenticated.user_id)
                    .map_err(crate::impls::ApiError::Internal)?;
                let scope = ThumbnailSignatureScope {
                    item_id: &item_id,
                    server_id,
                    credential_owner_id: credential_owner_id.unwrap_or(auth_user_id_key.as_str()),
                    max_height,
                    max_width,
                };
                if let Some(room_id) = authorize_thumbnail_request(
                    &state.shared_api_runtime.proxy_signing_key,
                    &auth_user_id_key,
                    &public_auth_user_id,
                    raw_query,
                    credential_owner_id,
                    scope,
                )
                .map_err(app_error_to_thumbnail_api_error)?
                {
                    let room_id = state
                        .shared_api_runtime
                        .public_id_codec
                        .decode_room_id(&room_id)
                        .map_err(crate::impls::ApiError::InvalidInput)?;
                    crate::impls::providers::proxy::validate_fresh_provider_proxy_access(
                        &state.user_service,
                        &state.shared_api_runtime.proxy_services,
                        &room_id,
                        &authenticated.user_id,
                    )
                    .await?;
                }

                let credential_lookup_user_id = if let Some(public_id) = credential_owner_id {
                    state
                        .shared_api_runtime
                        .public_id_codec
                        .decode_user_id(public_id)
                        .map_err(crate::impls::ApiError::InvalidInput)?
                } else {
                    authenticated.user_id
                };

                let credential = state
                    .user_provider_credential_repository
                    .get_by_provider_and_server(
                        credential_lookup_user_id,
                        synctv_core::provider::EmbyProvider::NAME,
                        server_id,
                    )
                    .await
                    .map_err(crate::impls::ApiError::from)?
                    .ok_or_else(|| {
                        crate::impls::ApiError::NotFound("Emby credential not found".to_string())
                    })?;

                let parsed = credential.get_credential().map_err(|error| {
                    crate::impls::ApiError::Internal(format!(
                        "Failed to parse stored Emby credential: {error}"
                    ))
                })?;
                let action = build_thumbnail_proxy_action_from_credential(
                    &item_id, &parsed, max_height, max_width,
                )?;

                Ok::<ProxyAction, ApiError>(action)
            },
        )
        .await
        .map_err(map_api_error)?;

    let response = super::execute_proxy_action_with_state(&state, action, &headers, None).await?;

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_thumbnail_query_requires_server_id() {
        let err = resolve_thumbnail_query(&ThumbnailQuery {
            server_id: "   ".to_string(),
            credential_owner_id: None,
            max_height: None,
            max_width: None,
        })
        .expect_err("thumbnail query must require server_id");

        assert_eq!(err.message, "server_id must not be empty");
    }

    #[test]
    fn test_resolve_thumbnail_query_preserves_shared_credential_owner_id() {
        let query = ThumbnailQuery {
            server_id: " emby-main ".to_string(),
            credential_owner_id: Some(" owner-123 ".to_string()),
            max_height: Some(480),
            max_width: Some(640),
        };
        let (server_id, credential_owner_id, max_height, max_width) =
            resolve_thumbnail_query(&query).expect("thumbnail query should parse");

        assert_eq!(server_id, "emby-main");
        assert_eq!(credential_owner_id, Some("owner-123"));
        assert_eq!(max_height, 480);
        assert_eq!(max_width, 640);
    }

    #[test]
    fn test_authorize_thumbnail_request_requires_signature_for_shared_credentials() {
        let signing_key = ProxySigningKey::derive_from(b"test-signing-key-minimum-32-bytes!!");
        let err = authorize_thumbnail_request(
            &signing_key,
            "viewer-1",
            "viewer-1",
            "server_id=emby-main&credential_owner_id=owner-1&max_height=300",
            Some("owner-1"),
            ThumbnailSignatureScope {
                item_id: "item-123",
                server_id: "emby-main",
                credential_owner_id: "owner-1",
                max_height: 300,
                max_width: 0,
            },
        )
        .expect_err("shared credential thumbnails must require a signed query");

        assert_eq!(err.status, axum::http::StatusCode::UNAUTHORIZED);
        assert_eq!(err.message, "Invalid thumbnail signature");
    }

    #[test]
    fn test_authorize_thumbnail_request_rejects_signed_url_for_other_user() {
        let signing_key = ProxySigningKey::derive_from(b"test-signing-key-minimum-32-bytes!!");
        let raw_query = build_signed_thumbnail_query(
            &signing_key,
            "room-1",
            "viewer-1",
            ThumbnailSignatureScope {
                item_id: "item-123",
                server_id: "emby-main",
                credential_owner_id: "owner-1",
                max_height: 300,
                max_width: 0,
            },
            chrono::Utc::now().timestamp() + 300,
        );

        let err = authorize_thumbnail_request(
            &signing_key,
            "viewer-2",
            "viewer-2",
            &raw_query,
            Some("owner-1"),
            ThumbnailSignatureScope {
                item_id: "item-123",
                server_id: "emby-main",
                credential_owner_id: "owner-1",
                max_height: 300,
                max_width: 0,
            },
        )
        .expect_err("signed query must be bound to the authenticated user");

        assert_eq!(err.status, axum::http::StatusCode::FORBIDDEN);
        assert_eq!(err.message, "Thumbnail URL is not valid for this user");
    }

    #[test]
    fn test_sign_emby_thumbnail_url_appends_room_scoped_signature() {
        let signing_key = ProxySigningKey::derive_from(b"test-signing-key-minimum-32-bytes!!");
        let signed = sign_emby_thumbnail_url(
            "/api/providers/emby/thumbnail/item-123?server_id=emby-main&credential_owner_id=owner-1&max_height=300",
            "room-7",
            "viewer-1",
            Some(&signing_key),
        )
        .expect("thumbnail URL should sign successfully");

        let query = signed.split('?').nth(1).expect("signed thumbnail query");
        let claims = signing_key
            .parse_and_verify_query(
                query,
                THUMBNAIL_SIGNATURE_PROVIDER,
                &thumbnail_signature_version(ThumbnailSignatureScope {
                    item_id: "item-123",
                    server_id: "emby-main",
                    credential_owner_id: "owner-1",
                    max_height: 300,
                    max_width: 0,
                }),
            )
            .expect("signed thumbnail query should verify");

        assert_eq!(claims.room_id, "room-7");
        assert_eq!(claims.user_id, "viewer-1");
        assert!(signed.contains("credential_owner_id=owner-1"));
    }

    #[test]
    fn test_build_thumbnail_proxy_action_from_credential_uses_server_side_token() {
        let action = build_thumbnail_proxy_action_from_credential(
            "item-123",
            &ProviderCredential::Emby {
                host: "https://emby.example.com/base".to_string(),
                api_key: "secret-token".to_string(),
                emby_user_id: "user-1".to_string(),
            },
            300,
            640,
        )
        .expect("emby credential should build thumbnail proxy action");

        match action {
            ProxyAction::FetchAndForward { url, headers, .. } => {
                assert_eq!(
                    url,
                    "https://emby.example.com/base/Items/item-123/Images/Primary?maxHeight=300&maxWidth=640&quality=90"
                );
                assert_eq!(
                    headers.get("X-Emby-Token"),
                    Some(&"secret-token".to_string())
                );
            }
            other => panic!("expected FetchAndForward, got {other:?}"),
        }
    }
}
