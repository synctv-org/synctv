use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use synctv_core::models::id::{MediaId, RoomId, UserId};
use synctv_core::models::permission::PermissionBits;
use synctv_core::models::playback::RoomPlaybackState;

/// The kind of cache to invalidate in a `CacheInvalidate` cluster event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheTarget {
    /// Invalidate a specific user's cached data
    User { user_id: String },
    /// Invalidate a specific user's username cache
    Username { user_id: String },
    /// Invalidate a specific room's cached data
    Room { room_id: String },
    /// Invalidate all caches (full flush)
    All,
}

/// Events that are synchronized across cluster nodes via Redis Pub/Sub.
///
/// Each event carries a unique `event_id` (nanoid) used as the primary
/// deduplication key, avoiding reliance on content hashing which can have
/// collisions under high throughput.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClusterEvent {
    /// Chat message sent in a room
    /// If position is set, this can be displayed as a danmaku (bullet comment)
    ChatMessage {
        #[serde(default = "generate_event_id")]
        event_id: String,
        room_id: RoomId,
        user_id: UserId,
        username: String,
        message: String,
        timestamp: DateTime<Utc>,
        /// Video position in seconds (for danmaku display)
        position: Option<f64>,
        /// Hex color (for danmaku display)
        color: Option<String>,
    },

    /// Room playback state changed (play, pause, seek, etc.)
    PlaybackStateChanged {
        #[serde(default = "generate_event_id")]
        event_id: String,
        room_id: RoomId,
        user_id: UserId,
        username: String,
        state: RoomPlaybackState,
        timestamp: DateTime<Utc>,
    },

    /// User joined a room
    UserJoined {
        #[serde(default = "generate_event_id")]
        event_id: String,
        room_id: RoomId,
        user_id: UserId,
        username: String,
        permissions: PermissionBits,
        timestamp: DateTime<Utc>,
    },

    /// User left a room
    UserLeft {
        #[serde(default = "generate_event_id")]
        event_id: String,
        room_id: RoomId,
        user_id: UserId,
        username: String,
        timestamp: DateTime<Utc>,
    },

    /// Media added to room playlist
    MediaAdded {
        #[serde(default = "generate_event_id")]
        event_id: String,
        room_id: RoomId,
        user_id: UserId,
        username: String,
        media_id: MediaId,
        media_title: String,
        timestamp: DateTime<Utc>,
    },

    /// Media removed from room playlist
    MediaRemoved {
        #[serde(default = "generate_event_id")]
        event_id: String,
        room_id: RoomId,
        user_id: UserId,
        username: String,
        media_id: MediaId,
        timestamp: DateTime<Utc>,
    },

    /// User permissions changed in a room
    PermissionChanged {
        #[serde(default = "generate_event_id")]
        event_id: String,
        room_id: RoomId,
        target_user_id: UserId,
        target_username: String,
        changed_by: UserId,
        changed_by_username: String,
        new_permissions: PermissionBits,
        timestamp: DateTime<Utc>,
    },

    /// Room settings updated
    RoomSettingsChanged {
        #[serde(default = "generate_event_id")]
        event_id: String,
        room_id: RoomId,
        user_id: UserId,
        username: String,
        /// Serialized settings JSON (bytes)
        #[serde(default)]
        settings_json: Vec<u8>,
        timestamp: DateTime<Utc>,
    },

    /// WebRTC signaling message (offer, answer, `ice_candidate`)
    WebRTCSignaling {
        #[serde(default = "generate_event_id")]
        event_id: String,
        room_id: RoomId,
        message_type: String, // "offer", "answer", "ice_candidate"
        from: String,         // "user_id:conn_id" (server-set, prevents forgery)
        to: String,           // "user_id:conn_id"
        data: String,         // Opaque SDP/ICE data
        timestamp: DateTime<Utc>,
    },

    /// User joined WebRTC call in room
    WebRTCJoin {
        #[serde(default = "generate_event_id")]
        event_id: String,
        room_id: RoomId,
        user_id: UserId,
        conn_id: String,
        username: String,
        timestamp: DateTime<Utc>,
    },

    /// User left WebRTC call in room
    WebRTCLeave {
        #[serde(default = "generate_event_id")]
        event_id: String,
        room_id: RoomId,
        user_id: UserId,
        conn_id: String,
        timestamp: DateTime<Utc>,
    },

    /// Notification for all clients (system-wide)
    SystemNotification {
        #[serde(default = "generate_event_id")]
        event_id: String,
        message: String,
        level: NotificationLevel,
        timestamp: DateTime<Utc>,
    },

    /// Kick an active publisher (RTMP stream termination).
    /// Broadcast cluster-wide when admin bans user/room or deletes media/room.
    KickPublisher {
        #[serde(default = "generate_event_id")]
        event_id: String,
        room_id: RoomId,
        media_id: MediaId,
        reason: String,
        timestamp: DateTime<Utc>,
    },

    /// Kick all active publishers for a user across all replicas.
    /// Broadcast cluster-wide when a user is banned.
    KickUser {
        #[serde(default = "generate_event_id")]
        event_id: String,
        user_id: UserId,
        reason: String,
        timestamp: DateTime<Utc>,
    },

    /// A new room was created.
    /// Broadcast cluster-wide so other replicas can update room lists / caches.
    RoomCreated {
        #[serde(default = "generate_event_id")]
        event_id: String,
        room_id: RoomId,
        room_name: String,
        creator_id: UserId,
        timestamp: DateTime<Utc>,
    },

    /// A room was deleted.
    /// Broadcast cluster-wide so other replicas can evict caches,
    /// disconnect users, and terminate active streams.
    RoomDeleted {
        #[serde(default = "generate_event_id")]
        event_id: String,
        room_id: RoomId,
        /// The user who initiated the deletion (may be the room creator or an admin).
        deleted_by: UserId,
        timestamp: DateTime<Utc>,
    },

    /// Generic cache invalidation event.
    ///
    /// Broadcast cluster-wide when a service mutates data that is cached on
    /// other replicas (user profile, room metadata, username, etc.).
    /// Receiving nodes should invalidate the specified cache targets in their
    /// local L1 caches.
    CacheInvalidate {
        #[serde(default = "generate_event_id")]
        event_id: String,
        /// One or more cache targets to invalidate.
        targets: Vec<CacheTarget>,
        timestamp: DateTime<Utc>,
    },
}

/// Generate a unique event ID using nanoid
fn generate_event_id() -> String {
    nanoid::nanoid!(16)
}

/// Notification severity level
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationLevel {
    Info,
    Warning,
    Error,
}

impl ClusterEvent {
    /// Get the unique event ID for deduplication
    #[must_use]
    pub fn event_id(&self) -> &str {
        match self {
            Self::ChatMessage { event_id, .. }
            | Self::PlaybackStateChanged { event_id, .. }
            | Self::UserJoined { event_id, .. }
            | Self::UserLeft { event_id, .. }
            | Self::MediaAdded { event_id, .. }
            | Self::MediaRemoved { event_id, .. }
            | Self::PermissionChanged { event_id, .. }
            | Self::RoomSettingsChanged { event_id, .. }
            | Self::WebRTCSignaling { event_id, .. }
            | Self::WebRTCJoin { event_id, .. }
            | Self::WebRTCLeave { event_id, .. }
            | Self::SystemNotification { event_id, .. }
            | Self::KickPublisher { event_id, .. }
            | Self::KickUser { event_id, .. }
            | Self::RoomCreated { event_id, .. }
            | Self::RoomDeleted { event_id, .. }
            | Self::CacheInvalidate { event_id, .. } => event_id,
        }
    }

    /// Get the room ID for events that belong to a specific room
    #[must_use]
    pub const fn room_id(&self) -> Option<&RoomId> {
        match self {
            Self::ChatMessage { room_id, .. }
            | Self::PlaybackStateChanged { room_id, .. }
            | Self::UserJoined { room_id, .. }
            | Self::UserLeft { room_id, .. }
            | Self::MediaAdded { room_id, .. }
            | Self::MediaRemoved { room_id, .. }
            | Self::PermissionChanged { room_id, .. }
            | Self::RoomSettingsChanged { room_id, .. }
            | Self::WebRTCSignaling { room_id, .. }
            | Self::WebRTCJoin { room_id, .. }
            | Self::WebRTCLeave { room_id, .. }
            | Self::KickPublisher { room_id, .. }
            | Self::RoomCreated { room_id, .. }
            | Self::RoomDeleted { room_id, .. } => Some(room_id),
            Self::SystemNotification { .. } | Self::KickUser { .. }
            | Self::CacheInvalidate { .. } => None,
        }
    }

    /// Get the user ID that initiated this event
    #[must_use]
    pub const fn user_id(&self) -> Option<&UserId> {
        match self {
            Self::ChatMessage { user_id, .. }
            | Self::PlaybackStateChanged { user_id, .. }
            | Self::UserJoined { user_id, .. }
            | Self::UserLeft { user_id, .. }
            | Self::MediaAdded { user_id, .. }
            | Self::MediaRemoved { user_id, .. }
            | Self::RoomSettingsChanged { user_id, .. }
            | Self::WebRTCJoin { user_id, .. }
            | Self::WebRTCLeave { user_id, .. }
            | Self::KickUser { user_id, .. } => Some(user_id),
            Self::RoomCreated { creator_id, .. } => Some(creator_id),
            Self::RoomDeleted { deleted_by, .. } => Some(deleted_by),
            Self::PermissionChanged { changed_by, .. } => Some(changed_by),
            Self::WebRTCSignaling { .. } | Self::SystemNotification { .. }
            | Self::KickPublisher { .. } | Self::CacheInvalidate { .. } => None,
        }
    }

    /// Get the timestamp of this event
    #[must_use]
    pub const fn timestamp(&self) -> &DateTime<Utc> {
        match self {
            Self::ChatMessage { timestamp, .. }
            | Self::PlaybackStateChanged { timestamp, .. }
            | Self::UserJoined { timestamp, .. }
            | Self::UserLeft { timestamp, .. }
            | Self::MediaAdded { timestamp, .. }
            | Self::MediaRemoved { timestamp, .. }
            | Self::PermissionChanged { timestamp, .. }
            | Self::RoomSettingsChanged { timestamp, .. }
            | Self::WebRTCSignaling { timestamp, .. }
            | Self::WebRTCJoin { timestamp, .. }
            | Self::WebRTCLeave { timestamp, .. }
            | Self::SystemNotification { timestamp, .. }
            | Self::KickPublisher { timestamp, .. }
            | Self::KickUser { timestamp, .. }
            | Self::RoomCreated { timestamp, .. }
            | Self::RoomDeleted { timestamp, .. }
            | Self::CacheInvalidate { timestamp, .. } => timestamp,
        }
    }

    /// Whether this is a critical event that must not be silently dropped.
    /// Critical events affect user access and administrative actions.
    #[must_use]
    pub const fn is_critical(&self) -> bool {
        matches!(
            self,
            Self::KickPublisher { .. }
            | Self::KickUser { .. }
            | Self::PermissionChanged { .. }
            | Self::PlaybackStateChanged { .. }
            | Self::RoomDeleted { .. }
        )
    }

    /// Extra discriminator for deduplication of events without `room_id/user_id`.
    /// Returns empty string for most events; non-empty for `SystemNotification`.
    #[must_use]
    pub fn dedup_extra(&self) -> String {
        match self {
            Self::SystemNotification { message, level, .. } => {
                format!("{level:?}:{message}")
            }
            Self::KickUser { user_id, .. } => {
                format!("kick_user:{}", user_id.as_str())
            }
            _ => String::new(),
        }
    }

    /// Get a short description of the event type
    #[must_use]
    pub const fn event_type(&self) -> &'static str {
        match self {
            Self::ChatMessage { .. } => "chat_message",
            Self::PlaybackStateChanged { .. } => "playback_state_changed",
            Self::UserJoined { .. } => "user_joined",
            Self::UserLeft { .. } => "user_left",
            Self::MediaAdded { .. } => "media_added",
            Self::MediaRemoved { .. } => "media_removed",
            Self::PermissionChanged { .. } => "permission_changed",
            Self::RoomSettingsChanged { .. } => "room_settings_changed",
            Self::WebRTCSignaling { .. } => "webrtc_signaling",
            Self::WebRTCJoin { .. } => "webrtc_join",
            Self::WebRTCLeave { .. } => "webrtc_leave",
            Self::SystemNotification { .. } => "system_notification",
            Self::KickPublisher { .. } => "kick_publisher",
            Self::KickUser { .. } => "kick_user",
            Self::RoomCreated { .. } => "room_created",
            Self::RoomDeleted { .. } => "room_deleted",
            Self::CacheInvalidate { .. } => "cache_invalidate",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cluster_event_serialization() {
        let event = ClusterEvent::ChatMessage {
            event_id: generate_event_id(),
            room_id: RoomId::from_string("room123".to_string()),
            user_id: UserId::from_string("user456".to_string()),
            username: "testuser".to_string(),
            message: "Hello world!".to_string(),
            timestamp: Utc::now(),
            position: None,
            color: None,
        };

        // Serialize to JSON
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("chat_message"));
        assert!(json.contains("Hello world!"));

        // Deserialize back
        let deserialized: ClusterEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.event_type(), "chat_message");
    }

    #[test]
    fn test_cluster_event_room_id() {
        let event = ClusterEvent::UserJoined {
            event_id: generate_event_id(),
            room_id: RoomId::from_string("room123".to_string()),
            user_id: UserId::from_string("user456".to_string()),
            username: "testuser".to_string(),
            permissions: PermissionBits(0),
            timestamp: Utc::now(),
        };

        assert_eq!(event.room_id().unwrap().as_str(), "room123");
        assert_eq!(event.user_id().unwrap().as_str(), "user456");
    }

    #[test]
    fn test_system_notification_no_room() {
        let event = ClusterEvent::SystemNotification {
            event_id: generate_event_id(),
            message: "Server maintenance in 1 hour".to_string(),
            level: NotificationLevel::Warning,
            timestamp: Utc::now(),
        };

        assert!(event.room_id().is_none());
        assert!(event.user_id().is_none());
        assert_eq!(event.event_type(), "system_notification");
    }

    #[test]
    fn test_kick_publisher_serialization() {
        let event = ClusterEvent::KickPublisher {
            event_id: generate_event_id(),
            room_id: RoomId::from_string("room123".to_string()),
            media_id: MediaId::from_string("media456".to_string()),
            reason: "user_banned".to_string(),
            timestamp: Utc::now(),
        };

        // Serialize to JSON
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("kick_publisher"));
        assert!(json.contains("room123"));
        assert!(json.contains("media456"));
        assert!(json.contains("user_banned"));

        // Deserialize back
        let deserialized: ClusterEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.event_type(), "kick_publisher");
        assert_eq!(deserialized.room_id().unwrap().as_str(), "room123");
        assert!(deserialized.user_id().is_none());

        if let ClusterEvent::KickPublisher { room_id, media_id, reason, .. } = &deserialized {
            assert_eq!(room_id.as_str(), "room123");
            assert_eq!(media_id.as_str(), "media456");
            assert_eq!(reason, "user_banned");
        } else {
            panic!("Expected KickPublisher variant");
        }
    }

    #[test]
    fn test_kick_publisher_has_room_id_no_user_id() {
        let event = ClusterEvent::KickPublisher {
            event_id: generate_event_id(),
            room_id: RoomId::from_string("room789".to_string()),
            media_id: MediaId::from_string("media012".to_string()),
            reason: "room_deleted".to_string(),
            timestamp: Utc::now(),
        };

        assert_eq!(event.room_id().unwrap().as_str(), "room789");
        assert!(event.user_id().is_none());
        assert_eq!(event.event_type(), "kick_publisher");
    }

    #[test]
    fn test_kick_user_serialization() {
        let event = ClusterEvent::KickUser {
            event_id: generate_event_id(),
            user_id: UserId::from_string("user123".to_string()),
            reason: "user_banned".to_string(),
            timestamp: Utc::now(),
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("kick_user"));
        assert!(json.contains("user123"));

        let deserialized: ClusterEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.event_type(), "kick_user");
        assert!(deserialized.room_id().is_none());
        assert_eq!(deserialized.user_id().unwrap().as_str(), "user123");
    }

    #[test]
    fn test_kick_user_dedup_extra() {
        let event = ClusterEvent::KickUser {
            event_id: generate_event_id(),
            user_id: UserId::from_string("user456".to_string()),
            reason: "user_banned".to_string(),
            timestamp: Utc::now(),
        };

        assert_eq!(event.dedup_extra(), "kick_user:user456");
    }

    #[test]
    fn test_cache_invalidate_serialization() {
        let event = ClusterEvent::CacheInvalidate {
            event_id: generate_event_id(),
            targets: vec![
                CacheTarget::User { user_id: "u1".to_string() },
                CacheTarget::Room { room_id: "r1".to_string() },
                CacheTarget::Username { user_id: "u2".to_string() },
                CacheTarget::All,
            ],
            timestamp: Utc::now(),
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("cache_invalidate"));
        assert!(json.contains("\"u1\""));
        assert!(json.contains("\"r1\""));

        let deserialized: ClusterEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.event_type(), "cache_invalidate");
        assert!(deserialized.room_id().is_none());
        assert!(deserialized.user_id().is_none());

        if let ClusterEvent::CacheInvalidate { targets, .. } = &deserialized {
            assert_eq!(targets.len(), 4);
        } else {
            panic!("Expected CacheInvalidate variant");
        }
    }
}
