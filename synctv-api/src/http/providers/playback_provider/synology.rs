use axum::{
    extract::{Path, RawQuery, State},
    http::{HeaderMap, Method},
};
use futures::FutureExt;
use synctv_proto::playback_provider::synology::{
    GetSynologyResourceRequest, GetSynologySegmentRequest, GetSynologySubtitleRequest,
    SynologyResourceResponse, SynologySegmentResponse, SynologySubtitleResponse,
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
pub struct SynologyResourcePath {
    pub version: String,
    pub mode_name: String,
    pub media_index: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SynologySubtitlePath {
    pub version: String,
    pub mode_name: String,
    pub subtitle_index: u32,
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

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/playback-providers/synology/{version}/resources/{modeName}/{mediaIndex}",
        tag = "Synology Playback Provider",
        params(
            ("version" = String, Path),
            ("modeName" = String, Path),
            ("mediaIndex" = u32, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("rid" = String, Query),
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
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
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
                crate::impls::playback_provider::synology::get_synology_resource(
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
        path = "/api/playback-providers/synology/{version}/segments",
        tag = "Synology Playback Provider",
        params(
            ("version" = String, Path),
            ("targetUrl" = String, Query),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("rid" = String, Query),
            ("exp" = i64, Query)
        ),
        responses((status = 200, description = "Synology Video Station HLS segment"))
    )
)]
pub fn get_synology_segment(
    Path(version): Path<String>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    synology_segment(
        version,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::GET,
    )
}

pub fn head_synology_segment(
    Path(version): Path<String>,
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    raw_query: RawQuery,
) -> impl futures::Future<Output = AppResult<axum::response::Response>> + Send + 'static {
    synology_segment(
        version,
        state,
        request_meta,
        headers,
        query(raw_query),
        Method::HEAD,
    )
}

async fn synology_segment(
    version: String,
    state: AppState,
    request_meta: RequestMetadata,
    headers: HeaderMap,
    query_string: String,
    method: Method,
) -> AppResult<axum::response::Response> {
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
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
                crate::impls::playback_provider::synology::get_synology_segment(
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
        path = "/api/playback-providers/synology/{version}/subtitles/{modeName}/{subtitleIndex}",
        tag = "Synology Playback Provider",
        params(
            ("version" = String, Path),
            ("modeName" = String, Path),
            ("subtitleIndex" = u32, Path),
            ("sig" = String, Query),
            ("uid" = String, Query),
            ("rid" = String, Query),
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
    let (sig, uid, rid, exp) =
        signed_query_fields(&query_string).map_err(crate::http::error::map_api_error)?;
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
                crate::impls::playback_provider::synology::get_synology_subtitle(
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
) -> crate::impls::playback_provider::synology::SynologyPlaybackProviderDeps<'a> {
    crate::impls::playback_provider::synology::SynologyPlaybackProviderDeps {
        playback_provider_service: &state.shared_api_runtime.synology_playback_provider_service,
        runtime: super::playback_provider_api_runtime(state),
        request_control,
    }
}
