use axum::{
    extract::{Path, RawQuery, State},
    http::{HeaderMap, Method},
};
use futures::FutureExt;
use synctv_proto::playback_provider::rtmp::{
    GetRtmpFlvStreamRequest, GetRtmpHlsPlaylistRequest, GetRtmpHlsSegmentRequest,
    RtmpFlvStreamResponse, RtmpHlsPlaylistResponse, RtmpHlsSegmentResponse,
};

use crate::http::{
    middleware::RequestMetadata,
    providers::playback_provider::transport::{
        query, range_header, signed_query_fields, stream_http_response,
        PlaybackProviderHttpResponse,
    },
    AppResult, AppState,
};

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RtmpVersionPath {
    pub version: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RtmpSegmentPath {
    pub version: String,
    pub segment_name: String,
}

impl PlaybackProviderHttpResponse for RtmpFlvStreamResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for RtmpHlsPlaylistResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for RtmpHlsSegmentResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/playback-providers/rtmp/{version}/flv-stream",
        tag = "RTMP Playback Provider",
        params(
            ("version" = String, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("rid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "RTMP FLV stream"),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Playback provider resource not found", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub fn get_rtmp_flv_stream(
    Path(path): Path<RtmpVersionPath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    rtmp_flv_stream(path, state, request_meta, headers, raw_query, Method::GET)
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        head,
        path = "/api/playback-providers/rtmp/{version}/flv-stream",
        tag = "RTMP Playback Provider",
        params(
            ("version" = String, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("rid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "RTMP FLV stream metadata"),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Playback provider resource not found", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub fn head_rtmp_flv_stream(
    Path(path): Path<RtmpVersionPath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    rtmp_flv_stream(path, state, request_meta, headers, raw_query, Method::HEAD)
}

async fn rtmp_flv_stream(
    path: RtmpVersionPath,
    state: AppState,
    request_meta: RequestMetadata,
    _headers: HeaderMap,
    raw_query: RawQuery,
    method: Method,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = GetRtmpFlvStreamRequest {
        version: path.version,
        sig,
        uid,
        rid,
        exp,
        head: method == Method::HEAD,
    };
    let state_for_stream = state.clone();
    stream_http_response::<RtmpFlvStreamResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                crate::impls::playback_provider::rtmp::get_rtmp_flv_stream(
                    rtmp_deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/rtmp/{version}/hls-playlist",
        tag = "RTMP Playback Provider",
        params(
            ("version" = String, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("rid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "RTMP HLS playlist"),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Playback provider resource not found", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub async fn get_rtmp_hls_playlist(
    Path(path): Path<RtmpVersionPath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = GetRtmpHlsPlaylistRequest {
        version: path.version,
        sig,
        uid,
        rid,
        exp,
    };
    let state_for_stream = state.clone();
    stream_http_response::<RtmpHlsPlaylistResponse, _>(
        state,
        request_meta,
        Method::GET,
        move |request_control| {
            let state = state_for_stream;
            async move {
                crate::impls::playback_provider::rtmp::get_rtmp_hls_playlist(
                    rtmp_deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/rtmp/{version}/hls-segments/{segmentName}",
        tag = "RTMP Playback Provider",
        params(
            ("version" = String, Path),
            ("segmentName" = String, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("rid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "RTMP HLS segment"),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Playback provider resource not found", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub fn get_rtmp_hls_segment(
    Path(path): Path<RtmpSegmentPath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    rtmp_hls_segment(
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
        path = "/api/playback-providers/rtmp/{version}/hls-segments/{segmentName}",
        tag = "RTMP Playback Provider",
        params(
            ("version" = String, Path),
            ("segmentName" = String, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("rid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "RTMP HLS segment metadata"),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Playback provider resource not found", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub fn head_rtmp_hls_segment(
    Path(path): Path<RtmpSegmentPath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    rtmp_hls_segment(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn rtmp_hls_segment(
    path: RtmpSegmentPath,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = GetRtmpHlsSegmentRequest {
        version: path.version,
        segment_name: path.segment_name,
        sig,
        uid,
        rid,
        exp,
        range: range_header(&headers).map_err(crate::http::error::map_api_error)?,
        head: method == Method::HEAD,
    };
    let state_for_stream = state.clone();
    stream_http_response::<RtmpHlsSegmentResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                crate::impls::playback_provider::rtmp::get_rtmp_hls_segment(
                    rtmp_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        },
    )
    .await
}

fn rtmp_deps<'a>(
    state: &'a AppState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> crate::impls::playback_provider::rtmp::RtmpPlaybackProviderDeps<'a> {
    crate::impls::playback_provider::rtmp::RtmpPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.rtmp_playback_provider_service,
        proxy_signing_key: &state.shared_api_runtime.proxy_signing_key,
        public_id_codec: &state.shared_api_runtime.public_id_codec,
        provider_stores: state.shared_api_runtime.provider_stores.as_ref(),
        user_service: &state.shared_api_runtime.client_api.user_service,
        playback_transport_services: &state.shared_api_runtime.playback_transport_services,
        request_control,
        proxy_http_client: &state.proxy_http_client,
        ssrf_guard: &state.ssrf_guard,
        proxy_slice_cache: &state.proxy_slice_cache,
        live_streaming_infrastructure: state.shared_api_runtime.client_api.live_infrastructure(),
        connection_runtime: state.connection_manager.as_ref(),
        livestream_config: &state.config.livestream,
        settings_registry: state.settings_registry.as_deref(),
    }
}
