//! HTTP realtime sessions: SSE downlink + REST uplink.
//!
//! A realtime session is the multi-channel counterpart of the WebSocket
//! connection: the same transport-agnostic `StreamMessageHandler` (admission,
//! presence, observations with cursors, WebRTC signaling) drives it, but its
//! lifetime is owned by a **lease** instead of a socket.
//!
//! - `POST   /api/rooms/{roomId}/realtime/sessions` creates the session and
//!   establishes initial observations;
//! - `GET    /api/rooms/{roomId}/realtime/sessions/{sessionId}/events`
//!   attaches the SSE downlink carrying every `ServerMessage` the session
//!   emits (multiplexed per-observation events, heartbeats, errors,
//!   terminations);
//! - `POST   /api/rooms/{roomId}/realtime/sessions/{sessionId}/messages`
//!   posts any `ClientMessage` (chat, playback, observe/unobserve, WebRTC
//!   signaling, heartbeat) through the same handler as WS/gRPC;
//! - `DELETE /api/rooms/{roomId}/realtime/sessions/{sessionId}` ends it.
//!
//! While detached (client switching networks, SSE reconnecting) the session
//! stays alive for the lease window so presence and signaling continuity
//! survive; messages emitted during the gap are dropped and clients resync by
//! re-sending their observations with cursors — the same semantics as a WS
//! reconnect, without re-running admission.

use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    Json,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};

use crate::http::middleware::RequestMetadata;
use crate::http::room::watch::resolve_realtime_principal;
use crate::http::websocket::{
    build_room_stream_handler, reserve_websocket_upgrade_slots,
    validate_websocket_runtime_dependencies, RealtimeTransportFormat, ReservationCleanupGuard,
};
use crate::http::{AppError, AppResult, AppState};
use synctv_api_common::impls::messaging::{
    FairQueueConfig, FairSessionSender, MessageSender, RealtimePrincipal,
};
use synctv_api_common::impls::realtime_sessions::SessionLease as SharedSessionLease;
use synctv_api_common::impls::{
    EndpointRateLimitCategory, ManagedRealtimeSession, REALTIME_SESSION_LEASE_TTL,
};
use synctv_core::models::RoomId;
use synctv_proto::client::{
    CreateRealtimeSessionRequest, CreateRealtimeSessionResponse, SendRealtimeSessionMessageRequest,
    ServerMessage,
};

/// Maximum initial observations accepted at session creation (the observer
/// itself caps concurrent observations per connection).
const MAX_INITIAL_OBSERVATIONS: usize = 64;

/// Per-subscriber SSE buffer. Overflow drops messages for that subscriber
/// (the client resyncs from cursors); the upstream fair queue only sees a
/// closed subscriber when the stream goes away entirely.
const SESSION_EVENT_BUFFER: usize = 128;

static SUBSCRIBER_IDS: AtomicU64 = AtomicU64::new(0);

/// Identity key for authorizing access to an existing session: the resolved
/// principal must match the session owner.
fn principal_owner_key(principal: &RealtimePrincipal) -> String {
    match principal.user_id() {
        Some(user_id) => format!("user:{user_id}"),
        // Guest display names share a 6-character prefix ("Guest a1b2c3"),
        // so keying on them would let one guest into another's session; the
        // full session id is unambiguous.
        None => match principal.guest_identity() {
            Some(identity) => format!("guest:{}", identity.session_id),
            None => format!("guest:{}", principal.username()),
        },
    }
}

/// Live HTTP realtime session.
struct HttpRealtimeSession {
    room_id: RoomId,
    owner_key: String,
    /// Feeds the handler's inbound pump (POST bodies).
    incoming_tx: tokio::sync::mpsc::Sender<synctv_proto::client::ClientMessage>,
    /// Fair-queue drain output, fanned out to the attached SSE subscriber.
    outbound: SessionTap,
    cancel_token: tokio_util::sync::CancellationToken,
    /// True while an SSE stream is attached: attachment suspends lease expiry
    /// because connection-level liveness replaces it.
    attached: Arc<std::sync::atomic::AtomicBool>,
}

impl HttpRealtimeSession {
    async fn post_message(
        &self,
        message: synctv_proto::client::ClientMessage,
    ) -> Result<(), AppError> {
        self.outbound.refresh_lease();
        self.incoming_tx.send(message).await.map_err(|_| {
            AppError::new(
                StatusCode::GONE,
                "Realtime session is no longer accepting messages",
            )
        })
    }

    fn matches(&self, room_id: &RoomId, principal: &RealtimePrincipal) -> bool {
        &self.room_id == room_id && self.owner_key == principal_owner_key(principal)
    }
}

impl ManagedRealtimeSession for HttpRealtimeSession {
    fn refresh_lease(&self) {
        self.outbound.refresh_lease();
    }

    fn lease_expired(&self) -> bool {
        self.cancel_token.is_cancelled()
            || (!self.attached.load(Ordering::Relaxed) && self.outbound.lease_expired())
    }

    fn cancel(&self) {
        self.cancel_token.cancel();
    }

    fn into_any_arc(self: Arc<Self>) -> Arc<dyn std::any::Any + Send + Sync> {
        self
    }
}

/// Result of handing one message to the tap.
#[derive(Debug, PartialEq, Eq)]
enum TapOutcome {
    /// Delivered to the subscriber (or buffered for the first attach).
    Delivered,
    /// A termination/error was lost and the subscriber can never be told:
    /// the caller must cancel the session so the client reconnects.
    LostCritical,
    /// A routine event was dropped; the client resyncs from cursors.
    LostTolerated,
}

/// True for messages whose loss must end the session: the client cannot
/// recover from a missed termination or session error via cursor resync.
fn is_critical_message(message: &ServerMessage) -> bool {
    use synctv_proto::client::server_message::Message;
    matches!(
        message.message.as_ref(),
        Some(Message::Termination(_) | Message::Error(_))
    )
}

/// Fans the fair queue's scheduled output to the currently attached SSE
/// subscriber. Until the first attach, messages are buffered (bounded,
/// drop-oldest) so the initial-observation replies — which race the client's
/// `GET .../events` — survive the create→attach gap and replay into the first
/// subscriber. After the first attach, drops follow the detach semantics: the
/// client is away or slow and resyncs via re-observed cursors on re-attach,
/// exactly like a WS reconnect.
struct SessionTap {
    subscriber: parking_lot::Mutex<Option<(u64, tokio::sync::mpsc::Sender<ServerMessage>)>>,
    lease: SharedSessionLease,
    /// Pre-attach buffer, drained into the first subscriber.
    pending: parking_lot::Mutex<std::collections::VecDeque<ServerMessage>>,
    /// Latches on the first attach; later detach gaps resync instead of
    /// buffering.
    ever_attached: std::sync::atomic::AtomicBool,
}

impl SessionTap {
    /// Installs a subscriber and marks the session attached. The `attached`
    /// flag update happens under the subscriber lock so it serializes with
    /// [`Self::detach`]: a replaced stream's drop callback can never mark a
    /// live replacement detached.
    fn attach(
        &self,
        attached: &std::sync::atomic::AtomicBool,
    ) -> (u64, tokio::sync::mpsc::Receiver<ServerMessage>) {
        let id = SUBSCRIBER_IDS.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = tokio::sync::mpsc::channel(SESSION_EVENT_BUFFER);
        // Hold both locks across the transition so a concurrent `forward`
        // cannot slip between "still buffering" and "subscriber installed".
        let mut subscriber = self.subscriber.lock();
        let mut pending = self.pending.lock();
        for message in pending.drain(..) {
            // Replay overflow means the client is already too slow to drain
            // its initial sync; it resyncs from cursors on later events.
            if tx.try_send(message).is_err() {
                break;
            }
        }
        self.ever_attached
            .store(true, std::sync::atomic::Ordering::Release);
        *subscriber = Some((id, tx));
        attached.store(true, Ordering::Release);
        drop(pending);
        drop(subscriber);
        self.lease.refresh();
        (id, rx)
    }

    /// Removes the subscriber with `id` when it is still the active one and,
    /// only then, marks the session detached and re-arms its lease — a stale
    /// drop callback from a replaced stream must not expire a live successor.
    fn detach(&self, id: u64, attached: &std::sync::atomic::AtomicBool) {
        let mut subscriber = self.subscriber.lock();
        if subscriber.as_ref().is_some_and(|(sub_id, _)| *sub_id == id) {
            *subscriber = None;
            attached.store(false, Ordering::Release);
            drop(subscriber);
            // Re-arm the lease from now so brief reconnects survive.
            self.lease.refresh();
        }
    }

    /// Ends the downlink: the attached subscriber (if any) sees the stream
    /// end after the final queued messages instead of open-ended keepalives.
    fn close(&self) {
        *self.subscriber.lock() = None;
        self.pending.lock().clear();
    }

    fn forward(&self, message: ServerMessage) -> TapOutcome {
        if !self
            .ever_attached
            .load(std::sync::atomic::Ordering::Acquire)
        {
            let mut pending = self.pending.lock();
            // Re-check under the lock: an attach that completed while this
            // task waited on the mutex must not leave the message orphaned
            // in the drained buffer.
            if !self
                .ever_attached
                .load(std::sync::atomic::Ordering::Acquire)
            {
                let evicted = if pending.len() >= SESSION_EVENT_BUFFER {
                    pending.pop_front()
                } else {
                    None
                };
                pending.push_back(message);
                return if evicted.as_ref().is_some_and(is_critical_message) {
                    TapOutcome::LostCritical
                } else {
                    TapOutcome::Delivered
                };
            }
        }
        let critical = is_critical_message(&message);
        let mut subscriber = self.subscriber.lock();
        match subscriber.as_mut() {
            Some((_, tx)) => {
                // A full buffer drops for this subscriber; the client
                // resyncs from cursors. A closed sender detaches lazily.
                if tx.try_send(message).is_err() {
                    if tx.is_closed() {
                        *subscriber = None;
                    }
                    if critical {
                        TapOutcome::LostCritical
                    } else {
                        TapOutcome::LostTolerated
                    }
                } else {
                    TapOutcome::Delivered
                }
            }
            // Detached: drop. Lease expiry (or re-attach) follows; a
            // critical message still ends the session so the client's next
            // attach 404s instead of parking on a dead session.
            None if critical => TapOutcome::LostCritical,
            None => TapOutcome::LostTolerated,
        }
    }

    fn refresh_lease(&self) {
        self.lease.refresh();
    }

    fn lease_expired(&self) -> bool {
        self.lease.expired()
    }
}

/// Encodes a full `ServerMessage` as one uniform SSE event: every session
/// message uses the `message` event with the complete envelope as data
/// (proto3 JSON, or base64-encoded protobuf), so clients decode the
/// downlink with the same decoder as the WebSocket and WebTransport
/// transports. Resource events additionally carry the cursor as the SSE
/// event id for diagnostics.
fn session_sse_event(
    format: RealtimeTransportFormat,
    message: &ServerMessage,
) -> Option<Result<Event, Infallible>> {
    use synctv_proto::client::server_message::Message;

    let event_id = match message.message.as_ref()? {
        Message::ResourceEvent(changed) => {
            crate::http::room::watch::sse_event_id_from_resource_event(changed)
        }
        _ => None,
    };
    let data = match encode_sse_payload(format, message) {
        Ok(data) => data,
        Err(error) => {
            tracing::warn!(error = %error, "Failed to serialize realtime session SSE event");
            return Some(Ok(Event::default().event("error").data(
                r#"{"message":"Failed to serialize realtime session event"}"#,
            )));
        }
    };
    let mut event = Event::default().event("message").data(data);
    if let Some(event_id) = event_id {
        event = event.id(event_id);
    }
    Some(Ok(event))
}

fn encode_sse_payload<M>(
    format: RealtimeTransportFormat,
    message: &M,
) -> Result<String, serde_json::Error>
where
    M: prost::Message + serde::Serialize,
{
    match format {
        RealtimeTransportFormat::Json => serde_json::to_string(message),
        RealtimeTransportFormat::Protobuf => Ok(BASE64_STANDARD.encode(message.encode_to_vec())),
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct SessionEventsQuery {
    #[serde(default)]
    format: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RealtimeSessionPath {
    pub room_id: String,
    pub session_id: String,
}

fn decode_room_id(state: &AppState, public_room_id: &str) -> AppResult<RoomId> {
    state
        .shared_api_runtime
        .public_id_codec
        .decode_room_id(public_room_id)
        .map_err(|error| AppError::bad_request(format!("Invalid room_id: {error}")))
}

/// Creates a realtime session: admission (room checks, membership, connection
/// limits) runs exactly as for a WebSocket upgrade, then the shared handler
/// joins the room and the initial observations are pushed.
pub async fn create_realtime_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(request): Json<CreateRealtimeSessionRequest>,
) -> AppResult<Json<CreateRealtimeSessionResponse>> {
    validate_websocket_runtime_dependencies(&state)?;

    let room_id = decode_room_id(&state, &path.room_id)?;
    let principal = resolve_realtime_principal(
        &state,
        request_meta,
        path.room_id.clone(),
        EndpointRateLimitCategory::WebSocket,
    )
    .await?;

    let room = state
        .room_service
        .get_room(&room_id)
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

    if request.initial_observations.len() > MAX_INITIAL_OBSERVATIONS {
        return Err(AppError::bad_request(format!(
            "Too many initial observations: {} > {MAX_INITIAL_OBSERVATIONS}",
            request.initial_observations.len()
        )));
    }
    for observation in &request.initial_observations {
        synctv_api_common::impls::validate_proto_request(observation)
            .map_err(crate::http::error::map_api_error)?;
    }

    let actor = principal
        .realtime_actor(&state.shared_api_runtime.public_id_codec)
        .map_err(AppError::bad_request)?;
    let reservation =
        reserve_websocket_upgrade_slots(state.connection_manager.as_ref(), &room_id, &actor)?;
    let mut reservation_cleanup =
        ReservationCleanupGuard::new(state.connection_manager.clone(), reservation.clone());

    // Outbound: fair queue -> tap -> (optional) SSE subscriber.
    let (fair_sender, fair_rx) = FairSessionSender::new(FairQueueConfig::default());
    let handler = build_room_stream_handler(
        &state,
        room_id,
        principal.clone(),
        None,
        Arc::new(fair_sender.clone()) as Arc<dyn MessageSender>,
    )?;
    let session_id = handler.connection_id().to_string();

    let (incoming_tx, cancel_token) = handler.start().await.map_err(|error| {
        tracing::error!(error = %error, "HTTP realtime session failed to join the room");
        AppError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to join realtime session: {error}"),
        )
    })?;

    reservation_cleanup.disarm();
    reservation.release(state.connection_manager.as_ref());

    let session_lease = SharedSessionLease::new();
    let session = Arc::new(HttpRealtimeSession {
        room_id,
        owner_key: principal_owner_key(&principal),
        incoming_tx,
        outbound: SessionTap {
            subscriber: parking_lot::Mutex::new(None),
            lease: session_lease.clone(),
            pending: parking_lot::Mutex::new(std::collections::VecDeque::new()),
            ever_attached: std::sync::atomic::AtomicBool::new(false),
        },
        cancel_token: cancel_token.clone(),
        attached: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    });

    // Drain the fair queue into the tap until the handler stops; the session
    // ends with it. Cancellation first closes the fair sender so everything
    // the session already queued (terminations above all) is forwarded into
    // the tap before the pump ends.
    let pump_session = Arc::clone(&session);
    tokio::spawn(async move {
        let mut fair_rx = fair_rx;
        let mut closing = false;
        loop {
            tokio::select! {
                biased;
                () = pump_session.cancel_token.cancelled(), if !closing => {
                    // Everything already queued is flushed by the forwarder,
                    // which then closes the channel — `recv()` returning None
                    // is the authoritative drain-done signal. (An is_empty()
                    // check here would race the forwarder's final hand-off
                    // and could cut the drain short.)
                    fair_sender.close();
                    closing = true;
                }
                message = fair_rx.recv() => {
                    let Some(message) = message else { break };
                    if pump_session.outbound.forward(message)
                        == TapOutcome::LostCritical
                    {
                        // The subscriber can never be told the session ended;
                        // cancel so the registry reaps it and the client
                        // reconnects instead of parking on a dead session.
                        pump_session.cancel_token.cancel();
                    }
                }
            }
        }
        pump_session.cancel_token.cancel();
        // The handler has stopped: end the SSE downlink too, or an attached
        // client would see open-ended keepalives on a dead session.
        pump_session.outbound.close();
    });

    // Initial observations.
    for observation in request.initial_observations {
        let message = synctv_proto::client::ClientMessage {
            message: Some(
                synctv_proto::client::client_message::Message::ObserveResource(observation),
            ),
        };
        if session.post_message(message).await.is_err() {
            break;
        }
    }

    state.realtime_sessions.insert_with_shared_lease(
        session_id.clone(),
        session as Arc<dyn ManagedRealtimeSession>,
        session_lease,
    );

    tracing::info!(
        session_id = %session_id,
        room_id = %path.room_id,
        "HTTP realtime session created"
    );

    Ok(Json(CreateRealtimeSessionResponse {
        session_id,
        lease_ttl_ms: i64::try_from(REALTIME_SESSION_LEASE_TTL.as_millis())
            .expect("lease ttl fits in i64"),
    }))
}

/// SSE downlink for a session. Attaching suspends lease expiry while the
/// stream is open; detaching re-arms it.
pub async fn realtime_session_events(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<RealtimeSessionPath>,
    Query(query): Query<SessionEventsQuery>,
) -> AppResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let format = RealtimeTransportFormat::parse(query.format.as_deref())?;

    let room_id = decode_room_id(&state, &path.room_id)?;
    let principal = resolve_realtime_principal(
        &state,
        request_meta,
        path.room_id.clone(),
        EndpointRateLimitCategory::WebSocket,
    )
    .await?;

    let session = load_session(&state, &room_id, &path.session_id, &principal)?;

    let (subscriber_id, rx) = session.outbound.attach(&session.attached);

    // The tap's detach is id-guarded and lock-serialized with attach, so a
    // replaced stream's late drop callback cannot mark its successor
    // detached (which would re-arm lease expiry under a live subscriber).
    let detach = {
        let session = Arc::clone(&session);
        move || {
            session.outbound.detach(subscriber_id, &session.attached);
        }
    };

    let stream = DetachOnDropStream {
        on_drop: Some(Box::new(detach)),
        inner: ReceiverStream::new(rx)
            .filter_map(move |message| session_sse_event(format, &message)),
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// Runs the SSE body stream and detaches the session subscriber when the
/// response ends (client disconnect, session termination, or server
/// shutdown).
struct DetachOnDropStream<S> {
    on_drop: Option<Box<dyn FnOnce() + Send>>,
    inner: S,
}

impl<S> Stream for DetachOnDropStream<S>
where
    S: Stream + Unpin,
{
    type Item = S::Item;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::pin::Pin::new(&mut self.inner).poll_next(cx)
    }
}

impl<S> Drop for DetachOnDropStream<S> {
    fn drop(&mut self) {
        if let Some(on_drop) = self.on_drop.take() {
            on_drop();
        }
    }
}

/// Posts a `ClientMessage` into the session (chat, playback, observe,
/// WebRTC signaling, heartbeat). Processed by the same handler logic as the
/// WS/gRPC transports.
pub async fn send_realtime_session_message(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<RealtimeSessionPath>,
    Json(request): Json<SendRealtimeSessionMessageRequest>,
) -> AppResult<StatusCode> {
    let room_id = decode_room_id(&state, &path.room_id)?;
    let principal = resolve_realtime_principal(
        &state,
        request_meta,
        path.room_id.clone(),
        EndpointRateLimitCategory::Read,
    )
    .await?;

    let session = load_session(&state, &room_id, &path.session_id, &principal)?;

    let Some(message) = request.message else {
        return Err(AppError::bad_request("message is required"));
    };
    session.post_message(message).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Gracefully ends a session.
pub async fn delete_realtime_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<RealtimeSessionPath>,
) -> AppResult<StatusCode> {
    let room_id = decode_room_id(&state, &path.room_id)?;
    let principal = resolve_realtime_principal(
        &state,
        request_meta,
        path.room_id.clone(),
        EndpointRateLimitCategory::Read,
    )
    .await?;

    let session = load_session(&state, &room_id, &path.session_id, &principal)?;
    session.cancel_token.cancel();
    state.realtime_sessions.remove(&path.session_id);
    Ok(StatusCode::NO_CONTENT)
}

/// Loads a session after re-resolving the caller's principal; mismatched
/// owners get a 404 so session existence is not leaked across users.
fn load_session(
    state: &AppState,
    room_id: &RoomId,
    session_id: &str,
    principal: &RealtimePrincipal,
) -> AppResult<Arc<HttpRealtimeSession>> {
    let not_found = || AppError::new(StatusCode::NOT_FOUND, "Realtime session not found");
    let session = state
        .realtime_sessions
        .get(session_id)
        .ok_or_else(not_found)?;
    let session =
        Arc::downcast::<HttpRealtimeSession>(session.into_any_arc()).map_err(|_| not_found())?;
    if !session.matches(room_id, principal) {
        return Err(not_found());
    }
    Ok(session)
}
