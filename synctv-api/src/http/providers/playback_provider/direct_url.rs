use axum::{
    extract::{Path, RawQuery, State},
    http::{HeaderMap, Method},
};
use futures::FutureExt;
use synctv_proto::playback_provider::direct_url::{
    DirectUrlHlsManifestResponse, DirectUrlHlsSegmentResponse, DirectUrlStreamResponse,
    DirectUrlSubtitleResponse, GetDirectUrlHlsManifestRequest, GetDirectUrlHlsSegmentRequest,
    GetDirectUrlStreamRequest, GetDirectUrlSubtitleRequest,
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
pub struct DirectUrlIndexedPath {
    pub version: String,
    pub mode_name: String,
    pub url_index: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectUrlSubtitlePath {
    pub version: String,
    pub mode_name: String,
    pub subtitle_index: u32,
}

impl PlaybackProviderHttpResponse for DirectUrlStreamResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for DirectUrlHlsManifestResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for DirectUrlHlsSegmentResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for DirectUrlSubtitleResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/playback-providers/direct-url/{version}/streams/{modeName}/{urlIndex}",
        tag = "DirectUrl Playback Provider",
        params(
            ("version" = String, Path),
            ("modeName" = String, Path),
            ("urlIndex" = u32, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("rid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "DirectUrl media stream"),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Playback provider resource not found", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub fn get_direct_url_stream(
    Path(path): Path<DirectUrlIndexedPath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    direct_url_stream(
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
        path = "/api/playback-providers/direct-url/{version}/streams/{modeName}/{urlIndex}",
        tag = "DirectUrl Playback Provider",
        params(
            ("version" = String, Path),
            ("modeName" = String, Path),
            ("urlIndex" = u32, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("rid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "DirectUrl media stream metadata"),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Playback provider resource not found", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub fn head_direct_url_stream(
    Path(path): Path<DirectUrlIndexedPath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    direct_url_stream(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn direct_url_stream(
    path: DirectUrlIndexedPath,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = GetDirectUrlStreamRequest {
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
    stream_http_response::<DirectUrlStreamResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                crate::impls::playback_provider::direct_url::get_direct_url_stream(
                    direct_url_deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/direct-url/{version}/hls-manifests/{modeName}/{urlIndex}",
        tag = "DirectUrl Playback Provider",
        params(
            ("version" = String, Path),
            ("modeName" = String, Path),
            ("urlIndex" = u32, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("rid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "DirectUrl HLS manifest"),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Playback provider resource not found", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub async fn get_direct_url_hls_manifest(
    Path(path): Path<DirectUrlIndexedPath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = GetDirectUrlHlsManifestRequest {
        version: path.version,
        mode_name: path.mode_name,
        url_index: path.url_index,
        sig,
        uid,
        rid,
        exp,
    };
    let state_for_stream = state.clone();
    stream_http_response::<DirectUrlHlsManifestResponse, _>(
        state,
        request_meta,
        Method::GET,
        move |request_control| {
            let state = state_for_stream;
            async move {
                crate::impls::playback_provider::direct_url::get_direct_url_hls_manifest(
                    direct_url_deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/direct-url/{version}/hls-segments",
        tag = "DirectUrl Playback Provider",
        params(
            ("version" = String, Path),
            ("targetUrl" = String, Query),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("rid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "DirectUrl HLS segment"),
            (status = 400, description = "Invalid targetUrl", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub fn get_direct_url_hls_segment(
    Path(version): Path<String>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    direct_url_hls_segment(
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
        path = "/api/playback-providers/direct-url/{version}/hls-segments",
        tag = "DirectUrl Playback Provider",
        params(
            ("version" = String, Path),
            ("targetUrl" = String, Query),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("rid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "DirectUrl HLS segment metadata"),
            (status = 400, description = "Invalid targetUrl", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub fn head_direct_url_hls_segment(
    Path(version): Path<String>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    direct_url_hls_segment(
        version,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn direct_url_hls_segment(
    version: String,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = GetDirectUrlHlsSegmentRequest {
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
    stream_http_response::<DirectUrlHlsSegmentResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                crate::impls::playback_provider::direct_url::get_direct_url_hls_segment(
                    direct_url_deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/direct-url/{version}/subtitles/{modeName}/{subtitleIndex}",
        tag = "DirectUrl Playback Provider",
        params(
            ("version" = String, Path),
            ("modeName" = String, Path),
            ("subtitleIndex" = u32, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("rid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "DirectUrl subtitle"),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Playback provider resource not found", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub async fn get_direct_url_subtitle(
    Path(path): Path<DirectUrlSubtitlePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = GetDirectUrlSubtitleRequest {
        version: path.version,
        mode_name: path.mode_name,
        subtitle_index: path.subtitle_index,
        sig,
        uid,
        rid,
        exp,
    };
    let state_for_stream = state.clone();
    stream_http_response::<DirectUrlSubtitleResponse, _>(
        state,
        request_meta,
        Method::GET,
        move |request_control| {
            let state = state_for_stream;
            async move {
                crate::impls::playback_provider::direct_url::get_direct_url_subtitle(
                    direct_url_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        },
    )
    .await
}

fn direct_url_deps<'a>(
    state: &'a AppState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> crate::impls::playback_provider::direct_url::DirectUrlPlaybackProviderDeps<'a> {
    crate::impls::playback_provider::direct_url::DirectUrlPlaybackProviderDeps {
        playback_provider_service: &state
            .shared_api_runtime
            .direct_url_playback_provider_service,
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
