//! Bilibili Provider HTTP Routes

use std::collections::HashMap;
use std::convert::Infallible;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    routing::{get, post},
    Json, Router,
};
use futures::stream::Stream;
use serde_json::json;

use crate::http::{AppError, AppState, error::AppResult, middleware::AuthUser, provider_common::InstanceQuery};
use crate::impls::provider::{resolve_media_from_playlist, resolve_provider_playback_result};

use synctv_core::models::{MediaId, RoomId};

/// Build Bilibili HTTP routes
pub fn bilibili_routes() -> Router<AppState> {
    Router::new()
        .route("/parse", post(parse))
        .route("/login/qr/generate", post(login_qr))
        .route("/login/qr/check", post(qr_check))
        .route("/login/captcha", post(new_captcha))
        .route("/login/sms/send", post(sms_send))
        .route("/login/sms/login", post(sms_login))
        .route("/me", get(user_info))
        .route("/logout", post(logout))
        // Provider-specific proxy routes
        // Note: MPD and stream proxying removed as DASH manifest structures were simplified
        .route(
            "/proxy/{room_id}/{media_id}/subtitle/{name}",
            get(proxy_subtitle).options(synctv_proxy::proxy_options_preflight),
        )
        .route("/proxy/{room_id}/{media_id}/m3u8", get(proxy_m3u8))
        .route("/proxy/{room_id}/{media_id}/danmu", get(danmu_sse))
}

// ------------------------------------------------------------------
// Proxy handlers
// ------------------------------------------------------------------

/// GET /`proxy/:room_id/:media_id/subtitle/:name` - Proxy subtitle
async fn proxy_subtitle(
    auth: AuthUser,
    Path((room_id, media_id, name)): Path<(String, String, String)>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<axum::response::Response> {
    let room_id_parsed = RoomId::from_string(room_id);
    let media_id_parsed = MediaId::from_string(media_id);

    let result =
        {
            let resolved = state.resolve_redis_conn().await;
            resolve_provider_playback_result(
                &auth.user_id,
                &room_id_parsed,
                &media_id_parsed,
                state.bilibili_provider.as_ref(),
                &state.room_service,
                resolved.as_ref(),
                state.credential_encryption.as_ref(),
            ).await
        }
        .map_err(crate::http::error::map_api_error)?;

    // Find subtitle by name across all playback infos
    let subtitle_url = result
        .playback_infos
        .values()
        .flat_map(|pi| &pi.subtitles)
        .find(|s| s.name == name)
        .map(|s| s.url.clone())
        .ok_or_else(|| anyhow::anyhow!("Subtitle '{name}' not found"))?;

    let provider_headers = synctv_core::provider::bilibili_headers();

    let cfg = synctv_proxy::ProxyConfig {
        url: &subtitle_url,
        provider_headers: &provider_headers,
        client_headers: &headers,
    };

    synctv_proxy::proxy_fetch_and_forward(cfg, &synctv_proxy::NoopMetrics)
        .await
        .map_err(Into::into)
}

/// GET /`proxy/:room_id/:media_id/m3u8` - Proxy Bilibili M3U8 (for live streams)
async fn proxy_m3u8(
    auth: AuthUser,
    Path((room_id, media_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> AppResult<axum::response::Response> {
    let room_id_parsed = RoomId::from_string(room_id.clone());
    let media_id_parsed = MediaId::from_string(media_id.clone());

    let result = {
        let resolved = state.resolve_redis_conn().await;
        resolve_provider_playback_result(
            &auth.user_id,
            &room_id_parsed,
            &media_id_parsed,
            state.bilibili_provider.as_ref(),
            &state.room_service,
            resolved.as_ref(),
            None,
        ).await
    }
        .map_err(crate::http::error::map_api_error)?;

    let default_info = result
        .playback_infos
        .get(&result.default_mode)
        .ok_or_else(|| anyhow::anyhow!("Default playback mode not found"))?;

    let url = default_info
        .urls
        .first()
        .ok_or_else(|| anyhow::anyhow!("No URLs in playback info"))?;

    let proxy_base = format!("/api/providers/bilibili/proxy/{room_id}/{media_id}");

    synctv_proxy::proxy_m3u8_and_rewrite(url, &default_info.headers, &proxy_base)
        .await
        .map_err(Into::into)
}

/// GET /`proxy/:room_id/:media_id/danmu` - Bilibili danmaku SSE
///
/// Returns danmaku server connection info as SSE events.
/// The client uses this info to connect to Bilibili's WebSocket danmu servers directly.
///
/// Events emitted:
/// - `danmu_info`: JSON with `token` and `host_list` for WebSocket connection
/// - `error`: If the media is not a live stream or danmu info cannot be fetched
async fn danmu_sse(
    auth: AuthUser,
    Path((room_id, media_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let room_id_parsed = RoomId::from_string(room_id);
    let media_id_parsed = MediaId::from_string(media_id);

    // Resolve media from playlist to get source_config
    let result = resolve_danmu_info(&auth, &room_id_parsed, &media_id_parsed, &state).await;

    let stream = futures::stream::once(async move {
        match result {
            Ok(danmu_event) => Ok(danmu_event),
            Err(e) => Ok(Event::default()
                .event("error")
                .data(json!({"error": e.to_string()}).to_string())),
        }
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("keep-alive"),
    )
}

/// Resolve danmaku connection info from a media item's source config.
///
/// Only Bilibili live streams have danmaku support.
/// Returns an SSE Event with danmu server connection details.
///
/// Note: `auth` is validated by the `AuthUser` extractor in the calling handler.
async fn resolve_danmu_info(
    auth: &AuthUser,
    room_id: &RoomId,
    media_id: &MediaId,
    state: &AppState,
) -> Result<Event, anyhow::Error> {
    let media = resolve_media_from_playlist(
        &auth.user_id,
        room_id,
        media_id,
        &state.room_service,
    ).await
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    // Parse source_config to determine if this is a live stream
    #[derive(serde::Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum SourceType {
        Live {
            room_id: u64,
            #[serde(default)]
            cookies: HashMap<String, String>,
            #[serde(default)]
            provider_instance_name: Option<String>,
        },
        #[serde(other)]
        Other,
    }

    let source: SourceType = serde_json::from_value(media.source_config.clone())
        .map_err(|e| anyhow::anyhow!("Failed to parse source config: {e}"))?;

    match source {
        SourceType::Live {
            room_id: bilibili_room_id,
            cookies,
            provider_instance_name,
        } => {
            let danmu_resp = state
                .bilibili_provider
                .get_live_danmu_info(
                    bilibili_room_id,
                    cookies,
                    provider_instance_name.as_deref(),
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to get danmu info: {e}"))?;

            let event_data = json!({
                "token": danmu_resp.token,
                "host_list": danmu_resp.host_list.iter().map(|h| {
                    json!({
                        "host": h.host,
                        "port": h.port,
                        "wss_port": h.wss_port,
                        "ws_port": h.ws_port,
                    })
                }).collect::<Vec<_>>(),
            });

            Ok(Event::default()
                .event("danmu_info")
                .data(event_data.to_string()))
        }
        SourceType::Other => Err(anyhow::anyhow!(
            "Danmaku is only available for Bilibili live streams"
        )),
    }
}

// bilibili_proxy_headers() removed: use synctv_core::provider::bilibili_headers() instead.

// ------------------------------------------------------------------
// Existing provider API handlers
// ------------------------------------------------------------------

/// Parse Bilibili URL
///
/// Rate limiting is handled by the global read_rate_limit middleware.
async fn parse(
    _auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
    Json(req): Json<crate::proto::providers::bilibili::ParseRequest>,
) -> axum::response::Response {
    tracing::info!("Bilibili parse request");

    let api = &state.bilibili_api;

    match api.parse(req, query.as_deref()).await {
        Ok(resp) => {
            (StatusCode::OK, Json(json!(resp))).into_response()
        }
        Err(e) => {
            tracing::error!("Bilibili parse failed: {}", e);
            AppError::from(e).into_response()
        }
    }
}

/// Generate Bilibili QR code for login
///
/// Rate limiting is handled by the global read_rate_limit middleware.
async fn login_qr(
    _auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
) -> axum::response::Response {
    tracing::info!("Bilibili login QR request");

    let api = &state.bilibili_api;
    let req = crate::proto::providers::bilibili::LoginQrRequest::default();

    match api.login_qr(req, query.as_deref()).await {
        Ok(resp) => {
            (StatusCode::OK, Json(json!(resp))).into_response()
        }
        Err(e) => {
            tracing::error!("Failed to generate QR code: {}", e);
            AppError::from(e).into_response()
        }
    }
}

/// Check Bilibili QR code login status
///
/// Rate limiting is handled by the global read_rate_limit middleware.
async fn qr_check(
    _auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
    Json(req): Json<crate::proto::providers::bilibili::CheckQrRequest>,
) -> axum::response::Response {
    tracing::info!("Bilibili QR check");

    let api = &state.bilibili_api;

    match api.check_qr(req, query.as_deref()).await {
        Ok(resp) => {
            (StatusCode::OK, Json(json!(resp))).into_response()
        }
        Err(e) => {
            tracing::error!("Failed to check QR status: {}", e);
            AppError::from(e).into_response()
        }
    }
}

/// Get captcha for SMS login
///
/// Rate limiting is handled by the global read_rate_limit middleware.
async fn new_captcha(
    _auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
) -> axum::response::Response {
    tracing::info!("Bilibili new captcha request");

    let api = &state.bilibili_api;
    let req = crate::proto::providers::bilibili::GetCaptchaRequest::default();

    match api.get_captcha(req, query.as_deref()).await {
        Ok(resp) => {
            (StatusCode::OK, Json(json!(resp))).into_response()
        }
        Err(e) => {
            tracing::error!("Failed to get captcha: {}", e);
            AppError::from(e).into_response()
        }
    }
}

/// Send SMS verification code
///
/// Rate limiting is handled by the global read_rate_limit middleware.
async fn sms_send(
    _auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
    Json(req): Json<crate::proto::providers::bilibili::SendSmsRequest>,
) -> axum::response::Response {
    tracing::info!("Bilibili SMS send request");

    let api = &state.bilibili_api;

    match api.send_sms(req, query.as_deref()).await {
        Ok(resp) => {
            (StatusCode::OK, Json(json!(resp))).into_response()
        }
        Err(e) => {
            tracing::error!("Failed to send SMS: {}", e);
            AppError::from(e).into_response()
        }
    }
}

/// Login with SMS code
///
/// Rate limiting is handled by the global read_rate_limit middleware.
async fn sms_login(
    _auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
    Json(req): Json<crate::proto::providers::bilibili::LoginSmsRequest>,
) -> axum::response::Response {
    tracing::info!("Bilibili SMS login request");

    let api = &state.bilibili_api;

    match api.login_sms(req, query.as_deref()).await {
        Ok(resp) => {
            (StatusCode::OK, Json(json!(resp))).into_response()
        }
        Err(e) => {
            tracing::error!("Failed to login with SMS: {}", e);
            AppError::from(e).into_response()
        }
    }
}

/// Get Bilibili user info (cookies are read from server-side provider instance)
async fn user_info(
    _auth: AuthUser,
    State(state): State<AppState>,
    Query(query): Query<InstanceQuery>,
) -> impl IntoResponse {
    tracing::info!("Bilibili user info request");

    let api = &state.bilibili_api;
    let req = crate::proto::providers::bilibili::UserInfoRequest {
        cookies: Default::default(),
        instance_name: query.instance_name.clone().unwrap_or_default(),
    };

    match api.get_user_info(req, query.as_deref()).await {
        Ok(resp) => {
            (StatusCode::OK, Json(json!(resp))).into_response()
        }
        Err(e) => {
            tracing::error!("Failed to get user info: {}", e);
            AppError::from(e).into_response()
        }
    }
}

/// Logout (just return success, cookies are client-side)
async fn logout() -> impl IntoResponse {
    tracing::info!("Bilibili logout request");
    (
        StatusCode::OK,
        Json(json!({"message": "Logout successful"})),
    )
        .into_response()
}
