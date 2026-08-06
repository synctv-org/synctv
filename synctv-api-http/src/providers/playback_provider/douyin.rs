use axum::{
    extract::{Path, RawQuery, State},
    http::{HeaderMap, Method},
    response::{sse::KeepAlive, IntoResponse, Sse},
};
use futures::{FutureExt, StreamExt};
#[cfg(feature = "openapi")]
use synctv_proto::playback_provider::douyin::DouyinDanmakuEvent;
use synctv_proto::playback_provider::douyin::{
    DouyinHlsResourceKind, DouyinHlsResourceResponse, DouyinResourceResponse,
    GetDouyinHlsResourceRequest, GetDouyinResourceRequest, WatchDouyinDanmakuRequest,
};

use crate::http::{middleware::RequestMetadata, AppResult, AppState};
use crate::providers::playback_provider::transport::{
    query, range_header, signed_query_fields, stream_http_response, target_url,
    PlaybackProviderHttpResponse,
};
use synctv_api_common::impls::EndpointRateLimitCategory;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DouyinResourcePath {
    pub version: String,
    pub mode_name: String,
    pub media_index: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DouyinHlsResourcePath {
    pub version: String,
    pub mode_name: String,
    pub media_index: u32,
    pub resource_kind: String,
}

impl PlaybackProviderHttpResponse for DouyinResourceResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for DouyinHlsResourceResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/playback-providers/douyin/{version}/resources/{modeName}/{mediaIndex}",
        tag = "Douyin Playback Provider",
        params(
            ("version" = String, Path), ("modeName" = String, Path),
            ("mediaIndex" = u32, Path), ("sig" = String, Query),
            ("uid" = String, Query), ("rid" = String, Query), ("exp" = i64, Query)
        ),
        responses((status = 200, description = "Refreshed Douyin media resource"))
    )
)]
pub fn get_resource(
    Path(path): Path<DouyinResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    resource(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::GET,
    )
}

pub fn head_resource(
    Path(path): Path<DouyinResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    resource(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn resource(
    path: DouyinResourcePath,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = GetDouyinResourceRequest {
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
    stream_http_response::<DouyinResourceResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::douyin::get_douyin_resource(
                    deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/douyin/{version}/hls-resources/{modeName}/{mediaIndex}/{resourceKind}",
        tag = "Douyin Playback Provider",
        params(
            ("version" = String, Path), ("modeName" = String, Path),
            ("mediaIndex" = u32, Path), ("resourceKind" = String, Path),
            ("targetUrl" = String, Query),
            ("sig" = String, Query), ("uid" = String, Query),
            ("rid" = String, Query), ("exp" = i64, Query)
        ),
        responses((status = 200, description = "Douyin HLS resource"))
    )
)]
pub fn get_hls_resource(
    Path(path): Path<DouyinHlsResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    hls_resource(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::GET,
    )
}

pub fn head_hls_resource(
    Path(path): Path<DouyinHlsResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    hls_resource(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn hls_resource(
    path: DouyinHlsResourcePath,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = GetDouyinHlsResourceRequest {
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
        resource_kind: douyin_hls_resource_kind(&path.resource_kind)?,
    };
    let state_for_stream = state.clone();
    stream_http_response::<DouyinHlsResourceResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::douyin::get_douyin_hls_resource(
                    deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        },
    )
    .await
}

fn douyin_hls_resource_kind(value: &str) -> AppResult<i32> {
    match value {
        "media" => Ok(DouyinHlsResourceKind::Media as i32),
        "manifest" => Ok(DouyinHlsResourceKind::Manifest as i32),
        _ => Err(crate::http::error::map_api_error(
            synctv_api_common::impls::ApiError::InvalidInput(
                "Invalid Douyin HLS resource kind".to_string(),
            ),
        )),
    }
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/playback-providers/douyin/{version}/danmakus/{modeName}/{mediaIndex}",
        tag = "Douyin Playback Provider",
        params(
            ("version" = String, Path), ("modeName" = String, Path),
            ("mediaIndex" = u32, Path), ("sig" = String, Query),
            ("uid" = String, Query), ("rid" = String, Query), ("exp" = i64, Query)
        ),
        responses((
            status = 200,
            description = "Douyin live danmaku SSE",
            body = DouyinDanmakuEvent,
            content_type = "text/event-stream"
        ))
    )
)]
pub async fn watch_danmaku(
    Path(path): Path<DouyinResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = WatchDouyinDanmakuRequest {
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
                    synctv_api_common::playback_provider::douyin::watch_douyin_danmaku(
                        deps(&state, Some(&request_control)),
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
            .map(super::transport::douyin_danmaku_sse_event)
            .boxed(),
    )
    .keep_alive(KeepAlive::default())
    .into_response())
}

fn deps<'a>(
    state: &'a AppState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> synctv_api_common::playback_provider::douyin::DouyinPlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::douyin::DouyinPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.douyin_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
