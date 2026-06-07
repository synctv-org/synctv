use std::sync::Arc;

use crate::impls::messaging::{MessageSender, StreamMessage};
use synctv_proto::client::{
    ClientMessage, ServerMessage, WatchChatEventsEvent, WatchPlaybackEvent,
    WatchPlaybackStateEvent, WatchPlaylistItemsEvent, WatchRoomMembersEvent,
    WatchRoomSettingsEvent,
};

pub(super) const MESSAGE_STREAM_BUFFER_SIZE: usize = 100;
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
    Changed(Box<synctv_proto::client::ResourceChanged>),
    Error(synctv_proto::client::ResourceObserveError),
}

fn watch_event_from_server_message<E, O>(message: ServerMessage, wrap: O) -> Option<E>
where
    O: FnOnce(GrpcWatchEvent) -> E,
{
    use synctv_proto::client::server_message::Message;

    let event = match message.message? {
        Message::ResourceObserved(observed) => GrpcWatchEvent::Observed(observed),
        Message::ResourceChanged(changed) => GrpcWatchEvent::Changed(Box::new(changed)),
        Message::ResourceObserveError(error) => GrpcWatchEvent::Error(error),
        _ => return None,
    };
    Some(wrap(event))
}

pub(super) fn watch_playback_state_event(
    message: ServerMessage,
) -> Option<WatchPlaybackStateEvent> {
    use synctv_proto::client::watch_playback_state_event::Event;
    watch_event_from_server_message(message, |event| WatchPlaybackStateEvent {
        event: Some(match event {
            GrpcWatchEvent::Observed(value) => Event::Observed(value),
            GrpcWatchEvent::Changed(value) => Event::Changed(*value),
            GrpcWatchEvent::Error(value) => Event::Error(value),
        }),
    })
}

pub(super) fn watch_playback_event(message: ServerMessage) -> Option<WatchPlaybackEvent> {
    use synctv_proto::client::watch_playback_event::Event;
    watch_event_from_server_message(message, |event| WatchPlaybackEvent {
        event: Some(match event {
            GrpcWatchEvent::Observed(value) => Event::Observed(value),
            GrpcWatchEvent::Changed(value) => Event::Changed(*value),
            GrpcWatchEvent::Error(value) => Event::Error(value),
        }),
    })
}

pub(super) fn watch_room_settings_event(message: ServerMessage) -> Option<WatchRoomSettingsEvent> {
    use synctv_proto::client::watch_room_settings_event::Event;
    watch_event_from_server_message(message, |event| WatchRoomSettingsEvent {
        event: Some(match event {
            GrpcWatchEvent::Observed(value) => Event::Observed(value),
            GrpcWatchEvent::Changed(value) => Event::Changed(*value),
            GrpcWatchEvent::Error(value) => Event::Error(value),
        }),
    })
}

pub(super) fn watch_playlist_items_event(
    message: ServerMessage,
) -> Option<WatchPlaylistItemsEvent> {
    use synctv_proto::client::watch_playlist_items_event::Event;
    watch_event_from_server_message(message, |event| WatchPlaylistItemsEvent {
        event: Some(match event {
            GrpcWatchEvent::Observed(value) => Event::Observed(value),
            GrpcWatchEvent::Changed(value) => Event::Changed(*value),
            GrpcWatchEvent::Error(value) => Event::Error(value),
        }),
    })
}

pub(super) fn watch_room_members_event(message: ServerMessage) -> Option<WatchRoomMembersEvent> {
    use synctv_proto::client::watch_room_members_event::Event;
    watch_event_from_server_message(message, |event| WatchRoomMembersEvent {
        event: Some(match event {
            GrpcWatchEvent::Observed(value) => Event::Observed(value),
            GrpcWatchEvent::Changed(value) => Event::Changed(*value),
            GrpcWatchEvent::Error(value) => Event::Error(value),
        }),
    })
}

pub(super) fn watch_chat_events_event(message: ServerMessage) -> Option<WatchChatEventsEvent> {
    use synctv_proto::client::watch_chat_events_event::Event;
    watch_event_from_server_message(message, |event| WatchChatEventsEvent {
        event: Some(match event {
            GrpcWatchEvent::Observed(value) => Event::Observed(value),
            GrpcWatchEvent::Changed(value) => Event::Changed(*value),
            GrpcWatchEvent::Error(value) => Event::Error(value),
        }),
    })
}

/// gRPC stream implementation of `StreamMessage` trait
///
/// Adapts `tonic::Streaming<ClientMessage>` + `mpsc::Sender<ServerMessage>` to the
/// unified `StreamMessage` interface, enabling full code reuse with the WebSocket path.
pub(super) struct GrpcStreamMessage {
    pub(super) client_stream: tonic::Streaming<ClientMessage>,
    pub(super) sender: Arc<GrpcMessageSender>,
    pub(super) alive: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl StreamMessage for GrpcStreamMessage {
    async fn recv(&mut self) -> Option<Result<ClientMessage, String>> {
        match await_grpc_receive_or_response_close(
            self.client_stream.message(),
            self.sender.sender.clone(),
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
        MessageSender::send(&*self.sender, message)
    }

    fn is_alive(&self) -> bool {
        self.alive.load(std::sync::atomic::Ordering::Relaxed) && self.sender.is_alive()
    }
}
