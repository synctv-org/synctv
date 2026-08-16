use axum::{
    extract::{Path, Query, RawQuery, State},
    http::{HeaderMap, Method},
};
use futures::FutureExt;
use synctv_proto::playback_provider::nextcloud::{
    GetNextcloudHlsManifestRequest, GetNextcloudHlsResourceRequest,
    GetNextcloudPreviewResourceRequest, GetNextcloudResourceRequest, GetNextcloudSubtitleRequest,
    NextcloudHlsManifestResponse, NextcloudHlsResourceKind, NextcloudHlsResourceResponse,
    NextcloudPreviewResourceResponse, NextcloudResourceResponse, NextcloudSubtitleResponse,
};

use crate::http::{middleware::RequestMetadata, AppResult, AppState};
use crate::providers::playback_provider::transport::{
    query, range_header, signed_query_fields, stream_http_response, target_url,
    PlaybackProviderHttpResponse,
};

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NextcloudResourcePath {
    pub room_id: String,
    pub version: String,
    pub mode_name: String,
    pub media_index: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NextcloudHlsResourcePath {
    pub room_id: String,
    pub version: String,
    pub mode_name: String,
    pub media_index: u32,
    pub resource_kind: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NextcloudSubtitlePath {
    pub room_id: String,
    pub version: String,
    pub mode_name: String,
    pub subtitle_index: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NextcloudPreviewResourceQuery {
    pub server_id: String,
    pub credential_owner_id: String,
    pub file_id: u64,
    pub width: u32,
    pub height: u32,
    pub crop: bool,
    pub sig: String,
    pub uid: String,
    pub exp: i64,
}

impl PlaybackProviderHttpResponse for NextcloudResourceResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for NextcloudHlsManifestResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for NextcloudHlsResourceResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for NextcloudSubtitleResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for NextcloudPreviewResourceResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

#[cfg_attr(feature = "openapi", utoipa::path(get, path = "/api/playback-providers/{roomId}/nextcloud/{version}/resources/{modeName}/{mediaIndex}", tag = "Nextcloud Playback Provider", params(("roomId" = String, Path), ("version" = String, Path), ("modeName" = String, Path), ("mediaIndex" = u32, Path), ("sig" = String, Query), ("uid" = String, Query), ("exp" = i64, Query)), responses((status = 200, description = "Nextcloud media resource"))))]
pub fn get_nextcloud_resource(
    Path(path): Path<NextcloudResourcePath>,
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

pub fn head_nextcloud_resource(
    Path(path): Path<NextcloudResourcePath>,
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
    path: NextcloudResourcePath,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = GetNextcloudResourceRequest {
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
    stream_http_response::<NextcloudResourceResponse, _>(
        state,
        request_meta,
        method,
        move |control| {
            let state = stream_state;
            async move {
                synctv_api_common::playback_provider::nextcloud::get_nextcloud_resource(
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

#[cfg_attr(feature = "openapi", utoipa::path(get, path = "/api/playback-providers/{roomId}/nextcloud/{version}/hls-manifests/{modeName}/{mediaIndex}", tag = "Nextcloud Playback Provider", params(("roomId" = String, Path), ("version" = String, Path), ("modeName" = String, Path), ("mediaIndex" = u32, Path), ("sig" = String, Query), ("uid" = String, Query), ("exp" = i64, Query)), responses((status = 200, description = "Nextcloud HLS manifest"))))]
pub async fn get_nextcloud_hls_manifest(
    Path(path): Path<NextcloudResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    hls_manifest(path, state, request_meta, query(raw_query), Method::GET).await
}

#[cfg_attr(feature = "openapi", utoipa::path(head, path = "/api/playback-providers/{roomId}/nextcloud/{version}/hls-manifests/{modeName}/{mediaIndex}", tag = "Nextcloud Playback Provider", params(("roomId" = String, Path), ("version" = String, Path), ("modeName" = String, Path), ("mediaIndex" = u32, Path), ("sig" = String, Query), ("uid" = String, Query), ("exp" = i64, Query)), responses((status = 200, description = "Nextcloud HLS manifest metadata"))))]
pub async fn head_nextcloud_hls_manifest(
    Path(path): Path<NextcloudResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    hls_manifest(path, state, request_meta, query(raw_query), Method::HEAD).await
}

async fn hls_manifest(
    path: NextcloudResourcePath,
    state: AppState,
    request_meta: RequestMetadata,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = GetNextcloudHlsManifestRequest {
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
    stream_http_response::<NextcloudHlsManifestResponse, _>(
        state,
        request_meta,
        method,
        move |control| {
            let state = stream_state;
            async move {
                synctv_api_common::playback_provider::nextcloud::get_nextcloud_hls_manifest(
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

#[cfg_attr(feature = "openapi", utoipa::path(get, path = "/api/playback-providers/{roomId}/nextcloud/{version}/hls-resources/{modeName}/{mediaIndex}/{resourceKind}", tag = "Nextcloud Playback Provider", params(("roomId" = String, Path), ("version" = String, Path), ("modeName" = String, Path), ("mediaIndex" = u32, Path), ("resourceKind" = String, Path), ("targetUrl" = String, Query), ("sig" = String, Query), ("uid" = String, Query), ("exp" = i64, Query)), responses((status = 200, description = "Nextcloud HLS resource"))))]
pub fn get_nextcloud_hls_resource(
    Path(path): Path<NextcloudHlsResourcePath>,
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

#[cfg_attr(feature = "openapi", utoipa::path(head, path = "/api/playback-providers/{roomId}/nextcloud/{version}/hls-resources/{modeName}/{mediaIndex}/{resourceKind}", tag = "Nextcloud Playback Provider", params(("roomId" = String, Path), ("version" = String, Path), ("modeName" = String, Path), ("mediaIndex" = u32, Path), ("resourceKind" = String, Path), ("targetUrl" = String, Query), ("sig" = String, Query), ("uid" = String, Query), ("exp" = i64, Query)), responses((status = 200, description = "Nextcloud HLS resource metadata"))))]
pub fn head_nextcloud_hls_resource(
    Path(path): Path<NextcloudHlsResourcePath>,
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
    path: NextcloudHlsResourcePath,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = GetNextcloudHlsResourceRequest {
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
        resource_kind: nextcloud_hls_resource_kind(&path.resource_kind)?,
    };
    let stream_state = state.clone();
    stream_http_response::<NextcloudHlsResourceResponse, _>(
        state,
        request_meta,
        method,
        move |control| {
            let state = stream_state;
            async move {
                synctv_api_common::playback_provider::nextcloud::get_nextcloud_hls_resource(
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

fn nextcloud_hls_resource_kind(value: &str) -> AppResult<i32> {
    match value {
        "media" => Ok(NextcloudHlsResourceKind::Media as i32),
        "manifest" => Ok(NextcloudHlsResourceKind::Manifest as i32),
        _ => Err(crate::http::error::map_api_error(
            synctv_api_common::impls::ApiError::InvalidInput(
                "Invalid Nextcloud HLS resource kind".to_string(),
            ),
        )),
    }
}

#[cfg_attr(feature = "openapi", utoipa::path(get, path = "/api/playback-providers/{roomId}/nextcloud/{version}/subtitles/{modeName}/{subtitleIndex}", tag = "Nextcloud Playback Provider", params(("roomId" = String, Path), ("version" = String, Path), ("modeName" = String, Path), ("subtitleIndex" = u32, Path), ("sig" = String, Query), ("uid" = String, Query), ("exp" = i64, Query)), responses((status = 200, description = "Nextcloud subtitle"))))]
pub async fn get_nextcloud_subtitle(
    Path(path): Path<NextcloudSubtitlePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = GetNextcloudSubtitleRequest {
        version: path.version,
        mode_name: path.mode_name,
        subtitle_index: path.subtitle_index,
        sig,
        uid,
        rid,
        exp,
    };
    let stream_state = state.clone();
    stream_http_response::<NextcloudSubtitleResponse, _>(
        state,
        request_meta,
        Method::GET,
        move |control| {
            let state = stream_state;
            async move {
                synctv_api_common::playback_provider::nextcloud::get_nextcloud_subtitle(
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

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/playback-providers/{roomId}/nextcloud/preview",
        tag = "Nextcloud Playback Provider",
        params(
            ("roomId" = String, Path), ("serverId" = String, Query),
            ("credentialOwnerId" = String, Query), ("fileId" = u64, Query),
            ("width" = u32, Query), ("height" = u32, Query),
            ("crop" = bool, Query), ("sig" = String, Query),
            ("uid" = String, Query), ("exp" = i64, Query)
        ),
        responses((status = 200, description = "Room-scoped Nextcloud preview"))
    )
)]
pub async fn get_nextcloud_preview_resource(
    Path(room_id): Path<String>,
    Query(query): Query<NextcloudPreviewResourceQuery>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
) -> AppResult<axum::response::Response> {
    let req = GetNextcloudPreviewResourceRequest {
        server_id: query.server_id,
        credential_owner_id: query.credential_owner_id,
        file_id: query.file_id,
        width: query.width,
        height: query.height,
        crop: query.crop,
        sig: query.sig,
        uid: query.uid,
        rid: room_id,
        exp: query.exp,
    };
    let state_for_stream = state.clone();
    stream_http_response::<NextcloudPreviewResourceResponse, _>(
        state,
        request_meta,
        Method::GET,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::nextcloud::get_nextcloud_preview_resource(
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

fn deps<'a>(
    state: &'a AppState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> synctv_api_common::playback_provider::nextcloud::NextcloudPlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::nextcloud::NextcloudPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.nextcloud_playback_provider_service,
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
            nextcloud_hls_resource_kind("media").expect("media route should be valid"),
            NextcloudHlsResourceKind::Media as i32
        );
        assert_eq!(
            nextcloud_hls_resource_kind("manifest").expect("manifest route should be valid"),
            NextcloudHlsResourceKind::Manifest as i32
        );
        assert!(nextcloud_hls_resource_kind("segment").is_err());
    }
}
