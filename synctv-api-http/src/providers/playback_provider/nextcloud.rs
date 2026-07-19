use axum::{
    extract::{Path, RawQuery, State},
    http::{HeaderMap, Method},
};
use futures::FutureExt;
use synctv_proto::playback_provider::nextcloud::{
    GetNextcloudResourceRequest, GetNextcloudSubtitleRequest, NextcloudResourceResponse,
    NextcloudSubtitleResponse,
};

use crate::http::{middleware::RequestMetadata, AppResult, AppState};
use crate::providers::playback_provider::transport::{
    query, range_header, signed_query_fields, stream_http_response, PlaybackProviderHttpResponse,
};

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NextcloudResourcePath {
    pub version: String,
    pub mode_name: String,
    pub media_index: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NextcloudSubtitlePath {
    pub version: String,
    pub mode_name: String,
    pub subtitle_index: u32,
}

impl PlaybackProviderHttpResponse for NextcloudResourceResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

impl PlaybackProviderHttpResponse for NextcloudSubtitleResponse {
    fn chunk(self) -> Option<synctv_proto::playback_provider::common::StreamChunk> {
        self.chunk
    }
}

#[cfg_attr(feature = "openapi", utoipa::path(get, path = "/api/playback-providers/nextcloud/{version}/resources/{modeName}/{mediaIndex}", tag = "Nextcloud Playback Provider", params(("version" = String, Path), ("modeName" = String, Path), ("mediaIndex" = u32, Path), ("sig" = String, Query), ("uid" = String, Query), ("rid" = String, Query), ("exp" = i64, Query)), responses((status = 200, description = "Nextcloud media resource"))))]
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
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
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

#[cfg_attr(feature = "openapi", utoipa::path(get, path = "/api/playback-providers/nextcloud/{version}/subtitles/{modeName}/{subtitleIndex}", tag = "Nextcloud Playback Provider", params(("version" = String, Path), ("modeName" = String, Path), ("subtitleIndex" = u32, Path), ("sig" = String, Query), ("uid" = String, Query), ("rid" = String, Query), ("exp" = i64, Query)), responses((status = 200, description = "Nextcloud subtitle"))))]
pub async fn get_nextcloud_subtitle(
    Path(path): Path<NextcloudSubtitlePath>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    raw_query: RawQuery,
) -> AppResult<axum::response::Response> {
    let query_string = query(raw_query);
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
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
