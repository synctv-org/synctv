use prost::Message as _;
use std::sync::Arc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status};

use super::super::{
    invalid_argument_status, map_api_error, map_message_stream_join_error,
    map_message_stream_membership_error, realtime_room_access_error, ClientServiceImpl,
};
use super::RoomService;
use crate::grpc::client_service::streaming::{
    watch_chat_events_event, watch_chat_pin_events_event, watch_playback_event,
    watch_playback_state_event, watch_playlist_items_event, watch_room_member_events_event,
    watch_room_settings_event, GrpcStreamMessage, GRPC_STREAM_UPLINK_MAX_BYTES,
    MESSAGE_STREAM_BUFFER_SIZE, PUMP_DRAIN_GRACE, PUMP_WRITE_TIMEOUT,
};
use synctv_api_common::impls::messaging::{
    GuestRealtimeIdentity, MessageConcurrencyConfig, MessageSender, RealtimePrincipal,
    StreamMessage, StreamMessageHandler, StreamMessageHandlerConfig, StreamMessageHandlerRuntime,
};
use synctv_api_common::impls::ApiError;
use synctv_api_common::impls::EndpointRateLimitCategory;
use synctv_proto::client::*;

pub(super) async fn message_stream(
    service: &ClientServiceImpl,
    request: Request<tonic::Streaming<ClientMessage>>,
) -> Result<Response<<ClientServiceImpl as RoomService>::MessageStreamStream>, Status> {
    use tokio::sync::mpsc;

    // Extract all data from request BEFORE any await points.
    // Request<Streaming<_>> is !Sync, so holding it across.await makes
    // the future !Send, violating the tonic trait requirement.
    let (metadata, room_id) = service.internal_room_request_context(&request)?;
    let guest_token = ClientServiceImpl::extract_guest_token_from_authorization(
        metadata.authorization.as_deref(),
    )?;
    let client_stream = request.into_inner();
    let executor = service.client_api.clone();
    let guest_principal = if let Some(guest_token) = guest_token {
        let public_room_id = service
            .client_api
            .public_id_codec
            .encode_room_id(room_id)
            .map_err(|error| invalid_argument_status(format!("Invalid room_id: {error}")))?;
        let client_api = service.client_api.clone();
        Some(
            executor
                .execute_public_endpoint(
                    &metadata,
                    EndpointRateLimitCategory::WebSocket,
                    move || async move {
                        let access = client_api
                            .validate_guest_room_access(&guest_token, &public_room_id)
                            .await?;
                        let identity = GuestRealtimeIdentity {
                            guest_id: access.guest_id,
                            display_name: access.display_name,
                            session_id: access.session_id,
                            token_jti: access.token_jti,
                            room_guest_version: access.room_guest_version,
                            permissions: access.permissions,
                        };
                        Ok::<_, synctv_api_common::impls::ApiError>(RealtimePrincipal::guest(
                            identity,
                        ))
                    },
                )
                .await
                .map_err(map_api_error)?,
        )
    } else {
        None
    };
    let principal = if let Some(principal) = guest_principal {
        principal
    } else {
        let user_id = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::WebSocket,
                move |authenticated| async move {
                    Ok::<_, synctv_api_common::impls::ApiError>(authenticated.user_id())
                },
            )
            .await
            .map_err(map_api_error)?;

        // Get user details from service
        let user = service
            .user_service
            .get_user(&user_id)
            .await
            .map_err(|err| map_api_error(ApiError::from(err)))?;
        let username = user.username;

        // Check room membership before establishing stream
        service
            .room_service
            .check_membership(&room_id, &user_id)
            .await
            .map_err(map_message_stream_membership_error)?;

        RealtimePrincipal::user(user_id, username)
    };

    let room = service
        .room_service
        .get_room(&room_id)
        .await
        .map_err(|err| map_api_error(ApiError::from(err)))?;
    if let Some(status) = realtime_room_access_error(&room) {
        return Err(status);
    }

    let realtime_actor = principal
        .realtime_actor(&service.client_api.public_id_codec)
        .map_err(invalid_argument_status)?;
    tracing::info!(
        actor = %realtime_actor,
        room_id = %room_id,
        "Client establishing MessageStream connection"
    );

    // Connection registration is handled by StreamMessageHandler::run()
    // which generates its own connection_id and manages the full lifecycle.

    let event_service = service.event_service.clone();

    // Create channel for outgoing messages with bounded capacity to prevent memory exhaustion
    let (outgoing_tx, outgoing_rx) = mpsc::channel::<ServerMessage>(MESSAGE_STREAM_BUFFER_SIZE);

    // Outbound traffic goes through the per-observation fair queue: control
    // traffic first, interactive lanes (playback state, WebRTC signaling)
    // weighted over bulk lanes, FIFO within each observation. The pump below
    // forwards scheduled messages onto the gRPC response stream; a slow
    // client fills the fair queue and trips its eviction escalation instead
    // of silently dropping messages.
    let (fair_sender, fair_rx) =
        synctv_api_common::impls::messaging::FairSessionSender::new(Default::default());

    // Create StreamMessageHandler with all configuration
    let stream_handler = StreamMessageHandler::new_with_runtime(
        StreamMessageHandlerConfig {
            room_id,
            principal,
            connection_id: None,
            room_service: service.room_service.clone(),
            chat_service: service.chat_service.clone(),
            event_service: event_service.clone(),
            connection_service: service.connection_service.clone(),
            rate_limiter: service.rate_limiter.clone(),
            rate_limit_config: service.rate_limit_config.clone(),
            content_filter: service.content_filter.clone(),
            public_id_codec: service.client_api.public_id_codec.clone(),
            sender: Arc::new(fair_sender.clone()) as Arc<dyn MessageSender>,
            concurrency_config: Arc::new(MessageConcurrencyConfig::default()),
        },
        StreamMessageHandlerRuntime {
            clock: service.client_api.clock.clone(),
            playback_service: service.client_api.clone(),
            playlist_items_snapshot_service: service.client_api.clone(),
            room_members_snapshot_service: service.client_api.clone(),
            room_settings_snapshot_service:
                synctv_api_common::impls::room_settings_snapshot::default_room_settings_snapshot_service(
                    service.room_service.clone(),
                ),
            playback_fanout: service.client_api.playback_fanout.clone(),
            chat_event_dispatcher: synctv_api_common::chat_event_dispatcher::default_chat_event_dispatcher(
                event_service.clone(),
            ),
            presence_service: service.presence_service.clone(),
            notification_service: service.notification_service.clone(),
            ws_message_rate_limit: service
                .runtime_settings
                .connection_limits
                .ws_message_rate_limit_per_second,
            heartbeat_schedule: service.heartbeat_schedule,
            filter_private_ice_candidates: service
                .runtime_settings
                .webrtc
                .filter_private_ice_candidates,
            swarm_signing_key: service.client_api.media_swarm_signing_key.clone(),
            media_swarm_tracker: service.client_api.media_swarm_tracker.clone(),
            runtime_settings_store: service.client_api.runtime_settings_store.clone(),
        },
    );

    // Start the shared real-time actor before returning the response stream so
    // admission failures still surface as gRPC status errors.
    let (incoming_tx, cancel_token) = stream_handler
        .start()
        .await
        .map_err(|error| map_message_stream_join_error(error.into()))?;

    // Fair-queue pump: forwards scheduled messages onto the gRPC response
    // stream. Every write is bounded (PUMP_WRITE_TIMEOUT) and cancellation
    // arms a drain deadline (PUMP_DRAIN_GRACE), so a client that stops
    // calling Recv cannot park the pump - and therefore cannot keep every
    // outgoing sender alive - indefinitely.
    let fair_pump_outgoing = outgoing_tx.clone();
    let pump_token = cancel_token.clone();
    let (pump_alive_tx, pump_alive_rx) = tokio::sync::watch::channel(());
    tokio::spawn(async move {
        let mut fair_rx = fair_rx;
        let mut drain_deadline: Option<tokio::time::Instant> = None;
        loop {
            let message = tokio::select! {
                () = pump_token.cancelled(), if drain_deadline.is_none() => {
                    drain_deadline = Some(tokio::time::Instant::now() + PUMP_DRAIN_GRACE);
                    continue;
                }
                () = async {
                    match drain_deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending::<()>().await,
                    }
                } => break, // grace expired: drop the sender so the stream can end
                message = fair_rx.recv() => match message {
                    Some(message) => message,
                    None => break,
                },
            };
            match tokio::time::timeout(PUMP_WRITE_TIMEOUT, fair_pump_outgoing.send(message)).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) | Err(_) => break, // receiver gone or write timed out: end the chain
            }
        }
        // Signal the response-close watcher; dropping fair_pump_outgoing here
        // is what lets ReceiverStream yield None so hyper can send trailers.
        let _ = pump_alive_tx.send(());
    });

    // Session end must not strand the fair queue's spawned forwarder: closing
    // the sender lets it hand over any already-queued frames (terminations
    // above all) and then close the response stream's channel chain.
    {
        let fair_closer_sender = fair_sender.clone();
        let fair_closer_token = cancel_token.clone();
        tokio::spawn(async move {
            fair_closer_token.cancelled().await;
            fair_closer_sender.close();
        });
    }

    // Pump transport input into the shared handler. The handler owns all
    // business logic and cleanup; this task only decodes transport frames.
    let transport_cancel_token = cancel_token.clone();
    let transport_response_watch = outgoing_tx.clone();
    tokio::spawn(async move {
        let mut grpc_stream = GrpcStreamMessage {
            client_stream,
            sender: fair_sender,
            response_watch: transport_response_watch,
            alive: std::sync::atomic::AtomicBool::new(true),
        };
        let mut drain_deadline: Option<tokio::time::Instant> = None;

        loop {
            let received = tokio::select! {
                () = transport_cancel_token.cancelled(), if drain_deadline.is_none() => {
                    drain_deadline = Some(tokio::time::Instant::now() + PUMP_DRAIN_GRACE);
                    continue;
                }
                () = async {
                    match drain_deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending::<()>().await,
                    }
                } => None, // grace expired: stop holding the response sender
                received = grpc_stream.recv() => Some(received),
            };
            let Some(received) = received else { break };
            match received {
                Some(Ok(message)) => {
                    // Signaling budget guard: the service-wide decode limit
                    // must stay large for other RoomService methods, so bound
                    // the streaming uplink here, post-decode.
                    if message.encoded_len() > GRPC_STREAM_UPLINK_MAX_BYTES {
                        tracing::warn!(
                            bytes = message.encoded_len(),
                            limit = GRPC_STREAM_UPLINK_MAX_BYTES,
                            "gRPC MessageStream uplink exceeded signaling budget; closing stream"
                        );
                        transport_cancel_token.cancel();
                        break;
                    }
                    if incoming_tx.send(message).await.is_err() {
                        break;
                    }
                }
                Some(Err(error)) => {
                    tracing::error!("gRPC stream receive error: {}", error);
                    transport_cancel_token.cancel();
                    break;
                }
                None => {
                    // Request-stream EOF is only a half-close for bidi gRPC.
                    // Keep the shared real-time actor running so it can still
                    // deliver the response to the client until the response
                    // stream itself closes or a business-level disconnect
                    // signal arrives.
                    break;
                }
            }
        }
    });

    // Response-close watcher: no task here may hold an outgoing sender -
    // a held clone keeps ReceiverStream (and hyper's response body) open
    // forever, making server-initiated completion impossible. The pump owns
    // the last functional sender and reports its exit via the watch channel.
    let response_close_token = cancel_token.clone();
    tokio::spawn(async move {
        let mut pump_alive_rx = pump_alive_rx;
        tokio::select! {
            _ = pump_alive_rx.changed() => {}, // pump exited: response chain closed
            () = response_close_token.cancelled() => {},
        }
        response_close_token.cancel();
    });

    let output_stream = ReceiverStream::new(outgoing_rx).map(Ok::<_, Status>);

    Ok(Response::new(
        Box::pin(output_stream) as <ClientServiceImpl as RoomService>::MessageStreamStream
    ))
}

pub(super) async fn watch_playback_state(
    service: &ClientServiceImpl,
    request: Request<WatchPlaybackStateRequest>,
) -> Result<Response<<ClientServiceImpl as RoomService>::WatchPlaybackStateStream>, Status> {
    let (metadata, room_id) = service.internal_room_request_context(&request)?;
    let observe =
        synctv_api_common::impls::messaging::watch_playback_state_observe(request.into_inner())
            .map_err(Status::invalid_argument)?;
    service
        .open_watch_stream(metadata, room_id, observe, watch_playback_state_event)
        .await
}

pub(super) async fn watch_playback(
    service: &ClientServiceImpl,
    request: Request<WatchPlaybackRequest>,
) -> Result<Response<<ClientServiceImpl as RoomService>::WatchPlaybackStream>, Status> {
    let (metadata, room_id) = service.internal_room_request_context(&request)?;
    let observe = synctv_api_common::impls::messaging::watch_playback_observe(request.into_inner())
        .map_err(Status::invalid_argument)?;
    service
        .open_watch_stream(metadata, room_id, observe, watch_playback_event)
        .await
}

pub(super) async fn watch_room_settings(
    service: &ClientServiceImpl,
    request: Request<WatchRoomSettingsRequest>,
) -> Result<Response<<ClientServiceImpl as RoomService>::WatchRoomSettingsStream>, Status> {
    let (metadata, room_id) = service.internal_room_request_context(&request)?;
    let observe =
        synctv_api_common::impls::messaging::watch_room_settings_observe(request.into_inner())
            .map_err(Status::invalid_argument)?;
    service
        .open_watch_stream(metadata, room_id, observe, watch_room_settings_event)
        .await
}

pub(super) async fn watch_playlist_items(
    service: &ClientServiceImpl,
    request: Request<WatchPlaylistItemsRequest>,
) -> Result<Response<<ClientServiceImpl as RoomService>::WatchPlaylistItemsStream>, Status> {
    let (metadata, room_id) = service.internal_room_request_context(&request)?;
    let observe =
        synctv_api_common::impls::messaging::watch_playlist_items_observe(request.into_inner())
            .map_err(Status::invalid_argument)?;
    service
        .open_watch_stream(metadata, room_id, observe, watch_playlist_items_event)
        .await
}

pub(super) async fn watch_room_member_events(
    service: &ClientServiceImpl,
    request: Request<WatchRoomMemberEventsRequest>,
) -> Result<Response<<ClientServiceImpl as RoomService>::WatchRoomMemberEventsStream>, Status> {
    let (metadata, room_id) = service.internal_room_request_context(&request)?;
    let observe =
        synctv_api_common::impls::messaging::watch_room_member_events_observe(request.into_inner())
            .map_err(Status::invalid_argument)?;
    service
        .open_watch_stream(metadata, room_id, observe, watch_room_member_events_event)
        .await
}

pub(super) async fn watch_chat_events(
    service: &ClientServiceImpl,
    request: Request<WatchChatEventsRequest>,
) -> Result<Response<<ClientServiceImpl as RoomService>::WatchChatEventsStream>, Status> {
    let (metadata, room_id) = service.internal_room_request_context(&request)?;
    let observe =
        synctv_api_common::impls::messaging::watch_chat_events_observe(request.into_inner())
            .map_err(Status::invalid_argument)?;
    service
        .open_watch_stream(metadata, room_id, observe, watch_chat_events_event)
        .await
}

pub(super) async fn watch_chat_pin_events(
    service: &ClientServiceImpl,
    request: Request<WatchChatPinEventsRequest>,
) -> Result<Response<<ClientServiceImpl as RoomService>::WatchChatPinEventsStream>, Status> {
    let (metadata, room_id) = service.internal_room_request_context(&request)?;
    let observe =
        synctv_api_common::impls::messaging::watch_chat_pin_events_observe(request.into_inner())
            .map_err(Status::invalid_argument)?;
    service
        .open_watch_stream(metadata, room_id, observe, watch_chat_pin_events_event)
        .await
}
