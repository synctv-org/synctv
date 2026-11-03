//! WebTransport room realtime transport.
//!
//! Serves the room realtime session over a WebTransport session (extended
//! CONNECT under the `webtransport` protocol on the server's HTTP/3
//! listener). The wire format mirrors the binary WebSocket transport —
//! length-prefixed protobuf `ClientMessage`/`ServerMessage` frames — with
//! one bidirectional control stream, scheduled through the same
//! per-observation fair queue and driven by the same `StreamMessageHandler`
//! as WS/gRPC.
//!
//! Clients that announce `streams=1` on the CONNECT — multi-stream
//! revision one — additionally get each observation lane pushed on its own
//! stream opened unidirectionally by the server, so a bulk snapshot's
//! packet loss or backpressure never delays heartbeats, terminations, or
//! unrelated observations. The protocol's only ordering guarantee, FIFO
//! within one `observe_id`, is preserved because every frame of an
//! observation stays on that observation's stream, and control traffic
//! stays on the control stream. Clients that do not announce the revision
//! keep the single-stream layout on the control stream.

use std::collections::HashMap;
use std::sync::Arc;

use axum::http::{Request, StatusCode};
use synctv_quic::{
    read_frame, write_frame, WebTransportHandler, WtSendStream, WtSessionDriver, WtStreamOpener,
};

use crate::http::websocket::{
    build_room_stream_handler, extract_handshake_auth, load_websocket_username,
    reserve_websocket_upgrade_slots, validate_websocket_origin, validate_websocket_room_membership,
    validate_websocket_runtime_dependencies, ReservationCleanupGuard, WsQuery,
};
use crate::http::AppState;
use synctv_api_common::impls::messaging::{
    classify_message_lane, FairQueueConfig, FairSessionSender, MessageLane, MessageSender,
    ProtoCodec,
};
use synctv_api_common::impls::RequestMetadata as ApiRequestMetadata;

use synctv_core::provider::ExecutionControl;

/// Base path of the WebTransport room endpoint (`/wt/rooms/{roomId}`).
pub const WEBTRANSPORT_ROOMS_PATH: &str = "/wt/rooms/";

/// Length of the base path prefix.
const WEBTRANSPORT_ROOMS_PATH_LEN: usize = WEBTRANSPORT_ROOMS_PATH.len();

/// Upper bound on concurrent per-observation push streams. Mirrors the fair
/// queue's lane defense: observations beyond the cap share the control
/// stream instead of refusing them (ordering within each such observation
/// stays consistent because it never changes streams mid-flight).
const MAX_OBSERVATION_STREAMS: usize = 32;

/// Bound on a single WebTransport frame write, mirroring the WebSocket
/// sink-write timeout. A client that stops reading parks the stream once QUIC
/// flow control fills; without a bound one parked lane stalls every other
/// lane (and the session) until the connection dies.
const WT_WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Bound on the post-cancellation drain: queued terminations get this long
/// to reach the wire, then the writer drops the lanes and streams so a peer
/// trickling flow-control credit cannot pin session resources past the
/// session's end.
const WT_DRAIN_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Parses `/wt/rooms/{roomId}` and its query string from the CONNECT target.
fn parse_room_target(uri: &axum::http::Uri) -> Option<(String, WsQuery)> {
    if !uri.path().starts_with(WEBTRANSPORT_ROOMS_PATH) {
        return None;
    }
    let room_id = uri.path()[WEBTRANSPORT_ROOMS_PATH_LEN..].to_string();
    if room_id.is_empty() || room_id.contains('/') {
        return None;
    }
    let query = axum::extract::Query::<WsQuery>::try_from_uri(uri).map_or_default(|query| query.0);
    Some((room_id, query))
}

/// Builds the transport-agnostic request metadata for a WT CONNECT, mirroring
/// the WebSocket handshake metadata: trusted-proxy-aware client IP derived
/// from the QUIC peer address (`PeerAddress` extension inserted by
/// synctv-quic for the CONNECT), Realtime endpoint scope, and the shared
/// handshake timeout, so ticket/guest handshakes rate-limit per peer like WS.
fn wt_request_metadata(
    state: &AppState,
    request: &Request<()>,
) -> Result<ApiRequestMetadata, crate::http::AppError> {
    let peer_ip = request
        .extensions()
        .get::<synctv_quic::PeerAddress>()
        .map(|peer| peer.0.ip());
    crate::http::websocket::websocket_request_metadata(
        &state.runtime_settings,
        request.headers(),
        peer_ip,
    )
}

/// Returns the client's announced multi-stream revision from the CONNECT
/// query (`streams=1`). Parsed off the raw query so an old server's typed
/// query struct never rejects an unknown parameter, and an old client's
/// query stays valid.
fn announced_stream_revision(uri: &axum::http::Uri) -> Option<u32> {
    let query = uri.query()?;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == "streams").then(|| value.parse::<u32>().ok())?
    })
}

/// True when the client announced multi-stream revision 1: per-observation
/// unidirectional push streams plus the control stream.
fn negotiated_multi_stream(uri: &axum::http::Uri) -> bool {
    announced_stream_revision(uri) == Some(1)
}

/// Room realtime sessions over WebTransport.
pub struct RoomWebTransportHandler {
    pub state: AppState,
}

#[async_trait::async_trait]
impl WebTransportHandler for RoomWebTransportHandler {
    /// Cheap admission so obvious rejections (bad path, missing auth
    /// material, undecodable id) answer the CONNECT immediately. Room
    /// existence and banned/closed state are deliberately NOT probed here:
    /// an unauthenticated peer must not learn them from the CONNECT
    /// response. [`Self::run_session`] validates the credential first and
    /// then re-checks the room, tearing the session down for unknown, banned
    /// or closed rooms — the same outcome an invalid credential already
    /// produces.
    async fn accept(&self, request: &Request<()>) -> Result<(), StatusCode> {
        let Some((_public_room_id, query)) = parse_room_target(request.uri()) else {
            return Err(StatusCode::NOT_FOUND);
        };
        let has_auth = request
            .headers()
            .contains_key(axum::http::header::AUTHORIZATION)
            || !query.ticket.is_empty();
        if !has_auth {
            return Err(StatusCode::UNAUTHORIZED);
        }
        Ok(())
    }

    async fn run_session(&self, request: Request<()>, session: WtSessionDriver) {
        if let Err(error) = self.run_session_inner(request, session).await {
            tracing::info!(error = %error, "WebTransport room session ended with error");
        }
    }
}

impl RoomWebTransportHandler {
    async fn run_session_inner(
        &self,
        request: Request<()>,
        session: WtSessionDriver,
    ) -> Result<(), String> {
        let state = &self.state;
        let Some((public_room_id, query)) = parse_room_target(request.uri()) else {
            return Err("invalid WebTransport room path".to_string());
        };
        let room_id = state
            .shared_api_runtime
            .public_id_codec
            .decode_room_id(&public_room_id)
            .map_err(|error| format!("invalid room id: {error}"))?;

        validate_websocket_runtime_dependencies(state).map_err(|error| error.to_string())?;
        validate_websocket_origin(
            request.headers(),
            &state.runtime_settings.server.cors_allowed_origins,
            None,
            &state.runtime_settings.server,
        )
        .map_err(|error| error.to_string())?;

        let request_meta =
            wt_request_metadata(state, &request).map_err(|error| error.to_string())?;
        let handshake_control = ExecutionControl::default();
        let mut auth =
            extract_handshake_auth(state, &request_meta, &query, &room_id, &handshake_control)
                .await
                .map_err(|error| error.to_string())?;

        let room = state
            .room_service
            .get_room(&room_id)
            .await
            .map_err(|error| error.to_string())?;
        if room.is_banned {
            return Err("room banned".to_string());
        }
        if room.status.is_closed() {
            return Err("room closed".to_string());
        }

        if let Some(user_id) = auth.principal.user_id() {
            validate_websocket_room_membership(&state.room_service, &room, &user_id)
                .await
                .map_err(|error| error.to_string())?;
            let username = load_websocket_username(state, &user_id)
                .await
                .map_err(|error| error.to_string())?;
            auth.principal =
                synctv_api_common::impls::messaging::RealtimePrincipal::user(user_id, username);
        }

        let actor = auth
            .principal
            .realtime_actor(&state.shared_api_runtime.public_id_codec)?;
        let reservation =
            reserve_websocket_upgrade_slots(state.connection_manager.as_ref(), &room_id, &actor)
                .map_err(|error| error.to_string())?;
        let mut reservation_cleanup =
            ReservationCleanupGuard::new(state.connection_manager.clone(), reservation.clone());

        // Commit the one-time ticket now that the session is established.
        commit_ticket(state, &room_id, &auth, &handshake_control).await?;

        // Outbound: fair queue -> frame writer.
        let (fair_sender, fair_rx) = FairSessionSender::new(FairQueueConfig::default());
        let handler = build_room_stream_handler(
            state,
            room_id,
            auth.principal.clone(),
            None,
            Arc::new(fair_sender.clone()) as Arc<dyn MessageSender>,
        )
        .map_err(|error| error.to_string())?;

        let connection_id = handler.connection_id().to_string();
        let (incoming_tx, cancel_token) = handler.start().await.map_err(|error| {
            tracing::error!(error = %error, "WebTransport room session failed to join");
            error
        })?;

        // Session end must not strand the fair queue's spawned forwarder:
        // closing the sender hands over any already-queued frames (final
        // terminations above all) and then closes the writer's channel.
        {
            let fair_closer_sender = fair_sender;
            let fair_closer_token = cancel_token.clone();
            tokio::spawn(async move {
                fair_closer_token.cancelled().await;
                fair_closer_sender.close();
            });
        }

        reservation_cleanup.disarm();
        reservation.release(state.connection_manager.as_ref());

        tracing::info!(
            connection_id = %connection_id,
            room_id = %public_room_id,
            "WebTransport room session established"
        );

        let stream_opener = session.opener();
        let (mut send_half, mut recv_half) =
            synctv_quic::h3::quic::BidiStream::split(session.control_stream);
        let multi_stream = negotiated_multi_stream(request.uri());

        // Writer: fair queue -> frames. Control-lane messages always use the
        // control stream; with `streams=1` negotiated, each observation gets
        // its own server-initiated unidirectional stream.
        let writer_cancel = cancel_token.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;

            let mut fair_rx = fair_rx;
            let mut lanes: HashMap<String, WtSendStream> = HashMap::new();
            // Session end must not drop what the session already queued:
            // keep writing until the fair channel closes (guaranteed by the
            // fair-sender closer above) instead of parking mid-drain - but
            // the whole drain is bounded by WT_DRAIN_GRACE, so a peer
            // trickling flow-control credit cannot pace it indefinitely.
            // The channel close — `recv()` returning `None` — is the
            // authoritative drain-done signal; an `is_empty()` check would
            // race the forwarder's final hand-off and could cut the drain
            // short of the queued terminations.
            let mut closing = false;
            let mut drain_deadline: Option<tokio::time::Instant> = None;
            let outcome = loop {
                tokio::select! {
                    biased;
                    () = writer_cancel.cancelled(), if !closing => {
                        closing = true;
                        // Session end bounds the entire drain, not only
                        // each write.
                        drain_deadline =
                            Some(tokio::time::Instant::now() + WT_DRAIN_GRACE);
                    }
                    () = async {
                        match drain_deadline {
                            Some(deadline) => tokio::time::sleep_until(deadline).await,
                            None => std::future::pending::<()>().await,
                        }
                    } => {
                        break Err("post-cancellation drain grace elapsed".to_string());
                    }
                    message = fair_rx.recv() => {
                        let Some(message) = message else { break Ok(()) };
                        let Ok(bytes) = ProtoCodec::encode_server_message(&message) else {
                            continue;
                        };
                        // Each write is individually bounded: a client that
                        // stops reading parks the stream once QUIC flow
                        // control fills, and without a bound one parked lane
                        // stalls every other lane. A timed-out write may have
                        // written a partial frame; the session is cancelled
                        // below either way, so the desynchronized stream
                        // never carries further frames. Post-cancellation the
                        // drain deadline bounds the write too, so an
                        // in-flight write cannot consume the full
                        // WT_WRITE_TIMEOUT after the grace expires.
                        let write_fut: std::pin::Pin<
                            Box<dyn std::future::Future<Output = bool> + Send>,
                        > = match classify_message_lane(&message) {
                            MessageLane::Observation { observe_id, .. } if multi_stream => {
                                Box::pin(write_observation_frame(
                                    &mut lanes,
                                    &stream_opener,
                                    &mut send_half,
                                    observe_id,
                                    &bytes,
                                ))
                            }
                            // Control lane, and every lane in single-stream
                            // mode, share the control stream.
                            _ => Box::pin(async {
                                write_frame(&mut send_half, &bytes).await.is_ok()
                            }),
                        };
                        let written = match drain_deadline {
                            Some(deadline) => tokio::time::timeout_at(deadline, write_fut)
                                .await
                                .unwrap_or(false),
                            None => tokio::time::timeout(WT_WRITE_TIMEOUT, write_fut)
                                .await
                                .unwrap_or(false),
                        };
                        if !written {
                            // A payload over the frame cap fails before any
                            // byte is written (frames.rs write_frame): drop
                            // this one message and keep the session instead
                            // of desyncing the client into a reconnect loop.
                            // Real write failures and timeouts stay fatal.
                            if bytes.len() > synctv_quic::MAX_OUTBOUND_FRAME_BYTES {
                                tracing::warn!(
                                    size = bytes.len(),
                                    "server message exceeds WebTransport frame cap; dropping message"
                                );
                                continue;
                            }
                            break Err("stream write failed or timed out".to_string());
                        }
                    }
                }
            };
            if outcome.is_err() {
                // A failed write desynchronizes the client: end the session
                // so it reconnects and resyncs from its observation cursors.
                writer_cancel.cancel();
            }
            for (_, mut lane) in lanes {
                let _ = lane.shutdown().await;
            }
            let _ = send_half.shutdown().await;
        });

        // Reader: frames -> handler inbound.
        loop {
            tokio::select! {
                () = cancel_token.cancelled() => break,
                frame = read_frame(&mut recv_half) => {
                    match frame {
                        Ok(Some(bytes)) => {
                            match ProtoCodec::decode_client_message(&bytes) {
                                Ok(message) => {
                                    if incoming_tx.send(message).await.is_err() {
                                        break;
                                    }
                                }
                                Err(error) => {
                                    tracing::debug!(
                                        error = %error,
                                        "WebTransport frame decode failed; ending session"
                                    );
                                    break;
                                }
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            tracing::debug!(error = %error, "WebTransport frame read failed");
                            break;
                        }
                    }
                }
            }
        }

        cancel_token.cancel();
        Ok(())
    }
}

/// Commits a pending one-time ticket after the session is established.
async fn commit_ticket(
    state: &AppState,
    room_id: &synctv_core::models::RoomId,
    auth: &crate::http::websocket::HandshakeAuthContext,
    handshake_control: &ExecutionControl,
) -> Result<(), String> {
    crate::http::websocket::commit_prevalidated_ticket(state, room_id, auth, handshake_control)
        .await
        .map_err(|error| error.to_string())
}

/// Writes one observation-lane frame, opening the lane's push stream lazily
/// on first use. Observations beyond [`MAX_OBSERVATION_STREAMS`] — or when
/// the session can no longer open streams — stay on the control stream for
/// their entire lifetime, which preserves the protocol's per-observation
/// FIFO guarantee (an observation never changes streams mid-flight).
async fn write_observation_frame<W>(
    lanes: &mut HashMap<String, WtSendStream>,
    opener: &WtStreamOpener,
    control: &mut W,
    observe_id: &str,
    bytes: &[u8],
) -> bool
where
    W: tokio::io::AsyncWrite + Unpin,
{
    if !lanes.contains_key(observe_id) {
        if lanes.len() >= MAX_OBSERVATION_STREAMS {
            return write_frame(control, bytes).await.is_ok();
        }
        match opener.open_uni().await {
            Some(stream) => {
                lanes.insert(observe_id.to_string(), stream);
            }
            None => return write_frame(control, bytes).await.is_ok(),
        }
    }
    match lanes.get_mut(observe_id) {
        Some(stream) => write_frame(stream, bytes).await.is_ok(),
        // Unreachable: the lane was inserted above.
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::announced_stream_revision;

    fn uri_with_query(query: &str) -> axum::http::Uri {
        format!("https://example.test/wt/rooms/room_1?{query}")
            .parse()
            .expect("uri parses")
    }

    #[test]
    fn multi_stream_revision_is_parsed_from_the_query() {
        assert_eq!(
            announced_stream_revision(&uri_with_query("streams=1&format=protobuf")),
            Some(1)
        );
        assert_eq!(
            announced_stream_revision(&uri_with_query("format=protobuf&streams=1")),
            Some(1)
        );
    }

    #[test]
    fn missing_or_unknown_revision_yields_none() {
        assert_eq!(
            announced_stream_revision(&uri_with_query("format=protobuf")),
            None
        );
        assert_eq!(
            announced_stream_revision(&uri_with_query("streams=2")),
            Some(2)
        );
        assert_eq!(announced_stream_revision(&uri_with_query("streams=")), None);
        assert_eq!(
            announced_stream_revision(
                &"https://example.test/wt/rooms/room_1".parse().expect("uri")
            ),
            None
        );
    }
}
