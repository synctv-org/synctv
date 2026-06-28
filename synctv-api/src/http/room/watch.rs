use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::sse::{Event, KeepAlive, Sse},
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};

use super::super::websocket::RealtimeTransportFormat;
use super::query::{parse_watch_delivery_mode, watch_after_event_sequence};
use super::request_metadata;
use super::WatchQuery;
use super::{AppResult, AppState, RequestMetadata};
use crate::http::validation::ProtoQuery;
use crate::impls::messaging::{
    MessageSender, RealtimeJoinError, ResourceWatchSession, ResourceWatchSessionConfig,
};
use crate::impls::EndpointRateLimitCategory;
use synctv_proto::client::{
    ListPlaylistItemsRequest, WatchChatEventsRequest, WatchChatPinEventsRequest,
    WatchPlaylistItemsRequest, WatchRoomMemberEventsRequest, WatchRoomSettingsRequest,
};

struct HttpWatchMessageSender {
    sender: tokio::sync::mpsc::Sender<synctv_proto::client::ServerMessage>,
}

impl MessageSender for HttpWatchMessageSender {
    fn send(&self, message: synctv_proto::client::ServerMessage) -> Result<(), String> {
        self.sender.try_send(message).map_err(|error| match error {
            tokio::sync::mpsc::error::TrySendError::Full(_) => {
                "SSE watch client is too slow to consume messages".to_string()
            }
            tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                "SSE watch client disconnected".to_string()
            }
        })
    }

    fn is_alive(&self) -> bool {
        !self.sender.is_closed()
    }
}

fn map_resource_watch_prepare_error(error: RealtimeJoinError) -> super::super::AppError {
    error.log_if_internal("http_resource_watch_prepare");
    super::super::AppError::from(crate::impls::ApiError::from(error))
}

pub(in crate::http::room) struct CancelOnDropStream<S> {
    inner: S,
    cancel_token: tokio_util::sync::CancellationToken,
}

impl<S> CancelOnDropStream<S> {
    pub(in crate::http::room) fn new(
        inner: S,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> Self {
        Self {
            inner,
            cancel_token,
        }
    }
}

impl<S> Stream for CancelOnDropStream<S>
where
    S: Stream + Unpin,
{
    type Item = S::Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

impl<S> Drop for CancelOnDropStream<S> {
    fn drop(&mut self) {
        self.cancel_token.cancel();
    }
}

fn encode_resource_watch_sse_data<M>(
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

pub(in crate::http::room) fn sse_event_from_server_message(
    format: RealtimeTransportFormat,
    message: synctv_proto::client::ServerMessage,
) -> Option<Result<Event, Infallible>> {
    use synctv_proto::client::server_message::Message;

    let (event_name, event_id, data) = match message.message? {
        Message::ResourceObserved(observed) => (
            "observed",
            None,
            encode_resource_watch_sse_data(format, &observed),
        ),
        Message::ResourceEvent(changed) => {
            let event_id = sse_event_id_from_resource_event(&changed);
            (
                "changed",
                event_id,
                encode_resource_watch_sse_data(format, &changed),
            )
        }
        Message::ResourceObserveError(error) => (
            "error",
            None,
            encode_resource_watch_sse_data(format, &error),
        ),
        _ => return None,
    };
    let data = match data {
        Ok(data) => data,
        Err(error) => {
            tracing::warn!(error = %error, "Failed to serialize resource watch SSE event");
            return Some(Ok(Event::default()
                .event("error")
                .data(r#"{"message":"Failed to serialize resource watch event"}"#)));
        }
    };
    let mut event = Event::default().event(event_name).data(data);
    if let Some(event_id) = event_id {
        event = event.id(event_id);
    }
    Some(Ok(event))
}

pub(in crate::http::room) fn sse_event_id_from_resource_event(
    changed: &synctv_proto::client::ResourceEvent,
) -> Option<String> {
    changed
        .event_cursor
        .as_ref()
        .map(|cursor| cursor.sequence.to_string())
        .or_else(|| {
            let Some(synctv_proto::client::resource_event::Payload::ChatEvent(event)) =
                changed.payload.as_ref()
            else {
                return None;
            };
            Some(event.sequence.to_string())
        })
}

pub(in crate::http::room) async fn open_resource_watch_sse(
    state: AppState,
    request_meta: RequestMetadata,
    public_room_id: String,
    observe: synctv_proto::client::ObserveResource,
    format: RealtimeTransportFormat,
) -> AppResult<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>> {
    let event_service = state.event_service.clone();
    let room_id = state
        .shared_api_runtime
        .public_id_codec
        .decode_room_id(&public_room_id)
        .map_err(|error| {
            super::super::AppError::bad_request(format!("Invalid room_id: {error}"))
        })?;
    let request_meta = request_metadata(request_meta).with_timeout(None);
    let principal = {
        let client_api = state.shared_api_runtime.client_api.clone();
        let user_service = state.user_service.clone();
        crate::impls::ClientApiImpl::execute_room_actor_endpoint(
            client_api,
            &request_meta,
            public_room_id,
            EndpointRateLimitCategory::WebSocket,
            move |_client_api, actor| async move {
                Ok::<_, crate::impls::ApiError>(match actor {
                    crate::impls::client::RoomActor::User { user_id, .. } => {
                        let username = user_service
                            .get_user(&user_id)
                            .await
                            .map_err(crate::impls::ApiError::from)?
                            .username;
                        crate::impls::messaging::RealtimePrincipal::user(user_id, username)
                    }
                    crate::impls::client::RoomActor::Guest(access) => {
                        let identity = crate::impls::messaging::GuestRealtimeIdentity {
                            guest_id: access.guest_id,
                            display_name: access.display_name,
                            session_id: access.session_id,
                            token_jti: access.token_jti,
                            room_guest_version: access.room_guest_version,
                            permissions: access.permissions,
                        };
                        crate::impls::messaging::RealtimePrincipal::guest(room_id, identity)
                            .map_err(|error| crate::impls::ApiError::Internal(error.to_string()))?
                    }
                })
            },
        )
        .await
        .map_err(super::super::error::map_api_error)?
    };

    let (outgoing_tx, outgoing_rx) =
        tokio::sync::mpsc::channel::<synctv_proto::client::ServerMessage>(64);
    let sender = Arc::new(HttpWatchMessageSender {
        sender: outgoing_tx,
    });
    let session = ResourceWatchSession::new(ResourceWatchSessionConfig {
        room_id,
        principal,
        room_service: state.room_service.clone(),
        chat_service: state.chat_service.clone(),
        event_service,
        connection_service: state.connection_manager.clone(),
        presence_service: state.presence_service.clone(),
        public_id_codec: state.shared_api_runtime.public_id_codec.clone(),
        sender,
        playback_service: state.shared_api_runtime.client_api.clone(),
        playlist_items_snapshot_service: state.shared_api_runtime.client_api.clone(),
        room_members_snapshot_service: state.shared_api_runtime.client_api.clone(),
        room_settings_snapshot_service:
            crate::impls::room_settings_snapshot::default_room_settings_snapshot_service(
                state.room_service.clone(),
            ),
    });
    let prepared_session = session
        .prepare(&observe)
        .await
        .map_err(map_resource_watch_prepare_error)?;
    let cancel_token = tokio_util::sync::CancellationToken::new();
    let session_cancel = cancel_token.clone();
    tokio::spawn(async move {
        if let Err(error) = prepared_session.run(session_cancel).await {
            tracing::warn!(error = %error, "HTTP resource watch session ended with error");
        }
    });

    let stream = ReceiverStream::new(outgoing_rx)
        .filter_map(move |message| sse_event_from_server_message(format, message));
    let stream = CancelOnDropStream::new(stream, cancel_token);
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

pub async fn watch_room_settings(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    headers: HeaderMap,
    Query(query): Query<WatchQuery>,
) -> AppResult<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>> {
    let room_id = path.room_id;
    let format = RealtimeTransportFormat::parse(query.format.as_deref())?;
    let after_event_sequence = watch_after_event_sequence(&headers, query.after_event_sequence)?;
    let request = WatchRoomSettingsRequest {
        delivery_mode: parse_watch_delivery_mode(query.delivery_mode)?,
        room_settings: Some(synctv_proto::client::ObserveRoomSettings {
            after_event_sequence,
        }),
    };
    let observe = crate::impls::messaging::watch_room_settings_observe(request)
        .map_err(super::super::AppError::bad_request)?;
    open_resource_watch_sse(state, request_meta, room_id, observe, format).await
}

pub async fn watch_playlist_items(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    headers: HeaderMap,
    Query(query): Query<WatchQuery>,
    ProtoQuery(request): ProtoQuery<ListPlaylistItemsRequest>,
) -> AppResult<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>> {
    let room_id = path.room_id;
    let format = RealtimeTransportFormat::parse(query.format.as_deref())?;
    let after_event_sequence = watch_after_event_sequence(&headers, query.after_event_sequence)?;
    let request = WatchPlaylistItemsRequest {
        delivery_mode: parse_watch_delivery_mode(query.delivery_mode)?,
        playlist_items: Some(synctv_proto::client::ObservePlaylistItems {
            request: Some(request),
            after_event_sequence,
        }),
    };
    let observe = crate::impls::messaging::watch_playlist_items_observe(request)
        .map_err(super::super::AppError::bad_request)?;
    open_resource_watch_sse(state, request_meta, room_id, observe, format).await
}

pub async fn watch_room_members(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    headers: HeaderMap,
    Query(query): Query<WatchQuery>,
) -> AppResult<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>> {
    let room_id = path.room_id;
    let format = RealtimeTransportFormat::parse(query.format.as_deref())?;
    let after_event_sequence = watch_after_event_sequence(&headers, query.after_event_sequence)?;
    let request = WatchRoomMemberEventsRequest {
        delivery_mode: parse_watch_delivery_mode(query.delivery_mode)?,
        room_member_events: Some(synctv_proto::client::ObserveRoomMemberEvents {
            after_event_sequence,
        }),
    };
    let observe = crate::impls::messaging::watch_room_member_events_observe(request)
        .map_err(super::super::AppError::bad_request)?;
    open_resource_watch_sse(state, request_meta, room_id, observe, format).await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{roomId}/watch/chat-events",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("format" = Option<String>, Query, description = "SSE payload format: json or protobuf"),
            ("afterEventSequence" = Option<i64>, Query, description = "Replay chat events strictly after this durable event sequence"),
            ("deliveryMode" = Option<i32>, Query, description = "Resource delivery mode enum integer")
        ),
        responses(
            (status = 200, description = "SSE stream of chat resource events"),
            (status = 400, description = "Invalid request or event cursor", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "VIEW_CHAT_HISTORY permission required", body = synctv_proto::client::ApiErrorResponse),
            (status = 503, description = "Realtime manager unavailable", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn watch_chat_events(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    headers: HeaderMap,
    Query(query): Query<WatchQuery>,
) -> AppResult<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>> {
    let room_id = path.room_id;
    let format = RealtimeTransportFormat::parse(query.format.as_deref())?;
    let after_event_sequence = watch_after_event_sequence(&headers, query.after_event_sequence)?;
    let request = WatchChatEventsRequest {
        delivery_mode: parse_watch_delivery_mode(query.delivery_mode)?,
        chat_events: Some(synctv_proto::client::ObserveChatEvents {
            after_event_sequence,
        }),
    };
    let observe = crate::impls::messaging::watch_chat_events_observe(request)
        .map_err(super::super::AppError::bad_request)?;
    open_resource_watch_sse(state, request_meta, room_id, observe, format).await
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{roomId}/watch/chat-pin-events",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("format" = Option<String>, Query, description = "SSE payload format: json or protobuf"),
            ("afterEventSequence" = Option<i64>, Query, description = "Replay chat pin events strictly after this durable event sequence"),
            ("deliveryMode" = Option<i32>, Query, description = "Resource delivery mode enum integer")
        ),
        responses(
            (status = 200, description = "SSE stream of chat pin resource events"),
            (status = 400, description = "Invalid request or event cursor", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "VIEW_CHAT_HISTORY permission required", body = synctv_proto::client::ApiErrorResponse),
            (status = 503, description = "Realtime manager unavailable", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn watch_chat_pin_events(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    headers: HeaderMap,
    Query(query): Query<WatchQuery>,
) -> AppResult<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>> {
    let room_id = path.room_id;
    let format = RealtimeTransportFormat::parse(query.format.as_deref())?;
    let after_event_sequence = watch_after_event_sequence(&headers, query.after_event_sequence)?;
    let request = WatchChatPinEventsRequest {
        delivery_mode: parse_watch_delivery_mode(query.delivery_mode)?,
        chat_pin_events: Some(synctv_proto::client::ObserveChatPinEvents {
            after_event_sequence,
        }),
    };
    let observe = crate::impls::messaging::watch_chat_pin_events_observe(request)
        .map_err(super::super::AppError::bad_request)?;
    open_resource_watch_sse(state, request_meta, room_id, observe, format).await
}
