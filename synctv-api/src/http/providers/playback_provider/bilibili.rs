use axum::{
    extract::{Path, RawQuery, State},
    http::{HeaderMap, Method},
    response::sse::{Event, KeepAlive, Sse},
};
use futures::{FutureExt, StreamExt};
use std::convert::Infallible;
use synctv_proto::playback_provider::bilibili::{
    BilibiliDanmakuFileResponse, BilibiliDashManifestMode, BilibiliDashManifestResponse,
    BilibiliDashSegmentResponse, BilibiliHlsManifestResponse, BilibiliHlsSegmentResponse,
    BilibiliMediaStreamResponse, BilibiliSubtitleResponse, GetBilibiliDanmakuFileRequest,
    GetBilibiliDashManifestRequest, GetBilibiliDashSegmentRequest, GetBilibiliHlsManifestRequest,
    GetBilibiliHlsSegmentRequest, GetBilibiliMediaStreamRequest, GetBilibiliSubtitleRequest,
    WatchBilibiliLiveDanmakuRequest,
};

use crate::http::{
    middleware::RequestMetadata,
    providers::playback_provider::transport::{
        query, range_header, signed_query_fields, stream_http_response, target_url,
        unsigned_query_field, PlaybackProviderHttpResponse,
    },
    AppResult, AppState,
};
use crate::impls::EndpointRateLimitCategory;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BilibiliIndexedPath {
    pub version: String,
    pub mode_name: String,
    pub url_index: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BilibiliDashManifestPath {
    pub version: String,
    pub mode_name: String,
    #[serde(default)]
    pub manifest_mode: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BilibiliSubtitlePath {
    pub version: String,
    pub mode_name: String,
    pub subtitle_index: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BilibiliDanmakuFilePath {
    pub version: String,
    pub danmaku_index: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BilibiliLiveDanmakuPath {
    pub media_id: String,
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

impl PlaybackProviderHttpResponse for BilibiliHlsSegmentResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for BilibiliDashManifestResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for BilibiliDashSegmentResponse {
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
        path = "/api/playback-providers/bilibili/{version}/media-streams/{modeName}/{urlIndex}",
        tag = "Bilibili Playback Provider",
        params(("version" = String, Path), ("modeName" = String, Path), ("urlIndex" = u32, Path), ("sig" = String, Query), ("uid" = String, Query), ("rid" = String, Query), ("exp" = i64, Query)),
        responses((status = 200, description = "Bilibili media stream"), (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse))
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
        path = "/api/playback-providers/bilibili/{version}/media-streams/{modeName}/{urlIndex}",
        tag = "Bilibili Playback Provider",
        params(("version" = String, Path), ("modeName" = String, Path), ("urlIndex" = u32, Path), ("sig" = String, Query), ("uid" = String, Query), ("rid" = String, Query), ("exp" = i64, Query)),
        responses((status = 200, description = "Bilibili media stream metadata"), (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse))
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
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
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
                crate::impls::playback_provider::bilibili::get_bilibili_media_stream(
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
        path = "/api/playback-providers/bilibili/{version}/hls-manifests/{modeName}/{urlIndex}",
        tag = "Bilibili Playback Provider",
        params(("version" = String, Path), ("modeName" = String, Path), ("urlIndex" = u32, Path), ("sig" = String, Query), ("uid" = String, Query), ("rid" = String, Query), ("exp" = i64, Query)),
        responses((status = 200, description = "Bilibili HLS manifest"), (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse))
    )
)]
pub async fn get_bilibili_hls_manifest(
    Path(path): Path<BilibiliIndexedPath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
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
                crate::impls::playback_provider::bilibili::get_bilibili_hls_manifest(
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
        path = "/api/playback-providers/bilibili/{version}/hls-segments",
        tag = "Bilibili Playback Provider",
        params(("version" = String, Path), ("targetUrl" = String, Query), ("sig" = String, Query), ("uid" = String, Query), ("rid" = String, Query), ("exp" = i64, Query)),
        responses((status = 200, description = "Bilibili HLS segment"), (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse))
    )
)]
pub fn get_bilibili_hls_segment(
    Path(version): Path<String>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    bilibili_hls_segment(
        version,
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
        path = "/api/playback-providers/bilibili/{version}/hls-segments",
        tag = "Bilibili Playback Provider",
        params(("version" = String, Path), ("targetUrl" = String, Query), ("sig" = String, Query), ("uid" = String, Query), ("rid" = String, Query), ("exp" = i64, Query)),
        responses((status = 200, description = "Bilibili HLS segment metadata"), (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse))
    )
)]
pub fn head_bilibili_hls_segment(
    Path(version): Path<String>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    bilibili_hls_segment(
        version,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn bilibili_hls_segment(
    version: String,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = GetBilibiliHlsSegmentRequest {
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
    stream_http_response::<BilibiliHlsSegmentResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                crate::impls::playback_provider::bilibili::get_bilibili_hls_segment(
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
        path = "/api/playback-providers/bilibili/{version}/dash-manifests/{modeName}/{manifestMode}",
        tag = "Bilibili Playback Provider",
        params(("version" = String, Path), ("modeName" = String, Path), ("manifestMode" = String, Path), ("sig" = String, Query), ("uid" = String, Query), ("rid" = String, Query), ("exp" = i64, Query)),
        responses((status = 200, description = "Bilibili DASH manifest"), (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse))
    )
)]
pub async fn get_bilibili_dash_manifest(
    Path(path): Path<BilibiliDashManifestPath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
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
                crate::impls::playback_provider::bilibili::get_bilibili_dash_manifest(
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
        path = "/api/playback-providers/bilibili/{version}/dash-segments/{modeName}/{urlIndex}",
        tag = "Bilibili Playback Provider",
        params(("version" = String, Path), ("modeName" = String, Path), ("urlIndex" = u32, Path), ("sig" = String, Query), ("uid" = String, Query), ("rid" = String, Query), ("exp" = i64, Query)),
        responses((status = 200, description = "Bilibili DASH segment"), (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse))
    )
)]
pub fn get_bilibili_dash_segment(
    Path(path): Path<BilibiliIndexedPath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    bilibili_dash_segment(
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
        path = "/api/playback-providers/bilibili/{version}/dash-segments/{modeName}/{urlIndex}",
        tag = "Bilibili Playback Provider",
        params(("version" = String, Path), ("modeName" = String, Path), ("urlIndex" = u32, Path), ("sig" = String, Query), ("uid" = String, Query), ("rid" = String, Query), ("exp" = i64, Query)),
        responses((status = 200, description = "Bilibili DASH segment metadata"), (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse))
    )
)]
pub fn head_bilibili_dash_segment(
    Path(path): Path<BilibiliIndexedPath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    bilibili_dash_segment(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn bilibili_dash_segment(
    path: BilibiliIndexedPath,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = GetBilibiliDashSegmentRequest {
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
    stream_http_response::<BilibiliDashSegmentResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                crate::impls::playback_provider::bilibili::get_bilibili_dash_segment(
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
        path = "/api/playback-providers/bilibili/{version}/subtitles/{modeName}/{subtitleIndex}",
        tag = "Bilibili Playback Provider",
        params(("version" = String, Path), ("modeName" = String, Path), ("subtitleIndex" = u32, Path), ("sig" = String, Query), ("uid" = String, Query), ("rid" = String, Query), ("exp" = i64, Query)),
        responses((status = 200, description = "Bilibili subtitle"), (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse))
    )
)]
pub async fn get_bilibili_subtitle(
    Path(path): Path<BilibiliSubtitlePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
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
                crate::impls::playback_provider::bilibili::get_bilibili_subtitle(
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
        path = "/api/playback-providers/bilibili/{version}/danmaku-files/{danmakuIndex}",
        tag = "Bilibili Playback Provider",
        params(("version" = String, Path), ("danmakuIndex" = u32, Path), ("sig" = String, Query), ("uid" = String, Query), ("rid" = String, Query), ("exp" = i64, Query)),
        responses((status = 200, description = "Bilibili danmaku file"), (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse))
    )
)]
pub async fn get_bilibili_danmaku_file(
    Path(path): Path<BilibiliDanmakuFilePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
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
                crate::impls::playback_provider::bilibili::get_bilibili_danmaku_file(
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
        path = "/api/playback-providers/bilibili/live-danmaku/{mediaId}",
        tag = "Bilibili Playback Provider",
        params(("mediaId" = String, Path)),
        responses((status = 200, description = "Bilibili live danmaku SSE"), (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse))
    )
)]
pub async fn watch_bilibili_live_danmaku(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<BilibiliLiveDanmakuPath>,
) -> AppResult<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>> {
    let req = WatchBilibiliLiveDanmakuRequest {
        media_id: path.media_id,
    };
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let state_for_stream = state.clone();
    let stream = state
        .shared_api_runtime
        .request_executor
        .execute_user_with_control(
            &request_meta,
            EndpointRateLimitCategory::Streaming,
            move |request_control, authenticated| {
                let state = state_for_stream;
                async move {
                    crate::impls::playback_provider::bilibili::watch_bilibili_live_danmaku(
                        crate::impls::playback_provider::bilibili::BilibiliLiveDanmakuDeps {
                            playback_provider_service: &state
                                .shared_api_runtime
                                .bilibili_playback_provider_service,
                            actor_user_id: authenticated.user_id,
                            request_control: Some(&request_control),
                        },
                        req,
                    )
                    .await
                }
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;
    let stream = stream.map(super::transport::bilibili_danmaku_sse_event);
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

fn dash_manifest_mode(path_mode: Option<&str>, query: &str) -> Result<i32, crate::impls::ApiError> {
    let query_mode = unsigned_query_field(query, "mode")?;
    path_mode.map(str::to_string).or(query_mode).map_or(
        Ok(BilibiliDashManifestMode::Direct as i32),
        |value| match value.as_str() {
            "" | "direct" => Ok(BilibiliDashManifestMode::Direct as i32),
            "proxy" => Ok(BilibiliDashManifestMode::Proxy as i32),
            _ => Err(crate::impls::ApiError::InvalidInput(
                "mode must be direct or proxy".to_string(),
            )),
        },
    )
}

fn bilibili_deps<'a>(
    state: &'a AppState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> crate::impls::playback_provider::bilibili::BilibiliPlaybackProviderDeps<'a> {
    crate::impls::playback_provider::bilibili::BilibiliPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.bilibili_playback_provider_service,
        proxy_signing_key: &state.shared_api_runtime.proxy_signing_key,
        public_id_codec: &state.shared_api_runtime.public_id_codec,
        provider_stores: state.shared_api_runtime.provider_stores.as_ref(),
        user_service: &state.shared_api_runtime.client_api.user_service,
        playback_transport_services: &state.shared_api_runtime.playback_transport_services,
        request_control,
        proxy_http_client: &state.proxy_http_client,
        ssrf_guard: &state.ssrf_guard,
        proxy_slice_cache: &state.proxy_slice_cache,
    }
}
