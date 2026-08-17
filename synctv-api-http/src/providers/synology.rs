use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use futures::FutureExt;
use serde::Deserialize;
use synctv_proto::providers::common::ProviderInstanceQuery;
use synctv_proto::providers::synology::{
    get_image_request, FileImageRequest, GetBindsResponse, GetImageRequest, ListEpisodesRequest,
    ListFilesRequest, ListHomeVideosRequest, ListLibrariesRequest, ListMoviesRequest,
    ListTvRecordingsRequest, ListTvShowsRequest, LoginRequest, LogoutRequest, PosterImageRequest,
};

use super::common::{
    execute_provider_user_endpoint, execute_provider_user_endpoint_with_control,
    provider_instance_name, provider_instance_name_from_request_field, provider_request_metadata,
};
use crate::http::{middleware::RequestMetadata, validation::ProtoQuery, AppResult, AppState};
use synctv_api_common::impls::EndpointRateLimitCategory;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ImageQuery {
    kind: String,
    server_id: String,
    path: Option<String>,
    size: Option<String>,
    item_id: Option<i64>,
    media_type: Option<String>,
    poster_mtime: Option<String>,
}

pub(crate) fn synology_auth_routes() -> Router<AppState> {
    Router::new()
        .route("/login", post(login))
        .route("/logout", post(logout))
}

pub(crate) fn synology_read_routes() -> Router<AppState> {
    Router::new()
        .route("/files", post(list_files))
        .route("/libraries", post(list_libraries))
        .route("/movies", post(list_movies))
        .route("/tv-shows", post(list_tv_shows))
        .route("/episodes", post(list_episodes))
        .route("/home-videos", post(list_home_videos))
        .route("/tv-recordings", post(list_tv_recordings))
        .route("/binds", get(binds))
        .route("/image", get(image))
}

macro_rules! user_endpoint {
    ($name:ident, $path:literal, $request:ty, $response:ty, $method:ident) => {
        #[cfg_attr(
                                                    feature = "openapi",
                                                    utoipa::path(
                                                        post,
                                                        path = $path,
                                                        tag = "Provider",
                                                        request_body = $request,
                                                        responses((status = 200, body = $response)),
                                                        security(("bearer_auth" = []))
                                                    )
                                                )]
        pub(crate) async fn $name(
            request_meta: RequestMetadata,
            State(state): State<AppState>,
            Json(req): Json<$request>,
        ) -> AppResult<Json<$response>> {
            let instance = provider_instance_name_from_request_field(&req.instance_name)?;
            let api = state.shared_api_runtime.synology_api.clone();
            execute_provider_user_endpoint_with_control(
                &state,
                request_meta,
                EndpointRateLimitCategory::Read,
                move |_control, auth| {
                    async move { api.$method(auth.user_id(), req, instance.as_deref()).await }.boxed()
                },
            )
            .await
        }
    };
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/synology/login",
        tag = "Provider",
        request_body = LoginRequest,
        responses((status = 200, body = synctv_proto::providers::synology::LoginResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn login(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> AppResult<Json<synctv_proto::providers::synology::LoginResponse>> {
    let instance = provider_instance_name_from_request_field(&req.instance_name)?;
    let api = state.shared_api_runtime.synology_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Auth,
        move |_control, auth| {
            async move { api.login(auth.user_id(), req, instance.as_deref()).await }.boxed()
        },
    )
    .await
}

user_endpoint!(
    list_files,
    "/api/providers/synology/files",
    ListFilesRequest,
    synctv_proto::providers::synology::ListFilesResponse,
    list_files
);
user_endpoint!(
    list_libraries,
    "/api/providers/synology/libraries",
    ListLibrariesRequest,
    synctv_proto::providers::synology::ListLibrariesResponse,
    list_libraries
);
user_endpoint!(
    list_movies,
    "/api/providers/synology/movies",
    ListMoviesRequest,
    synctv_proto::providers::synology::ListVideoItemsResponse,
    list_movies
);
user_endpoint!(
    list_tv_shows,
    "/api/providers/synology/tv-shows",
    ListTvShowsRequest,
    synctv_proto::providers::synology::ListVideoItemsResponse,
    list_tv_shows
);
user_endpoint!(
    list_episodes,
    "/api/providers/synology/episodes",
    ListEpisodesRequest,
    synctv_proto::providers::synology::ListVideoItemsResponse,
    list_episodes
);
user_endpoint!(
    list_home_videos,
    "/api/providers/synology/home-videos",
    ListHomeVideosRequest,
    synctv_proto::providers::synology::ListVideoItemsResponse,
    list_home_videos
);
user_endpoint!(
    list_tv_recordings,
    "/api/providers/synology/tv-recordings",
    ListTvRecordingsRequest,
    synctv_proto::providers::synology::ListVideoItemsResponse,
    list_tv_recordings
);

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/providers/synology/logout",
        tag = "Provider",
        request_body = LogoutRequest,
        responses((status = 200, body = synctv_proto::providers::synology::LogoutResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn logout(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<LogoutRequest>,
) -> AppResult<Json<synctv_proto::providers::synology::LogoutResponse>> {
    let api = state.shared_api_runtime.synology_api.clone();
    execute_provider_user_endpoint_with_control(
        &state,
        request_meta,
        EndpointRateLimitCategory::Write,
        move |_control, auth| async move { api.logout(auth.user_id(), req).await }.boxed(),
    )
    .await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/providers/synology/binds",
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
    let api = state.shared_api_runtime.synology_api.clone();
    execute_provider_user_endpoint(
        &state,
        request_meta,
        EndpointRateLimitCategory::Read,
        move |auth| async move { api.binds(auth.user_id(), instance.as_deref()).await }.boxed(),
    )
    .await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/providers/synology/image",
        tag = "Provider",
        params(
            ("kind" = String, Query),
            ("serverId" = String, Query),
            ("path" = Option<String>, Query),
            ("size" = Option<String>, Query),
            ("itemId" = Option<i64>, Query),
            ("mediaType" = Option<String>, Query),
            ("posterMtime" = Option<String>, Query)
        ),
        responses((status = 200, description = "Synology file thumbnail or Video Station poster")),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn image(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Query(query): Query<ImageQuery>,
) -> AppResult<axum::response::Response> {
    let operation_state = state.clone();
    let metadata = provider_request_metadata(request_meta);
    let req = provider_image_request(&query)?;
    let action = state
        .shared_api_runtime
        .client_api
        .execute_user_endpoint(
            &metadata,
            EndpointRateLimitCategory::Read,
            move |auth| async move {
                let state = operation_state;
                state
                    .shared_api_runtime
                    .synology_api
                    .image_action(auth.user_id(), req)
                    .await
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;
    super::execute_provider_preview_transport(&state, action, None).await
}

fn provider_image_request(
    query: &ImageQuery,
) -> Result<GetImageRequest, synctv_api_common::impls::ApiError> {
    let server_id = required(&query.server_id, "serverId")?.to_string();
    let image = match query.kind.trim() {
        "file" => get_image_request::Image::File(FileImageRequest {
            path: required(query.path.as_deref().unwrap_or_default(), "path")?.to_string(),
            size: required(query.size.as_deref().unwrap_or("medium"), "size")?.to_string(),
        }),
        "poster" => get_image_request::Image::Poster(PosterImageRequest {
            item_id: query.item_id.filter(|value| *value > 0).ok_or_else(|| {
                synctv_api_common::impls::ApiError::InvalidInput(
                    "itemId must be greater than zero".to_string(),
                )
            })?,
            media_type: required(query.media_type.as_deref().unwrap_or_default(), "mediaType")?
                .to_string(),
            poster_mtime: query.poster_mtime.clone(),
        }),
        _ => {
            return Err(synctv_api_common::impls::ApiError::InvalidInput(
                "Synology image kind must be file or poster".to_string(),
            ));
        }
    };
    Ok(GetImageRequest {
        server_id,
        image: Some(image),
    })
}

fn required<'a>(
    value: &'a str,
    field: &str,
) -> Result<&'a str, synctv_api_common::impls::ApiError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(synctv_api_common::impls::ApiError::InvalidInput(format!(
            "Synology image {field} is required"
        )));
    }
    Ok(value)
}
