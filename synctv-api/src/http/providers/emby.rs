//! Emby Provider HTTP Routes
//!
//! Provider API endpoints for Emby login, listing, etc.
//! Playback transport routes live under the Emby playback-provider
//! modules, while thumbnail fetches use an authenticated route that resolves
//! Emby credentials server-side.

use axum::{
    extract::{Path, Query, RawQuery, State},
    routing::{get, post},
    Json, Router,
};
use futures::FutureExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use synctv_core::models::ProviderCredential;
use synctv_core::provider::playback_transport::PlaybackTransportAction;
use synctv_core::proxy_signature::{ProxySigningKey, ProxyUrlClaims};

use crate::http::{
    error::map_api_error, middleware::RequestMetadata, validation::ProtoQuery, AppError, AppResult,
    AppState,
};
use crate::impls::ApiError;
use crate::impls::EndpointRateLimitCategory;
use synctv_proto::providers::common::ProviderInstanceQuery;
use synctv_proto::providers::emby::GetBindsResponse;

use super::common::{
    execute_provider_user_endpoint, execute_provider_user_endpoint_with_control,
    provider_instance_name, provider_instance_name_from_request_field, provider_request_metadata,
};

const DEFAULT_THUMBNAIL_HEIGHT: u32 = 300;
const MAX_THUMBNAIL_DIMENSION: u32 = 1920;
const THUMBNAIL_ROUTE_PREFIX: &str = "/api/providers/emby/thumbnail/";
const THUMBNAIL_SIGNATURE_PROVIDER: &str = "emby-thumbnail";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ThumbnailQuery {
    server_id: String,
    #[serde(default)]
    credential_owner_id: Option<String>,
    #[serde(default)]
    max_height: Option<u32>,
    #[serde(default)]
    max_width: Option<u32>,
    #[serde(default, rename = "sig")]
    _sig: Option<String>,
    #[serde(default, rename = "uid")]
    _uid: Option<String>,
    #[serde(default, rename = "rid")]
    _rid: Option<String>,
    #[serde(default, rename = "exp")]
    _exp: Option<i64>,
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
        return Err(AppError::bad_request("serverId must not be empty"));
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
) -> Result<PlaybackTransportAction, ApiError> {
    let thumbnail_path = if max_width > 0 {
        format!(
            "/Items/{item_id}/Images/Primary?maxHeight={max_height}&maxWidth={max_width}&quality=90"
        )
    } else {
        format!("/Items/{item_id}/Images/Primary?maxHeight={max_height}&quality=90")
    };

    let url = synctv_core::provider::emby::emby_server_url(host, &thumbnail_path)
        .map_err(|error| ApiError::Internal(error.to_string()))?;

    Ok(PlaybackTransportAction::FetchAndForward {
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
) -> Result<PlaybackTransportAction, ApiError> {
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
    hex::encode(digest)
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
        resource: "thumbnail".to_string(),
        room_id: room_id.to_string(),
        user_id: user_id.to_string(),
        expires_at,
        target_url: None,
    };
    signing_key.build_signed_query(&claims)
}

fn thumbnail_signature_present(raw_query: &str) -> bool {
    url::form_urlencoded::parse(raw_query.as_bytes()).any(|(key, _)| key.as_ref() == "sig")
}

fn thumbnail_signature_query(raw_query: &str) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(
            url::form_urlencoded::parse(raw_query.as_bytes())
                .filter(|(key, _)| matches!(key.as_ref(), "sig" | "uid" | "rid" | "exp" | "pv")),
        )
        .finish()
}

fn verify_signed_thumbnail_access(
    signing_key: &ProxySigningKey,
    auth_user_id: &str,
    raw_query: &str,
    scope: ThumbnailSignatureScope<'_>,
) -> Result<String, AppError> {
    let signature_query = thumbnail_signature_query(raw_query);
    let claims = signing_key
        .parse_and_verify_query(
            &signature_query,
            THUMBNAIL_SIGNATURE_PROVIDER,
            &thumbnail_signature_version(scope),
            "thumbnail",
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
    public_credential_owner_id: &str,
    public_auth_user_id: &str,
    raw_query: &str,
    credential_owner_id: Option<&str>,
    scope: ThumbnailSignatureScope<'_>,
) -> Result<Option<String>, AppError> {
    let credential_owner_id = credential_owner_id.unwrap_or(public_credential_owner_id);
    if !thumbnail_signature_present(raw_query) && credential_owner_id == public_credential_owner_id
    {
        return Ok(None);
    }

    let scope = ThumbnailSignatureScope {
        credential_owner_id,
        ..scope
    };
    verify_signed_thumbnail_access(signing_key, public_auth_user_id, raw_query, scope).map(Some)
}

fn app_error_to_thumbnail_api_error(error: &AppError) -> ApiError {
    match error.status() {
        axum::http::StatusCode::UNAUTHORIZED => {
            ApiError::Authentication(error.message().to_string())
        }
        axum::http::StatusCode::FORBIDDEN => ApiError::Authorization(error.message().to_string()),
        axum::http::StatusCode::BAD_REQUEST => ApiError::InvalidInput(error.message().to_string()),
        axum::http::StatusCode::NOT_FOUND => ApiError::NotFound(error.message().to_string()),
        axum::http::StatusCode::REQUEST_TIMEOUT => ApiError::Timeout(error.message().to_string()),
        axum::http::StatusCode::TOO_MANY_REQUESTS => {
            ApiError::RateLimited(error.message().to_string())
        }
        axum::http::StatusCode::SERVICE_UNAVAILABLE => {
            ApiError::ServiceUnavailable(error.message().to_string())
        }
        status if status.is_server_error() => ApiError::Internal(error.message().to_string()),
        _ => ApiError::InvalidInput(error.message().to_string()),
    }
}

pub(crate) fn sign_emby_thumbnail_url(
    thumbnail_url: &str,
    room_id: &str,
    user_id: &str,
    signing_key: &ProxySigningKey,
) -> Result<String, String> {
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
            "serverId" => server_id = Some(value.into_owned()),
            "credentialOwnerId" => credential_owner_id = Some(value.into_owned()),
            "maxHeight" => {
                max_height = Some(value.parse().map_err(|error| {
                    format!("Failed to parse Emby thumbnail maxHeight: {error}")
                })?);
            }
            "maxWidth" => {
                max_width = Some(value.parse().map_err(|error| {
                    format!("Failed to parse Emby thumbnail maxWidth: {error}")
                })?);
            }
            _ => {}
        }
    }
    let server_id = server_id.ok_or_else(|| "Emby thumbnail URL missing serverId".to_string())?;
    let query = ThumbnailQuery {
        server_id,
        credential_owner_id,
        max_height,
        max_width,
        _sig: None,
        _uid: None,
        _rid: None,
        _exp: None,
    };
    let (server_id, credential_owner_id, max_height, max_width) =
        resolve_thumbnail_query(&query).map_err(|error| error.message().to_string())?;
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
pub(crate) fn emby_auth_routes() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/logout", post(logout))
}

/// Emby read/query endpoints.
pub(crate) fn emby_read_routes() -> Router<AppState> {
    Router::new()
        .route("/list", post(list))
        .route("/me", post(me))
        .route("/binds", get(binds))
        .route("/thumbnail/{itemId}", get(thumbnail))
}

// Existing provider API handlers

/// Login to Emby (validate API key and persist credential)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/emby/login",
        tag = "Provider",
        request_body = synctv_proto::providers::emby::LoginRequest,
        responses(
            (status = 200, description = "Emby login succeeded", body = synctv_proto::providers::emby::LoginResponse),
            (status = 400, description = "Invalid login request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Provider access denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Provider resource not found", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 408, description = "Provider request timed out", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 409, description = "Provider request conflict", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn login(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<synctv_proto::providers::emby::LoginRequest>,
) -> AppResult<Json<synctv_proto::providers::emby::LoginResponse>> {
    tracing::info!("Emby login request");

    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
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
        request_body = synctv_proto::providers::emby::ListRequest,
        responses(
            (status = 200, description = "Emby library listing", body = synctv_proto::providers::emby::ListResponse),
            (status = 400, description = "Invalid list request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Provider access denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Provider resource not found", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 408, description = "Provider request timed out", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 409, description = "Provider request conflict", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn list(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<synctv_proto::providers::emby::ListRequest>,
) -> AppResult<Json<synctv_proto::providers::emby::ListResponse>> {
    tracing::info!("Emby list request");

    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
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
        request_body = synctv_proto::providers::emby::GetMeRequest,
        responses(
            (status = 200, description = "Emby account info", body = synctv_proto::providers::emby::GetMeResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Provider access denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Provider resource not found", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 408, description = "Provider request timed out", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 409, description = "Provider request conflict", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn me(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<synctv_proto::providers::emby::GetMeRequest>,
) -> AppResult<Json<synctv_proto::providers::emby::GetMeResponse>> {
    tracing::info!("Emby me request");

    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
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
        request_body = synctv_proto::providers::emby::LogoutRequest,
        responses(
            (status = 200, description = "Emby credential removed", body = synctv_proto::providers::emby::LogoutResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Provider access denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Provider resource not found", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 408, description = "Provider request timed out", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 409, description = "Provider request conflict", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn logout(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<synctv_proto::providers::emby::LogoutRequest>,
) -> AppResult<Json<synctv_proto::providers::emby::LogoutResponse>> {
    tracing::info!("Emby logout request");

    provider_instance_name_from_request_field(&req.instance_name)?;
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
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 400, description = "Invalid provider instance query", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Provider access denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 408, description = "Provider bind request timed out", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Provider bind information unavailable", body = crate::openapi::GoogleRpcStatusSchema)
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
        path = "/api/providers/emby/thumbnail/{itemId}",
        tag = "Provider",
        params(
            ("itemId" = String, Path, description = "Emby item ID"),
            ("serverId" = String, Query, description = "Saved Emby credential server ID"),
            ("credentialOwnerId" = Option<String>, Query, description = "Original credential owner for shared Emby media"),
            ("maxHeight" = Option<u32>, Query, description = "Maximum thumbnail height"),
            ("maxWidth" = Option<u32>, Query, description = "Maximum thumbnail width")
        ),
        responses(
            (status = 200, description = "Proxied Emby thumbnail"),
            (status = 400, description = "Invalid thumbnail request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Provider access denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Emby credential not found", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 408, description = "Thumbnail request timed out", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Provider service unavailable", body = crate::openapi::GoogleRpcStatusSchema)
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
                let public_auth_user_id = state
                    .shared_api_runtime
                    .public_id_codec
                    .encode_user_id(authenticated.user_id)
                    .map_err(crate::impls::ApiError::Internal)?;
                let scope = ThumbnailSignatureScope {
                    item_id: &item_id,
                    server_id,
                    credential_owner_id: credential_owner_id.unwrap_or(&public_auth_user_id),
                    max_height,
                    max_width,
                };
                if let Some(room_id) = authorize_thumbnail_request(
                    &state.shared_api_runtime.proxy_signing_key,
                    &public_auth_user_id,
                    &public_auth_user_id,
                    raw_query,
                    credential_owner_id,
                    scope,
                )
                .map_err(|error| app_error_to_thumbnail_api_error(&error))?
                {
                    let room_id = state
                        .shared_api_runtime
                        .public_id_codec
                        .decode_room_id(&room_id)
                        .map_err(crate::impls::ApiError::InvalidInput)?;
                    crate::impls::playback_provider::validate_fresh_playback_provider_access(
                        &state.user_service,
                        &state.shared_api_runtime.playback_transport_services,
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

                let parsed = credential.credential_data.clone();
                let action = build_thumbnail_proxy_action_from_credential(
                    &item_id, &parsed, max_height, max_width,
                )?;

                Ok::<PlaybackTransportAction, ApiError>(action)
            },
        )
        .await
        .map_err(map_api_error)?;

    let response = super::execute_playback_transport_with_state(&state, action, None).await?;

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn route_ok<T>(result: Result<T, AppError>) -> TestResult<T> {
        result.map_err(|error| test_error(format!("route error: {error:?}")))
    }

    fn api_ok<T>(result: Result<T, ApiError>) -> TestResult<T> {
        result.map_err(|error| test_error(format!("{error:?}")))
    }

    fn string_ok<T>(result: Result<T, String>) -> TestResult<T> {
        result.map_err(test_error)
    }

    fn route_err<T>(result: Result<T, AppError>) -> TestResult<AppError> {
        match result {
            Ok(_) => Err(test_error("expected route error")),
            Err(error) => Ok(error),
        }
    }

    #[test]
    fn test_resolve_thumbnail_query_requires_server_id() -> TestResult {
        let err = route_err(resolve_thumbnail_query(&ThumbnailQuery {
            server_id: "   ".to_string(),
            credential_owner_id: None,
            max_height: None,
            max_width: None,
            _sig: None,
            _uid: None,
            _rid: None,
            _exp: None,
        }))?;

        assert_eq!(err.message(), "serverId must not be empty");
        Ok(())
    }

    #[test]
    fn test_resolve_thumbnail_query_preserves_shared_credential_owner_id() -> TestResult {
        let query = ThumbnailQuery {
            server_id: " emby-main ".to_string(),
            credential_owner_id: Some(" owner-123 ".to_string()),
            max_height: Some(480),
            max_width: Some(640),
            _sig: None,
            _uid: None,
            _rid: None,
            _exp: None,
        };
        let (server_id, credential_owner_id, max_height, max_width) =
            route_ok(resolve_thumbnail_query(&query))?;

        assert_eq!(server_id, "emby-main");
        assert_eq!(credential_owner_id, Some("owner-123"));
        assert_eq!(max_height, 480);
        assert_eq!(max_width, 640);
        Ok(())
    }

    #[test]
    fn test_thumbnail_query_uses_lower_camel_case() -> TestResult {
        let query: ThumbnailQuery = serde_urlencoded::from_str(
            "serverId=emby-main&credentialOwnerId=owner-1&maxHeight=300&maxWidth=640&sig=s&uid=u&rid=r&exp=1",
        )?;

        assert_eq!(query.server_id, "emby-main");
        assert_eq!(query.credential_owner_id.as_deref(), Some("owner-1"));
        assert_eq!(query.max_height, Some(300));
        assert_eq!(query.max_width, Some(640));

        let query = serde_urlencoded::from_str::<ThumbnailQuery>("serverId=emby-main&extra=value")?;
        assert_eq!(query.server_id, "emby-main");
        Ok(())
    }

    #[test]
    fn test_authorize_thumbnail_request_requires_signature_for_shared_credentials() -> TestResult {
        let signing_key = ProxySigningKey::try_derive_from(b"test-signing-key-minimum-32-bytes!!")?;
        let err = route_err(authorize_thumbnail_request(
            &signing_key,
            "viewer-1",
            "viewer-1",
            "serverId=emby-main&credentialOwnerId=owner-1&maxHeight=300",
            Some("owner-1"),
            ThumbnailSignatureScope {
                item_id: "item-123",
                server_id: "emby-main",
                credential_owner_id: "owner-1",
                max_height: 300,
                max_width: 0,
            },
        ))?;

        assert_eq!(err.status(), axum::http::StatusCode::UNAUTHORIZED);
        assert_eq!(err.message(), "Invalid thumbnail signature");
        Ok(())
    }

    #[test]
    fn test_authorize_thumbnail_request_rejects_signed_url_for_other_user() -> TestResult {
        let signing_key = ProxySigningKey::try_derive_from(b"test-signing-key-minimum-32-bytes!!")?;
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

        let err = route_err(authorize_thumbnail_request(
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
        ))?;

        assert_eq!(err.status(), axum::http::StatusCode::FORBIDDEN);
        assert_eq!(err.message(), "Thumbnail URL is not valid for this user");
        Ok(())
    }

    #[test]
    fn test_sign_emby_thumbnail_url_appends_room_scoped_signature() -> TestResult {
        let signing_key = ProxySigningKey::try_derive_from(b"test-signing-key-minimum-32-bytes!!")?;
        let signed = string_ok(sign_emby_thumbnail_url(
            "/api/providers/emby/thumbnail/item-123?serverId=emby-main&credentialOwnerId=owner-1&maxHeight=300",
            "room-7",
            "viewer-1",
            &signing_key,
        ))?;

        let raw_query = signed
            .split('?')
            .nth(1)
            .ok_or_else(|| test_error("signed thumbnail query should exist"))?;
        let query = thumbnail_signature_query(raw_query);
        let claims = signing_key
            .parse_and_verify_query(
                &query,
                THUMBNAIL_SIGNATURE_PROVIDER,
                &thumbnail_signature_version(ThumbnailSignatureScope {
                    item_id: "item-123",
                    server_id: "emby-main",
                    credential_owner_id: "owner-1",
                    max_height: 300,
                    max_width: 0,
                }),
                "thumbnail",
            )
            .map_err(|error| test_error(error.to_string()))?;

        assert_eq!(claims.room_id, "room-7");
        assert_eq!(claims.user_id, "viewer-1");
        assert!(signed.contains("credentialOwnerId=owner-1"));
        Ok(())
    }

    #[test]
    fn test_signed_emby_thumbnail_url_authorizes_roundtrip() -> TestResult {
        let signing_key = ProxySigningKey::try_derive_from(b"test-signing-key-minimum-32-bytes!!")?;
        let signed = string_ok(sign_emby_thumbnail_url(
            "/api/providers/emby/thumbnail/item1?serverId=emby-main&credentialOwnerId=usr_2&maxHeight=300",
            "room_1",
            "usr_2",
            &signing_key,
        ))?;
        let raw_query = signed
            .split_once('?')
            .map(|(_, query)| query)
            .ok_or_else(|| test_error("signed thumbnail query should exist"))?;

        let room_id = route_ok(authorize_thumbnail_request(
            &signing_key,
            "usr_2",
            "usr_2",
            raw_query,
            Some("usr_2"),
            ThumbnailSignatureScope {
                item_id: "item1",
                server_id: "emby-main",
                credential_owner_id: "usr_2",
                max_height: 300,
                max_width: 0,
            },
        ))?;

        assert_eq!(room_id.as_deref(), Some("room_1"));
        Ok(())
    }

    #[test]
    fn test_sign_emby_thumbnail_url_requires_server_id() -> TestResult {
        let signing_key = ProxySigningKey::try_derive_from(b"test-signing-key-minimum-32-bytes!!")?;
        let err = sign_emby_thumbnail_url(
            "/api/providers/emby/thumbnail/item-123?maxHeight=300",
            "room-7",
            "viewer-1",
            &signing_key,
        )
        .expect_err("missing serverId should fail signing");

        assert_eq!(err, "Emby thumbnail URL missing serverId");
        Ok(())
    }

    #[test]
    fn test_build_thumbnail_proxy_action_from_credential_uses_server_side_token() -> TestResult {
        let action = api_ok(build_thumbnail_proxy_action_from_credential(
            "item-123",
            &ProviderCredential::Emby {
                host: "https://emby.example.com/base".to_string(),
                api_key: "secret-token".to_string(),
                emby_user_id: "user-1".to_string(),
            },
            300,
            640,
        ))?;

        match action {
            PlaybackTransportAction::FetchAndForward { url, headers, .. } => {
                assert_eq!(
                    url,
                    "https://emby.example.com/base/Items/item-123/Images/Primary?maxHeight=300&maxWidth=640&quality=90"
                );
                assert_eq!(
                    headers.get("X-Emby-Token"),
                    Some(&"secret-token".to_string())
                );
            }
            other => {
                return Err(test_error(format!(
                    "expected FetchAndForward, got {other:?}"
                )))
            }
        }
        Ok(())
    }
}
