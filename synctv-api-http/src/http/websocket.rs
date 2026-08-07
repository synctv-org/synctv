//! WebSocket handler for room realtime messaging.
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
//! - Then use `ws://host/ws/rooms/{roomId}?ticket=xxx`
//! - Tickets are single-use and expire quickly (30 seconds by default)

use axum::{
    extract::{ConnectInfo, FromRef, FromRequestParts, Path, Query, State, WebSocketUpgrade},
    http::request::Parts,
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
};
use futures::{SinkExt, StreamExt};
use prost_reflect::{DynamicMessage, ReflectMessage};
use std::convert::Infallible;
use std::future::Future;
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};
use std::time::Duration;
use tracing::{error, info, warn};

use crate::http::{optional_header_str, AppError, AppState};
use synctv_api_common::impls::messaging::{
    GuestRealtimeIdentity, MessageConcurrencyConfig, MessageSender, ProtoCodec, RealtimeJoinError,
    RealtimePrincipal, StreamMessage, StreamMessageHandler, StreamMessageHandlerConfig,
    StreamMessageHandlerRuntime,
};
use synctv_api_common::impls::{
    ApiError, EndpointRateLimitCategory, EndpointRateLimitScope,
    RequestMetadata as ApiRequestMetadata, TransportProtocol,
};
use synctv_core::models::{RoomId, UserId};
use synctv_core::provider::ExecutionControl;
use synctv_core::service::{AuthErrorCategory, JwtValidator};
use synctv_core::service::{ContentFilter, PendingValidatedTicket, ValidatedGuestTicket};
use synctv_proto::client::{ClientMessage, ServerMessage};
use synctv_realtime::sync::ConnectionRuntime;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RealtimeTransportFormat {
    Json,
    Protobuf,
}

impl RealtimeTransportFormat {
    pub(crate) fn parse(value: Option<&str>) -> Result<Self, AppError> {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            None | Some("json") => Ok(Self::Json),
            Some("protobuf") => Ok(Self::Protobuf),
            Some(other) => Err(AppError::bad_request(format!(
                "Invalid format '{other}'. Expected json or protobuf"
            ))),
        }
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct WsQuery {
    #[serde(default)]
    ticket: String,
    #[serde(default)]
    format: Option<String>,
}

fn websocket_connect_request(query: &WsQuery) -> synctv_proto::client::WebSocketConnectRequest {
    synctv_proto::client::WebSocketConnectRequest {
        ticket: query.ticket.clone(),
    }
}

fn guest_principal_from_ticket(
    room_id: RoomId,
    guest: &ValidatedGuestTicket,
) -> Result<RealtimePrincipal, RealtimeJoinError> {
    RealtimePrincipal::guest(
        room_id,
        GuestRealtimeIdentity {
            guest_id: guest.guest_id.clone(),
            display_name: guest.display_name.clone(),
            session_id: guest.session_id.clone(),
            token_jti: guest.token_jti.clone(),
            room_guest_version: guest.room_guest_version,
            permissions: guest.permissions,
        },
    )
}

/// Authentication method used for WebSocket connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    Header,
    Ticket,
    GuestToken,
}

#[derive(Debug, Clone)]
struct TicketAuthCommit {
    ticket: String,
    pending: PendingValidatedTicket,
}

#[derive(Debug, Clone)]
struct HandshakeAuthContext {
    user_id: UserId,
    principal: RealtimePrincipal,
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

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(Ok(Self(
            parts
                .extensions
                .get::<ConnectInfo<std::net::SocketAddr>>()
                .map(|info| info.0.ip()),
        )))
    }
}

pub struct WebSocketRuntimeReady;

impl<S> FromRequestParts<S> for WebSocketRuntimeReady
where
    S: Send + Sync,
    AppState: axum::extract::FromRef<S>,
{
    type Rejection = AppError;

    fn from_request_parts(
        _parts: &mut Parts,
        state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        let app_state = AppState::from_ref(state);
        std::future::ready(validate_websocket_runtime_dependencies(&app_state).map(|()| Self))
    }
}

fn websocket_request_metadata(
    runtime_settings: &synctv_api_common::ApiRuntimeSettings,
    headers: &HeaderMap,
    direct_peer_ip: Option<std::net::IpAddr>,
) -> Result<ApiRequestMetadata, AppError> {
    super::reject_duplicate_header(headers, &header::AUTHORIZATION)?;
    let authorization = headers
        .get(header::AUTHORIZATION)
        .map(|value| {
            value
                .to_str()
                .map(str::to_owned)
                .map_err(|_| AppError::invalid_authorization_header_non_utf8())
        })
        .transpose()?;
    let user_agent = optional_header_str(headers, &header::USER_AGENT)?.map(str::to_owned);
    let client_ip = direct_peer_ip
        .map(|peer_ip| {
            synctv_adapter::client_ip::extract_client_ip_from_headers(
                |ip| runtime_settings.server.is_trusted_proxy(ip),
                peer_ip,
                headers,
            )
            .map_err(|error| AppError::bad_request(error.to_string()))
        })
        .transpose()?;

    Ok(ApiRequestMetadata::new(TransportProtocol::Http)
        .with_authorization(authorization)
        .with_client_ip(client_ip)
        .with_socket_ip(direct_peer_ip)
        .with_user_agent(user_agent)
        .with_endpoint_scope(Some(EndpointRateLimitScope::Realtime))
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

/// Extract user identity for the WebSocket handshake using explicit request execution.
async fn extract_handshake_auth(
    state: &AppState,
    request_meta: &ApiRequestMetadata,
    query: &WsQuery,
    room_id: &synctv_core::models::RoomId,
    handshake_control: &ExecutionControl,
) -> Result<HandshakeAuthContext, AppError> {
    if request_meta.authorization.is_some() && !query.ticket.is_empty() {
        return Err(AppError::bad_request(
            "Use exactly one WebSocket authentication method",
        ));
    }

    if let Some(authorization) = request_meta.authorization.as_deref() {
        validate_websocket_authorization_header(Some(authorization))?;
        let token = JwtValidator::extract_bearer_token(authorization)
            .map_err(|_| AppError::invalid_authorization_header())?;
        if synctv_core::service::JwtService::token_type_hint(&token)
            == Some(synctv_core::service::TokenType::Guest)
        {
            let public_room_id = state
                .shared_api_runtime
                .public_id_codec
                .encode_room_id(*room_id)
                .map_err(AppError::bad_request)?;
            return state
                .shared_api_runtime
                .request_executor
                .execute_public_with_control(
                    request_meta,
                    EndpointRateLimitCategory::WebSocket,
                    move |_request_control| async move {
                        let access = state
                            .shared_api_runtime
                            .client_api
                            .validate_guest_room_access(&token, &public_room_id)
                            .await?;
                        let identity = GuestRealtimeIdentity {
                            guest_id: access.guest_id,
                            display_name: access.display_name,
                            session_id: access.session_id,
                            token_jti: access.token_jti,
                            room_guest_version: access.room_guest_version,
                            permissions: access.permissions,
                        };
                        let principal =
                            RealtimePrincipal::guest(*room_id, identity).map_err(|error| {
                                synctv_api_common::impls::ApiError::Internal(error.to_string())
                            })?;
                        Ok(HandshakeAuthContext {
                            user_id: principal.connection_user_id(),
                            principal,
                            ticket_commit: None,
                        })
                    },
                )
                .await
                .map_err(crate::http::error::map_api_error);
        }
        return state
            .shared_api_runtime
            .request_executor
            .execute_user_with_control(
                request_meta,
                EndpointRateLimitCategory::WebSocket,
                |_request_control, authenticated| async move {
                    Ok(HandshakeAuthContext {
                        user_id: authenticated.user_id,
                        principal: RealtimePrincipal::user(authenticated.user_id, String::new()),
                        ticket_commit: None,
                    })
                },
            )
            .await
            .map_err(crate::http::error::map_api_error);
    }

    state
        .shared_api_runtime
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
                    .map_err(map_websocket_ticket_validation_api_error)?;

                let principal = match &pending {
                    PendingValidatedTicket::User { user_id, .. } => {
                        Ok(RealtimePrincipal::user(*user_id, String::new()))
                    }
                    PendingValidatedTicket::Guest { guest, .. } => {
                        guest_principal_from_ticket(*room_id, guest)
                    }
                }
                .map_err(|error| synctv_api_common::impls::ApiError::Internal(error.to_string()))?;
                Ok(HandshakeAuthContext {
                    user_id: principal.connection_user_id(),
                    principal,
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
    match synctv_core::service::SecurityPipeline::classify_auth_error(&error) {
        AuthErrorCategory::Authentication => AppError::invalid_or_expired_token(),
        AuthErrorCategory::Authorization => {
            crate::http::error::map_auth_authorization_error(&error)
        }
        AuthErrorCategory::Unavailable | AuthErrorCategory::Internal => AppError::from(error),
    }
}

fn map_websocket_ticket_validation_error(error: synctv_core::Error) -> AppError {
    match synctv_core::service::SecurityPipeline::classify_auth_error(&error) {
        AuthErrorCategory::Authentication => AppError::invalid_or_expired_ticket(),
        AuthErrorCategory::Authorization => {
            crate::http::error::map_auth_authorization_error(&error)
        }
        AuthErrorCategory::Unavailable | AuthErrorCategory::Internal => AppError::from(error),
    }
}

fn map_websocket_ticket_validation_api_error(error: synctv_core::Error) -> ApiError {
    match synctv_core::service::SecurityPipeline::classify_auth_error(&error) {
        AuthErrorCategory::Authentication => {
            ApiError::Authentication("Invalid or expired ticket".to_string())
        }
        AuthErrorCategory::Authorization
        | AuthErrorCategory::Unavailable
        | AuthErrorCategory::Internal => ApiError::from(error),
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
    server_config: &synctv_api_common::ApiServerSettings,
) -> Result<(), AppError> {
    super::reject_duplicate_header(headers, &header::ORIGIN)?;
    super::reject_duplicate_header(headers, &header::HOST)?;
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

    if let Some(host_header) = headers.get(header::HOST) {
        let host = host_header
            .to_str()
            .map_err(|_| AppError::forbidden("Invalid Host header: non-UTF-8 value"))?;
        let forwarded_proto = match direct_peer_ip {
            Some(peer_ip) if server_config.is_trusted_proxy(&peer_ip) => {
                let header_name = header::HeaderName::from_static("x-forwarded-proto");
                super::reject_duplicate_header(headers, &header_name)?;
                headers
                    .get(&header_name)
                    .map(|value| {
                        value.to_str().map_err(|_| {
                            AppError::forbidden("Invalid x-forwarded-proto header: non-UTF-8 value")
                        })
                    })
                    .transpose()?
            }
            _ => None,
        };
        if same_origin_as_host(&parsed_origin, host, forwarded_proto)? {
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

fn same_origin_as_host(
    origin: &url::Url,
    host_header: &str,
    forwarded_proto: Option<&str>,
) -> Result<bool, AppError> {
    let Some(origin_host) = origin.host_str() else {
        return Ok(false);
    };

    let (request_host, request_port) = split_host_and_port(host_header)?;
    if !origin_host.eq_ignore_ascii_case(request_host) {
        return Ok(false);
    }

    if let Some(request_scheme) = forwarded_proto {
        if !origin.scheme().eq_ignore_ascii_case(request_scheme) {
            return Ok(false);
        }
    }

    Ok(origin.port_or_known_default()
        == request_port.or_else(|| default_port_for_scheme(origin.scheme())))
}

fn split_host_and_port(host_header: &str) -> Result<(&str, Option<u16>), AppError> {
    if let Some(stripped) = host_header.strip_prefix('[') {
        if let Some(end) = stripped.find(']') {
            let host = &stripped[..end];
            let remainder = &stripped[end + 1..];
            let port = match remainder.strip_prefix(':') {
                Some(port) => Some(parse_host_port(port)?),
                None if remainder.is_empty() => None,
                _ => return Err(AppError::forbidden("Invalid Host header format")),
            };
            return Ok((host, port));
        }
        return Err(AppError::forbidden("Invalid Host header format"));
    }

    match host_header.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => Ok((host, Some(parse_host_port(port)?))),
        _ => Ok((host_header, None)),
    }
}

fn parse_host_port(port: &str) -> Result<u16, AppError> {
    if port.is_empty() {
        return Err(AppError::forbidden("Invalid Host header port"));
    }
    port.parse()
        .map_err(|_| AppError::forbidden("Invalid Host header port"))
}

fn default_port_for_scheme(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    }
}

pub(crate) fn websocket_runtime_dependencies_available(state: &AppState) -> bool {
    state.chat_service.is_some()
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
                user_id = %user_id,
                error = %error,
                "WebSocket handshake rejected: failed to load username"
            );
            AppError::service_unavailable()
        })?
        .ok_or_else(|| {
            warn!(
                user_id = %user_id,
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
    format: RealtimeTransportFormat,
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
                    if self.format == RealtimeTransportFormat::Protobuf {
                        return Some(ProtoCodec::decode_client_message(&bytes));
                    }
                }
                Some(Ok(axum::extract::ws::Message::Text(text))) => {
                    if self.format == RealtimeTransportFormat::Json {
                        return Some(decode_client_message_json(&text));
                    }
                }
                Some(Ok(axum::extract::ws::Message::Ping(payload))) => {
                    if let Err(error) = reply_to_websocket_ping(&self.raw_sender, payload).await {
                        return Some(Err(error));
                    }
                }
                Some(Ok(axum::extract::ws::Message::Close(_))) => {
                    return None; // Graceful close
                }
                Some(Err(e)) => return Some(Err(format!("WebSocket error: {e}"))),
                None => return None, // Stream ended
                Some(Ok(_)) => {
                    // Ignore control frames and frames for the other transport format.
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

async fn reply_to_websocket_ping(
    raw_sender: &tokio::sync::mpsc::Sender<axum::extract::ws::Message>,
    payload: axum::body::Bytes,
) -> Result<(), String> {
    raw_sender
        .send(axum::extract::ws::Message::Pong(payload))
        .await
        .map_err(|_| "Failed to reply to WebSocket ping: channel closed".to_string())
}

/// WebSocket message sender implementation
struct WebSocketMessageSender {
    normal_sender: tokio::sync::mpsc::Sender<axum::extract::ws::Message>,
    critical_sender: tokio::sync::mpsc::Sender<axum::extract::ws::Message>,
    format: RealtimeTransportFormat,
    /// Count of consecutive message drops (channel full). When this exceeds
    /// `SLOW_CLIENT_DROP_THRESHOLD` the `send()` method returns an error to trigger
    /// a graceful disconnect for the slow client.
    consecutive_drops: Arc<AtomicU32>,
}

impl WebSocketMessageSender {
    fn new(
        normal_sender: tokio::sync::mpsc::Sender<axum::extract::ws::Message>,
        critical_sender: tokio::sync::mpsc::Sender<axum::extract::ws::Message>,
        format: RealtimeTransportFormat,
    ) -> Self {
        Self {
            normal_sender,
            critical_sender,
            format,
            consecutive_drops: Arc::new(AtomicU32::new(0)),
        }
    }

    /// Clone the sender sharing the same drop counter (used to give handler and ping
    /// channel different senders that still track slowness jointly).
    fn clone_sender(&self) -> Self {
        Self {
            normal_sender: self.normal_sender.clone(),
            critical_sender: self.critical_sender.clone(),
            format: self.format,
            consecutive_drops: Arc::clone(&self.consecutive_drops),
        }
    }

    fn encode_message(
        &self,
        message: &ServerMessage,
    ) -> Result<axum::extract::ws::Message, String> {
        match self.format {
            RealtimeTransportFormat::Json => encode_server_message_json(message)
                .map(Into::into)
                .map(axum::extract::ws::Message::Text)
                .map_err(|e| format!("Failed to encode JSON message: {e}")),
            RealtimeTransportFormat::Protobuf => ProtoCodec::encode_server_message(message)
                .map(Into::into)
                .map(axum::extract::ws::Message::Binary),
        }
    }
}

fn decode_client_message_json(text: &str) -> Result<ClientMessage, String> {
    let descriptor = ClientMessage::default().descriptor();
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let dynamic = DynamicMessage::deserialize(descriptor, &mut deserializer)
        .map_err(|e| format!("Failed to decode JSON message: {e}"))?;
    deserializer
        .end()
        .map_err(|e| format!("Failed to decode JSON message: {e}"))?;
    dynamic
        .transcode_to::<ClientMessage>()
        .map_err(|e| format!("Failed to decode JSON message: {e}"))
}

fn encode_server_message_json(message: &ServerMessage) -> Result<String, serde_json::Error> {
    serde_json::to_string(message)
}

async fn forward_websocket_messages<S>(
    mut critical_messages: tokio::sync::mpsc::Receiver<axum::extract::ws::Message>,
    mut outbound_messages: tokio::sync::mpsc::Receiver<axum::extract::ws::Message>,
    mut ws_sender_sink: S,
    is_alive: Arc<std::sync::atomic::AtomicBool>,
    connection_service: Arc<dyn ConnectionRuntime>,
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
/// MUST be delivered (playback state changes, room kick or platform ban notifications, room
/// deletion). Critical messages use a blocking send with timeout so they are
/// not silently dropped.
const fn is_critical_message(message: &ServerMessage) -> bool {
    use synctv_proto::client::server_message::Message;
    matches!(
        &message.message,
        Some(
            Message::Error(_)
                | Message::ResourceObserved(_)
                | Message::ResourceEvent(_)
                | Message::ResourceObserveError(_)
        )
    )
}

const fn requires_state_resync(message: &ServerMessage) -> bool {
    use synctv_proto::client::server_message::Message;
    matches!(&message.message, Some(Message::Notification(_)))
}

/// Returns a human-readable message type name for logging purposes.
const fn message_type_name(message: &ServerMessage) -> &'static str {
    use synctv_proto::client::server_message::Message;
    match &message.message {
        Some(Message::HeartbeatAck(_)) => "HeartbeatAck",
        Some(Message::Error(_)) => "Error",
        Some(Message::ResourceObserved(_)) => "ResourceObserved",
        Some(Message::ResourceEvent(_)) => "ResourceEvent",
        Some(Message::ResourceObserveError(_)) => "ResourceObserveError",
        Some(Message::Notification(_)) => "Notification",
        None => "None",
    }
}

impl synctv_api_common::impls::messaging::MessageSender for WebSocketMessageSender {
    fn send(&self, message: ServerMessage) -> Result<(), String> {
        let ws_msg = self.encode_message(&message)?;

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
/// - Native clients: `ws://host/ws/rooms/{roomId}` with `Authorization: Bearer <token>`
/// - Browser clients: `ws://host/ws/rooms/{roomId}?ticket=<ticket>` (obtained from POST /api/tickets)
#[cfg(feature = "openapi")]
#[utoipa::path(
        get,
        path = "/ws/rooms/{roomId}",
        tag = "WebSocket",
        operation_id = "connectRoomWebSocket",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("ticket" = Option<String>, Query, description = "Short-lived one-time ticket returned by POST /api/tickets. Optional when the Authorization header is provided."),
            ("Authorization" = Option<String>, Header, description = "Bearer access token in the form `Bearer <jwt>`. Optional when the ticket query parameter is provided."),
            ("Origin" = Option<String>, Header, description = "Browser origin header. When WebSocket origin checks are enabled, this header must match an allowed origin.")
        ),
        responses(
            (status = 101, description = "Switching Protocols. The HTTP connection is upgraded to a WebSocket stream after authentication, room membership, origin, and runtime checks pass."),
            (status = 400, description = "Invalid room_id or ticket format", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Missing authentication, invalid or expired token, or invalid or expired ticket", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Origin rejected, room banned, or caller is not allowed to connect to the room", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Room not found", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 408, description = "WebSocket handshake timed out", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 429, description = "Rate limited or connection limit exceeded", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Realtime runtime or ticket backend unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        )
)]
pub(crate) const fn websocket_room_connect_doc() {}

#[cfg(feature = "openapi")]
const _: fn() = websocket_room_connect_doc;

pub async fn websocket_handler(
    State(state): State<AppState>,
    _runtime_ready: WebSocketRuntimeReady,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Query(query): Query<WsQuery>,
    headers: HeaderMap,
    peer_ip: OptionalPeerIp,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, AppError> {
    let room_id = path.room_id;
    let transport_format = RealtimeTransportFormat::parse(query.format.as_deref())?;
    let request_meta =
        websocket_request_metadata(state.runtime_settings.as_ref(), &headers, peer_ip.0)?;
    let handshake_control = ExecutionControl::from_timeout(request_meta.timeout);

    let prepared = run_websocket_handshake_with_timeout(async {
        let prepared = prepare_websocket_upgrade(
            &state,
            &room_id,
            &query,
            &headers,
            peer_ip.0,
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
        .on_upgrade(move |socket| handle_socket(socket, state, prepared, transport_format)))
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
    connection_service: Arc<dyn ConnectionRuntime>,
    reservation: HandshakeReservation,
    armed: bool,
}

impl ReservationCleanupGuard {
    const fn new(
        connection_service: Arc<dyn ConnectionRuntime>,
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
    fn release(&self, connection_service: &dyn ConnectionRuntime) {
        connection_service.release_room_reservation(&self.room_id);
        connection_service.release_user_reservation(&self.user_id);
    }
}

fn reserve_websocket_upgrade_slots(
    connection_service: &dyn ConnectionRuntime,
    room_id: &RoomId,
    user_id: &UserId,
) -> Result<HandshakeReservation, AppError> {
    connection_service
        .reserve_user_slot(user_id)
        .map_err(synctv_api_common::runtime::RealtimeAdmissionError::from_runtime_message)
        .map_err(RealtimeJoinError::from)
        .map_err(map_websocket_pre_join_error)?;

    if let Err(error) = connection_service
        .reserve_room_slot(room_id)
        .map_err(synctv_api_common::runtime::RealtimeAdmissionError::from_runtime_message)
        .map_err(RealtimeJoinError::from)
        .map_err(map_websocket_pre_join_error)
    {
        connection_service.release_user_reservation(user_id);
        return Err(error);
    }

    Ok(HandshakeReservation {
        room_id: *room_id,
        user_id: *user_id,
    })
}

async fn prepare_websocket_upgrade(
    state: &AppState,
    room_id: &str,
    query: &WsQuery,
    headers: &HeaderMap,
    direct_peer_ip: Option<std::net::IpAddr>,
    request_meta: &ApiRequestMetadata,
    handshake_control: &ExecutionControl,
) -> Result<PreparedWebSocketUpgrade, AppError> {
    synctv_api_common::impls::validation::validate_websocket_connect_request(
        &websocket_connect_request(query),
    )
    .map_err(crate::http::error::map_api_error)?;

    validate_websocket_origin(
        headers,
        &state.runtime_settings.server.cors_allowed_origins,
        direct_peer_ip,
        &state.runtime_settings.server,
    )?;

    let rid = state
        .shared_api_runtime
        .public_id_codec
        .decode_room_id(room_id)
        .map_err(|error| AppError::bad_request(format!("Invalid room_id: {error}")))?;

    let mut auth =
        extract_handshake_auth(state, request_meta, query, &rid, handshake_control).await?;
    let user_id = auth.user_id;

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

    if !matches!(auth.principal, RealtimePrincipal::Guest { .. }) {
        validate_websocket_room_membership(&state.room_service, &room, &user_id).await?;
    }

    validate_websocket_runtime_dependencies(state)?;
    let username = if matches!(auth.principal, RealtimePrincipal::Guest { .. }) {
        auth.principal.username().to_string()
    } else {
        let username = load_websocket_username(state, &user_id).await?;
        auth.principal = RealtimePrincipal::user(user_id, username.clone());
        username
    };
    let connection_id = StreamMessageHandler::generate_connection_id();
    let reservation =
        reserve_websocket_upgrade_slots(state.connection_manager.as_ref(), &rid, &user_id)?;

    Ok(PreparedWebSocketUpgrade {
        room_id: rid,
        auth,
        username,
        connection_id: connection_id.into_string(),
        reservation,
    })
}

fn build_failed_upgrade_cleanup(
    connection_service: Arc<dyn ConnectionRuntime>,
    reservation: HandshakeReservation,
) -> impl FnOnce(axum::Error) + Send + 'static {
    move |error| {
        warn!(
            room_id = %reservation.room_id,
            user_id = %reservation.user_id,
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
    error.log_if_internal("websocket_pre_join");
    AppError::from(ApiError::from(error))
}

async fn handle_socket(
    socket: axum::extract::ws::WebSocket,
    state: AppState,
    prepared: PreparedWebSocketUpgrade,
    transport_format: RealtimeTransportFormat,
) {
    let PreparedWebSocketUpgrade {
        room_id,
        auth,
        username: _username,
        connection_id,
        reservation,
    } = prepared;
    let user_id = auth.user_id;
    let principal = auth.principal.clone();
    let mut reservation_cleanup =
        ReservationCleanupGuard::new(state.connection_manager.clone(), reservation.clone());

    info!(
        "WebSocket connection established: user={}, room={}",
        user_id, room_id
    );

    let event_service = state.event_service.clone();

    let _metrics_guard = MetricsGuard::new();

    // Use the shared rate limiter from app state
    let rate_limiter = state.rate_limiter.clone();
    let rate_limit_config = state.shared_api_runtime.messaging_rate_limit_config.clone();
    let content_filter = websocket_content_filter(&state.shared_api_runtime.content_filter);

    // Separate outbound channels keep critical state/control messages from being
    // starved behind a backlog of best-effort traffic.
    let (critical_tx, critical_rx) = tokio::sync::mpsc::channel::<axum::extract::ws::Message>(64);
    let (tx, rx) = tokio::sync::mpsc::channel::<axum::extract::ws::Message>(1000);
    let is_alive = Arc::new(std::sync::atomic::AtomicBool::new(true));

    // Create WebSocket sender - wrapped in Arc for sharing with handler.
    // All senders share the same consecutive-drop counter via clone_sender().
    let ws_sender_primary =
        WebSocketMessageSender::new(tx.clone(), critical_tx.clone(), transport_format);
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
    let stream_handler = StreamMessageHandler::new_with_runtime(
        StreamMessageHandlerConfig {
            room_id,
            principal,
            connection_id: Some(connection_id.clone()),
            room_service: state.room_service.clone(),
            chat_service,
            event_service: event_service.clone(),
            connection_service: state.connection_manager.clone(),
            rate_limiter,
            rate_limit_config,
            content_filter,
            public_id_codec: state.shared_api_runtime.public_id_codec.clone(),
            sender: ws_sender_for_handler,
            concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
        },
        StreamMessageHandlerRuntime {
            clock: state.shared_api_runtime.client_api.clock.clone(),
            playback_service: state.shared_api_runtime.client_api.clone(),
            playlist_items_snapshot_service: state.shared_api_runtime.client_api.clone(),
            room_members_snapshot_service: state.shared_api_runtime.client_api.clone(),
            room_settings_snapshot_service:
                synctv_api_common::impls::room_settings_snapshot::default_room_settings_snapshot_service(
                    state.room_service.clone(),
                ),
            playback_fanout: state.shared_api_runtime.client_api.playback_fanout.clone(),
            chat_event_dispatcher: synctv_api_common::chat_event_dispatcher::default_chat_event_dispatcher(
                event_service.clone(),
            ),
            presence_service: state.presence_service.clone(),
            notification_service: state.notification_service.clone(),
            ws_message_rate_limit: state
                .runtime_settings
                .connection_limits
                .ws_message_rate_limit_per_second,
            heartbeat_schedule: state.shared_api_runtime.heartbeat_schedule,
            filter_private_ice_candidates: state
                .runtime_settings
                .webrtc
                .filter_private_ice_candidates,
            swarm_signing_key: state.shared_api_runtime.media_swarm_signing_key.clone(),
            media_swarm_tracker: state
                .shared_api_runtime
                .client_api
                .media_swarm_tracker
                .clone(),
            runtime_settings_store: state.runtime_settings_store.clone(),
        },
    );
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
            format: transport_format,
            is_alive,
            raw_sender: raw_sender_for_ping,
        };

        loop {
            tokio::select! {
                () = input_cancel_token.cancelled() => {
                    if let Err(error) = close_sender_on_cancel
                        .try_send(axum::extract::ws::Message::Close(None))
                    {
                        warn!(
                            error = %error,
                            "Failed to enqueue WebSocket close frame after cancellation"
                        );
                    }
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
        user_id, room_id
    );
}

#[cfg(test)]
mod tests;
