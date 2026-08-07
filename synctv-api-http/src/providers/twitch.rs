use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use futures::FutureExt;

use crate::http::{middleware::RequestMetadata, validation::ProtoQuery, AppResult, AppState};
use synctv_api_common::impls::EndpointRateLimitCategory;
use synctv_proto::providers::common::ProviderInstanceQuery;
use synctv_proto::providers::twitch::{
    BindRequest, BindResponse, GetBindsResponse, ListCategoryStreamsRequest,
    ListCategoryStreamsResponse, ListChannelItemsRequest, ListChannelItemsResponse,
    ListFollowedLiveRequest, ListFollowedLiveResponse, ListScheduleRequest, ListScheduleResponse,
    ListTopCategoriesRequest, ListTopCategoriesResponse, ResolveRequest, ResolveResponse,
    SearchLiveChannelsRequest, SearchLiveChannelsResponse, UnbindRequest,
};

use super::common::{
    execute_provider_user_endpoint, execute_provider_user_endpoint_with_control,
    provider_instance_name, provider_instance_name_from_request_field,
};

pub(crate) fn twitch_auth_routes() -> Router<AppState> {
    Router::new()
        .route("/bind", post(bind))
        .route("/unbind", post(unbind))
}

pub(crate) fn twitch_read_routes() -> Router<AppState> {
    Router::new()
        .route("/binds", get(binds))
        .route("/resolve", post(resolve))
        .route("/channel-items", post(list_channel_items))
        .route("/followed-live", post(list_followed_live))
        .route("/category-streams", post(list_category_streams))
        .route("/top-categories", post(list_top_categories))
        .route("/search-live", post(search_live_channels))
        .route("/schedule", post(list_schedule))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/twitch/bind",
        tag = "Provider",
        request_body = BindRequest,
        responses(
            (status = 200, description = "Twitch credential bound", body = BindResponse),
            (
                status = 400,
                description = "Invalid Twitch credential",
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
                description = "Twitch unavailable",
                body = crate::openapi::GoogleRpcStatusSchema
            )
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn bind(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<BindRequest>,
) -> AppResult<Json<BindResponse>> {
    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.twitch_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Auth,
        move |_control, authenticated| {
            async move {
                api.bind(authenticated.user_id, req, instance_name.as_deref())
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
        path = "/api/providers/twitch/binds",
        tag = "Provider",
        params(ProviderInstanceQuery),
        responses(
            (status = 200, description = "Saved Twitch credentials", body = GetBindsResponse),
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
    let api = state.shared_api_runtime.twitch_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |authenticated| {
            async move {
                api.get_binds(authenticated.user_id, instance_name.as_deref())
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
        path = "/api/providers/twitch/unbind",
        tag = "Provider",
        request_body = UnbindRequest,
        responses(
            (
                status = 200,
                description = "Twitch credential removed",
                body = synctv_proto::providers::twitch::UnbindResponse
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
pub(crate) async fn unbind(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<UnbindRequest>,
) -> AppResult<Json<synctv_proto::providers::twitch::UnbindResponse>> {
    let api = state.shared_api_runtime.twitch_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Auth,
        move |authenticated| async move { api.unbind(authenticated.user_id, req).await }.boxed(),
    )
    .await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/twitch/resolve",
        tag = "Provider",
        request_body = ResolveRequest,
        responses((status = 200, description = "Resolved Twitch live, VOD, or clip", body = ResolveResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn resolve(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<ResolveRequest>,
) -> AppResult<Json<ResolveResponse>> {
    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.twitch_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |authenticated| {
            async move {
                api.resolve(authenticated.user_id, req, instance_name.as_deref())
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
        path = "/api/providers/twitch/channel-items",
        tag = "Provider",
        request_body = ListChannelItemsRequest,
        responses((status = 200, description = "Twitch channel items cursor page", body = ListChannelItemsResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_channel_items(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<ListChannelItemsRequest>,
) -> AppResult<Json<ListChannelItemsResponse>> {
    let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.twitch_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |authenticated| {
            async move {
                api.list_channel_items(authenticated.user_id, req, instance_name.as_deref())
                    .await
            }
            .boxed()
        },
    )
    .await
}

macro_rules! twitch_read_endpoint {
    ($handler:ident, $request:ty, $response:ty, $method:ident, $path:literal, $description:literal) => {
        #[cfg_attr(
                    feature = "openapi",
                    utoipa::path(
                        post,
                        path = $path,
                        tag = "Provider",
                        request_body = $request,
                        responses((status = 200, description = $description, body = $response)),
                        security(("bearer_auth" = []))
                    )
                )]
        pub(crate) async fn $handler(
            request_meta: RequestMetadata,
            State(state): State<AppState>,
            Json(req): Json<$request>,
        ) -> AppResult<Json<$response>> {
            let instance_name = provider_instance_name_from_request_field(&req.instance_name)?;
            let api = state.shared_api_runtime.twitch_api.clone();
            execute_provider_user_endpoint(
                &state,
                request_meta,
                EndpointRateLimitCategory::Read,
                move |authenticated| {
                    async move {
                        api.$method(authenticated.user_id, req, instance_name.as_deref())
                            .await
                    }
                    .boxed()
                },
            )
            .await
        }
    };
}

twitch_read_endpoint!(
    list_followed_live,
    ListFollowedLiveRequest,
    ListFollowedLiveResponse,
    list_followed_live,
    "/api/providers/twitch/followed-live",
    "Live channels followed by the authenticated Twitch user"
);
twitch_read_endpoint!(
    list_category_streams,
    ListCategoryStreamsRequest,
    ListCategoryStreamsResponse,
    list_category_streams,
    "/api/providers/twitch/category-streams",
    "Live Twitch streams in a category"
);
twitch_read_endpoint!(
    list_top_categories,
    ListTopCategoriesRequest,
    ListTopCategoriesResponse,
    list_top_categories,
    "/api/providers/twitch/top-categories",
    "Top Twitch categories"
);
twitch_read_endpoint!(
    search_live_channels,
    SearchLiveChannelsRequest,
    SearchLiveChannelsResponse,
    search_live_channels,
    "/api/providers/twitch/search-live",
    "Live Twitch channel search results"
);
twitch_read_endpoint!(
    list_schedule,
    ListScheduleRequest,
    ListScheduleResponse,
    list_schedule,
    "/api/providers/twitch/schedule",
    "Twitch broadcaster schedule"
);
