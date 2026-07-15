use axum::{
    extract::{Path, RawQuery, State},
    http::{HeaderMap, Method},
};
use futures::FutureExt;
use synctv_proto::playback_provider::cctv::{
    CctvResourceResponse, CctvSegmentResponse, GetCctvResourceRequest, GetCctvSegmentRequest,
};

use crate::http::{
    middleware::RequestMetadata,
    providers::playback_provider::transport::{
        query, range_header, signed_query_fields, stream_http_response, target_url,
        PlaybackProviderHttpResponse,
    },
    AppResult, AppState,
};

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CctvResourcePath {
    pub version: String,
    pub mode_name: String,
    pub media_index: u32,
}

impl PlaybackProviderHttpResponse for CctvResourceResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for CctvSegmentResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/playback-providers/cctv/{version}/resources/{modeName}/{mediaIndex}",
        tag = "CCTV Playback Provider",
        params(
            ("version" = String, Path), ("modeName" = String, Path),
            ("mediaIndex" = u32, Path), ("sig" = String, Query),
            ("uid" = String, Query), ("rid" = String, Query), ("exp" = i64, Query)
        ),
        responses((status = 200, description = "Refreshed CCTV media resource"))
    )
)]
pub fn get_cctv_resource(
    Path(path): Path<CctvResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    cctv_resource(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::GET,
    )
}

pub fn head_cctv_resource(
    Path(path): Path<CctvResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    cctv_resource(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn cctv_resource(
    path: CctvResourcePath,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = GetCctvResourceRequest {
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
    stream_http_response::<CctvResourceResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                crate::impls::playback_provider::cctv::get_cctv_resource(
                    cctv_deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/cctv/{version}/segments",
        tag = "CCTV Playback Provider",
        params(
            ("version" = String, Path), ("targetUrl" = String, Query),
            ("sig" = String, Query), ("uid" = String, Query),
            ("rid" = String, Query), ("exp" = i64, Query)
        ),
        responses((status = 200, description = "CCTV HLS segment"))
    )
)]
pub fn get_cctv_segment(
    Path(version): Path<String>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    cctv_segment(
        version,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::GET,
    )
}

pub fn head_cctv_segment(
    Path(version): Path<String>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    cctv_segment(
        version,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn cctv_segment(
    version: String,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = GetCctvSegmentRequest {
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
    stream_http_response::<CctvSegmentResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                crate::impls::playback_provider::cctv::get_cctv_segment(
                    cctv_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        },
    )
    .await
}

fn cctv_deps<'a>(
    state: &'a AppState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> crate::impls::playback_provider::cctv::CctvPlaybackProviderDeps<'a> {
    crate::impls::playback_provider::cctv::CctvPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.cctv_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
