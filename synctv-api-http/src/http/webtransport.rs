//! WebTransport room realtime transport.
//!
//! Serves the room realtime session over a WebTransport session (extended
//! CONNECT under the `webtransport` protocol on the server's HTTP/3
//! listener). The wire format mirrors the binary WebSocket transport:
//! length-prefixed protobuf `ClientMessage`/`ServerMessage` frames on one
//! bidirectional stream, scheduled through the same per-observation fair
//! queue and driven by the same `StreamMessageHandler` as WS/gRPC.

use std::sync::Arc;

use axum::http::{Request, StatusCode};
use synctv_quic::{read_frame, write_frame, WebTransportHandler, WtBidiStream};

use crate::http::websocket::{
    build_room_stream_handler, extract_handshake_auth, load_websocket_username,
    reserve_websocket_upgrade_slots, validate_websocket_origin, validate_websocket_room_membership,
    validate_websocket_runtime_dependencies, ReservationCleanupGuard, WsQuery,
};
use crate::http::AppState;
use synctv_api_common::impls::messaging::{
    FairQueueConfig, FairSessionSender, MessageSender, ProtoCodec,
};
use synctv_api_common::impls::RequestMetadata as ApiRequestMetadata;

use synctv_core::provider::ExecutionControl;

/// Base path of the WebTransport room endpoint (`/wt/rooms/{roomId}`).
pub const WEBTRANSPORT_ROOMS_PATH: &str = "/wt/rooms/";

/// Length of the base path prefix.
const WEBTRANSPORT_ROOMS_PATH_LEN: usize = WEBTRANSPORT_ROOMS_PATH.len();

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

/// Builds the transport-agnostic request metadata for a WT CONNECT.
fn wt_request_metadata(request: &Request<()>) -> ApiRequestMetadata {
    ApiRequestMetadata {
        transport: synctv_api_common::impls::TransportProtocol::Http,
        authorization: request
            .headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        client_ip: None,
        socket_ip: None,
        user_agent: request
            .headers()
            .get(axum::http::header::USER_AGENT)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        endpoint_scope: None,
        timeout: None,
    }
}

/// Room realtime sessions over WebTransport.
pub struct RoomWebTransportHandler {
    pub state: AppState,
}

#[async_trait::async_trait]
impl WebTransportHandler for RoomWebTransportHandler {
    /// Cheap admission so obvious rejections (bad path, banned/closed room,
    /// missing auth material) answer the CONNECT immediately. The full
    /// handshake (ticket commit, membership, connection limits) runs in
    /// [`Self::run_session`] before any frames flow.
    async fn accept(&self, request: &Request<()>) -> Result<(), StatusCode> {
        let Some((public_room_id, query)) = parse_room_target(request.uri()) else {
            return Err(StatusCode::NOT_FOUND);
        };
        let has_auth = request
            .headers()
            .contains_key(axum::http::header::AUTHORIZATION)
            || !query.ticket.is_empty();
        if !has_auth {
            return Err(StatusCode::UNAUTHORIZED);
        }
        let Ok(room_id) = self
            .state
            .shared_api_runtime
            .public_id_codec
            .decode_room_id(&public_room_id)
        else {
            return Err(StatusCode::BAD_REQUEST);
        };
        let Ok(room) = self.state.room_service.get_room(&room_id).await else {
            return Err(StatusCode::NOT_FOUND);
        };
        if room.is_banned || room.status.is_closed() {
            return Err(StatusCode::FORBIDDEN);
        }
        Ok(())
    }

    async fn run_session(&self, request: Request<()>, stream: WtBidiStream) {
        if let Err(error) = self.run_session_inner(request, stream).await {
            tracing::info!(error = %error, "WebTransport room session ended with error");
        }
    }
}

impl RoomWebTransportHandler {
    async fn run_session_inner(
        &self,
        request: Request<()>,
        stream: WtBidiStream,
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

        let request_meta = wt_request_metadata(&request);
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
            Arc::new(fair_sender) as Arc<dyn MessageSender>,
        )
        .map_err(|error| error.to_string())?;

        let connection_id = handler.connection_id().to_string();
        let (incoming_tx, cancel_token) = handler.start().await.map_err(|error| {
            tracing::error!(error = %error, "WebTransport room session failed to join");
            error
        })?;

        reservation_cleanup.disarm();
        reservation.release(state.connection_manager.as_ref());

        tracing::info!(
            connection_id = %connection_id,
            room_id = %public_room_id,
            "WebTransport room session established"
        );

        let (mut send_half, mut recv_half) = synctv_quic::h3::quic::BidiStream::split(stream);

        // Writer: fair queue -> length-prefixed protobuf frames.
        let writer_cancel = cancel_token.clone();
        tokio::spawn(async move {
            let mut fair_rx = fair_rx;
            loop {
                tokio::select! {
                    message = fair_rx.recv() => {
                        let Some(message) = message else { break };
                        let Ok(bytes) = ProtoCodec::encode_server_message(&message) else {
                            continue;
                        };
                        if write_frame(&mut send_half, &bytes).await.is_err() {
                            break;
                        }
                    }
                    () = writer_cancel.cancelled() => {
                        use tokio::io::AsyncWriteExt;
                        let _ = send_half.shutdown().await;
                        break;
                    }
                }
            }
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
