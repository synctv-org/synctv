use axum::{
    extract::{Path, Query, RawQuery, State},
    http::{HeaderMap, Method},
    response::{
        sse::{KeepAlive, Sse},
        IntoResponse, Response,
    },
};
use base64::Engine as _;
use futures::{FutureExt, StreamExt as _};
use synctv_proto::playback_provider::bilibili::{
    BilibiliDanmakuFileResponse, BilibiliDashManifestMode, BilibiliDashManifestResponse,
    BilibiliDashResourceKind, BilibiliDashResourceResponse, BilibiliDynamicLiveDanmakuTarget,
    BilibiliHlsManifestResponse, BilibiliHlsResourceKind, BilibiliHlsResourceResponse,
    BilibiliMediaStreamResponse, BilibiliSubtitleResponse, GetBilibiliDanmakuFileRequest,
    GetBilibiliDashManifestRequest, GetBilibiliDashResourceRequest, GetBilibiliHlsManifestRequest,
    GetBilibiliHlsResourceRequest, GetBilibiliMediaStreamRequest, GetBilibiliSubtitleRequest,
    WatchBilibiliLiveDanmakuRequest,
};

use crate::http::{
    middleware::RequestMetadata, room::execute::execute_room_actor_endpoint_with_control,
    AppResult, AppState,
};
use crate::providers::playback_provider::transport::{
    bilibili_danmaku_sse_event, query, range_header, signed_query_fields, stream_http_response,
    target_url, unsigned_query_field, PlaybackProviderHttpResponse,
};
use synctv_api_common::impls::{EndpointRateLimitCategory, EndpointRateLimitScope};

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BilibiliIndexedPath {
    pub room_id: String,
    pub version: String,
    pub mode_name: String,
    pub url_index: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BilibiliDashManifestPath {
    pub room_id: String,
    pub version: String,
    pub mode_name: String,
    #[serde(default)]
    pub manifest_mode: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BilibiliHlsResourcePath {
    pub room_id: String,
    pub version: String,
    pub mode_name: String,
    pub media_index: u32,
    pub resource_kind: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BilibiliDashResourcePath {
    pub room_id: String,
    pub version: String,
    pub mode_name: String,
    pub resource_kind: String,
    pub scope: String,
    pub uid: String,
    pub exp: i64,
    pub sig: String,
    #[serde(default)]
    pub resource_path: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BilibiliSubtitlePath {
    pub room_id: String,
    pub version: String,
    pub mode_name: String,
    pub subtitle_index: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BilibiliDanmakuFilePath {
    pub room_id: String,
    pub version: String,
    pub danmaku_index: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BilibiliDynamicLiveDanmakuQuery {
    pub live_room_id: u64,
}

impl PlaybackProviderHttpResponse for BilibiliMediaStreamResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for BilibiliHlsManifestResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for BilibiliHlsResourceResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for BilibiliDashManifestResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for BilibiliDashResourceResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for BilibiliSubtitleResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for BilibiliDanmakuFileResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/playback-providers/{roomId}/bilibili/{version}/media-streams/{modeName}/{urlIndex}",
        tag = "Bilibili Playback Provider",
        params(("roomId" = String, Path), ("version" = String, Path), ("modeName" = String, Path), ("urlIndex" = u32, Path), ("sig" = String, Query), ("uid" = String, Query), ("exp" = i64, Query)),
        responses((status = 200, description = "Bilibili media stream"), (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema))
    )
)]
pub fn get_bilibili_media_stream(
    Path(path): Path<BilibiliIndexedPath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    bilibili_media_stream(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::GET,
    )
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        head,
        path = "/api/playback-providers/{roomId}/bilibili/{version}/media-streams/{modeName}/{urlIndex}",
        tag = "Bilibili Playback Provider",
        params(("roomId" = String, Path), ("version" = String, Path), ("modeName" = String, Path), ("urlIndex" = u32, Path), ("sig" = String, Query), ("uid" = String, Query), ("exp" = i64, Query)),
        responses((status = 200, description = "Bilibili media stream metadata"), (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema))
    )
)]
pub fn head_bilibili_media_stream(
    Path(path): Path<BilibiliIndexedPath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    bilibili_media_stream(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn bilibili_media_stream(
    path: BilibiliIndexedPath,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = GetBilibiliMediaStreamRequest {
        version: path.version,
        mode_name: path.mode_name,
        url_index: path.url_index,
        sig,
        uid,
        rid,
        exp,
        range: range_header(&headers).map_err(crate::http::error::map_api_error)?,
        head: method == Method::HEAD,
    };
    let state_for_stream = state.clone();
    stream_http_response::<BilibiliMediaStreamResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::bilibili::get_bilibili_media_stream(
                    bilibili_deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/{roomId}/bilibili/{version}/hls-manifests/{modeName}/{urlIndex}",
        tag = "Bilibili Playback Provider",
        params(("roomId" = String, Path), ("version" = String, Path), ("modeName" = String, Path), ("urlIndex" = u32, Path), ("sig" = String, Query), ("uid" = String, Query), ("exp" = i64, Query)),
        responses((status = 200, description = "Bilibili HLS manifest"), (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema))
    )
)]
pub async fn get_bilibili_hls_manifest(
    Path(path): Path<BilibiliIndexedPath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = GetBilibiliHlsManifestRequest {
        version: path.version,
        mode_name: path.mode_name,
        url_index: path.url_index,
        sig,
        uid,
        rid,
        exp,
    };
    let state_for_stream = state.clone();
    stream_http_response::<BilibiliHlsManifestResponse, _>(
        state,
        request_meta,
        Method::GET,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::bilibili::get_bilibili_hls_manifest(
                    bilibili_deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/{roomId}/bilibili/{version}/hls-resources/{modeName}/{mediaIndex}/{resourceKind}",
        tag = "Bilibili Playback Provider",
        params(("roomId" = String, Path), ("version" = String, Path), ("modeName" = String, Path), ("mediaIndex" = u32, Path), ("resourceKind" = String, Path), ("targetUrl" = String, Query), ("sig" = String, Query), ("uid" = String, Query), ("exp" = i64, Query)),
        responses((status = 200, description = "Bilibili HLS resource"), (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema))
    )
)]
pub fn get_bilibili_hls_resource(
    Path(path): Path<BilibiliHlsResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    bilibili_hls_resource(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::GET,
    )
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        head,
        path = "/api/playback-providers/{roomId}/bilibili/{version}/hls-resources/{modeName}/{mediaIndex}/{resourceKind}",
        tag = "Bilibili Playback Provider",
        params(("roomId" = String, Path), ("version" = String, Path), ("modeName" = String, Path), ("mediaIndex" = u32, Path), ("resourceKind" = String, Path), ("targetUrl" = String, Query), ("sig" = String, Query), ("uid" = String, Query), ("exp" = i64, Query)),
        responses((status = 200, description = "Bilibili HLS resource metadata"), (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema))
    )
)]
pub fn head_bilibili_hls_resource(
    Path(path): Path<BilibiliHlsResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    bilibili_hls_resource(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn bilibili_hls_resource(
    path: BilibiliHlsResourcePath,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = GetBilibiliHlsResourceRequest {
        version: path.version,
        target_url: target_url(&query_string).map_err(crate::http::error::map_api_error)?,
        sig,
        uid,
        rid,
        exp,
        range: range_header(&headers).map_err(crate::http::error::map_api_error)?,
        head: method == Method::HEAD,
        mode_name: path.mode_name,
        media_index: path.media_index,
        resource_kind: bilibili_hls_resource_kind(&path.resource_kind)?,
    };
    let state_for_stream = state.clone();
    stream_http_response::<BilibiliHlsResourceResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::bilibili::get_bilibili_hls_resource(
                    bilibili_deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/{roomId}/bilibili/{version}/dash-manifests/{modeName}/{manifestMode}",
        tag = "Bilibili Playback Provider",
        params(("roomId" = String, Path), ("version" = String, Path), ("modeName" = String, Path), ("manifestMode" = String, Path), ("sig" = String, Query), ("uid" = String, Query), ("exp" = i64, Query)),
        responses((status = 200, description = "Bilibili DASH manifest"), (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema))
    )
)]
pub async fn get_bilibili_dash_manifest(
    Path(path): Path<BilibiliDashManifestPath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = GetBilibiliDashManifestRequest {
        version: path.version,
        mode_name: path.mode_name,
        mode: dash_manifest_mode(path.manifest_mode.as_deref(), &query_string)
            .map_err(crate::http::error::map_api_error)?,
        sig,
        uid,
        rid,
        exp,
    };
    let state_for_stream = state.clone();
    stream_http_response::<BilibiliDashManifestResponse, _>(
        state,
        request_meta,
        Method::GET,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::bilibili::get_bilibili_dash_manifest(
                    bilibili_deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/{roomId}/bilibili/{version}/dash-resources/{modeName}/{resourceKind}/{scope}/{uid}/{exp}/{sig}/{resourcePath}",
        tag = "Bilibili Playback Provider",
        params(("roomId" = String, Path), ("version" = String, Path), ("modeName" = String, Path), ("resourceKind" = String, Path), ("scope" = String, Path), ("uid" = String, Path), ("exp" = i64, Path), ("sig" = String, Path), ("resourcePath" = String, Path)),
        responses((status = 200, description = "Bilibili DASH resource"), (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema))
    )
)]
pub fn get_bilibili_dash_resource(
    Path(path): Path<BilibiliDashResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    bilibili_dash_resource(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::GET,
    )
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        head,
        path = "/api/playback-providers/{roomId}/bilibili/{version}/dash-resources/{modeName}/{resourceKind}/{scope}/{uid}/{exp}/{sig}/{resourcePath}",
        tag = "Bilibili Playback Provider",
        params(("roomId" = String, Path), ("version" = String, Path), ("modeName" = String, Path), ("resourceKind" = String, Path), ("scope" = String, Path), ("uid" = String, Path), ("exp" = i64, Path), ("sig" = String, Path), ("resourcePath" = String, Path)),
        responses((status = 200, description = "Bilibili DASH resource metadata"), (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema))
    )
)]
pub fn head_bilibili_dash_resource(
    Path(path): Path<BilibiliDashResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    bilibili_dash_resource(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn bilibili_dash_resource(
    path: BilibiliDashResourcePath,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    resource_query: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let scope_url = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&path.scope)
        .map_err(|_| {
            crate::http::error::map_api_error(synctv_api_common::impls::ApiError::InvalidInput(
                "Invalid Bilibili DASH resource scope".to_string(),
            ))
        })?;
    let scope_url = String::from_utf8(scope_url).map_err(|_| {
        crate::http::error::map_api_error(synctv_api_common::impls::ApiError::InvalidInput(
            "Invalid Bilibili DASH scope encoding".to_string(),
        ))
    })?;
    let req = GetBilibiliDashResourceRequest {
        version: path.version,
        mode_name: path.mode_name,
        scope_url,
        resource_path: path.resource_path,
        resource_query: (!resource_query.is_empty()).then_some(resource_query),
        resource_kind: bilibili_dash_resource_kind(&path.resource_kind)?,
        sig: path.sig,
        uid: path.uid,
        rid: path.room_id,
        exp: path.exp,
        range: range_header(&headers).map_err(crate::http::error::map_api_error)?,
        head: method == Method::HEAD,
    };
    let state_for_stream = state.clone();
    stream_http_response::<BilibiliDashResourceResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::bilibili::get_bilibili_dash_resource(
                    bilibili_deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/{roomId}/bilibili/{version}/subtitles/{modeName}/{subtitleIndex}",
        tag = "Bilibili Playback Provider",
        params(("roomId" = String, Path), ("version" = String, Path), ("modeName" = String, Path), ("subtitleIndex" = u32, Path), ("sig" = String, Query), ("uid" = String, Query), ("exp" = i64, Query)),
        responses((status = 200, description = "Bilibili subtitle"), (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema))
    )
)]
pub async fn get_bilibili_subtitle(
    Path(path): Path<BilibiliSubtitlePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = GetBilibiliSubtitleRequest {
        version: path.version,
        mode_name: path.mode_name,
        subtitle_index: path.subtitle_index,
        sig,
        uid,
        rid,
        exp,
    };
    let state_for_stream = state.clone();
    stream_http_response::<BilibiliSubtitleResponse, _>(
        state,
        request_meta,
        Method::GET,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::bilibili::get_bilibili_subtitle(
                    bilibili_deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/{roomId}/bilibili/{version}/danmaku-files/{danmakuIndex}",
        tag = "Bilibili Playback Provider",
        params(("roomId" = String, Path), ("version" = String, Path), ("danmakuIndex" = u32, Path), ("sig" = String, Query), ("uid" = String, Query), ("exp" = i64, Query)),
        responses((status = 200, description = "Bilibili danmaku file"), (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema))
    )
)]
pub async fn get_bilibili_danmaku_file(
    Path(path): Path<BilibiliDanmakuFilePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = GetBilibiliDanmakuFileRequest {
        version: path.version,
        danmaku_index: path.danmaku_index,
        sig,
        uid,
        rid,
        exp,
    };
    let state_for_stream = state.clone();
    stream_http_response::<BilibiliDanmakuFileResponse, _>(
        state,
        request_meta,
        Method::GET,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::bilibili::get_bilibili_danmaku_file(
                    bilibili_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        },
    )
    .await
}

pub async fn watch_bilibili_live_danmaku(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomMediaTargetPathRequest>,
) -> AppResult<Response> {
    let synctv_proto::client::RoomMediaTargetPathRequest { room_id, media_id } = path;
    let req = WatchBilibiliLiveDanmakuRequest {
        target: Some(
            synctv_proto::playback_provider::bilibili::watch_bilibili_live_danmaku_request::Target::MediaId(media_id),
        ),
    };
    watch_bilibili_live_danmaku_stream(state, request_meta, room_id, req).await
}

pub async fn watch_bilibili_dynamic_live_danmaku(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPlaylistTargetPathRequest>,
    Query(query): Query<BilibiliDynamicLiveDanmakuQuery>,
) -> AppResult<Response> {
    let synctv_proto::client::RoomPlaylistTargetPathRequest {
        room_id,
        playlist_id,
    } = path;
    let req = WatchBilibiliLiveDanmakuRequest {
        target: Some(
            synctv_proto::playback_provider::bilibili::watch_bilibili_live_danmaku_request::Target::Dynamic(
                BilibiliDynamicLiveDanmakuTarget {
                    playlist_id,
                    live_room_id: query.live_room_id,
                },
            ),
        ),
    };
    watch_bilibili_live_danmaku_stream(state, request_meta, room_id, req).await
}

async fn watch_bilibili_live_danmaku_stream(
    state: AppState,
    request_meta: RequestMetadata,
    room_id: String,
    req: WatchBilibiliLiveDanmakuRequest,
) -> AppResult<Response> {
    let stream = execute_room_actor_endpoint_with_control(
        &state,
        request_meta,
        room_id,
        EndpointRateLimitCategory::Streaming,
        EndpointRateLimitScope::RoomPlayback,
        move |client_api, request_control, actor| async move {
            client_api
                .watch_bilibili_live_danmaku_for_actor(&actor, req, Some(&request_control))
                .await
        },
    )
    .await?;
    let stream = stream.map(bilibili_danmaku_sse_event).boxed();
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response())
}

fn dash_manifest_mode(
    path_mode: Option<&str>,
    query: &str,
) -> Result<i32, synctv_api_common::impls::ApiError> {
    let query_mode = unsigned_query_field(query, "mode")?;
    path_mode.map(str::to_string).or(query_mode).map_or(
        Ok(BilibiliDashManifestMode::Direct as i32),
        |value| match value.as_str() {
            "" | "direct" => Ok(BilibiliDashManifestMode::Direct as i32),
            "proxy" => Ok(BilibiliDashManifestMode::Proxy as i32),
            _ => Err(synctv_api_common::impls::ApiError::InvalidInput(
                "mode must be direct or proxy".to_string(),
            )),
        },
    )
}

fn bilibili_hls_resource_kind(value: &str) -> AppResult<i32> {
    match value {
        "media" => Ok(BilibiliHlsResourceKind::Media as i32),
        "manifest" => Ok(BilibiliHlsResourceKind::Manifest as i32),
        _ => Err(crate::http::error::map_api_error(
            synctv_api_common::impls::ApiError::InvalidInput(
                "Invalid Bilibili HLS resource kind".to_string(),
            ),
        )),
    }
}

fn bilibili_dash_resource_kind(value: &str) -> AppResult<i32> {
    match value {
        "media" => Ok(BilibiliDashResourceKind::Media as i32),
        "manifest" => Ok(BilibiliDashResourceKind::Manifest as i32),
        _ => Err(crate::http::error::map_api_error(
            synctv_api_common::impls::ApiError::InvalidInput(
                "Invalid Bilibili DASH resource kind".to_string(),
            ),
        )),
    }
}

fn bilibili_deps<'a>(
    state: &'a AppState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> synctv_api_common::playback_provider::bilibili::BilibiliPlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::bilibili::BilibiliPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.bilibili_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
