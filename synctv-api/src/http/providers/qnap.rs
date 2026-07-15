use axum::{
    extract::{Query, RawQuery, State},
    routing::{get, post},
    Json, Router,
};
use futures::FutureExt;
use serde::Deserialize;

use super::common::{
    execute_provider_user_endpoint, execute_provider_user_endpoint_with_control,
    provider_instance_name, provider_instance_name_from_request_field, provider_request_metadata,
};
use crate::http::{middleware::RequestMetadata, validation::ProtoQuery, AppResult, AppState};
use crate::impls::EndpointRateLimitCategory;
use synctv_proto::providers::common::ProviderInstanceQuery;
use synctv_proto::providers::qnap::{
    GetBindsResponse, GetCapabilitiesRequest, ListRequest, LoginRequest, LogoutRequest,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ThumbnailQuery {
    server_id: String,
    #[serde(default)]
    credential_owner_id: Option<String>,
    path: String,
    #[serde(default = "default_size")]
    size: u32,
    #[serde(default, rename = "sig")]
    _sig: Option<String>,
    #[serde(default, rename = "uid")]
    _uid: Option<String>,
    #[serde(default, rename = "rid")]
    _rid: Option<String>,
    #[serde(default, rename = "exp")]
    _exp: Option<i64>,
}

const fn default_size() -> u32 {
    640
}

pub(crate) fn qnap_auth_routes() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/logout", post(logout))
}

pub(crate) fn qnap_read_routes() -> Router<AppState> {
    Router::new()
        .route("/list", post(list))
        .route("/capabilities", post(capabilities))
        .route("/binds", get(binds))
        .route("/thumbnail", get(thumbnail))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/qnap/login",
        tag = "Provider",
        request_body = LoginRequest,
        responses((status = 200, body = synctv_proto::providers::qnap::LoginResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn login(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> AppResult<Json<synctv_proto::providers::qnap::LoginResponse>> {
    let instance = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.qnap_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Auth,
        move |_control, auth| {
            async move { api.login(auth.user_id, req, instance.as_deref()).await }.boxed()
        },
    )
    .await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/qnap/list",
        tag = "Provider",
        request_body = ListRequest,
        responses((status = 200, body = synctv_proto::providers::qnap::ListResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<ListRequest>,
) -> AppResult<Json<synctv_proto::providers::qnap::ListResponse>> {
    let instance = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.qnap_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |_control, auth| {
            async move { api.list(auth.user_id, req, instance.as_deref()).await }.boxed()
        },
    )
    .await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/qnap/capabilities",
        tag = "Provider",
        request_body = GetCapabilitiesRequest,
        responses((
            status = 200,
            body = synctv_proto::providers::qnap::GetCapabilitiesResponse
        )),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn capabilities(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<GetCapabilitiesRequest>,
) -> AppResult<Json<synctv_proto::providers::qnap::GetCapabilitiesResponse>> {
    let instance = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.qnap_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |_control, auth| {
            async move {
                api.capabilities(auth.user_id, req, instance.as_deref())
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
        post,
        path = "/api/providers/qnap/logout",
        tag = "Provider",
        request_body = LogoutRequest,
        responses((status = 200, body = synctv_proto::providers::qnap::LogoutResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn logout(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<LogoutRequest>,
) -> AppResult<Json<synctv_proto::providers::qnap::LogoutResponse>> {
    let instance = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.qnap_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        move |_control, auth| {
            async move { api.logout(auth.user_id, req, instance.as_deref()).await }.boxed()
        },
    )
    .await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/providers/qnap/binds",
        tag = "Provider",
        responses((status = 200, body = GetBindsResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn binds(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(query): ProtoQuery<ProviderInstanceQuery>,
) -> AppResult<Json<GetBindsResponse>> {
    let instance = provider_instance_name(&query)?.map(str::to_string);
    let api = state.shared_api_runtime.qnap_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |auth| async move { api.binds(auth.user_id, instance.as_deref()).await }.boxed(),
    )
    .await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/providers/qnap/thumbnail",
        tag = "Provider",
        params(
            ("serverId" = String, Query),
            ("credentialOwnerId" = Option<String>, Query),
            ("path" = String, Query),
            ("size" = u32, Query)
        ),
        responses((status = 200, description = "QNAP thumbnail")),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn thumbnail(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Query(query): Query<ThumbnailQuery>,
    RawQuery(raw_query): RawQuery,
) -> AppResult<axum::response::Response> {
    let server_id = query.server_id.trim().to_string();
    let path = query.path.trim().to_string();
    let requested_owner = query
        .credential_owner_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if server_id.is_empty() || path.is_empty() {
        return Err(crate::http::AppError::bad_request(
            "QNAP thumbnail parameters must not be empty",
        ));
    }
    let size = query.size.clamp(1, 640);
    let raw_query = raw_query.unwrap_or_default();
    let operation_state = state.clone();
    let metadata = provider_request_metadata(request_meta);
    let action = state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint(
            &metadata,
            EndpointRateLimitCategory::Read,
            move |auth| async move {
                let state = operation_state;
                let public_user = state
                    .shared_api_runtime
                    .public_id_codec
                    .encode_user_id(auth.user_id)
                    .map_err(crate::impls::ApiError::Internal)?;
                let public_owner = requested_owner
                    .clone()
                    .unwrap_or_else(|| public_user.clone());
                let signed =
                    url::form_urlencoded::parse(raw_query.as_bytes()).any(|(key, _)| key == "sig");
                if signed || public_owner != public_user {
                    let room_id = crate::qnap_thumbnail_urls::verify_qnap_thumbnail_access(
                        &state.shared_api_runtime.proxy_signing_key,
                        &public_user,
                        &raw_query,
                        crate::qnap_thumbnail_urls::QnapThumbnailScope {
                            server_id: &server_id,
                            credential_owner_id: &public_owner,
                            path: &path,
                            size,
                        },
                    )
                    .map_err(|error| match error {
                        crate::qnap_thumbnail_urls::QnapThumbnailAccessError::Invalid => {
                            crate::impls::ApiError::Authentication(
                                "Invalid QNAP thumbnail signature".to_string(),
                            )
                        }
                        crate::qnap_thumbnail_urls::QnapThumbnailAccessError::WrongUser => {
                            crate::impls::ApiError::Authorization(
                                "QNAP thumbnail URL is scoped to another user".to_string(),
                            )
                        }
                    })?;
                    let room_id = state
                        .shared_api_runtime
                        .public_id_codec
                        .decode_room_id(&room_id)
                        .map_err(crate::impls::ApiError::InvalidInput)?;
                    super::playback_provider::playback_provider_api_runtime(&state)
                        .validate_fresh_access(&room_id, &auth.user_id)
                        .await?;
                }
                let owner = state
                    .shared_api_runtime
                    .public_id_codec
                    .decode_user_id(&public_owner)
                    .map_err(crate::impls::ApiError::InvalidInput)?;
                state
                    .shared_api_runtime
                    .qnap_api
                    .thumbnail_action(owner, &server_id, &path, size)
                    .await
                    .map_err(crate::impls::ApiError::from)
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;
    super::execute_playback_transport_with_state(&state, action, None).await
}
