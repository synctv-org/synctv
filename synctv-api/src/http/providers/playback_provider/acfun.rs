use axum::{
    extract::{Path, RawQuery, State},
    http::{HeaderMap, Method},
    response::{sse::KeepAlive, IntoResponse, Sse},
};
use futures::{FutureExt, StreamExt};
#[cfg(feature = "openapi")]
use synctv_proto::playback_provider::acfun::AcFunDanmakuEvent;
use synctv_proto::playback_provider::acfun::{
    AcFunDanmakuFileResponse, AcFunResourceResponse, AcFunSegmentResponse,
    GetAcFunDanmakuFileRequest, GetAcFunResourceRequest, GetAcFunSegmentRequest,
    WatchAcFunDanmakuRequest,
};

use crate::http::{
    middleware::RequestMetadata,
    providers::playback_provider::transport::{
        query, range_header, signed_query_fields, stream_http_response, target_url,
        PlaybackProviderHttpResponse,
    },
    AppResult, AppState,
};
use crate::impls::EndpointRateLimitCategory;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcFunResourcePath {
    pub version: String,
    pub mode_name: String,
    pub media_index: u32,
}

impl PlaybackProviderHttpResponse for AcFunResourceResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for AcFunSegmentResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for AcFunDanmakuFileResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/playback-providers/acfun/{version}/resources/{modeName}/{mediaIndex}",
        tag = "AcFun Playback Provider",
        params(
            ("version" = String, Path), ("modeName" = String, Path),
            ("mediaIndex" = u32, Path), ("sig" = String, Query),
            ("uid" = String, Query), ("rid" = String, Query), ("exp" = i64, Query)
        ),
        responses((status = 200, description = "Refreshed AcFun media resource"))
    )
)]
pub fn get_acfun_resource(
    Path(path): Path<AcFunResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    acfun_resource(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::GET,
    )
}

pub fn head_acfun_resource(
    Path(path): Path<AcFunResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    acfun_resource(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn acfun_resource(
    path: AcFunResourcePath,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = GetAcFunResourceRequest {
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
    stream_http_response::<AcFunResourceResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                crate::impls::playback_provider::acfun::get_acfun_resource(
                    acfun_deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/acfun/{version}/segments",
        tag = "AcFun Playback Provider",
        params(
            ("version" = String, Path), ("targetUrl" = String, Query),
            ("sig" = String, Query), ("uid" = String, Query),
            ("rid" = String, Query), ("exp" = i64, Query)
        ),
        responses((status = 200, description = "AcFun HLS segment"))
    )
)]
pub fn get_acfun_segment(
    Path(version): Path<String>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    acfun_segment(
        version,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::GET,
    )
}

pub fn head_acfun_segment(
    Path(version): Path<String>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    acfun_segment(
        version,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn acfun_segment(
    version: String,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = GetAcFunSegmentRequest {
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
    stream_http_response::<AcFunSegmentResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                crate::impls::playback_provider::acfun::get_acfun_segment(
                    acfun_deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/acfun/{version}/danmaku-files/{modeName}/{mediaIndex}",
        tag = "AcFun Playback Provider",
        params(
            ("version" = String, Path), ("modeName" = String, Path),
            ("mediaIndex" = u32, Path), ("sig" = String, Query),
            ("uid" = String, Query), ("rid" = String, Query), ("exp" = i64, Query)
        ),
        responses((status = 200, description = "AcFun VOD danmaku JSON track"))
    )
)]
pub fn get_acfun_danmaku_file(
    Path(path): Path<AcFunResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    acfun_danmaku_file(path, state, request_meta, query(raw_query), Method::GET)
}

pub fn head_acfun_danmaku_file(
    Path(path): Path<AcFunResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    acfun_danmaku_file(path, state, request_meta, query(raw_query), Method::HEAD)
}

async fn acfun_danmaku_file(
    path: AcFunResourcePath,
    state: AppState,
    request_meta: RequestMetadata,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = GetAcFunDanmakuFileRequest {
        version: path.version,
        mode_name: path.mode_name,
        media_index: path.media_index,
        sig,
        uid,
        rid,
        exp,
        head: method == Method::HEAD,
    };
    let state_for_stream = state.clone();
    stream_http_response::<AcFunDanmakuFileResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                crate::impls::playback_provider::acfun::get_acfun_danmaku_file(
                    acfun_deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/acfun/{version}/danmakus/{modeName}/{mediaIndex}",
        tag = "AcFun Playback Provider",
        params(
            ("version" = String, Path), ("modeName" = String, Path),
            ("mediaIndex" = u32, Path), ("sig" = String, Query),
            ("uid" = String, Query), ("rid" = String, Query), ("exp" = i64, Query)
        ),
        responses((
            status = 200,
            description = "AcFun live danmaku SSE",
            body = AcFunDanmakuEvent,
            content_type = "text/event-stream"
        ))
    )
)]
pub async fn watch_acfun_danmaku(
    Path(path): Path<AcFunResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = WatchAcFunDanmakuRequest {
        version: path.version,
        mode_name: path.mode_name,
        media_index: path.media_index,
        sig,
        uid,
        rid,
        exp,
    };
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let state_for_stream = state.clone();
    let stream = state
        .shared_api_runtime
        .request_executor
        .execute_public_with_control(
            &request_meta,
            EndpointRateLimitCategory::Streaming,
            move |request_control| {
                let state = state_for_stream;
                async move {
                    crate::impls::playback_provider::acfun::watch_acfun_danmaku(
                        acfun_deps(&state, Some(&request_control)),
                        req,
                    )
                    .await
                }
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)?;
    Ok(Sse::new(
        stream
            .map(super::transport::acfun_danmaku_sse_event)
            .boxed(),
    )
    .keep_alive(KeepAlive::default())
    .into_response())
}

fn acfun_deps<'a>(
    state: &'a AppState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> crate::impls::playback_provider::acfun::AcFunPlaybackProviderDeps<'a> {
    crate::impls::playback_provider::acfun::AcFunPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.acfun_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
