use synctv_proto::client::ServerMessage;

use super::codec::required_realtime_text;

pub(super) fn user_notification_server_message(
    notification_id: impl Into<String>,
    notification_type: impl Into<String>,
    title: impl Into<String>,
    content: impl Into<String>,
    timestamp: chrono::DateTime<chrono::Utc>,
) -> ServerMessage {
    let notification_id = notification_id.into();
    let notification_type = notification_type.into();
    let title = title.into();
    let content = content.into();
    let data = serde_json::json!({
        "type": "user_notification",
        "notification_id": &notification_id,
        "notification_type": &notification_type,
        "title": &title,
        "content": &content,
    });

    ServerMessage {
        message: Some(synctv_proto::client::server_message::Message::Notification(
            synctv_proto::client::UserNotification {
                notification_id,
                notification_type,
                title,
                content,
                data: data.to_string(),
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
    let data = serde_json::json!({
        "type": "system_notification",
        "notification_type": "system_announcement",
        "title": &message,
        "content": &message,
    });

    Ok(ServerMessage {
        message: Some(synctv_proto::client::server_message::Message::Notification(
            synctv_proto::client::UserNotification {
                notification_id: String::new(),
                notification_type: "system_announcement".to_string(),
                title: message.clone(),
                content: message,
                data: data.to_string(),
                timestamp: timestamp.timestamp(),
            },
        )),
    })
}
