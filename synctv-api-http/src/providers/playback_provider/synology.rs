use axum::{
    extract::{Path, Query, RawQuery, State},
    http::{HeaderMap, Method},
};
use futures::FutureExt;
use synctv_proto::playback_provider::synology::{
    get_synology_image_resource_request, GetSynologyImageResourceRequest,
    GetSynologyResourceRequest, GetSynologySegmentRequest, GetSynologySubtitleRequest,
    SynologyFileImageResource, SynologyImageResourceResponse, SynologyPosterImageResource,
    SynologyResourceResponse, SynologySegmentResponse, SynologySubtitleResponse,
};

use crate::http::{middleware::RequestMetadata, AppResult, AppState};
use crate::providers::playback_provider::transport::{
    query, range_header, signed_query_fields, stream_http_response, target_url,
    PlaybackProviderHttpResponse,
};

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SynologyResourcePath {
    pub room_id: String,
    pub version: String,
    pub mode_name: String,
    pub media_index: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SynologySubtitlePath {
    pub room_id: String,
    pub version: String,
    pub mode_name: String,
    pub subtitle_index: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SynologyImageResourceQuery {
    pub kind: String,
    pub server_id: String,
    pub credential_owner_id: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub item_id: Option<i64>,
    #[serde(default)]
    pub media_type: Option<String>,
    #[serde(default)]
    pub poster_mtime: Option<String>,
    pub sig: String,
    pub uid: String,
    pub exp: i64,
}

impl PlaybackProviderHttpResponse for SynologyResourceResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for SynologySubtitleResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for SynologySegmentResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for SynologyImageResourceResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/playback-providers/{roomId}/synology/{version}/resources/{modeName}/{mediaIndex}",
        tag = "Synology Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path),
            ("modeName" = String, Path),
            ("mediaIndex" = u32, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses((status = 200, description = "Synology media resource"))
    )
)]
pub fn get_synology_resource(
    Path(path): Path<SynologyResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    synology_resource(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::GET,
    )
}

pub fn head_synology_resource(
    Path(path): Path<SynologyResourcePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    synology_resource(
        path,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn synology_resource(
    path: SynologyResourcePath,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = GetSynologyResourceRequest {
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
    stream_http_response::<SynologyResourceResponse, _>(
        state,
        request_meta,
        method,
        move |control| {
            let state = stream_state;
            async move {
                synctv_api_common::playback_provider::synology::get_synology_resource(
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
        path = "/api/playback-providers/{roomId}/synology/{version}/segments",
        tag = "Synology Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path),
            ("targetUrl" = String, Query),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses((status = 200, description = "Synology Video Station HLS segment"))
    )
)]
pub fn get_synology_segment(
    Path((room_id, version)): Path<(String, String)>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    synology_segment(
        room_id,
        version,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::GET,
    )
}

pub fn head_synology_segment(
    Path((room_id, version)): Path<(String, String)>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    synology_segment(
        room_id,
        version,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn synology_segment(
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
    let req = GetSynologySegmentRequest {
        version,
        target_url: target_url(&query_string).map_err(crate::http::error::map_api_error)?,
        sig,
        uid,
        rid,
        exp,
        range: range_header(&headers).map_err(crate::http::error::map_api_error)?,
        head: method == Method::HEAD,
    };
    let stream_state = state.clone();
    stream_http_response::<SynologySegmentResponse, _>(
        state,
        request_meta,
        method,
        move |control| {
            let state = stream_state;
            async move {
                synctv_api_common::playback_provider::synology::get_synology_segment(
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
        path = "/api/playback-providers/{roomId}/synology/{version}/subtitles/{modeName}/{subtitleIndex}",
        tag = "Synology Playback Provider",
        params(
            ("roomId" = String, Path),
            ("version" = String, Path),
            ("modeName" = String, Path),
            ("subtitleIndex" = u32, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses((status = 200, description = "Synology subtitle"))
    )
)]
pub async fn get_synology_subtitle(
    Path(path): Path<SynologySubtitlePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) = signed_query_fields(&query_string, &path.room_id)
        .map_err(crate::http::error::map_api_error)?;
    let req = GetSynologySubtitleRequest {
        version: path.version,
        mode_name: path.mode_name,
        subtitle_index: path.subtitle_index,
        sig,
        uid,
        rid,
        exp,
    };
    let stream_state = state.clone();
    stream_http_response::<SynologySubtitleResponse, _>(
        state,
        request_meta,
        Method::GET,
        move |control| {
            let state = stream_state;
            async move {
                synctv_api_common::playback_provider::synology::get_synology_subtitle(
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
        path = "/api/playback-providers/{roomId}/synology/image",
        tag = "Synology Playback Provider",
        params(
            ("roomId" = String, Path), ("kind" = String, Query),
            ("serverId" = String, Query), ("credentialOwnerId" = String, Query),
            ("path" = Option<String>, Query), ("size" = Option<String>, Query),
            ("itemId" = Option<i64>, Query), ("mediaType" = Option<String>, Query),
            ("posterMtime" = Option<String>, Query), ("sig" = String, Query),
            ("uid" = String, Query), ("exp" = i64, Query)
        ),
        responses((status = 200, description = "Room-scoped Synology image"))
    )
)]
pub async fn get_synology_image_resource(
    Path(room_id): Path<String>,
    Query(query): Query<SynologyImageResourceQuery>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
) -> AppResult<axum::response::Response> {
    let image = match query.kind.as_str() {
        "file" => {
            let path = query.path.ok_or_else(|| {
                crate::http::AppError::bad_request("Synology file image path is required")
            })?;
            let size = query.size.ok_or_else(|| {
                crate::http::AppError::bad_request("Synology file image size is required")
            })?;
            get_synology_image_resource_request::Image::File(SynologyFileImageResource {
                path,
                size,
            })
        }
        "poster" => {
            let item_id = query.item_id.ok_or_else(|| {
                crate::http::AppError::bad_request("Synology poster item_id is required")
            })?;
            let media_type = query.media_type.ok_or_else(|| {
                crate::http::AppError::bad_request("Synology poster media_type is required")
            })?;
            get_synology_image_resource_request::Image::Poster(SynologyPosterImageResource {
                item_id,
                media_type,
                poster_mtime: query.poster_mtime,
            })
        }
        _ => {
            return Err(crate::http::AppError::bad_request(
                "Synology image kind must be file or poster",
            ));
        }
    };
    let req = GetSynologyImageResourceRequest {
        server_id: query.server_id,
        credential_owner_id: query.credential_owner_id,
        image: Some(image),
        sig: query.sig,
        uid: query.uid,
        rid: room_id,
        exp: query.exp,
    };
    let state_for_stream = state.clone();
    stream_http_response::<SynologyImageResourceResponse, _>(
        state,
        request_meta,
        Method::GET,
        move |request_control| {
            let state = state_for_stream;
            async move {
                synctv_api_common::playback_provider::synology::get_synology_image_resource(
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
) -> synctv_api_common::playback_provider::synology::SynologyPlaybackProviderDeps<'a> {
    synctv_api_common::playback_provider::synology::SynologyPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.synology_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
