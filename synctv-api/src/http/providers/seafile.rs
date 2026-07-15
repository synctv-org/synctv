use axum::{
    extract::{Query, RawQuery, State},
    routing::{get, post},
    Json, Router,
};
use futures::FutureExt;
use serde::Deserialize;
use synctv_proto::providers::common::ProviderInstanceQuery;
use synctv_proto::providers::seafile::{
    GetBindsResponse, ListRepositoriesRequest, ListRequest, ListStarredRequest, LoginRequest,
    LogoutRequest, UnlockLibraryRequest,
};

use super::common::{
    execute_provider_user_endpoint, execute_provider_user_endpoint_with_control,
    provider_instance_name, provider_instance_name_from_request_field, provider_request_metadata,
};
use crate::http::{middleware::RequestMetadata, validation::ProtoQuery, AppResult, AppState};
use crate::impls::EndpointRateLimitCategory;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ThumbnailQuery {
    server_id: String,
    #[serde(default)]
    credential_owner_id: Option<String>,
    repository_id: String,
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

pub(crate) fn seafile_auth_routes() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/unlock-library", post(unlock_library))
        .route("/logout", post(logout))
}

pub(crate) fn seafile_read_routes() -> Router<AppState> {
    Router::new()
        .route("/repositories", post(list_repositories))
        .route("/list", post(list))
        .route("/starred", post(list_starred))
        .route("/binds", get(binds))
        .route("/thumbnail", get(thumbnail))
}

macro_rules! user_post {
    ($name:ident, $path:literal, $req:ty, $resp:ty, $category:expr, $method:ident) => {
        #[cfg_attr(feature = "openapi", utoipa::path(post, path = $path, tag = "Provider", request_body = $req, responses((status = 200, body = $resp)), security(("bearer_auth" = []))))]
        pub(crate) async fn $name(request_meta: RequestMetadata, State(state): State<AppState>, Json(req): Json<$req>) -> AppResult<Json<$resp>> {
            let instance = provider_instance_name_from_request_field(&req.instance_name)?;
            let api = state.shared_api_runtime.seafile_api.clone();
            execute_provider_user_endpoint_with_control(&state, request_meta, $category, move |_control, auth| async move { api.$method(auth.user_id, req, instance.as_deref()).await }.boxed()).await
        }
    };
}

user_post!(
    login,
    "/api/providers/seafile/login",
    LoginRequest,
    synctv_proto::providers::seafile::LoginResponse,
    EndpointRateLimitCategory::Auth,
    login
);
user_post!(
    unlock_library,
    "/api/providers/seafile/unlock-library",
    UnlockLibraryRequest,
    synctv_proto::providers::seafile::UnlockLibraryResponse,
    EndpointRateLimitCategory::Write,
    unlock_library
);
user_post!(
    list_repositories,
    "/api/providers/seafile/repositories",
    ListRepositoriesRequest,
    synctv_proto::providers::seafile::ListResponse,
    EndpointRateLimitCategory::Read,
    list_repositories
);
user_post!(
    list,
    "/api/providers/seafile/list",
    ListRequest,
    synctv_proto::providers::seafile::ListResponse,
    EndpointRateLimitCategory::Read,
    list
);
user_post!(
    list_starred,
    "/api/providers/seafile/starred",
    ListStarredRequest,
    synctv_proto::providers::seafile::ListResponse,
    EndpointRateLimitCategory::Read,
    list_starred
);
user_post!(
    logout,
    "/api/providers/seafile/logout",
    LogoutRequest,
    synctv_proto::providers::seafile::LogoutResponse,
    EndpointRateLimitCategory::Write,
    logout
);

#[cfg_attr(feature = "openapi", utoipa::path(get, path = "/api/providers/seafile/binds", tag = "Provider", responses((status = 200, body = GetBindsResponse)), security(("bearer_auth" = []))))]
pub(crate) async fn binds(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(query): ProtoQuery<ProviderInstanceQuery>,
) -> AppResult<Json<GetBindsResponse>> {
    let instance = provider_instance_name(&query)?.map(str::to_string);
    let api = state.shared_api_runtime.seafile_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |auth| async move { api.binds(auth.user_id, instance.as_deref()).await }.boxed(),
    )
    .await
}

#[cfg_attr(feature = "openapi", utoipa::path(get, path = "/api/providers/seafile/thumbnail", tag = "Provider", params(("serverId" = String, Query), ("credentialOwnerId" = Option<String>, Query), ("repositoryId" = String, Query), ("path" = String, Query), ("size" = u32, Query)), responses((status = 200, description = "Seafile thumbnail")), security(("bearer_auth" = []))))]
pub(crate) async fn thumbnail(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Query(query): Query<ThumbnailQuery>,
    RawQuery(raw_query): RawQuery,
) -> AppResult<axum::response::Response> {
    let server_id = query.server_id.trim().to_string();
    let repository_id = query.repository_id.trim().to_string();
    let path = query.path.trim().to_string();
    if server_id.is_empty() || repository_id.is_empty() || path.is_empty() {
        return Err(crate::http::AppError::bad_request(
            "Seafile thumbnail parameters are required",
        ));
    }
    let size = query.size.clamp(32, 2048);
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
                    let room_id = crate::seafile_thumbnail_urls::verify_seafile_thumbnail_access(
                        &state.shared_api_runtime.proxy_signing_key,
                        &public_user,
                        &raw_query,
                        crate::seafile_thumbnail_urls::SeafileThumbnailScope {
                            server_id: &server_id,
                            credential_owner_id: &public_owner,
                            repository_id: &repository_id,
                            path: &path,
                            size,
                        },
                    )
                    .map_err(|error| match error {
                        crate::seafile_thumbnail_urls::SeafileThumbnailAccessError::Invalid => {
                            crate::impls::ApiError::Authentication(
                                "Invalid Seafile thumbnail signature".to_string(),
                            )
                        }
                        crate::seafile_thumbnail_urls::SeafileThumbnailAccessError::WrongUser => {
                            crate::impls::ApiError::Authorization(
                                "Seafile thumbnail URL is scoped to another user".to_string(),
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
                    .seafile_api
                    .thumbnail_action(owner, &server_id, &repository_id, &path, size)
                    .await
                    .map_err(crate::impls::ApiError::from)
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;
    super::execute_playback_transport_with_state(&state, action, None).await
}
