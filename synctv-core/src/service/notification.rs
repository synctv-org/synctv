//! Notification and event broadcasting service
//!
//! Handles broadcasting events to connected clients via WebSocket
//! and Redis Pub/Sub for cross-node messaging.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::{
    models::{RoomId, UserId},
    Result,
};

/// Guest kick reasons
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuestKickReason {
    /// Global guest mode was disabled
    GlobalGuestModeDisabled,
    /// Room guest mode was disabled
    RoomGuestModeDisabled,
    /// Room password was added (guests cannot join password-protected rooms)
    RoomPasswordAdded,
    /// Admin manually kicked the guest
    AdminKick,
}

impl GuestKickReason {
    /// Get human-readable message for the kick reason
    #[must_use]
    pub const fn message(&self) -> &'static str {
        match self {
            Self::GlobalGuestModeDisabled => "Guest mode has been disabled globally",
            Self::RoomGuestModeDisabled => "Guest access has been disabled for this room",
            Self::RoomPasswordAdded => "This room now requires authentication",
            Self::AdminKick => "You have been removed from the room",
        }
    }
}

/// Room event types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum RoomEvent {
    /// User joined the room
    UserJoined { user_id: UserId, username: String },
    /// User left the room
    UserLeft { user_id: UserId, username: String },
    /// Chat message
    ChatMessage {
        message_id: String,
        user_id: UserId,
        username: String,
        content: String,
        timestamp: chrono::DateTime<chrono::Utc>,
    },
    /// Danmaku message
    Danmaku {
        user_id: UserId,
        username: String,
        content: String,
        position: String, // "top", "bottom", or "scrolling"
        timestamp: chrono::DateTime<chrono::Utc>,
    },
    /// Playback state changed
    PlaybackStateChanged {
        playing: bool,
        position: f64,
        speed: f64,
        media_id: Option<String>,
    },
    /// Media added to playlist
    MediaAdded {
        media_id: String,
        title: String,
        url: String,
        position: i32,
    },
    /// Media removed from playlist
    MediaRemoved { media_id: String },
    /// Playlist reordered
    PlaylistReordered { media_ids: Vec<String> },
    /// Member permissions changed
    PermissionChanged {
        user_id: UserId,
        permissions: i64,
    },
    /// Member kicked
    MemberKicked { user_id: UserId },
    /// Guest kicked (for anonymous guests)
    GuestKicked {
        reason: GuestKickReason,
        message: String,
    },
    /// Room settings updated
    SettingsUpdated { settings: serde_json::Value },
    /// Room deleted
    RoomDeleted,
    /// Live stream started (publisher connected)
    StreamStarted {
        media_id: String,
        user_id: UserId,
    },
    /// Live stream stopped (publisher disconnected)
    StreamStopped {
        media_id: String,
        user_id: UserId,
    },
}

impl RoomEvent {
    /// Convert `RoomEvent` to JSON string
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|e| crate::Error::Internal(format!("Failed to serialize event: {e}")))
    }

    /// Get event type name
    #[must_use]
    pub const fn event_type(&self) -> &'static str {
        match self {
            Self::UserJoined { .. } => "user_joined",
            Self::UserLeft { .. } => "user_left",
            Self::ChatMessage { .. } => "chat_message",
            Self::Danmaku { .. } => "danmaku",
            Self::PlaybackStateChanged { .. } => "playback_state_changed",
            Self::MediaAdded { .. } => "media_added",
            Self::MediaRemoved { .. } => "media_removed",
            Self::PlaylistReordered { .. } => "playlist_reordered",
            Self::PermissionChanged { .. } => "permission_changed",
            Self::MemberKicked { .. } => "member_kicked",
            Self::GuestKicked { .. } => "guest_kicked",
            Self::SettingsUpdated { .. } => "settings_updated",
            Self::RoomDeleted => "room_deleted",
            Self::StreamStarted { .. } => "stream_started",
            Self::StreamStopped { .. } => "stream_stopped",
        }
    }
}

/// Event broadcaster trait
///
/// Abstracts the broadcasting mechanism, allowing different implementations
/// (WebSocket, Redis Pub/Sub, etc.)
#[async_trait::async_trait]
pub trait EventBroadcaster: Send + Sync {
    /// Broadcast an event to a room
    async fn broadcast_to_room(&self, room_id: &RoomId, event: &RoomEvent) -> Result<usize>;

    /// Send an event to a specific user in a room
    async fn send_to_user(&self, room_id: &RoomId, user_id: &UserId, event: &RoomEvent) -> Result<bool>;

    /// Broadcast to all nodes in cluster
    async fn broadcast_to_cluster(&self, room_id: &RoomId, event: &RoomEvent) -> Result<()>;
}

/// Notification service configuration
#[derive(Clone, Debug)]
pub struct NotificationConfig {
    /// Channel capacity for broadcast events
    pub channel_capacity: usize,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            channel_capacity: 1000,
        }
    }
}

/// Notification service
///
/// Provides a high-level API for broadcasting room events.
/// Uses an `EventBroadcaster` implementation for actual distribution.
#[derive(Clone)]
pub struct NotificationService {
    /// Event broadcaster
    broadcaster: Arc<dyn EventBroadcaster>,
    /// Broadcast channel for local event subscribers
    event_tx: broadcast::Sender<(RoomId, RoomEvent)>,
    /// Configuration
    config: NotificationConfig,
}

impl std::fmt::Debug for NotificationService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotificationService")
            .field("config", &self.config)
            .field("has_broadcaster", &"true")
            .finish()
    }
}

impl NotificationService {
    /// Create a new notification service with a broadcaster
    pub fn new(broadcaster: Arc<dyn EventBroadcaster>) -> Self {
        Self::with_config(broadcaster, NotificationConfig::default())
    }

    /// Create a new notification service with custom configuration
    pub fn with_config(
        broadcaster: Arc<dyn EventBroadcaster>,
        config: NotificationConfig,
    ) -> Self {
        let (event_tx, _) = broadcast::channel(config.channel_capacity);

        Self {
            broadcaster,
            event_tx,
            config,
        }
    }

    /// Subscribe to room events locally
    ///
    /// Returns a receiver that can be used to receive events for all rooms.
    /// This is useful for components that need to react to all room events.
    #[must_use] 
    pub fn subscribe(&self) -> broadcast::Receiver<(RoomId, RoomEvent)> {
        self.event_tx.subscribe()
    }

    /// Get the event broadcaster
    #[must_use] 
    pub fn broadcaster(&self) -> &Arc<dyn EventBroadcaster> {
        &self.broadcaster
    }

    /// Broadcast an event to all members of a room
    ///
    /// This sends the event to:
    /// 1. All WebSocket subscribers via the broadcaster
    /// 2. All local broadcast channel subscribers
    pub async fn broadcast_to_room(&self, room_id: &RoomId, event: RoomEvent) -> Result<()> {
        tracing::trace!(
            "Broadcasting event {} to room {}",
            event.event_type(),
            room_id.as_str()
        );

        // Send to broadcast channel (for local subscribers)
        let _ = self.event_tx.send((room_id.clone(), event.clone()));

        // Broadcast via broadcaster implementation (local WebSocket connections)
        let sent_count = self.broadcaster.broadcast_to_room(room_id, &event).await?;

        // Broadcast to cluster (Redis Pub/Sub) so other nodes deliver the event
        if let Err(e) = self.broadcaster.broadcast_to_cluster(room_id, &event).await {
            tracing::warn!(
                "Failed to broadcast event {} to cluster for room {}: {}",
                event.event_type(),
                room_id.as_str(),
                e
            );
        }

        tracing::debug!(
            "Broadcast event {} to room {}: {} local recipients",
            event.event_type(),
            room_id.as_str(),
            sent_count
        );

        Ok(())
    }

    /// Broadcast an event to a specific user in a room
    pub async fn send_to_user(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        event: RoomEvent,
    ) -> Result<()> {
        tracing::trace!(
            "Sending event {} to user {} in room {}",
            event.event_type(),
            user_id.as_str(),
            room_id.as_str()
        );

        let sent = self.broadcaster.send_to_user(room_id, user_id, &event).await?;

        if !sent {
            tracing::warn!(
                "No active connection found for user {} in room {}",
                user_id.as_str(),
                room_id.as_str()
            );
        }

        Ok(())
    }

    /// Broadcast to all nodes in cluster (via Redis Pub/Sub)
    pub async fn broadcast_to_cluster(
        &self,
        room_id: &RoomId,
        event: RoomEvent,
    ) -> Result<()> {
        tracing::trace!(
            "Broadcasting event {} to cluster for room {}",
            event.event_type(),
            room_id.as_str()
        );

        self.broadcaster.broadcast_to_cluster(room_id, &event).await
    }

    /// Notify room members that a user joined
    pub async fn notify_user_joined(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
    ) -> Result<()> {
        let event = RoomEvent::UserJoined {
            user_id: user_id.clone(),
            username: username.to_string(),
        };
        self.broadcast_to_room(room_id, event).await
    }

    /// Notify room members that a user left
    pub async fn notify_user_left(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
    ) -> Result<()> {
        let event = RoomEvent::UserLeft {
            user_id: user_id.clone(),
            username: username.to_string(),
        };
        self.broadcast_to_room(room_id, event).await
    }

    /// Broadcast chat message
    pub async fn notify_chat_message(
        &self,
        room_id: &RoomId,
        message_id: &str,
        user_id: &UserId,
        username: &str,
        content: &str,
    ) -> Result<()> {
        let event = RoomEvent::ChatMessage {
            message_id: message_id.to_string(),
            user_id: user_id.clone(),
            username: username.to_string(),
            content: content.to_string(),
            timestamp: chrono::Utc::now(),
        };
        self.broadcast_to_room(room_id, event).await
    }

    /// Broadcast danmaku message
    pub async fn notify_danmaku(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        content: &str,
        position: &str,
    ) -> Result<()> {
        let event = RoomEvent::Danmaku {
            user_id: user_id.clone(),
            username: username.to_string(),
            content: content.to_string(),
            position: position.to_string(),
            timestamp: chrono::Utc::now(),
        };
        self.broadcast_to_room(room_id, event).await
    }

    /// Notify playback state change
    pub async fn notify_playback_state_changed(
        &self,
        room_id: &RoomId,
        playing: bool,
        position: f64,
        speed: f64,
        media_id: Option<String>,
    ) -> Result<()> {
        let event = RoomEvent::PlaybackStateChanged {
            playing,
            position,
            speed,
            media_id,
        };
        self.broadcast_to_room(room_id, event).await
    }

    /// Notify media added
    pub async fn notify_media_added(
        &self,
        room_id: &RoomId,
        media_id: &str,
        title: &str,
        url: &str,
        position: i32,
    ) -> Result<()> {
        let event = RoomEvent::MediaAdded {
            media_id: media_id.to_string(),
            title: title.to_string(),
            url: url.to_string(),
            position,
        };
        self.broadcast_to_room(room_id, event).await
    }

    /// Notify media removed
    pub async fn notify_media_removed(
        &self,
        room_id: &RoomId,
        media_id: &str,
    ) -> Result<()> {
        let event = RoomEvent::MediaRemoved {
            media_id: media_id.to_string(),
        };
        self.broadcast_to_room(room_id, event).await
    }

    /// Notify permission changed
    pub async fn notify_permission_changed(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permissions: i64,
    ) -> Result<()> {
        let event = RoomEvent::PermissionChanged {
            user_id: user_id.clone(),
            permissions,
        };
        self.broadcast_to_room(room_id, event).await
    }

    /// Notify member kicked
    pub async fn notify_member_kicked(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<()> {
        let event = RoomEvent::MemberKicked {
            user_id: user_id.clone(),
        };
        self.broadcast_to_room(room_id, event).await
    }

    /// Notify settings updated
    pub async fn notify_settings_updated(
        &self,
        room_id: &RoomId,
        settings: serde_json::Value,
    ) -> Result<()> {
        let event = RoomEvent::SettingsUpdated { settings };
        self.broadcast_to_room(room_id, event).await
    }

    /// Notify room deleted
    pub async fn notify_room_deleted(&self, room_id: &RoomId) -> Result<()> {
        let event = RoomEvent::RoomDeleted;
        self.broadcast_to_room(room_id, event).await
    }

    /// Notify room members that a live stream started
    pub async fn notify_stream_started(
        &self,
        room_id: &RoomId,
        media_id: &str,
        user_id: &UserId,
    ) -> Result<()> {
        let event = RoomEvent::StreamStarted {
            media_id: media_id.to_string(),
            user_id: user_id.clone(),
        };
        self.broadcast_to_room(room_id, event).await
    }

    /// Notify room members that a live stream stopped
    pub async fn notify_stream_stopped(
        &self,
        room_id: &RoomId,
        media_id: &str,
        user_id: &UserId,
    ) -> Result<()> {
        let event = RoomEvent::StreamStopped {
            media_id: media_id.to_string(),
            user_id: user_id.clone(),
        };
        self.broadcast_to_room(room_id, event).await
    }

    /// Kick all guests from a room
    ///
    /// This sends a `GuestKicked` event to all guest connections in the room.
    /// The actual disconnection logic should be handled by the WebSocket server
    /// or connection manager when they receive this event.
    ///
    /// # Arguments
    /// * `room_id` - Room ID to kick guests from
    /// * `reason` - Reason for kicking guests
    pub async fn kick_all_guests(
        &self,
        room_id: &RoomId,
        reason: GuestKickReason,
    ) -> Result<()> {
        let message = reason.message().to_string();
        let event = RoomEvent::GuestKicked {
            reason,
            message,
        };

        tracing::info!(
            "Kicking all guests from room {} due to: {}",
            room_id.as_str(),
            event.event_type()
        );

        self.broadcast_to_room(room_id, event).await
    }
}

impl Default for NotificationService {
    fn default() -> Self {
        // Use a no-op broadcaster as default — logs a warning on first broadcast
        // so operators notice that notifications are silently dropped.
        struct NoOpBroadcaster {
            warned: std::sync::Once,
        }

        impl NoOpBroadcaster {
            fn warn_once(&self) {
                self.warned.call_once(|| {
                    tracing::warn!(
                        "NotificationService is using a no-op broadcaster (created via Default). \
                         All room event notifications will be silently dropped. \
                         Call NotificationService::new() with a real EventBroadcaster to enable notifications."
                    );
                });
            }
        }

        #[async_trait::async_trait]
        impl EventBroadcaster for NoOpBroadcaster {
            async fn broadcast_to_room(&self, _room_id: &RoomId, _event: &RoomEvent) -> Result<usize> {
                self.warn_once();
                Ok(0)
            }

            async fn send_to_user(&self, _room_id: &RoomId, _user_id: &UserId, _event: &RoomEvent) -> Result<bool> {
                self.warn_once();
                Ok(false)
            }

            async fn broadcast_to_cluster(&self, _room_id: &RoomId, _event: &RoomEvent) -> Result<()> {
                self.warn_once();
                Ok(())
            }
        }

        Self::new(Arc::new(NoOpBroadcaster { warned: std::sync::Once::new() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_room_event_serialization() {
        let event = RoomEvent::UserJoined {
            user_id: UserId::from_string("user123".to_string()),
            username: "testuser".to_string(),
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"UserJoined""#));
        assert!(json.contains("user123"));
    }

    #[tokio::test]
    async fn test_notification_service_creation() {
        struct MockBroadcaster;

        #[async_trait::async_trait]
        impl EventBroadcaster for MockBroadcaster {
            async fn broadcast_to_room(&self, _room_id: &RoomId, _event: &RoomEvent) -> Result<usize> {
                Ok(0)
            }

            async fn send_to_user(&self, _room_id: &RoomId, _user_id: &UserId, _event: &RoomEvent) -> Result<bool> {
                Ok(false)
            }

            async fn broadcast_to_cluster(&self, _room_id: &RoomId, _event: &RoomEvent) -> Result<()> {
                Ok(())
            }
        }

        let service = NotificationService::new(Arc::new(MockBroadcaster));
        assert_eq!(service.config.channel_capacity, 1000);
    }

    #[tokio::test]
    async fn test_subscribe_and_broadcast() {
        struct MockBroadcaster;

        #[async_trait::async_trait]
        impl EventBroadcaster for MockBroadcaster {
            async fn broadcast_to_room(&self, _room_id: &RoomId, _event: &RoomEvent) -> Result<usize> {
                Ok(1)
            }

            async fn send_to_user(&self, _room_id: &RoomId, _user_id: &UserId, _event: &RoomEvent) -> Result<bool> {
                Ok(true)
            }

            async fn broadcast_to_cluster(&self, _room_id: &RoomId, _event: &RoomEvent) -> Result<()> {
                Ok(())
            }
        }

        let service = NotificationService::new(Arc::new(MockBroadcaster));

        // Subscribe to events
        let mut rx = service.subscribe();

        // Create test room and user
        let room_id = RoomId::from_string("test_room".to_string());
        let user_id = UserId::from_string("test_user".to_string());

        // Broadcast user joined event
        let event = RoomEvent::UserJoined {
            user_id: user_id.clone(),
            username: "testuser".to_string(),
        };

        service.broadcast_to_room(&room_id, event).await.unwrap();

        // Receive event
        let (received_room_id, received_event) = tokio::time::timeout(
            tokio::time::Duration::from_millis(100),
            rx.recv()
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(received_room_id, room_id);
        assert!(
            matches!(&received_event, RoomEvent::UserJoined { username, .. } if username == "testuser"),
            "Expected UserJoined event with username 'testuser', got {received_event:?}"
        );
    }

    #[test]
    fn test_room_event_types() {
        // Test that all event types can be serialized
        let events = vec![
            RoomEvent::UserJoined {
                user_id: UserId::new(),
                username: "test".to_string(),
            },
            RoomEvent::UserLeft {
                user_id: UserId::new(),
                username: "test".to_string(),
            },
            RoomEvent::ChatMessage {
                message_id: "msg123".to_string(),
                user_id: UserId::new(),
                username: "test".to_string(),
                content: "hello".to_string(),
                timestamp: chrono::Utc::now(),
            },
            RoomEvent::Danmaku {
                user_id: UserId::new(),
                username: "test".to_string(),
                content: "hello".to_string(),
                position: "top".to_string(),
                timestamp: chrono::Utc::now(),
            },
            RoomEvent::PlaybackStateChanged {
                playing: true,
                position: 100.0,
                speed: 1.0,
                media_id: Some("media123".to_string()),
            },
            RoomEvent::MediaAdded {
                media_id: "media123".to_string(),
                title: "Test Video".to_string(),
                url: "http://example.com/video.mp4".to_string(),
                position: 1,
            },
            RoomEvent::MediaRemoved {
                media_id: "media123".to_string(),
            },
            RoomEvent::PlaylistReordered {
                media_ids: vec!["media1".to_string(), "media2".to_string()],
            },
            RoomEvent::PermissionChanged {
                user_id: UserId::new(),
                permissions: 123,
            },
            RoomEvent::MemberKicked {
                user_id: UserId::new(),
            },
            RoomEvent::GuestKicked {
                reason: GuestKickReason::RoomGuestModeDisabled,
                message: "Guest access has been disabled for this room".to_string(),
            },
            RoomEvent::SettingsUpdated {
                settings: serde_json::json!({"key": "value"}),
            },
            RoomEvent::RoomDeleted,
            RoomEvent::StreamStarted {
                media_id: "media123".to_string(),
                user_id: UserId::new(),
            },
            RoomEvent::StreamStopped {
                media_id: "media123".to_string(),
                user_id: UserId::new(),
            },
        ];

        for event in events {
            let json = event.to_json().unwrap();
            assert!(!json.is_empty());
        }
    }
}
