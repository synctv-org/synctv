use axum::{
    extract::{Path, Query, RawQuery, State},
    http::{HeaderMap, Method},
};
use futures::FutureExt;
use synctv_proto::playback_provider::emby::{
    EmbyHlsManifestResponse, EmbyHlsResourceKind, EmbyHlsResourceResponse, EmbyMediaStreamResponse,
    EmbySubtitleResponse, EmbyThumbnailResourceResponse, GetEmbyHlsManifestRequest,
    GetEmbyHlsResourceRequest, GetEmbyMediaStreamRequest, GetEmbySubtitleRequest,
    GetEmbyThumbnailResourceRequest,
};

use crate::http::{middleware::RequestMetadata, AppResult, AppState};
use crate::providers::playback_provider::transport::{
    query, range_header, signed_query_fields, stream_http_response, target_url,
    PlaybackProviderHttpResponse,
};

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbyIndexedPath {
    pub room_id: String,
    pub version: String,
    pub mode_name: String,
    pub url_index: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbyHlsResourcePath {
    pub room_id: String,
    pub version: String,
    pub mode_name: String,
    pub media_index: u32,
    pub resource_kind: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbySubtitlePath {
    pub room_id: String,
    pub version: String,
    pub mode_name: String,
    pub subtitle_index: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmbyThumbnailResourceQuery {
    pub server_id: String,
    pub credential_owner_id: String,
    pub max_height: u32,
    #[serde(default)]
    pub max_width: u32,
    pub sig: String,
    pub uid: String,
    pub exp: i64,
}

impl PlaybackProviderHttpResponse for EmbyMediaStreamResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for EmbyHlsManifestResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for EmbyHlsResourceResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for EmbySubtitleResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for EmbyThumbnailResourceResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/playback-providers/{roomId}/emby/{version}/media-streams/{modeName}/{urlIndex}",
        tag = "Emby Playback Provider",
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
            (status = 200, description = "Emby media stream"),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Playback provider resource not found", body = crate::openapi::GoogleRpcStatusSchema)
        )
    )
)]
pub fn get_emby_media_stream(
    Path(path): Path<EmbyIndexedPath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    emby_media_stream(
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
        path = "/api/playback-providers/{roomId}/emby/{version}/media-streams/{modeName}/{urlIndex}",
        tag = "Emby Playback Provider",
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
            (status = 200, description = "Emby media stream metadata"),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Playback provider resource not found", body = crate::openapi::GoogleRpcStatusSchema)
        )
    )
)]
pub fn head_emby_media_stream(
    Path(path): Path<EmbyIndexedPath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    emby_media_stream(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn emby_media_stream(
    path: EmbyIndexedPath,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = GetEmbyMediaStreamRequest {
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
    stream_http_response::<EmbyMediaStreamResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::emby::get_emby_media_stream(
                    emby_deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/{roomId}/emby/{version}/hls-manifests/{modeName}/{urlIndex}",
        tag = "Emby Playback Provider",
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
            (status = 200, description = "Emby HLS manifest"),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Playback provider resource not found", body = crate::openapi::GoogleRpcStatusSchema)
        )
    )
)]
pub async fn get_emby_hls_manifest(
    Path(path): Path<EmbyIndexedPath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = GetEmbyHlsManifestRequest {
        version: path.version,
        mode_name: path.mode_name,
        url_index: path.url_index,
        sig,
        uid,
        rid,
        exp,
    };
    let state_for_stream = state.clone();
    stream_http_response::<EmbyHlsManifestResponse, _>(
        state,
        request_meta,
        Method::GET,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::emby::get_emby_hls_manifest(
                    emby_deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/{roomId}/emby/{version}/hls-resources/{modeName}/{mediaIndex}/{resourceKind}",
        tag = "Emby Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path), ("modeName" = String, Path), ("mediaIndex" = u32, Path), ("resourceKind" = String, Path),
            ("targetUrl" = String, Query),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "Emby HLS resource"),
            (status = 400, description = "Invalid targetUrl", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        )
    )
)]
pub fn get_emby_hls_resource(
    Path(path): Path<EmbyHlsResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    emby_hls_resource(
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
        path = "/api/playback-providers/{roomId}/emby/{version}/hls-resources/{modeName}/{mediaIndex}/{resourceKind}",
        tag = "Emby Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path), ("modeName" = String, Path), ("mediaIndex" = u32, Path), ("resourceKind" = String, Path),
            ("targetUrl" = String, Query),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses(
            (status = 200, description = "Emby HLS resource metadata"),
            (status = 400, description = "Invalid targetUrl", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        )
    )
)]
pub fn head_emby_hls_resource(
    Path(path): Path<EmbyHlsResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    emby_hls_resource(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn emby_hls_resource(
    path: EmbyHlsResourcePath,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = GetEmbyHlsResourceRequest {
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
        resource_kind: emby_hls_resource_kind(&path.resource_kind)?,
    };
    let state_for_stream = state.clone();
    stream_http_response::<EmbyHlsResourceResponse, _>(
        state,
        request_meta,
        method,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::emby::get_emby_hls_resource(
                    emby_deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/{roomId}/emby/{version}/subtitles/{modeName}/{subtitleIndex}",
        tag = "Emby Playback Provider",
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
            (status = 200, description = "Emby subtitle"),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Playback provider resource not found", body = crate::openapi::GoogleRpcStatusSchema)
        )
    )
)]
pub async fn get_emby_subtitle(
    Path(path): Path<EmbySubtitlePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = GetEmbySubtitleRequest {
        version: path.version,
        mode_name: path.mode_name,
        subtitle_index: path.subtitle_index,
        sig,
        uid,
        rid,
        exp,
    };
    let state_for_stream = state.clone();
    stream_http_response::<EmbySubtitleResponse, _>(
        state,
        request_meta,
        Method::GET,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::emby::get_emby_subtitle(
                    emby_deps(&state, Some(&request_control)),
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
        path = "/api/playback-providers/{roomId}/emby/thumbnail/{itemId}",
        tag = "Emby Playback Provider",
        params(
            ("roomId" = String, Path), ("itemId" = String, Path),
            ("serverId" = String, Query), ("credentialOwnerId" = String, Query),
            ("maxHeight" = u32, Query), ("maxWidth" = Option<u32>, Query),
            ("sig" = String, Query), ("uid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses((status = 200, description = "Room-scoped Emby thumbnail"))
    )
)]
pub async fn get_emby_thumbnail_resource(
    Path((room_id, item_id)): Path<(String, String)>,
    Query(query): Query<EmbyThumbnailResourceQuery>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
) -> AppResult<axum::response::Response> {
    let req = GetEmbyThumbnailResourceRequest {
        item_id,
        server_id: query.server_id,
        credential_owner_id: query.credential_owner_id,
        max_height: query.max_height,
        max_width: query.max_width,
        sig: query.sig,
        uid: query.uid,
        rid: room_id,
        exp: query.exp,
    };
    let state_for_stream = state.clone();
    stream_http_response::<EmbyThumbnailResourceResponse, _>(
        state,
        request_meta,
        Method::GET,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::emby::get_emby_thumbnail_resource(
                    emby_deps(&state, Some(&request_control)),
                    req,
                )
                .await
            }
            .boxed()
        },
    )
    .await
}

fn emby_hls_resource_kind(value: &str) -> AppResult<i32> {
    match value {
        "media" => Ok(EmbyHlsResourceKind::Media as i32),
        "manifest" => Ok(EmbyHlsResourceKind::Manifest as i32),
        _ => Err(crate::http::error::map_api_error(
            synctv_api_common::impls::ApiError::InvalidInput(
                "Invalid Emby HLS resource kind".to_string(),
            ),
        )),
    }
}

pub(crate) fn emby_deps<'a>(
    state: &'a AppState,
    request_control: Option<&'a synctv_core::provider::ExecutionControl>,
) -> synctv_api_common::playback_provider::emby::EmbyPlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::emby::EmbyPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.emby_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
