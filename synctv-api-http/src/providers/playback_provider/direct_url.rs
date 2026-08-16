use axum::{
    extract::{Path, RawQuery, State},
    http::{HeaderMap, Method},
};
use base64::Engine as _;
use futures::FutureExt;
use synctv_proto::playback_provider::direct_url::{
    DirectUrlDashManifestResponse, DirectUrlDashResourceResponse, DirectUrlHlsManifestResponse,
    DirectUrlHlsResourceResponse, DirectUrlManifestResourceKind, DirectUrlStreamResponse,
    DirectUrlSubtitleResponse, GetDirectUrlDashManifestRequest, GetDirectUrlDashResourceRequest,
    GetDirectUrlHlsManifestRequest, GetDirectUrlHlsResourceRequest, GetDirectUrlStreamRequest,
    GetDirectUrlSubtitleRequest,
};

use crate::http::{middleware::RequestMetadata, AppResult, AppState};
use crate::providers::playback_provider::transport::{
    query, range_header, signed_query_fields, stream_http_response, target_url,
    PlaybackProviderHttpResponse,
};

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectUrlIndexedPath {
    pub room_id: String,
    pub version: String,
    pub mode_name: String,
    pub url_index: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectUrlSubtitlePath {
    pub room_id: String,
    pub version: String,
    pub mode_name: String,
    pub subtitle_index: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectUrlManifestResourcePath {
    pub room_id: String,
    pub version: String,
    pub mode_name: String,
    pub url_index: u32,
    pub resource_kind: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectUrlDashResourcePath {
    pub room_id: String,
    pub version: String,
    pub mode_name: String,
    pub url_index: u32,
    pub resource_kind: String,
    pub scope: String,
    pub uid: String,
    pub exp: i64,
    pub sig: String,
    #[serde(default)]
    pub resource_path: String,
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

impl PlaybackProviderHttpResponse for DirectUrlHlsResourceResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for DirectUrlDashManifestResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for DirectUrlDashResourceResponse {
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
        path = "/api/playback-providers/{roomId}/direct-url/{version}/streams/{modeName}/{urlIndex}",
        tag = "DirectUrl Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path),
            ("modeName" = String, Path),
            ("urlIndex" = u32, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "DirectUrl media stream"),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Playback provider resource not found", body = crate::openapi::GoogleRpcStatusSchema)
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
        path = "/api/playback-providers/{roomId}/direct-url/{version}/streams/{modeName}/{urlIndex}",
        tag = "DirectUrl Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path),
            ("modeName" = String, Path),
            ("urlIndex" = u32, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "DirectUrl media stream metadata"),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Playback provider resource not found", body = crate::openapi::GoogleRpcStatusSchema)
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
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
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
                synctv_api_common::playback_provider::direct_url::get_direct_url_stream(
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
        path = "/api/playback-providers/{roomId}/direct-url/{version}/hls-manifests/{modeName}/{urlIndex}",
        tag = "DirectUrl Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path),
            ("modeName" = String, Path),
            ("urlIndex" = u32, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "DirectUrl HLS manifest"),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Playback provider resource not found", body = crate::openapi::GoogleRpcStatusSchema)
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
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
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
                synctv_api_common::playback_provider::direct_url::get_direct_url_hls_manifest(
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
        path = "/api/playback-providers/{roomId}/direct-url/{version}/hls-resources/{modeName}/{urlIndex}/{resourceKind}",
        tag = "DirectUrl Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path),
            ("modeName" = String, Path),
            ("urlIndex" = u32, Path),
            ("resourceKind" = String, Path),
            ("targetUrl" = String, Query),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "DirectUrl HLS resource"),
            (status = 400, description = "Invalid targetUrl", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        )
    )
)]
pub fn get_direct_url_hls_resource(
    Path(path): Path<DirectUrlManifestResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    direct_url_hls_resource(
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
        path = "/api/playback-providers/{roomId}/direct-url/{version}/hls-resources/{modeName}/{urlIndex}/{resourceKind}",
        tag = "DirectUrl Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path),
            ("modeName" = String, Path),
            ("urlIndex" = u32, Path),
            ("resourceKind" = String, Path),
            ("targetUrl" = String, Query),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "DirectUrl HLS resource metadata"),
            (status = 400, description = "Invalid targetUrl", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        )
    )
)]
pub fn head_direct_url_hls_resource(
    Path(path): Path<DirectUrlManifestResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    direct_url_hls_resource(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn direct_url_hls_resource(
    path: DirectUrlManifestResourcePath,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = GetDirectUrlHlsResourceRequest {
        version: path.version,
        target_url: target_url(&query_string).map_err(crate::http::error::map_api_error)?,
        sig,
        uid,
        rid,
        exp,
        range: range_header(&headers).map_err(crate::http::error::map_api_error)?,
        head: method == Method::HEAD,
        mode_name: path.mode_name,
        url_index: path.url_index,
        resource_kind: manifest_resource_kind(&path.resource_kind)?,
    };
    let state_for_stream = state.clone();
    stream_http_response::<DirectUrlHlsResourceResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::direct_url::get_direct_url_hls_resource(
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
        path = "/api/playback-providers/{roomId}/direct-url/{version}/dash-manifests/{modeName}/{urlIndex}",
        tag = "DirectUrl Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path),
            ("modeName" = String, Path),
            ("urlIndex" = u32, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses((status = 200, description = "DirectUrl DASH manifest"))
    )
)]
pub async fn get_direct_url_dash_manifest(
    Path(path): Path<DirectUrlIndexedPath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = GetDirectUrlDashManifestRequest {
        version: path.version,
        mode_name: path.mode_name,
        url_index: path.url_index,
        sig,
        uid,
        rid,
        exp,
    };
    let state_for_stream = state.clone();
    stream_http_response::<DirectUrlDashManifestResponse, _>(
        state,
        request_meta,
        Method::GET,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::direct_url::get_direct_url_dash_manifest(
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
        path = "/api/playback-providers/{roomId}/direct-url/{version}/dash-resources/{modeName}/{urlIndex}/{resourceKind}/{scope}/{uid}/{exp}/{sig}/{resourcePath}",
        tag = "DirectUrl Playback Provider",
        params(
           ("roomId" = String, Path),
            ("roomId" = String, Path),
            ("version" = String, Path),
            ("modeName" = String, Path),
            ("urlIndex" = u32, Path),
            ("resourceKind" = String, Path),
            ("scope" = String, Path),
            ("uid" = String, Path),
            ("exp" = i64, Path),
            ("sig" = String, Path),
            ("resourcePath" = String, Path),
            ("Range" = Option<String>, Header)
        ),
        responses(
            (status = 200, description = "DirectUrl DASH resource"),
            (status = 206, description = "DirectUrl DASH partial resource"),
            (status = 400, description = "Invalid DASH resource scope", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Playback provider resource not found", body = crate::openapi::GoogleRpcStatusSchema)
        )
    )
)]
pub fn get_direct_url_dash_resource(
    Path(path): Path<DirectUrlDashResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    direct_url_dash_resource(
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
        path = "/api/playback-providers/{roomId}/direct-url/{version}/dash-resources/{modeName}/{urlIndex}/{resourceKind}/{scope}/{uid}/{exp}/{sig}/{resourcePath}",
        tag = "DirectUrl Playback Provider",
        params(
           ("roomId" = String, Path),
            ("roomId" = String, Path),
            ("version" = String, Path),
            ("modeName" = String, Path),
            ("urlIndex" = u32, Path),
            ("resourceKind" = String, Path),
            ("scope" = String, Path),
            ("uid" = String, Path),
            ("exp" = i64, Path),
            ("sig" = String, Path),
            ("resourcePath" = String, Path),
            ("Range" = Option<String>, Header)
        ),
        responses(
            (status = 200, description = "DirectUrl DASH resource metadata"),
            (status = 206, description = "DirectUrl DASH partial resource metadata"),
            (status = 400, description = "Invalid DASH resource scope", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Playback provider resource not found", body = crate::openapi::GoogleRpcStatusSchema)
        )
    )
)]
pub fn head_direct_url_dash_resource(
    Path(path): Path<DirectUrlDashResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    direct_url_dash_resource(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn direct_url_dash_resource(
    path: DirectUrlDashResourcePath,
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
                "Invalid DASH resource scope".to_string(),
            ))
        })?;
    let scope_url = String::from_utf8(scope_url).map_err(|_| {
        crate::http::error::map_api_error(synctv_api_common::impls::ApiError::InvalidInput(
            "Invalid DASH resource scope encoding".to_string(),
        ))
    })?;
    let req = GetDirectUrlDashResourceRequest {
        version: path.version,
        mode_name: path.mode_name,
        url_index: path.url_index,
        scope_url,
        resource_path: path.resource_path,
        resource_query: (!resource_query.is_empty()).then_some(resource_query),
        resource_kind: manifest_resource_kind(&path.resource_kind)?,
        sig: path.sig,
        uid: path.uid,
        rid: path.room_id,
        exp: path.exp,
        range: range_header(&headers).map_err(crate::http::error::map_api_error)?,
        head: method == Method::HEAD,
    };
    let state_for_stream = state.clone();
    stream_http_response::<DirectUrlDashResourceResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::direct_url::get_direct_url_dash_resource(
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

fn manifest_resource_kind(value: &str) -> AppResult<i32> {
    match value {
        "media" => Ok(DirectUrlManifestResourceKind::Media as i32),
        "manifest" => Ok(DirectUrlManifestResourceKind::Manifest as i32),
        _ => Err(crate::http::error::map_api_error(
            synctv_api_common::impls::ApiError::InvalidInput(
                "Invalid manifest resource kind".to_string(),
            ),
        )),
    }
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/playback-providers/{roomId}/direct-url/{version}/subtitles/{modeName}/{subtitleIndex}",
        tag = "DirectUrl Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path),
            ("modeName" = String, Path),
            ("subtitleIndex" = u32, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "DirectUrl subtitle"),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Playback provider resource not found", body = crate::openapi::GoogleRpcStatusSchema)
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
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
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
                synctv_api_common::playback_provider::direct_url::get_direct_url_subtitle(
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
) -> synctv_api_common::playback_provider::direct_url::DirectUrlPlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::direct_url::DirectUrlPlaybackProviderDeps {
        playback_provider_service: &state
            .shared_api_runtime
            .direct_url_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
