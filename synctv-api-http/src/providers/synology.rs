use axum::{
    extract::{Query, RawQuery, State},
    routing::{get, post},
    Json, Router,
};
use futures::FutureExt;
use serde::Deserialize;
use synctv_proto::providers::common::ProviderInstanceQuery;
use synctv_proto::providers::synology::{
    GetBindsResponse, ListEpisodesRequest, ListFilesRequest, ListHomeVideosRequest,
    ListLibrariesRequest, ListMoviesRequest, ListTvRecordingsRequest, ListTvShowsRequest,
    LoginRequest, LogoutRequest,
};

use super::common::{
    execute_provider_user_endpoint, execute_provider_user_endpoint_with_control,
    provider_instance_name, provider_instance_name_from_request_field, provider_request_metadata,
};
use crate::http::{middleware::RequestMetadata, validation::ProtoQuery, AppResult, AppState};
use synctv_api_common::impls::EndpointRateLimitCategory;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ImageQuery {
    kind: String,
    server_id: String,
    credential_owner_id: Option<String>,
    path: Option<String>,
    size: Option<String>,
    item_id: Option<i64>,
    media_type: Option<String>,
    poster_mtime: Option<String>,
    #[serde(default, rename = "sig")]
    _sig: Option<String>,
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
            ("credentialOwnerId" = Option<String>, Query),
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
    RawQuery(raw_query): RawQuery,
) -> AppResult<axum::response::Response> {
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
                    .encode_user_id(auth.user_id())
                    .map_err(synctv_api_common::impls::ApiError::Internal)?;
                let public_owner = query
                    .credential_owner_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or(&public_user)
                    .to_string();
                let scope = image_scope(&query, &public_owner)?;
                let signed =
                    url::form_urlencoded::parse(raw_query.as_bytes()).any(|(key, _)| key == "sig");
                if signed || public_owner != public_user {
                    let room_id = synctv_api_common::synology_image_urls::verify_synology_image_access(
                        &state.shared_api_runtime.proxy_signing_key,
                        &public_user,
                        &raw_query,
                        scope,
                    )
                    .map_err(|error| match error {
                        synctv_api_common::synology_image_urls::SynologyImageAccessError::Invalid => {
                            synctv_api_common::impls::ApiError::Authentication(
                                "Invalid Synology image signature".to_string(),
                            )
                        }
                        synctv_api_common::synology_image_urls::SynologyImageAccessError::WrongUser => {
                            synctv_api_common::impls::ApiError::Authorization(
                                "Synology image URL is scoped to another user".to_string(),
                            )
                        }
                    })?;
                    let room_id = state
                        .shared_api_runtime
                        .public_id_codec
                        .decode_room_id(&room_id)
                        .map_err(synctv_api_common::impls::ApiError::InvalidInput)?;
                    super::playback_provider::playback_provider_api_runtime(&state)
                        .validate_fresh_access(&room_id, &auth.user_id())
                        .await?;
                }
                let owner = state
                    .shared_api_runtime
                    .public_id_codec
                    .decode_user_id(&public_owner)
                    .map_err(synctv_api_common::impls::ApiError::InvalidInput)?;
                match scope {
                    synctv_api_common::synology_image_urls::SynologyImageScope::File {
                        server_id,
                        path,
                        size,
                        ..
                    } => state
                        .shared_api_runtime
                        .synology_api
                        .file_thumbnail_action(owner, server_id, path, size)
                        .await
                        .map_err(synctv_api_common::impls::ApiError::from),
                    synctv_api_common::synology_image_urls::SynologyImageScope::Poster {
                        server_id,
                        item_id,
                        media_type,
                        poster_mtime,
                        ..
                    } => state
                        .shared_api_runtime
                        .synology_api
                        .poster_action(owner, server_id, item_id, media_type, poster_mtime)
                        .await
                        .map_err(synctv_api_common::impls::ApiError::from),
                }
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;
    super::execute_playback_transport_with_state(&state, action, None).await
}

fn image_scope<'a>(
    query: &'a ImageQuery,
    credential_owner_id: &'a str,
) -> Result<
    synctv_api_common::synology_image_urls::SynologyImageScope<'a>,
    synctv_api_common::impls::ApiError,
> {
    let server_id = required(&query.server_id, "serverId")?;
    match query.kind.trim() {
        "file" => Ok(
            synctv_api_common::synology_image_urls::SynologyImageScope::File {
                server_id,
                credential_owner_id,
                path: required(query.path.as_deref().unwrap_or_default(), "path")?,
                size: required(query.size.as_deref().unwrap_or("medium"), "size")?,
            },
        ),
        "poster" => Ok(
            synctv_api_common::synology_image_urls::SynologyImageScope::Poster {
                server_id,
                credential_owner_id,
                item_id: query.item_id.filter(|value| *value > 0).ok_or_else(|| {
                    synctv_api_common::impls::ApiError::InvalidInput(
                        "itemId must be greater than zero".to_string(),
                    )
                })?,
                media_type: required(query.media_type.as_deref().unwrap_or_default(), "mediaType")?,
                poster_mtime: query.poster_mtime.as_deref(),
            },
        ),
        _ => Err(synctv_api_common::impls::ApiError::InvalidInput(
            "Synology image kind must be file or poster".to_string(),
        )),
    }
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
