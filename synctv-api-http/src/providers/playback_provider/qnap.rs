use axum::{
    extract::{Path, Query, RawQuery, State},
    http::{HeaderMap, Method},
};
use futures::FutureExt;
use synctv_proto::playback_provider::qnap::{
    GetQnapHlsManifestRequest, GetQnapHlsResourceRequest, GetQnapResourceRequest,
    GetQnapSubtitleRequest, GetQnapThumbnailRequest, GetQnapThumbnailResourceRequest,
    QnapHlsManifestResponse, QnapHlsResourceKind, QnapHlsResourceResponse, QnapResourceResponse,
    QnapSubtitleResponse, QnapThumbnailResourceResponse, QnapThumbnailResponse,
};

use crate::http::{middleware::RequestMetadata, AppResult, AppState};
use crate::providers::playback_provider::transport::{
    query, range_header, signed_query_fields, stream_http_response, target_url,
    PlaybackProviderHttpResponse,
};

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QnapResourcePath {
    pub room_id: String,
    pub version: String,
    pub mode_name: String,
    pub media_index: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QnapHlsResourcePath {
    pub room_id: String,
    pub version: String,
    pub mode_name: String,
    pub media_index: u32,
    pub resource_kind: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QnapSubtitlePath {
    pub room_id: String,
    pub version: String,
    pub mode_name: String,
    pub subtitle_index: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QnapThumbnailResourceQuery {
    pub server_id: String,
    pub credential_owner_id: String,
    pub path: String,
    pub size: u32,
    pub sig: String,
    pub uid: String,
    pub exp: i64,
}

impl PlaybackProviderHttpResponse for QnapResourceResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for QnapHlsManifestResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for QnapHlsResourceResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for QnapSubtitleResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for QnapThumbnailResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for QnapThumbnailResourceResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/playback-providers/{roomId}/qnap/{version}/resources/{modeName}/{mediaIndex}",
        tag = "QNAP Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path),
            ("modeName" = String, Path),
            ("mediaIndex" = u32, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses((status = 200, description = "QNAP media resource"))
    )
)]
pub fn get_qnap_resource(
    Path(path): Path<QnapResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    qnap_resource(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::GET,
    )
}

pub fn head_qnap_resource(
    Path(path): Path<QnapResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    qnap_resource(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn qnap_resource(
    path: QnapResourcePath,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = GetQnapResourceRequest {
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
    stream_http_response::<QnapResourceResponse, _>(state, request_meta, method, move |control| {
        let state = stream_state;
        async move {
            synctv_api_common::playback_provider::qnap::get_qnap_resource(
                deps(&state, Some(&control)),
                req,
            )
            .await
        }
        .boxed()
    })
    .await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/playback-providers/{roomId}/qnap/{version}/hls-manifests/{modeName}/{mediaIndex}",
        tag = "QNAP Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path),
            ("modeName" = String, Path),
            ("mediaIndex" = u32, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses((status = 200, description = "QNAP HLS manifest"))
    )
)]
pub async fn get_qnap_hls_manifest(
    Path(path): Path<QnapResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    qnap_hls_manifest(path, state, request_meta, query(raw_query), Method::GET).await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        head,
        path = "/api/playback-providers/{roomId}/qnap/{version}/hls-manifests/{modeName}/{mediaIndex}",
        tag = "QNAP Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path),
            ("modeName" = String, Path),
            ("mediaIndex" = u32, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses((status = 200, description = "QNAP HLS manifest metadata"))
    )
)]
pub async fn head_qnap_hls_manifest(
    Path(path): Path<QnapResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    qnap_hls_manifest(path, state, request_meta, query(raw_query), Method::HEAD).await
}

async fn qnap_hls_manifest(
    path: QnapResourcePath,
    state: AppState,
    request_meta: RequestMetadata,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = GetQnapHlsManifestRequest {
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
    stream_http_response::<QnapHlsManifestResponse, _>(
        state,
        request_meta,
        method,
        move |control| {
            let state = stream_state;
            async move {
                synctv_api_common::playback_provider::qnap::get_qnap_hls_manifest(
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
        path = "/api/playback-providers/{roomId}/qnap/{version}/hls-resources/{modeName}/{mediaIndex}/{resourceKind}",
        tag = "QNAP Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path),
            ("modeName" = String, Path),
            ("mediaIndex" = u32, Path),
            ("resourceKind" = String, Path),
            ("targetUrl" = String, Query),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses((status = 200, description = "QNAP HLS resource"))
    )
)]
pub fn get_qnap_hls_resource(
    Path(path): Path<QnapHlsResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    qnap_hls_resource(
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
        path = "/api/playback-providers/{roomId}/qnap/{version}/hls-resources/{modeName}/{mediaIndex}/{resourceKind}",
        tag = "QNAP Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path),
            ("modeName" = String, Path),
            ("mediaIndex" = u32, Path),
            ("resourceKind" = String, Path),
            ("targetUrl" = String, Query),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses((status = 200, description = "QNAP HLS resource metadata"))
    )
)]
pub fn head_qnap_hls_resource(
    Path(path): Path<QnapHlsResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    qnap_hls_resource(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn qnap_hls_resource(
    path: QnapHlsResourcePath,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = GetQnapHlsResourceRequest {
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
        resource_kind: qnap_hls_resource_kind(&path.resource_kind)?,
    };
    let stream_state = state.clone();
    stream_http_response::<QnapHlsResourceResponse, _>(
        state,
        request_meta,
        method,
        move |control| {
            let state = stream_state;
            async move {
                synctv_api_common::playback_provider::qnap::get_qnap_hls_resource(
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

fn qnap_hls_resource_kind(value: &str) -> AppResult<i32> {
    match value {
        "media" => Ok(QnapHlsResourceKind::Media as i32),
        "manifest" => Ok(QnapHlsResourceKind::Manifest as i32),
        _ => Err(crate::http::error::map_api_error(
            synctv_api_common::impls::ApiError::InvalidInput(
                "Invalid QNAP HLS resource kind".to_string(),
            ),
        )),
    }
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/playback-providers/{roomId}/qnap/{version}/subtitles/{modeName}/{subtitleIndex}",
        tag = "QNAP Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path),
            ("modeName" = String, Path),
            ("subtitleIndex" = u32, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses((status = 200, description = "QNAP subtitle"))
    )
)]
pub async fn get_qnap_subtitle(
    Path(path): Path<QnapSubtitlePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = GetQnapSubtitleRequest {
        version: path.version,
        mode_name: path.mode_name,
        subtitle_index: path.subtitle_index,
        sig,
        uid,
        rid,
        exp,
    };
    let stream_state = state.clone();
    stream_http_response::<QnapSubtitleResponse, _>(
        state,
        request_meta,
        Method::GET,
        move |control| {
            let state = stream_state;
            async move {
                synctv_api_common::playback_provider::qnap::get_qnap_subtitle(
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
        path = "/api/playback-providers/{roomId}/qnap/{version}/thumbnail",
        tag = "QNAP Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses((status = 200, description = "QNAP thumbnail"))
    )
)]
pub async fn get_qnap_thumbnail(
    Path((room_id, version)): Path<(String, String)>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string, &room_id).map_err(crate::http::error::map_api_error)?;
    let req = GetQnapThumbnailRequest {
        version,
        sig,
        uid,
        rid,
        exp,
    };
    let stream_state = state.clone();
    stream_http_response::<QnapThumbnailResponse, _>(
        state,
        request_meta,
        Method::GET,
        move |control| {
            let state = stream_state;
            async move {
                synctv_api_common::playback_provider::qnap::get_qnap_thumbnail(
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
        path = "/api/playback-providers/{roomId}/qnap/thumbnail",
        tag = "QNAP Playback Provider",
        params(
            ("roomId" = String, Path), ("serverId" = String, Query),
            ("credentialOwnerId" = String, Query), ("path" = String, Query),
            ("size" = u32, Query), ("sig" = String, Query),
            ("uid" = String, Query), ("exp" = i64, Query)
        ),
        responses((status = 200, description = "Room-scoped QNAP thumbnail"))
    )
)]
pub async fn get_qnap_thumbnail_resource(
    Path(room_id): Path<String>,
    Query(query): Query<QnapThumbnailResourceQuery>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
) -> AppResult<axum::response::Response> {
    let req = GetQnapThumbnailResourceRequest {
        server_id: query.server_id,
        credential_owner_id: query.credential_owner_id,
        path: query.path,
        size: query.size,
        sig: query.sig,
        uid: query.uid,
        rid: room_id,
        exp: query.exp,
    };
    let state_for_stream = state.clone();
    stream_http_response::<QnapThumbnailResourceResponse, _>(
        state,
        request_meta,
        Method::GET,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::qnap::get_qnap_thumbnail_resource(
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
) -> synctv_api_common::playback_provider::qnap::QnapPlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::qnap::QnapPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.qnap_playback_provider_service,
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
            qnap_hls_resource_kind("media").expect("media route should be valid"),
            QnapHlsResourceKind::Media as i32
        );
        assert_eq!(
            qnap_hls_resource_kind("manifest").expect("manifest route should be valid"),
            QnapHlsResourceKind::Manifest as i32
        );
        assert!(qnap_hls_resource_kind("segment").is_err());
    }
}
