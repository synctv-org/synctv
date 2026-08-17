use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use futures::FutureExt;
use serde::Deserialize;

use crate::http::{middleware::RequestMetadata, validation::ProtoQuery, AppResult, AppState};
use synctv_api_common::impls::EndpointRateLimitCategory;
use synctv_proto::providers::common::ProviderInstanceQuery;
use synctv_proto::providers::fnos::{
    GetBindsResponse, GetServerInfoRequest, ListMediaItemsRequest, ListMediaLibrariesRequest,
    ListRequest, LoginRequest, LogoutRequest, SetFavoriteRequest, SetWatchedRequest,
};

use super::common::{
    execute_provider_user_endpoint, execute_provider_user_endpoint_with_control,
    provider_instance_name, provider_instance_name_from_request_field, provider_request_metadata,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ThumbnailQuery {
    server_id: String,
    image_path: String,
    #[serde(default = "default_thumbnail_width")]
    width: u32,
}

const fn default_thumbnail_width() -> u32 {
    800
}

pub(crate) fn fnos_auth_routes() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/logout", post(logout))
}

pub(crate) fn fnos_read_routes() -> Router<AppState> {
    Router::new()
        .route("/list", post(list))
        .route("/media-libraries", post(media_libraries))
        .route("/media-items", post(media_items))
        .route("/media-favorite", post(set_favorite))
        .route("/media-watched", post(set_watched))
        .route("/server-info", post(server_info))
        .route("/binds", get(binds))
        .route("/thumbnail", get(thumbnail))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/providers/fnos/thumbnail",
        tag = "Provider",
        params(
            ("serverId" = String, Query),
            ("imagePath" = String, Query),
            ("width" = u32, Query)
        ),
        responses((status = 200, description = "Proxied FNOS media image")),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn thumbnail(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Query(query): Query<ThumbnailQuery>,
) -> AppResult<axum::response::Response> {
    let req = synctv_proto::providers::fnos::GetThumbnailRequest {
        server_id: query.server_id,
        image_path: query.image_path,
        width: query.width,
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
                    .fnos_api
                    .thumbnail_action(authenticated.user_id(), req)
                    .await
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;
    super::execute_provider_preview_transport(&state, action, None).await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/fnos/login",
        tag = "Provider",
        request_body = LoginRequest,
        responses(
            (
                status = 200,
                description = "FNOS login result",
                body = synctv_proto::providers::fnos::LoginResponse
            ),
            (
                status = 400,
                description = "Invalid FNOS login request",
                body = crate::openapi::GoogleRpcStatusSchema
            ),
            (
                status = 401,
                description = "Authentication required",
                body = crate::openapi::GoogleRpcStatusSchema
            ),
            (
                status = 429,
                description = "Rate limited",
                body = crate::openapi::GoogleRpcStatusSchema
            ),
            (
                status = 503,
                description = "FNOS unavailable",
                body = crate::openapi::GoogleRpcStatusSchema
            )
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn login(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> AppResult<Json<synctv_proto::providers::fnos::LoginResponse>> {
    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.fnos_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Auth,
        move |_control, authenticated| {
            async move {
                api.login(authenticated.user_id(), req, instance_name.as_deref())
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
        path = "/api/providers/fnos/list",
        tag = "Provider",
        request_body = ListRequest,
        responses(
            (
                status = 200,
                description = "FNOS directory listing",
                body = synctv_proto::providers::fnos::ListResponse
            ),
            (
                status = 400,
                description = "Invalid FNOS list request",
                body = crate::openapi::GoogleRpcStatusSchema
            ),
            (
                status = 401,
                description = "Authentication required",
                body = crate::openapi::GoogleRpcStatusSchema
            ),
            (
                status = 429,
                description = "Rate limited",
                body = crate::openapi::GoogleRpcStatusSchema
            ),
            (
                status = 503,
                description = "FNOS unavailable",
                body = crate::openapi::GoogleRpcStatusSchema
            )
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<ListRequest>,
) -> AppResult<Json<synctv_proto::providers::fnos::ListResponse>> {
    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.fnos_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |_control, authenticated| {
            async move {
                api.list(authenticated.user_id(), req, instance_name.as_deref())
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
        path = "/api/providers/fnos/media-libraries",
        tag = "Provider",
        request_body = ListMediaLibrariesRequest,
        responses((
            status = 200,
            description = "FNOS media libraries",
            body = synctv_proto::providers::fnos::ListMediaLibrariesResponse
        )),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn media_libraries(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<ListMediaLibrariesRequest>,
) -> AppResult<Json<synctv_proto::providers::fnos::ListMediaLibrariesResponse>> {
    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.fnos_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |_control, authenticated| {
            async move {
                api.list_media_libraries(authenticated.user_id(), req, instance_name.as_deref())
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
        path = "/api/providers/fnos/media-items",
        tag = "Provider",
        request_body = ListMediaItemsRequest,
        responses((
            status = 200,
            description = "FNOS media items",
            body = synctv_proto::providers::fnos::ListMediaItemsResponse
        )),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn media_items(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<ListMediaItemsRequest>,
) -> AppResult<Json<synctv_proto::providers::fnos::ListMediaItemsResponse>> {
    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.fnos_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |_control, authenticated| {
            async move {
                api.list_media_items(authenticated.user_id(), req, instance_name.as_deref())
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
        path = "/api/providers/fnos/media-favorite",
        tag = "Provider",
        request_body = SetFavoriteRequest,
        responses((
            status = 200,
            description = "FNOS favorite state updated",
            body = synctv_proto::providers::fnos::SetFavoriteResponse
        )),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn set_favorite(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<SetFavoriteRequest>,
) -> AppResult<Json<synctv_proto::providers::fnos::SetFavoriteResponse>> {
    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.fnos_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        move |_control, authenticated| {
            async move {
                api.set_favorite(authenticated.user_id(), req, instance_name.as_deref())
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
        path = "/api/providers/fnos/media-watched",
        tag = "Provider",
        request_body = SetWatchedRequest,
        responses((
            status = 200,
            description = "FNOS watched state updated",
            body = synctv_proto::providers::fnos::SetWatchedResponse
        )),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn set_watched(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<SetWatchedRequest>,
) -> AppResult<Json<synctv_proto::providers::fnos::SetWatchedResponse>> {
    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.fnos_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        move |_control, authenticated| {
            async move {
                api.set_watched(authenticated.user_id(), req, instance_name.as_deref())
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
        path = "/api/providers/fnos/server-info",
        tag = "Provider",
        request_body = GetServerInfoRequest,
        responses(
            (
                status = 200,
                description = "FNOS server information",
                body = synctv_proto::providers::fnos::GetServerInfoResponse
            ),
            (
                status = 400,
                description = "Invalid FNOS server request",
                body = crate::openapi::GoogleRpcStatusSchema
            ),
            (
                status = 401,
                description = "Authentication required",
                body = crate::openapi::GoogleRpcStatusSchema
            ),
            (
                status = 503,
                description = "FNOS unavailable",
                body = crate::openapi::GoogleRpcStatusSchema
            )
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn server_info(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<GetServerInfoRequest>,
) -> AppResult<Json<synctv_proto::providers::fnos::GetServerInfoResponse>> {
    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.fnos_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |_control, authenticated| {
            async move {
                api.get_server_info(authenticated.user_id(), req, instance_name.as_deref())
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
        path = "/api/providers/fnos/logout",
        tag = "Provider",
        request_body = LogoutRequest,
        responses(
            (
                status = 200,
                description = "FNOS credential removed",
                body = synctv_proto::providers::fnos::LogoutResponse
            ),
            (
                status = 400,
                description = "Invalid FNOS logout request",
                body = crate::openapi::GoogleRpcStatusSchema
            ),
            (
                status = 401,
                description = "Authentication required",
                body = crate::openapi::GoogleRpcStatusSchema
            ),
            (
                status = 503,
                description = "Credential storage unavailable",
                body = crate::openapi::GoogleRpcStatusSchema
            )
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn logout(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<LogoutRequest>,
) -> AppResult<Json<synctv_proto::providers::fnos::LogoutResponse>> {
    let api = state.shared_api_runtime.fnos_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Auth,
        move |_control, authenticated| {
            async move { api.logout(authenticated.user_id(), req).await }.boxed()
        },
    )
    .await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/providers/fnos/binds",
        tag = "Provider",
        params(ProviderInstanceQuery),
        responses(
            (status = 200, description = "Saved FNOS credentials", body = GetBindsResponse),
            (
                status = 400,
                description = "Invalid provider instance query",
                body = crate::openapi::GoogleRpcStatusSchema
            ),
            (
                status = 401,
                description = "Authentication required",
                body = crate::openapi::GoogleRpcStatusSchema
            ),
            (
                status = 503,
                description = "Credential storage unavailable",
                body = crate::openapi::GoogleRpcStatusSchema
            )
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn binds(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(query): ProtoQuery<ProviderInstanceQuery>,
) -> AppResult<Json<GetBindsResponse>> {
    let instance_name = provider_instance_name(&query)?.map(str::to_owned);
    let api = state.shared_api_runtime.fnos_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |authenticated| {
            async move {
                api.get_binds(authenticated.user_id(), instance_name.as_deref())
                    .await
            }
            .boxed()
        },
    )
    .await
}
