use axum::{
    extract::{Query, RawQuery, State},
    routing::{get, post},
    Json, Router,
};
use futures::FutureExt;
use serde::Deserialize;
use synctv_proto::providers::common::ProviderInstanceQuery;
use synctv_proto::providers::nextcloud::{
    GetBindsResponse, ListFavoritesRequest, ListRequest, LoginRequest, LogoutRequest,
    PollLoginFlowRequest, StartLoginFlowRequest,
};

use super::common::{
    execute_provider_user_endpoint, execute_provider_user_endpoint_with_control,
    provider_instance_name, provider_instance_name_from_request_field, provider_request_metadata,
};
use crate::http::{middleware::RequestMetadata, validation::ProtoQuery, AppResult, AppState};
use crate::impls::EndpointRateLimitCategory;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreviewQuery {
    server_id: String,
    #[serde(default)]
    credential_owner_id: Option<String>,
    file_id: u64,
    #[serde(default = "default_size")]
    width: u32,
    #[serde(default = "default_size")]
    height: u32,
    #[serde(default = "default_crop")]
    crop: bool,
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
const fn default_crop() -> bool {
    true
}

pub(crate) fn nextcloud_auth_routes() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/login-flow/start", post(start_login_flow))
        .route("/login-flow/poll", post(poll_login_flow))
        .route("/logout", post(logout))
}

pub(crate) fn nextcloud_read_routes() -> Router<AppState> {
    Router::new()
        .route("/list", post(list))
        .route("/favorites", post(list_favorites))
        .route("/binds", get(binds))
        .route("/preview", get(preview))
}

macro_rules! user_post {
    ($name:ident, $path:literal, $req:ty, $resp:ty, $category:expr, $method:ident) => {
        #[cfg_attr(feature = "openapi", utoipa::path(post, path = $path, tag = "Provider", request_body = $req, responses((status = 200, body = $resp)), security(("bearer_auth" = []))))]
        pub(crate) async fn $name(request_meta: RequestMetadata, State(state): State<AppState>, Json(req): Json<$req>) -> AppResult<Json<$resp>> {
            let instance = provider_instance_name_from_request_field(&req.instance_name)?;
            let api = state.shared_api_runtime.nextcloud_api.clone();
            execute_provider_user_endpoint_with_control(&state, request_meta, $category, move |_control, auth| { async move { api.$method(auth.user_id, req, instance.as_deref()).await }.boxed() }).await
        }
    };
}

user_post!(
    login,
    "/api/providers/nextcloud/login",
    LoginRequest,
    synctv_proto::providers::nextcloud::LoginResponse,
    EndpointRateLimitCategory::Auth,
    login
);
user_post!(
    poll_login_flow,
    "/api/providers/nextcloud/login-flow/poll",
    PollLoginFlowRequest,
    synctv_proto::providers::nextcloud::LoginResponse,
    EndpointRateLimitCategory::Auth,
    poll_login_flow
);
user_post!(
    list,
    "/api/providers/nextcloud/list",
    ListRequest,
    synctv_proto::providers::nextcloud::ListResponse,
    EndpointRateLimitCategory::Read,
    list
);
user_post!(
    list_favorites,
    "/api/providers/nextcloud/favorites",
    ListFavoritesRequest,
    synctv_proto::providers::nextcloud::ListResponse,
    EndpointRateLimitCategory::Read,
    list_favorites
);

#[cfg_attr(feature = "openapi", utoipa::path(post, path = "/api/providers/nextcloud/login-flow/start", tag = "Provider", request_body = StartLoginFlowRequest, responses((status = 200, body = synctv_proto::providers::nextcloud::StartLoginFlowResponse)), security(("bearer_auth" = []))))]
pub(crate) async fn start_login_flow(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<StartLoginFlowRequest>,
) -> AppResult<Json<synctv_proto::providers::nextcloud::StartLoginFlowResponse>> {
    let api = state.shared_api_runtime.nextcloud_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Auth,
        move |_auth| async move { api.start_login_flow(req).await }.boxed(),
    )
    .await
}

#[cfg_attr(feature = "openapi", utoipa::path(post, path = "/api/providers/nextcloud/logout", tag = "Provider", request_body = LogoutRequest, responses((status = 200, body = synctv_proto::providers::nextcloud::LogoutResponse)), security(("bearer_auth" = []))))]
pub(crate) async fn logout(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<LogoutRequest>,
) -> AppResult<Json<synctv_proto::providers::nextcloud::LogoutResponse>> {
    let api = state.shared_api_runtime.nextcloud_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        move |_control, auth| async move { api.logout(auth.user_id, req).await }.boxed(),
    )
    .await
}

#[cfg_attr(feature = "openapi", utoipa::path(get, path = "/api/providers/nextcloud/binds", tag = "Provider", responses((status = 200, body = GetBindsResponse)), security(("bearer_auth" = []))))]
pub(crate) async fn binds(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(query): ProtoQuery<ProviderInstanceQuery>,
) -> AppResult<Json<GetBindsResponse>> {
    let instance = provider_instance_name(&query)?.map(str::to_string);
    let api = state.shared_api_runtime.nextcloud_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |auth| async move { api.binds(auth.user_id, instance.as_deref()).await }.boxed(),
    )
    .await
}

#[cfg_attr(feature = "openapi", utoipa::path(get, path = "/api/providers/nextcloud/preview", tag = "Provider", params(("serverId" = String, Query), ("credentialOwnerId" = Option<String>, Query), ("fileId" = u64, Query), ("width" = u32, Query), ("height" = u32, Query), ("crop" = bool, Query)), responses((status = 200, description = "Nextcloud preview")), security(("bearer_auth" = []))))]
pub(crate) async fn preview(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Query(query): Query<PreviewQuery>,
    RawQuery(raw_query): RawQuery,
) -> AppResult<axum::response::Response> {
    let server_id = query.server_id.trim().to_string();
    if server_id.is_empty() || query.file_id == 0 {
        return Err(crate::http::AppError::bad_request(
            "Nextcloud preview serverId and fileId are required",
        ));
    }
    let width = query.width.clamp(1, 2048);
    let height = query.height.clamp(1, 2048);
    let crop = query.crop;
    let requested_owner = query
        .credential_owner_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
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
                    let room_id = crate::nextcloud_preview_urls::verify_nextcloud_preview_access(
                        &state.shared_api_runtime.proxy_signing_key,
                        &public_user,
                        &raw_query,
                        crate::nextcloud_preview_urls::NextcloudPreviewScope {
                            server_id: &server_id,
                            credential_owner_id: &public_owner,
                            file_id: query.file_id,
                            width,
                            height,
                            crop,
                        },
                    )
                    .map_err(|error| match error {
                        crate::nextcloud_preview_urls::NextcloudPreviewAccessError::Invalid => {
                            crate::impls::ApiError::Authentication(
                                "Invalid Nextcloud preview signature".to_string(),
                            )
                        }
                        crate::nextcloud_preview_urls::NextcloudPreviewAccessError::WrongUser => {
                            crate::impls::ApiError::Authorization(
                                "Nextcloud preview URL is scoped to another user".to_string(),
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
                    .nextcloud_api
                    .preview_action(owner, &server_id, query.file_id, width, height, crop)
                    .await
                    .map_err(crate::impls::ApiError::from)
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;
    super::execute_playback_transport_with_state(&state, action, None).await
}
