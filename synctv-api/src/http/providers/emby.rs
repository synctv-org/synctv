//! Emby Provider HTTP Routes
//!
//! Provider API endpoints for Emby login, listing, etc.
//! Playback transport routes live under the Emby playback-provider
//! modules, while thumbnail fetches use an authenticated route that resolves
//! Emby credentials server-side.

use crate::emby_thumbnail_urls::{
    clamp_thumbnail_dimension, thumbnail_signature_present, verify_signed_thumbnail_access,
    ThumbnailSignatureAccessError, ThumbnailSignatureScope,
};
use axum::{
    extract::{Path, Query, RawQuery, State},
    routing::{get, post},
    Json, Router,
};
use futures::FutureExt;
use serde::Deserialize;
use synctv_core::provider::{EmbyProvider, PlaybackTransportAction};

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

fn authorize_thumbnail_request(
    signing_key: &crate::proxy_signature::ProxySigningKey,
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
    verify_signed_thumbnail_access(signing_key, public_auth_user_id, raw_query, scope)
        .map(Some)
        .map_err(|error| match error {
            ThumbnailSignatureAccessError::Invalid => {
                AppError::unauthorized("Invalid thumbnail signature")
            }
            ThumbnailSignatureAccessError::WrongUser => {
                AppError::forbidden("Thumbnail URL is not valid for this user")
            }
        })
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
                    super::playback_provider::playback_provider_api_runtime(&state)
                        .validate_fresh_access(&room_id, &authenticated.user_id)
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

                let access = state
                    .shared_api_runtime
                    .provider_access_service
                    .emby_access(credential_lookup_user_id, server_id, None, None)
                    .await?;
                let action = EmbyProvider::thumbnail_action(
                    &item_id,
                    &access.host,
                    &access.api_key,
                    max_height,
                    max_width,
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
    use crate::emby_thumbnail_urls::{
        build_signed_thumbnail_query, sign_emby_thumbnail_url, thumbnail_signature_query,
        thumbnail_signature_version, THUMBNAIL_SIGNATURE_PROVIDER,
    };
    use crate::proxy_signature::{ProxySigningKey, ProxySigningKeyQueryExt};

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn route_ok<T>(result: Result<T, AppError>) -> TestResult<T> {
        result.map_err(|error| test_error(format!("route error: {error:?}")))
    }

    fn provider_ok<T>(result: Result<T, synctv_core::provider::ProviderError>) -> TestResult<T> {
        result.map_err(|error| test_error(error.to_string()))
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
    fn test_resolve_thumbnail_query_floors_zero_height_to_default() -> TestResult {
        let query = ThumbnailQuery {
            server_id: "emby-main".to_string(),
            credential_owner_id: None,
            max_height: Some(0),
            max_width: Some(0),
            _sig: None,
            _uid: None,
            _rid: None,
            _exp: None,
        };
        let (_, _, max_height, max_width) = route_ok(resolve_thumbnail_query(&query))?;

        assert_eq!(max_height, 300);
        assert_eq!(max_width, 0);
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
        let query_with_provider_version = format!("{raw_query}&pv=stale-provider-version");
        let filtered_with_provider_version =
            thumbnail_signature_query(&query_with_provider_version);
        assert!(!filtered_with_provider_version.contains("pv="));
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
    fn test_thumbnail_action_uses_server_side_token() -> TestResult {
        let action = provider_ok(EmbyProvider::thumbnail_action(
            "item-123",
            "https://emby.example.com/base",
            "secret-token",
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

    #[test]
    fn test_thumbnail_action_encodes_item_id_path_segment() -> TestResult {
        let action = provider_ok(EmbyProvider::thumbnail_action(
            "folder/item?x#y",
            "https://emby.example.com/base?ignored=true#fragment",
            "secret-token",
            0,
            0,
        ))?;

        match action {
            PlaybackTransportAction::FetchAndForward { url, headers, .. } => {
                assert_eq!(
                    url,
                    "https://emby.example.com/base/Items/folder%2Fitem%3Fx%23y/Images/Primary?quality=90"
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
