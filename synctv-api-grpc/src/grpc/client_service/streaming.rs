use synctv_api_common::impls::messaging::{MessageSender, StreamMessage};
use synctv_proto::client::{
    ClientMessage, ServerMessage, WatchChatEventsEvent, WatchChatPinEventsEvent,
    WatchPlaybackEvent, WatchPlaybackStateEvent, WatchPlaylistItemsEvent,
    WatchRoomMemberEventsEvent, WatchRoomSettingsEvent,
};

pub(super) const MESSAGE_STREAM_BUFFER_SIZE: usize = 100;

/// Signaling budget for a single gRPC MessageStream ClientMessage, matching
/// the WebSocket / WebTransport inbound frame caps. The transport-level
/// decode limit stays at the service-wide `grpc_max_message_size_bytes`
/// (other RoomService methods legitimately carry large blobs), so this
/// post-decode check is what bounds the streaming uplink.
pub(super) const GRPC_STREAM_UPLINK_MAX_BYTES: usize = 64 * 1024;

/// Per-write bound for response-stream pumps: a client that stops calling
/// `Recv` parks hyper's response body on h2 flow control; without a bound
/// the pump waits forever and every outbound sender clone stays alive, so
/// the response stream can never complete and the session's queued lanes
/// linger until process restart.
pub(super) const PUMP_WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Grace between session cancellation and force-ending the outbound chain:
/// lets already-scheduled terminations reach the wire, then guarantees the
/// response stream ends (trailers included).
pub(super) const PUMP_DRAIN_GRACE: std::time::Duration = std::time::Duration::from_secs(5);
pub(super) const WATCH_STREAM_BUFFER_SIZE: usize = 64;

#[derive(Debug)]
pub(super) enum GrpcReceiveOutcome<T, E> {
    Message(Result<Option<T>, E>),
    ResponseStreamClosed,
}

pub(super) async fn await_grpc_receive_or_response_close<T, E, F>(
    receive_future: F,
    response_sender: tokio::sync::mpsc::Sender<ServerMessage>,
) -> GrpcReceiveOutcome<T, E>
where
    F: std::future::Future<Output = Result<Option<T>, E>>,
{
    tokio::select! {
        result = receive_future => GrpcReceiveOutcome::Message(result),
        () = response_sender.closed() => GrpcReceiveOutcome::ResponseStreamClosed,
    }
}

/// gRPC message sender for `StreamMessageHandler`
pub(super) struct GrpcMessageSender {
    pub(super) sender: tokio::sync::mpsc::Sender<ServerMessage>,
}

impl GrpcMessageSender {
    pub(super) const fn new(sender: tokio::sync::mpsc::Sender<ServerMessage>) -> Self {
        Self { sender }
    }
}

impl MessageSender for GrpcMessageSender {
    fn send(&self, message: ServerMessage) -> Result<(), String> {
        self.sender.try_send(message).map_err(|e| match e {
            tokio::sync::mpsc::error::TrySendError::Full(_) => {
                tracing::warn!(
                    "gRPC outgoing message dropped: client stream buffer is full \
                         (buffer capacity: {}). Client may be too slow to consume messages.",
                    MESSAGE_STREAM_BUFFER_SIZE,
                );
                "Channel full: client too slow to consume messages".to_string()
            }
            tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                "Channel closed: client disconnected".to_string()
            }
        })
    }

    fn is_alive(&self) -> bool {
        !self.sender.is_closed()
    }
}

pub(super) enum GrpcWatchEvent {
    Observed(synctv_proto::client::ResourceObserved),
    Changed(Box<synctv_proto::client::ResourceEvent>),
    Error(synctv_proto::client::ResourceObserveError),
}

fn watch_event_from_server_message<E, O>(message: ServerMessage, wrap: O) -> Option<E>
where
    O: FnOnce(GrpcWatchEvent) -> E,
{
    use synctv_proto::client::server_message::Message;

    let event = match message.message? {
        Message::ResourceObserved(observed) => GrpcWatchEvent::Observed(observed),
        Message::ResourceEvent(changed) => GrpcWatchEvent::Changed(Box::new(changed)),
        Message::ResourceObserveError(error) => GrpcWatchEvent::Error(error),
        _ => return None,
    };
    Some(wrap(event))
}

macro_rules! impl_watch_event_converter {
    ($fn_name:ident, $event_type:ty, $event_enum:path) => {
        pub(super) fn $fn_name(message: ServerMessage) -> Option<$event_type> {
            use $event_enum as Event;
            watch_event_from_server_message(message, |event| {
                let mut response = <$event_type>::default();
                response.event = Some(match event {
                    GrpcWatchEvent::Observed(value) => Event::Observed(value),
                    GrpcWatchEvent::Changed(value) => Event::ResourceEvent(*value),
                    GrpcWatchEvent::Error(value) => Event::Error(value),
                });
                response
            })
        }
    };
}

impl_watch_event_converter!(
    watch_playback_state_event,
    WatchPlaybackStateEvent,
    synctv_proto::client::watch_playback_state_event::Event
);
impl_watch_event_converter!(
    watch_playback_event,
    WatchPlaybackEvent,
    synctv_proto::client::watch_playback_event::Event
);
impl_watch_event_converter!(
    watch_room_settings_event,
    WatchRoomSettingsEvent,
    synctv_proto::client::watch_room_settings_event::Event
);
impl_watch_event_converter!(
    watch_playlist_items_event,
    WatchPlaylistItemsEvent,
    synctv_proto::client::watch_playlist_items_event::Event
);
impl_watch_event_converter!(
    watch_room_member_events_event,
    WatchRoomMemberEventsEvent,
    synctv_proto::client::watch_room_member_events_event::Event
);
impl_watch_event_converter!(
    watch_chat_events_event,
    WatchChatEventsEvent,
    synctv_proto::client::watch_chat_events_event::Event
);
impl_watch_event_converter!(
    watch_chat_pin_events_event,
    WatchChatPinEventsEvent,
    synctv_proto::client::watch_chat_pin_events_event::Event
);

/// gRPC stream implementation of `StreamMessage` trait
///
/// Adapts `tonic::Streaming<ClientMessage>` + the session's fair queue to the
/// unified `StreamMessage` interface, enabling full code reuse with the WebSocket
/// path. `response_watch` mirrors the outgoing response channel so `recv` can
/// stop as soon as the client stops reading the response stream.
pub(super) struct GrpcStreamMessage {
    pub(super) client_stream: tonic::Streaming<ClientMessage>,
    pub(super) sender: synctv_api_common::impls::messaging::FairSessionSender,
    pub(super) response_watch: tokio::sync::mpsc::Sender<ServerMessage>,
    pub(super) alive: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl StreamMessage for GrpcStreamMessage {
    async fn recv(&mut self) -> Option<Result<ClientMessage, String>> {
        match await_grpc_receive_or_response_close(
            self.client_stream.message(),
            self.response_watch.clone(),
        )
        .await
        {
            GrpcReceiveOutcome::Message(Ok(Some(msg))) => Some(Ok(msg)),
            GrpcReceiveOutcome::Message(Ok(None)) => None,
            GrpcReceiveOutcome::ResponseStreamClosed => {
                self.alive
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                None
            }
            GrpcReceiveOutcome::Message(Err(e)) => {
                self.alive
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                Some(Err(format!("gRPC stream error: {e}")))
            }
        }
    }

    fn send(&self, message: ServerMessage) -> Result<(), String> {
        MessageSender::send(&self.sender, message)
    }

    fn is_alive(&self) -> bool {
        self.alive.load(std::sync::atomic::Ordering::Relaxed) && self.sender.is_alive()
    }
}
