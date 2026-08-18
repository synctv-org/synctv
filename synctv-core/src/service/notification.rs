//! Notification and event broadcasting service
//!
//! Provides local in-process room event fan-out for services that need a simple
//! domain notification channel.

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::{
    models::{MediaId, PlaylistId, RealtimeEvent, RoomId, RoomRole, RoomSettings, UserId},
    Result,
};

/// Guest kick reasons
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GuestKickReason {
    /// Global guest mode was disabled
    GlobalGuestModeDisabled,
    /// Room guest mode was disabled
    RoomGuestModeDisabled,
    /// Room was removed from public access
    RoomMadePrivate,
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
            Self::RoomMadePrivate => "This room is no longer public",
            Self::RoomPasswordAdded => "This room now requires authentication",
            Self::AdminKick => "You have been removed from the room",
        }
    }
}

/// Room event types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
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
    /// Media added to playlist
    MediaAdded {
        user_id: UserId,
        username: String,
        media_id: MediaId,
        title: String,
        url: String,
        position: f64,
    },
    /// Media removed from playlist
    MediaRemoved {
        user_id: Option<UserId>,
        username: String,
        media_id: MediaId,
    },
    /// Media updated (name or position changed)
    MediaUpdated {
        user_id: UserId,
        username: String,
        media_id: MediaId,
        title: String,
        position: f64,
    },
    /// Playlist reordered
    PlaylistReordered {
        user_id: Option<UserId>,
        username: String,
        media_ids: Vec<MediaId>,
    },
    /// Playlist deleted
    PlaylistDeleted {
        user_id: Option<UserId>,
        username: String,
        playlist_id: PlaylistId,
    },
    /// Member permissions changed
    PermissionChanged {
        user_id: UserId,
        role: RoomRole,
        effective_permissions: u64,
        added_permissions: u64,
        removed_permissions: u64,
        admin_added_permissions: u64,
        admin_removed_permissions: u64,
        updated_by_user_id: UserId,
        updated_by_username: String,
    },
    /// Member kicked
    MemberKicked { user_id: UserId },
    /// Guest kicked (for anonymous guests)
    GuestKicked {
        reason: GuestKickReason,
        message: String,
    },
    /// Room settings updated
    SettingsUpdated {
        settings: RoomSettings,
        version: i64,
        user_id: Option<UserId>,
        username: String,
    },
    /// Room deleted
    RoomDeleted,
    /// Live stream started (publisher connected)
    StreamStarted { media_id: MediaId, user_id: UserId },
    /// Live stream stopped (publisher disconnected)
    StreamStopped { media_id: MediaId, user_id: UserId },
}

#[derive(Debug, Clone)]
pub struct MediaAddedNotification<'a> {
    pub user_id: &'a UserId,
    pub username: &'a str,
    pub media_id: MediaId,
    pub title: &'a str,
    pub url: &'a str,
    pub position: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct PermissionChangedNotification<'a> {
    pub user_id: &'a UserId,
    pub role: RoomRole,
    pub effective_permissions: u64,
    pub added_permissions: u64,
    pub removed_permissions: u64,
    pub admin_added_permissions: u64,
    pub admin_removed_permissions: u64,
    pub updated_by_user_id: &'a UserId,
    pub updated_by_username: &'a str,
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
            Self::MediaAdded { .. } => "media_added",
            Self::MediaRemoved { .. } => "media_removed",
            Self::MediaUpdated { .. } => "media_updated",
            Self::PlaylistReordered { .. } => "playlist_reordered",
            Self::PlaylistDeleted { .. } => "playlist_deleted",
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
/// Provides a high-level API for publishing in-process room domain events.
///
/// This service is intentionally local-only. Cross-connection and cross-node
/// realtime delivery is handled by dedicated runtime broadcasters elsewhere.
#[derive(Clone)]
pub struct NotificationService {
    /// Broadcast channel for local event subscribers
    event_tx: broadcast::Sender<(RoomId, RoomEvent)>,
    /// Events created inside a database transaction and ready for local delivery.
    committed_realtime_event_tx: broadcast::Sender<RealtimeEvent>,
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
    /// Create a new notification service with custom configuration
    pub fn new_with_config(config: NotificationConfig) -> Self {
        let (event_tx, _) = broadcast::channel(config.channel_capacity);
        let (committed_realtime_event_tx, _) = broadcast::channel(config.channel_capacity);

        Self {
            event_tx,
            committed_realtime_event_tx,
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

    /// Subscribe to durable realtime events after their database transaction commits.
    #[must_use]
    pub fn subscribe_committed_realtime_events(&self) -> broadcast::Receiver<RealtimeEvent> {
        self.committed_realtime_event_tx.subscribe()
    }

    /// Deliver a durable realtime event to the local runtime after commit.
    #[must_use]
    pub fn notify_committed_realtime_event(&self, event: RealtimeEvent) -> usize {
        match self.committed_realtime_event_tx.send(event) {
            Ok(count) => count,
            Err(error) => {
                tracing::trace!(
                    event_type = %error.0.event_type(),
                    event_id = %error.0.event_id(),
                    "Committed realtime event has no local subscribers"
                );
                0
            }
        }
    }

    /// Publish a room-scoped domain event to local subscribers.
    #[must_use]
    pub fn broadcast_to_room(&self, room_id: &RoomId, event: &RoomEvent) -> usize {
        tracing::trace!(
            "Publishing local room event {} for room {}",
            event.event_type(),
            room_id
        );

        let subscriber_count = match self.event_tx.send((*room_id, event.clone())) {
            Ok(count) => count,
            Err(error) => {
                tracing::trace!(
                    "Local room event {} for room {} has no active subscribers: {error}",
                    event.event_type(),
                    room_id
                );
                0
            }
        };

        tracing::debug!(
            subscriber_count,
            "Published local room event {} for room {}",
            event.event_type(),
            room_id
        );

        subscriber_count
    }

    /// Notify room members that a user joined
    pub fn notify_user_joined(&self, room_id: &RoomId, user_id: &UserId, username: &str) -> usize {
        let event = RoomEvent::UserJoined {
            user_id: *user_id,
            username: username.to_string(),
        };
        self.broadcast_to_room(room_id, &event)
    }

    /// Notify room members that a user left
    pub fn notify_user_left(&self, room_id: &RoomId, user_id: &UserId, username: &str) -> usize {
        let event = RoomEvent::UserLeft {
            user_id: *user_id,
            username: username.to_string(),
        };
        self.broadcast_to_room(room_id, &event)
    }

    /// Broadcast chat message
    pub fn notify_chat_message(
        &self,
        room_id: &RoomId,
        message_id: &str,
        user_id: &UserId,
        username: &str,
        content: &str,
    ) -> usize {
        let event = RoomEvent::ChatMessage {
            message_id: message_id.to_string(),
            user_id: *user_id,
            username: username.to_string(),
            content: content.to_string(),
            timestamp: crate::SystemClock.now(),
        };
        self.broadcast_to_room(room_id, &event)
    }

    /// Notify media added
    pub fn notify_media_added(
        &self,
        room_id: &RoomId,
        notification: &MediaAddedNotification<'_>,
    ) -> usize {
        let event = RoomEvent::MediaAdded {
            user_id: *notification.user_id,
            username: notification.username.to_string(),
            media_id: notification.media_id,
            title: notification.title.to_string(),
            url: notification.url.to_string(),
            position: notification.position,
        };
        self.broadcast_to_room(room_id, &event)
    }

    /// Notify media removed
    pub fn notify_media_removed(
        &self,
        room_id: &RoomId,
        user_id: Option<&UserId>,
        username: &str,
        media_id: MediaId,
    ) -> usize {
        let event = RoomEvent::MediaRemoved {
            user_id: user_id.copied(),
            username: username.to_string(),
            media_id,
        };
        self.broadcast_to_room(room_id, &event)
    }

    /// Notify media updated
    pub fn notify_media_updated(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        username: &str,
        media_id: MediaId,
        title: &str,
        position: f64,
    ) -> usize {
        let event = RoomEvent::MediaUpdated {
            user_id: *user_id,
            username: username.to_string(),
            media_id,
            title: title.to_string(),
            position,
        };
        self.broadcast_to_room(room_id, &event)
    }

    /// Notify that media order changed within a playlist scope.
    pub fn notify_playlist_reordered(
        &self,
        room_id: &RoomId,
        user_id: Option<&UserId>,
        username: &str,
        media_ids: &[MediaId],
    ) -> usize {
        let event = RoomEvent::PlaylistReordered {
            user_id: user_id.copied(),
            username: username.to_string(),
            media_ids: media_ids.to_vec(),
        };
        self.broadcast_to_room(room_id, &event)
    }

    /// Notify playlist deleted.
    pub fn notify_playlist_deleted(
        &self,
        room_id: &RoomId,
        user_id: Option<&UserId>,
        username: &str,
        playlist_id: PlaylistId,
    ) -> usize {
        let event = RoomEvent::PlaylistDeleted {
            user_id: user_id.copied(),
            username: username.to_string(),
            playlist_id,
        };
        self.broadcast_to_room(room_id, &event)
    }

    /// Notify permission changed
    pub fn notify_permission_changed(
        &self,
        room_id: &RoomId,
        notification: PermissionChangedNotification<'_>,
    ) -> usize {
        let event = RoomEvent::PermissionChanged {
            user_id: *notification.user_id,
            role: notification.role,
            effective_permissions: notification.effective_permissions,
            added_permissions: notification.added_permissions,
            removed_permissions: notification.removed_permissions,
            admin_added_permissions: notification.admin_added_permissions,
            admin_removed_permissions: notification.admin_removed_permissions,
            updated_by_user_id: *notification.updated_by_user_id,
            updated_by_username: notification.updated_by_username.to_string(),
        };
        self.broadcast_to_room(room_id, &event)
    }

    /// Notify member kicked
    pub fn notify_member_kicked(&self, room_id: &RoomId, user_id: &UserId) -> usize {
        let event = RoomEvent::MemberKicked { user_id: *user_id };
        self.broadcast_to_room(room_id, &event)
    }

    /// Notify settings updated
    pub fn notify_settings_updated(
        &self,
        room_id: &RoomId,
        user_id: Option<&UserId>,
        username: &str,
        settings: RoomSettings,
        version: i64,
    ) -> usize {
        let event = RoomEvent::SettingsUpdated {
            settings,
            version,
            user_id: user_id.copied(),
            username: username.to_string(),
        };
        self.broadcast_to_room(room_id, &event)
    }

    /// Notify room deleted
    pub fn notify_room_deleted(&self, room_id: &RoomId) -> usize {
        let event = RoomEvent::RoomDeleted;
        self.broadcast_to_room(room_id, &event)
    }

    /// Notify room members that a live stream started
    pub fn notify_stream_started(
        &self,
        room_id: &RoomId,
        media_id: &MediaId,
        user_id: &UserId,
    ) -> usize {
        let event = RoomEvent::StreamStarted {
            media_id: *media_id,
            user_id: *user_id,
        };
        self.broadcast_to_room(room_id, &event)
    }

    /// Notify room members that a live stream stopped
    pub fn notify_stream_stopped(
        &self,
        room_id: &RoomId,
        media_id: &MediaId,
        user_id: &UserId,
    ) -> usize {
        let event = RoomEvent::StreamStopped {
            media_id: *media_id,
            user_id: *user_id,
        };
        self.broadcast_to_room(room_id, &event)
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
    pub fn kick_all_guests(&self, room_id: &RoomId, reason: GuestKickReason) -> usize {
        let message = reason.message().to_string();
        let event = RoomEvent::GuestKicked { reason, message };

        tracing::info!(
            "Kicking all guests from room {} due to: {}",
            room_id,
            event.event_type()
        );

        self.broadcast_to_room(room_id, &event)
    }
}

impl Default for NotificationService {
    fn default() -> Self {
        Self::new_with_config(NotificationConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    #[tokio::test]
    async fn test_subscribe_and_broadcast() {
        let service = NotificationService::default();

        // Subscribe to events
        let mut rx = service.subscribe();

        // Create test room and user
        let room_id = RoomId::expect_positive(1);
        let user_id = UserId::expect_positive(2);

        // Broadcast user joined event
        let event = RoomEvent::UserJoined {
            user_id,
            username: "testuser".to_string(),
        };

        assert_eq!(service.broadcast_to_room(&room_id, &event), 1);

        // Receive event
        let received = ok(
            tokio::time::timeout(tokio::time::Duration::from_millis(100), rx.recv()).await,
            "room event should arrive",
        );
        let (received_room_id, received_event) = ok(received, "broadcast channel should stay open");

        assert_eq!(received_room_id, room_id);
        assert!(
            matches!(&received_event, RoomEvent::UserJoined { username, .. } if username == "testuser"),
            "Expected UserJoined event with username 'testuser', got {received_event:?}"
        );
    }

    #[tokio::test]
    async fn test_local_only_notification_service_keeps_in_process_subscribers_working() {
        let service = NotificationService::default();
        let mut rx = service.subscribe();
        let room_id = RoomId::expect_positive(3);
        let user_id = UserId::expect_positive(4);

        assert_eq!(
            service.broadcast_to_room(
                &room_id,
                &RoomEvent::UserJoined {
                    user_id,
                    username: "local-only".to_string(),
                },
            ),
            1
        );

        let received = ok(
            tokio::time::timeout(tokio::time::Duration::from_millis(100), rx.recv()).await,
            "local-only event should arrive",
        );
        let (received_room_id, received_event) = ok(received, "broadcast channel should stay open");

        assert_eq!(received_room_id, room_id);
        assert!(matches!(
            received_event,
            RoomEvent::UserJoined { username, .. } if username == "local-only"
        ));
    }

    #[tokio::test]
    async fn test_broadcast_without_subscribers_reports_zero_delivery() {
        let service = NotificationService::default();
        let room_id = RoomId::expect_positive(5);
        let user_id = UserId::expect_positive(6);
        let event = RoomEvent::UserJoined {
            user_id,
            username: "no-subscriber".to_string(),
        };

        assert_eq!(service.broadcast_to_room(&room_id, &event), 0);
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
                timestamp: crate::SystemClock.now(),
            },
            RoomEvent::MediaAdded {
                user_id: UserId::new(),
                username: "test".to_string(),
                media_id: MediaId::expect_positive(123),
                title: "Test Video".to_string(),
                url: "http://example.com/video.mp4".to_string(),
                position: 1.0,
            },
            RoomEvent::MediaRemoved {
                user_id: Some(UserId::new()),
                username: "test".to_string(),
                media_id: MediaId::expect_positive(123),
            },
            RoomEvent::MediaUpdated {
                user_id: UserId::new(),
                username: "test".to_string(),
                media_id: MediaId::expect_positive(123),
                title: "Updated Video".to_string(),
                position: 2.0,
            },
            RoomEvent::PlaylistReordered {
                user_id: Some(UserId::new()),
                username: "test".to_string(),
                media_ids: vec![MediaId::expect_positive(1), MediaId::expect_positive(2)],
            },
            RoomEvent::PlaylistDeleted {
                user_id: Some(UserId::new()),
                username: "test".to_string(),
                playlist_id: PlaylistId::expect_positive(123),
            },
            RoomEvent::PermissionChanged {
                user_id: UserId::new(),
                role: RoomRole::Creator,
                effective_permissions: 123,
                added_permissions: 1,
                removed_permissions: 2,
                admin_added_permissions: 4,
                admin_removed_permissions: 8,
                updated_by_user_id: UserId::new(),
                updated_by_username: "test".to_string(),
            },
            RoomEvent::MemberKicked {
                user_id: UserId::new(),
            },
            RoomEvent::GuestKicked {
                reason: GuestKickReason::RoomGuestModeDisabled,
                message: "Guest access has been disabled for this room".to_string(),
            },
            RoomEvent::SettingsUpdated {
                settings: RoomSettings::default(),
                version: 1,
                user_id: Some(UserId::new()),
                username: "test".to_string(),
            },
            RoomEvent::RoomDeleted,
            RoomEvent::StreamStarted {
                media_id: MediaId::expect_positive(123),
                user_id: UserId::new(),
            },
            RoomEvent::StreamStopped {
                media_id: MediaId::expect_positive(123),
                user_id: UserId::new(),
            },
        ];

        for event in events {
            let json = ok(event.to_json(), "room event should serialize");
            assert!(!json.is_empty());
        }
    }
}
