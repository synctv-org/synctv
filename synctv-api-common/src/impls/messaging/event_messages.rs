use super::notifications::system_notification_server_message;
use synctv_proto::client::{RealtimeTerminationCode, ServerMessage};

/// Build a terminal realtime message that is delivered before the transport
/// is closed. The dedicated code is stable for client-side classification.
pub(super) fn realtime_termination_server_message(
    message: impl Into<String>,
    code: RealtimeTerminationCode,
) -> ServerMessage {
    ServerMessage {
        message: Some(synctv_proto::client::server_message::Message::Termination(
            synctv_proto::client::RealtimeTermination {
                message: message.into(),
                code: code as i32,
            },
        )),
    }
}

/// Convert a realtime event into one or more server messages.
pub(super) fn realtime_event_to_server_messages(
    event: &synctv_realtime::sync::RealtimeEvent,
    _room_id: &str,
    _public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<Vec<ServerMessage>, String> {
    use synctv_realtime::sync::RealtimeEvent;

    let messages = match event {
        RealtimeEvent::SystemNotification {
            message, timestamp, ..
        } => vec![system_notification_server_message(
            message.clone(),
            *timestamp,
        )?],
        RealtimeEvent::RoomDeleted { .. } => {
            vec![realtime_termination_server_message(
                "Room has been deleted",
                RealtimeTerminationCode::RoomDeleted,
            )]
        }
        RealtimeEvent::RoomBanned { .. } => {
            vec![realtime_termination_server_message(
                "Room has been banned",
                RealtimeTerminationCode::RoomBanned,
            )]
        }
        RealtimeEvent::RoomOwnerInactive { .. } => {
            vec![realtime_termination_server_message(
                "Room is unavailable because its creator is not active",
                RealtimeTerminationCode::RoomOwnerInactive,
            )]
        }
        RealtimeEvent::KickPublisher { .. }
        | RealtimeEvent::KickUser { .. }
        | RealtimeEvent::KickUserFromRoom { .. }
        | RealtimeEvent::RoomCreated { .. }
        | RealtimeEvent::CacheInvalidate { .. }
        | RealtimeEvent::ProviderCredentialChanged { .. }
        | RealtimeEvent::UserNotification { .. } => {
            // Admin/internal events are handled by other channels,
            // not forwarded to WebSocket clients via the room event path
            vec![]
        }
        _ => Vec::new(),
    };
    Ok(messages)
}
