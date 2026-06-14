//! Unified Message Stream Implementation
//!
//! This module provides a unified implementation for handling real-time messaging
//! that can be used by both gRPC streaming and WebSocket connections.
//!
//! Architecture:
//! - Binary proto encoding/decoding
//! - Shared business logic in impls layer
//! - Transport-agnostic message handling via `MessageSender` and `StreamMessage` traits
//! - Cluster-aware broadcasting (local + Redis)
//! - All logic encapsulated in `StreamMessageHandler` (rate limiting, filtering, permissions)
//! - Complete IO abstraction via `StreamMessage` trait for both sending and receiving

use std::sync::Arc;
use synctv_common::ExecutionControl;
use synctv_core::spawn::spawn_monitored;
use synctv_core::{
    models::{
        ChatMentionInput, ChatMessageType, RoomId, RoomPermission, RoomPermissionSet, RoomStatus,
        SendChatMessage, UserId, UserStatus,
    },
    service::{
        ChatService, ContentFilter, OnlinePresenceService, RateLimitConfig,
        RequestRateLimiterService, RoomService,
    },
};
use synctv_realtime::sync::ConnectionId;
use synctv_realtime::sync::RealtimeEvent;

use crate::chat_event_dispatcher::ChatEventDispatcher;

/// Maximum size of a WebRTC SDP offer/answer payload in bytes.
/// SDP descriptions can be large but should not exceed ~10 KB.
pub const MAX_SDP_SIZE: usize = 10_000;

/// Maximum size of a WebRTC ICE candidate payload in bytes.
/// Individual ICE candidates are small (typically under 200 bytes).
pub const MAX_ICE_CANDIDATE_SIZE: usize = 500;

fn is_private_ice_candidate_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified()
        }
        std::net::IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
        }
    }
}

use crate::impls::playback::PlaybackService;
use crate::impls::playlist_items_snapshot::PlaylistItemsSnapshotService;
use crate::impls::room_members_snapshot::RoomMembersSnapshotService;
use crate::impls::room_settings_snapshot::RoomSettingsSnapshotService;
#[cfg(test)]
use crate::playback_fanout::default_playback_fanout_service;
use crate::playback_fanout::PlaybackFanoutService;
#[cfg(test)]
use crate::resource_change::ResourceInvalidation;
use crate::runtime::RealtimeEventService;
use synctv_proto::client::{ClientMessage, ServerMessage};
use synctv_realtime::sync::ConnectionRuntime;

mod resource_observer;
use resource_observer::{ResourceObserver, ResourceObserverParams};

mod concurrency;
pub use concurrency::MessageConcurrencyConfig;
mod heartbeat;
pub use heartbeat::HeartbeatSchedule;
mod observed_playback;
pub use observed_playback::{
    spawn_observed_playback_lifecycle_event_source, ObservedPlaybackLifecycleEvent,
    ObservedPlaybackLifecycleSubscriber, ProviderPlaybackProgressSubscriber,
};
mod playback;
#[cfg(test)]
pub(crate) use playback::should_persist_playback_progress;
mod resource_watch;
pub use resource_watch::{
    watch_chat_events_observe, watch_playback_observe, watch_playback_state_observe,
    watch_playlist_items_observe, watch_room_member_events_observe, watch_room_settings_observe,
    PreparedResourceWatchSession, ResourceWatchSession, ResourceWatchSessionConfig,
    WatchResourceKind,
};
mod webrtc;

mod codec;
pub use codec::ProtoCodec;
pub(crate) use codec::{
    chat_display_color_from_metadata, chat_display_position_from_metadata,
    chat_event_kind_to_proto, chat_message_event_to_proto, chat_metadata_for_send,
    chat_playback_metadata_from_metadata, core_chat_attachment_to_proto, online_event_to_proto,
    proto_chat_attachment_kind_from_mime_type, proto_chat_attachment_to_core,
    room_member_event_to_proto,
};
#[cfg(test)]
pub(crate) use codec::{
    chat_playback_media_id_from_metadata, chat_playback_playlist_id_from_metadata,
    chat_playback_target_from_metadata, chat_playback_target_hash,
};
mod event_policy;
pub(crate) use event_policy::{
    admin_event_requires_skip_cleanup, disconnect_signal_requires_skip_cleanup,
    should_broadcast_user_left, should_transition_webrtc_membership,
};
#[cfg(test)]
pub(crate) use event_policy::{watch_admin_event_matches, watch_disconnect_signal_matches};
mod identity;
pub use identity::{
    guest_display_name, guest_public_id, GuestRealtimeIdentity, RealtimeJoinError,
    RealtimePrincipal,
};
#[cfg(test)]
pub(crate) use identity::{
    internal_guest_user_id, GUEST_INTERNAL_USER_ID_BASE, GUEST_INTERNAL_USER_ID_SPAN,
};
mod membership;
use membership::{
    guest_admission_denial_reason, probe_realtime_membership_access,
    probe_realtime_membership_access_with_room, realtime_membership_denial_reason,
    InitialRealtimeJoinState, RealtimeMembershipAccess,
};
#[cfg(test)]
pub(crate) use membership::{
    guest_policy_error_to_denial_reason, guest_token_blacklist_denial_reason,
};

/// Cached membership status for heartbeat validation.
///
/// This struct stores the result of a membership check to avoid
/// repeated database queries during heartbeat validation.
#[derive(Clone, Copy, Debug)]
struct CachedMembership {
    /// Whether the user is still a valid member of the room
    is_member: bool,
}

impl CachedMembership {
    /// Create a cached membership from a member lookup result.
    fn from_member(member: Option<&synctv_core::models::RoomMember>) -> Self {
        match member {
            Some(_) => Self { is_member: true },
            None => Self { is_member: false },
        }
    }
}

// Re-use the canonical role proto mapper from client::convert.
use crate::impls::client::room_role_to_proto;

mod transport;
pub use transport::{MessageSender, StreamMessage};

mod notifications;
use notifications::{system_notification_server_message, user_notification_server_message};

/// Per-connection stream message handler with complete logic encapsulation
///
/// Each connection gets its own handler instance with:
/// - Connection state (`room_id`, `user_id`, username)
/// - Message I/O channels
/// - Rate limiting, content filtering, permission checking
/// - Cluster broadcasting
///
/// The handler runs its own message loop, external code only needs to:
/// 1. Create the handler with proper I/O channels
/// 2. Call `start()` to begin processing
pub struct StreamMessageHandler {
    room_id: RoomId,
    principal: RealtimePrincipal,
    user_id: UserId,
    username: String,
    connection_id: ConnectionId,
    room_service: Arc<RoomService>,
    /// `ChatService` for chat message handling with business logic.
    /// Chat messages are processed through `ChatService::send_message()`
    /// which handles permission checks, content filtering, rate limiting, and persistence.
    chat_service: Arc<ChatService>,
    event_service: Arc<dyn RealtimeEventService>,
    playback_fanout: Arc<dyn PlaybackFanoutService>,
    chat_event_dispatcher: Arc<dyn ChatEventDispatcher>,
    /// Optional notification service for direct real-time push to connected clients.
    /// When set, the handler subscribes to notification events and pushes them
    /// without depending on the gRPC notification-to-realtime bridge.
    notification_service: Option<Arc<synctv_core::service::UserNotificationService>>,
    connection_service: Arc<dyn ConnectionRuntime>,
    presence_service: Arc<OnlinePresenceService>,
    rate_limiter: Arc<dyn RequestRateLimiterService>,
    rate_limit_config: Arc<RateLimitConfig>,
    content_filter: Arc<ContentFilter>,
    public_id_codec: Arc<synctv_core::PublicIdCodec>,
    sender: Arc<dyn MessageSender>,
    playback_service: Arc<dyn PlaybackService>,
    playlist_items_snapshot_service: Arc<dyn PlaylistItemsSnapshotService>,
    room_members_snapshot_service: Arc<dyn RoomMembersSnapshotService>,
    room_settings_snapshot_service: Arc<dyn RoomSettingsSnapshotService>,
    resource_observer: Arc<ResourceObserver>,
    /// Global per-connection WebSocket message rate limit (messages per second)
    ws_message_rate_limit: u32,
    /// Tracks whether this connection has an active WebRTC session.
    /// Used by `cleanup()` to decrement `WEBRTC_PEERS_ACTIVE` on ungraceful disconnect.
    has_webrtc_session: Arc<std::sync::atomic::AtomicBool>,
    /// When true, `cleanup()` skips broadcasting `UserLeft`.
    ///
    /// Used when:
    /// - the event was already published by an explicit API call (`leave_room/delete_room`)
    /// - the connection never completed its initial join handshake, so broadcasting
    ///   `UserLeft` would create a ghost offline event for a user that was never
    ///   actually announced as online
    skip_cleanup_user_left: Arc<std::sync::atomic::AtomicBool>,
    /// Last known room role for this connection's actor.
    ///
    /// Cleanup uses this cached value so disconnect paths do not depend on a
    /// fresh database read while the transport is already failing.
    current_room_role: Arc<std::sync::atomic::AtomicI32>,
    /// Cached membership status for heartbeat validation.
    /// Uses TTL-based expiration (30 seconds) to reduce database load while
    /// maintaining reasonable responsiveness to membership changes.
    /// Key: (`room_id`, `user_id`) tuple for O(1) lookup.
    membership_cache: Arc<moka::sync::Cache<(RoomId, UserId), CachedMembership>>,
    /// Room event receiver created during `pre_join()` so transports do not expose
    /// a window where the connection is joined in `ConnectionManager` but not yet
    /// subscribed in `RoomMessageHub`.
    pending_room_event_rx:
        Arc<tokio::sync::Mutex<Option<tokio::sync::mpsc::Receiver<RealtimeEvent>>>>,
    /// Authenticated member/settings snapshot validated during `pre_join()`.
    pending_initial_join_state: Arc<tokio::sync::Mutex<Option<InitialRealtimeJoinState>>>,
    /// Instance-level concurrency configuration for backpressure control.
    /// This replaces the global `MESSAGE_PROCESSING_SEMAPHORE` with per-AppState configuration.
    concurrency_config: Arc<MessageConcurrencyConfig>,
    /// Throttle state for playback progress DB writes.
    /// Stores the (last_written_position, last_write_time) to avoid
    /// writing to the DB on every progress heartbeat.
    last_progress_write: Arc<tokio::sync::Mutex<Option<(f64, tokio::time::Instant)>>>,
    heartbeat_schedule: HeartbeatSchedule,
    filter_private_ice_candidates: bool,
}

pub struct StreamMessageHandlerConfig {
    pub room_id: RoomId,
    pub principal: RealtimePrincipal,
    pub connection_id: Option<String>,
    pub room_service: Arc<RoomService>,
    pub chat_service: Arc<ChatService>,
    pub event_service: Arc<dyn RealtimeEventService>,
    pub connection_service: Arc<dyn ConnectionRuntime>,
    pub rate_limiter: Arc<dyn RequestRateLimiterService>,
    pub rate_limit_config: Arc<RateLimitConfig>,
    pub content_filter: Arc<ContentFilter>,
    pub public_id_codec: Arc<synctv_core::PublicIdCodec>,
    pub sender: Arc<dyn MessageSender>,
    pub concurrency_config: Arc<MessageConcurrencyConfig>,
}

#[derive(Clone)]
pub struct StreamMessageHandlerRuntime {
    pub playback_service: Arc<dyn PlaybackService>,
    pub playlist_items_snapshot_service: Arc<dyn PlaylistItemsSnapshotService>,
    pub room_members_snapshot_service: Arc<dyn RoomMembersSnapshotService>,
    pub room_settings_snapshot_service: Arc<dyn RoomSettingsSnapshotService>,
    pub playback_fanout: Arc<dyn PlaybackFanoutService>,
    pub chat_event_dispatcher: Arc<dyn ChatEventDispatcher>,
    pub presence_service: Arc<OnlinePresenceService>,
    pub notification_service: Option<Arc<synctv_core::service::UserNotificationService>>,
    pub ws_message_rate_limit: u32,
    pub heartbeat_schedule: HeartbeatSchedule,
    pub filter_private_ice_candidates: bool,
}

impl StreamMessageHandlerRuntime {
    #[must_use]
    #[cfg(test)]
    pub fn local(event_service: &Arc<dyn RealtimeEventService>) -> Self {
        Self {
            playback_service: Arc::new(tests::UnconfiguredPlaybackService),
            playlist_items_snapshot_service: Arc::new(
                tests::UnconfiguredPlaylistItemsSnapshotService,
            ),
            room_members_snapshot_service: Arc::new(tests::UnconfiguredRoomMembersSnapshotService),
            room_settings_snapshot_service: Arc::new(
                tests::UnconfiguredRoomSettingsSnapshotService,
            ),
            playback_fanout: default_playback_fanout_service(
                crate::realtime_fanout::local_realtime_fanout_service(event_service.clone()),
            ),
            chat_event_dispatcher: crate::chat_event_dispatcher::default_chat_event_dispatcher(
                event_service.clone(),
            ),
            presence_service: Arc::new(OnlinePresenceService::local()),
            notification_service: None,
            ws_message_rate_limit: 50,
            heartbeat_schedule: HeartbeatSchedule::production(),
            filter_private_ice_candidates: true,
        }
    }
}

impl Clone for StreamMessageHandler {
    fn clone(&self) -> Self {
        Self {
            room_id: self.room_id,
            principal: self.principal.clone(),
            user_id: self.user_id,
            username: self.username.clone(),
            connection_id: self.connection_id.clone(),
            room_service: Arc::clone(&self.room_service),
            chat_service: Arc::clone(&self.chat_service),
            event_service: Arc::clone(&self.event_service),
            playback_fanout: Arc::clone(&self.playback_fanout),
            chat_event_dispatcher: Arc::clone(&self.chat_event_dispatcher),
            notification_service: self.notification_service.clone(),
            connection_service: Arc::clone(&self.connection_service),
            presence_service: Arc::clone(&self.presence_service),
            rate_limiter: Arc::clone(&self.rate_limiter),
            rate_limit_config: Arc::clone(&self.rate_limit_config),
            content_filter: Arc::clone(&self.content_filter),
            public_id_codec: Arc::clone(&self.public_id_codec),
            sender: Arc::clone(&self.sender),
            playback_service: self.playback_service.clone(),
            playlist_items_snapshot_service: self.playlist_items_snapshot_service.clone(),
            room_members_snapshot_service: self.room_members_snapshot_service.clone(),
            room_settings_snapshot_service: Arc::clone(&self.room_settings_snapshot_service),
            resource_observer: Arc::clone(&self.resource_observer),
            ws_message_rate_limit: self.ws_message_rate_limit,
            has_webrtc_session: Arc::clone(&self.has_webrtc_session),
            skip_cleanup_user_left: Arc::clone(&self.skip_cleanup_user_left),
            current_room_role: Arc::clone(&self.current_room_role),
            membership_cache: Arc::clone(&self.membership_cache),
            pending_room_event_rx: Arc::clone(&self.pending_room_event_rx),
            pending_initial_join_state: Arc::clone(&self.pending_initial_join_state),
            concurrency_config: Arc::clone(&self.concurrency_config),
            last_progress_write: Arc::clone(&self.last_progress_write),
            heartbeat_schedule: self.heartbeat_schedule,
            filter_private_ice_candidates: self.filter_private_ice_candidates,
        }
    }
}

impl StreamMessageHandler {
    #[must_use]
    pub fn generate_connection_id() -> ConnectionId {
        ConnectionId::new(format!("conn_c{}", synctv_common::snanoid!(16)))
    }

    fn error_server_message(error: impl Into<crate::impls::ApiError>) -> ServerMessage {
        let api_error: crate::impls::ApiError = error.into();
        ServerMessage {
            message: Some(synctv_proto::client::server_message::Message::Error(
                api_error.to_proto_error(),
            )),
        }
    }

    #[cfg(test)]
    pub fn new(config: StreamMessageHandlerConfig) -> Self {
        let runtime = StreamMessageHandlerRuntime::local(&config.event_service);
        Self::new_with_runtime(config, runtime)
    }

    pub fn new_with_runtime(
        config: StreamMessageHandlerConfig,
        runtime: StreamMessageHandlerRuntime,
    ) -> Self {
        let StreamMessageHandlerConfig {
            room_id,
            principal,
            connection_id,
            room_service,
            chat_service,
            event_service,
            connection_service,
            rate_limiter,
            rate_limit_config,
            content_filter,
            public_id_codec,
            sender,
            concurrency_config,
        } = config;
        let connection_id =
            connection_id.map_or_else(Self::generate_connection_id, ConnectionId::new);
        let user_id = principal.connection_user_id();
        let username = principal.username().to_string();
        let heartbeat_schedule = runtime.heartbeat_schedule;
        let membership_cache = Arc::new(
            moka::sync::Cache::builder()
                .time_to_live(heartbeat_schedule.membership_cache_ttl())
                .build(),
        );
        let room_settings_snapshot_service = Arc::clone(&runtime.room_settings_snapshot_service);
        let room_actor = principal.room_actor(room_id);
        let chat_event_dispatcher = runtime.chat_event_dispatcher;
        let playback_fanout = runtime.playback_fanout;
        let presence_service = runtime.presence_service;
        let resource_observer = Arc::new(ResourceObserver::new(ResourceObserverParams {
            room_id,
            user_id,
            actor: room_actor,
            connection_id: connection_id.as_str().to_string(),
            room_service: Arc::clone(&room_service),
            presence_service: Arc::clone(&presence_service),
            public_id_codec: Arc::clone(&public_id_codec),
            sender: Arc::clone(&sender),
            playback_service: Arc::clone(&runtime.playback_service),
            playlist_items_snapshot_service: Arc::clone(&runtime.playlist_items_snapshot_service),
            room_settings_snapshot_service: Arc::clone(&room_settings_snapshot_service),
        }));

        Self {
            room_id,
            principal,
            user_id,
            username,
            connection_id,
            room_service,
            chat_service,
            event_service,
            playback_fanout,
            chat_event_dispatcher,
            notification_service: runtime.notification_service,
            connection_service,
            presence_service,
            rate_limiter,
            rate_limit_config,
            content_filter,
            public_id_codec,
            sender,
            playback_service: runtime.playback_service,
            playlist_items_snapshot_service: runtime.playlist_items_snapshot_service,
            room_members_snapshot_service: runtime.room_members_snapshot_service,
            room_settings_snapshot_service,
            resource_observer,
            ws_message_rate_limit: runtime.ws_message_rate_limit,
            has_webrtc_session: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            skip_cleanup_user_left: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            current_room_role: Arc::new(std::sync::atomic::AtomicI32::new(
                synctv_proto::common::RoomMemberRole::Member as i32,
            )),
            membership_cache,
            pending_room_event_rx: Arc::new(tokio::sync::Mutex::new(None)),
            pending_initial_join_state: Arc::new(tokio::sync::Mutex::new(None)),
            concurrency_config,
            last_progress_write: Arc::new(tokio::sync::Mutex::new(None)),
            heartbeat_schedule,
            filter_private_ice_candidates: runtime.filter_private_ice_candidates,
        }
    }

    #[must_use]
    pub fn connection_id(&self) -> &str {
        self.connection_id.as_str()
    }

    /// Invalidate the membership cache entry for a specific user in a room.
    ///
    /// Called when a `KickUser` or `KickUserFromRoom` admin event is received,
    /// ensuring that the heartbeat check will re-query the database on the next
    /// tick instead of trusting the stale cached "member" status.
    pub fn invalidate_membership_cache(&self, room_id: &RoomId, user_id: &UserId) {
        let cache_key = (*room_id, *user_id);
        self.membership_cache.invalidate(&cache_key);
    }

    fn public_room_id(&self) -> Result<String, String> {
        self.public_id_codec
            .encode_room_id(self.room_id)
            .map_err(|error| format!("Failed to encode room public id: {error}"))
    }

    fn public_actor_id(&self) -> Result<String, String> {
        self.principal.public_actor_id(&self.public_id_codec)
    }

    fn webrtc_event_server_message_for_current_connection(
        &self,
        event: &RealtimeEvent,
    ) -> Result<Option<ServerMessage>, String> {
        use synctv_proto::client::resource_event::Payload;
        use synctv_proto::client::server_message::Message;
        use synctv_proto::client::web_rtc_event::Event;
        use synctv_proto::client::{
            ResourceEvent, ServerMessage, WebRtcAnswer, WebRtcEvent, WebRtcIceCandidate,
            WebRtcJoin, WebRtcLeave, WebRtcOffer,
        };
        use synctv_realtime::sync::WebRTCSignalKind;

        let payload = match event {
            RealtimeEvent::WebRTCSignaling {
                message_type,
                from,
                to,
                data,
                ..
            } => {
                let Some((actor_id, conn_id)) = to.rsplit_once(':') else {
                    return Ok(None);
                };
                if conn_id != self.connection_id.as_str() || actor_id != self.public_actor_id()? {
                    return Ok(None);
                }
                match message_type {
                    WebRTCSignalKind::Offer => Event::Offer(WebRtcOffer {
                        from: from.clone(),
                        to: to.clone(),
                        data: data.clone(),
                    }),
                    WebRTCSignalKind::Answer => Event::Answer(WebRtcAnswer {
                        from: from.clone(),
                        to: to.clone(),
                        data: data.clone(),
                    }),
                    WebRTCSignalKind::IceCandidate => Event::IceCandidate(WebRtcIceCandidate {
                        from: from.clone(),
                        to: to.clone(),
                        data: data.clone(),
                    }),
                }
            }
            RealtimeEvent::WebRTCJoin {
                actor_id,
                conn_id,
                username,
                ..
            } => {
                if conn_id == self.connection_id.as_str() || !self.current_connection_rtc_joined() {
                    return Ok(None);
                }
                Event::Join(WebRtcJoin {
                    user_id: actor_id.clone(),
                    conn_id: conn_id.clone(),
                    username: username.clone(),
                })
            }
            RealtimeEvent::WebRTCLeave {
                actor_id, conn_id, ..
            } => {
                if conn_id == self.connection_id.as_str() || !self.current_connection_rtc_joined() {
                    return Ok(None);
                }
                Event::Leave(WebRtcLeave {
                    user_id: actor_id.clone(),
                    conn_id: conn_id.clone(),
                })
            }
            _ => return Ok(None),
        };

        Ok(Some(ServerMessage {
            message: Some(Message::ResourceEvent(ResourceEvent {
                observe_id: "webrtc".to_string(),
                payload: Some(Payload::WebrtcEvent(WebRtcEvent {
                    event: Some(payload),
                })),
                event_cursor: None,
            })),
        }))
    }

    fn current_connection_rtc_joined(&self) -> bool {
        self.connection_service
            .get_connection(self.connection_id.as_str())
            .is_some_and(|connection| connection.rtc_joined)
    }

    fn apply_connection_state_from_room_event(&self, event: &RealtimeEvent) {
        if let RealtimeEvent::PermissionChanged {
            target_user_id,
            role_changed,
            role,
            ..
        } = event
        {
            if *target_user_id == self.user_id && *role_changed {
                self.current_room_role
                    .store(*role, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    async fn guest_permissions(&self) -> Result<RoomPermissionSet, synctv_core::Error> {
        self.room_service.get_guest_permissions(&self.room_id).await
    }

    async fn check_realtime_permission(
        &self,
        permission: RoomPermission,
    ) -> Result<(), synctv_core::Error> {
        if self.principal.is_guest() {
            let permissions = self.guest_permissions().await?;
            if permissions.has(permission) {
                Ok(())
            } else {
                Err(synctv_core::Error::Authorization(
                    "Guests do not have permission to perform this action".to_string(),
                ))
            }
        } else {
            self.room_service
                .check_permission(&self.room_id, &self.user_id, permission)
                .await
        }
    }

    async fn ensure_observe_resource_allowed(
        &self,
        observe: &synctv_proto::client::ObserveResource,
    ) -> Result<(), String> {
        let Some(resource) = observe.resource.as_ref() else {
            return Err("observe resource is required".to_string());
        };

        match resource {
            synctv_proto::client::observe_resource::Resource::PlaybackState(_)
            | synctv_proto::client::observe_resource::Resource::RoomSettings(_)
            | synctv_proto::client::observe_resource::Resource::OnlineCount(_) => {
                if self.principal.is_guest() {
                    self.ensure_guest_admission_for_action().await?;
                }
                Ok(())
            }
            synctv_proto::client::observe_resource::Resource::PlaylistItems(_) => {
                if self.principal.is_guest() {
                    Err("Guests cannot observe playlist items".to_string())
                } else {
                    Ok(())
                }
            }
            synctv_proto::client::observe_resource::Resource::RoomMemberEvents(_)
            | synctv_proto::client::observe_resource::Resource::OnlineEvent(_) => {
                if self.principal.is_guest() {
                    self.ensure_guest_admission_for_action().await?;
                }
                self.check_realtime_permission(RoomPermission::VIEW_MEMBER_LIST)
                    .await
                    .map_err(|e| e.to_string())
            }
            synctv_proto::client::observe_resource::Resource::SelfRoomMember(_) => {
                if self.principal.is_guest() {
                    return Err("Guests do not have a room member permission snapshot".to_string());
                }
                Ok(())
            }
            synctv_proto::client::observe_resource::Resource::ChatEvents(_) => {
                if self.principal.is_guest() {
                    self.ensure_guest_admission_for_action().await?;
                }
                self.check_realtime_permission(RoomPermission::VIEW_CHAT_HISTORY)
                    .await
                    .map_err(|e| e.to_string())
            }
            synctv_proto::client::observe_resource::Resource::Playback(_) => {
                if self.principal.is_guest() {
                    Err(
                        "Guests cannot observe playbacks because playbacks may depend on signed-in provider credentials"
                            .to_string(),
                    )
                } else {
                    Ok(())
                }
            }
        }
    }

    async fn guest_admission_denial_reason(&self) -> Result<Option<String>, RealtimeJoinError> {
        guest_admission_denial_reason(
            &self.room_service,
            &self.room_id,
            &self.user_id,
            &self.principal,
        )
        .await
    }

    async fn prepare_initial_realtime_join_state(
        &self,
    ) -> Result<Result<InitialRealtimeJoinState, String>, RealtimeJoinError> {
        if self.principal.is_guest() {
            return Ok(match self.guest_admission_denial_reason().await? {
                Some(reason) => Err(reason),
                None => Ok(InitialRealtimeJoinState {
                    member: None,
                    room_settings: None,
                }),
            });
        }

        let user = self
            .room_service
            .user_service()
            .get_user(&self.user_id)
            .await
            .map_err(|error| {
                tracing::warn!(
                    error = %error,
                    room_id = %self.room_id,
                    user_id = %self.user_id,
                    "Failed to re-validate user access during pre_join; rejecting connection because final admission must fail closed"
                );
                RealtimeJoinError::ServiceUnavailable(
                    "User re-validation temporarily unavailable".to_string(),
                )
        })?;

        if user.status == UserStatus::Banned {
            return Ok(Err(
                "User is no longer allowed to use real-time messaging".to_string()
            ));
        }
        if user.deleted_at.is_some() {
            return Ok(Err("User account is no longer available".to_string()));
        }

        let room = self.room_service.get_room(&self.room_id).await.map_err(|error| {
            tracing::warn!(
                error = %error,
                room_id = %self.room_id,
                user_id = %self.user_id,
                "Failed to re-validate room access during pre_join; rejecting connection because final admission must fail closed"
            );
            RealtimeJoinError::ServiceUnavailable(
                "Room re-validation temporarily unavailable".to_string(),
            )
        })?;

        if room.is_banned {
            return Ok(Err("This room has been banned".to_string()));
        }
        if room.status == RoomStatus::Closed {
            return Ok(Err(
                "This room is closed and not accepting new connections".to_string()
            ));
        }

        let membership_lookup =
            probe_realtime_membership_access_with_room(&self.room_service, &room, &self.user_id)
                .await;
        let member = match membership_lookup {
            Ok(RealtimeMembershipAccess::Allowed(member)) => member,
            Ok(RealtimeMembershipAccess::Denied(reason)) => return Ok(Err(reason)),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    room_id = %self.room_id,
                    user_id = %self.user_id,
                    "Failed to re-validate membership during pre_join; rejecting connection because final admission must fail closed"
                );
                return Err(RealtimeJoinError::ServiceUnavailable(
                    "Membership re-validation temporarily unavailable".to_string(),
                ));
            }
        };

        let room_settings = self.room_service.get_room_settings(&self.room_id).await.map_err(
            |error| {
                tracing::warn!(
                    error = %error,
                    room_id = %self.room_id,
                    user_id = %self.user_id,
                    "Failed to load room settings during pre_join; rejecting connection because permission snapshots must fail closed"
                );
                RealtimeJoinError::from(crate::impls::ApiError::from(error))
            },
        )?;

        Ok(Ok(InitialRealtimeJoinState {
            member: Some(member),
            room_settings: Some(room_settings),
        }))
    }

    /// Register the connection and join the room, enforcing connection limits.
    ///
    /// Call this **before** returning the gRPC response stream so that limit
    /// violations surface as a proper gRPC error instead of silently failing
    /// inside a background task.  After a successful `pre_join`, call
    /// [`run_after_join`] to enter the message loop.
    pub async fn pre_join(&self) -> Result<(), RealtimeJoinError> {
        if let Err(e) = self
            .connection_service
            .register_actor(
                self.connection_id.clone().into_string(),
                self.user_id,
                self.public_actor_id()?,
            )
            .await
        {
            tracing::warn!("Failed to register connection: {}", e);
            return Err(RealtimeJoinError::from(
                crate::runtime::RealtimeAdmissionError::from_runtime_message(e),
            ));
        }

        self.pre_join_after_registration().await
    }

    /// Continue admission after the connection was already registered.
    ///
    /// This is used by transports that need an early registration/backpressure
    /// step before they can finish reading the room-scoped handshake.
    pub async fn pre_join_after_registration(&self) -> Result<(), RealtimeJoinError> {
        if let Err(e) = self
            .connection_service
            .join_room(self.connection_id.as_str(), self.room_id)
            .await
        {
            self.connection_service
                .unregister(self.connection_id.as_str())
                .await;
            return Err(RealtimeJoinError::from(
                crate::runtime::RealtimeAdmissionError::from_runtime_message(e),
            ));
        }

        let initial_join_state = match self.prepare_initial_realtime_join_state().await {
            Ok(state) => state,
            Err(error) => {
                self.skip_cleanup_user_left
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                self.connection_service
                    .unregister(self.connection_id.as_str())
                    .await;
                return Err(error);
            }
        };
        let initial_join_state = match initial_join_state {
            Ok(state) => state,
            Err(reason) => {
                self.skip_cleanup_user_left
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                self.connection_service
                    .unregister(self.connection_id.as_str())
                    .await;
                return Err(RealtimeJoinError::PermissionDenied(reason));
            }
        };

        if let Err(error) = self
            .cache_initial_realtime_join_state(initial_join_state)
            .await
        {
            self.skip_cleanup_user_left
                .store(true, std::sync::atomic::Ordering::Relaxed);
            self.connection_service
                .unregister(self.connection_id.as_str())
                .await;
            return Err(error);
        }

        if let Err(error) = self.cache_room_event_subscription().await {
            self.skip_cleanup_user_left
                .store(true, std::sync::atomic::Ordering::Relaxed);
            self.connection_service
                .unregister(self.connection_id.as_str())
                .await;
            return Err(error);
        }

        Ok(())
    }

    async fn cache_initial_realtime_join_state(
        &self,
        state: InitialRealtimeJoinState,
    ) -> Result<(), RealtimeJoinError> {
        let mut pending_state = self.pending_initial_join_state.lock().await;
        if pending_state.is_some() {
            return Err(RealtimeJoinError::Internal(
                "Initial realtime join state is already cached".to_string(),
            ));
        }
        *pending_state = Some(state);
        Ok(())
    }

    async fn cache_room_event_subscription(&self) -> Result<(), RealtimeJoinError> {
        let mut pending_rx = self.pending_room_event_rx.lock().await;
        if pending_rx.is_some() {
            return Ok(());
        }

        let (event_rx, _connection_id) = self
            .event_service
            .subscribe_with_id(self.room_id, self.user_id, self.connection_id.clone())
            .await
            .map_err(|e| {
                self.skip_cleanup_user_left
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                RealtimeJoinError::Internal(format!(
                    "Failed to subscribe to realtime events during pre_join: {e}"
                ))
            })?;
        *pending_rx = Some(event_rx);

        Ok(())
    }

    async fn take_room_event_subscription(
        &self,
    ) -> Result<tokio::sync::mpsc::Receiver<RealtimeEvent>, String> {
        if let Some(event_rx) = self.pending_room_event_rx.lock().await.take() {
            return Ok(event_rx);
        }

        self.event_service
            .subscribe_with_id(self.room_id, self.user_id, self.connection_id.clone())
            .await
            .map(|(event_rx, _connection_id)| event_rx)
            .map_err(|e| format!("Failed to subscribe to realtime events: {e}"))
    }

    /// Run the complete message loop using unified IO abstraction.
    ///
    /// This is the recommended method that handles both sending and receiving
    /// in a single unified loop using the `StreamMessage` trait.
    ///
    /// This method:
    /// 1. Registers the connection and joins the room (enforcing limits)
    /// 2. Subscribes to realtime events and forwards them to the client
    /// 3. Receives client messages via the `StreamMessage` trait
    /// 4. Handles rate limiting, content filtering, and permissions
    /// 5. Broadcasts events to the cluster
    /// 6. Monitors for disconnect signals (user ban, kick, etc.)
    /// 7. Handles cleanup on disconnect
    ///
    /// The caller only needs to provide a `StreamMessage` implementation (WebSocket or gRPC).
    ///
    /// If you need to check connection limits *before* returning a response stream
    /// (e.g. in gRPC), call [`pre_join`] first and then [`run_after_join`].
    pub async fn run<S: StreamMessage>(&self, stream: &mut S) -> Result<(), String> {
        self.pre_join().await.map_err(String::from)?;
        self.run_after_join(stream).await
    }

    /// Continue the message loop after a successful [`pre_join`].
    ///
    /// This is identical to [`run`] but skips the register/join_room steps
    /// that were already performed by `pre_join`.
    pub async fn run_after_join<S: StreamMessage>(&self, stream: &mut S) -> Result<(), String> {
        let room_id_str = self.public_room_id()?;

        // Pre-join caches the room subscription so there is no gap between
        // admission success and the transport starting its receive loop.
        let mut event_rx = self.take_room_event_subscription().await?;
        // Subscribe to disconnect signals
        let mut disconnect_rx = self.connection_service.subscribe_disconnect();

        // Subscribe to admin events (KickUser, etc.) for cross-replica disconnect propagation.
        // KickUser events arrive via Redis PubSub on the admin channel and are not
        // delivered through the room-level event subscription, so each connection
        // must independently monitor admin events and disconnect when targeted.
        let mut admin_rx = self.event_service.subscribe_admin_events();

        // Subscribe to notification events directly so WebSocket clients receive
        // notifications even when the gRPC notification bridge is not running.
        let mut notification_rx = self
            .notification_service
            .as_ref()
            .map(|svc| svc.subscribe_events());

        // Fetch member data and room settings once and reuse them for the join
        // payload and realtime event. Authenticated users must have both so
        // outbound permission snapshots cannot silently fall back to role-only
        // defaults when a read fails.
        let initial_join = self.take_initial_realtime_join_state(&room_id_str).await?;
        if let Some(member) = initial_join.member.as_ref() {
            self.current_room_role
                .store(i32::from(member.role), std::sync::atomic::Ordering::Relaxed);
        }

        // Broadcast UserJoined event to observers and other replicas.
        self.broadcast_user_joined(
            initial_join.member.as_ref(),
            initial_join.room_settings.as_ref(),
        )
        .await;

        // Create heartbeat interval OUTSIDE the loop so it doesn't reset
        // when other select! branches fire.
        // Add random jitter (±5 s around the 30 s base) so that 1000 concurrent
        // connections do not all fire their DB membership checks in the same
        // one-second window (thundering-herd protection).
        let heartbeat_period = self.heartbeat_schedule.period_with_random_jitter();
        let mut heartbeat_interval = tokio::time::interval(heartbeat_period);
        heartbeat_interval.tick().await; // Skip the immediate first tick

        // Global per-connection message rate limiter (token bucket).
        // Configured via connection_limits.ws_message_rate_limit_per_second.
        // This is local to each connection (no Redis needed).
        let global_msg_rate_limit = self.ws_message_rate_limit;
        let mut global_msg_count: u32 = 0;
        let mut global_msg_window_start = tokio::time::Instant::now();
        let message_control = ExecutionControl::default();

        // Main message loop using tokio::select! for concurrent operations
        loop {
            tokio::select! {
                // Incoming client message
                client_msg_result = stream.recv() => {
                    match client_msg_result {
                        Some(Ok(msg)) => {
                            self.connection_service.record_message(self.connection_id.as_str());

                            // Global per-connection rate limit check (before any processing)
                            let now = tokio::time::Instant::now();
                            if now.duration_since(global_msg_window_start) >= std::time::Duration::from_secs(1) {
                                // Reset window
                                global_msg_count = 0;
                                global_msg_window_start = now;
                            }
                            global_msg_count += 1;
                            if global_msg_count > global_msg_rate_limit {
                                tracing::warn!(
                                    user_id = %self.user_id,
                                    room_id = %self.room_id,
                                    limit = global_msg_rate_limit,
                                    "Global WebSocket message rate limit exceeded, dropping message"
                                );
                                continue;
                            }

                            // Backpressure control: try to acquire a semaphore permit.
                            // If the system is overloaded, return ResourceExhausted error instead of processing.
                            let semaphore = self.concurrency_config.semaphore();
                            let Ok(permit) = semaphore.try_acquire_owned() else {
                                tracing::warn!(
                                    user_id = %self.user_id,
                                    room_id = %self.room_id,
                                    "System overloaded: message processing semaphore exhausted, returning ResourceExhausted"
                                );
                                // Send ResourceExhausted error to client
                            let error_msg = Self::error_server_message(
                                crate::impls::ApiError::RateLimited(
                                    "System overloaded, please retry later".to_string(),
                                ),
                            );
                            if let Err(e) = stream.send(error_msg) {
                                tracing::error!(
                                    "Failed to send ResourceExhausted error to client: {}",
                                    e
                                );
                                break;
                            }
                            continue;
                            };

                            // Process message with semaphore permit held
                            let _permit = permit; // Hold permit for duration of processing
                            if let Err(e) = self
                                .handle_client_message_with_control(&msg, Some(&message_control))
                                .await
                            {
                                tracing::error!("Failed to handle client message: {}", e);
                                if let Err(send_err) =
                                    stream.send(Self::error_server_message(e.clone()))
                                {
                                    tracing::error!(
                                        "Failed to send message error to client: {}",
                                        send_err
                                    );
                                    break;
                                }
                            }
                        }
                        Some(Err(e)) => {
                            tracing::error!("Error receiving message: {}", e);
                            break;
                        }
                        None => {
                            tracing::info!("Client disconnected gracefully");
                            break;
                        }
                    }
                }

                // Realtime event (broadcast to client)
                event = event_rx.recv() => {
                    if let Some(event) = event {
                        self.apply_connection_state_from_room_event(&event);
                        match self.webrtc_event_server_message_for_current_connection(&event) {
                            Ok(Some(message)) => {
                                if let Err(error) = stream.send(message) {
                                    tracing::error!("Failed to send WebRTC server message: {}", error);
                                    break;
                                }
                            }
                            Ok(None) => {}
                            Err(error) => {
                                tracing::error!(
                                    room_id = %self.room_id,
                                    event_id = %event.event_id(),
                                    error = %error,
                                    "Failed to convert WebRTC event to server message"
                                );
                                break;
                            }
                        }
                        if !matches!(event, RealtimeEvent::ChatMessageEvent { .. }) {
                            let mut send_failed = false;
                            let messages = match realtime_event_to_server_messages(
                                &event,
                                &room_id_str,
                                &self.public_id_codec,
                            ) {
                                Ok(messages) => messages,
                                Err(error) => {
                                    tracing::error!(
                                        room_id = %self.room_id,
                                        event_id = %event.event_id(),
                                        error = %error,
                                        "Failed to convert realtime event to server message"
                                    );
                                    break;
                                }
                            };
                            for msg in messages {
                                if let Err(e) = stream.send(msg) {
                                    tracing::error!("Failed to send server message: {}", e);
                                    send_failed = true;
                                    break;
                                }
                            }
                            if send_failed {
                                break;
                            }
                        }

                        let should_refresh_observed_resources = !matches!(
                            event,
                            RealtimeEvent::ChatMessageEvent { .. }
                        ) || self.resource_observer.has_chat_events_observation().await;

                        if should_refresh_observed_resources {
                            if let Err(error) = self
                                .resource_observer
                                .room_hub
                                .refresh_for_room_event(&event, Some(self.connection_id.as_str()))
                                .await
                            {
                                tracing::error!(
                                    "Failed to refresh observed resources for room event: {}",
                                    error
                                );
                                break;
                            }
                        }
                    } else {
                        tracing::error!("Realtime event channel closed");
                        break;
                    }
                }

                () = async {
                    match self
                        .resource_observer
                        .next_expired_resource_refresh_deadline()
                        .await
                    {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    if let Err(error) = self
                        .resource_observer
                        .refresh_expired_resource_observations()
                        .await
                    {
                        tracing::error!(
                            "Failed to refresh observed resources after expiration: {}",
                            error
                        );
                        break;
                    }
                }

                // Disconnect signal (forced disconnect by server)
                signal = disconnect_rx.recv() => {
                    match signal {
                        Ok(synctv_realtime::sync::DisconnectSignal::Connection(conn_id)) => {
                            if conn_id == self.connection_id.as_str() {
                                tracing::info!(
                                    connection_id = %self.connection_id,
                                    "Received disconnect signal for this connection"
                                );
                                break;
                            }
                        }
                        Ok(synctv_realtime::sync::DisconnectSignal::User(uid)) => {
                            if uid == self.user_id {
                                tracing::info!(
                                    user_id = %self.user_id,
                                    "Received disconnect signal for this user (room kick or platform ban)"
                                );
                                self.skip_cleanup_user_left
                                    .store(true, std::sync::atomic::Ordering::Relaxed);
                                break;
                            }
                        }
                        Ok(synctv_realtime::sync::DisconnectSignal::Room(rid)) => {
                            if rid == self.room_id {
                                tracing::info!(
                                    room_id = %self.room_id,
                                    "Received disconnect signal for this room"
                                );
                                // Room deletion already published RoomDeleted;
                                // skip redundant UserLeft.
                                self.skip_cleanup_user_left.store(true, std::sync::atomic::Ordering::Relaxed);
                                break;
                            }
                        }
                        Ok(synctv_realtime::sync::DisconnectSignal::UserFromRoom { user_id: uid, room_id: rid }) => {
                            if uid == self.user_id && rid == self.room_id {
                                tracing::info!(
                                    user_id = %self.user_id,
                                    room_id = %self.room_id,
                                    "Received disconnect signal: kicked from room"
                                );
                                // The leave_room API already published UserLeft;
                                // skip redundant broadcast in cleanup().
                                self.skip_cleanup_user_left.store(true, std::sync::atomic::Ordering::Relaxed);
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            // Channel lagged: we may have missed critical disconnect signals.
                            // Re-subscribe to get a fresh receiver so future signals are not lost,
                            // then verify membership to catch any missed room kick or platform ban.
                            tracing::warn!(
                                lagged = n,
                                user_id = %self.user_id,
                                room_id = %self.room_id,
                                "Disconnect signal channel lagged, re-subscribing and verifying membership"
                            );
                            disconnect_rx = self.connection_service.subscribe_disconnect();

                            match realtime_membership_denial_reason(
                                &self.room_service,
                                &self.room_id,
                                &self.user_id,
                            )
                            .await
                            {
                                Ok(Some(reason)) => {
                                    tracing::info!(
                                        user_id = %self.user_id,
                                        room_id = %self.room_id,
                                        reason,
                                        "Real-time access is no longer valid (detected after disconnect signal lag), disconnecting"
                                    );
                                    self.skip_cleanup_user_left
                                        .store(true, std::sync::atomic::Ordering::Relaxed);
                                    break;
                                }
                                Ok(None) => {}
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        "Failed to verify membership after disconnect signal lag"
                                    );
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            tracing::error!("Disconnect signal channel closed");
                            break;
                        }
                    }
                }

                // Admin events from cluster (cross-replica room kick or platform ban propagation)
                admin_event = admin_rx.recv() => {
                    match admin_event {
                        Ok(RealtimeEvent::KickUser { ref user_id, ref reason, .. }) => {
                            // Invalidate membership cache immediately so the disconnected user
                            // cannot send messages during the remaining cache TTL window.
                            let cache_key = (self.room_id, *user_id);
                            self.membership_cache.invalidate(&cache_key);

                            if *user_id == self.user_id {
                                tracing::info!(
                                    user_id = %self.user_id,
                                    reason = %reason,
                                    "Received cross-replica KickUser event, disconnecting"
                                );
                                self.skip_cleanup_user_left.store(
                                    true,
                                    std::sync::atomic::Ordering::Relaxed,
                                );
                                break;
                            }
                        }
                        Ok(RealtimeEvent::KickUserFromRoom { ref user_id, ref room_id, ref reason, .. }) => {
                            // Invalidate membership cache immediately so the kicked or platform-banned
                            // user cannot send messages during the remaining cache TTL window.
                            let cache_key = (*room_id, *user_id);
                            self.membership_cache.invalidate(&cache_key);

                            if *user_id == self.user_id && *room_id == self.room_id {
                                tracing::info!(
                                    user_id = %self.user_id,
                                    room_id = %self.room_id,
                                    reason = %reason,
                                    "Received cross-replica KickUserFromRoom event, disconnecting"
                                );
                                self.skip_cleanup_user_left.store(
                                    true,
                                    std::sync::atomic::Ordering::Relaxed,
                                );
                                break;
                            }
                        }
                        Ok(RealtimeEvent::UserLeft { ref user_id, ref room_id, .. }) => {
                            if *user_id == self.user_id && *room_id == self.room_id {
                                tracing::info!(
                                    user_id = %self.user_id,
                                    room_id = %self.room_id,
                                    "Received cross-replica UserLeft event, disconnecting"
                                );
                                // UserLeft was already published by the leave_room
                                // or delete_room API call. Skip the redundant
                                // broadcast in cleanup().
                                self.skip_cleanup_user_left.store(true, std::sync::atomic::Ordering::Relaxed);
                                break;
                            }
                        }
                        Ok(RealtimeEvent::UserNotification { ref user_id, ref title, ref content, ref notification_type, ref notification_id, timestamp, .. }) => {
                            // Uses the dedicated Notification variant (not ErrorMessage abuse).
                            if *user_id == self.user_id {
                                let msg = user_notification_server_message(
                                    notification_id.clone(),
                                    notification_type.clone(),
                                    title.clone(),
                                    content.clone(),
                                    timestamp,
                                );
                                if let Err(e) = stream.send(msg) {
                                    tracing::error!("Failed to push notification to WebSocket: {}", e);
                                    break;
                                }
                            }
                        }
                        Ok(RealtimeEvent::SystemNotification { ref message, timestamp, .. }) => {
                            let msg = match system_notification_server_message(message.clone(), timestamp) {
                                Ok(msg) => msg,
                                Err(error) => {
                                    tracing::error!(error = %error, "Invalid system notification realtime event");
                                    break;
                                }
                            };
                            if let Err(e) = stream.send(msg) {
                                tracing::error!("Failed to push system notification to WebSocket: {}", e);
                                break;
                            }
                        }
                        Ok(RealtimeEvent::ProviderCredentialChanged { ref event_id, ref user_id, ref provider, ref server_id, .. }) => {
                            self.resource_observer.handle_provider_credential_changed_admin_event(
                                event_id,
                                user_id,
                                provider,
                                server_id,
                            )
                            .await;
                        }
                        Ok(RealtimeEvent::CacheInvalidate { ref event_id, ref targets, .. }) => {
                            self.resource_observer.handle_cache_invalidate_admin_event(event_id, targets).await;
                        }
                        Ok(_) => {
                            // Other admin events (KickPublisher, etc.) not relevant to this connection
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            // Channel lagged: we may have missed critical KickUser/KickUserFromRoom events.
                            // Re-subscribe to get a fresh receiver so future events are not lost.
                            tracing::warn!(
                                lagged = n,
                                user_id = %self.user_id,
                                "Admin event channel lagged, re-subscribing and verifying membership"
                            );
                            admin_rx = self.event_service.subscribe_admin_events();

                            match realtime_membership_denial_reason(
                                &self.room_service,
                                &self.room_id,
                                &self.user_id,
                            )
                            .await
                            {
                                Ok(Some(reason)) => {
                                    tracing::info!(
                                        user_id = %self.user_id,
                                        room_id = %self.room_id,
                                        reason,
                                        "Real-time access is no longer valid (detected after admin event lag), disconnecting"
                                    );
                                    self.skip_cleanup_user_left
                                        .store(true, std::sync::atomic::Ordering::Relaxed);
                                    break;
                                }
                                Ok(None) => {}
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        "Failed to verify membership after admin event lag"
                                    );
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            tracing::error!("Admin event channel closed");
                            break;
                        }
                    }
                }

                // When notification_service is configured, notifications are pushed
                // directly without depending on the gRPC bridge task.
                result = async {
                    match notification_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    match result {
                        Ok(event) => {
                            // Only push if this notification targets the connected user
                            if event.user_id == self.user_id {
                                let data = serde_json::json!({
                                    "type": "user_notification",
                                    "notification_id": event.notification.id.to_string(),
                                    "notification_type": event.notification.notification_type.to_string(),
                                    "title": &event.notification.title,
                                    "content": &event.notification.content,
                                });
                                let msg = ServerMessage {
                                    message: Some(synctv_proto::client::server_message::Message::Notification(
                                        synctv_proto::client::UserNotification {
                                            notification_id: event.notification.id.to_string(),
                                            notification_type: event.notification.notification_type.to_string(),
                                            title: event.notification.title,
                                            content: event.notification.content,
                                            data: data.to_string(),
                                            timestamp: event.notification.created_at.timestamp(),
                                        },
                                    )),
                                };
                                if let Err(e) = stream.send(msg) {
                                    tracing::error!("Failed to push direct notification to WebSocket: {}", e);
                                    break;
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(
                                lagged = n,
                                user_id = %self.user_id,
                                "Notification event channel lagged, re-subscribing"
                            );
                            notification_rx = self
                                .notification_service
                                .as_ref()
                                .map(|svc| svc.subscribe_events());
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            tracing::debug!("Notification event channel closed");
                            notification_rx = None;
                        }
                    }
                }

                // Heartbeat/health check every 30 seconds.
                // Also acts as a periodic membership re-validation backstop:
                // verifies the user is still a valid (active member of the room. This catches cases where the disconnect
                // signal channel lagged and the room kick or platform ban signal was lost.
                // Uses the membership cache to reduce database queries: if a
                // cached entry exists and shows the user as a valid member, the
                // DB query is skipped. When a KickUser or KickUserFromRoom admin
                // event arrives, the cache entry is invalidated immediately,
                // forcing the next heartbeat to re-query the DB.
                _ = heartbeat_interval.tick() => {
                    if !stream.is_alive() {
                        tracing::info!("Connection no longer alive");
                        break;
                    }
                    if let Err(e) = stream.ping() {
                        tracing::info!("Ping failed, connection dead: {}", e);
                        break;
                    }

                    if self.principal.is_guest() {
                        match self.guest_admission_denial_reason().await {
                            Ok(Some(reason)) => {
                                tracing::info!(
                                    room_id = %self.room_id,
                                    user_id = %self.user_id,
                                    reason,
                                    "Periodic check: guest access is no longer valid, disconnecting"
                                );
                                break;
                            }
                            Ok(None) => continue,
                            Err(error) => {
                                tracing::warn!(
                                    error = %error,
                                    user_id = %self.user_id,
                                    "Periodic guest access check failed (will retry)"
                                );
                                continue;
                            }
                        }
                    }

                    // Check membership cache first to avoid unnecessary DB queries.
                    let cache_key = (self.room_id, self.user_id);
                    if let Some(cached) = self.membership_cache.get(&cache_key) {
                        if !cached.is_member {
                            tracing::info!(
                                user_id = %self.user_id,
                                room_id = %self.room_id,
                                "Periodic check (cached): user is no longer a member, disconnecting"
                            );
                            self.skip_cleanup_user_left
                                .store(true, std::sync::atomic::Ordering::Relaxed);
                            break;
                        }
                        // Cache hit with valid member status -- skip DB query
                        continue;
                    }

                    // Cache miss: query database and populate cache.
                    match probe_realtime_membership_access(
                        &self.room_service,
                        &self.room_id,
                        &self.user_id,
                    )
                    .await
                    {
                        Ok(RealtimeMembershipAccess::Allowed(member)) => {
                            let cached = CachedMembership::from_member(Some(&member));
                            self.membership_cache.insert(cache_key, cached);
                        }
                        Ok(RealtimeMembershipAccess::Denied(reason)) => {
                            let cached = CachedMembership::from_member(None);
                            self.membership_cache.insert(cache_key, cached);
                            tracing::info!(
                                user_id = %self.user_id,
                                room_id = %self.room_id,
                                reason,
                                "Periodic check: real-time access is no longer valid, disconnecting"
                            );
                            self.skip_cleanup_user_left
                                .store(true, std::sync::atomic::Ordering::Relaxed);
                            break;
                        }
                        Err(e) => {
                            // Log but don't disconnect — transient DB error should not
                            // kick valid users. Will retry on the next 30-second tick.
                            // Don't cache the error -- next tick will retry.
                            tracing::warn!(
                                error = %e,
                                user_id = %self.user_id,
                                "Periodic membership check failed (will retry)"
                            );
                        }
                    }
                }
            }
        }

        // Cleanup: notify cluster that user left
        self.cleanup(&room_id_str).await;

        Ok(())
    }

    /// Broadcast `UserJoined` event to cluster replicas.
    async fn broadcast_user_joined(
        &self,
        member: Option<&synctv_core::models::RoomMember>,
        room_settings: Option<&synctv_core::models::RoomSettings>,
    ) {
        match self
            .presence_service
            .user_has_other_connection_in_room(
                self.user_id,
                self.room_id,
                self.connection_id.as_str(),
            )
            .await
        {
            Ok(true) => {
                tracing::debug!(
                    room_id = %self.room_id,
                    user_id = %self.user_id,
                    connection_id = %self.connection_id,
                    "Skipping UserJoined broadcast because the user is already present in the room on another connection"
                );
                return;
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    room_id = %self.room_id,
                    user_id = %self.user_id,
                    connection_id = %self.connection_id,
                    "Distributed same-user presence lookup failed during join; continuing with UserJoined broadcast to avoid missing online signal"
                );
            }
            Ok(false) => {}
        }

        let (
            role_proto,
            permissions,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
        ) = if let Some(identity) = self.principal.guest_identity() {
            (
                synctv_proto::common::RoomMemberRole::Guest as i32,
                identity.permissions,
                synctv_core::models::RoomPermissionSet(0),
                synctv_core::models::RoomPermissionSet(0),
                synctv_core::models::RoomPermissionSet(0),
                synctv_core::models::RoomPermissionSet(0),
            )
        } else {
            match member {
                Some(member) => {
                    let Some(settings) = room_settings else {
                        tracing::error!(
                            room_id = %self.room_id,
                            user_id = %self.user_id,
                            connection_id = %self.connection_id,
                            "Skipping UserJoined broadcast because room settings are missing"
                        );
                        return;
                    };
                    let effective = self
                        .room_service
                        .permission_service()
                        .effective_member_permissions(member, settings);
                    let role = room_role_to_proto(member.role);
                    (
                        role,
                        effective,
                        synctv_core::models::RoomPermissionSet(member.added_permissions),
                        synctv_core::models::RoomPermissionSet(member.removed_permissions),
                        synctv_core::models::RoomPermissionSet(member.admin_added_permissions),
                        synctv_core::models::RoomPermissionSet(member.admin_removed_permissions),
                    )
                }
                None => {
                    // Fallback: if we can't fetch membership, use Member defaults
                    (
                        synctv_proto::common::RoomMemberRole::Member as i32,
                        synctv_core::models::RoomPermissionSet::default_member(),
                        synctv_core::models::RoomPermissionSet(0),
                        synctv_core::models::RoomPermissionSet(0),
                        synctv_core::models::RoomPermissionSet(0),
                        synctv_core::models::RoomPermissionSet(0),
                    )
                }
            }
        };

        let event = if self.principal.is_guest() {
            let guest_id = match self.public_actor_id() {
                Ok(actor_id) => actor_id,
                Err(error) => {
                    tracing::error!(
                        room_id = %self.room_id,
                        user_id = %self.user_id,
                        error = %error,
                        "Skipping GuestJoined broadcast because actor public id encoding failed"
                    );
                    return;
                }
            };
            RealtimeEvent::GuestJoined {
                event_id: synctv_common::snanoid!(16),
                room_id: self.room_id,
                guest_id,
                username: self.username.clone(),
                permissions,
                role: role_proto,
                joined_at: chrono::Utc::now(),
                timestamp: chrono::Utc::now(),
            }
        } else {
            RealtimeEvent::UserJoined {
                event_id: synctv_common::snanoid!(16),
                room_id: self.room_id,
                user_id: self.user_id,
                username: self.username.clone(),
                permissions,
                role: role_proto,
                added_permissions,
                removed_permissions,
                admin_added_permissions,
                admin_removed_permissions,
                joined_at: chrono::Utc::now(),
                timestamp: chrono::Utc::now(),
            }
        };
        let outcome = self.event_service.broadcast_outcome(event);
        if outcome.distributed_delivery_missed() {
            tracing::warn!(
                room_id = %self.room_id,
                user_id = %self.user_id,
                "UserJoined broadcast missed the distributed fan-out path (non-critical: join is local-only)"
            );
        }
    }

    /// Cleanup on disconnect
    async fn cleanup(&self, room_id: &str) {
        self.resource_observer.clear_observations().await;

        // If this connection had an active WebRTC session, decrement the metric
        // and broadcast WebRtcLeave so other peers can clean up.
        // Use Acquire ordering to synchronize with the Release store in handle_webrtc_join/leave.
        // IMPORTANT: We must check if the connection is STILL marked as RTC-joined
        // in the connection manager before decrementing the metric. This prevents
        // a race condition where:
        // 1. Cleanup task times out the WebRTC session (mark_rtc_joined(false))
        // 2. Connection ungracefully disconnects
        // 3. cleanup() sees has_webrtc_session=true and decrements the metric again
        // Result: Metric underflow (negative value)
        // By checking the connection manager's state, we ensure idempotency:
        // - If the cleanup task already timed out the session, the connection
        //   manager will have rtc_joined=false, and we skip the decrement
        // - If the user explicitly left WebRTC, the flag is already false, and we skip
        // - Only if the connection truly had an active session do we decrement
        if self
            .has_webrtc_session
            .swap(false, std::sync::atomic::Ordering::Acquire)
        {
            // Check if the connection is still marked as RTC-joined in the connection manager
            // This prevents double-decrement if the cleanup task already timed out the session
            let is_still_rtc_joined = self
                .connection_service
                .get_connection(self.connection_id.as_str())
                .is_some_and(|conn| conn.rtc_joined);

            if is_still_rtc_joined {
                // Only decrement the metric if the connection was still RTC-joined
                synctv_core::metrics::http::WEBRTC_PEERS_ACTIVE.dec();

                // Mark the connection as no longer RTC-joined in the connection manager
                self.connection_service.mark_rtc_joined(
                    &self.room_id,
                    &self.user_id,
                    self.connection_id.as_str(),
                    false,
                );

                // Broadcast WebRtcLeave so other peers know this user dropped
                match self.public_actor_id() {
                    Ok(actor_id) => {
                        let leave_event = RealtimeEvent::WebRTCLeave {
                            event_id: synctv_common::snanoid!(16),
                            room_id: self.room_id,
                            actor_id,
                            conn_id: self.connection_id.as_str().to_string(),
                            timestamp: chrono::Utc::now(),
                        };
                        self.event_service.broadcast(leave_event);
                    }
                    Err(error) => {
                        tracing::error!(
                            room_id = %self.room_id,
                            user_id = %self.user_id,
                            connection_id = %self.connection_id,
                            error = %error,
                            "Skipping WebRTC leave broadcast because actor public id encoding failed"
                        );
                    }
                }

                tracing::info!(
                    user = %self.username,
                    room = %room_id,
                    connection = %self.connection_id,
                    "WebRTC session cleaned up on disconnect"
                );
            } else {
                // Session was already cleaned up by timeout task or explicit leave
                // Just clear the connection manager state (idempotent)
                tracing::debug!(
                    user = %self.username,
                    room = %room_id,
                    connection = %self.connection_id,
                    "WebRTC session already cleaned up (skipped metric decrement and broadcast)"
                );
            }
        }

        // If the disconnect was triggered by a realtime event that already
        // published UserLeft, skip the redundant broadcast to avoid duplicate
        // UserLeft events.
        if self
            .skip_cleanup_user_left
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            tracing::debug!(
                user = %self.username,
                room = %room_id,
                "Skipping UserLeft broadcast in cleanup (already published by API call)"
            );
            // Still unregister from connection manager
            self.connection_service
                .unregister(self.connection_id.as_str())
                .await;
            self.event_service.unsubscribe(self.connection_id.as_str());
            return;
        }

        let has_other_local_connection = self
            .connection_service
            .get_user_connections(&self.user_id)
            .into_iter()
            .any(|conn| {
                conn.connection_id != self.connection_id.as_str()
                    && conn
                        .room_id
                        .as_ref()
                        .is_some_and(|rid| rid == &self.room_id)
            });

        let should_broadcast_left = match self
            .presence_service
            .user_has_other_connection_in_room(
                self.user_id,
                self.room_id,
                self.connection_id.as_str(),
            )
            .await
        {
            Ok(has_other_connection) => {
                should_broadcast_user_left(has_other_local_connection, Ok(has_other_connection))
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    user = %self.username,
                    room = %room_id,
                    connection = %self.connection_id,
                    "Distributed same-user presence lookup failed during cleanup; using local presence fallback for UserLeft broadcast"
                );
                should_broadcast_user_left(has_other_local_connection, Err(()))
            }
        };

        // Broadcast UserLeft BEFORE unregistering from the connection manager.
        // This order prevents state divergence: if the broadcast reaches subscribers
        // while this connection is still registered, they see a consistent view.
        // Previously, unregistering first could leave the hub with a stale subscriber
        // if the broadcast was delayed or had no receivers.
        let event = if self.principal.is_guest() {
            let guest_id = match self.public_actor_id() {
                Ok(actor_id) => actor_id,
                Err(error) => {
                    tracing::error!(
                        room_id = %self.room_id,
                        user_id = %self.user_id,
                        error = %error,
                        "Skipping GuestLeft broadcast because actor public id encoding failed"
                    );
                    self.connection_service
                        .unregister(self.connection_id.as_str())
                        .await;
                    self.event_service.unsubscribe(self.connection_id.as_str());
                    return;
                }
            };
            RealtimeEvent::GuestLeft {
                event_id: synctv_common::snanoid!(16),
                room_id: self.room_id,
                guest_id,
                username: self.username.clone(),
                timestamp: chrono::Utc::now(),
            }
        } else {
            let role = self
                .current_room_role
                .load(std::sync::atomic::Ordering::Relaxed);
            RealtimeEvent::UserLeft {
                event_id: synctv_common::snanoid!(16),
                room_id: self.room_id,
                user_id: self.user_id,
                username: self.username.clone(),
                role,
                timestamp: chrono::Utc::now(),
            }
        };
        let result = if should_broadcast_left {
            Some(self.event_service.broadcast_outcome(event))
        } else {
            tracing::debug!(
                user = %self.username,
                room = %room_id,
                connection = %self.connection_id,
                "Skipping UserLeft broadcast in cleanup because another connection for the same user remains in the room"
            );
            None
        };

        if let Some(outcome) = result {
            if outcome.distributed_delivery_missed() {
                tracing::warn!(
                    user = %self.username,
                    room = %room_id,
                    connection = %self.connection_id,
                    local_delivered = outcome.local_delivered(),
                    distributed_available = outcome.distributed_available(),
                    "UserLeft distributed publish missed during cleanup"
                );
            }
        }

        // Now unregister from connection manager after broadcast has been sent
        self.connection_service
            .unregister(self.connection_id.as_str())
            .await;
        self.event_service.unsubscribe(self.connection_id.as_str());

        tracing::info!(
            "Cleanup complete for user {} in room {} (connection: {})",
            self.username,
            room_id,
            self.connection_id
        );
    }

    /// Start the message handling loop
    ///
    /// This method:
    /// 1. Registers the connection and joins the room (enforcing connection limits)
    /// 2. Subscribes to realtime events and forwards them to the client
    /// 3. Spawns a task to handle incoming client messages
    /// 4. Returns a sender and a cancellation token for the caller to manage lifecycle
    ///
    /// Returns a tuple of (sender, `CancellationToken`), or an error if connection limits
    /// are exceeded. Drop the `CancellationToken` or call `cancel()` on it to stop the
    /// spawned tasks and trigger cleanup (unregister, unsubscribe).
    pub async fn start(
        &self,
    ) -> Result<
        (
            tokio::sync::mpsc::Sender<ClientMessage>,
            tokio_util::sync::CancellationToken,
        ),
        String,
    > {
        // Register connection with connection manager
        self.connection_service
            .register_actor(
                self.connection_id.clone().into_string(),
                self.user_id,
                self.public_actor_id()?,
            )
            .await?;

        self.pre_join_after_registration()
            .await
            .map_err(String::from)?;

        let cancel_token = tokio_util::sync::CancellationToken::new();
        let room_id_str = self.public_room_id()?;

        // Fetch member data and room settings once and reuse them for the join
        // payload and realtime event. Authenticated users must have both so
        // outbound permission snapshots cannot silently fall back to role-only
        // defaults when a read fails.
        let initial_join = self.take_initial_realtime_join_state(&room_id_str).await?;
        if let Some(member) = initial_join.member.as_ref() {
            self.current_room_role
                .store(i32::from(member.role), std::sync::atomic::Ordering::Relaxed);
        }

        self.broadcast_user_joined(
            initial_join.member.as_ref(),
            initial_join.room_settings.as_ref(),
        )
        .await;

        // Use bounded channel to prevent memory exhaustion from fast clients
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ClientMessage>(256);

        let room_id_str = self.public_room_id()?;
        let event_connection_id = self.connection_id.as_str().to_string();
        let mut rx_events = match self.take_room_event_subscription().await {
            Ok(rx_events) => rx_events,
            Err(error) => {
                self.cleanup(&room_id_str).await;
                return Err(error);
            }
        };
        let sender = self.sender.clone();
        let event_handler = self.clone();
        let public_id_codec = self.public_id_codec.clone();
        let event_token = cancel_token.clone();
        spawn_monitored("messaging_event_dispatch", async move {
            loop {
                tokio::select! {
                    () = event_token.cancelled() => break,
                    event = rx_events.recv() => {
                        match event {
                            Some(event) => {
                                event_handler.apply_connection_state_from_room_event(&event);
                                let is_room_shutdown = matches!(
                                    event,
                                    RealtimeEvent::RoomDeleted { .. }
                                        | RealtimeEvent::RoomBanned { .. }
                                        | RealtimeEvent::RoomOwnerInactive { .. }
                                );

                                match event_handler
                                    .webrtc_event_server_message_for_current_connection(&event)
                                {
                                    Ok(Some(message)) => {
                                        if let Err(error) = sender.send(message) {
                                            tracing::error!(
                                                "Failed to send WebRTC server message: {}",
                                                error
                                            );
                                            event_token.cancel();
                                            break;
                                        }
                                    }
                                    Ok(None) => {}
                                    Err(error) => {
                                        tracing::error!(
                                            room_id = %event_handler.room_id,
                                            event_id = %event.event_id(),
                                            error = %error,
                                            "Failed to convert WebRTC event to server message"
                                        );
                                        event_token.cancel();
                                        break;
                                    }
                                }

                                if !matches!(event, RealtimeEvent::ChatMessageEvent { .. }) {
                                    let messages = match realtime_event_to_server_messages(
                                        &event,
                                        &room_id_str,
                                        &public_id_codec,
                                    ) {
                                        Ok(messages) => messages,
                                        Err(error) => {
                                            tracing::error!(
                                                room_id = %event_handler.room_id,
                                                event_id = %event.event_id(),
                                                error = %error,
                                                "Failed to convert realtime event to server message"
                                            );
                                            event_token.cancel();
                                            break;
                                        }
                                    };
                                    for msg in messages {
                                        if let Err(e) = sender.send(msg) {
                                            tracing::error!("Failed to send message: {}", e);
                                            event_token.cancel();
                                            break;
                                        }
                                    }
                                }

                                if let Err(error) =
                                    event_handler
                                        .resource_observer
                                        .room_hub
                                        .refresh_for_room_event(&event, Some(&event_connection_id))
                                        .await
                                {
                                    tracing::error!(
                                        "Failed to refresh observed resources in start(): {}",
                                        error
                                    );
                                    event_token.cancel();
                                    break;
                                }

                                // After delivering a terminal room-wide admin event, trigger cancellation so
                                // cleanup fires only after the event has been forwarded.
                                // This prevents the race where the cleanup task fires
                                // before the critical event reaches the client.
                                if is_room_shutdown {
                                    tracing::info!(
                                        "Terminal room event delivered in start(), triggering cleanup"
                                    );
                                    event_token.cancel();
                                    break;
                                }
                            }
                            None => break,
                        }
                    }
                    () = async {
                        match event_handler
                            .resource_observer
                            .next_expired_resource_refresh_deadline()
                            .await {
                            Some(deadline) => tokio::time::sleep_until(deadline).await,
                            None => std::future::pending::<()>().await,
                        }
                    } => {
                        if let Err(error) = event_handler
                            .resource_observer
                            .refresh_expired_resource_observations()
                            .await
                        {
                            tracing::error!(
                                "Failed to refresh observed resources in start(): {}",
                                error
                            );
                            event_token.cancel();
                            break;
                        }
                    }
                }
            }
        });

        // Spawn task to handle incoming messages (with rate limiting matching run())
        let handler = self.clone();
        let msg_token = cancel_token.clone();
        let global_msg_rate_limit = self.ws_message_rate_limit;
        spawn_monitored("messaging_client_handler", async move {
            let mut global_msg_count: u32 = 0;
            let mut global_msg_window_start = tokio::time::Instant::now();
            let message_control = ExecutionControl::from_parts(None, msg_token.clone());
            loop {
                tokio::select! {
                    () = msg_token.cancelled() => break,
                    msg = rx.recv() => {
                        match msg {
                            Some(msg) => {
                                handler.connection_service.record_message(&handler.connection_id);

                                // Global per-connection rate limit check (matching run() logic)
                                let now = tokio::time::Instant::now();
                                if now.duration_since(global_msg_window_start) >= std::time::Duration::from_secs(1) {
                                    global_msg_count = 0;
                                    global_msg_window_start = now;
                                }
                                global_msg_count += 1;
                                if global_msg_count > global_msg_rate_limit {
                                    tracing::warn!(
                                        connection_id = %handler.connection_id,
                                        limit = global_msg_rate_limit,
                                        "gRPC start() message rate limit exceeded, dropping message"
                                    );
                                    continue;
                                }

                                // Backpressure control: try to acquire a semaphore permit.
                                // If the system is overloaded, skip this message.
                                let semaphore = handler.concurrency_config.semaphore();
                                let Ok(permit) = semaphore.try_acquire_owned() else {
                                    tracing::warn!(
                                        connection_id = %handler.connection_id,
                                        "System overloaded: message processing semaphore exhausted in start()"
                                    );
                                    continue;
                                };

                                // Process message with semaphore permit held
                                let _permit = permit;
                                if let Err(e) = handler
                                    .handle_client_message_with_control(&msg, Some(&message_control))
                                    .await
                                {
                                    tracing::error!("Failed to handle client message: {}", e);
                                    if let Err(send_err) = handler.sender.send(
                                        Self::error_server_message(e.clone()),
                                    ) {
                                        tracing::error!(
                                            "Failed to send message error to client in start(): {}",
                                            send_err
                                        );
                                        msg_token.cancel();
                                        break;
                                    }
                                }
                            }
                            None => break,
                        }
                    }
                }
            }
        });

        // Spawn task to monitor disconnect signals and admin events.
        // When a relevant signal is received, cancel the token to stop all other tasks.
        {
            let mut disconnect_rx = self.connection_service.subscribe_disconnect();
            let mut admin_rx = self.event_service.subscribe_admin_events();
            let disconnect_token = cancel_token.clone();
            let connection_id = self.connection_id.as_str().to_string();
            let user_id = self.user_id;
            let room_id = self.room_id;
            let room_service = Arc::clone(&self.room_service);
            let event_service = Arc::clone(&self.event_service);
            let connection_service = self.connection_service.clone();
            let admin_sender = self.sender.clone();
            let admin_handler = self.clone();
            let skip_cleanup_user_left = Arc::clone(&self.skip_cleanup_user_left);
            let is_guest = self.principal.is_guest();

            spawn_monitored("messaging_disconnect_monitor", async move {
                loop {
                    tokio::select! {
                        () = disconnect_token.cancelled() => break,

                        signal = disconnect_rx.recv() => {
                            let should_disconnect = match &signal {
                                Ok(synctv_realtime::sync::DisconnectSignal::Connection(conn_id)) => {
                                    *conn_id == connection_id
                                }
                                Ok(synctv_realtime::sync::DisconnectSignal::User(uid)) => {
                                    *uid == user_id
                                }
                                Ok(synctv_realtime::sync::DisconnectSignal::Room(rid)) => {
                                    *rid == room_id
                                }
                                Ok(synctv_realtime::sync::DisconnectSignal::UserFromRoom { user_id: uid, room_id: rid }) => {
                                    *uid == user_id && *rid == room_id
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => false,
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => true,
                            };
                            // Handle lag separately (needs mutable borrow of disconnect_rx)
                            if let Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) = signal {
                                tracing::warn!(
                                    lagged = n,
                                    user_id = %user_id,
                                    "Disconnect signal channel lagged in start(), re-subscribing and verifying"
                                );
                                disconnect_rx = connection_service.subscribe_disconnect();
                                if !is_guest {
                                    match realtime_membership_denial_reason(
                                        &room_service,
                                        &room_id,
                                        &user_id,
                                    )
                                    .await
                                    {
                                        Ok(Some(reason)) => {
                                            tracing::info!(
                                                user_id = %user_id,
                                                room_id = %room_id,
                                                reason,
                                                "start() real-time access is no longer valid after disconnect signal lag"
                                            );
                                            skip_cleanup_user_left.store(true, std::sync::atomic::Ordering::Relaxed);
                                            disconnect_token.cancel();
                                            break;
                                        }
                                        Ok(None) => {}
                                        Err(error) => {
                                            tracing::warn!(
                                                error = %error,
                                                user_id = %user_id,
                                                room_id = %room_id,
                                                "start() failed to verify membership after disconnect signal lag"
                                            );
                                        }
                                    }
                                }
                            } else if should_disconnect {
                                if let Ok(signal) = &signal {
                                    if disconnect_signal_requires_skip_cleanup(signal, &user_id, &room_id, &connection_id) {
                                        skip_cleanup_user_left.store(true, std::sync::atomic::Ordering::Relaxed);
                                    }
                                }
                                tracing::info!(
                                    connection_id = %connection_id,
                                    "Disconnect signal received in start(), cancelling"
                                );
                                disconnect_token.cancel();
                                break;
                            }
                        }

                        admin_event = admin_rx.recv() => {
                            // Uses the dedicated Notification variant (not ErrorMessage abuse).
                            if let Ok(RealtimeEvent::UserNotification { user_id: uid, title, content, notification_type, notification_id, timestamp, .. }) = &admin_event {
                                if *uid == user_id {
                                    let msg = user_notification_server_message(
                                        notification_id.clone(),
                                        notification_type.clone(),
                                        title.clone(),
                                        content.clone(),
                                        *timestamp,
                                    );
                                    if let Err(e) = admin_sender.send(msg) {
                                        tracing::error!("Failed to push notification in start(): {}", e);
                                        disconnect_token.cancel();
                                        break;
                                    }
                                }
                                continue;
                            }
                            if let Ok(RealtimeEvent::SystemNotification { message, timestamp, .. }) = &admin_event {
                                let msg = match system_notification_server_message(message.clone(), *timestamp) {
                                    Ok(msg) => msg,
                                    Err(error) => {
                                        tracing::error!(error = %error, "Invalid system notification realtime event");
                                        disconnect_token.cancel();
                                        break;
                                    }
                                };
                                if let Err(e) = admin_sender.send(msg) {
                                    tracing::error!(
                                        "Failed to push system notification in start(): {}",
                                        e
                                    );
                                    disconnect_token.cancel();
                                    break;
                                }
                                continue;
                            }
                            if let Ok(RealtimeEvent::ProviderCredentialChanged { event_id, user_id: changed_user_id, provider, server_id, .. }) = &admin_event {
                                admin_handler
                                    .resource_observer
                                    .handle_provider_credential_changed_admin_event(
                                        event_id,
                                        changed_user_id,
                                        provider,
                                        server_id,
                                    )
                                    .await;
                                continue;
                            }
                            if let Ok(RealtimeEvent::CacheInvalidate { event_id, targets, .. }) = &admin_event
                            {
                                admin_handler
                                    .resource_observer
                                    .handle_cache_invalidate_admin_event(event_id, targets)
                                    .await;
                                continue;
                            }
                            let should_disconnect = match &admin_event {
                                Ok(RealtimeEvent::KickUser { user_id: uid, .. }) => {
                                    *uid == user_id
                                }
                                Ok(
                                    RealtimeEvent::KickUserFromRoom { user_id: uid, room_id: rid, .. }
                                    | RealtimeEvent::UserLeft { user_id: uid, room_id: rid, .. },
                                ) => {
                                    *uid == user_id && *rid == room_id
                                }
                                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => false,
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => true,
                            };
                            // Handle lag separately
                            if let Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) = admin_event {
                                tracing::warn!(
                                    lagged = n,
                                    user_id = %user_id,
                                    "Admin event channel lagged in start(), re-subscribing and verifying"
                                );
                                admin_rx = event_service.subscribe_admin_events();
                                if !is_guest {
                                    match realtime_membership_denial_reason(
                                        &room_service,
                                        &room_id,
                                        &user_id,
                                    )
                                    .await
                                    {
                                        Ok(Some(reason)) => {
                                            tracing::info!(
                                                user_id = %user_id,
                                                room_id = %room_id,
                                                reason,
                                                "start() real-time access is no longer valid after admin event lag"
                                            );
                                            skip_cleanup_user_left.store(true, std::sync::atomic::Ordering::Relaxed);
                                            disconnect_token.cancel();
                                            break;
                                        }
                                        Ok(None) => {}
                                        Err(error) => {
                                            tracing::warn!(
                                                error = %error,
                                                user_id = %user_id,
                                                room_id = %room_id,
                                                "start() failed to verify membership after admin event lag"
                                            );
                                        }
                                    }
                                }
                            } else if should_disconnect {
                                if let Ok(event) = &admin_event {
                                    if admin_event_requires_skip_cleanup(event, &user_id, &room_id) {
                                        skip_cleanup_user_left.store(true, std::sync::atomic::Ordering::Relaxed);
                                    }
                                }
                                tracing::info!(
                                    connection_id = %connection_id,
                                    "Admin event triggered disconnect in start(), cancelling"
                                );
                                disconnect_token.cancel();
                                break;
                            }
                        }
                    }
                }
            });
        }

        // Spawn periodic heartbeat task for membership re-validation (mirrors run() behavior).
        // Verifies every 25-35 seconds that the user is still a valid, member.
        // Jitter prevents the thundering-herd problem where all 1000+ concurrent connections
        // fire their DB membership checks simultaneously at the same 30-second boundary.
        // This catches cases where disconnect signals were lost (e.g., channel lag).
        {
            let heartbeat_token = cancel_token.clone();
            let heartbeat_room_id = self.room_id;
            let heartbeat_user_id = self.user_id;
            let heartbeat_room_service = Arc::clone(&self.room_service);
            let heartbeat_sender = Arc::clone(&self.sender);
            let heartbeat_schedule = self.heartbeat_schedule;
            let skip_cleanup_user_left = Arc::clone(&self.skip_cleanup_user_left);
            let heartbeat_handler = self.clone();
            spawn_monitored("messaging_heartbeat", async move {
                // Derive jitter from the user_id bytes so each connection gets a
                // stable-but-different offset within the 25–35 s window.
                let period = heartbeat_schedule.period_for_user(&heartbeat_user_id);
                let mut interval = tokio::time::interval(period);
                interval.tick().await; // Skip the immediate first tick
                loop {
                    tokio::select! {
                        () = heartbeat_token.cancelled() => break,
                        _ = interval.tick() => {
                            // Check connection liveness first (mirrors run() behavior)
                            if !heartbeat_sender.is_alive() {
                                tracing::info!("start() connection no longer alive");
                                heartbeat_token.cancel();
                                break;
                            }
                            if let Err(e) = heartbeat_sender.ping() {
                                tracing::info!("start() ping failed, connection dead: {}", e);
                                heartbeat_token.cancel();
                                break;
                            }

                            if heartbeat_handler.principal.is_guest() {
                                match heartbeat_handler.guest_admission_denial_reason().await {
                                    Ok(Some(reason)) => {
                                        tracing::info!(
                                            user_id = %heartbeat_user_id,
                                            room_id = %heartbeat_room_id,
                                            reason,
                                            "start() periodic check: guest access is no longer valid, disconnecting"
                                        );
                                        heartbeat_token.cancel();
                                        break;
                                    }
                                    Ok(None) => continue,
                                    Err(e) => {
                                        tracing::warn!(
                                            error = %e,
                                            user_id = %heartbeat_user_id,
                                            "start() periodic guest access check failed (will retry)"
                                        );
                                        continue;
                                    }
                                }
                            }

                            match realtime_membership_denial_reason(
                                &heartbeat_room_service,
                                &heartbeat_room_id,
                                &heartbeat_user_id,
                            )
                            .await
                            {
                                Ok(Some(reason)) => {
                                    tracing::info!(
                                        user_id = %heartbeat_user_id,
                                        room_id = %heartbeat_room_id,
                                        reason,
                                        "start() periodic check: real-time access is no longer valid, disconnecting"
                                    );
                                    skip_cleanup_user_left
                                        .store(true, std::sync::atomic::Ordering::Relaxed);
                                    heartbeat_token.cancel();
                                    break;
                                }
                                Ok(None) => {}
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        user_id = %heartbeat_user_id,
                                        "start() periodic membership check failed (will retry)"
                                    );
                                }
                            }
                        }
                    }
                }
            });
        }

        // Spawn cleanup task that waits for cancellation
        let cleanup_handler = self.clone();
        let cleanup_room_id = self.public_room_id()?;
        let cleanup_token = cancel_token.clone();
        spawn_monitored("messaging_cleanup", async move {
            cleanup_token.cancelled().await;
            cleanup_handler.cleanup(&cleanup_room_id).await;
        });

        Ok((tx, cancel_token))
    }
}

impl StreamMessageHandler {
    /// Handle incoming client message with all validations
    pub async fn handle_client_message(&self, msg: &ClientMessage) -> Result<(), String> {
        self.handle_client_message_with_control(msg, None).await
    }

    pub async fn handle_client_message_with_control(
        &self,
        msg: &ClientMessage,
        control: Option<&ExecutionControl>,
    ) -> Result<(), String> {
        use synctv_proto::client::client_message::Message;

        match &msg.message {
            Some(Message::Chat(chat_msg)) => {
                if self.principal.is_guest() {
                    return Err("Guests cannot send chat messages".to_string());
                }

                // ChatService handles permissions, room settings, rate limiting,
                // content filtering, persistence, and event dispatch.
                self.handle_chat_message_with_control(chat_msg, control)
                    .await?;
            }
            Some(Message::Heartbeat(_)) => {
                // Respond with HeartbeatAck to let client know server is alive
                // This completes the heartbeat request-response cycle
                self.send_heartbeat_ack()?;
            }
            Some(Message::Webrtc(command)) => {
                self.handle_webrtc_command(command).await?;
            }
            Some(Message::PlaybackUpdate(update)) => {
                self.handle_playback_source_update(update).await?;
            }
            Some(Message::PlaybackStateUpdate(update)) => {
                self.handle_playback_state_update(update).await?;
            }
            Some(Message::ObserveResource(observe)) => {
                self.ensure_observe_resource_allowed(observe).await?;
                self.resource_observer
                    .handle_observe_resource(observe)
                    .await?;
                self.resource_observer
                    .replay_chat_events_after(&self.chat_service, observe)
                    .await?;
                self.resource_observer
                    .replay_room_resource_events_after(observe)
                    .await?;
            }
            Some(Message::UnobserveResource(unobserve)) => {
                self.resource_observer
                    .handle_unobserve_resource(unobserve)
                    .await?;
            }
            None => {
                return Err("Empty message".to_string());
            }
        }

        Ok(())
    }

    async fn handle_chat_message_with_control(
        &self,
        chat_msg: &synctv_proto::client::ChatMessageSend,
        control: Option<&ExecutionControl>,
    ) -> Result<(), String> {
        if self.principal.is_guest() {
            self.ensure_guest_admission_for_action().await?;
            return Err("Guests cannot send chat messages".to_string());
        }

        // Delegate to ChatService which handles permission checks, content filtering,
        // rate limiting, and persistence (no fallback path).
        let attachments = chat_msg
            .attachments
            .iter()
            .map(proto_chat_attachment_to_core)
            .collect::<Result<Vec<_>, _>>()?;
        let reply_to_message_id = parse_optional_chat_message_id(&chat_msg.reply_to_message_id)?;
        let playback_state = self
            .room_service
            .playback_service()
            .get_state(&self.room_id)
            .await
            .map_err(|error| format!("Failed to load playback state for chat metadata: {error}"))?;
        let metadata = chat_metadata_for_send(
            serde_json::Value::Object(Default::default()),
            &chat_msg.display_position,
            &chat_msg.display_color,
            Some(&playback_state),
        )?;
        let outcome = self
            .chat_service
            .send_message_event_with_control_outcome(
                SendChatMessage {
                    room_id: self.room_id,
                    user_id: self.user_id,
                    client_message_id: (!chat_msg.client_message_id.trim().is_empty())
                        .then(|| chat_msg.client_message_id.trim().to_string()),
                    content: chat_msg.content.clone(),
                    message_type: if attachments.is_empty() {
                        ChatMessageType::Text
                    } else {
                        ChatMessageType::Attachment
                    },
                    reply_to_message_id,
                    metadata,
                    attachments,
                    mentions: proto_chat_mentions_to_core(
                        &chat_msg.mentions,
                        &self.public_id_codec,
                    )?,
                },
                control,
            )
            .await
            .map_err(|e| e.to_string())?;
        // Touch room activity to prevent TTL expiry on active rooms
        self.room_service.touch_room_activity(self.room_id).await;

        // Track chat message metric
        synctv_core::metrics::http::CHAT_MESSAGES_TOTAL
            .with_label_values(&[] as &[&str])
            .inc();

        if outcome.inserted {
            self.chat_event_dispatcher.dispatch(&outcome.event);
        }

        Ok(())
    }

    async fn ensure_guest_admission_for_action(&self) -> Result<(), String> {
        match self.guest_admission_denial_reason().await {
            Ok(Some(reason)) => Err(reason),
            Ok(None) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    /// Send heartbeat acknowledgment to client
    fn send_heartbeat_ack(&self) -> Result<(), String> {
        use synctv_proto::client::server_message::Message;
        use synctv_proto::client::HeartbeatAck;

        let msg = ServerMessage {
            message: Some(Message::HeartbeatAck(HeartbeatAck {
                timestamp: chrono::Utc::now().timestamp_millis(),
            })),
        };

        self.sender.send(msg)
    }

    /// Get room ID
    #[must_use]
    pub const fn get_room_id(&self) -> &RoomId {
        &self.room_id
    }

    /// Get user ID
    #[must_use]
    pub fn get_user_id(&self) -> UserId {
        self.user_id
    }
}

mod event_messages;
use event_messages::realtime_event_to_server_messages;

impl StreamMessageHandler {
    async fn take_initial_realtime_join_state(
        &self,
        room_id_str: &str,
    ) -> Result<InitialRealtimeJoinState, String> {
        if let Some(state) = self.pending_initial_join_state.lock().await.take() {
            return Ok(state);
        }

        if self.principal.is_guest() {
            return Ok(InitialRealtimeJoinState {
                member: None,
                room_settings: None,
            });
        }

        let member_lookup =
            probe_realtime_membership_access(&self.room_service, &self.room_id, &self.user_id)
                .await;

        let member = match member_lookup {
            Ok(RealtimeMembershipAccess::Allowed(member)) => member,
            Ok(RealtimeMembershipAccess::Denied(reason)) => {
                self.skip_cleanup_user_left
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                self.cleanup(room_id_str).await;
                return Err(reason);
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    room_id = %self.room_id,
                    user_id = %self.user_id,
                    "Failed to fetch membership during initial real-time join"
                );
                self.skip_cleanup_user_left
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                self.cleanup(room_id_str).await;
                return Err(error.to_string());
            }
        };

        let room_settings = self
            .room_service
            .get_room_settings(&self.room_id)
            .await
            .map_err(|error| {
                tracing::warn!(
                    error = %error,
                    room_id = %self.room_id,
                    user_id = %self.user_id,
                    "Failed to fetch room settings during initial real-time join"
                );
                error.to_string()
            });
        let room_settings = match room_settings {
            Ok(room_settings) => room_settings,
            Err(error) => {
                self.skip_cleanup_user_left
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                self.cleanup(room_id_str).await;
                return Err(error);
            }
        };

        Ok(InitialRealtimeJoinState {
            member: Some(member),
            room_settings: Some(room_settings),
        })
    }
}

fn parse_optional_chat_message_id(raw: &str) -> Result<Option<i64>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed
        .parse::<i64>()
        .map(Some)
        .map_err(|_| "Invalid chat message id".to_string())
}

fn proto_chat_mentions_to_core(
    mentions: &[synctv_proto::client::ChatMentionInput],
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<Vec<ChatMentionInput>, String> {
    mentions
        .iter()
        .map(|mention| {
            let user_id = public_id_codec
                .decode_user_id(&mention.user_id)
                .map_err(|error| format!("Invalid mention user_id: {error}"))?;
            Ok(ChatMentionInput {
                user_id,
                start: mention.start,
                length: mention.length,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
