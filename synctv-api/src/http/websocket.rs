//! WebSocket handler with binary proto transmission
//!
//! This handler uses the unified `StreamMessage` trait from impls layer,
//! enabling full code reuse between gRPC and WebSocket.
//!
//! All business logic (rate limiting, content filtering, permissions, broadcasting)
//! is handled by `StreamMessageHandler.run()` with the `WebSocketStream` implementation.
//!
//! # Security Considerations
//!
//! Authentication can be provided via:
//! 1. Authorization header: `Authorization: Bearer <jwt>` (preferred, more secure)
//! 2. Query parameter: `?ticket=<ticket>` (recommended for browser clients, short-lived one-time-use)
//!
//! For browser clients, the ticket system is recommended:
//! - First call POST /api/tickets to get a short-lived ticket
//! - Then use `ws://host/ws/rooms/{room_id}?ticket=xxx`
//! - Tickets are single-use and expire quickly (30 seconds by default)

use axum::{
    extract::{ConnectInfo, FromRef, FromRequestParts, Path, Query, State, WebSocketUpgrade},
    http::request::Parts,
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
};
use futures::{SinkExt, StreamExt};
use std::convert::Infallible;
use std::future::Future;
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};
use std::time::Duration;
use tracing::{error, info, warn};

use crate::http::{AppError, AppState};
use crate::impls::messaging::{
    MessageSender, ProtoCodec, RealtimeJoinError, StreamMessage, StreamMessageHandler,
};
use crate::impls::{
    ApiError, EndpointRateLimitCategory, RequestMetadata as ApiRequestMetadata, TransportProtocol,
};
use crate::proto::client::{ClientMessage, ServerMessage};
use crate::runtime::RealtimeConnectionService;
use synctv_core::models::{RoomId, UserId};
use synctv_core::provider::ExecutionControl;
use synctv_core::service::auth::{AuthErrorCategory, JwtValidator};
use synctv_core::service::{ContentFilter, PendingValidatedTicket};

/// Threshold for consecutive slow-client drops before disconnecting them
const SLOW_CLIENT_DROP_THRESHOLD: u32 = 10;
const WEBSOCKET_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

// MetricsGuard - RAII guard for WebSocket metrics

/// RAII guard that increments WebSocket metrics on creation and decrements on drop.
///
/// This ensures metrics are correctly maintained even if the connection handling
/// panics or returns early. Without this guard, metrics would leak in error paths.
///
/// # Example
///
/// ```text
/// async fn handle_socket() {
/// let _guard = MetricsGuard::new();
///
/// // Even if this panics, metrics will be decremented
/// // when _guard is dropped
/// do_work().await;
/// }
/// ```
pub struct MetricsGuard {
    /// Track if we've already decremented (to prevent double-decrement)
    decremented: bool,
}

impl MetricsGuard {
    /// Create a new guard, incrementing WebSocket connection metrics.
    #[must_use = "MetricsGuard must be held for metrics to be tracked correctly"]
    pub fn new() -> Self {
        synctv_core::metrics::http::WEBSOCKET_CONNECTIONS_ACTIVE.inc();
        synctv_core::metrics::http::WEBSOCKET_CONNECTIONS_TOTAL
            .with_label_values(&["success"])
            .inc();

        Self { decremented: false }
    }
}

impl Default for MetricsGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for MetricsGuard {
    fn drop(&mut self) {
        if !self.decremented {
            synctv_core::metrics::http::WEBSOCKET_CONNECTIONS_ACTIVE.dec();
        }
    }
}

type WsQuery = crate::proto::client::WebSocketConnectRequest;

/// Authentication method used for WebSocket connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    Header,
    Ticket,
}

#[derive(Debug, Clone)]
struct TicketAuthCommit {
    ticket: String,
    pending: PendingValidatedTicket,
}

#[derive(Debug, Clone)]
struct HandshakeAuthContext {
    user_id: UserId,
    ticket_commit: Option<TicketAuthCommit>,
}

#[derive(Debug, Clone)]
struct PreparedWebSocketUpgrade {
    room_id: RoomId,
    auth: HandshakeAuthContext,
    username: String,
    connection_id: String,
    reservation: HandshakeReservation,
}

pub struct OptionalPeerIp(Option<std::net::IpAddr>);

impl<S> FromRequestParts<S> for OptionalPeerIp
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(
            parts
                .extensions
                .get::<ConnectInfo<std::net::SocketAddr>>()
                .map(|info| info.0.ip()),
        ))
    }
}

pub struct WebSocketRuntimeReady;

impl<S> FromRequestParts<S> for WebSocketRuntimeReady
where
    S: Send + Sync,
    AppState: axum::extract::FromRef<S>,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let _ = parts;
        let app_state = AppState::from_ref(state);
        validate_websocket_runtime_dependencies(&app_state)?;
        Ok(Self)
    }
}

fn websocket_request_metadata(
    config: &synctv_core::Config,
    headers: &HeaderMap,
    direct_peer_ip: Option<std::net::IpAddr>,
) -> Result<ApiRequestMetadata, AppError> {
    let authorization = headers
        .get(header::AUTHORIZATION)
        .map(|value| {
            value
                .to_str()
                .map(str::to_owned)
                .map_err(|_| AppError::invalid_authorization_header_non_utf8())
        })
        .transpose()?;
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let client_ip = direct_peer_ip
        .map(|peer_ip| crate::client_ip::extract_client_ip_from_headers(config, peer_ip, headers));

    Ok(ApiRequestMetadata::new(TransportProtocol::Http)
        .with_authorization(authorization)
        .with_client_ip(client_ip)
        .with_user_agent(user_agent)
        .with_timeout(Some(WEBSOCKET_HANDSHAKE_TIMEOUT)))
}

fn validate_websocket_authorization_header(authorization: Option<&str>) -> Result<(), AppError> {
    if let Some(authorization) = authorization {
        JwtValidator::extract_bearer_token(authorization)
            .map_err(|_| AppError::invalid_authorization_header())?;
    }
    Ok(())
}

#[cfg(test)]
fn extract_authorization_bearer_token(headers: &HeaderMap) -> Result<Option<String>, AppError> {
    let Some(auth_header) = headers.get(header::AUTHORIZATION) else {
        return Ok(None);
    };

    let auth_str = auth_header
        .to_str()
        .map_err(|_| AppError::invalid_authorization_header_non_utf8())?;

    let token = JwtValidator::extract_bearer_token(auth_str)
        .map_err(|_| AppError::invalid_authorization_header())?;

    Ok(Some(token))
}

fn app_error_to_api_error(err: AppError) -> ApiError {
    match err.status {
        StatusCode::BAD_REQUEST => ApiError::InvalidInput(err.message),
        StatusCode::UNAUTHORIZED => ApiError::Authentication(err.message),
        StatusCode::FORBIDDEN => ApiError::Authorization(err.message),
        StatusCode::NOT_FOUND => ApiError::NotFound(err.message),
        StatusCode::TOO_MANY_REQUESTS => ApiError::RateLimited(err.message),
        StatusCode::REQUEST_TIMEOUT => ApiError::Timeout(err.message),
        StatusCode::SERVICE_UNAVAILABLE => ApiError::ServiceUnavailable(err.message),
        _ => ApiError::Internal(err.message),
    }
}

/// Extract user identity for the WebSocket handshake using explicit request execution.
async fn extract_handshake_auth(
    state: &AppState,
    request_meta: &ApiRequestMetadata,
    query: &WsQuery,
    room_id: &synctv_core::models::RoomId,
    handshake_control: &ExecutionControl,
) -> Result<HandshakeAuthContext, AppError> {
    if request_meta.authorization.is_some() {
        validate_websocket_authorization_header(request_meta.authorization.as_deref())?;
        return state
            .request_executor
            .execute_user_with_control(
                request_meta,
                EndpointRateLimitCategory::WebSocket,
                |_request_control, authenticated| async move {
                    Ok(HandshakeAuthContext {
                        user_id: authenticated.user_id,
                        ticket_commit: None,
                    })
                },
            )
            .await
            .map_err(crate::http::error::map_api_error);
    }

    state
        .request_executor
        .execute_public_with_control(
            request_meta,
            EndpointRateLimitCategory::WebSocket,
            move |_request_control| async move {
                if query.ticket.is_empty() {
                    return Err(ApiError::Authentication(
                        "Missing authentication: provide token via Authorization header or ?ticket="
                            .to_string(),
                    ));
                }

                let pending = state
                    .ws_ticket_service
                    .validate_checked_with_control(
                        &query.ticket,
                        room_id,
                        &*state.user_service,
                        Some(handshake_control),
                    )
                    .await
                    .map_err(map_websocket_ticket_validation_error)
                    .map_err(app_error_to_api_error)?;

                Ok(HandshakeAuthContext {
                    user_id: pending.user_id.clone(),
                    ticket_commit: Some(TicketAuthCommit {
                        ticket: query.ticket.clone(),
                        pending,
                    }),
                })
            },
        )
        .await
        .map_err(crate::http::error::map_api_error)
}

#[cfg(test)]
fn map_security_pipeline_error(error: synctv_core::Error) -> AppError {
    match synctv_core::service::auth::SecurityPipeline::classify_auth_error(&error) {
        AuthErrorCategory::Authentication => AppError::invalid_or_expired_token(),
        AuthErrorCategory::Authorization => {
            crate::http::error::map_auth_authorization_error(&error)
        }
        AuthErrorCategory::Unavailable | AuthErrorCategory::Internal => AppError::from(error),
    }
}

fn map_websocket_ticket_validation_error(error: synctv_core::Error) -> AppError {
    if let synctv_core::Error::Authorization(message) = &error {
        if message.eq_ignore_ascii_case("Invalid or expired ticket") {
            return AppError::invalid_or_expired_ticket();
        }
    }

    match synctv_core::service::auth::SecurityPipeline::classify_auth_error(&error) {
        AuthErrorCategory::Authentication => AppError::invalid_or_expired_ticket(),
        AuthErrorCategory::Authorization => {
            crate::http::error::map_auth_authorization_error(&error)
        }
        AuthErrorCategory::Unavailable | AuthErrorCategory::Internal => AppError::from(error),
    }
}

fn map_websocket_membership_probe_error(error: synctv_core::Error) -> AppError {
    AppError::from(error)
}

async fn validate_websocket_room_membership(
    room_service: &synctv_core::service::RoomService,
    room: &synctv_core::models::Room,
    user_id: &UserId,
) -> Result<(), AppError> {
    room_service
        .check_membership_with_room(room, user_id)
        .await
        .map_err(map_websocket_membership_probe_error)
}

fn validate_websocket_origin(
    headers: &HeaderMap,
    allowed_origins: &[String],
    direct_peer_ip: Option<std::net::IpAddr>,
    trusted_proxies: &[String],
) -> Result<(), AppError> {
    let Some(origin) = headers.get(header::ORIGIN) else {
        // Non-browser clients typically omit Origin. Keep supporting them.
        return Ok(());
    };

    let origin = origin
        .to_str()
        .map_err(|_| AppError::forbidden("Invalid Origin header: non-UTF-8 value"))?;

    if origin.eq_ignore_ascii_case("null") {
        return Err(AppError::forbidden(
            "WebSocket Origin is not allowed for this endpoint",
        ));
    }

    let parsed_origin =
        url::Url::parse(origin).map_err(|_| AppError::forbidden("Invalid Origin header format"))?;

    if !matches!(parsed_origin.scheme(), "http" | "https") {
        return Err(AppError::forbidden(
            "WebSocket Origin must use http or https",
        ));
    }

    if let Some(host) = headers
        .get(header::HOST)
        .and_then(|host| host.to_str().ok())
    {
        let forwarded_proto = direct_peer_ip.and_then(|peer_ip| {
            if is_trusted_proxy(peer_ip, trusted_proxies) {
                headers
                    .get("x-forwarded-proto")
                    .and_then(|value| value.to_str().ok())
            } else {
                None
            }
        });
        if same_origin_as_host(&parsed_origin, host, forwarded_proto) {
            return Ok(());
        }
    }

    if allowed_origins.iter().any(|allowed| allowed == origin) {
        return Ok(());
    }

    Err(AppError::forbidden(
        "WebSocket Origin is not allowed for this endpoint",
    ))
}

fn is_trusted_proxy(peer_ip: std::net::IpAddr, trusted_proxies: &[String]) -> bool {
    trusted_proxies.iter().any(|proxy| {
        proxy
            .parse::<ipnet::IpNet>()
            .map(|network| network.contains(&peer_ip))
            .or_else(|_| {
                proxy
                    .parse::<std::net::IpAddr>()
                    .map(|proxy_ip| proxy_ip == peer_ip)
            })
            .unwrap_or(false)
    })
}

fn same_origin_as_host(
    origin: &url::Url,
    host_header: &str,
    forwarded_proto: Option<&str>,
) -> bool {
    let Some(origin_host) = origin.host_str() else {
        return false;
    };

    let (request_host, request_port) = split_host_and_port(host_header);
    if !origin_host.eq_ignore_ascii_case(request_host) {
        return false;
    }

    if let Some(request_scheme) = forwarded_proto {
        if !origin.scheme().eq_ignore_ascii_case(request_scheme) {
            return false;
        }
    }

    origin.port_or_known_default()
        == request_port.or_else(|| default_port_for_scheme(origin.scheme()))
}

fn split_host_and_port(host_header: &str) -> (&str, Option<u16>) {
    if let Some(stripped) = host_header.strip_prefix('[') {
        if let Some(end) = stripped.find(']') {
            let host = &stripped[..end];
            let remainder = &stripped[end + 1..];
            let port = remainder
                .strip_prefix(':')
                .and_then(|port| port.parse().ok());
            return (host, port);
        }
    }

    match host_header.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => (host, port.parse().ok()),
        _ => (host_header, None),
    }
}

fn default_port_for_scheme(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    }
}

pub(crate) fn websocket_runtime_dependencies_available(state: &AppState) -> bool {
    state.event_service.is_some() && state.chat_service.is_some()
}

pub(crate) fn validate_websocket_runtime_dependencies(state: &AppState) -> Result<(), AppError> {
    validate_websocket_runtime_dependency_flags(websocket_runtime_dependencies_available(state))
}

fn validate_websocket_runtime_dependency_flags(
    dependencies_available: bool,
) -> Result<(), AppError> {
    if !dependencies_available {
        return Err(AppError::service_unavailable());
    }

    Ok(())
}

async fn load_websocket_username(state: &AppState, user_id: &UserId) -> Result<String, AppError> {
    state
        .user_service
        .get_username(user_id)
        .await
        .map_err(|error| {
            error!(
                user_id = %user_id.as_str(),
                error = %error,
                "WebSocket handshake rejected: failed to load username"
            );
            AppError::service_unavailable()
        })?
        .ok_or_else(|| {
            warn!(
                user_id = %user_id.as_str(),
                "WebSocket handshake rejected: authenticated user missing username record"
            );
            AppError::unauthorized("Authentication failed")
        })
}

/// WebSocket stream implementation of `StreamMessage` trait
///
/// This adapts WebSocket's `axum::extract::ws::WebSocket` to our unified `StreamMessage` interface.
struct WebSocketStream {
    receiver: futures::stream::SplitStream<axum::extract::ws::WebSocket>,
    sender: WebSocketMessageSender,
    is_alive: Arc<std::sync::atomic::AtomicBool>,
    /// Raw channel for sending WebSocket control frames (Ping)
    raw_sender: tokio::sync::mpsc::Sender<axum::extract::ws::Message>,
}

#[async_trait::async_trait]
impl StreamMessage for WebSocketStream {
    async fn recv(&mut self) -> Option<Result<ClientMessage, String>> {
        loop {
            match self.receiver.next().await {
                Some(Ok(axum::extract::ws::Message::Binary(bytes))) => {
                    return Some(ProtoCodec::decode_client_message(&bytes));
                }
                Some(Ok(axum::extract::ws::Message::Close(_))) => {
                    return None; // Graceful close
                }
                Some(Err(e)) => return Some(Err(format!("WebSocket error: {e}"))),
                None => return None, // Stream ended
                Some(Ok(_)) => {
                    // Ignore non-binary messages (text, ping, pong) and continue loop
                }
            }
        }
    }

    fn send(&self, message: ServerMessage) -> Result<(), String> {
        MessageSender::send(&self.sender, message)
    }

    fn is_alive(&self) -> bool {
        self.is_alive.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn ping(&self) -> Result<(), String> {
        self.raw_sender
            .try_send(axum::extract::ws::Message::Ping(vec![].into()))
            .map_err(|e| match e {
                tokio::sync::mpsc::error::TrySendError::Full(_) => {
                    "Ping failed: channel full".to_string()
                }
                tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                    "Ping failed: channel closed".to_string()
                }
            })
    }
}

/// WebSocket message sender implementation
struct WebSocketMessageSender {
    normal_sender: tokio::sync::mpsc::Sender<axum::extract::ws::Message>,
    critical_sender: tokio::sync::mpsc::Sender<axum::extract::ws::Message>,
    /// Count of consecutive message drops (channel full). When this exceeds
    /// `SLOW_CLIENT_DROP_THRESHOLD` the `send()` method returns an error to trigger
    /// a graceful disconnect for the slow client.
    consecutive_drops: Arc<AtomicU32>,
}

impl WebSocketMessageSender {
    fn new(
        normal_sender: tokio::sync::mpsc::Sender<axum::extract::ws::Message>,
        critical_sender: tokio::sync::mpsc::Sender<axum::extract::ws::Message>,
    ) -> Self {
        Self {
            normal_sender,
            critical_sender,
            consecutive_drops: Arc::new(AtomicU32::new(0)),
        }
    }

    /// Clone the sender sharing the same drop counter (used to give handler and ping
    /// channel different senders that still track slowness jointly).
    fn clone_sender(&self) -> Self {
        Self {
            normal_sender: self.normal_sender.clone(),
            critical_sender: self.critical_sender.clone(),
            consecutive_drops: Arc::clone(&self.consecutive_drops),
        }
    }
}

async fn forward_websocket_messages<S>(
    mut critical_messages: tokio::sync::mpsc::Receiver<axum::extract::ws::Message>,
    mut outbound_messages: tokio::sync::mpsc::Receiver<axum::extract::ws::Message>,
    mut ws_sender_sink: S,
    is_alive: Arc<std::sync::atomic::AtomicBool>,
    connection_service: Arc<dyn RealtimeConnectionService>,
    connection_id: String,
) where
    S: futures::Sink<axum::extract::ws::Message, Error = axum::Error> + Unpin,
{
    let mut critical_closed = false;
    let mut outbound_closed = false;
    let mut prioritize_critical = true;

    loop {
        let msg = if prioritize_critical {
            if critical_closed {
                tokio::select! {
                    outbound = outbound_messages.recv(), if !outbound_closed => {
                        if let Some(msg) = outbound {
                            prioritize_critical = true;
                            Some(msg)
                        } else {
                            outbound_closed = true;
                            None
                        }
                    }
                    else => break,
                }
            } else {
                match critical_messages.try_recv() {
                    Ok(msg) => {
                        prioritize_critical = false;
                        Some(msg)
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                        critical_closed = true;
                        prioritize_critical = false;
                        None
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                        tokio::select! {
                            critical = critical_messages.recv(), if !critical_closed => {
                                if let Some(msg) = critical {
                                    prioritize_critical = false;
                                    Some(msg)
                                } else {
                                    critical_closed = true;
                                    prioritize_critical = false;
                                    None
                                }
                            }
                            outbound = outbound_messages.recv(), if !outbound_closed => {
                                if let Some(msg) = outbound {
                                    prioritize_critical = true;
                                    Some(msg)
                                } else {
                                    outbound_closed = true;
                                    None
                                }
                            }
                            else => break,
                        }
                    }
                }
            }
        } else {
            if outbound_closed {
                tokio::select! {
                    critical = critical_messages.recv(), if !critical_closed => {
                        if let Some(msg) = critical {
                            prioritize_critical = false;
                            Some(msg)
                        } else {
                            critical_closed = true;
                            None
                        }
                    }
                    else => break,
                }
            } else {
                match outbound_messages.try_recv() {
                    Ok(msg) => {
                        prioritize_critical = true;
                        Some(msg)
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                        outbound_closed = true;
                        prioritize_critical = true;
                        None
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                        tokio::select! {
                            outbound = outbound_messages.recv(), if !outbound_closed => {
                                if let Some(msg) = outbound {
                                    prioritize_critical = true;
                                    Some(msg)
                                } else {
                                    outbound_closed = true;
                                    prioritize_critical = true;
                                    None
                                }
                            }
                            critical = critical_messages.recv(), if !critical_closed => {
                                if let Some(msg) = critical {
                                    prioritize_critical = false;
                                    Some(msg)
                                } else {
                                    critical_closed = true;
                                    None
                                }
                            }
                            else => break,
                        }
                    }
                }
            }
        };

        let Some(msg) = msg else {
            if critical_closed && outbound_closed {
                break;
            }
            continue;
        };

        if let Err(e) = ws_sender_sink.send(msg).await {
            error!(
                connection_id = %connection_id,
                error = %e,
                "Failed to send WebSocket message"
            );
            is_alive.store(false, std::sync::atomic::Ordering::Relaxed);
            connection_service.disconnect_connection(&connection_id);
            break;
        }
    }
}

/// Returns `true` if the given `ServerMessage` carries a critical payload that
/// MUST be delivered (playback state changes, kick/ban notifications, room
/// deletion). Critical messages use a blocking send with timeout so they are
/// not silently dropped.
const fn is_critical_message(message: &ServerMessage) -> bool {
    use crate::proto::client::server_message::Message;
    matches!(
        &message.message,
        Some(
            Message::PlaybackState(_)
                | Message::PlayingChanged(_)
                | Message::Error(_)
                | Message::PermissionChanged(_)
                | Message::RoomSettings(_)
        )
    )
}

const fn requires_state_resync(message: &ServerMessage) -> bool {
    use crate::proto::client::server_message::Message;
    matches!(
        &message.message,
        Some(
            Message::UserJoined(_)
                | Message::UserLeft(_)
                | Message::MediaAdded(_)
                | Message::MediaRemoved(_)
                | Message::MediaRemovedBatch(_)
                | Message::MediaUpdated(_)
                | Message::PlaylistReordered(_)
                | Message::PlaylistCreated(_)
                | Message::PlaylistUpdated(_)
                | Message::PlaylistDeleted(_)
                | Message::PlaylistItems(_)
                | Message::PlaybackSnapshot(_)
                | Message::RoomMembers(_)
                | Message::Notification(_)
        )
    )
}

/// Returns a human-readable message type name for logging purposes.
const fn message_type_name(message: &ServerMessage) -> &'static str {
    use crate::proto::client::server_message::Message;
    match &message.message {
        Some(Message::Chat(_)) => "Chat",
        Some(Message::PlaybackState(_)) => "PlaybackState",
        Some(Message::UserJoined(_)) => "UserJoined",
        Some(Message::UserLeft(_)) => "UserLeft",
        Some(Message::RoomSettings(_)) => "RoomSettings",
        Some(Message::HeartbeatAck(_)) => "HeartbeatAck",
        Some(Message::Error(_)) => "Error",
        Some(Message::MediaAdded(_)) => "MediaAdded",
        Some(Message::MediaRemoved(_)) => "MediaRemoved",
        Some(Message::MediaRemovedBatch(_)) => "MediaRemovedBatch",
        Some(Message::MediaUpdated(_)) => "MediaUpdated",
        Some(Message::PermissionChanged(_)) => "PermissionChanged",
        Some(Message::PlaylistReordered(_)) => "PlaylistReordered",
        Some(Message::PlaylistCreated(_)) => "PlaylistCreated",
        Some(Message::PlaylistUpdated(_)) => "PlaylistUpdated",
        Some(Message::PlaylistDeleted(_)) => "PlaylistDeleted",
        Some(Message::PlaylistItems(_)) => "PlaylistItems",
        Some(Message::RoomMembers(_)) => "RoomMembers",
        Some(Message::PlayingChanged(_)) => "PlayingChanged",
        Some(Message::WebrtcOffer(_)) => "WebrtcOffer",
        Some(Message::WebrtcAnswer(_)) => "WebrtcAnswer",
        Some(Message::WebrtcIceCandidate(_)) => "WebrtcIceCandidate",
        Some(Message::WebrtcJoin(_)) => "WebrtcJoin",
        Some(Message::WebrtcLeave(_)) => "WebrtcLeave",
        Some(Message::SfuMigrationOffer(_)) => "SfuMigrationOffer",
        Some(Message::SfuMigrationStatus(_)) => "SfuMigrationStatus",
        Some(Message::Notification(_)) => "Notification",
        Some(Message::PlaybackSnapshot(_)) => "PlaybackSnapshot",
        None => "None",
    }
}

impl crate::impls::messaging::MessageSender for WebSocketMessageSender {
    fn send(&self, message: ServerMessage) -> Result<(), String> {
        // Encode to binary proto
        let bytes = ProtoCodec::encode_server_message(&message)?;
        let ws_msg = axum::extract::ws::Message::Binary(bytes.into());

        let critical = is_critical_message(&message);

        if critical {
            match self.critical_sender.try_send(ws_msg) {
                Ok(()) => {
                    self.consecutive_drops.store(0, Ordering::Relaxed);
                    return Ok(());
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    return Err("Channel closed: WebSocket client disconnected".to_string());
                }
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    let drops = self.consecutive_drops.fetch_add(1, Ordering::Relaxed) + 1;
                    let msg_type = message_type_name(&message);
                    warn!(
                        consecutive_drops = drops,
                        message_type = msg_type,
                        "Critical WebSocket message rejected: critical queue full (slow client)"
                    );
                    synctv_core::metrics::http::WEBSOCKET_ERRORS_TOTAL
                        .with_label_values(&["message_dropped_critical"])
                        .inc();
                    return Err(format!(
                        "Critical message (type={msg_type}) rejected: critical queue full after {drops} consecutive drops (slow client)"
                    ));
                }
            }
        }

        // Non-critical messages: use try_send; track drops but do not error unless
        // the client has been consistently slow for SLOW_CLIENT_DROP_THRESHOLD sends.
        match self.normal_sender.try_send(ws_msg) {
            Ok(()) => {
                self.consecutive_drops.store(0, Ordering::Relaxed);
                Ok(())
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                let drops = self.consecutive_drops.fetch_add(1, Ordering::Relaxed) + 1;
                let msg_type = message_type_name(&message);
                let requires_resync = requires_state_resync(&message);
                warn!(
                    consecutive_drops = drops,
                    message_type = msg_type,
                    "WebSocket message dropped: channel full (slow client)"
                );
                synctv_core::metrics::http::WEBSOCKET_ERRORS_TOTAL
                    .with_label_values(&["message_dropped"])
                    .inc();
                if requires_resync || drops >= SLOW_CLIENT_DROP_THRESHOLD {
                    // Too many consecutive drops: disconnect the slow client gracefully
                    Err(format!(
                        "Slow client disconnected: dropped stateful message (type={msg_type}) after {drops} consecutive drops"
                    ))
                } else {
                    // Still within threshold: log and drop the non-critical message
                    // Note: Non-critical messages (chat, user join/leave, etc.) can be dropped
                    // but this log helps diagnose sync issues. If playback state seems out of
                    // sync, check for frequent "message dropped" warnings.
                    Ok(())
                }
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                Err("Channel closed: WebSocket client disconnected".to_string())
            }
        }
    }
}

/// WebSocket handler for room real-time updates
///
/// Clients can authenticate via:
/// 1. Authorization header: `Authorization: Bearer <token>` (most secure)
/// 2. Ticket query parameter: `?ticket=<ticket>` (recommended for browsers)
///
/// Example:
/// - Native clients: `ws://host/ws/rooms/{room_id}` with `Authorization: Bearer <token>`
/// - Browser clients: `ws://host/ws/rooms/{room_id}?ticket=<ticket>` (obtained from POST /api/tickets)
#[allow(dead_code)]
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/ws/rooms/{room_id}",
        tag = "WebSocket",
        operation_id = "connectRoomWebSocket",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("ticket" = Option<String>, Query, description = "Short-lived one-time ticket returned by POST /api/tickets. Optional when the Authorization header is provided."),
            ("Authorization" = Option<String>, Header, description = "Bearer access token in the form `Bearer <jwt>`. Optional when the ticket query parameter is provided."),
            ("Origin" = Option<String>, Header, description = "Browser origin header. When WebSocket origin checks are enabled, this header must match an allowed origin.")
        ),
        responses(
            (status = 101, description = "Switching Protocols. The HTTP connection is upgraded to a WebSocket stream after authentication, room membership, origin, and runtime checks pass."),
            (status = 400, description = "Invalid room_id or ticket format", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Missing authentication, invalid or expired token, or invalid or expired ticket", body = crate::openapi::ErrorResponseDoc),
            (status = 403, description = "Origin rejected, room banned, or caller is not allowed to connect to the room", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Room not found", body = crate::openapi::ErrorResponseDoc),
            (status = 408, description = "WebSocket handshake timed out", body = crate::openapi::ErrorResponseDoc),
            (status = 429, description = "Rate limited or connection limit exceeded", body = crate::openapi::ErrorResponseDoc),
            (status = 503, description = "Realtime runtime or ticket backend unavailable", body = crate::openapi::ErrorResponseDoc)
        )
    )
)]
pub(crate) const fn websocket_room_connect_doc() {}

pub async fn websocket_handler(
    State(state): State<AppState>,
    _runtime_ready: WebSocketRuntimeReady,
    Path(path): Path<crate::proto::client::RoomPathRequest>,
    Query(query): Query<WsQuery>,
    headers: HeaderMap,
    peer_ip: OptionalPeerIp,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, AppError> {
    crate::impls::validate_proto_request(&path).map_err(crate::http::error::map_api_error)?;
    crate::impls::validate_proto_request(&query).map_err(crate::http::error::map_api_error)?;
    let room_id = path.room_id;
    let request_meta = websocket_request_metadata(state.config.as_ref(), &headers, peer_ip.0)?;
    let handshake_control = ExecutionControl::from_timeout(request_meta.timeout);

    let prepared = run_websocket_handshake_with_timeout(async {
        let prepared = prepare_websocket_upgrade(
            &state,
            &room_id,
            &query,
            &headers,
            &request_meta,
            &handshake_control,
        )
        .await?;

        commit_websocket_upgrade(&state, prepared, &handshake_control).await
    })
    .await?;

    let failed_upgrade_cleanup = build_failed_upgrade_cleanup(
        state.connection_manager.clone(),
        prepared.reservation.clone(),
    );

    // Authentication and membership verified, upgrade to WebSocket.
    // Reservations are released inside handle_socket after join_room completes.
    // Limit max message size to 64KB (default is 64MB which is excessive for signaling)
    Ok(ws
        .max_message_size(64 * 1024)
        .on_failed_upgrade(failed_upgrade_cleanup)
        .on_upgrade(move |socket| {
            handle_socket(
                socket,
                state,
                prepared.room_id,
                prepared.auth,
                prepared.username,
                prepared.connection_id,
                prepared.reservation,
            )
        }))
}

async fn commit_prevalidated_ticket(
    state: &AppState,
    room_id: &RoomId,
    auth: &HandshakeAuthContext,
    handshake_control: &ExecutionControl,
) -> Result<(), AppError> {
    let Some(ticket_commit) = auth.ticket_commit.as_ref() else {
        return Ok(());
    };

    state
        .ws_ticket_service
        .consume_prevalidated_with_control(
            &ticket_commit.ticket,
            room_id,
            &ticket_commit.pending,
            Some(handshake_control),
        )
        .await
        .map(|_| ())
        .map_err(map_websocket_ticket_validation_error)
}

async fn commit_websocket_upgrade(
    state: &AppState,
    prepared: PreparedWebSocketUpgrade,
    handshake_control: &ExecutionControl,
) -> Result<PreparedWebSocketUpgrade, AppError> {
    let mut cleanup = ReservationCleanupGuard::new(
        state.connection_manager.clone(),
        prepared.reservation.clone(),
    );

    commit_prevalidated_ticket(state, &prepared.room_id, &prepared.auth, handshake_control).await?;
    cleanup.disarm();

    Ok(prepared)
}

async fn run_websocket_handshake_with_timeout<T>(
    handshake: impl Future<Output = Result<T, AppError>>,
) -> Result<T, AppError> {
    tokio::time::timeout(WEBSOCKET_HANDSHAKE_TIMEOUT, handshake)
        .await
        .map_err(|_| AppError::new(StatusCode::REQUEST_TIMEOUT, "WebSocket handshake timed out"))?
}

struct ReservationCleanupGuard {
    connection_service: Arc<dyn RealtimeConnectionService>,
    reservation: HandshakeReservation,
    armed: bool,
}

impl ReservationCleanupGuard {
    const fn new(
        connection_service: Arc<dyn RealtimeConnectionService>,
        reservation: HandshakeReservation,
    ) -> Self {
        Self {
            connection_service,
            reservation,
            armed: true,
        }
    }

    const fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ReservationCleanupGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        self.reservation.release(self.connection_service.as_ref());
    }
}

#[derive(Debug, Clone)]
struct HandshakeReservation {
    room_id: RoomId,
    user_id: UserId,
}

impl HandshakeReservation {
    fn release(&self, connection_service: &dyn RealtimeConnectionService) {
        connection_service.release_room_reservation(&self.room_id);
        connection_service.release_user_reservation(&self.user_id);
    }
}

fn reserve_websocket_upgrade_slots(
    connection_service: &dyn RealtimeConnectionService,
    room_id: &RoomId,
    user_id: &UserId,
) -> Result<HandshakeReservation, AppError> {
    connection_service
        .reserve_user_slot(user_id)
        .map_err(RealtimeJoinError::from)
        .map_err(map_websocket_pre_join_error)?;

    if let Err(error) = connection_service
        .reserve_room_slot(room_id)
        .map_err(RealtimeJoinError::from)
        .map_err(map_websocket_pre_join_error)
    {
        connection_service.release_user_reservation(user_id);
        return Err(error);
    }

    Ok(HandshakeReservation {
        room_id: room_id.clone(),
        user_id: user_id.clone(),
    })
}

async fn prepare_websocket_upgrade(
    state: &AppState,
    room_id: &str,
    query: &WsQuery,
    headers: &HeaderMap,
    request_meta: &ApiRequestMetadata,
    handshake_control: &ExecutionControl,
) -> Result<PreparedWebSocketUpgrade, AppError> {
    validate_websocket_origin(
        headers,
        &state.config.server.cors_allowed_origins,
        request_meta.client_ip,
        &state.config.server.trusted_proxies,
    )?;

    let rid = synctv_core::models::RoomId::from_string(room_id.to_string());

    let auth = extract_handshake_auth(state, request_meta, query, &rid, handshake_control).await?;
    let user_id = auth.user_id.clone();

    let room = state
        .room_service
        .get_room(&rid)
        .await
        .map_err(AppError::from)?;

    if room.is_banned {
        return Err(AppError::forbidden("This room has been banned"));
    }

    if room.status.is_closed() {
        return Err(AppError::forbidden(
            "This room is closed and not accepting new connections",
        ));
    }

    validate_websocket_room_membership(&state.room_service, &room, &user_id).await?;

    validate_websocket_runtime_dependencies(state)?;
    let username = load_websocket_username(state, &user_id).await?;
    let connection_id = StreamMessageHandler::generate_connection_id(&user_id);
    let reservation =
        reserve_websocket_upgrade_slots(state.connection_manager.as_ref(), &rid, &user_id)?;

    Ok(PreparedWebSocketUpgrade {
        room_id: rid,
        auth,
        username,
        connection_id,
        reservation,
    })
}

fn build_failed_upgrade_cleanup(
    connection_service: Arc<dyn RealtimeConnectionService>,
    reservation: HandshakeReservation,
) -> impl FnOnce(axum::Error) + Send + 'static {
    move |error| {
        warn!(
            room_id = %reservation.room_id.as_str(),
            user_id = %reservation.user_id.as_str(),
            error = %error,
            "WebSocket upgrade failed after reserving connection capacity; releasing reservation"
        );
        reservation.release(connection_service.as_ref());
    }
}

fn websocket_content_filter(filter: &Arc<ContentFilter>) -> Arc<ContentFilter> {
    Arc::clone(filter)
}

fn map_websocket_pre_join_error(error: RealtimeJoinError) -> AppError {
    match error {
        RealtimeJoinError::RateLimited(message) => AppError::too_many_requests(message),
        RealtimeJoinError::ServiceUnavailable(_) => AppError::service_unavailable(),
        RealtimeJoinError::PermissionDenied(message) => AppError::forbidden(message),
        RealtimeJoinError::Internal(message) => {
            tracing::error!("Unexpected WebSocket pre_join failure: {message}");
            AppError::internal_server_error("Failed to establish WebSocket connection")
        }
    }
}

async fn handle_socket(
    socket: axum::extract::ws::WebSocket,
    state: AppState,
    room_id: RoomId,
    auth: HandshakeAuthContext,
    username: String,
    connection_id: String,
    reservation: HandshakeReservation,
) {
    let user_id = auth.user_id.clone();
    let socket = socket;
    let mut reservation_cleanup =
        ReservationCleanupGuard::new(state.connection_manager.clone(), reservation.clone());

    info!(
        "WebSocket connection established: user={}, room={}",
        user_id.as_str(),
        room_id.as_str()
    );

    // Check if cluster_manager is available BEFORE incrementing metrics.
    // This prevents counter drift: if we return early, we never incremented,
    // so there's nothing to decrement.
    let event_service = if let Some(ref service) = state.event_service {
        service.clone()
    } else {
        error!("Realtime event service not available, WebSocket connection not supported");
        return;
    };

    // Create RAII guard for metrics - ensures metrics are decremented even on panic.
    // This must be created AFTER all early-return checks to prevent false decrements.
    let _metrics_guard = MetricsGuard::new();

    // Use the shared rate limiter from app state
    let rate_limiter = state.rate_limiter.clone();
    let rate_limit_config = state.messaging_rate_limit_config.clone();
    let content_filter = websocket_content_filter(&state.content_filter);

    // Separate outbound channels keep critical state/control messages from being
    // starved behind a backlog of best-effort traffic.
    let (critical_tx, critical_rx) = tokio::sync::mpsc::channel::<axum::extract::ws::Message>(64);
    let (tx, rx) = tokio::sync::mpsc::channel::<axum::extract::ws::Message>(1000);
    let is_alive = Arc::new(std::sync::atomic::AtomicBool::new(true));

    // Create WebSocket sender - wrapped in Arc for sharing with handler.
    // All senders share the same consecutive-drop counter via clone_sender().
    let ws_sender_primary = WebSocketMessageSender::new(tx.clone(), critical_tx.clone());
    let ws_sender_for_handler = Arc::new(ws_sender_primary.clone_sender());
    let raw_sender_for_ping = tx.clone();
    let ws_sender = ws_sender_primary;

    // Resolve chat_service (required for proper business logic enforcement)
    let chat_service = if let Some(ref svc) = state.chat_service {
        svc.clone()
    } else {
        error!("chat_service not available, WebSocket connection not supported");
        return;
    };

    // Create StreamMessageHandler with all configuration
    let stream_handler = StreamMessageHandler::new(
        room_id.clone(),
        user_id.clone(),
        username.clone(),
        &state.room_service,
        chat_service,
        event_service,
        state.connection_manager.clone(),
        rate_limiter,
        rate_limit_config,
        content_filter,
        ws_sender_for_handler,
    )
    .with_playback_snapshot_service(state.client_api.clone())
    .with_playlist_items_snapshot_service(state.client_api.clone())
    .with_room_members_snapshot_service(state.client_api.clone())
    .with_connection_id(connection_id.clone())
    .with_heartbeat_schedule(state.heartbeat_schedule)
    .with_ws_message_rate_limit(
        state
            .config
            .connection_limits
            .ws_message_rate_limit_per_second,
    );

    // H11: Wire notification service for direct real-time push
    let stream_handler = if let Some(ref notif_svc) = state.notification_service {
        stream_handler.with_notification_service(Arc::clone(notif_svc))
    } else {
        stream_handler
    };
    let connection_id = stream_handler.connection_id().to_string();

    let (incoming_tx, cancel_token) = match stream_handler.start().await {
        Ok(started) => started,
        Err(error) => {
            error!(
                "Failed to join WebSocket connection before message loop: {}",
                error
            );
            return;
        }
    };

    reservation_cleanup.disarm();
    reservation.release(state.connection_manager.as_ref());

    // Split WebSocket into sender and receiver
    let (mut ws_sender_sink, ws_receiver) = socket.split();

    // Spawn task to handle server messages -> WebSocket
    let is_alive_clone = is_alive.clone();
    let connection_service = state.connection_manager.clone();
    tokio::spawn(async move {
        forward_websocket_messages(
            critical_rx,
            rx,
            &mut ws_sender_sink,
            is_alive_clone,
            connection_service,
            connection_id,
        )
        .await;
    });

    // Pump transport input into the shared handler. The handler owns all
    // business logic and cleanup; this task only decodes WebSocket frames.
    let input_cancel_token = cancel_token.clone();
    let close_sender_on_cancel = critical_tx.clone();
    tokio::spawn(async move {
        let mut stream = WebSocketStream {
            receiver: ws_receiver,
            sender: ws_sender,
            is_alive,
            raw_sender: raw_sender_for_ping,
        };

        loop {
            tokio::select! {
                () = input_cancel_token.cancelled() => {
                    let _ = close_sender_on_cancel.try_send(axum::extract::ws::Message::Close(None));
                    break;
                }
                message = stream.recv() => {
                    match message {
                        Some(Ok(message)) => {
                            if incoming_tx.send(message).await.is_err() {
                                break;
                            }
                        }
                        Some(Err(error)) => {
                            error!("WebSocket receive error: {}", error);
                            input_cancel_token.cancel();
                            break;
                        }
                        None => {
                            input_cancel_token.cancel();
                            break;
                        }
                    }
                }
            }
        }
    });

    cancel_token.cancelled().await;

    // Metrics are automatically decremented when _metrics_guard is dropped
    // (RAII ensures this happens even if the above code panics)

    info!(
        "WebSocket connection closed: user={}, room={}",
        user_id.as_str(),
        room_id.as_str()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use synctv_cluster::sync::{ConnectionLimits, ConnectionManager};
    use synctv_core::{
        cache::{KeyBuilder, UsernameCache},
        config::PasswordComplexityConfig,
        models::{RoomId, UserId},
        service::{
            auth::{BruteForceProtection, JwtService},
            InMemoryTokenBlacklistStore, RoomService, UserService, UserValidationResult,
            UserValidator,
        },
    };

    struct AllowAllTicketValidator;

    #[async_trait::async_trait]
    impl UserValidator for AllowAllTicketValidator {
        async fn validate_for_ticket(
            &self,
            _user_id: &UserId,
        ) -> synctv_core::Result<UserValidationResult> {
            Ok(UserValidationResult {
                password_version: 0,
            })
        }
    }

    fn test_user_service(pool: sqlx::PgPool) -> UserService {
        let jwt_service =
            JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").expect("jwt service");
        let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
        let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));

        UserService::new(
            pool,
            jwt_service,
            username_cache,
            PasswordComplexityConfig::default(),
            token_blacklist,
            KeyBuilder::new("test"),
            BruteForceProtection::in_memory("test".to_string()),
        )
    }

    fn test_room_service(pool: sqlx::PgPool) -> RoomService {
        RoomService::new(pool.clone(), test_user_service(pool))
    }

    #[test]
    fn test_ws_query_no_auth() {
        let query = WsQuery {
            ticket: String::new(),
        };
        assert!(query.ticket.is_empty());
    }

    #[test]
    fn test_ws_query_with_ticket() {
        let query = WsQuery {
            ticket: "ticket_abc".to_string(),
        };
        assert_eq!(query.ticket, "ticket_abc");
    }

    #[test]
    fn test_websocket_request_metadata_uses_forwarded_ip_for_trusted_proxy() {
        let mut config = synctv_core::Config::default();
        config.server.trusted_proxies = vec!["127.0.0.1".to_string()];

        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.50".parse().unwrap());

        let metadata =
            websocket_request_metadata(&config, &headers, Some("127.0.0.1".parse().unwrap()))
                .expect("metadata should build");

        assert_eq!(metadata.client_ip, Some("203.0.113.50".parse().unwrap()));
    }

    #[test]
    fn test_websocket_content_filter_reuses_shared_filter() {
        let shared = Arc::new(ContentFilter::with_config(
            17,
            9,
            Some(vec!["blocked".to_string()]),
            false,
        ));
        let selected = websocket_content_filter(&shared);
        assert!(
            Arc::ptr_eq(&selected, &shared),
            "websocket path must reuse the shared ContentFilter instance"
        );
        assert_eq!(selected.max_chat_length, 17);
        assert_eq!(selected.max_danmaku_length, 9);
        assert_eq!(
            selected.filter_chat("<b>hi</b>").unwrap(),
            "<b>hi</b>",
            "websocket path must reuse the shared filter config instead of default strip_html=true"
        );
    }

    #[test]
    fn test_ws_query_deserialization_empty() {
        let json = "{}";
        let query: WsQuery = serde_json::from_str(json).expect("deserialize empty");
        assert!(query.ticket.is_empty());
    }

    #[test]
    fn test_ws_query_deserialization_with_ticket() {
        let json = r#"{"ticket":"my_ticket"}"#;
        let query: WsQuery = serde_json::from_str(json).expect("deserialize");
        assert_eq!(query.ticket, "my_ticket");
    }

    #[test]
    fn test_ws_query_deserialization_ignores_extra_fields() {
        let json = r#"{"ticket":"tix","extra":"ignored"}"#;
        let query: WsQuery = serde_json::from_str(json).expect("deserialize with extra");
        assert_eq!(query.ticket, "tix");
    }

    #[test]
    fn test_media_removed_batch_requires_state_resync() {
        let message = ServerMessage {
            message: Some(
                crate::proto::client::server_message::Message::MediaRemovedBatch(
                    crate::proto::client::MediaRemovedBatch {
                        room_id: "room_test".to_string(),
                        media_ids: vec!["media_a".to_string(), "media_b".to_string()],
                        removed_by: "frank".to_string(),
                        removed_by_user_id: "user_test".to_string(),
                    },
                ),
            ),
        };

        assert!(requires_state_resync(&message));
        assert_eq!(message_type_name(&message), "MediaRemovedBatch");
    }

    #[test]
    fn test_playback_snapshot_requires_state_resync() {
        let message = ServerMessage {
            message: Some(
                crate::proto::client::server_message::Message::PlaybackSnapshot(
                    crate::proto::client::PlaybackSnapshotChanged {
                        room_id: "room_test".to_string(),
                        snapshot: Some(crate::proto::client::PlaybackSnapshot {
                            media_id: String::new(),
                            playlist_id: String::new(),
                            room_id: "room_test".to_string(),
                            name: String::new(),
                            position: 0.0,
                            playback_infos: std::collections::HashMap::new(),
                            default_mode: String::new(),
                            metadata: std::collections::HashMap::new(),
                            version: "1".to_string(),
                            expires_at: Some(4_102_444_800),
                        }),
                    },
                ),
            ),
        };

        assert!(requires_state_resync(&message));
        assert_eq!(message_type_name(&message), "PlaybackSnapshot");
    }

    // extract_user_id is async and requires AppState, so we test the priority
    // logic via the documented contract:
    // 1. Header > 2. Ticket
    // These tests verify the query parsing that feeds into extract_user_id.

    #[test]
    fn test_auth_priority_header_present_in_header_map() {
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", "Bearer some_jwt_token".parse().unwrap());

        // When Authorization header is present, it should be checked first
        let auth_header = headers.get("Authorization");
        assert!(auth_header.is_some());
        let auth_str = auth_header.unwrap().to_str().unwrap();
        assert!(auth_str.starts_with("Bearer "));
        let token = auth_str.strip_prefix("Bearer ").unwrap();
        assert_eq!(token, "some_jwt_token");
    }

    #[test]
    fn test_auth_priority_no_bearer_prefix_in_header() {
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", "Basic dXNlcjpwYXNz".parse().unwrap());

        // Non-Bearer auth should not extract a token
        let auth_header = headers.get("Authorization").unwrap();
        let auth_str = auth_header.to_str().unwrap();
        assert!(auth_str.strip_prefix("Bearer ").is_none());
    }

    #[test]
    fn test_auth_priority_invalid_utf8_header_is_not_ignored() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            axum::http::HeaderValue::from_bytes(b"Bearer \xFFinvalid").unwrap(),
        );

        let err = extract_authorization_bearer_token(&headers)
            .expect_err("non-UTF-8 authorization header must fail closed");
        assert_eq!(err.status, axum::http::StatusCode::UNAUTHORIZED);
        assert!(err.message.contains("non-UTF-8"));
    }

    #[test]
    fn test_auth_priority_no_header_falls_through_to_ticket() {
        let headers = HeaderMap::new();
        let query = WsQuery {
            ticket: "ticket_abc".to_string(),
        };

        // No Authorization header
        assert!(headers.get("Authorization").is_none());
        // Ticket is available as fallback
        assert!(!query.ticket.is_empty());
    }

    #[test]
    fn test_auth_priority_no_auth_at_all() {
        let headers = HeaderMap::new();
        let query = WsQuery {
            ticket: String::new(),
        };

        assert!(headers.get("Authorization").is_none());
        assert!(query.ticket.is_empty());
        // This would produce an Unauthorized error in extract_user_id
    }

    #[test]
    fn test_unauthorized_error_for_missing_auth() {
        let err = AppError::unauthorized(
            "Missing authentication: provide token via Authorization header or ?ticket=",
        );
        assert_eq!(err.status, axum::http::StatusCode::UNAUTHORIZED);
        assert!(err.message.contains("Missing authentication"));
    }

    #[test]
    fn test_forbidden_error_for_non_member() {
        let err = AppError::forbidden("Not a member of this room");
        assert_eq!(err.status, axum::http::StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_unauthorized_error_for_revoked_token() {
        let err = AppError::unauthorized("Token has been revoked");
        assert_eq!(err.status, axum::http::StatusCode::UNAUTHORIZED);
        assert_eq!(err.message, "Token has been revoked");
    }

    #[test]
    fn test_validate_websocket_origin_allows_missing_origin_for_non_browser_clients() {
        let headers = HeaderMap::new();
        validate_websocket_origin(&headers, &[], None, &[])
            .expect("missing origin should be allowed");
    }

    #[test]
    fn test_validate_websocket_origin_allows_same_origin_host_when_explicitly_allowlisted() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "app.example.com".parse().unwrap());
        headers.insert(header::ORIGIN, "https://app.example.com".parse().unwrap());

        validate_websocket_origin(
            &headers,
            &["https://app.example.com".to_string()],
            None,
            &[],
        )
        .expect("same-origin browser websocket should only be allowed when explicitly configured");
    }

    #[test]
    fn test_validate_websocket_origin_allows_same_origin_host_without_explicit_allowlist() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "app.example.com".parse().unwrap());
        headers.insert(header::ORIGIN, "https://app.example.com".parse().unwrap());

        validate_websocket_origin(&headers, &[], None, &[]).expect(
            "same-origin browser websocket should be allowed without explicit CORS allowlist",
        );
    }

    #[test]
    fn test_validate_websocket_origin_allows_explicitly_configured_cross_origin() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "api.example.com".parse().unwrap());
        headers.insert(header::ORIGIN, "https://app.example.com".parse().unwrap());

        validate_websocket_origin(
            &headers,
            &["https://app.example.com".to_string()],
            None,
            &[],
        )
        .expect("configured frontend origin should be allowed");
    }

    #[test]
    fn test_validate_websocket_origin_rejects_same_host_with_mismatched_scheme() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "app.example.com".parse().unwrap());
        headers.insert(header::ORIGIN, "http://app.example.com".parse().unwrap());
        headers.insert("x-forwarded-proto", "https".parse().unwrap());

        let err = validate_websocket_origin(
            &headers,
            &[],
            Some("127.0.0.1".parse().unwrap()),
            &["127.0.0.1".to_string()],
        )
        .expect_err("same host with proxy-reported https must reject an http origin");
        assert_eq!(err.status, axum::http::StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_validate_websocket_origin_ignores_forwarded_proto_from_untrusted_peer() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "app.example.com".parse().unwrap());
        headers.insert(header::ORIGIN, "http://app.example.com".parse().unwrap());
        headers.insert("x-forwarded-proto", "https".parse().unwrap());

        validate_websocket_origin(
            &headers,
            &[],
            Some("198.51.100.10".parse().unwrap()),
            &["127.0.0.1".to_string()],
        )
        .expect("untrusted peers must not influence same-origin checks via x-forwarded-proto");
    }

    #[test]
    fn test_split_host_and_port_supports_ipv6_host_header() {
        let (host, port) = split_host_and_port("[::1]:8080");
        assert_eq!(host, "::1");
        assert_eq!(port, Some(8080));
    }

    #[test]
    fn test_validate_websocket_origin_rejects_unconfigured_cross_origin() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "api.example.com".parse().unwrap());
        headers.insert(header::ORIGIN, "https://evil.example.com".parse().unwrap());

        let err = validate_websocket_origin(&headers, &[], None, &[])
            .expect_err("unconfigured cross-origin websocket must fail closed");
        assert_eq!(err.status, axum::http::StatusCode::FORBIDDEN);
        assert!(err.message.contains("Origin"));
    }

    #[test]
    fn test_validate_websocket_origin_rejects_null_origin() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "api.example.com".parse().unwrap());
        headers.insert(header::ORIGIN, "null".parse().unwrap());

        let err = validate_websocket_origin(&headers, &[], None, &[])
            .expect_err("null origin should not be trusted");
        assert_eq!(err.status, axum::http::StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_validate_websocket_runtime_dependency_flags_require_realtime_and_chat_services() {
        let err = validate_websocket_runtime_dependency_flags(false)
            .expect_err("missing realtime event service must fail before websocket upgrade");
        assert_eq!(err.status, axum::http::StatusCode::SERVICE_UNAVAILABLE);

        validate_websocket_runtime_dependency_flags(true)
            .expect("present dependencies should allow websocket upgrade to proceed");
    }

    #[test]
    fn test_websocket_handshake_timeout_matches_global_http_timeout_budget() {
        assert_eq!(
            WEBSOCKET_HANDSHAKE_TIMEOUT,
            Duration::from_secs(30),
            "websocket handshake timeout should match the HTTP request timeout budget"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_websocket_handshake_timeout_returns_request_timeout_error() {
        let handshake = async {
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            Ok::<(), AppError>(())
        };

        let timeout_task =
            tokio::spawn(async move { run_websocket_handshake_with_timeout(handshake).await });

        tokio::time::advance(WEBSOCKET_HANDSHAKE_TIMEOUT + Duration::from_secs(1)).await;

        let err = timeout_task
            .await
            .expect("timeout task should complete")
            .expect_err("pending handshake must time out");

        assert_eq!(err.status, StatusCode::REQUEST_TIMEOUT);
        assert_eq!(err.message, "WebSocket handshake timed out");
    }

    #[tokio::test(start_paused = true)]
    async fn test_handshake_timeout_releases_reserved_capacity_without_marking_presence() {
        let manager = Arc::new(ConnectionManager::new(ConnectionLimits {
            max_per_room: 1,
            max_per_user: 1,
            ..ConnectionLimits::default()
        }));
        let user_id = UserId::from_string("user-timeout-cleanup".to_string());
        let room_id = RoomId::from_string("room-timeout-cleanup".to_string());
        let reservation = HandshakeReservation {
            room_id: room_id.clone(),
            user_id: user_id.clone(),
        };

        manager
            .reserve_user_slot(&user_id)
            .expect("handshake should reserve a user slot");
        manager
            .reserve_room_slot(&room_id)
            .expect("handshake should reserve a room slot");

        assert!(
            manager.get_connection_id(&room_id, &user_id).is_none(),
            "reserved handshakes must not appear as active presence"
        );
        assert!(
            manager.reserve_user_slot(&user_id).is_err(),
            "user handshake reservations should remain full while the reservation is active"
        );
        assert!(
            manager.reserve_room_slot(&room_id).is_err(),
            "room handshake reservations should remain full while the reservation is active"
        );

        let handshake_manager = manager.clone();
        let handshake = async move {
            let _cleanup = ReservationCleanupGuard::new(handshake_manager, reservation);
            std::future::pending::<Result<(), AppError>>().await
        };

        let timeout_task =
            tokio::spawn(async move { run_websocket_handshake_with_timeout(handshake).await });

        tokio::time::advance(WEBSOCKET_HANDSHAKE_TIMEOUT + Duration::from_secs(1)).await;

        let err = timeout_task
            .await
            .expect("timeout task should complete")
            .expect_err("pending reserved handshake must time out");
        assert_eq!(err.status, StatusCode::REQUEST_TIMEOUT);

        assert!(
            manager.reserve_user_slot(&user_id).is_ok(),
            "timeout cleanup should free user reservation capacity"
        );
        assert!(
            manager.reserve_room_slot(&room_id).is_ok(),
            "timeout cleanup should free room reservation capacity"
        );
    }

    #[test]
    fn test_room_not_found_maps_to_not_found_error_during_websocket_prepare() {
        let err = AppError::from(synctv_core::Error::NotFound("Room not found".to_string()));
        assert_eq!(err.status, StatusCode::NOT_FOUND);
        assert_eq!(err.message, "Room not found");
    }

    #[tokio::test]
    async fn test_load_websocket_username_fails_closed_on_storage_error() {
        let state = crate::http::tests::test_app_state();
        state.user_service.pool().close().await;
        let user_id = UserId::from_string("ws-user-storage-error".to_string());

        let err = load_websocket_username(&state, &user_id)
            .await
            .expect_err("username lookup infrastructure failures must fail closed");

        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            err.message.contains("temporarily unavailable"),
            "username lookup outages should surface as retryable handshake failures"
        );
    }

    #[test]
    fn test_map_security_pipeline_error_maps_backend_outages_to_service_unavailable() {
        let err = map_security_pipeline_error(synctv_core::Error::ServiceUnavailable(
            "Authentication service temporarily unavailable".to_string(),
        ));

        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            err.message.contains("temporarily unavailable"),
            "websocket auth backend outages should remain retryable"
        );
    }

    #[test]
    fn test_map_websocket_ticket_validation_error_preserves_backend_outages() {
        let err = map_websocket_ticket_validation_error(synctv_core::Error::ServiceUnavailable(
            "Authentication service temporarily unavailable".to_string(),
        ));

        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            err.message.contains("temporarily unavailable"),
            "ticket validation outages should not be collapsed into invalid-ticket 401s"
        );
    }

    #[test]
    fn test_map_websocket_ticket_validation_error_keeps_invalid_ticket_as_401() {
        let err = map_websocket_ticket_validation_error(synctv_core::Error::Authorization(
            "Invalid or expired ticket".to_string(),
        ));

        assert_eq!(err.status, StatusCode::UNAUTHORIZED);
        assert_eq!(err.message, "Invalid or expired ticket");
    }

    #[test]
    fn test_map_websocket_membership_probe_error_preserves_backend_outages() {
        let err = map_websocket_membership_probe_error(synctv_core::Error::ServiceUnavailable(
            "membership backend temporarily unavailable".to_string(),
        ));

        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            err.message.contains("temporarily unavailable"),
            "websocket membership probe outages should remain retryable"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_validate_websocket_room_membership_rejects_room_with_inactive_creator() {
        let (_container, pool) = synctv_core_testing::create_test_pool().await;
        let room_service = test_room_service(pool.clone());
        let user_service = room_service.user_service().clone();

        let owner = user_service
            .register(
                "ws-owner-inactive".to_string(),
                Some("ws-owner-inactive@test.invalid".to_string()),
                "Password123!".to_string(),
                None,
            )
            .await
            .expect("owner should register")
            .0;
        let member = user_service
            .register(
                "ws-member-inactive-owner".to_string(),
                Some("ws-member-inactive-owner@test.invalid".to_string()),
                "Password123!".to_string(),
                None,
            )
            .await
            .expect("member should register")
            .0;

        let room = room_service
            .create_room(
                "ws-room-inactive-owner".to_string(),
                String::new(),
                owner.id.clone(),
                None,
                None,
            )
            .await
            .expect("room should be created")
            .0;
        room_service
            .join_room(room.id.clone(), member.id.clone(), None)
            .await
            .expect("member should join room");

        synctv_core::repository::UserRepository::new(pool.clone())
            .ban(&owner.id, None, Some("websocket test".to_string()))
            .await
            .expect("banning owner should succeed");

        let err = validate_websocket_room_membership(&room_service, &room, &member.id)
            .await
            .expect_err("room with inactive creator must be rejected during websocket prepare");

        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert!(
            err.message.contains("creator is not active"),
            "expected creator-inactive error, got: {}",
            err.message
        );

        pool.close().await;
    }

    #[test]
    fn test_map_websocket_pre_join_error_maps_typed_rate_limit_prefix() {
        let err = map_websocket_pre_join_error(RealtimeJoinError::RateLimited(
            "realtime room capacity exceeded".to_string(),
        ));

        assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(err.message, "realtime room capacity exceeded");
    }

    #[test]
    fn test_map_websocket_pre_join_error_maps_raw_capacity_error() {
        let err = map_websocket_pre_join_error(RealtimeJoinError::RateLimited(
            "Room at capacity (42 connections, max: 40)".to_string(),
        ));

        assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(err.message, "Room at capacity (42 connections, max: 40)");
    }

    #[test]
    fn test_map_websocket_pre_join_error_maps_raw_user_capacity_error() {
        let err = map_websocket_pre_join_error(RealtimeJoinError::RateLimited(
            "Too many connections for this user across all replicas (max 3)".to_string(),
        ));

        assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            err.message,
            "Too many connections for this user across all replicas (max 3)"
        );
    }

    #[test]
    fn test_map_websocket_pre_join_error_maps_raw_total_capacity_error() {
        let err = map_websocket_pre_join_error(RealtimeJoinError::RateLimited(
            "Server at capacity across all replicas (42 connections)".to_string(),
        ));

        assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            err.message,
            "Server at capacity across all replicas (42 connections)"
        );
    }

    #[test]
    fn test_map_websocket_pre_join_error_maps_typed_service_unavailable_prefix() {
        let err = map_websocket_pre_join_error(RealtimeJoinError::ServiceUnavailable(
            "distributed room capacity check unavailable".to_string(),
        ));

        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            err.message.contains("temporarily unavailable"),
            "typed service-unavailable pre-join failures should remain retryable"
        );
    }

    #[test]
    fn test_map_websocket_pre_join_error_maps_raw_degraded_cluster_error() {
        let err = map_websocket_pre_join_error(
            RealtimeJoinError::ServiceUnavailable(
                "Distributed room capacity check unavailable; refusing room join while cluster Redis is degraded"
                    .to_string(),
            ),
        );

        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            err.message.contains("temporarily unavailable"),
            "raw degraded-cluster pre-join failures should remain retryable"
        );
    }

    #[test]
    fn test_map_websocket_pre_join_error_maps_raw_degraded_user_check_error() {
        let err = map_websocket_pre_join_error(
            RealtimeJoinError::ServiceUnavailable(
                "Distributed user connection check unavailable; refusing new connection while cluster Redis is degraded"
                    .to_string(),
            ),
        );

        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            err.message.contains("temporarily unavailable"),
            "raw degraded user-check failures should remain retryable"
        );
    }

    #[test]
    fn test_map_websocket_pre_join_error_maps_raw_degraded_total_check_error() {
        let err = map_websocket_pre_join_error(
            RealtimeJoinError::ServiceUnavailable(
                "Distributed total connection check unavailable; refusing new connection while cluster Redis is degraded"
                    .to_string(),
            ),
        );

        assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            err.message.contains("temporarily unavailable"),
            "raw degraded total-check failures should remain retryable"
        );
    }

    #[test]
    fn test_map_websocket_pre_join_error_maps_business_denial_to_forbidden() {
        let err = map_websocket_pre_join_error(RealtimeJoinError::PermissionDenied(
            "User is no longer allowed to use real-time messaging".to_string(),
        ));

        assert_eq!(err.status, StatusCode::FORBIDDEN);
        assert_eq!(
            err.message,
            "User is no longer allowed to use real-time messaging"
        );
    }

    #[test]
    fn test_map_websocket_pre_join_error_hides_unexpected_internal_details() {
        let err = map_websocket_pre_join_error(RealtimeJoinError::Internal(
            "cluster subscription cache blew up".to_string(),
        ));

        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.message, "Failed to establish WebSocket connection");
    }

    // These tests verify that the RateLimitConfig used for WebSocket message handling
    // has sensible defaults and can be customized.

    #[tokio::test]
    async fn test_failed_upgrade_cleanup_releases_reserved_capacity_without_presence() {
        let manager = Arc::new(ConnectionManager::new(ConnectionLimits {
            max_per_room: 1,
            max_per_user: 1,
            ..ConnectionLimits::default()
        }));
        let user_id = UserId::from_string("user-upgrade-fail".to_string());
        let room_id = RoomId::from_string("room-upgrade-fail".to_string());
        let reservation = HandshakeReservation {
            room_id: room_id.clone(),
            user_id: user_id.clone(),
        };

        manager
            .reserve_user_slot(&user_id)
            .expect("handshake should reserve a user slot");
        manager
            .reserve_room_slot(&room_id)
            .expect("handshake should reserve a room slot");

        assert!(
            manager.get_connection_id(&room_id, &user_id).is_none(),
            "failed upgrades must not leave a visible active connection"
        );
        assert!(
            manager.reserve_user_slot(&user_id).is_err(),
            "user handshake reservations should remain full while the upgrade reservation is active"
        );
        assert!(
            manager.reserve_room_slot(&room_id).is_err(),
            "room handshake reservations should remain full while the upgrade reservation is active"
        );

        let cleanup = build_failed_upgrade_cleanup(manager.clone(), reservation);
        cleanup(axum::Error::new(std::io::Error::other("upgrade failed")));

        assert!(
            manager.reserve_user_slot(&user_id).is_ok(),
            "cleanup should free user reservation capacity"
        );
        assert!(
            manager.reserve_room_slot(&room_id).is_ok(),
            "cleanup should free room reservation capacity"
        );
    }

    #[tokio::test]
    async fn test_failed_upgrade_cleanup_leaves_consumed_ticket_spent() {
        let state = crate::http::tests::test_app_state();
        let ws_ticket_service = state.ws_ticket_service.clone();
        let user_id = UserId::from_string("user-ticket-restore".to_string());
        let room_id = RoomId::from_string("room-ticket-restore".to_string());
        let reservation = HandshakeReservation {
            room_id: room_id.clone(),
            user_id: user_id.clone(),
        };

        state
            .router_config
            .connection_manager
            .reserve_user_slot(&user_id)
            .expect("handshake should reserve a user slot");
        state
            .router_config
            .connection_manager
            .reserve_room_slot(&room_id)
            .expect("handshake should reserve a room slot");

        let ticket = ws_ticket_service
            .create_ticket(&user_id, &room_id, 0)
            .await
            .expect("create websocket ticket");
        let pending = ws_ticket_service
            .validate_checked(&ticket, &room_id, &AllowAllTicketValidator)
            .await
            .expect("ticket should prevalidate before upgrade");
        let prepared = PreparedWebSocketUpgrade {
            room_id: room_id.clone(),
            auth: HandshakeAuthContext {
                user_id: user_id.clone(),
                ticket_commit: Some(TicketAuthCommit {
                    ticket: ticket.clone(),
                    pending,
                }),
            },
            username: "ticket-user".to_string(),
            connection_id: "conn-ticket-restore".to_string(),
            reservation: reservation.clone(),
        };
        let handshake_control = ExecutionControl::default();

        commit_websocket_upgrade(&state, prepared, &handshake_control)
            .await
            .expect("handshake commit should consume the ticket before switching protocols");

        let cleanup = build_failed_upgrade_cleanup(
            state.router_config.connection_manager.clone(),
            reservation,
        );
        cleanup(axum::Error::new(std::io::Error::other("upgrade failed")));

        let validated = ws_ticket_service
            .validate_and_consume(&ticket, &room_id)
            .await;
        assert!(
            validated.is_err(),
            "failed upgrade cleanup must not resurrect a one-time ticket after the HTTP handshake succeeded"
        );
    }

    #[tokio::test]
    async fn test_commit_websocket_upgrade_releases_reservation_when_ticket_claim_fails() {
        let state = crate::http::tests::test_app_state();
        let ws_ticket_service = state.ws_ticket_service.clone();
        let user_id = UserId::from_string("user-ticket-claim-fail".to_string());
        let room_id = RoomId::from_string("room-ticket-claim-fail".to_string());

        let reservation = reserve_websocket_upgrade_slots(
            state.router_config.connection_manager.as_ref(),
            &room_id,
            &user_id,
        )
        .expect("handshake should reserve websocket capacity");

        let ticket = ws_ticket_service
            .create_ticket(&user_id, &room_id, 0)
            .await
            .expect("create websocket ticket");
        let pending = ws_ticket_service
            .validate_checked(&ticket, &room_id, &AllowAllTicketValidator)
            .await
            .expect("ticket should prevalidate before upgrade");

        ws_ticket_service
            .consume_prevalidated(&ticket, &room_id, &pending)
            .await
            .expect("fixture should spend the ticket before commit");

        let prepared = PreparedWebSocketUpgrade {
            room_id: room_id.clone(),
            auth: HandshakeAuthContext {
                user_id: user_id.clone(),
                ticket_commit: Some(TicketAuthCommit { ticket, pending }),
            },
            username: "ticket-user".to_string(),
            connection_id: "conn-ticket-claim-fail".to_string(),
            reservation,
        };
        let handshake_control = ExecutionControl::default();

        let error = commit_websocket_upgrade(&state, prepared, &handshake_control)
            .await
            .expect_err("commit should fail when another handshake already claimed the ticket");
        assert_eq!(error.status, StatusCode::UNAUTHORIZED);

        state
            .router_config
            .connection_manager
            .reserve_user_slot(&user_id)
            .expect("failed commit should release the reserved user slot");
        state
            .router_config
            .connection_manager
            .reserve_room_slot(&room_id)
            .expect("failed commit should release the reserved room slot");
    }

    #[tokio::test(start_paused = true)]
    async fn test_commit_websocket_upgrade_releases_reservation_when_timeout_cancels_commit() {
        let state = crate::http::tests::test_app_state();
        let timeout_state = state.clone();
        let user_id = UserId::from_string("user-ticket-timeout".to_string());
        let room_id = RoomId::from_string("room-ticket-timeout".to_string());
        let reservation = reserve_websocket_upgrade_slots(
            state.router_config.connection_manager.as_ref(),
            &room_id,
            &user_id,
        )
        .expect("handshake should reserve websocket capacity");

        let prepared = PreparedWebSocketUpgrade {
            room_id: room_id.clone(),
            auth: HandshakeAuthContext {
                user_id: user_id.clone(),
                ticket_commit: None,
            },
            username: "ticket-user".to_string(),
            connection_id: "conn-ticket-timeout".to_string(),
            reservation,
        };
        let handshake_control = ExecutionControl::default();

        let timeout_task = tokio::spawn(async move {
            run_websocket_handshake_with_timeout(async move {
                let prepared =
                    commit_websocket_upgrade(&timeout_state, prepared, &handshake_control).await?;
                std::future::pending::<()>().await;
                #[allow(unreachable_code)]
                Ok::<PreparedWebSocketUpgrade, AppError>(prepared)
            })
            .await
        });

        tokio::time::advance(WEBSOCKET_HANDSHAKE_TIMEOUT + Duration::from_secs(1)).await;

        let err = timeout_task
            .await
            .expect("timeout task should complete")
            .expect_err("commit path should time out");
        assert_eq!(err.status, StatusCode::REQUEST_TIMEOUT);

        state
            .router_config
            .connection_manager
            .reserve_user_slot(&user_id)
            .expect("timed out commit should release the reserved user slot");
        state
            .router_config
            .connection_manager
            .reserve_room_slot(&room_id)
            .expect("timed out commit should release the reserved room slot");
    }

    #[tokio::test]
    async fn test_reservation_stays_full_until_connection_pre_join_succeeds() {
        let manager = Arc::new(ConnectionManager::new(ConnectionLimits {
            max_per_room: 1,
            max_per_user: 1,
            ..ConnectionLimits::default()
        }));
        let user_id = UserId::from_string("user-pre-join-transfer".to_string());
        let room_id = RoomId::from_string("room-pre-join-transfer".to_string());
        let reservation = HandshakeReservation {
            room_id: room_id.clone(),
            user_id: user_id.clone(),
        };
        let connection_id = "conn-pre-join-transfer".to_string();

        manager
            .reserve_user_slot(&user_id)
            .expect("handshake should reserve a user slot");
        manager
            .reserve_room_slot(&room_id)
            .expect("handshake should reserve a room slot");

        assert!(
            manager.reserve_user_slot(&user_id).is_err(),
            "user capacity must remain full while only the handshake reservation exists"
        );
        assert!(
            manager.reserve_room_slot(&room_id).is_err(),
            "room capacity must remain full while only the handshake reservation exists"
        );

        manager
            .register(connection_id.clone(), user_id.clone())
            .await
            .expect("pre_join should register the connection before releasing reservation");
        manager
            .join_room(&connection_id, room_id.clone())
            .await
            .expect("pre_join should join the room before releasing reservation");

        assert!(
            manager.reserve_user_slot(&user_id).is_err(),
            "active registration must keep user capacity full before reservation release"
        );
        assert!(
            manager.reserve_room_slot(&room_id).is_err(),
            "active room membership must keep room capacity full before reservation release"
        );

        reservation.release(manager.as_ref());

        assert!(
            manager.reserve_user_slot(&user_id).is_err(),
            "releasing the handshake reservation must not free capacity still used by the active connection"
        );
        assert!(
            manager.reserve_room_slot(&room_id).is_err(),
            "releasing the handshake reservation must not free room capacity still used by the active connection"
        );

        manager.unregister(&connection_id).await;

        assert!(
            manager.reserve_user_slot(&user_id).is_ok(),
            "capacity should reopen only after the active connection leaves"
        );
        assert!(
            manager.reserve_room_slot(&room_id).is_ok(),
            "room capacity should reopen only after the active connection leaves"
        );
    }

    #[test]
    fn test_state_resync_messages_disconnect_slow_client_immediately() {
        use crate::impls::messaging::MessageSender;
        use crate::proto::client::{server_message::Message, ServerMessage, UserJoinedRoom};

        let (critical_tx, _critical_rx) = tokio::sync::mpsc::channel(1);
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let sender = WebSocketMessageSender::new(tx.clone(), critical_tx);

        tx.try_send(axum::extract::ws::Message::Text("occupied".into()))
            .expect("fill the channel");

        let result = sender.send(ServerMessage {
            message: Some(Message::UserJoined(UserJoinedRoom {
                room_id: "room12345678".to_string(),
                member: None,
            })),
        });

        assert!(
            result.is_err(),
            "stateful join messages must disconnect slow clients instead of being silently dropped"
        );
        let err = result.unwrap_err();
        assert!(err.contains("stateful message"));
        assert!(err.contains("UserJoined"));
    }

    #[test]
    fn test_critical_messages_bypass_full_normal_queue() {
        use crate::impls::messaging::MessageSender;
        use crate::proto::client::{server_message::Message, ErrorMessage, ServerMessage};

        let (critical_tx, mut critical_rx) = tokio::sync::mpsc::channel(1);
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let sender = WebSocketMessageSender::new(tx.clone(), critical_tx);

        tx.try_send(axum::extract::ws::Message::Text("occupied".into()))
            .expect("fill normal queue");

        let result = sender.send(ServerMessage {
            message: Some(Message::Error(ErrorMessage {
                message: "critical".to_string(),
                code: synctv_proto::common::ErrorCode::Forbidden as i32,
                detail: String::new(),
            })),
        });

        assert!(
            result.is_ok(),
            "critical websocket messages must still enqueue when the normal queue is full"
        );
        assert!(
            critical_rx.try_recv().is_ok(),
            "critical message should be queued on the dedicated critical channel"
        );
    }

    #[tokio::test]
    async fn test_forward_websocket_messages_disconnects_connection_on_sink_failure() {
        use axum::Error;
        use futures::task::{Context, Poll};
        use std::pin::Pin;

        struct FailingSink;

        impl futures::Sink<axum::extract::ws::Message> for FailingSink {
            type Error = Error;

            fn poll_ready(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }

            fn start_send(
                self: Pin<&mut Self>,
                _item: axum::extract::ws::Message,
            ) -> Result<(), Self::Error> {
                Ok(())
            }

            fn poll_flush(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Err(Error::new(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "synthetic sink failure",
                ))))
            }

            fn poll_close(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }
        }

        let connection_id = "conn-forward-failure".to_string();
        let user_id = UserId::from_string("user-forward-failure".to_string());
        let manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
        manager
            .register(connection_id.clone(), user_id)
            .await
            .expect("register connection");

        let mut disconnect_rx = manager.subscribe_disconnect();
        let is_alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (critical_tx, critical_rx) = tokio::sync::mpsc::channel(1);
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        tx.send(axum::extract::ws::Message::Text("payload".into()))
            .await
            .expect("enqueue outbound message");
        drop(tx);
        drop(critical_tx);

        forward_websocket_messages(
            critical_rx,
            rx,
            FailingSink,
            is_alive.clone(),
            manager,
            connection_id.clone(),
        )
        .await;

        assert!(
            !is_alive.load(std::sync::atomic::Ordering::Relaxed),
            "sink failure must mark the connection dead immediately"
        );

        let signal = tokio::time::timeout(std::time::Duration::from_secs(1), disconnect_rx.recv())
            .await
            .expect("disconnect signal should be sent promptly")
            .expect("disconnect channel should remain open");

        match signal {
            synctv_cluster::sync::DisconnectSignal::Connection(id) => {
                assert_eq!(id, connection_id);
            }
            other => panic!("expected connection disconnect signal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_forward_websocket_messages_prioritizes_critical_queue() {
        use axum::Error;
        use futures::task::{Context, Poll};
        use std::pin::Pin;

        #[derive(Default)]
        struct RecordingSink {
            sent: Vec<String>,
        }

        impl futures::Sink<axum::extract::ws::Message> for RecordingSink {
            type Error = Error;

            fn poll_ready(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }

            fn start_send(
                mut self: Pin<&mut Self>,
                item: axum::extract::ws::Message,
            ) -> Result<(), Self::Error> {
                let label = match item {
                    axum::extract::ws::Message::Text(text) => text.to_string(),
                    other => format!("{other:?}"),
                };
                self.sent.push(label);
                Ok(())
            }

            fn poll_flush(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }

            fn poll_close(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }
        }

        let manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
        let is_alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (critical_tx, critical_rx) = tokio::sync::mpsc::channel(2);
        let (tx, rx) = tokio::sync::mpsc::channel(2);

        tx.send(axum::extract::ws::Message::Text("normal".into()))
            .await
            .expect("enqueue normal message");
        critical_tx
            .send(axum::extract::ws::Message::Text("critical".into()))
            .await
            .expect("enqueue critical message");
        drop(tx);
        drop(critical_tx);

        let mut sink = RecordingSink::default();
        forward_websocket_messages(
            critical_rx,
            rx,
            &mut sink,
            is_alive,
            manager,
            "conn-priority".to_string(),
        )
        .await;

        assert_eq!(
            sink.sent,
            vec!["critical".to_string(), "normal".to_string()],
            "critical websocket queue must be drained before best-effort backlog"
        );
    }

    #[tokio::test]
    async fn test_forward_websocket_messages_prevents_normal_queue_starvation() {
        use axum::Error;
        use futures::task::{Context, Poll};
        use std::pin::Pin;

        #[derive(Default)]
        struct RecordingSink {
            sent: Vec<String>,
        }

        impl futures::Sink<axum::extract::ws::Message> for RecordingSink {
            type Error = Error;

            fn poll_ready(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }

            fn start_send(
                mut self: Pin<&mut Self>,
                item: axum::extract::ws::Message,
            ) -> Result<(), Self::Error> {
                let label = match item {
                    axum::extract::ws::Message::Text(text) => text.to_string(),
                    other => format!("{other:?}"),
                };
                self.sent.push(label);
                Ok(())
            }

            fn poll_flush(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }

            fn poll_close(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }
        }

        let manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
        let is_alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (critical_tx, critical_rx) = tokio::sync::mpsc::channel(8);
        let (tx, rx) = tokio::sync::mpsc::channel(8);

        for idx in 0..3 {
            critical_tx
                .send(axum::extract::ws::Message::Text(
                    format!("critical-{idx}").into(),
                ))
                .await
                .expect("enqueue critical message");
        }
        tx.send(axum::extract::ws::Message::Text("normal".into()))
            .await
            .expect("enqueue normal message");
        for idx in 3..6 {
            critical_tx
                .send(axum::extract::ws::Message::Text(
                    format!("critical-{idx}").into(),
                ))
                .await
                .expect("enqueue later critical message");
        }
        drop(tx);
        drop(critical_tx);

        let mut sink = RecordingSink::default();
        forward_websocket_messages(
            critical_rx,
            rx,
            &mut sink,
            is_alive,
            manager,
            "conn-fairness".to_string(),
        )
        .await;

        let normal_index = sink
            .sent
            .iter()
            .position(|message| message == "normal")
            .expect("normal queue message must be forwarded");

        assert!(
            normal_index < 4,
            "normal queue should not starve behind sustained critical traffic: {:?}",
            sink.sent
        );
    }
}
