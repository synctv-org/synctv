use axum::{
    extract::{Path, Query, RawQuery, State},
    http::{HeaderMap, Method},
};
use futures::FutureExt;
use synctv_proto::playback_provider::fnos::{
    FnosImageResourceResponse, FnosResourceResponse, FnosSegmentResponse, FnosSubtitleResponse,
    FnosThumbnailResponse, GetFnosImageResourceRequest, GetFnosResourceRequest,
    GetFnosSegmentRequest, GetFnosSubtitleRequest, GetFnosThumbnailRequest,
};

use crate::http::{middleware::RequestMetadata, AppResult, AppState};
use crate::providers::playback_provider::transport::{
    query, range_header, signed_query_fields, stream_http_response, target_url,
    PlaybackProviderHttpResponse,
};

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FnosResourcePath {
    pub room_id: String,
    pub version: String,
    pub mode_name: String,
    pub media_index: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FnosSubtitlePath {
    pub room_id: String,
    pub version: String,
    pub mode_name: String,
    pub subtitle_index: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FnosImageResourceQuery {
    pub server_id: String,
    pub credential_owner_id: String,
    pub image_path: String,
    pub width: u32,
    pub sig: String,
    pub uid: String,
    pub exp: i64,
}

impl PlaybackProviderHttpResponse for FnosResourceResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for FnosSegmentResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for FnosSubtitleResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for FnosThumbnailResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for FnosImageResourceResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/playback-providers/{roomId}/fnos/{version}/resources/{modeName}/{mediaIndex}",
        tag = "FNOS Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path), ("modeName" = String, Path),
            ("mediaIndex" = u32, Path), ("sig" = String, Query),
            ("uid" = String, Query), ("exp" = i64, Query)
        ),
        responses((status = 200, description = "FNOS WebDAV media resource"))
    )
)]
pub fn get_fnos_resource(
    Path(path): Path<FnosResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    fnos_resource(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::GET,
    )
}

pub fn head_fnos_resource(
    Path(path): Path<FnosResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    fnos_resource(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn fnos_resource(
    path: FnosResourcePath,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = GetFnosResourceRequest {
        version: path.version,
        mode_name: path.mode_name,
        media_index: path.media_index,
        sig,
        uid,
        rid,
        exp,
        range: range_header(&headers).map_err(crate::http::error::map_api_error)?,
        head: method == Method::HEAD,
    };
    let state_for_stream = state.clone();
    stream_http_response::<FnosResourceResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::fnos::get_fnos_resource(
                    fnos_deps(&state, Some(&request_control)),
                    req,
                )
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
        path = "/api/playback-providers/{roomId}/fnos/{version}/segments",
        tag = "FNOS Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path), ("targetUrl" = String, Query),
            ("sig" = String, Query), ("uid" = String, Query), ("exp" = i64, Query)
        ),
        responses((status = 200, description = "FNOS HLS segment"))
    )
)]
pub fn get_fnos_segment(
    Path((room_id, version)): Path<(String, String)>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    fnos_segment(
        room_id,
        version,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::GET,
    )
}

pub fn head_fnos_segment(
    Path((room_id, version)): Path<(String, String)>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    fnos_segment(
        room_id,
        version,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn fnos_segment(
    room_id: String,
    version: String,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string, &room_id).map_err(crate::http::error::map_api_error)?;
    let req = GetFnosSegmentRequest {
        version,
        target_url: target_url(&query_string).map_err(crate::http::error::map_api_error)?,
        sig,
        uid,
        rid,
        exp,
        range: range_header(&headers).map_err(crate::http::error::map_api_error)?,
        head: method == Method::HEAD,
    };
    let state_for_stream = state.clone();
    stream_http_response::<FnosSegmentResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::fnos::get_fnos_segment(
                    fnos_deps(&state, Some(&request_control)),
                    req,
                )
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
        path = "/api/playback-providers/{roomId}/fnos/{version}/subtitles/{modeName}/{subtitleIndex}",
        tag = "FNOS Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path), ("modeName" = String, Path),
            ("subtitleIndex" = u32, Path), ("sig" = String, Query),
            ("uid" = String, Query), ("exp" = i64, Query)
        ),
        responses((status = 200, description = "FNOS subtitle"))
    )
)]
pub async fn get_fnos_subtitle(
    Path(path): Path<FnosSubtitlePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = GetFnosSubtitleRequest {
        version: path.version,
        mode_name: path.mode_name,
        subtitle_index: path.subtitle_index,
        sig,
        uid,
        rid,
        exp,
    };
    let state_for_stream = state.clone();
    stream_http_response::<FnosSubtitleResponse, _>(
        state,
        request_meta,
        Method::GET,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::fnos::get_fnos_subtitle(
                    fnos_deps(&state, Some(&request_control)),
                    req,
                )
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
        path = "/api/playback-providers/{roomId}/fnos/{version}/thumbnail",
        tag = "FNOS Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path), ("sig" = String, Query),
            ("uid" = String, Query), ("exp" = i64, Query)
        ),
        responses((status = 200, description = "FNOS media thumbnail"))
    )
)]
pub async fn get_fnos_thumbnail(
    Path((room_id, version)): Path<(String, String)>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string, &room_id).map_err(crate::http::error::map_api_error)?;
    let req = GetFnosThumbnailRequest {
        version,
        sig,
        uid,
        rid,
        exp,
    };
    let state_for_stream = state.clone();
    stream_http_response::<FnosThumbnailResponse, _>(
        state,
        request_meta,
        Method::GET,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::fnos::get_fnos_thumbnail(
                    fnos_deps(&state, Some(&request_control)),
                    req,
                )
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
        path = "/api/playback-providers/{roomId}/fnos/image",
        tag = "FNOS Playback Provider",
        params(
            ("roomId" = String, Path), ("serverId" = String, Query),
            ("credentialOwnerId" = String, Query), ("imagePath" = String, Query),
            ("width" = u32, Query), ("sig" = String, Query),
            ("uid" = String, Query), ("exp" = i64, Query)
        ),
        responses((status = 200, description = "Room-scoped FNOS image"))
    )
)]
pub async fn get_fnos_image_resource(
    Path(room_id): Path<String>,
    Query(query): Query<FnosImageResourceQuery>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
) -> AppResult<axum::response::Response> {
    let req = GetFnosImageResourceRequest {
        server_id: query.server_id,
        credential_owner_id: query.credential_owner_id,
        image_path: query.image_path,
        width: query.width,
        sig: query.sig,
        uid: query.uid,
        rid: room_id,
        exp: query.exp,
    };
    let state_for_stream = state.clone();
    stream_http_response::<FnosImageResourceResponse, _>(
        state,
        request_meta,
        Method::GET,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::fnos::get_fnos_image_resource(
                    fnos_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        },
    )
    .await
}

fn fnos_deps<'a>(
    state: &'a AppState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> synctv_api_common::playback_provider::fnos::FnosPlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::fnos::FnosPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.fnos_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
