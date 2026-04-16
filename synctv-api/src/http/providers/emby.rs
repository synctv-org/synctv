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
use serde::Deserialize;
use sha2::{Digest, Sha256};
use synctv_core::models::ProviderCredential;
use synctv_core::provider::proxy::ProxyAction;
use synctv_core::service::{ProxySigningKey, ProxyUrlClaims};

use crate::http::{
    middleware::AuthUser, provider_common::provider_instance_name, validation::ValidatedQuery,
    AppError, AppResult, AppState,
};
use crate::proto::client::ProviderInstanceQuery;
use crate::proto::providers::emby::{BindInfo, GetBindsResponse};

use crate::impls::providers::get_provider_binds;

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
) -> ProxyAction {
    let thumbnail_path = if max_width > 0 {
        format!(
            "/Items/{item_id}/Images/Primary?maxHeight={max_height}&maxWidth={max_width}&quality=90"
        )
    } else {
        format!("/Items/{item_id}/Images/Primary?maxHeight={max_height}&quality=90")
    };

    ProxyAction::FetchAndForward {
        url: format!("{}{}", host.trim_end_matches('/'), thumbnail_path),
        headers: std::collections::HashMap::from([(
            "X-Emby-Token".to_string(),
            api_key.to_string(),
        )]),
    }
}

fn build_thumbnail_proxy_action_from_credential(
    item_id: &str,
    credential: &ProviderCredential,
    max_height: u32,
    max_width: u32,
) -> Result<ProxyAction, AppError> {
    if item_id.trim().is_empty() {
        return Err(AppError::bad_request("item_id must not be empty"));
    }

    match credential {
        ProviderCredential::Emby { host, api_key, .. } => Ok(build_thumbnail_proxy_action(
            item_id.trim(),
            host,
            api_key,
            max_height,
            max_width,
        )),
        _ => Err(AppError::internal(
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
    verify_signed_thumbnail_access(signing_key, auth_user_id, raw_query, scope).map(Some)
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
        + synctv_core::service::ProxySigningKey::default_expiry_secs();
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
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn login(
    auth: AuthUser,
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<ProviderInstanceQuery>,
    Json(req): Json<crate::proto::providers::emby::LoginRequest>,
) -> AppResult<Json<crate::proto::providers::emby::LoginResponse>> {
    tracing::info!("Emby login request");

    let instance_name = provider_instance_name(&query)?;
    let api = &state.emby_api;
    let resp = api
        .login(&auth.user_id.to_string(), req, instance_name)
        .await
        .map_err(|e| {
            tracing::error!("Emby login failed: {}", e);
            AppError::from(e)
        })?;
    Ok(Json(resp))
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
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<ProviderInstanceQuery>,
    Json(req): Json<crate::proto::providers::emby::ListRequest>,
) -> AppResult<Json<crate::proto::providers::emby::ListResponse>> {
    tracing::info!("Emby list request");

    let instance_name = provider_instance_name(&query)?;
    let api = &state.emby_api;
    let resp = api
        .list(&auth.user_id.to_string(), req, instance_name)
        .await
        .map_err(|e| {
            tracing::error!("Emby list failed: {}", e);
            AppError::from(e)
        })?;
    Ok(Json(resp))
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
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn me(
    auth: AuthUser,
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<ProviderInstanceQuery>,
    Json(req): Json<crate::proto::providers::emby::GetMeRequest>,
) -> AppResult<Json<crate::proto::providers::emby::GetMeResponse>> {
    tracing::info!("Emby me request");

    let instance_name = provider_instance_name(&query)?;
    let api = &state.emby_api;
    let resp = api
        .get_me(&auth.user_id.to_string(), req, instance_name)
        .await
        .map_err(|e| {
            tracing::error!("Emby me failed: {}", e);
            AppError::from(e)
        })?;
    Ok(Json(resp))
}

/// Logout from Emby (delete stored credential)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/emby/logout",
        tag = "Provider",
        request_body = crate::proto::providers::emby::LogoutRequest,
        responses(
            (status = 200, description = "Emby credential removed", body = crate::proto::providers::emby::LogoutResponse),
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
    Json(req): Json<crate::proto::providers::emby::LogoutRequest>,
) -> AppResult<Json<crate::proto::providers::emby::LogoutResponse>> {
    tracing::info!("Emby logout request");

    let api = &state.emby_api;
    let resp = api
        .logout(&auth.user_id.to_string(), req)
        .await
        .map_err(|e| {
            tracing::error!("Emby logout failed: {}", e);
            AppError::from(e)
        })?;
    Ok(Json(resp))
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
            (status = 503, description = "Provider bind information unavailable", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn binds(
    auth: crate::http::middleware::AuthUser,
    State(state): State<AppState>,
    ValidatedQuery(query): ValidatedQuery<ProviderInstanceQuery>,
) -> AppResult<Json<GetBindsResponse>> {
    tracing::info!("Emby binds request for user: {}", auth.user_id);
    let instance_name = provider_instance_name(&query)?;

    let provider_binds = get_provider_binds(
        &state.user_provider_credential_repository,
        &auth.user_id.to_string(),
        synctv_core::provider::EmbyProvider::NAME,
        "emby_user_id",
        instance_name,
    )
    .await
    .map_err(AppError::from)?;

    let emby_binds: Vec<_> = provider_binds
        .into_iter()
        .map(|b| BindInfo {
            id: b.id,
            host: b.host,
            user_id: b.label_value,
            created_at: b.created_at,
        })
        .collect();

    Ok(Json(GetBindsResponse { binds: emby_binds }))
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
            (status = 404, description = "Emby credential not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub(crate) async fn thumbnail(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(item_id): Path<String>,
    Query(query): Query<ThumbnailQuery>,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
) -> AppResult<axum::response::Response> {
    let (server_id, credential_owner_id, max_height, max_width) = resolve_thumbnail_query(&query)?;
    let raw_query = raw_query.as_deref().unwrap_or("");
    let scope = ThumbnailSignatureScope {
        item_id: &item_id,
        server_id,
        credential_owner_id: credential_owner_id.unwrap_or(auth.user_id.as_str()),
        max_height,
        max_width,
    };
    if let Some(room_id) = authorize_thumbnail_request(
        &state.proxy_signing_key,
        auth.user_id.as_str(),
        raw_query,
        credential_owner_id,
        scope,
    )? {
        super::validate_fresh_proxy_access(
            &state,
            &synctv_core::models::RoomId::from_string(room_id),
            &auth.user_id,
        )
        .await?;
    }

    let credential_lookup_user_id = credential_owner_id.unwrap_or_else(|| auth.user_id.as_str());

    let credential = state
        .user_provider_credential_repository
        .get_by_provider_and_server(
            credential_lookup_user_id,
            synctv_core::provider::EmbyProvider::NAME,
            server_id,
        )
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("Emby credential not found"))?;

    let parsed = credential.get_credential().map_err(|error| {
        AppError::internal(format!("Failed to parse stored Emby credential: {error}"))
    })?;
    let action =
        build_thumbnail_proxy_action_from_credential(&item_id, &parsed, max_height, max_width)?;

    super::execute_proxy_action_with_state(&state, action, &headers).await
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
            ProxyAction::FetchAndForward { url, headers } => {
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
