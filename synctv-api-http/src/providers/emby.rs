//! Emby Provider HTTP Routes
//!
//! Provider API endpoints for Emby login, listing, etc.
//! Playback transport routes live under the Emby playback-provider
//! modules, while thumbnail fetches use an authenticated route that resolves
//! Emby credentials server-side.

use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use futures::FutureExt;
use serde::Deserialize;

use crate::http::{
    error::map_api_error, middleware::RequestMetadata, validation::ProtoQuery, AppResult, AppState,
};
use synctv_api_common::impls::EndpointRateLimitCategory;
use synctv_proto::providers::common::ProviderInstanceQuery;
use synctv_proto::providers::emby::GetBindsResponse;

use super::common::{
    execute_provider_user_endpoint, execute_provider_user_endpoint_with_control,
    provider_instance_name, provider_instance_name_from_request_field, provider_request_metadata,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ThumbnailQuery {
    server_id: String,
    #[serde(default)]
    max_height: Option<u32>,
    #[serde(default)]
    max_width: Option<u32>,
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
                    &authenticated.user_id(),
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
                    &authenticated.user_id(),
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
                    &authenticated.user_id(),
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

    let api = state.shared_api_runtime.emby_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Auth,
        move |authenticated| async move { api.logout(&authenticated.user_id(), req).await }.boxed(),
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
                tracing::info!("Emby binds request for user: {}", authenticated.user_id());
                api.get_binds(&authenticated.user_id(), instance_name.as_deref())
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
) -> AppResult<axum::response::Response> {
    let req = synctv_proto::providers::emby::GetThumbnailRequest {
        server_id: query.server_id,
        item_id,
        max_height: query.max_height.unwrap_or_default(),
        max_width: query.max_width.unwrap_or_default(),
    };
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
                state
                    .shared_api_runtime
                    .emby_api
                    .thumbnail_action(&authenticated.user_id(), req, None)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;

    let response = super::execute_provider_preview_transport(&state, action, None).await?;

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use synctv_core::provider::EmbyProvider;
    use synctv_core::provider::PlaybackTransportAction;

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn provider_ok<T>(result: Result<T, synctv_core::provider::ProviderError>) -> TestResult<T> {
        result.map_err(|error| test_error(error.to_string()))
    }

    #[test]
    fn test_thumbnail_query_accepts_provider_resource_fields() -> TestResult {
        let query: ThumbnailQuery =
            serde_urlencoded::from_str("serverId=emby-main&maxHeight=300&maxWidth=640")?;

        assert_eq!(query.server_id, "emby-main");
        assert_eq!(query.max_height, Some(300));
        assert_eq!(query.max_width, Some(640));
        Ok(())
    }

    #[test]
    fn test_thumbnail_query_rejects_room_resource_fields() {
        for query in [
            "serverId=emby-main&credentialOwnerId=owner-1",
            "serverId=emby-main&sig=value",
            "serverId=emby-main&uid=user-1",
            "serverId=emby-main&rid=room-1",
            "serverId=emby-main&exp=1",
        ] {
            assert!(serde_urlencoded::from_str::<ThumbnailQuery>(query).is_err());
        }
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
