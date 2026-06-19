use axum::{
    extract::{Path, RawQuery, State},
    http::{HeaderMap, Method},
};
use futures::FutureExt;
use synctv_proto::playback_provider::alist::{
    AlistFileStreamResponse, AlistSubtitleResponse, AlistThumbnailResponse,
    AlistTranscodedHlsManifestResponse, AlistTranscodedHlsSegmentResponse,
    GetAlistFileStreamRequest, GetAlistSubtitleRequest, GetAlistThumbnailRequest,
    GetAlistTranscodedHlsManifestRequest, GetAlistTranscodedHlsSegmentRequest,
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
pub struct AlistIndexedPath {
    pub version: String,
    pub mode_name: String,
    pub url_index: u32,
}

#[derive(Debug, serde::Deserialize)]
pub struct AlistSubtitlePath {
    pub version: String,
    pub mode_name: String,
    pub subtitle_index: u32,
}

impl PlaybackProviderHttpResponse for AlistFileStreamResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for AlistTranscodedHlsManifestResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for AlistTranscodedHlsSegmentResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for AlistSubtitleResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for AlistThumbnailResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/playback-providers/alist/{version}/files/{mode_name}/{url_index}",
        tag = "Alist Playback Provider",
        params(
            ("version" = String, Path),
            ("mode_name" = String, Path),
            ("url_index" = u32, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("rid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "Alist file stream"),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Playback provider resource not found", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub fn get_alist_file_stream(
    Path(path): Path<AlistIndexedPath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    alist_file_stream(
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
        path = "/api/playback-providers/alist/{version}/files/{mode_name}/{url_index}",
        tag = "Alist Playback Provider",
        params(
            ("version" = String, Path),
            ("mode_name" = String, Path),
            ("url_index" = u32, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("rid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "Alist file stream metadata"),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Playback provider resource not found", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub fn head_alist_file_stream(
    Path(path): Path<AlistIndexedPath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    alist_file_stream(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn alist_file_stream(
    path: AlistIndexedPath,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = GetAlistFileStreamRequest {
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
    stream_http_response::<AlistFileStreamResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                crate::impls::playback_provider::alist::get_alist_file_stream(
                    alist_deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/alist/{version}/transcoded-hls-manifests/{mode_name}/{url_index}",
        tag = "Alist Playback Provider",
        params(
            ("version" = String, Path),
            ("mode_name" = String, Path),
            ("url_index" = u32, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("rid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "Alist transcoded HLS manifest"),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Playback provider resource not found", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub async fn get_alist_transcoded_hls_manifest(
    Path(path): Path<AlistIndexedPath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = GetAlistTranscodedHlsManifestRequest {
        version: path.version,
        mode_name: path.mode_name,
        url_index: path.url_index,
        sig,
        uid,
        rid,
        exp,
    };
    let state_for_stream = state.clone();
    stream_http_response::<AlistTranscodedHlsManifestResponse, _>(
        state,
        request_meta,
        Method::GET,
        move |request_control| {
            let state = state_for_stream;
            async move {
                crate::impls::playback_provider::alist::get_alist_transcoded_hls_manifest(
                    alist_deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/alist/{version}/transcoded-hls-segments",
        tag = "Alist Playback Provider",
        params(
            ("version" = String, Path),
            ("target_url" = String, Query),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("rid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "Alist transcoded HLS segment"),
            (status = 400, description = "Invalid target_url", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub fn get_alist_transcoded_hls_segment(
    Path(version): Path<String>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    alist_transcoded_hls_segment(
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
        path = "/api/playback-providers/alist/{version}/transcoded-hls-segments",
        tag = "Alist Playback Provider",
        params(
            ("version" = String, Path),
            ("target_url" = String, Query),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("rid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "Alist transcoded HLS segment metadata"),
            (status = 400, description = "Invalid target_url", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub fn head_alist_transcoded_hls_segment(
    Path(version): Path<String>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    alist_transcoded_hls_segment(
        version,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn alist_transcoded_hls_segment(
    version: String,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = GetAlistTranscodedHlsSegmentRequest {
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
    stream_http_response::<AlistTranscodedHlsSegmentResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                crate::impls::playback_provider::alist::get_alist_transcoded_hls_segment(
                    alist_deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/alist/{version}/subtitles/{mode_name}/{subtitle_index}",
        tag = "Alist Playback Provider",
        params(
            ("version" = String, Path),
            ("mode_name" = String, Path),
            ("subtitle_index" = u32, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("rid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "Alist subtitle"),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Playback provider resource not found", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub async fn get_alist_subtitle(
    Path(path): Path<AlistSubtitlePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = GetAlistSubtitleRequest {
        version: path.version,
        mode_name: path.mode_name,
        subtitle_index: path.subtitle_index,
        sig,
        uid,
        rid,
        exp,
    };
    let state_for_stream = state.clone();
    stream_http_response::<AlistSubtitleResponse, _>(
        state,
        request_meta,
        Method::GET,
        move |request_control| {
            let state = state_for_stream;
            async move {
                crate::impls::playback_provider::alist::get_alist_subtitle(
                    alist_deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/alist/{version}/thumbnail",
        tag = "Alist Playback Provider",
        params(
            ("version" = String, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("rid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "Alist thumbnail"),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Playback provider resource not found", body = synctv_proto::client::ApiErrorResponse)
        )
    )
)]
pub async fn get_alist_thumbnail(
    Path(version): Path<String>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = GetAlistThumbnailRequest {
        version,
        sig,
        uid,
        rid,
        exp,
    };
    let state_for_stream = state.clone();
    stream_http_response::<AlistThumbnailResponse, _>(
        state,
        request_meta,
        Method::GET,
        move |request_control| {
            let state = state_for_stream;
            async move {
                crate::impls::playback_provider::alist::get_alist_thumbnail(
                    alist_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        },
    )
    .await
}

fn alist_deps<'a>(
    state: &'a AppState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> crate::impls::playback_provider::alist::AlistPlaybackProviderDeps<'a> {
    crate::impls::playback_provider::alist::AlistPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.alist_playback_provider_service,
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
