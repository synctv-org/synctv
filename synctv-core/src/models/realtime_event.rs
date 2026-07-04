use crate::models::id::{MediaId, PlaylistId, RoomId, UserId};
use crate::models::notification::{NotificationData, NotificationType};
use crate::models::playback::RoomPlaybackState;
use crate::models::RoomPermissionSet;
use crate::models::{ChatMessageEvent, ChatPinEvent, Playlist, RoomSettings};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The kind of cache to invalidate in a `CacheInvalidate` realtime event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CacheTarget {
    /// Invalidate a specific user's cached data
    User { user_id: UserId },
    /// Invalidate a specific user's username cache
    Username { user_id: UserId },
    /// Invalidate a specific room's cached data
    Room { room_id: RoomId },
    /// Invalidate all caches (full flush)
    All,
}

/// Client/subscriber route used for local and cross-replica realtime delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RealtimeDeliveryRoute {
    /// Deliver to subscribers of the event's room channel.
    Room,
    /// Deliver only through the replica-wide admin channel.
    Admin,
    /// Deliver to both the room channel and replica-wide admin channel.
    RoomAndAdmin,
}

/// WebRTC signaling payload kind routed between room peers.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WebRTCSignalKind {
    Offer,
    Answer,
    IceCandidate,
}

/// Events that are synchronized across cluster nodes via Redis Pub/Sub.
///
/// Each event carries a unique `event_id` (shared base62 ID) used as the primary
/// deduplication key, avoiding reliance on content hashing which can have
/// collisions under high throughput.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RealtimeEvent {
    /// Chat message sent in a room.
    ChatMessage {
        event_id: String,
        room_id: RoomId,
        user_id: UserId,
        username: String,
        message: String,
        timestamp: DateTime<Utc>,
        /// Optional chat presentation placement.
        display_position: Option<String>,
        /// Optional chat presentation color.
        display_color: Option<String>,
    },

    /// Durable chat event for create/edit/delete message state changes.
    ChatMessageEvent {
        event_id: String,
        room_id: RoomId,
        actor_user_id: UserId,
        event: ChatMessageEvent,
        timestamp: DateTime<Utc>,
    },

    /// Durable chat pin resource event for pin list changes.
    ChatPinEvent {
        event_id: String,
        room_id: RoomId,
        actor_user_id: UserId,
        event: ChatPinEvent,
        timestamp: DateTime<Utc>,
    },

    /// Room playback state changed (play, pause, seek, etc.)
    PlaybackStateChanged {
        event_id: String,
        room_id: RoomId,
        user_id: UserId,
        username: String,
        state: RoomPlaybackState,
        #[serde(default)]
        source_changed: bool,
        timestamp: DateTime<Utc>,
    },

    /// User joined a room
    UserJoined {
        event_id: String,
        room_id: RoomId,
        user_id: UserId,
        username: String,
        #[serde(default)]
        remark_name: String,
        #[serde(default)]
        display_tag: String,
        permissions: RoomPermissionSet,
        role: i32, // RoomMemberRole as i32 for serde compatibility
        #[serde(default)]
        added_permissions: RoomPermissionSet,
        #[serde(default)]
        removed_permissions: RoomPermissionSet,
        #[serde(default)]
        admin_added_permissions: RoomPermissionSet,
        #[serde(default)]
        admin_removed_permissions: RoomPermissionSet,
        #[serde(default = "chrono::Utc::now")]
        joined_at: DateTime<Utc>,
        timestamp: DateTime<Utc>,
    },

    /// Stateless guest joined a public room.
    GuestJoined {
        event_id: String,
        room_id: RoomId,
        guest_id: String,
        username: String,
        permissions: RoomPermissionSet,
        role: i32,
        joined_at: DateTime<Utc>,
        timestamp: DateTime<Utc>,
    },

    /// User left a room
    UserLeft {
        event_id: String,
        room_id: RoomId,
        user_id: UserId,
        username: String,
        #[serde(default)]
        remark_name: String,
        #[serde(default)]
        display_tag: String,
        #[serde(default)]
        role: i32,
        timestamp: DateTime<Utc>,
    },

    /// Stateless guest left a public room.
    GuestLeft {
        event_id: String,
        room_id: RoomId,
        guest_id: String,
        username: String,
        timestamp: DateTime<Utc>,
    },

    /// Media added to room playlist
    MediaAdded {
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
        event_id: String,
        room_id: RoomId,
        user_id: UserId,
        username: String,
        media_id: MediaId,
        timestamp: DateTime<Utc>,
    },

    /// Media metadata updated in room playlist
    MediaUpdated {
        event_id: String,
        room_id: RoomId,
        user_id: UserId,
        username: String,
        media_id: MediaId,
        media_title: String,
        timestamp: DateTime<Utc>,
    },

    /// Batch of media removed from room playlist (efficient for bulk deletions)
    /// Instead of sending 100 individual MediaRemoved events, send one batch event.
    MediaRemovedBatch {
        event_id: String,
        room_id: RoomId,
        user_id: UserId,
        username: String,
        /// List of media IDs that were removed
        media_ids: Vec<MediaId>,
        timestamp: DateTime<Utc>,
    },

    /// Playlist media order changed in a room.
    PlaylistReordered {
        event_id: String,
        room_id: RoomId,
        user_id: UserId,
        username: String,
        media_ids: Vec<MediaId>,
        timestamp: DateTime<Utc>,
    },

    /// Playlist created in a room.
    PlaylistCreated {
        event_id: String,
        room_id: RoomId,
        user_id: UserId,
        username: String,
        playlist: Playlist,
        timestamp: DateTime<Utc>,
    },

    /// Playlist updated in a room.
    PlaylistUpdated {
        event_id: String,
        room_id: RoomId,
        user_id: UserId,
        username: String,
        playlist: Playlist,
        timestamp: DateTime<Utc>,
    },

    /// Playlist deleted from a room.
    PlaylistDeleted {
        event_id: String,
        room_id: RoomId,
        user_id: UserId,
        username: String,
        playlist_id: PlaylistId,
        timestamp: DateTime<Utc>,
    },

    /// User permissions changed in a room
    PermissionChanged {
        event_id: String,
        room_id: RoomId,
        target_user_id: UserId,
        target_username: String,
        #[serde(default)]
        target_remark_name: String,
        #[serde(default)]
        target_display_tag: String,
        changed_by: UserId,
        changed_by_username: String,
        #[serde(default)]
        role_changed: bool,
        new_permissions: RoomPermissionSet,
        role: i32, // RoomMemberRole as i32 for serde compatibility
        added_permissions: RoomPermissionSet,
        removed_permissions: RoomPermissionSet,
        #[serde(default)]
        admin_added_permissions: RoomPermissionSet,
        #[serde(default)]
        admin_removed_permissions: RoomPermissionSet,
        #[serde(default)]
        target_is_online: bool,
        #[serde(default)]
        target_connection_count: usize,
        timestamp: DateTime<Utc>,
    },

    /// Room settings updated
    RoomSettingsChanged {
        event_id: String,
        room_id: RoomId,
        user_id: UserId,
        username: String,
        settings: RoomSettings,
        version: i64,
        timestamp: DateTime<Utc>,
    },

    /// WebRTC signaling message.
    WebRTCSignaling {
        event_id: String,
        room_id: RoomId,
        message_type: WebRTCSignalKind,
        /// Server-set field: `"<user_id>|<conn_id>"`.
        ///
        /// The `|` separator is used instead of `:` because user IDs may contain
        /// colons (e.g., namespaced IDs).  Using `|` makes parsing unambiguous:
        /// `from.splitn(2, '|')` always yields exactly `[user_id, conn_id]`.
        from: String,
        /// Client-provided target: `"<user_id>:<conn_id>"`.
        ///
        /// Parsed on the server with `rsplit_once(':')` so that colons in the
        /// user_id portion are handled safely.
        to: String,
        data: String, // Opaque SDP/ICE data
        timestamp: DateTime<Utc>,
    },

    /// Actor joined WebRTC call in room.
    ///
    /// `actor_id` is the public realtime actor identifier used by clients in
    /// signaling targets. Signed-in users use `usr_*`; guests use `gst_*`.
    WebRTCJoin {
        event_id: String,
        room_id: RoomId,
        actor_id: String,
        conn_id: String,
        username: String,
        timestamp: DateTime<Utc>,
    },

    /// Actor left WebRTC call in room.
    WebRTCLeave {
        event_id: String,
        room_id: RoomId,
        actor_id: String,
        conn_id: String,
        timestamp: DateTime<Utc>,
    },

    /// Notification for all clients (system-wide)
    SystemNotification {
        event_id: String,
        message: String,
        level: NotificationLevel,
        timestamp: DateTime<Utc>,
    },

    /// Kick an active publisher (RTMP stream termination).
    /// Broadcast replica-wide when admin bans user/room or deletes media/room.
    KickPublisher {
        event_id: String,
        room_id: RoomId,
        media_id: MediaId,
        reason: String,
        timestamp: DateTime<Utc>,
    },

    /// Kick all active publishers for a user across all replicas.
    /// Broadcast replica-wide when a user is banned.
    KickUser {
        event_id: String,
        user_id: UserId,
        reason: String,
        timestamp: DateTime<Utc>,
    },

    /// Kick a user from a specific room across all replicas.
    /// Broadcast replica-wide when a member is kicked or banned from a room,
    /// so other replicas can force-disconnect the user's connections to that room.
    KickUserFromRoom {
        event_id: String,
        room_id: RoomId,
        user_id: UserId,
        reason: String,
        timestamp: DateTime<Utc>,
    },

    /// A new room was created.
    /// Broadcast replica-wide so other replicas can update room lists / caches.
    RoomCreated {
        event_id: String,
        room_id: RoomId,
        room_name: String,
        creator_id: UserId,
        timestamp: DateTime<Utc>,
    },

    /// A room was deleted.
    /// Broadcast replica-wide so other replicas can evict caches,
    /// disconnect users, and terminate active streams.
    RoomDeleted {
        event_id: String,
        room_id: RoomId,
        /// The user who initiated the deletion (may be the room creator or an admin).
        deleted_by: UserId,
        timestamp: DateTime<Utc>,
    },

    /// A room was forcibly banned by an administrator.
    ///
    /// Broadcast replica-wide so other replicas can evict active members,
    /// terminate room-scoped streams, and reject further participation while
    /// preserving the room record itself.
    RoomBanned {
        event_id: String,
        room_id: RoomId,
        banned_by: UserId,
        timestamp: DateTime<Utc>,
    },

    /// A room became unavailable because its creator account is no longer active.
    ///
    /// Broadcast replica-wide so replicas disconnect active members, invalidate
    /// room-related caches, and stop allowing normal room participation.
    RoomOwnerInactive {
        event_id: String,
        room_id: RoomId,
        owner_id: UserId,
        triggered_by: UserId,
        timestamp: DateTime<Utc>,
    },

    /// A persistent user notification was created.
    ///
    /// Broadcast replica-wide so that the node hosting the user's active WebSocket
    /// connection can push the notification in real time instead of requiring the
    /// client to poll.
    UserNotification {
        event_id: String,
        /// The user who should receive the notification.
        user_id: UserId,
        /// Notification ID (UUID string) for client-side deduplication.
        notification_id: String,
        /// Notification title for display.
        title: String,
        /// Notification content for display.
        content: String,
        notification_type: NotificationType,
        data: NotificationData,
        timestamp: DateTime<Utc>,
    },

    /// A user's provider credential was created, replaced, or removed.
    ///
    /// Broadcast on the admin channel because the affected WebSocket
    /// connections may be in any room. Consumers should use provider-owned
    /// credential dependency resolution to decide whether a watched playback
    /// snapshot must be refreshed.
    ProviderCredentialChanged {
        event_id: String,
        user_id: UserId,
        provider: String,
        server_id: String,
        timestamp: DateTime<Utc>,
    },

    /// Generic cache invalidation event.
    ///
    /// Broadcast replica-wide when a service mutates data that is cached on
    /// other replicas (user profile, room metadata, username, etc.).
    /// Receiving nodes should invalidate the specified cache targets in their
    /// local L1 caches.
    CacheInvalidate {
        event_id: String,
        /// One or more cache targets to invalidate.
        targets: Vec<CacheTarget>,
        timestamp: DateTime<Utc>,
    },
}

/// Notification severity level
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationLevel {
    Info,
    Warning,
    Error,
}

impl RealtimeEvent {
    /// Get the unique event ID for deduplication
    #[must_use]
    pub fn event_id(&self) -> &str {
        match self {
            Self::ChatMessage { event_id, .. }
            | Self::ChatMessageEvent { event_id, .. }
            | Self::ChatPinEvent { event_id, .. }
            | Self::PlaybackStateChanged { event_id, .. }
            | Self::UserJoined { event_id, .. }
            | Self::GuestJoined { event_id, .. }
            | Self::UserLeft { event_id, .. }
            | Self::GuestLeft { event_id, .. }
            | Self::MediaAdded { event_id, .. }
            | Self::MediaRemoved { event_id, .. }
            | Self::MediaUpdated { event_id, .. }
            | Self::MediaRemovedBatch { event_id, .. }
            | Self::PlaylistReordered { event_id, .. }
            | Self::PlaylistCreated { event_id, .. }
            | Self::PlaylistUpdated { event_id, .. }
            | Self::PlaylistDeleted { event_id, .. }
            | Self::PermissionChanged { event_id, .. }
            | Self::RoomSettingsChanged { event_id, .. }
            | Self::WebRTCSignaling { event_id, .. }
            | Self::WebRTCJoin { event_id, .. }
            | Self::WebRTCLeave { event_id, .. }
            | Self::SystemNotification { event_id, .. }
            | Self::KickPublisher { event_id, .. }
            | Self::KickUser { event_id, .. }
            | Self::KickUserFromRoom { event_id, .. }
            | Self::RoomCreated { event_id, .. }
            | Self::RoomDeleted { event_id, .. }
            | Self::RoomBanned { event_id, .. }
            | Self::RoomOwnerInactive { event_id, .. }
            | Self::UserNotification { event_id, .. }
            | Self::ProviderCredentialChanged { event_id, .. }
            | Self::CacheInvalidate { event_id, .. } => event_id,
        }
    }

    /// Get the room ID for events that belong to a specific room
    #[must_use]
    pub const fn room_id(&self) -> Option<&RoomId> {
        match self {
            Self::ChatMessage { room_id, .. }
            | Self::ChatMessageEvent { room_id, .. }
            | Self::ChatPinEvent { room_id, .. }
            | Self::PlaybackStateChanged { room_id, .. }
            | Self::UserJoined { room_id, .. }
            | Self::GuestJoined { room_id, .. }
            | Self::UserLeft { room_id, .. }
            | Self::GuestLeft { room_id, .. }
            | Self::MediaAdded { room_id, .. }
            | Self::MediaRemoved { room_id, .. }
            | Self::MediaUpdated { room_id, .. }
            | Self::MediaRemovedBatch { room_id, .. }
            | Self::PlaylistReordered { room_id, .. }
            | Self::PlaylistCreated { room_id, .. }
            | Self::PlaylistUpdated { room_id, .. }
            | Self::PlaylistDeleted { room_id, .. }
            | Self::PermissionChanged { room_id, .. }
            | Self::RoomSettingsChanged { room_id, .. }
            | Self::WebRTCSignaling { room_id, .. }
            | Self::WebRTCJoin { room_id, .. }
            | Self::WebRTCLeave { room_id, .. }
            | Self::KickPublisher { room_id, .. }
            | Self::KickUserFromRoom { room_id, .. }
            | Self::RoomCreated { room_id, .. }
            | Self::RoomDeleted { room_id, .. }
            | Self::RoomBanned { room_id, .. }
            | Self::RoomOwnerInactive { room_id, .. } => Some(room_id),
            Self::SystemNotification { .. }
            | Self::KickUser { .. }
            | Self::UserNotification { .. }
            | Self::ProviderCredentialChanged { .. }
            | Self::CacheInvalidate { .. } => None,
        }
    }

    /// Delivery route for local subscribers and Redis fanout.
    #[must_use]
    pub const fn delivery_route(&self) -> RealtimeDeliveryRoute {
        match self {
            Self::RoomCreated { .. }
            | Self::KickPublisher { .. }
            | Self::KickUserFromRoom { .. }
            | Self::RoomDeleted { .. }
            | Self::RoomBanned { .. }
            | Self::RoomOwnerInactive { .. } => RealtimeDeliveryRoute::RoomAndAdmin,
            Self::KickUser { .. }
            | Self::UserNotification { .. }
            | Self::ProviderCredentialChanged { .. }
            | Self::CacheInvalidate { .. }
            | Self::SystemNotification { .. } => RealtimeDeliveryRoute::Admin,
            Self::ChatMessage { .. }
            | Self::ChatMessageEvent { .. }
            | Self::ChatPinEvent { .. }
            | Self::PlaybackStateChanged { .. }
            | Self::UserJoined { .. }
            | Self::GuestJoined { .. }
            | Self::UserLeft { .. }
            | Self::GuestLeft { .. }
            | Self::MediaAdded { .. }
            | Self::MediaRemoved { .. }
            | Self::MediaUpdated { .. }
            | Self::MediaRemovedBatch { .. }
            | Self::PlaylistReordered { .. }
            | Self::PlaylistCreated { .. }
            | Self::PlaylistUpdated { .. }
            | Self::PlaylistDeleted { .. }
            | Self::PermissionChanged { .. }
            | Self::RoomSettingsChanged { .. }
            | Self::WebRTCSignaling { .. }
            | Self::WebRTCJoin { .. }
            | Self::WebRTCLeave { .. } => RealtimeDeliveryRoute::Room,
        }
    }

    #[must_use]
    pub const fn delivers_to_admin_channel(&self) -> bool {
        matches!(
            self.delivery_route(),
            RealtimeDeliveryRoute::Admin | RealtimeDeliveryRoute::RoomAndAdmin
        )
    }

    #[must_use]
    pub const fn delivers_to_room_channel(&self) -> bool {
        matches!(
            self.delivery_route(),
            RealtimeDeliveryRoute::Room | RealtimeDeliveryRoute::RoomAndAdmin
        )
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
            | Self::MediaUpdated { user_id, .. }
            | Self::MediaRemovedBatch { user_id, .. }
            | Self::PlaylistReordered { user_id, .. }
            | Self::PlaylistCreated { user_id, .. }
            | Self::PlaylistUpdated { user_id, .. }
            | Self::PlaylistDeleted { user_id, .. }
            | Self::RoomSettingsChanged { user_id, .. }
            | Self::KickUser { user_id, .. }
            | Self::KickUserFromRoom { user_id, .. }
            | Self::UserNotification { user_id, .. }
            | Self::ProviderCredentialChanged { user_id, .. } => Some(user_id),
            Self::ChatMessageEvent { actor_user_id, .. }
            | Self::ChatPinEvent { actor_user_id, .. } => Some(actor_user_id),
            Self::RoomCreated { creator_id, .. } => Some(creator_id),
            Self::RoomDeleted { deleted_by, .. } => Some(deleted_by),
            Self::RoomBanned { banned_by, .. } => Some(banned_by),
            Self::RoomOwnerInactive { triggered_by, .. } => Some(triggered_by),
            Self::PermissionChanged { changed_by, .. } => Some(changed_by),
            Self::WebRTCSignaling { .. }
            | Self::WebRTCJoin { .. }
            | Self::WebRTCLeave { .. }
            | Self::GuestJoined { .. }
            | Self::GuestLeft { .. }
            | Self::SystemNotification { .. }
            | Self::KickPublisher { .. }
            | Self::CacheInvalidate { .. } => None,
        }
    }

    /// Get the timestamp of this event
    #[must_use]
    pub const fn timestamp(&self) -> &DateTime<Utc> {
        match self {
            Self::ChatMessage { timestamp, .. }
            | Self::ChatMessageEvent { timestamp, .. }
            | Self::ChatPinEvent { timestamp, .. }
            | Self::PlaybackStateChanged { timestamp, .. }
            | Self::UserJoined { timestamp, .. }
            | Self::GuestJoined { timestamp, .. }
            | Self::UserLeft { timestamp, .. }
            | Self::GuestLeft { timestamp, .. }
            | Self::MediaAdded { timestamp, .. }
            | Self::MediaRemoved { timestamp, .. }
            | Self::MediaUpdated { timestamp, .. }
            | Self::MediaRemovedBatch { timestamp, .. }
            | Self::PlaylistReordered { timestamp, .. }
            | Self::PlaylistCreated { timestamp, .. }
            | Self::PlaylistUpdated { timestamp, .. }
            | Self::PlaylistDeleted { timestamp, .. }
            | Self::PermissionChanged { timestamp, .. }
            | Self::RoomSettingsChanged { timestamp, .. }
            | Self::WebRTCSignaling { timestamp, .. }
            | Self::WebRTCJoin { timestamp, .. }
            | Self::WebRTCLeave { timestamp, .. }
            | Self::SystemNotification { timestamp, .. }
            | Self::KickPublisher { timestamp, .. }
            | Self::KickUser { timestamp, .. }
            | Self::KickUserFromRoom { timestamp, .. }
            | Self::RoomCreated { timestamp, .. }
            | Self::RoomDeleted { timestamp, .. }
            | Self::RoomBanned { timestamp, .. }
            | Self::RoomOwnerInactive { timestamp, .. }
            | Self::UserNotification { timestamp, .. }
            | Self::ProviderCredentialChanged { timestamp, .. }
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
                | Self::KickUserFromRoom { .. }
                | Self::UserLeft { .. }
                | Self::PermissionChanged { .. }
                | Self::RoomBanned { .. }
                | Self::RoomOwnerInactive { .. }
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
            Self::KickUser { user_id, .. } => format!("kick_user:{user_id}"),
            Self::KickUserFromRoom {
                user_id, room_id, ..
            } => format!("kick_user_from_room:{user_id}:{room_id}"),
            Self::RoomBanned { room_id, .. } => format!("room_banned:{room_id}"),
            Self::RoomOwnerInactive { room_id, .. } => {
                format!("room_owner_inactive:{room_id}")
            }
            Self::UserNotification {
                user_id,
                notification_id,
                ..
            } => format!("user_notification:{user_id}:{notification_id}"),
            Self::ProviderCredentialChanged {
                user_id,
                provider,
                server_id,
                ..
            } => format!("provider_credential_changed:{user_id}:{provider}:{server_id}"),
            _ => String::new(),
        }
    }

    /// Get a short description of the event type
    #[must_use]
    pub const fn event_type(&self) -> &'static str {
        match self {
            Self::ChatMessage { .. } => "chat_message",
            Self::ChatMessageEvent { .. } => "chat_message_event",
            Self::ChatPinEvent { .. } => "chat_pin_event",
            Self::PlaybackStateChanged { .. } => "playback_state_changed",
            Self::UserJoined { .. } => "user_joined",
            Self::GuestJoined { .. } => "guest_joined",
            Self::UserLeft { .. } => "user_left",
            Self::GuestLeft { .. } => "guest_left",
            Self::MediaAdded { .. } => "media_added",
            Self::MediaRemoved { .. } => "media_removed",
            Self::MediaUpdated { .. } => "media_updated",
            Self::MediaRemovedBatch { .. } => "media_removed_batch",
            Self::PlaylistReordered { .. } => "playlist_reordered",
            Self::PlaylistCreated { .. } => "playlist_created",
            Self::PlaylistUpdated { .. } => "playlist_updated",
            Self::PlaylistDeleted { .. } => "playlist_deleted",
            Self::PermissionChanged { .. } => "permission_changed",
            Self::RoomSettingsChanged { .. } => "room_settings_changed",
            Self::WebRTCSignaling { .. } => "webrtc_signaling",
            Self::WebRTCJoin { .. } => "webrtc_join",
            Self::WebRTCLeave { .. } => "webrtc_leave",
            Self::SystemNotification { .. } => "system_notification",
            Self::KickPublisher { .. } => "kick_publisher",
            Self::KickUser { .. } => "kick_user",
            Self::KickUserFromRoom { .. } => "kick_user_from_room",
            Self::RoomCreated { .. } => "room_created",
            Self::RoomBanned { .. } => "room_banned",
            Self::RoomOwnerInactive { .. } => "room_owner_inactive",
            Self::RoomDeleted { .. } => "room_deleted",
            Self::UserNotification { .. } => "user_notification",
            Self::ProviderCredentialChanged { .. } => "provider_credential_changed",
            Self::CacheInvalidate { .. } => "cache_invalidate",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_realtime_event_serialization() -> serde_json::Result<()> {
        let event = RealtimeEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id: RoomId::expect_positive(10_000_140),
            user_id: UserId::expect_positive(10_000_141),
            username: "testuser".to_string(),
            message: "Hello world!".to_string(),
            timestamp: Utc::now(),
            display_position: None,
            display_color: None,
        };

        let json = serde_json::to_string(&event)?;
        assert!(json.contains("chatMessage"));
        assert!(json.contains("Hello world!"));

        let deserialized: RealtimeEvent = serde_json::from_str(&json)?;
        assert_eq!(deserialized.event_type(), "chat_message");
        Ok(())
    }

    #[test]
    fn test_provider_credential_changed_is_admin_channel_event() -> serde_json::Result<()> {
        let event = RealtimeEvent::ProviderCredentialChanged {
            event_id: synctv_common::snanoid!(16),
            user_id: UserId::expect_positive(42),
            provider: "bilibili".to_string(),
            server_id: "global".to_string(),
            timestamp: Utc::now(),
        };

        assert_eq!(event.event_type(), "provider_credential_changed");
        assert!(event.room_id().is_none());
        assert_eq!(event.user_id().copied(), Some(UserId::expect_positive(42)));

        let json = serde_json::to_string(&event)?;
        assert!(json.contains("providerCredentialChanged"));
        let deserialized: RealtimeEvent = serde_json::from_str(&json)?;
        assert_eq!(deserialized.event_type(), "provider_credential_changed");
        Ok(())
    }

    #[test]
    fn test_system_notification_is_admin_channel_event() {
        let event = RealtimeEvent::SystemNotification {
            event_id: synctv_common::snanoid!(16),
            message: "maintenance".to_string(),
            level: NotificationLevel::Warning,
            timestamp: Utc::now(),
        };

        assert!(event.room_id().is_none());
        assert_eq!(event.delivery_route(), RealtimeDeliveryRoute::Admin);
        assert!(event.delivers_to_admin_channel());
        assert!(!event.delivers_to_room_channel());
    }

    #[test]
    fn test_room_created_is_replica_wide_admin_routed() {
        let event = RealtimeEvent::RoomCreated {
            event_id: synctv_common::snanoid!(16),
            room_id: RoomId::expect_positive(10_000_151),
            room_name: "created room".to_string(),
            creator_id: UserId::expect_positive(10_000_152),
            timestamp: Utc::now(),
        };

        assert_eq!(event.delivery_route(), RealtimeDeliveryRoute::RoomAndAdmin);
        assert!(event.delivers_to_admin_channel());
        assert!(event.delivers_to_room_channel());
    }

    #[test]
    fn test_realtime_event_deserialization_requires_event_id() {
        let json = serde_json::json!({
            "type": "chatMessage",
            "roomId": 123,
            "userId": 456,
            "username": "testuser",
            "message": "Hello world!",
            "timestamp": Utc::now(),
            "displayPosition": null,
            "displayColor": null
        });

        let err = serde_json::from_value::<RealtimeEvent>(json)
            .expect_err("realtime events without event_id must fail closed");

        assert!(
            err.to_string().contains("event_id") || err.to_string().contains("eventId"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_realtime_event_deserialization_rejects_unknown_type() {
        let json = serde_json::json!({
            "type": "futureEventType",
            "eventId": synctv_common::snanoid!(16),
            "timestamp": Utc::now()
        });

        let err = serde_json::from_value::<RealtimeEvent>(json)
            .expect_err("unknown realtime event types must fail closed");

        assert!(
            err.to_string().contains("unknown variant"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_realtime_event_room_id() {
        let event = RealtimeEvent::UserJoined {
            event_id: synctv_common::snanoid!(16),
            room_id: RoomId::expect_positive(10_000_140),
            user_id: UserId::expect_positive(10_000_141),
            username: "testuser".to_string(),
            remark_name: String::new(),
            display_tag: String::new(),
            permissions: RoomPermissionSet(0),
            role: 2, // Member role
            added_permissions: RoomPermissionSet(0),
            removed_permissions: RoomPermissionSet(0),
            admin_added_permissions: RoomPermissionSet(0),
            admin_removed_permissions: RoomPermissionSet(0),
            joined_at: Utc::now(),
            timestamp: Utc::now(),
        };

        assert_eq!(
            event.room_id().copied(),
            Some(RoomId::expect_positive(10_000_140))
        );
        assert_eq!(
            event.user_id().copied(),
            Some(UserId::expect_positive(10_000_141))
        );
    }

    #[test]
    fn test_system_notification_no_room() {
        let event = RealtimeEvent::SystemNotification {
            event_id: synctv_common::snanoid!(16),
            message: "Server maintenance in 1 hour".to_string(),
            level: NotificationLevel::Warning,
            timestamp: Utc::now(),
        };

        assert!(event.room_id().is_none());
        assert!(event.user_id().is_none());
        assert_eq!(event.event_type(), "system_notification");
    }

    #[test]
    fn test_kick_publisher_serialization() -> Result<(), Box<dyn std::error::Error>> {
        let event = RealtimeEvent::KickPublisher {
            event_id: synctv_common::snanoid!(16),
            room_id: RoomId::expect_positive(10_000_140),
            media_id: MediaId::expect_positive(10_000_142),
            reason: "user_banned".to_string(),
            timestamp: Utc::now(),
        };

        let json = serde_json::to_string(&event)?;
        assert!(json.contains("kickPublisher"));
        assert!(json.contains("10000140"));
        assert!(json.contains("10000142"));
        assert!(json.contains("user_banned"));

        let deserialized: RealtimeEvent = serde_json::from_str(&json)?;
        assert_eq!(deserialized.event_type(), "kick_publisher");
        assert_eq!(
            deserialized.room_id().copied(),
            Some(RoomId::expect_positive(10_000_140))
        );
        assert!(deserialized.user_id().is_none());

        let RealtimeEvent::KickPublisher {
            room_id,
            media_id,
            reason,
            ..
        } = &deserialized
        else {
            return Err("expected KickPublisher variant".into());
        };
        assert_eq!(*room_id, RoomId::expect_positive(10_000_140));
        assert_eq!(*media_id, MediaId::expect_positive(10_000_142));
        assert_eq!(reason, "user_banned");
        Ok(())
    }

    #[test]
    fn test_kick_publisher_has_room_id_no_user_id() {
        let event = RealtimeEvent::KickPublisher {
            event_id: synctv_common::snanoid!(16),
            room_id: RoomId::expect_positive(10_000_143),
            media_id: MediaId::expect_positive(10_000_144),
            reason: "room_deleted".to_string(),
            timestamp: Utc::now(),
        };

        assert_eq!(
            event.room_id().copied(),
            Some(RoomId::expect_positive(10_000_143))
        );
        assert!(event.user_id().is_none());
        assert_eq!(event.event_type(), "kick_publisher");
    }

    #[test]
    fn test_kick_user_serialization() -> serde_json::Result<()> {
        let event = RealtimeEvent::KickUser {
            event_id: synctv_common::snanoid!(16),
            user_id: UserId::expect_positive(10_000_145),
            reason: "user_banned".to_string(),
            timestamp: Utc::now(),
        };

        let json = serde_json::to_string(&event)?;
        assert!(json.contains("kickUser"));
        assert!(json.contains("10000145"));

        let deserialized: RealtimeEvent = serde_json::from_str(&json)?;
        assert_eq!(deserialized.event_type(), "kick_user");
        assert!(deserialized.room_id().is_none());
        assert_eq!(
            deserialized.user_id().copied(),
            Some(UserId::expect_positive(10_000_145))
        );
        Ok(())
    }

    #[test]
    fn test_cache_invalidate_serialization() -> Result<(), Box<dyn std::error::Error>> {
        let event = RealtimeEvent::CacheInvalidate {
            event_id: synctv_common::snanoid!(16),
            targets: vec![
                CacheTarget::User {
                    user_id: UserId::expect_positive(10_000_001),
                },
                CacheTarget::Room {
                    room_id: RoomId::expect_positive(10_000_002),
                },
                CacheTarget::Username {
                    user_id: UserId::expect_positive(10_000_003),
                },
                CacheTarget::All,
            ],
            timestamp: Utc::now(),
        };

        let json = serde_json::to_string(&event)?;
        assert!(json.contains("cacheInvalidate"));
        assert!(json.contains("10000001"));
        assert!(json.contains("10000002"));

        let deserialized: RealtimeEvent = serde_json::from_str(&json)?;
        assert_eq!(deserialized.event_type(), "cache_invalidate");
        assert!(deserialized.room_id().is_none());
        assert!(deserialized.user_id().is_none());

        let RealtimeEvent::CacheInvalidate { targets, .. } = &deserialized else {
            return Err("expected CacheInvalidate variant".into());
        };
        assert_eq!(targets.len(), 4);
        Ok(())
    }
}
