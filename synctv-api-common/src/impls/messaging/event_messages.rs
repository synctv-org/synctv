use super::notifications::system_notification_server_message;
use synctv_proto::client::ServerMessage;

/// Convert a realtime event into one or more server messages.
pub(super) fn realtime_event_to_server_messages(
    event: &synctv_realtime::sync::RealtimeEvent,
    _room_id: &str,
    _public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<Vec<ServerMessage>, String> {
    use synctv_proto::client::server_message::Message;
    use synctv_proto::client::{ErrorMessage, ServerMessage};
    use synctv_realtime::sync::RealtimeEvent;

    let messages = match event {
        RealtimeEvent::SystemNotification {
            message, timestamp, ..
        } => vec![system_notification_server_message(
            message.clone(),
            *timestamp,
        )?],
        RealtimeEvent::RoomDeleted { .. } => {
            // Notify WebSocket clients that the room has been deleted
            vec![ServerMessage {
                message: Some(Message::Error(ErrorMessage {
                    message: "Room has been deleted".to_string(),
                    code: crate::impls::error_codes::NOT_FOUND,
                    detail: String::new(),
                })),
            }]
        }
        RealtimeEvent::RoomBanned { .. } => {
            vec![ServerMessage {
                message: Some(Message::Error(ErrorMessage {
                    message: "Room has been banned".to_string(),
                    code: crate::impls::error_codes::FORBIDDEN,
                    detail: String::new(),
                })),
            }]
        }
        RealtimeEvent::RoomOwnerInactive { .. } => {
            vec![ServerMessage {
                message: Some(Message::Error(ErrorMessage {
                    message: "Room is unavailable because its creator is not active".to_string(),
                    code: crate::impls::error_codes::FORBIDDEN,
                    detail: String::new(),
                })),
            }]
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
