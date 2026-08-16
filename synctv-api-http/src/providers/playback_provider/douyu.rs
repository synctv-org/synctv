use axum::{
    extract::{Path, RawQuery, State},
    http::{HeaderMap, Method},
    response::{sse::KeepAlive, IntoResponse, Sse},
};
use futures::{FutureExt, StreamExt};
#[cfg(feature = "openapi")]
use synctv_proto::playback_provider::douyu::DouyuDanmakuEvent;
use synctv_proto::playback_provider::douyu::{
    DouyuResourceResponse, DouyuSegmentResponse, GetDouyuResourceRequest, GetDouyuSegmentRequest,
    WatchDouyuDanmakuRequest,
};

use crate::http::{middleware::RequestMetadata, AppResult, AppState};
use crate::providers::playback_provider::transport::{
    query, range_header, signed_query_fields, stream_http_response, target_url,
    PlaybackProviderHttpResponse,
};
use synctv_api_common::impls::EndpointRateLimitCategory;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DouyuResourcePath {
    pub room_id: String,
    pub version: String,
    pub mode_name: String,
    pub media_index: u32,
}

impl PlaybackProviderHttpResponse for DouyuResourceResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for DouyuSegmentResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/playback-providers/{roomId}/douyu/{version}/resources/{modeName}/{mediaIndex}",
        tag = "Douyu Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path), ("modeName" = String, Path),
            ("mediaIndex" = u32, Path), ("sig" = String, Query),
            ("uid" = String, Query), ("exp" = i64, Query)
        ),
        responses((status = 200, description = "Refreshed Douyu media resource"))
    )
)]
pub fn get_douyu_resource(
    Path(path): Path<DouyuResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    douyu_resource(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::GET,
    )
}

pub fn head_douyu_resource(
    Path(path): Path<DouyuResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    douyu_resource(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn douyu_resource(
    path: DouyuResourcePath,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = GetDouyuResourceRequest {
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
    stream_http_response::<DouyuResourceResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::douyu::get_douyu_resource(
                    douyu_deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/{roomId}/douyu/{version}/segments",
        tag = "Douyu Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path), ("targetUrl" = String, Query),
            ("sig" = String, Query), ("uid" = String, Query), ("exp" = i64, Query)
        ),
        responses((status = 200, description = "Douyu HLS segment"))
    )
)]
pub fn get_douyu_segment(
    Path((room_id, version)): Path<(String, String)>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    douyu_segment(
        room_id,
        version,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::GET,
    )
}

pub fn head_douyu_segment(
    Path((room_id, version)): Path<(String, String)>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    douyu_segment(
        room_id,
        version,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn douyu_segment(
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
    let req = GetDouyuSegmentRequest {
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
    stream_http_response::<DouyuSegmentResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::douyu::get_douyu_segment(
                    douyu_deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/{roomId}/douyu/{version}/danmakus/{modeName}/{mediaIndex}",
        tag = "Douyu Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path), ("modeName" = String, Path),
            ("mediaIndex" = u32, Path), ("sig" = String, Query),
            ("uid" = String, Query), ("exp" = i64, Query)
        ),
        responses((
            status = 200,
            description = "Douyu live danmaku SSE",
            body = DouyuDanmakuEvent,
            content_type = "text/event-stream"
        ))
    )
)]
pub async fn watch_douyu_danmaku(
    Path(path): Path<DouyuResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = WatchDouyuDanmakuRequest {
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
                    synctv_api_common::playback_provider::douyu::watch_douyu_danmaku(
                        douyu_deps(&state, Some(&request_control)),
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
            .map(super::transport::douyu_danmaku_sse_event)
            .boxed(),
    )
    .keep_alive(KeepAlive::default())
    .into_response())
}

fn douyu_deps<'a>(
    state: &'a AppState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> synctv_api_common::playback_provider::douyu::DouyuPlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::douyu::DouyuPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.douyu_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
