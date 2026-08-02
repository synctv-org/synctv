// HTTP/JSON REST API.
//
// This module is a transport adapter. Business behavior belongs in
// synctv-api-common/src/impls and synctv-core so HTTP and gRPC share one execution
// path. JSON request bodies use protobuf messages; REST handlers compose
// path/query fields into those protobuf requests before calling impls. Bare
// binary bodies, file uploads, and playback streams own raw bytes/streams.
// File downloads still look like ordinary binary HTTP responses; the body is
// backed by FileObjectDownload streams from core storage services.

pub(crate) mod admin;
pub(crate) mod admin_execute;
pub(crate) mod auth;
pub(crate) mod email;
pub(crate) mod error;
pub(crate) mod health;
pub(crate) mod metrics_middleware;
pub(crate) mod middleware;
pub(crate) mod native_app_association;
pub(crate) mod notifications;
pub(crate) mod oauth2;
pub(crate) mod public;
pub(crate) mod room;
pub(crate) mod room_extra;
pub(crate) mod ticket;
pub(crate) mod user;
pub(crate) mod validation;
pub(crate) mod webrtc;
pub(crate) mod websocket;

use crate::providers;
use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    middleware as axum_middleware,
    response::{IntoResponse, Redirect},
    routing::{get, post},
    Router,
};
use futures::StreamExt;
use std::sync::{Arc, LazyLock};
use tower_http::compression::{
    predicate::{DefaultPredicate, Predicate},
    CompressionLayer,
};
use tower_http::cors::CorsLayer;
use tower_http::on_early_drop::{EarlyDropsAsFailures, OnEarlyDropLayer};
use tower_http::trace::{
    DefaultMakeSpan, DefaultOnFailure, DefaultOnRequest, DefaultOnResponse, TraceLayer,
};

pub use auth::extract_client_ip;
pub use error::{map_api_error, AppError, AppResult};
pub use health::{create_health_router, create_metrics_router, liveness_check};
pub use middleware::{hsts_header, security_headers_middleware};
pub use websocket::{websocket_handler, AuthMethod};

pub(crate) fn reject_duplicate_header(headers: &HeaderMap, name: &HeaderName) -> AppResult<()> {
    let mut values = headers.get_all(name).iter();
    let _ = values.next();
    if values.next().is_some() {
        return Err(AppError::bad_request(format!(
            "Multiple {name} headers are not allowed"
        )));
    }
    Ok(())
}

pub(crate) fn required_header_str<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
    missing_message: &'static str,
) -> AppResult<&'a str> {
    let header_name = HeaderName::from_static(name);
    reject_duplicate_header(headers, &header_name)?;
    let value = headers
        .get(&header_name)
        .ok_or_else(|| AppError::bad_request(missing_message))
        .and_then(|value| {
            value
                .to_str()
                .map_err(|_| AppError::bad_request(format!("Invalid {name} header")))
        })?;
    if value.trim().is_empty() {
        return Err(AppError::bad_request(missing_message));
    }
    Ok(value)
}

pub(crate) fn optional_header_str<'a>(
    headers: &'a HeaderMap,
    name: &'static HeaderName,
) -> AppResult<Option<&'a str>> {
    reject_duplicate_header(headers, name)?;
    headers
        .get(name)
        .map(|value| {
            value
                .to_str()
                .map_err(|_| AppError::bad_request(format!("Invalid {name} header")))
        })
        .transpose()
}

pub(crate) fn optional_content_range(
    headers: &HeaderMap,
) -> AppResult<Option<synctv_core::models::FileUploadRange>> {
    let Some(value) = optional_header_str(headers, &axum::http::header::CONTENT_RANGE)? else {
        return Ok(None);
    };
    let value = value.trim();
    let Some(rest) = value.strip_prefix("bytes ") else {
        return Err(AppError::bad_request("Invalid Content-Range header"));
    };
    let (range, total) = rest
        .split_once('/')
        .ok_or_else(|| AppError::bad_request("Invalid Content-Range header"))?;
    let (start, end) = range
        .split_once('-')
        .ok_or_else(|| AppError::bad_request("Invalid Content-Range header"))?;
    let start = start
        .parse::<i64>()
        .map_err(|_| AppError::bad_request("Invalid Content-Range header"))?;
    let end_inclusive = end
        .parse::<i64>()
        .map_err(|_| AppError::bad_request("Invalid Content-Range header"))?;
    let total_size = total
        .parse::<i64>()
        .map_err(|_| AppError::bad_request("Invalid Content-Range header"))?;
    Ok(Some(synctv_core::models::FileUploadRange {
        start,
        end_inclusive,
        total_size,
    }))
}

pub(crate) fn optional_file_range(
    headers: &HeaderMap,
) -> AppResult<Option<synctv_core::models::FileRangeRequest>> {
    let Some(value) = optional_header_str(headers, &header::RANGE)? else {
        return Ok(None);
    };
    let value = value.trim();
    let Some(rest) = value.strip_prefix("bytes=") else {
        return Err(AppError::bad_request("Invalid Range header"));
    };
    if rest.contains(',') {
        return Err(AppError::bad_request(
            "Multiple byte ranges are unsupported",
        ));
    }
    let (start, end) = rest
        .split_once('-')
        .ok_or_else(|| AppError::bad_request("Invalid Range header"))?;
    match (start.trim(), end.trim()) {
        ("", "") => Err(AppError::bad_request("Invalid Range header")),
        ("", suffix) => {
            let length = suffix
                .parse::<u64>()
                .map_err(|_| AppError::bad_request("Invalid Range header"))?;
            if length == 0 {
                return Err(AppError::bad_request("Invalid Range header"));
            }
            Ok(Some(synctv_core::models::FileRangeRequest::Suffix {
                length,
            }))
        }
        (start, "") => {
            let start = start
                .parse::<u64>()
                .map_err(|_| AppError::bad_request("Invalid Range header"))?;
            Ok(Some(synctv_core::models::FileRangeRequest::From { start }))
        }
        (start, end) => {
            let start = start
                .parse::<u64>()
                .map_err(|_| AppError::bad_request("Invalid Range header"))?;
            let end_inclusive = end
                .parse::<u64>()
                .map_err(|_| AppError::bad_request("Invalid Range header"))?;
            if end_inclusive < start {
                return Err(AppError::bad_request("Invalid Range header"));
            }
            Ok(Some(synctv_core::models::FileRangeRequest::Exact(
                synctv_core::models::FileByteRange {
                    start,
                    end_inclusive,
                },
            )))
        }
    }
}

pub(crate) fn file_range_request_to_proto(
    range: synctv_core::models::FileRangeRequest,
) -> synctv_proto::client::FileRangeRequest {
    use synctv_proto::client::file_range_request::Range;

    let range = match range {
        synctv_core::models::FileRangeRequest::Exact(range) => {
            Range::Exact(synctv_proto::client::FileByteRange {
                start: range.start,
                end_inclusive: range.end_inclusive,
            })
        }
        synctv_core::models::FileRangeRequest::From { start } => Range::FromStart(start),
        synctv_core::models::FileRangeRequest::Suffix { length } => Range::SuffixLength(length),
    };
    synctv_proto::client::FileRangeRequest { range: Some(range) }
}

pub(crate) fn file_object_download_response(
    download: synctv_core::models::FileObjectDownload,
    cache_control: Option<&'static str>,
) -> AppResult<axum::response::Response> {
    let metadata = download.metadata;
    let body_stream = download.stream.map(|chunk| {
        chunk.map_err(|error| {
            let app_error = AppError::from(error);
            std::io::Error::other(app_error.message().to_string())
        })
    });
    let mut response = (StatusCode::OK, Body::from_stream(body_stream)).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&metadata.mime_type)
            .map_err(|_| AppError::internal_server_error("Invalid file content type"))?,
    );
    response
        .headers_mut()
        .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if let Ok(value) = HeaderValue::from_str(&metadata.size_bytes.to_string()) {
        response.headers_mut().insert(header::CONTENT_LENGTH, value);
    }
    if let Some(cache_control) = cache_control {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(cache_control),
        );
    }
    if let Ok(value) = HeaderValue::from_str(&metadata.content_manifest_sha256) {
        response.headers_mut().insert(
            HeaderName::from_static("x-synctv-content-manifest-sha256"),
            value,
        );
    }
    if let Some(range) = metadata.range {
        let start = range.start;
        let end = range.end_inclusive;
        let content_range = format!("bytes {start}-{end}/{}", metadata.total_size_bytes);
        *response.status_mut() = StatusCode::PARTIAL_CONTENT;
        if let Ok(value) = HeaderValue::from_str(&content_range) {
            response.headers_mut().insert(header::CONTENT_RANGE, value);
        }
    }
    Ok(response)
}

static X_FORWARDED_PROTO: LazyLock<HeaderName> =
    LazyLock::new(|| HeaderName::from_static("x-forwarded-proto"));

pub use synctv_api_common::app_state::{
    build_app_state, create_app_state_from_options, AppState, ProxyCacheLifecycleRuntime,
    RouterOptions,
};

pub fn create_router_from_options(options: RouterOptions) -> anyhow::Result<axum::Router> {
    let state = create_app_state_from_options(options)?;
    create_router_from_shared_state(&state)
}

pub fn create_router_from_shared_state(state: &AppState) -> anyhow::Result<axum::Router> {
    let state = state.clone();
    let router = register_all_routes();
    apply_global_layers(router, &state)
}

pub fn create_router_with_state_from_options(
    options: RouterOptions,
) -> anyhow::Result<(axum::Router, AppState)> {
    let state = create_app_state_from_options(options)?;
    let router = create_router_from_shared_state(&state)?;
    Ok((router, state))
}

pub fn start_proxy_cache_lifecycle(
    cache: &Arc<synctv_proxy::slice_cache::SliceCache>,
) -> ProxyCacheLifecycleRuntime {
    let manager = synctv_proxy::slice_cache::CacheLifecycleManager::new(
        cache.backend().clone(),
        cache.config().clone(),
    );
    let cancel = manager.cancellation_token();
    let handle = manager.start();
    ProxyCacheLifecycleRuntime { cancel, handle }
}

/// Body size limits for specific endpoint categories.
///
/// These are applied as `route_layer`s INSIDE the rate-limit route groups so that
/// the limit is enforced before the handler reads the body, and the global 10 MB
/// safety net remains as a fallback for routes not explicitly limited here.
pub(crate) mod body_limits {
    /// Auth endpoints: 64 KB.
    /// Typical auth JSON bodies are under 1 KB; 64 KB leaves room for OPAQUE
    /// and passkey payloads.
    pub const AUTH: usize = 64 * 1024;

    /// Room create / update / settings: 64 KB.
    pub const ROOM: usize = 64 * 1024;

    /// Media add / edit requests: 512 KB (media metadata may include longer URLs or
    /// subtitles, but should never be megabyte-scale).
    pub const MEDIA: usize = 512 * 1024;

    /// Chat attachment database uploads: match the service-level attachment cap.
    pub const CHAT_ATTACHMENT: usize = 50 * 1024 * 1024;

    /// User avatar database uploads: match the service-level avatar cap.
    pub const USER_AVATAR: usize = 5 * 1024 * 1024;

    /// Cover database uploads: match the service-level cover cap.
    pub const COVER: usize = 10 * 1024 * 1024;
}

/// Authentication routes that are mounted inside the strict rate-limit group.
/// Strict rate limiting: 5 req/min. Body limit: 64 KB.
fn register_auth_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/oauth2/exchange",
            post(oauth2::exchange_authorization_code),
        )
        .layer(axum::extract::DefaultBodyLimit::max(body_limits::AUTH))
}

fn register_extracted_auth_routes() -> Router<AppState> {
    Router::new()
        .route("/api/auth/email/confirm", post(auth::confirm_email_login))
        .route("/api/auth/guest-token", post(auth::create_guest_token))
        .route(
            "/api/auth/direct-password/register",
            post(auth::register_with_direct_password),
        )
        .route(
            "/api/auth/direct-password/login",
            post(auth::login_with_direct_password),
        )
        .route("/api/auth/login/start", post(auth::start_login))
        .route(
            "/api/auth/email/registration/request",
            post(auth::request_email_registration),
        )
        .route(
            "/api/auth/email/registration/confirm",
            post(auth::confirm_email_registration),
        )
        .route(
            "/api/auth/passkeys/registration/start",
            post(auth::start_passkey_registration),
        )
        .route(
            "/api/auth/passkeys/registration/finish",
            post(auth::finish_passkey_registration),
        )
        .route(
            "/api/auth/passkeys/login/start",
            post(auth::start_passkey_login),
        )
        .route(
            "/api/auth/passkeys/login/finish",
            post(auth::finish_passkey_login),
        )
        .route(
            "/api/auth/opaque/login/start",
            post(auth::start_opaque_login),
        )
        .route(
            "/api/auth/opaque/login/finish",
            post(auth::finish_opaque_login),
        )
        .route(
            "/api/auth/opaque/registration/start",
            post(auth::start_opaque_registration),
        )
        .route(
            "/api/auth/opaque/registration/finish",
            post(auth::finish_opaque_registration),
        )
        .route("/api/auth/email/request", post(auth::request_email_login))
        .route(
            "/api/auth/mfa/email/request",
            post(auth::request_mfa_email_code),
        )
        .route(
            "/api/auth/mfa/email/verify",
            post(auth::verify_mfa_email_code),
        )
        .route(
            "/api/auth/mfa/passkeys/start",
            post(auth::start_mfa_passkey),
        )
        .route(
            "/api/auth/mfa/passkeys/finish",
            post(auth::finish_mfa_passkey),
        )
        .route("/api/auth/mfa/totp/verify", post(auth::verify_mfa_totp))
        .route(
            "/api/auth/mfa/recovery-code/verify",
            post(auth::verify_mfa_recovery_code),
        )
        .route("/api/auth/refresh", post(auth::refresh_token))
        // Tighter body limit for authentication endpoints (64 KB)
        .layer(axum::extract::DefaultBodyLimit::max(body_limits::AUTH))
}

/// Media mutation routes (add, delete, reorder, edit, batch operations).
/// Moderate rate limiting: 20 req/min. Body limit: 512 KB.
fn register_media_routes() -> Router<AppState> {
    Router::new()
        .route("/api/rooms/{roomId}/media", post(room::add_media))
        .route(
            "/api/rooms/{roomId}/media",
            axum::routing::delete(room::clear_playlist),
        )
        .route(
            "/api/rooms/{roomId}/media/batch",
            post(room::push_media_batch),
        )
        .route("/api/rooms/{roomId}/media/move", post(room::move_media))
        .route(
            "/api/rooms/{roomId}/media/{mediaId}",
            axum::routing::delete(room::delete_media),
        )
        .route(
            "/api/rooms/{roomId}/media/{mediaId}",
            axum::routing::patch(room::edit_media),
        )
        .route(
            "/api/rooms/{roomId}/media/{mediaId}/cover/upload-session",
            post(room::create_media_cover_upload_session),
        )
        .route(
            "/api/rooms/{roomId}/media/{mediaId}/cover",
            axum::routing::put(room::update_media_cover).delete(room::clear_media_cover),
        )
        .route(
            "/api/rooms/{roomId}/media/{mediaId}/thumbnail/upload-session",
            post(room::create_media_thumbnail_upload_session),
        )
        .route(
            "/api/rooms/{roomId}/media/{mediaId}/thumbnail",
            axum::routing::put(room::update_media_thumbnail).delete(room::clear_media_thumbnail),
        )
        .route(
            "/api/rooms/{roomId}/cover/upload-session",
            post(room::create_room_cover_upload_session),
        )
        .route(
            "/api/rooms/{roomId}/cover",
            axum::routing::put(room::update_room_cover).delete(room::clear_room_cover),
        )
        .route(
            "/api/rooms/{roomId}/playlists/{playlistId}/cover/upload-session",
            post(room::create_playlist_cover_upload_session),
        )
        .route(
            "/api/rooms/{roomId}/playlists/{playlistId}/cover",
            axum::routing::put(room::update_playlist_cover).delete(room::clear_playlist_cover),
        )
        // Media metadata bodies are small (URLs, titles, subtitles)
        .layer(axum::extract::DefaultBodyLimit::max(body_limits::MEDIA))
}

/// Write routes (room CRUD, membership, playback control, playlists, user updates).
/// Moderate rate limiting: 30 req/min. Room create/update body limit: 64 KB.
fn register_write_routes() -> Router<AppState> {
    let router = Router::new()
        .route("/api/rooms", post(room::create_room))
        .route(
            "/api/rooms/{roomId}",
            axum::routing::delete(room::delete_room),
        )
        .route(
            "/api/rooms/{roomId}/members/@me",
            axum::routing::put(room::join_room),
        )
        .route(
            "/api/rooms/{roomId}/password/opaque/login/start",
            post(room::start_room_password_login),
        )
        .route(
            "/api/rooms/{roomId}/password/opaque/login/finish",
            post(room::finish_room_password_login),
        )
        .route(
            "/api/rooms/{roomId}/members/@me",
            axum::routing::delete(room::leave_room),
        )
        .route(
            "/api/rooms/{roomId}/settings",
            axum::routing::patch(room::update_room_settings),
        )
        .route(
            "/api/rooms/{roomId}/owner",
            post(room::transfer_room_ownership),
        )
        .route(
            "/api/rooms/{roomId}/password/opaque/registration/start",
            axum::routing::patch(room::start_room_password_registration),
        )
        .route(
            "/api/rooms/{roomId}/password/opaque/registration/finish",
            axum::routing::patch(room::finish_room_password_registration),
        )
        .route(
            "/api/rooms/{roomId}/password",
            axum::routing::delete(room::clear_room_password),
        )
        .route(
            "/api/rooms/{roomId}/playback/start",
            post(room::start_playback),
        )
        .route(
            "/api/rooms/{roomId}/playback/stop",
            post(room::stop_playback),
        )
        .route("/api/rooms/{roomId}/playback/next", post(room::play_next))
        .route(
            "/api/rooms/{roomId}/playback/previous",
            post(room::play_previous),
        )
        .route(
            "/api/rooms/{roomId}/playback/history/{entryId}/play",
            post(room::play_history_entry),
        )
        .route(
            "/api/rooms/{roomId}/playback",
            axum::routing::patch(room::update_playback_state),
        )
        .route("/api/rooms/{roomId}/playlists", post(room::create_playlist))
        .route(
            "/api/rooms/{roomId}/playlists/{playlistId}",
            axum::routing::patch(room::update_playlist),
        )
        .route(
            "/api/rooms/{roomId}/playlists/{playlistId}/move",
            post(room::move_playlist),
        )
        .route(
            "/api/rooms/{roomId}/playlists/{playlistId}",
            axum::routing::delete(room::delete_playlist),
        )
        .route(
            "/api/rooms/{roomId}/entries",
            axum::routing::delete(room::delete_entries),
        )
        .route(
            "/api/rooms/{roomId}/streams/{mediaId}/kick",
            post(room::kick_room_stream),
        )
        .route(
            "/api/rooms/{roomId}/settings/reset",
            post(room::reset_room_settings),
        )
        .route(
            "/api/rooms/{roomId}/chat/messages",
            post(room::send_chat_message),
        )
        .route(
            "/api/rooms/{roomId}/chat/attachments/upload-session",
            post(room::create_chat_attachment_upload_session),
        )
        .route(
            "/api/rooms/{roomId}/chat/messages/{messageId}",
            axum::routing::patch(room::edit_chat_message),
        )
        .route(
            "/api/rooms/{roomId}/chat/messages/{messageId}",
            axum::routing::delete(room::delete_chat_message),
        )
        .route(
            "/api/rooms/{roomId}/chat/messages/{messageId}/pin",
            axum::routing::put(room::pin_chat_message).delete(room::unpin_chat_message),
        )
        .route(
            "/api/rooms/{roomId}/chat/messages/{messageId}/reactions/{reactionKey}",
            axum::routing::put(room::set_chat_reaction).delete(room::clear_chat_reaction),
        )
        .route(
            "/api/rooms/{roomId}/chat/read-state",
            post(room::mark_chat_read),
        );

    router
        // Room/user write bodies should be small (room metadata, settings, passwords)
        .layer(axum::extract::DefaultBodyLimit::max(body_limits::ROOM))
}

/// Read routes (user info, room discovery, room details, playlists, chat, media, playback).
/// Rate limited: 100 req/min.
fn register_read_routes() -> Router<AppState> {
    Router::new()
        .route("/api/rooms/discover", get(room::discover_rooms))
        .route("/api/rooms/categories", get(room::list_room_categories))
        .route("/api/rooms/labels", get(room::list_room_labels))
        .route(
            "/api/rooms/{roomId}/discovery",
            get(room::get_room_discovery),
        )
        .route("/api/rooms/{roomId}", get(room::get_room))
        .route("/api/rooms/{roomId}/settings", get(room::get_room_settings))
        .route("/api/rooms/{roomId}/members", get(room::get_room_members))
        .route("/api/rooms/{roomId}/streams", get(room::list_room_streams))
        .route(
            "/api/rooms/{roomId}/streams/{mediaId}",
            get(room::get_room_stream_info),
        )
        .route(
            "/api/rooms/{roomId}/chat/history",
            get(room::get_chat_history),
        )
        .route(
            "/api/rooms/{roomId}/chat/search",
            get(room::search_chat_messages),
        )
        .route(
            "/api/rooms/{roomId}/chat/playback-messages",
            get(room::get_chat_playback_messages),
        )
        .route(
            "/api/rooms/{roomId}/chat/pinned-messages",
            get(room::list_pinned_chat_messages),
        )
        .route(
            "/api/rooms/{roomId}/chat/messages/{messageId}",
            get(room::get_chat_message),
        )
        .route(
            "/api/rooms/{roomId}/chat/messages/{messageId}/context",
            get(room::get_chat_message_context),
        )
        .route(
            "/api/rooms/{roomId}/chat/messages/{messageId}/reactions/{reactionKey}/users",
            get(room::list_chat_reaction_users),
        )
        .route(
            "/api/rooms/{roomId}/chat/messages/{messageId}/read-receipts",
            get(room::get_chat_message_read_receipts),
        )
        .route(
            "/api/rooms/{roomId}/chat/read-state",
            get(room::get_chat_read_state),
        )
        // Playlist and Media APIs
        .route("/api/rooms/{roomId}/playlists", get(room::list_playlists))
        .route(
            "/api/rooms/{roomId}/playlists/{playlistId}",
            get(room::get_playlist),
        )
        .route(
            "/api/rooms/{roomId}/media/list",
            post(room::list_playlist_items),
        )
        .route("/api/rooms/{roomId}/media/{mediaId}", get(room::get_media))
        .route("/api/rooms/{roomId}/playback", get(room::get_playback))
        .route(
            "/api/rooms/{roomId}/playback/history",
            get(room::list_playback_history),
        )
        .route(
            "/api/rooms/{roomId}/watch/playback-state",
            get(room::watch_playback_state),
        )
        .route(
            "/api/rooms/{roomId}/watch/playback",
            get(room::watch_playback),
        )
        .route(
            "/api/rooms/{roomId}/media/{mediaId}/danmaku/bilibili-live",
            get(room::watch_bilibili_live_danmaku),
        )
        .route(
            "/api/rooms/{roomId}/watch/room-settings",
            get(room::watch_room_settings),
        )
        .route(
            "/api/rooms/{roomId}/watch/playlist-items",
            get(room::watch_playlist_items),
        )
        .route(
            "/api/rooms/{roomId}/watch/room-members",
            get(room::watch_room_members),
        )
        .route(
            "/api/rooms/{roomId}/watch/chat-events",
            get(room::watch_chat_events),
        )
        .route(
            "/api/rooms/{roomId}/watch/chat-pin-events",
            get(room::watch_chat_pin_events),
        )
}

fn register_chat_attachment_object_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/chat/attachment-objects/{encodedObjectKey}/complete",
            post(room::complete_chat_attachment_upload_session),
        )
        .route(
            "/api/chat/attachment-objects/{encodedObjectKey}",
            axum::routing::put(room::upload_chat_attachment_object)
                .get(room::get_chat_attachment_object),
        )
        .layer(axum::extract::DefaultBodyLimit::max(
            body_limits::CHAT_ATTACHMENT,
        ))
}

fn register_media_cover_object_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/media/cover-objects/{encodedObjectKey}/complete",
            post(room::complete_media_cover_upload_session),
        )
        .route(
            "/api/media/cover-objects/{encodedObjectKey}",
            axum::routing::put(room::upload_media_cover_object).get(room::get_media_cover_object),
        )
        .layer(axum::extract::DefaultBodyLimit::max(body_limits::COVER))
}

fn register_media_thumbnail_object_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/media/thumbnail-objects/{encodedObjectKey}/complete",
            post(room::complete_media_thumbnail_upload_session),
        )
        .route(
            "/api/media/thumbnail-objects/{encodedObjectKey}",
            axum::routing::put(room::upload_media_thumbnail_object)
                .get(room::get_media_thumbnail_object),
        )
        .layer(axum::extract::DefaultBodyLimit::max(body_limits::COVER))
}

fn register_room_cover_object_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/room/cover-objects/{encodedObjectKey}/complete",
            post(room::complete_room_cover_upload_session),
        )
        .route(
            "/api/room/cover-objects/{encodedObjectKey}",
            axum::routing::put(room::upload_room_cover_object).get(room::get_room_cover_object),
        )
        .layer(axum::extract::DefaultBodyLimit::max(body_limits::COVER))
}

fn register_playlist_cover_object_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/playlist/cover-objects/{encodedObjectKey}/complete",
            post(room::complete_playlist_cover_upload_session),
        )
        .route(
            "/api/playlist/cover-objects/{encodedObjectKey}",
            axum::routing::put(room::upload_playlist_cover_object)
                .get(room::get_playlist_cover_object),
        )
        .layer(axum::extract::DefaultBodyLimit::max(body_limits::COVER))
}

fn register_extracted_user_routes() -> Router<AppState> {
    Router::new()
        .route("/api/user", get(user::get_me))
        .route("/api/user/rooms/discover", get(user::discover_rooms))
        .route(
            "/api/user/rooms/{roomId}/discovery",
            get(user::get_room_discovery),
        )
        .route("/api/user/rooms", get(user::list_my_rooms))
        .route("/api/user/favorite-rooms", get(user::list_favorite_rooms))
        .route(
            "/api/user/rooms/{roomId}/favorite",
            axum::routing::put(user::favorite_room).delete(user::unfavorite_room),
        )
        .route("/api/user", axum::routing::patch(user::update_user))
        .route(
            "/api/user/avatar/upload-session",
            post(user::create_user_avatar_upload_session),
        )
        .route(
            "/api/user/avatar",
            axum::routing::put(user::update_user_avatar).delete(user::clear_user_avatar),
        )
        .route("/api/user/email/bind/start", post(user::start_email_bind))
        .route(
            "/api/user/email/bind/confirm",
            post(user::confirm_email_bind),
        )
        .route("/api/user/email/unbind", post(user::unbind_email))
        .route(
            "/api/user/sensitive-verification/start",
            post(user::start_sensitive_operation_verification),
        )
        .route(
            "/api/user/sensitive-verification/passkey/start",
            post(user::start_sensitive_operation_passkey),
        )
        .route(
            "/api/user/sensitive-verification/email/request",
            post(user::request_sensitive_operation_email_code),
        )
        .route(
            "/api/user/sensitive-verification/finish",
            post(user::finish_sensitive_operation_verification),
        )
        .route(
            "/api/user/preferences",
            get(user::get_user_preferences).patch(user::update_user_preferences),
        )
        .route(
            "/api/user/two-factor",
            axum::routing::put(user::set_two_factor_enabled),
        )
        .route("/api/user/passkeys", get(user::list_passkeys))
        .route(
            "/api/user/passkeys/bind/start",
            post(user::start_passkey_bind),
        )
        .route(
            "/api/user/passkeys/bind/finish",
            post(user::finish_passkey_bind),
        )
        .route(
            "/api/user/opaque-password/update/start",
            post(user::start_opaque_password_update),
        )
        .route(
            "/api/user/opaque-password/update/finish",
            post(user::finish_opaque_password_update),
        )
        .route(
            "/api/user/passkeys/{credentialId}",
            axum::routing::delete(user::delete_passkey),
        )
        .route("/api/user/totp/setup/start", post(user::start_totp_setup))
        .route("/api/user/totp/setup/finish", post(user::finish_totp_setup))
        .route(
            "/api/user/totp/recovery-codes/regenerate",
            post(user::regenerate_totp_recovery_codes),
        )
        .route("/api/user/totp", axum::routing::delete(user::delete_totp))
        .route("/api/user/account-closure", post(user::close_account))
        .route("/api/user/logout", post(auth::logout))
}

fn register_user_avatar_object_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/user/avatar-objects/{encodedObjectKey}/complete",
            post(user::complete_user_avatar_upload_session),
        )
        .route(
            "/api/user/avatar-objects/{encodedObjectKey}",
            axum::routing::put(user::upload_user_avatar_object).get(user::get_user_avatar_object),
        )
        .layer(axum::extract::DefaultBodyLimit::max(
            body_limits::USER_AVATAR,
        ))
}

/// Assemble all route groups into a single router.
fn register_websocket_routes() -> Router<AppState> {
    Router::new().route(
        "/ws/rooms/{roomId}",
        axum::routing::get(websocket::websocket_handler),
    )
}

fn register_all_routes() -> Router<AppState> {
    let mut router = Router::new()
        .route("/", get(redirect_to_project))
        .route(
            "/.well-known/apple-app-site-association",
            get(native_app_association::apple_app_site_association),
        )
        .route(
            "/.well-known/assetlinks.json",
            get(native_app_association::android_asset_links),
        )
        .merge(openapi_router())
        .merge(public::create_public_router())
        .merge(register_extracted_auth_routes())
        .merge(register_auth_routes())
        .merge(register_extracted_user_routes())
        .merge(register_user_avatar_object_routes())
        .merge(
            Router::new()
                .route("/api/rooms/{roomId}/members", post(room_extra::add_member))
                .route(
                    "/api/rooms/{roomId}/reviews/joins",
                    get(room_extra::list_room_join_reviews),
                )
                .route(
                    "/api/rooms/{roomId}/reviews/joins/{requestId}/approve",
                    post(room_extra::approve_room_join_review),
                )
                .route(
                    "/api/rooms/{roomId}/reviews/joins/{requestId}/reject",
                    post(room_extra::reject_room_join_review),
                )
                .route(
                    "/api/rooms/{roomId}/members/{userId}",
                    axum::routing::delete(room_extra::kick_member),
                )
                .route(
                    "/api/rooms/{roomId}/members/{userId}",
                    axum::routing::patch(room_extra::set_member_permissions),
                )
                .route(
                    "/api/rooms/{roomId}/members/{userId}/remark-name",
                    axum::routing::patch(room_extra::update_member_remark_name),
                )
                .route(
                    "/api/rooms/{roomId}/members/{userId}/display-tag",
                    axum::routing::patch(room_extra::update_member_display_tag),
                )
                .route(
                    "/api/rooms/{roomId}/reports",
                    get(room::list_room_content_reports).post(room::report_content),
                )
                .route(
                    "/api/rooms/{roomId}/reports/{reportId}",
                    get(room::get_room_content_report),
                )
                .route(
                    "/api/rooms/{roomId}/reports/{reportId}/status",
                    post(room::update_room_content_report_status),
                ),
        )
        .merge(Router::new().route("/api/tickets", post(ticket::create_ticket)))
        .merge(
            Router::new()
                .route(
                    "/api/oauth2/{provider}/bind",
                    get(oauth2::get_bind_authorize_url),
                )
                .route(
                    "/api/oauth2/type/{provider}/unlink",
                    axum::routing::delete(oauth2::unlink_provider),
                )
                .route("/api/oauth2/linked", get(oauth2::get_linked_providers)),
        )
        .merge(register_media_routes())
        .merge(register_write_routes())
        .merge(register_read_routes())
        .merge(register_chat_attachment_object_routes())
        .merge(register_media_cover_object_routes())
        .merge(register_media_thumbnail_object_routes())
        .merge(register_room_cover_object_routes())
        .merge(register_playlist_cover_object_routes())
        // WebRTC configuration endpoints
        .merge(Router::new().route(
            "/api/rooms/{roomId}/webrtc/ice-servers",
            get(webrtc::get_ice_servers),
        ))
        // Admin routes
        .merge(Router::new().nest("/api/admin", admin::create_admin_router()))
        // Provider routes
        .merge(
            Router::new()
                .merge(register_provider_management_routes())
                .merge(Router::new().nest(
                    "/api/providers",
                    providers::common::register_common_routes(),
                )),
        )
        .route(
            "/api/playback-providers/bilibili/live-danmaku/{mediaId}",
            get(crate::providers::playback_provider::bilibili::watch_bilibili_live_danmaku)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/bilibili/{version}/media-streams/{modeName}/{urlIndex}",
            get(crate::providers::playback_provider::bilibili::get_bilibili_media_stream)
                .head(crate::providers::playback_provider::bilibili::head_bilibili_media_stream)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/bilibili/{version}/hls-manifests/{modeName}/{urlIndex}",
            get(crate::providers::playback_provider::bilibili::get_bilibili_hls_manifest)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/bilibili/{version}/hls-resources/{modeName}/{mediaIndex}/{resourceKind}",
            get(crate::providers::playback_provider::bilibili::get_bilibili_hls_resource)
                .head(crate::providers::playback_provider::bilibili::head_bilibili_hls_resource)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/bilibili/{version}/dash-manifests/{modeName}",
            get(crate::providers::playback_provider::bilibili::get_bilibili_dash_manifest)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/bilibili/{version}/dash-manifests/{modeName}/{manifestMode}",
            get(crate::providers::playback_provider::bilibili::get_bilibili_dash_manifest)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/bilibili/{version}/dash-resources/{modeName}/{resourceKind}/{scope}/{uid}/{rid}/{exp}/{sig}",
            get(crate::providers::playback_provider::bilibili::get_bilibili_dash_resource)
                .head(crate::providers::playback_provider::bilibili::head_bilibili_dash_resource)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/bilibili/{version}/dash-resources/{modeName}/{resourceKind}/{scope}/{uid}/{rid}/{exp}/{sig}/{*resourcePath}",
            get(crate::providers::playback_provider::bilibili::get_bilibili_dash_resource)
                .head(crate::providers::playback_provider::bilibili::head_bilibili_dash_resource)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/bilibili/{version}/subtitles/{modeName}/{subtitleIndex}",
            get(crate::providers::playback_provider::bilibili::get_bilibili_subtitle)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/bilibili/{version}/danmaku-files/{danmakuIndex}",
            get(crate::providers::playback_provider::bilibili::get_bilibili_danmaku_file)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/direct-url/{version}/streams/{modeName}/{urlIndex}",
            get(crate::providers::playback_provider::direct_url::get_direct_url_stream)
                .head(crate::providers::playback_provider::direct_url::head_direct_url_stream)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/direct-url/{version}/hls-manifests/{modeName}/{urlIndex}",
            get(crate::providers::playback_provider::direct_url::get_direct_url_hls_manifest)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/direct-url/{version}/hls-resources/{modeName}/{urlIndex}/{resourceKind}",
            get(crate::providers::playback_provider::direct_url::get_direct_url_hls_resource)
                .head(crate::providers::playback_provider::direct_url::head_direct_url_hls_resource)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/direct-url/{version}/dash-manifests/{modeName}/{urlIndex}",
            get(crate::providers::playback_provider::direct_url::get_direct_url_dash_manifest)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/direct-url/{version}/dash-resources/{modeName}/{urlIndex}/{resourceKind}/{scope}/{uid}/{rid}/{exp}/{sig}",
            get(crate::providers::playback_provider::direct_url::get_direct_url_dash_resource)
                .head(crate::providers::playback_provider::direct_url::head_direct_url_dash_resource)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/direct-url/{version}/dash-resources/{modeName}/{urlIndex}/{resourceKind}/{scope}/{uid}/{rid}/{exp}/{sig}/{*resourcePath}",
            get(crate::providers::playback_provider::direct_url::get_direct_url_dash_resource)
                .head(crate::providers::playback_provider::direct_url::head_direct_url_dash_resource)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/direct-url/{version}/subtitles/{modeName}/{subtitleIndex}",
            get(crate::providers::playback_provider::direct_url::get_direct_url_subtitle)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/twitch/{version}/resources/{modeName}/{mediaIndex}",
            get(crate::providers::playback_provider::twitch::get_twitch_resource)
                .head(crate::providers::playback_provider::twitch::head_twitch_resource)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/twitch/{version}/segments",
            get(crate::providers::playback_provider::twitch::get_twitch_segment)
                .head(crate::providers::playback_provider::twitch::head_twitch_segment)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/twitch/{version}/chats/{modeName}/{mediaIndex}",
            get(crate::providers::playback_provider::twitch::watch_twitch_chat)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/youtube/{version}/resources/{modeName}/{mediaIndex}",
            get(crate::providers::playback_provider::youtube::get_youtube_resource)
                .head(crate::providers::playback_provider::youtube::head_youtube_resource)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/youtube/{version}/segments",
            get(crate::providers::playback_provider::youtube::get_youtube_segment)
                .head(crate::providers::playback_provider::youtube::head_youtube_segment)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/youtube/{version}/subtitles/{modeName}/{subtitleIndex}",
            get(crate::providers::playback_provider::youtube::get_youtube_subtitle)
                .head(crate::providers::playback_provider::youtube::head_youtube_subtitle)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/huya/{version}/resources/{modeName}/{mediaIndex}",
            get(crate::providers::playback_provider::huya::get_huya_resource)
                .head(crate::providers::playback_provider::huya::head_huya_resource)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/huya/{version}/segments",
            get(crate::providers::playback_provider::huya::get_huya_segment)
                .head(crate::providers::playback_provider::huya::head_huya_segment)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/huya/{version}/segments.ts",
            get(crate::providers::playback_provider::huya::get_huya_segment)
                .head(crate::providers::playback_provider::huya::head_huya_segment)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/huya/{version}/danmakus/{modeName}/{mediaIndex}",
            get(crate::providers::playback_provider::huya::watch_huya_danmaku)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/douyu/{version}/resources/{modeName}/{mediaIndex}",
            get(crate::providers::playback_provider::douyu::get_douyu_resource)
                .head(crate::providers::playback_provider::douyu::head_douyu_resource)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/douyin/{version}/resources/{modeName}/{mediaIndex}",
            get(crate::providers::playback_provider::douyin::get_resource)
                .head(crate::providers::playback_provider::douyin::head_resource)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/douyin/{version}/segments",
            get(crate::providers::playback_provider::douyin::get_segment)
                .head(crate::providers::playback_provider::douyin::head_segment)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/douyin/{version}/danmakus/{modeName}/{mediaIndex}",
            get(crate::providers::playback_provider::douyin::watch_danmaku)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/tiktok/{version}/resources/{modeName}/{mediaIndex}",
            get(crate::providers::playback_provider::tiktok::get_tiktok_resource)
                .head(crate::providers::playback_provider::tiktok::head_tiktok_resource)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/tiktok/{version}/segments",
            get(crate::providers::playback_provider::tiktok::get_tiktok_segment)
                .head(crate::providers::playback_provider::tiktok::head_tiktok_segment)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/tiktok/{version}/subtitles/{modeName}/{subtitleIndex}",
            get(crate::providers::playback_provider::tiktok::get_tiktok_subtitle)
                .head(crate::providers::playback_provider::tiktok::head_tiktok_subtitle)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/douyu/{version}/segments",
            get(crate::providers::playback_provider::douyu::get_douyu_segment)
                .head(crate::providers::playback_provider::douyu::head_douyu_segment)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/douyu/{version}/danmakus/{modeName}/{mediaIndex}",
            get(crate::providers::playback_provider::douyu::watch_douyu_danmaku)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/acfun/{version}/resources/{modeName}/{mediaIndex}",
            get(crate::providers::playback_provider::acfun::get_acfun_resource)
                .head(crate::providers::playback_provider::acfun::head_acfun_resource)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/acfun/{version}/segments.ts",
            get(crate::providers::playback_provider::acfun::get_acfun_segment)
                .head(crate::providers::playback_provider::acfun::head_acfun_segment)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/acfun/{version}/danmaku-files/{modeName}/{mediaIndex}",
            get(crate::providers::playback_provider::acfun::get_acfun_danmaku_file)
                .head(crate::providers::playback_provider::acfun::head_acfun_danmaku_file)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/acfun/{version}/danmakus/{modeName}/{mediaIndex}",
            get(crate::providers::playback_provider::acfun::watch_acfun_danmaku)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/cctv/{version}/resources/{modeName}/{mediaIndex}",
            get(crate::providers::playback_provider::cctv::get_cctv_resource)
                .head(crate::providers::playback_provider::cctv::head_cctv_resource)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/cctv/{version}/segments",
            get(crate::providers::playback_provider::cctv::get_cctv_segment)
                .head(crate::providers::playback_provider::cctv::head_cctv_segment)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/fnos/{version}/resources/{modeName}/{mediaIndex}",
            get(crate::providers::playback_provider::fnos::get_fnos_resource)
                .head(crate::providers::playback_provider::fnos::head_fnos_resource)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/fnos/{version}/segments",
            get(crate::providers::playback_provider::fnos::get_fnos_segment)
                .head(crate::providers::playback_provider::fnos::head_fnos_segment)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/fnos/{version}/subtitles/{modeName}/{subtitleIndex}",
            get(crate::providers::playback_provider::fnos::get_fnos_subtitle)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/fnos/{version}/thumbnail",
            get(crate::providers::playback_provider::fnos::get_fnos_thumbnail)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/qnap/{version}/resources/{modeName}/{mediaIndex}",
            get(crate::providers::playback_provider::qnap::get_qnap_resource)
                .head(crate::providers::playback_provider::qnap::head_qnap_resource)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/qnap/{version}/subtitles/{modeName}/{subtitleIndex}",
            get(crate::providers::playback_provider::qnap::get_qnap_subtitle)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/qnap/{version}/thumbnail",
            get(crate::providers::playback_provider::qnap::get_qnap_thumbnail)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/synology/{version}/resources/{modeName}/{mediaIndex}",
            get(crate::providers::playback_provider::synology::get_synology_resource)
                .head(crate::providers::playback_provider::synology::head_synology_resource)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/nextcloud/{version}/resources/{modeName}/{mediaIndex}",
            get(crate::providers::playback_provider::nextcloud::get_nextcloud_resource)
                .head(crate::providers::playback_provider::nextcloud::head_nextcloud_resource)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/nextcloud/{version}/subtitles/{modeName}/{subtitleIndex}",
            get(crate::providers::playback_provider::nextcloud::get_nextcloud_subtitle)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/seafile/{version}/resources/{modeName}/{mediaIndex}",
            get(crate::providers::playback_provider::seafile::get_seafile_resource)
                .head(crate::providers::playback_provider::seafile::head_seafile_resource)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/seafile/{version}/subtitles/{modeName}/{subtitleIndex}",
            get(crate::providers::playback_provider::seafile::get_seafile_subtitle)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/truenas/{version}/resources/{modeName}/{mediaIndex}",
            get(crate::providers::playback_provider::truenas::get_truenas_resource)
                .head(crate::providers::playback_provider::truenas::head_truenas_resource)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/truenas/{version}/subtitles/{modeName}/{subtitleIndex}",
            get(crate::providers::playback_provider::truenas::get_truenas_subtitle)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/synology/{version}/subtitles/{modeName}/{subtitleIndex}",
            get(crate::providers::playback_provider::synology::get_synology_subtitle)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/synology/{version}/segments",
            get(crate::providers::playback_provider::synology::get_synology_segment)
                .head(crate::providers::playback_provider::synology::head_synology_segment)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/alist/{version}/files/{modeName}/{urlIndex}",
            get(crate::providers::playback_provider::alist::get_alist_file_stream)
                .head(crate::providers::playback_provider::alist::head_alist_file_stream)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/alist/{version}/transcoded-hls-manifests/{modeName}/{urlIndex}",
            get(crate::providers::playback_provider::alist::get_alist_transcoded_hls_manifest)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/alist/{version}/transcoded-hls-resources/{modeName}/{mediaIndex}/{resourceKind}",
            get(crate::providers::playback_provider::alist::get_alist_transcoded_hls_resource)
                .head(crate::providers::playback_provider::alist::head_alist_transcoded_hls_resource)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/alist/{version}/subtitles/{modeName}/{subtitleIndex}",
            get(crate::providers::playback_provider::alist::get_alist_subtitle)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/alist/{version}/thumbnail",
            get(crate::providers::playback_provider::alist::get_alist_thumbnail)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/emby/{version}/media-streams/{modeName}/{urlIndex}",
            get(crate::providers::playback_provider::emby::get_emby_media_stream)
                .head(crate::providers::playback_provider::emby::head_emby_media_stream)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/emby/{version}/hls-manifests/{modeName}/{urlIndex}",
            get(crate::providers::playback_provider::emby::get_emby_hls_manifest)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/emby/{version}/hls-resources/{modeName}/{mediaIndex}/{resourceKind}",
            get(crate::providers::playback_provider::emby::get_emby_hls_resource)
                .head(crate::providers::playback_provider::emby::head_emby_hls_resource)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/emby/{version}/subtitles/{modeName}/{subtitleIndex}",
            get(crate::providers::playback_provider::emby::get_emby_subtitle)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/rtmp/{version}/flv-stream",
            get(crate::providers::playback_provider::rtmp::get_rtmp_flv_stream)
                .head(crate::providers::playback_provider::rtmp::head_rtmp_flv_stream)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/rtmp/{version}/hls-master",
            get(crate::providers::playback_provider::rtmp::get_rtmp_hls_master)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/rtmp/{version}/hls/{generationId}/index.m3u8",
            get(crate::providers::playback_provider::rtmp::get_rtmp_hls_playlist)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/rtmp/{version}/hls/{generationId}/{segmentName}",
            get(crate::providers::playback_provider::rtmp::get_rtmp_hls_segment)
                .head(crate::providers::playback_provider::rtmp::head_rtmp_hls_segment)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/live-proxy/{version}/flv-stream",
            get(crate::providers::playback_provider::live_proxy::get_live_proxy_flv_stream)
                .head(crate::providers::playback_provider::live_proxy::head_live_proxy_flv_stream)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/live-proxy/{version}/hls-master",
            get(crate::providers::playback_provider::live_proxy::get_live_proxy_hls_master)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/live-proxy/{version}/hls/{generationId}/index.m3u8",
            get(crate::providers::playback_provider::live_proxy::get_live_proxy_hls_playlist)
                .options(providers::playback_provider_options_preflight),
        )
        .route(
            "/api/playback-providers/live-proxy/{version}/hls/{generationId}/{segmentName}",
            get(crate::providers::playback_provider::live_proxy::get_live_proxy_hls_segment)
                .head(crate::providers::playback_provider::live_proxy::head_live_proxy_hls_segment)
                .options(providers::playback_provider_options_preflight),
        )
        .merge(register_websocket_routes());

    router = router
        .merge(notifications::create_notification_read_router())
        .merge(notifications::create_notification_write_router());

    let email_routes = email::create_email_router();
    router = router.merge(email_routes);

    router = router.merge(
        Router::new()
            .route(
                "/api/oauth2/{provider}/authorize",
                get(oauth2::get_authorize_url),
            )
            .route(
                "/api/oauth2/providers",
                get(oauth2::list_available_providers),
            ),
    );

    router =
        router.merge(Router::new().nest("/api/providers/rtmp", providers::rtmp::rtmp_routes()));

    router
}

async fn redirect_to_project(State(state): State<AppState>) -> Redirect {
    Redirect::temporary(&state.runtime_settings.server.project_url)
}

#[cfg(feature = "openapi")]
fn openapi_router() -> Router<AppState> {
    crate::openapi::router()
}

#[cfg(not(feature = "openapi"))]
fn openapi_router() -> Router<AppState> {
    Router::new()
}

fn register_provider_management_routes() -> Router<AppState> {
    Router::new()
        .merge(
            Router::new()
                .nest(
                    "/api/providers/bilibili",
                    providers::bilibili::bilibili_auth_routes(),
                )
                .nest(
                    "/api/providers/alist",
                    providers::alist::alist_auth_routes(),
                )
                .nest(
                    "/api/providers/cloudreve",
                    providers::cloudreve::cloudreve_auth_routes(),
                )
                .nest(
                    "/api/providers/twitch",
                    providers::twitch::twitch_auth_routes(),
                )
                .nest(
                    "/api/providers/youtube",
                    providers::youtube::youtube_auth_routes(),
                )
                .nest(
                    "/api/providers/douyin",
                    providers::douyin::douyin_auth_routes(),
                )
                .nest(
                    "/api/providers/tiktok",
                    providers::tiktok::tiktok_auth_routes(),
                )
                .nest("/api/providers/fnos", providers::fnos::fnos_auth_routes())
                .nest("/api/providers/qnap", providers::qnap::qnap_auth_routes())
                .nest(
                    "/api/providers/synology",
                    providers::synology::synology_auth_routes(),
                )
                .nest(
                    "/api/providers/nextcloud",
                    providers::nextcloud::nextcloud_auth_routes(),
                )
                .nest(
                    "/api/providers/seafile",
                    providers::seafile::seafile_auth_routes(),
                )
                .nest(
                    "/api/providers/truenas",
                    providers::truenas::truenas_auth_routes(),
                )
                .nest("/api/providers/emby", providers::emby::emby_auth_routes()),
        )
        .merge(
            Router::new()
                .nest(
                    "/api/providers/bilibili",
                    providers::bilibili::bilibili_read_routes(),
                )
                .nest(
                    "/api/providers/alist",
                    providers::alist::alist_read_routes(),
                )
                .nest(
                    "/api/providers/cloudreve",
                    providers::cloudreve::cloudreve_read_routes(),
                )
                .nest(
                    "/api/providers/twitch",
                    providers::twitch::twitch_read_routes(),
                )
                .nest("/api/providers/huya", providers::huya::huya_routes())
                .nest("/api/providers/douyu", providers::douyu::douyu_routes())
                .nest("/api/providers/acfun", providers::acfun::acfun_routes())
                .nest("/api/providers/cctv", providers::cctv::cctv_routes())
                .nest(
                    "/api/providers/youtube",
                    providers::youtube::youtube_read_routes(),
                )
                .nest(
                    "/api/providers/douyin",
                    providers::douyin::douyin_read_routes(),
                )
                .nest(
                    "/api/providers/tiktok",
                    providers::tiktok::tiktok_read_routes(),
                )
                .nest("/api/providers/fnos", providers::fnos::fnos_read_routes())
                .nest("/api/providers/qnap", providers::qnap::qnap_read_routes())
                .nest(
                    "/api/providers/synology",
                    providers::synology::synology_read_routes(),
                )
                .nest(
                    "/api/providers/nextcloud",
                    providers::nextcloud::nextcloud_read_routes(),
                )
                .nest(
                    "/api/providers/seafile",
                    providers::seafile::seafile_read_routes(),
                )
                .nest(
                    "/api/providers/truenas",
                    providers::truenas::truenas_read_routes(),
                )
                .nest("/api/providers/emby", providers::emby::emby_read_routes()),
        )
}

/// Build CORS layer from API runtime settings.
fn build_cors_layer(
    runtime_settings: &synctv_api_common::ApiRuntimeSettings,
) -> anyhow::Result<CorsLayer> {
    if runtime_settings.server.cors_allowed_origins.is_empty() {
        tracing::warn!(
            "CORS policy: DENY ALL cross-origin requests (no origins configured). \
             Web frontends on different origins will fail to connect. \
             To fix, set server.cors_allowed_origins to your frontend URL(s): \
             SYNCTV_SERVER_CORS_ALLOWED_ORIGINS='[\"https://app.example.com\"]'"
        );
        Ok(CorsLayer::new())
    } else {
        let origins: Vec<HeaderValue> = runtime_settings
            .server
            .cors_allowed_origins
            .iter()
            .map(|origin| parse_configured_cors_origin(origin))
            .collect::<anyhow::Result<Vec<_>>>()?;
        tracing::info!(
            origins = ?origins,
            "CORS: Configured with {} allowed origin(s)",
            origins.len()
        );
        Ok(CorsLayer::new()
            .allow_origin(origins)
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers([
                axum::http::header::AUTHORIZATION,
                axum::http::header::CONTENT_TYPE,
                axum::http::header::ACCEPT,
                axum::http::header::CONTENT_RANGE,
                axum::http::header::RANGE,
                axum::http::HeaderName::from_static("x-request-id"),
                axum::http::HeaderName::from_static(synctv_core::service::FILE_UPLOAD_TOKEN_HEADER),
                axum::http::HeaderName::from_static("traceparent"),
                axum::http::HeaderName::from_static("tracestate"),
            ])
            .expose_headers([
                axum::http::HeaderName::from_static("x-request-id"),
                axum::http::header::ACCEPT_RANGES,
                axum::http::header::CONTENT_RANGE,
                axum::http::HeaderName::from_static("x-synctv-content-manifest-sha256"),
                axum::http::HeaderName::from_static("x-synctv-upload-complete"),
                axum::http::HeaderName::from_static("x-synctv-uploaded-size-bytes"),
                axum::http::HeaderName::from_static("x-synctv-uploaded-parts"),
            ])
            .vary([
                axum::http::header::ORIGIN,
                axum::http::header::ACCESS_CONTROL_REQUEST_METHOD,
                axum::http::header::ACCESS_CONTROL_REQUEST_HEADERS,
            ]))
    }
}

fn parse_configured_cors_origin(origin: &str) -> anyhow::Result<HeaderValue> {
    synctv_api_common::validate_cors_origin(origin)
        .map_err(|error| anyhow::anyhow!("invalid CORS origin configured: {error}"))?;

    HeaderValue::from_str(origin)
        .map_err(|_| anyhow::anyhow!("invalid CORS origin configured: `{origin}`"))
}

fn forwarded_proto_is_https(
    server: &synctv_api_common::ApiServerSettings,
    headers: &HeaderMap,
    remote_addr: Option<std::net::IpAddr>,
) -> AppResult<bool> {
    let Some(remote_addr) = remote_addr else {
        return Ok(false);
    };
    if !server.is_trusted_proxy(&remote_addr) {
        return Ok(false);
    }

    let Some(value) = optional_header_str(headers, &X_FORWARDED_PROTO)? else {
        return Ok(false);
    };

    Ok(value.eq_ignore_ascii_case("https"))
}

fn should_compress_json_response(
    _status: StatusCode,
    _version: axum::http::Version,
    headers: &HeaderMap,
    _extensions: &axum::http::Extensions,
) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .is_some_and(|media_type| {
            media_type.eq_ignore_ascii_case("application/json") || media_type.ends_with("+json")
        })
}

/// Apply shared transport layers (CORS, body limit, security headers, HSTS,
/// request ID propagation, and tracing) and bind state.
fn apply_shared_http_layers(
    router: Router<AppState>,
    cors: CorsLayer,
    server_config: synctv_api_common::ApiServerSettings,
    hsts_value: String,
) -> Router<AppState> {
    router
        .layer(cors)
        .layer(
            CompressionLayer::new()
                .br(true)
                .gzip(true)
                .zstd(true)
                .compress_when(DefaultPredicate::default().and(should_compress_json_response)),
        )
        .layer(axum::extract::DefaultBodyLimit::max(10 * 1024 * 1024))
        .layer(axum_middleware::from_fn(middleware::request_id_middleware))
        .layer(axum_middleware::from_fn(
            middleware::security_headers_middleware,
        ))
        .layer(axum_middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let hsts = hsts_value.clone();
                let server_config = server_config.clone();
                async move {
                    let remote_addr = request
                        .extensions()
                        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                        .map(|ci| ci.0.ip());
                    let forwarded_proto_https = match forwarded_proto_is_https(
                        &server_config,
                        request.headers(),
                        remote_addr,
                    ) {
                        Ok(value) => value,
                        Err(error) => return error.into_response(),
                    };

                    let mut response = next.run(request).await;
                    if forwarded_proto_https {
                        if let Ok(value) = axum::http::HeaderValue::from_str(&hsts) {
                            response
                                .headers_mut()
                                .insert(axum::http::header::STRICT_TRANSPORT_SECURITY, value);
                        }
                    } else {
                        response
                            .headers_mut()
                            .remove(axum::http::header::STRICT_TRANSPORT_SECURITY);
                    }
                    response
                }
            },
        ))
}

fn apply_global_layers(router: Router<AppState>, state: &AppState) -> anyhow::Result<axum::Router> {
    let cors = build_cors_layer(&state.runtime_settings)?;
    let server_config = state.runtime_settings.server.clone();
    let hsts_value = middleware::hsts_header(63_072_000, true, false);
    Ok(
        apply_shared_http_layers(router, cors, server_config, hsts_value)
            .layer(axum_middleware::from_fn(metrics_middleware::metrics_layer))
            .layer(OnEarlyDropLayer::new(EarlyDropsAsFailures::new(
                DefaultOnFailure::default(),
            )))
            .layer(
                TraceLayer::new_for_http()
                    .make_span_with(DefaultMakeSpan::new().level(tracing::Level::DEBUG))
                    .on_request(DefaultOnRequest::new().level(tracing::Level::DEBUG))
                    .on_response(DefaultOnResponse::new().level(tracing::Level::INFO)),
            )
            .with_state(state.clone()),
    )
}

#[cfg(test)]
mod tests;
