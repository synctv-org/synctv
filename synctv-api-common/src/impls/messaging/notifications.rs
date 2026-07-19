use synctv_proto::client::ServerMessage;

use super::codec::required_realtime_text;

pub(super) fn user_notification_server_message(
    notification_id: impl Into<String>,
    notification_type: synctv_core::models::NotificationType,
    data: &synctv_core::models::NotificationData,
    title: impl Into<String>,
    content: impl Into<String>,
    timestamp: chrono::DateTime<chrono::Utc>,
) -> ServerMessage {
    let notification_id = notification_id.into();
    let title = title.into();
    let content = content.into();

    ServerMessage {
        message: Some(synctv_proto::client::server_message::Message::Notification(
            synctv_proto::client::UserNotification {
                notification_id,
                notification_type: crate::impls::notification::notification_type_to_proto(
                    notification_type,
                ) as i32,
                title,
                content,
                data: Some(crate::impls::notification::notification_data_to_proto(data)),
                timestamp: timestamp.timestamp(),
            },
        )),
    }
}

pub(super) fn system_notification_server_message(
    message: impl Into<String>,
    timestamp: chrono::DateTime<chrono::Utc>,
) -> Result<ServerMessage, String> {
    let message = required_realtime_text(&message.into(), "system notification message", 1024)?;

    Ok(ServerMessage {
        message: Some(synctv_proto::client::server_message::Message::Notification(
            synctv_proto::client::UserNotification {
                notification_id: String::new(),
                notification_type: crate::impls::notification::notification_type_to_proto(
                    synctv_core::models::NotificationType::SystemAnnouncement,
                ) as i32,
                title: message.clone(),
                content: message,
                data: Some(crate::impls::notification::notification_data_to_proto(
                    &synctv_core::models::NotificationData::default(),
                )),
                timestamp: timestamp.timestamp(),
            },
        )),
    })
}
