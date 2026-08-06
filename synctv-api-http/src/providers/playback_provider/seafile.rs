use axum::{
    extract::{Path, RawQuery, State},
    http::{HeaderMap, Method},
};
use futures::FutureExt;
use synctv_proto::playback_provider::seafile::{
    GetSeafileHlsManifestRequest, GetSeafileHlsResourceRequest, GetSeafileResourceRequest,
    GetSeafileSubtitleRequest, SeafileHlsManifestResponse, SeafileHlsResourceKind,
    SeafileHlsResourceResponse, SeafileResourceResponse, SeafileSubtitleResponse,
};

use crate::http::{middleware::RequestMetadata, AppResult, AppState};
use crate::providers::playback_provider::transport::{
    query, range_header, signed_query_fields, stream_http_response, target_url,
    PlaybackProviderHttpResponse,
};

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeafileResourcePath {
    pub version: String,
    pub mode_name: String,
    pub media_index: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeafileHlsResourcePath {
    pub version: String,
    pub mode_name: String,
    pub media_index: u32,
    pub resource_kind: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeafileSubtitlePath {
    pub version: String,
    pub mode_name: String,
    pub subtitle_index: u32,
}

impl PlaybackProviderHttpResponse for SeafileResourceResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for SeafileHlsManifestResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for SeafileHlsResourceResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for SeafileSubtitleResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

#[cfg_attr(feature = "openapi", utoipa::path(get, path = "/api/playback-providers/seafile/{version}/resources/{modeName}/{mediaIndex}", tag = "Seafile Playback Provider", params(("version" = String, Path), ("modeName" = String, Path), ("mediaIndex" = u32, Path), ("sig" = String, Query), ("uid" = String, Query), ("rid" = String, Query), ("exp" = i64, Query)), responses((status = 200, description = "Seafile media resource"))))]
pub fn get_seafile_resource(
    Path(path): Path<SeafileResourcePath>,
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

pub fn head_seafile_resource(
    Path(path): Path<SeafileResourcePath>,
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
    path: SeafileResourcePath,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = GetSeafileResourceRequest {
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
    let stream_state = state.clone();
    stream_http_response::<SeafileResourceResponse, _>(
        state,
        request_meta,
        method,
        move |control| {
            let state = stream_state;
            async move {
                synctv_api_common::playback_provider::seafile::get_seafile_resource(
                    deps(&state, Some(&control)),
                    req,
                )
                .await
            }
            .boxed()
        },
    )
    .await
}

#[cfg_attr(feature = "openapi", utoipa::path(get, path = "/api/playback-providers/seafile/{version}/hls-manifests/{modeName}/{mediaIndex}", tag = "Seafile Playback Provider", params(("version" = String, Path), ("modeName" = String, Path), ("mediaIndex" = u32, Path), ("sig" = String, Query), ("uid" = String, Query), ("rid" = String, Query), ("exp" = i64, Query)), responses((status = 200, description = "Seafile HLS manifest"))))]
pub async fn get_seafile_hls_manifest(
    Path(path): Path<SeafileResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    hls_manifest(path, state, request_meta, query(raw_query), Method::GET).await
}

#[cfg_attr(feature = "openapi", utoipa::path(head, path = "/api/playback-providers/seafile/{version}/hls-manifests/{modeName}/{mediaIndex}", tag = "Seafile Playback Provider", params(("version" = String, Path), ("modeName" = String, Path), ("mediaIndex" = u32, Path), ("sig" = String, Query), ("uid" = String, Query), ("rid" = String, Query), ("exp" = i64, Query)), responses((status = 200, description = "Seafile HLS manifest metadata"))))]
pub async fn head_seafile_hls_manifest(
    Path(path): Path<SeafileResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    hls_manifest(path, state, request_meta, query(raw_query), Method::HEAD).await
}

async fn hls_manifest(
    path: SeafileResourcePath,
    state: AppState,
    request_meta: RequestMetadata,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = GetSeafileHlsManifestRequest {
        version: path.version,
        mode_name: path.mode_name,
        media_index: path.media_index,
        sig,
        uid,
        rid,
        exp,
        head: method == Method::HEAD,
    };
    let stream_state = state.clone();
    stream_http_response::<SeafileHlsManifestResponse, _>(
        state,
        request_meta,
        method,
        move |control| {
            let state = stream_state;
            async move {
                synctv_api_common::playback_provider::seafile::get_seafile_hls_manifest(
                    deps(&state, Some(&control)),
                    req,
                )
                .await
            }
            .boxed()
        },
    )
    .await
}

#[cfg_attr(feature = "openapi", utoipa::path(get, path = "/api/playback-providers/seafile/{version}/hls-resources/{modeName}/{mediaIndex}/{resourceKind}", tag = "Seafile Playback Provider", params(("version" = String, Path), ("modeName" = String, Path), ("mediaIndex" = u32, Path), ("resourceKind" = String, Path), ("targetUrl" = String, Query), ("sig" = String, Query), ("uid" = String, Query), ("rid" = String, Query), ("exp" = i64, Query)), responses((status = 200, description = "Seafile HLS resource"))))]
pub fn get_seafile_hls_resource(
    Path(path): Path<SeafileHlsResourcePath>,
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

#[cfg_attr(feature = "openapi", utoipa::path(head, path = "/api/playback-providers/seafile/{version}/hls-resources/{modeName}/{mediaIndex}/{resourceKind}", tag = "Seafile Playback Provider", params(("version" = String, Path), ("modeName" = String, Path), ("mediaIndex" = u32, Path), ("resourceKind" = String, Path), ("targetUrl" = String, Query), ("sig" = String, Query), ("uid" = String, Query), ("rid" = String, Query), ("exp" = i64, Query)), responses((status = 200, description = "Seafile HLS resource metadata"))))]
pub fn head_seafile_hls_resource(
    Path(path): Path<SeafileHlsResourcePath>,
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
    path: SeafileHlsResourcePath,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = GetSeafileHlsResourceRequest {
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
        resource_kind: seafile_hls_resource_kind(&path.resource_kind)?,
    };
    let stream_state = state.clone();
    stream_http_response::<SeafileHlsResourceResponse, _>(
        state,
        request_meta,
        method,
        move |control| {
            let state = stream_state;
            async move {
                synctv_api_common::playback_provider::seafile::get_seafile_hls_resource(
                    deps(&state, Some(&control)),
                    req,
                )
                .await
            }
            .boxed()
        },
    )
    .await
}

fn seafile_hls_resource_kind(value: &str) -> AppResult<i32> {
    match value {
        "media" => Ok(SeafileHlsResourceKind::Media as i32),
        "manifest" => Ok(SeafileHlsResourceKind::Manifest as i32),
        _ => Err(crate::http::error::map_api_error(
            synctv_api_common::impls::ApiError::InvalidInput(
                "Invalid Seafile HLS resource kind".to_string(),
            ),
        )),
    }
}

#[cfg_attr(feature = "openapi", utoipa::path(get, path = "/api/playback-providers/seafile/{version}/subtitles/{modeName}/{subtitleIndex}", tag = "Seafile Playback Provider", params(("version" = String, Path), ("modeName" = String, Path), ("subtitleIndex" = u32, Path), ("sig" = String, Query), ("uid" = String, Query), ("rid" = String, Query), ("exp" = i64, Query)), responses((status = 200, description = "Seafile subtitle"))))]
pub async fn get_seafile_subtitle(
    Path(path): Path<SeafileSubtitlePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
    let req = GetSeafileSubtitleRequest {
        version: path.version,
        mode_name: path.mode_name,
        subtitle_index: path.subtitle_index,
        sig,
        uid,
        rid,
        exp,
    };
    let stream_state = state.clone();
    stream_http_response::<SeafileSubtitleResponse, _>(
        state,
        request_meta,
        Method::GET,
        move |control| {
            let state = stream_state;
            async move {
                synctv_api_common::playback_provider::seafile::get_seafile_subtitle(
                    deps(&state, Some(&control)),
                    req,
                )
                .await
            }
            .boxed()
        },
    )
    .await
}

fn deps<'a>(
    state: &'a AppState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> synctv_api_common::playback_provider::seafile::SeafilePlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::seafile::SeafilePlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.seafile_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hls_resource_path_kind_maps_only_typed_routes() {
        assert_eq!(
            seafile_hls_resource_kind("media").expect("media route should be valid"),
            SeafileHlsResourceKind::Media as i32
        );
        assert_eq!(
            seafile_hls_resource_kind("manifest").expect("manifest route should be valid"),
            SeafileHlsResourceKind::Manifest as i32
        );
        assert!(seafile_hls_resource_kind("segment").is_err());
    }
}
