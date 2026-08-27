use std::sync::Arc;

use axum::{
    body::{to_bytes, Body},
    extract::{Path, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};

use super::{
    error::map_api_error, middleware::RequestMetadata, room::execute_room_actor_endpoint, AppError,
    AppResult, AppState,
};
use synctv_api_common::impls::{
    map_livestream_backend_error, EndpointRateLimitCategory, EndpointRateLimitScope,
};
use synctv_livestream::{LiveStreamingInfrastructure, StreamError, WebRtcAnswer};

const SDP_CONTENT_TYPE: &str = "application/sdp";

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveWebRtcPath {
    room_id: String,
    media_id: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveWebRtcSessionPath {
    room_id: String,
    media_id: String,
    session_id: String,
}

fn infrastructure(state: &AppState) -> AppResult<Arc<LiveStreamingInfrastructure>> {
    state
        .live_streaming_infrastructure
        .clone()
        .ok_or_else(AppError::service_unavailable)
}

fn map_stream_error(error: &StreamError) -> AppError {
    map_api_error(map_livestream_backend_error(error))
}

fn remote_address(request_meta: &RequestMetadata) -> String {
    request_meta
        .0
        .client_ip
        .or(request_meta.0.socket_ip)
        .map_or_else(String::new, |address| address.to_string())
}

fn publish_token(request_meta: &RequestMetadata) -> AppResult<String> {
    let authorization = request_meta
        .0
        .authorization
        .as_deref()
        .ok_or_else(|| AppError::unauthorized("Publish key is required"))?;
    synctv_core::service::JwtValidator::extract_bearer_token(authorization)
        .map_err(|_| AppError::invalid_authorization_header())
}

fn validate_sdp_content_type(headers: &HeaderMap) -> AppResult<()> {
    super::reject_duplicate_header(headers, &header::CONTENT_TYPE)?;
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim);
    if content_type.is_some_and(|value| value.eq_ignore_ascii_case(SDP_CONTENT_TYPE)) {
        return Ok(());
    }
    Err(AppError::new(
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "Content-Type must be application/sdp",
    ))
}

async fn read_sdp_offer(request: Request, max_sdp_bytes: usize) -> AppResult<String> {
    validate_sdp_content_type(request.headers())?;
    super::reject_duplicate_header(request.headers(), &header::CONTENT_LENGTH)?;
    if let Some(content_length) = request.headers().get(header::CONTENT_LENGTH) {
        let content_length = content_length
            .to_str()
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .ok_or_else(|| AppError::bad_request("Invalid Content-Length header"))?;
        if content_length > max_sdp_bytes {
            return Err(AppError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "SDP offer exceeds the configured size limit",
            ));
        }
    }
    let body = to_bytes(request.into_body(), max_sdp_bytes)
        .await
        .map_err(|_| {
            AppError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "SDP offer exceeds the configured size limit",
            )
        })?;
    String::from_utf8(body.to_vec()).map_err(|_| AppError::bad_request("SDP offer is not UTF-8"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WhepProvider {
    Live,
    LiveProxy,
}

impl WhepProvider {
    const fn route_slug(self) -> &'static str {
        match self {
            Self::Live => synctv_core::provider::RtmpProvider::PUBLIC_NAME,
            Self::LiveProxy => synctv_core::provider::LiveProxyProvider::PUBLIC_NAME,
        }
    }
}

fn whip_session_location(path: &LiveWebRtcPath, session_id: &str) -> String {
    format!(
        "/api/rooms/{}/streams/{}/whip/{session_id}",
        path.room_id, path.media_id
    )
}

fn whep_session_location(
    path: &LiveWebRtcPath,
    provider: WhepProvider,
    session_id: &str,
) -> String {
    format!(
        "/api/playback-providers/{}/{}/{}/whep/{session_id}",
        path.room_id,
        provider.route_slug(),
        path.media_id
    )
}

fn created_session_response(answer: WebRtcAnswer, location: &str) -> AppResult<Response> {
    let mut response = Response::new(Body::from(answer.answer_sdp));
    *response.status_mut() = StatusCode::CREATED;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(SDP_CONTENT_TYPE),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(location)
            .map_err(|_| AppError::internal_server_error("Invalid WebRTC session location"))?,
    );
    Ok(response)
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{roomId}/streams/{mediaId}/whip",
        tag = "Live Streaming",
        params(
            ("roomId" = String, Path, description = "Room public ID"),
            ("mediaId" = String, Path, description = "Media public ID")
        ),
        request_body(content = String, content_type = "application/sdp"),
        responses(
            (status = 201, description = "WHIP resource created", body = String, content_type = "application/sdp"),
            (status = 400, description = "Invalid SDP", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Invalid publish key", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 409, description = "Stream is already published", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 413, description = "SDP exceeds configured limit", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 415, description = "Content-Type is not application/sdp", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "WebRTC session capacity exhausted", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub async fn create_whip_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<LiveWebRtcPath>,
    request: Request,
) -> AppResult<Response> {
    let infrastructure = infrastructure(&state)?;
    let manager = infrastructure.webrtc_session_manager();
    let token = publish_token(&request_meta)?;
    let remote_addr = remote_address(&request_meta);
    let offer = read_sdp_offer(request, manager.max_sdp_bytes()).await?;
    let answer = manager
        .publish_whip(&path.room_id, &path.media_id, &token, &offer, &remote_addr)
        .await
        .map_err(|error| map_stream_error(&error))?;
    let location = whip_session_location(&path, &answer.session_id);
    created_session_response(answer, &location)
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/rooms/{roomId}/streams/{mediaId}/whip/{sessionId}",
        tag = "Live Streaming",
        params(
            ("roomId" = String, Path, description = "Room public ID"),
            ("mediaId" = String, Path, description = "Media public ID"),
            ("sessionId" = String, Path, description = "WHIP resource ID")
        ),
        responses(
            (status = 204, description = "WHIP resource deleted"),
            (status = 401, description = "Invalid publish key", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "WHIP resource not found", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub async fn delete_whip_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<LiveWebRtcSessionPath>,
) -> AppResult<Response> {
    let infrastructure = infrastructure(&state)?;
    let room_id = synctv_api_common::impls::proto_validated_room_id(
        &path.room_id,
        &state.shared_api_runtime.public_id_codec,
    )
    .map_err(map_api_error)?;
    let media_id = synctv_api_common::impls::proto_validated_media_id(
        &path.media_id,
        &state.shared_api_runtime.public_id_codec,
    )
    .map_err(map_api_error)?;
    let token = publish_token(&request_meta)?;
    let deleted = infrastructure
        .delete_whip_session(
            &path.session_id,
            &room_id.to_string(),
            &media_id.to_string(),
            &token,
        )
        .await
        .map_err(|error| map_stream_error(&error))?;
    if !deleted {
        return Err(AppError::not_found("WHIP session not found"));
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/playback-providers/{roomId}/live/{mediaId}/whep",
        tag = "Live Playback Provider",
        params(
            ("roomId" = String, Path, description = "Room public ID"),
            ("mediaId" = String, Path, description = "Media public ID")
        ),
        request_body(content = String, content_type = "application/sdp"),
        responses(
            (status = 201, description = "WHEP resource created", body = String, content_type = "application/sdp"),
            (status = 400, description = "Invalid SDP or source has no RTP", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Playback permission denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Room, media, or publisher not found", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 413, description = "SDP exceeds configured limit", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 415, description = "Content-Type is not application/sdp", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "WebRTC session capacity exhausted", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub async fn create_live_whep_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<LiveWebRtcPath>,
    request: Request,
) -> AppResult<Response> {
    create_whep_session(Some(WhepProvider::Live), request_meta, state, path, request).await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/playback-providers/{roomId}/live-proxy/{mediaId}/whep",
        tag = "LiveProxy Playback Provider",
        params(
            ("roomId" = String, Path, description = "Room public ID"),
            ("mediaId" = String, Path, description = "Media public ID")
        ),
        request_body(content = String, content_type = "application/sdp"),
        responses(
            (status = 201, description = "WHEP resource created", body = String, content_type = "application/sdp"),
            (status = 400, description = "Invalid SDP, provider, or source has no RTP", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Playback permission denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Room, media, or publisher not found", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 413, description = "SDP exceeds configured limit", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 415, description = "Content-Type is not application/sdp", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "WebRTC session capacity exhausted", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub async fn create_live_proxy_whep_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<LiveWebRtcPath>,
    request: Request,
) -> AppResult<Response> {
    create_whep_session(
        Some(WhepProvider::LiveProxy),
        request_meta,
        state,
        path,
        request,
    )
    .await
}

pub async fn create_legacy_whep_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<LiveWebRtcPath>,
    request: Request,
) -> AppResult<Response> {
    create_whep_session(None, request_meta, state, path, request).await
}

async fn create_whep_session(
    expected_provider: Option<WhepProvider>,
    request_meta: RequestMetadata,
    state: AppState,
    path: LiveWebRtcPath,
    request: Request,
) -> AppResult<Response> {
    let infrastructure = infrastructure(&state)?;
    let manager = infrastructure.webrtc_session_manager();
    let offer = read_sdp_offer(request, manager.max_sdp_bytes()).await?;
    let remote_addr = remote_address(&request_meta);
    let public_room_id = path.room_id.clone();
    let public_media_id = path.media_id.clone();
    let (answer, provider) = execute_room_actor_endpoint(
        &state,
        request_meta,
        public_room_id,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, actor| {
            let infrastructure = Arc::clone(&infrastructure);
            let manager = Arc::clone(&manager);
            async move {
                let authorized = client_api
                    .authorize_live_stream_playback_for_actor(&actor, &public_media_id)
                    .await?;
                let provider = if authorized.external_source.is_some() {
                    WhepProvider::LiveProxy
                } else {
                    WhepProvider::Live
                };
                if expected_provider.is_some_and(|expected| expected != provider) {
                    return Err(synctv_api_common::impls::ApiError::InvalidInput(
                        "Media does not belong to the requested playback provider".to_string(),
                    ));
                }
                let room_id = authorized.room_id.to_string();
                let media_id = authorized.media_id.to_string();
                let guard = match authorized.external_source.as_ref() {
                    Some(source) => {
                        infrastructure
                            .ensure_external_pull_stream(&room_id, &media_id, source)
                            .await
                    }
                    None => {
                        infrastructure
                            .ensure_pull_stream(&room_id, &media_id, None)
                            .await
                    }
                }
                .map_err(|error| map_livestream_backend_error(error.as_ref()))?;
                let answer = manager
                    .play_whep(&room_id, &media_id, &offer, &remote_addr, guard)
                    .await
                    .map_err(|error| map_livestream_backend_error(&error))?;
                Ok((answer, provider))
            }
        },
    )
    .await?;
    let location = whep_session_location(&path, provider, &answer.session_id);
    created_session_response(answer, &location)
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/playback-providers/{roomId}/live/{mediaId}/whep/{sessionId}",
        tag = "Live Playback Provider",
        params(
            ("roomId" = String, Path, description = "Room public ID"),
            ("mediaId" = String, Path, description = "Media public ID"),
            ("sessionId" = String, Path, description = "WHEP resource ID")
        ),
        responses(
            (status = 204, description = "WHEP resource deleted"),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Playback permission denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "WHEP resource not found", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub async fn delete_live_whep_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<LiveWebRtcSessionPath>,
) -> AppResult<Response> {
    delete_whep_session(Some(WhepProvider::Live), request_meta, state, path).await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/playback-providers/{roomId}/live-proxy/{mediaId}/whep/{sessionId}",
        tag = "LiveProxy Playback Provider",
        params(
            ("roomId" = String, Path, description = "Room public ID"),
            ("mediaId" = String, Path, description = "Media public ID"),
            ("sessionId" = String, Path, description = "WHEP resource ID")
        ),
        responses(
            (status = 204, description = "WHEP resource deleted"),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Playback permission denied", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "WHEP resource not found", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub async fn delete_live_proxy_whep_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<LiveWebRtcSessionPath>,
) -> AppResult<Response> {
    delete_whep_session(Some(WhepProvider::LiveProxy), request_meta, state, path).await
}

pub async fn delete_legacy_whep_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<LiveWebRtcSessionPath>,
) -> AppResult<Response> {
    delete_whep_session(None, request_meta, state, path).await
}

async fn delete_whep_session(
    expected_provider: Option<WhepProvider>,
    request_meta: RequestMetadata,
    state: AppState,
    path: LiveWebRtcSessionPath,
) -> AppResult<Response> {
    let infrastructure = infrastructure(&state)?;
    let public_room_id = path.room_id.clone();
    let public_media_id = path.media_id.clone();
    let session_id = path.session_id;
    let deleted = execute_room_actor_endpoint(
        &state,
        request_meta,
        public_room_id,
        EndpointRateLimitCategory::Media,
        EndpointRateLimitScope::RoomMedia,
        move |client_api, actor| async move {
            let authorized = client_api
                .authorize_live_stream_playback_for_actor(&actor, &public_media_id)
                .await?;
            let provider = if authorized.external_source.is_some() {
                WhepProvider::LiveProxy
            } else {
                WhepProvider::Live
            };
            if expected_provider.is_some_and(|expected| expected != provider) {
                return Err(synctv_api_common::impls::ApiError::InvalidInput(
                    "Media does not belong to the requested playback provider".to_string(),
                ));
            }
            infrastructure
                .delete_whep_session(
                    &session_id,
                    &authorized.room_id.to_string(),
                    &authorized.media_id.to_string(),
                )
                .await
                .map_err(|error| map_livestream_backend_error(&error))
        },
    )
    .await?;
    if !deleted {
        return Err(AppError::not_found("WHEP session not found"));
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sdp_request(body: &'static str) -> Request {
        Request::builder()
            .header(header::CONTENT_TYPE, SDP_CONTENT_TYPE)
            .body(Body::from(body))
            .expect("valid SDP request")
    }

    #[tokio::test]
    async fn oversized_sdp_offer_returns_payload_too_large() {
        let error = read_sdp_offer(sdp_request("12345"), 4)
            .await
            .expect_err("oversized SDP must be rejected");
        assert_eq!(error.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn non_sdp_content_type_returns_unsupported_media_type() {
        let request = Request::builder()
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .expect("valid HTTP request");
        let error = read_sdp_offer(request, 1024)
            .await
            .expect_err("non-SDP content type must be rejected");
        assert_eq!(error.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[test]
    fn invalid_publish_key_maps_to_unauthorized() {
        let error = map_stream_error(&StreamError::Authentication(
            "invalid publish key".to_string(),
        ));
        assert_eq!(error.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn session_locations_use_canonical_routes() {
        let path = LiveWebRtcPath {
            room_id: "room-public".to_string(),
            media_id: "media-public".to_string(),
        };

        assert_eq!(
            whip_session_location(&path, "whip-session"),
            "/api/rooms/room-public/streams/media-public/whip/whip-session"
        );
        assert_eq!(
            whep_session_location(&path, WhepProvider::Live, "live-session"),
            "/api/playback-providers/room-public/live/media-public/whep/live-session"
        );
        assert_eq!(
            whep_session_location(&path, WhepProvider::LiveProxy, "proxy-session"),
            "/api/playback-providers/room-public/live-proxy/media-public/whep/proxy-session"
        );
    }
}
