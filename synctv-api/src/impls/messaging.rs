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
use prost::Message;
use synctv_core::{
    models::{RoomId, UserId, PermissionBits},
    service::{ContentFilter, RateLimitConfig, RateLimiter, RoomService},
};
use synctv_cluster::sync::{ClusterEvent, ClusterManager, ConnectionManager};

use crate::proto::client::{ClientMessage, ServerMessage};

/// Trait for sending server messages to clients
///
/// Implemented by both gRPC streaming and WebSocket transports
pub trait MessageSender: Send + Sync {
    /// Send a server message to the client
    fn send(&self, message: ServerMessage) -> Result<(), String>;
}

/// Unified IO abstraction for bidirectional messaging
///
/// This trait encapsulates both sending and receiving operations for real-time communication.
/// Implemented by both WebSocket and gRPC streaming transports, allowing complete code reuse.
///
/// The key insight is that WebSocket and gRPC streaming are conceptually identical:
/// - Both are bidirectional byte streams
/// - Both use proto encoding
/// - Both need the same business logic (rate limiting, permissions, broadcasting)
///
/// By implementing this trait, we ensure that ALL connection handling logic lives in impls/,
/// with the transport layer (http/, grpc/) providing only the IO implementation.
#[async_trait::async_trait]
pub trait StreamMessage: Send + Sync {
    /// Receive a client message (blocking/async)
    ///
    /// Returns None when the connection is closed
    async fn recv(&mut self) -> Option<Result<ClientMessage, String>>;

    /// Send a server message
    fn send(&self, message: ServerMessage) -> Result<(), String>;

    /// Check if connection is still alive
    fn is_alive(&self) -> bool;

    /// Send a ping to check connection liveness.
    /// Default implementation is a no-op (gRPC uses HTTP/2 PING automatically).
    fn ping(&self) -> Result<(), String> {
        Ok(())
    }
}

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
    user_id: UserId,
    username: String,
    connection_id: String,
    room_service: Arc<RoomService>,
    cluster_manager: Arc<ClusterManager>,
    connection_manager: ConnectionManager,
    rate_limiter: Arc<RateLimiter>,
    rate_limit_config: Arc<RateLimitConfig>,
    content_filter: Arc<ContentFilter>,
    sender: Arc<dyn MessageSender>,
}

impl Clone for StreamMessageHandler {
    fn clone(&self) -> Self {
        Self {
            room_id: self.room_id.clone(),
            user_id: self.user_id.clone(),
            username: self.username.clone(),
            connection_id: self.connection_id.clone(),
            room_service: Arc::clone(&self.room_service),
            cluster_manager: Arc::clone(&self.cluster_manager),
            connection_manager: self.connection_manager.clone(),
            rate_limiter: Arc::clone(&self.rate_limiter),
            rate_limit_config: Arc::clone(&self.rate_limit_config),
            content_filter: Arc::clone(&self.content_filter),
            sender: Arc::clone(&self.sender),
        }
    }
}

impl StreamMessageHandler {
    /// Create a new stream message handler
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        room_id: RoomId,
        user_id: UserId,
        username: String,
        room_service: Arc<RoomService>,
        cluster_manager: Arc<ClusterManager>,
        connection_manager: ConnectionManager,
        rate_limiter: Arc<RateLimiter>,
        rate_limit_config: Arc<RateLimitConfig>,
        content_filter: Arc<ContentFilter>,
        sender: Arc<dyn MessageSender>,
    ) -> Self {
        let connection_id = format!("{}_{}", user_id.as_str(), nanoid::nanoid!(8));
        Self {
            room_id,
            user_id,
            username,
            connection_id,
            room_service,
            cluster_manager,
            connection_manager,
            rate_limiter,
            rate_limit_config,
            content_filter,
            sender,
        }
    }

    /// Run the complete message loop using unified IO abstraction
    ///
    /// This is the NEW recommended method that handles both sending and receiving
    /// in a single unified loop using the `StreamMessage` trait.
    ///
    /// This method:
    /// 1. Subscribes to cluster events and forwards them to the client
    /// 2. Receives client messages via the `StreamMessage` trait
    /// 3. Handles rate limiting, content filtering, and permissions
    /// 4. Broadcasts events to the cluster
    /// 5. Monitors for disconnect signals (user ban, kick, etc.)
    /// 6. Handles cleanup on disconnect
    ///
    /// The caller only needs to provide a `StreamMessage` implementation (WebSocket or gRPC).
    pub async fn run<S: StreamMessage>(&self, stream: &mut S) -> Result<(), String> {
        let room_id_str = self.room_id.as_str().to_string();

        // Register connection with connection manager
        if let Err(e) = self.connection_manager.register(
            self.connection_id.clone(),
            self.user_id.clone(),
        ) {
            tracing::warn!("Failed to register connection: {}", e);
        }

        // Subscribe to cluster events
        let (mut event_rx, _connection_id) = self.cluster_manager.subscribe(
            self.room_id.clone(),
            self.user_id.clone()
        );

        // Subscribe to disconnect signals
        let mut disconnect_rx = self.connection_manager.subscribe_disconnect();

        // Subscribe to admin events (KickUser, etc.) for cross-replica disconnect propagation.
        // KickUser events arrive via Redis PubSub on the admin channel and are not
        // delivered through the room-level event subscription, so each connection
        // must independently monitor admin events and disconnect when targeted.
        let mut admin_rx = self.cluster_manager.subscribe_admin_events();

        // Send initial user joined notification
        stream.send(self.create_user_joined_message(&room_id_str).await)?;

        // Broadcast UserJoined event to other replicas
        self.broadcast_user_joined().await;

        // Create heartbeat interval OUTSIDE the loop so it doesn't reset
        // when other select! branches fire
        let mut heartbeat_interval = tokio::time::interval(std::time::Duration::from_secs(30));
        heartbeat_interval.tick().await; // Skip the immediate first tick

        // Main message loop using tokio::select! for concurrent operations
        loop {
            tokio::select! {
                // Incoming client message
                client_msg_result = stream.recv() => {
                    match client_msg_result {
                        Some(Ok(msg)) => {
                            if let Err(e) = self.handle_client_message(&msg).await {
                                tracing::error!("Failed to handle client message: {}", e);
                                // Don't break on individual message errors, continue processing
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

                // Cluster event (broadcast to client)
                event = event_rx.recv() => {
                    if let Some(event) = event {
                        if let Some(msg) = cluster_event_to_server_message(&event, &room_id_str) {
                            if let Err(e) = stream.send(msg) {
                                tracing::error!("Failed to send server message: {}", e);
                                break;
                            }
                        }
                    } else {
                        tracing::error!("Cluster event channel closed");
                        break;
                    }
                }

                // Disconnect signal (forced disconnect by server)
                signal = disconnect_rx.recv() => {
                    match signal {
                        Ok(synctv_cluster::sync::DisconnectSignal::Connection(conn_id)) => {
                            if conn_id == self.connection_id {
                                tracing::info!(
                                    connection_id = %self.connection_id,
                                    "Received disconnect signal for this connection"
                                );
                                break;
                            }
                        }
                        Ok(synctv_cluster::sync::DisconnectSignal::User(uid)) => {
                            if uid == self.user_id {
                                tracing::info!(
                                    user_id = %self.user_id.as_str(),
                                    "Received disconnect signal for this user (ban/kick)"
                                );
                                break;
                            }
                        }
                        Ok(synctv_cluster::sync::DisconnectSignal::Room(rid)) => {
                            if rid == self.room_id {
                                tracing::info!(
                                    room_id = %self.room_id.as_str(),
                                    "Received disconnect signal for this room"
                                );
                                break;
                            }
                        }
                        Ok(synctv_cluster::sync::DisconnectSignal::UserFromRoom { user_id: uid, room_id: rid }) => {
                            if uid == self.user_id && rid == self.room_id {
                                tracing::info!(
                                    user_id = %self.user_id.as_str(),
                                    room_id = %self.room_id.as_str(),
                                    "Received disconnect signal: kicked from room"
                                );
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            // Channel lagged, continue - we might have missed some signals
                            // but we'll still receive future ones
                            tracing::warn!("Disconnect signal channel lagged");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            tracing::error!("Disconnect signal channel closed");
                            break;
                        }
                    }
                }

                // Admin events from cluster (cross-replica kick/ban propagation)
                admin_event = admin_rx.recv() => {
                    match admin_event {
                        Ok(ClusterEvent::KickUser { ref user_id, ref reason, .. }) => {
                            if *user_id == self.user_id {
                                tracing::info!(
                                    user_id = %self.user_id.as_str(),
                                    reason = %reason,
                                    "Received cross-replica KickUser event, disconnecting"
                                );
                                break;
                            }
                        }
                        Ok(_) => {
                            // Other admin events (KickPublisher, etc.) not relevant to this connection
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            // Channel lagged: we may have missed critical KickUser events.
                            // Re-subscribe to get a fresh receiver so future events are not lost.
                            tracing::warn!(
                                lagged = n,
                                user_id = %self.user_id.as_str(),
                                "Admin event channel lagged, re-subscribing to avoid missed kicks"
                            );
                            admin_rx = self.cluster_manager.subscribe_admin_events();
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            tracing::error!("Admin event channel closed");
                            break;
                        }
                    }
                }

                // Heartbeat/health check every 30 seconds
                _ = heartbeat_interval.tick() => {
                    if !stream.is_alive() {
                        tracing::info!("Connection no longer alive");
                        break;
                    }
                    if let Err(e) = stream.ping() {
                        tracing::info!("Ping failed, connection dead: {}", e);
                        break;
                    }
                }
            }
        }

        // Cleanup: notify cluster that user left
        self.cleanup(&room_id_str).await;

        Ok(())
    }

    /// Create initial user joined message with actual role and permissions
    /// fetched from the room membership data.
    async fn create_user_joined_message(&self, room_id: &str) -> ServerMessage {
        use crate::proto::client::server_message::Message;
        use crate::proto::client::UserJoinedRoom;
        use synctv_proto::common::RoomMember;

        // Fetch the actual role and permissions from the membership record
        let (role_proto, permissions, added, removed, admin_added, admin_removed) =
            match self.room_service.member_service().get_member(&self.room_id, &self.user_id).await {
                Ok(Some(member)) => {
                    let effective = member.effective_permissions(member.role.permissions());
                    let role = match member.role {
                        synctv_core::models::RoomRole::Creator => synctv_proto::common::RoomMemberRole::Creator as i32,
                        synctv_core::models::RoomRole::Admin => synctv_proto::common::RoomMemberRole::Admin as i32,
                        synctv_core::models::RoomRole::Member => synctv_proto::common::RoomMemberRole::Member as i32,
                        synctv_core::models::RoomRole::Guest => synctv_proto::common::RoomMemberRole::Guest as i32,
                    };
                    (
                        role,
                        effective.0,
                        member.added_permissions,
                        member.removed_permissions,
                        member.admin_added_permissions,
                        member.admin_removed_permissions,
                    )
                }
                _ => {
                    // Fallback: if we can't fetch membership, use Member defaults
                    (
                        synctv_proto::common::RoomMemberRole::Member as i32,
                        synctv_core::models::PermissionBits::DEFAULT_MEMBER,
                        0, 0, 0, 0,
                    )
                }
            };

        ServerMessage {
            message: Some(Message::UserJoined(UserJoinedRoom {
                room_id: room_id.to_string(),
                member: Some(RoomMember {
                    room_id: room_id.to_string(),
                    user_id: self.user_id.as_str().to_string(),
                    username: self.username.clone(),
                    role: role_proto,
                    permissions,
                    added_permissions: added,
                    removed_permissions: removed,
                    admin_added_permissions: admin_added,
                    admin_removed_permissions: admin_removed,
                    joined_at: chrono::Utc::now().timestamp(),
                    is_online: true,
                }),
            })),
        }
    }

    /// Broadcast UserJoined event to cluster replicas
    async fn broadcast_user_joined(&self) {
        // Fetch the actual role and permissions from the membership record
        let (role_proto, permissions) =
            match self.room_service.member_service().get_member(&self.room_id, &self.user_id).await {
                Ok(Some(member)) => {
                    let effective = member.effective_permissions(member.role.permissions());
                    let role = match member.role {
                        synctv_core::models::RoomRole::Creator => synctv_proto::common::RoomMemberRole::Creator as i32,
                        synctv_core::models::RoomRole::Admin => synctv_proto::common::RoomMemberRole::Admin as i32,
                        synctv_core::models::RoomRole::Member => synctv_proto::common::RoomMemberRole::Member as i32,
                        synctv_core::models::RoomRole::Guest => synctv_proto::common::RoomMemberRole::Guest as i32,
                    };
                    (role, effective)
                }
                _ => {
                    // Fallback: if we can't fetch membership, use Member defaults
                    (
                        synctv_proto::common::RoomMemberRole::Member as i32,
                        synctv_core::models::PermissionBits(synctv_core::models::PermissionBits::DEFAULT_MEMBER),
                    )
                }
            };

        let event = ClusterEvent::UserJoined {
            event_id: nanoid::nanoid!(16),
            room_id: self.room_id.clone(),
            user_id: self.user_id.clone(),
            username: self.username.clone(),
            permissions,
            role: role_proto,
            timestamp: chrono::Utc::now(),
        };
        let _result = self.cluster_manager.broadcast(event);
    }

    /// Cleanup on disconnect
    async fn cleanup(&self, room_id: &str) {
        // Broadcast UserLeft BEFORE unregistering from the connection manager.
        // This order prevents state divergence: if the broadcast reaches subscribers
        // while this connection is still registered, they see a consistent view.
        // Previously, unregistering first could leave the hub with a stale subscriber
        // if the broadcast was delayed or had no receivers.
        let event = ClusterEvent::UserLeft {
            event_id: nanoid::nanoid!(16),
            room_id: self.room_id.clone(),
            user_id: self.user_id.clone(),
            username: self.username.clone(),
            timestamp: chrono::Utc::now(),
        };
        let result = self.cluster_manager.broadcast(event);

        if result.local_sent == 0 && !result.redis_sent {
            tracing::warn!(
                user = %self.username,
                room = %room_id,
                connection = %self.connection_id,
                "UserLeft broadcast reached no subscribers; hub may have stale state"
            );
        }

        // Now unregister from connection manager after broadcast has been sent
        self.connection_manager.unregister(&self.connection_id);

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
    /// 1. Subscribes to cluster events and forwards them to the client
    /// 2. Spawns a task to handle incoming client messages
    /// 3. Returns a sender that the caller should use to send `ClientMessages` to this handler
    ///
    /// Returns a sender that the caller should use to send `ClientMessages`
    #[must_use]
    pub fn start(
        &self,
    ) -> tokio::sync::mpsc::Sender<ClientMessage> {
        // Use bounded channel to prevent memory exhaustion from fast clients
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ClientMessage>(256);

        // Subscribe to cluster events and forward to client
        let room_id = self.room_id.clone();
        let user_id = self.user_id.clone();
        let room_id_str = room_id.as_str().to_string();
        let (mut rx_events, _connection_id) = self.cluster_manager.subscribe(room_id, user_id);
        let sender = self.sender.clone();

        tokio::spawn(async move {
            while let Some(event) = rx_events.recv().await {
                if let Some(msg) = cluster_event_to_server_message(&event, &room_id_str) {
                    if let Err(e) = sender.send(msg) {
                        tracing::error!("Failed to send message: {}", e);
                        break;
                    }
                }
            }
        });

        // Spawn task to handle incoming messages
        let handler = self.clone();
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if let Err(e) = handler.handle_client_message(&msg).await {
                    tracing::error!("Failed to handle client message: {}", e);
                }
            }
        });

        tx
    }

    /// Handle incoming client message with all validations
    pub async fn handle_client_message(&self, msg: &ClientMessage) -> Result<(), String> {
        use crate::proto::client::client_message::Message;

        match &msg.message {
            Some(Message::Chat(chat_msg)) => {
                // Validate message length
                if chat_msg.content.is_empty() {
                    return Err("Chat message cannot be empty".to_string());
                }
                if chat_msg.content.len() > 2000 {
                    return Err("Chat message too long (max 2000 characters)".to_string());
                }

                // Check if this is a danmaku message (has position)
                let is_danmaku = chat_msg.position.is_some();

                // Check rate limit
                let rate_limit_key = if is_danmaku {
                    format!("user:{}:danmaku", self.user_id.as_str())
                } else {
                    format!("user:{}:chat", self.user_id.as_str())
                };

                let rate_limit = if is_danmaku {
                    self.rate_limit_config.danmaku_per_second
                } else {
                    self.rate_limit_config.chat_per_second
                };

                self.rate_limiter
                    .check_rate_limit(
                        &rate_limit_key,
                        rate_limit,
                        self.rate_limit_config.window_seconds,
                    )
                    .await
                    .map_err(|e| e.to_string())?;

                // Filter and sanitize content
                let sanitized_content = if is_danmaku {
                    self.content_filter
                        .filter_danmaku(&chat_msg.content)
                        .map_err(|e| e.to_string())?
                } else {
                    self.content_filter
                        .filter_chat(&chat_msg.content)
                        .map_err(|e| e.to_string())?
                };

                // Check permission (same permission for all chat messages)
                self.room_service
                    .check_permission(&self.room_id, &self.user_id, PermissionBits::SEND_CHAT)
                    .await
                    .map_err(|e| e.to_string())?;

                // Handle message
                if is_danmaku {
                    self.handle_danmaku(
                        &sanitized_content,
                        chat_msg.position.unwrap_or(0.0),
                        chat_msg.color.clone(),
                    ).await?;
                } else {
                    self.handle_chat_message(&sanitized_content).await?;
                }
            }
            Some(Message::Heartbeat(_)) => {
                // Respond with HeartbeatAck to let client know server is alive
                // This completes the heartbeat request-response cycle
                self.send_heartbeat_ack()?;
            }
            Some(Message::WebrtcOffer(offer)) => {
                self.handle_webrtc_offer(offer).await?;
            }
            Some(Message::WebrtcAnswer(answer)) => {
                self.handle_webrtc_answer(answer).await?;
            }
            Some(Message::WebrtcIceCandidate(candidate)) => {
                self.handle_webrtc_ice_candidate(candidate).await?;
            }
            Some(Message::WebrtcJoin(join)) => {
                self.handle_webrtc_join(join).await?;
            }
            Some(Message::WebrtcLeave(leave)) => {
                self.handle_webrtc_leave(leave).await?;
            }
            None => {
                return Err("Empty message".to_string());
            }
        }

        Ok(())
    }

    async fn handle_chat_message(&self, content: &str) -> Result<(), String> {
        // Save to database
        let _saved_msg = self
            .room_service
            .save_chat_message(
                self.room_id.clone(),
                self.user_id.clone(),
                content.to_string(),
            )
            .await
            .map_err(|e| e.to_string())?;

        // Track chat message metric
        synctv_core::metrics::http::CHAT_MESSAGES_TOTAL
            .with_label_values(&[] as &[&str])
            .inc();

        let event = ClusterEvent::ChatMessage {
            event_id: nanoid::nanoid!(16),
            room_id: self.room_id.clone(),
            user_id: self.user_id.clone(),
            username: self.username.clone(),
            message: content.to_string(),
            timestamp: chrono::Utc::now(),
            position: None,
            color: None,
        };

        // Broadcast to cluster (handles both local and Redis)
        let _result = self.cluster_manager.broadcast(event);

        Ok(())
    }

    /// Handle danmaku (bullet comment) messages.
    ///
    /// Danmaku are intentionally ephemeral and NOT persisted to the database.
    /// Unlike regular chat messages, danmaku are time-anchored video overlays
    /// that only make sense in the context of the current playback session.
    /// They are broadcast to all connected clients for real-time display but
    /// are not saved for later retrieval. This is consistent with how major
    /// danmaku platforms (Bilibili, Niconico) treat live/real-time danmaku.
    async fn handle_danmaku(&self, content: &str, position: f64, color: Option<String>) -> Result<(), String> {
        let event = ClusterEvent::ChatMessage {
            event_id: nanoid::nanoid!(16),
            room_id: self.room_id.clone(),
            user_id: self.user_id.clone(),
            username: self.username.clone(),
            message: content.to_string(),
            timestamp: chrono::Utc::now(),
            position: Some(position),
            color,
        };

        // Broadcast to cluster (handles both local and Redis)
        let _result = self.cluster_manager.broadcast(event);

        Ok(())
    }

    // ==================== WebRTC Message Handlers ====================

    async fn handle_webrtc_offer(&self, offer: &crate::proto::client::WebRtcOffer) -> Result<(), String> {
        // Check permission
        self.room_service
            .check_permission(&self.room_id, &self.user_id, PermissionBits::USE_WEBRTC)
            .await
            .map_err(|e| format!("WebRTC permission denied: {e}"))?;

        // Get connection ID from ConnectionManager
        let conn_id = self.connection_manager
            .get_connection_id(&self.room_id, &self.user_id)
            .ok_or_else(|| "Connection not found".to_string())?;

        // Create event with server-set 'from' field (防止伪造)
        let event = ClusterEvent::WebRTCSignaling {
            event_id: nanoid::nanoid!(16),
            room_id: self.room_id.clone(),
            message_type: "offer".to_string(),
            from: format!("{}:{}", self.user_id.as_str(), conn_id),
            to: offer.to.clone(),
            data: offer.data.clone(),
            timestamp: chrono::Utc::now(),
        };

        // Broadcast to cluster
        let _result = self.cluster_manager.broadcast(event);

        Ok(())
    }

    async fn handle_webrtc_answer(&self, answer: &crate::proto::client::WebRtcAnswer) -> Result<(), String> {
        // Check permission
        self.room_service
            .check_permission(&self.room_id, &self.user_id, PermissionBits::USE_WEBRTC)
            .await
            .map_err(|e| format!("WebRTC permission denied: {e}"))?;

        // Get connection ID
        let conn_id = self.connection_manager
            .get_connection_id(&self.room_id, &self.user_id)
            .ok_or_else(|| "Connection not found".to_string())?;

        // Create event with server-set 'from' field
        let event = ClusterEvent::WebRTCSignaling {
            event_id: nanoid::nanoid!(16),
            room_id: self.room_id.clone(),
            message_type: "answer".to_string(),
            from: format!("{}:{}", self.user_id.as_str(), conn_id),
            to: answer.to.clone(),
            data: answer.data.clone(),
            timestamp: chrono::Utc::now(),
        };

        // Broadcast to cluster
        let _result = self.cluster_manager.broadcast(event);

        Ok(())
    }

    async fn handle_webrtc_ice_candidate(&self, candidate: &crate::proto::client::WebRtcIceCandidate) -> Result<(), String> {
        // Check permission
        self.room_service
            .check_permission(&self.room_id, &self.user_id, PermissionBits::USE_WEBRTC)
            .await
            .map_err(|e| format!("WebRTC permission denied: {e}"))?;

        // Get connection ID
        let conn_id = self.connection_manager
            .get_connection_id(&self.room_id, &self.user_id)
            .ok_or_else(|| "Connection not found".to_string())?;

        // Create event with server-set 'from' field
        let event = ClusterEvent::WebRTCSignaling {
            event_id: nanoid::nanoid!(16),
            room_id: self.room_id.clone(),
            message_type: "ice_candidate".to_string(),
            from: format!("{}:{}", self.user_id.as_str(), conn_id),
            to: candidate.to.clone(),
            data: candidate.data.clone(),
            timestamp: chrono::Utc::now(),
        };

        // Broadcast to cluster
        let _result = self.cluster_manager.broadcast(event);

        Ok(())
    }

    async fn handle_webrtc_join(&self, _join: &crate::proto::client::WebRtcJoin) -> Result<(), String> {
        // Check permission
        self.room_service
            .check_permission(&self.room_id, &self.user_id, PermissionBits::USE_WEBRTC)
            .await
            .map_err(|e| format!("WebRTC permission denied: {e}"))?;

        // Get connection ID
        let conn_id = self.connection_manager
            .get_connection_id(&self.room_id, &self.user_id)
            .ok_or_else(|| "Connection not found".to_string())?;

        // Mark this connection as joined WebRTC session
        self.connection_manager
            .mark_rtc_joined(&self.room_id, &self.user_id, &conn_id, true);

        // Track WebRTC peer metrics
        synctv_core::metrics::http::WEBRTC_PEERS_ACTIVE.inc();

        // Broadcast Join event to all RTC-joined users in the room
        let event = ClusterEvent::WebRTCJoin {
            event_id: nanoid::nanoid!(16),
            room_id: self.room_id.clone(),
            user_id: self.user_id.clone(),
            conn_id,
            username: self.username.clone(),
            timestamp: chrono::Utc::now(),
        };

        // Broadcast to cluster
        let _result = self.cluster_manager.broadcast(event);

        Ok(())
    }

    async fn handle_webrtc_leave(&self, _leave: &crate::proto::client::WebRtcLeave) -> Result<(), String> {
        // Get connection ID
        let conn_id = self.connection_manager
            .get_connection_id(&self.room_id, &self.user_id)
            .ok_or_else(|| "Connection not found".to_string())?;

        // Mark this connection as left WebRTC session
        self.connection_manager
            .mark_rtc_joined(&self.room_id, &self.user_id, &conn_id, false);

        // Track WebRTC peer metrics
        synctv_core::metrics::http::WEBRTC_PEERS_ACTIVE.dec();

        // Broadcast Leave event to all RTC-joined users in the room
        let event = ClusterEvent::WebRTCLeave {
            event_id: nanoid::nanoid!(16),
            room_id: self.room_id.clone(),
            user_id: self.user_id.clone(),
            conn_id,
            timestamp: chrono::Utc::now(),
        };

        // Broadcast to cluster
        let _result = self.cluster_manager.broadcast(event);

        Ok(())
    }

    /// Send heartbeat acknowledgment to client
    fn send_heartbeat_ack(&self) -> Result<(), String> {
        use crate::proto::client::server_message::Message;
        use crate::proto::client::HeartbeatAck;

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
        self.user_id.clone()
    }
}

/// Convert cluster event to server message
fn cluster_event_to_server_message(
    event: &synctv_cluster::sync::ClusterEvent,
    room_id: &str,
) -> Option<ServerMessage> {
    use crate::proto::client::server_message::Message;
    use crate::proto::client::{ServerMessage, ChatMessageReceive, PlaybackStateChanged, PlaybackState, UserJoinedRoom, UserLeftRoom, RoomSettingsChanged, ErrorMessage};
    use synctv_proto::common::RoomMember;
    use synctv_cluster::sync::ClusterEvent;

    match event {
        ClusterEvent::ChatMessage { user_id, username, message, timestamp, position, color, .. } => {
            Some(ServerMessage {
                message: Some(Message::Chat(ChatMessageReceive {
                    id: nanoid::nanoid!(12),
                    room_id: room_id.to_string(),
                    user_id: user_id.as_str().to_string(),
                    username: username.clone(),
                    content: message.clone(),
                    timestamp: timestamp.timestamp_micros(),
                    position: *position,
                    color: color.clone(),
                })),
            })
        }
        ClusterEvent::PlaybackStateChanged { state, .. } => {
            Some(ServerMessage {
                message: Some(Message::PlaybackState(PlaybackStateChanged {
                    room_id: room_id.to_string(),
                    state: Some(PlaybackState {
                        room_id: state.room_id.as_str().to_string(),
                        playing_media_id: state
                            .playing_media_id
                            .as_ref()
                            .map(|id| id.as_str().to_string())
                            .unwrap_or_default(),
                        current_time: state.current_time,
                        speed: state.speed,
                        is_playing: state.is_playing,
                        updated_at: state.updated_at.timestamp(),
                        version: state.version as i32,
                        playing_playlist_id: state
                            .playing_playlist_id
                            .as_ref()
                            .map(|id| id.as_str().to_string())
                            .unwrap_or_default(),
                        relative_path: state.relative_path.clone(),
                    }),
                })),
            })
        }
        ClusterEvent::UserJoined { user_id, username, permissions, role, .. } => {
            Some(ServerMessage {
                message: Some(Message::UserJoined(UserJoinedRoom {
                    room_id: room_id.to_string(),
                    member: Some(RoomMember {
                        room_id: room_id.to_string(),
                        user_id: user_id.as_str().to_string(),
                        username: username.clone(),
                        role: *role,
                        permissions: permissions.0,
                        added_permissions: 0,
                        removed_permissions: 0,
                        admin_added_permissions: 0,
                        admin_removed_permissions: 0,
                        joined_at: chrono::Utc::now().timestamp(),
                        is_online: true,
                    }),
                })),
            })
        }
        ClusterEvent::UserLeft { user_id, .. } => {
            Some(ServerMessage {
                message: Some(Message::UserLeft(UserLeftRoom {
                    room_id: room_id.to_string(),
                    user_id: user_id.as_str().to_string(),
                })),
            })
        }
        ClusterEvent::MediaAdded { media_id, media_title, user_id, username, .. } => {
            Some(ServerMessage {
                message: Some(Message::MediaAdded(crate::proto::client::MediaAdded {
                    room_id: room_id.to_string(),
                    media_id: media_id.as_str().to_string(),
                    title: media_title.clone(),
                    added_by: username.clone(),
                    added_by_user_id: user_id.as_str().to_string(),
                })),
            })
        }
        ClusterEvent::MediaRemoved { media_id, user_id, username, .. } => {
            Some(ServerMessage {
                message: Some(Message::MediaRemoved(crate::proto::client::MediaRemoved {
                    room_id: room_id.to_string(),
                    media_id: media_id.as_str().to_string(),
                    removed_by: username.clone(),
                    removed_by_user_id: user_id.as_str().to_string(),
                })),
            })
        }
        ClusterEvent::PermissionChanged { target_user_id, new_permissions, role, added_permissions, removed_permissions, changed_by_username, .. } => {
            Some(ServerMessage {
                message: Some(Message::PermissionChanged(crate::proto::client::PermissionChanged {
                    room_id: room_id.to_string(),
                    user_id: target_user_id.as_str().to_string(),
                    role: *role,
                    effective_permissions: new_permissions.0,
                    added_permissions: added_permissions.0,
                    removed_permissions: removed_permissions.0,
                    admin_added_permissions: 0,
                    admin_removed_permissions: 0,
                    updated_by: changed_by_username.clone(),
                })),
            })
        }
        ClusterEvent::RoomSettingsChanged { settings_json, .. } => {
            Some(ServerMessage {
                message: Some(Message::RoomSettings(RoomSettingsChanged {
                    room_id: room_id.to_string(),
                    settings: settings_json.clone(),
                })),
            })
        }
        ClusterEvent::WebRTCSignaling { message_type, from, to, data, .. } => {
            // Convert to appropriate proto message based on message_type
            match message_type.as_str() {
                "offer" => Some(ServerMessage {
                    message: Some(Message::WebrtcOffer(crate::proto::client::WebRtcOffer {
                        from: from.clone(),
                        to: to.clone(),
                        data: data.clone(),
                    })),
                }),
                "answer" => Some(ServerMessage {
                    message: Some(Message::WebrtcAnswer(crate::proto::client::WebRtcAnswer {
                        from: from.clone(),
                        to: to.clone(),
                        data: data.clone(),
                    })),
                }),
                "ice_candidate" => Some(ServerMessage {
                    message: Some(Message::WebrtcIceCandidate(crate::proto::client::WebRtcIceCandidate {
                        from: from.clone(),
                        to: to.clone(),
                        data: data.clone(),
                    })),
                }),
                _ => {
                    tracing::warn!("Unknown WebRTC message type: {}", message_type);
                    None
                }
            }
        }
        ClusterEvent::WebRTCJoin { user_id, conn_id, username, .. } => {
            Some(ServerMessage {
                message: Some(Message::WebrtcJoin(crate::proto::client::WebRtcJoin {
                    user_id: user_id.as_str().to_string(),
                    conn_id: conn_id.clone(),
                    username: username.clone(),
                })),
            })
        }
        ClusterEvent::WebRTCLeave { user_id, conn_id, .. } => {
            Some(ServerMessage {
                message: Some(Message::WebrtcLeave(crate::proto::client::WebRtcLeave {
                    user_id: user_id.as_str().to_string(),
                    conn_id: conn_id.clone(),
                })),
            })
        }
        ClusterEvent::SystemNotification { message, .. } => {
            Some(ServerMessage {
                message: Some(Message::Error(ErrorMessage {
                    message: message.clone(),
                    code: 0, // System notifications use code 0 (not actual errors)
                    detail: String::new(),
                })),
            })
        }
        ClusterEvent::RoomDeleted { .. } => {
            // Notify WebSocket clients that the room has been deleted
            Some(ServerMessage {
                message: Some(Message::Error(ErrorMessage {
                    message: "Room has been deleted".to_string(),
                    code: crate::impls::error_codes::NOT_FOUND,
                    detail: String::new(),
                })),
            })
        }
        ClusterEvent::KickPublisher { .. } | ClusterEvent::KickUser { .. }
        | ClusterEvent::RoomCreated { .. } | ClusterEvent::CacheInvalidate { .. } => {
            // Admin/internal events are handled by other channels,
            // not forwarded to WebSocket clients
            None
        }
    }
}

/// Binary codec for proto messages
pub struct ProtoCodec;

impl ProtoCodec {
    /// Encode `ClientMessage` to binary
    pub fn encode_client_message(msg: &ClientMessage) -> Result<Vec<u8>, String> {
        Ok(msg.encode_to_vec())
    }

    /// Decode `ClientMessage` from binary
    pub fn decode_client_message(data: &[u8]) -> Result<ClientMessage, String> {
        ClientMessage::decode(data)
            .map_err(|e| format!("Failed to decode message: {e}"))
    }

    /// Encode `ServerMessage` to binary
    pub fn encode_server_message(msg: &ServerMessage) -> Result<Vec<u8>, String> {
        Ok(msg.encode_to_vec())
    }

    /// Decode `ServerMessage` from binary
    pub fn decode_server_message(data: &[u8]) -> Result<ServerMessage, String> {
        ServerMessage::decode(data)
            .map_err(|e| format!("Failed to decode message: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use synctv_cluster::sync::{ClusterEvent, NotificationLevel};
    use synctv_core::models::{RoomId, UserId, MediaId, PermissionBits, RoomPlaybackState};
    use crate::proto::client::server_message::Message;

    fn room_id() -> RoomId { RoomId("room_test".to_string()) }
    fn user_id() -> UserId { UserId("user_test".to_string()) }
    fn media_id() -> MediaId { MediaId::from_string("media_test".to_string()) }
    fn now() -> chrono::DateTime<chrono::Utc> { chrono::Utc::now() }

    // ========== cluster_event_to_server_message Tests ==========

    #[test]
    fn test_chat_message_event_conversion() {
        let event = ClusterEvent::ChatMessage {
            event_id: "evt1".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "alice".to_string(),
            message: "hello world".to_string(),
            timestamp: now(),
            position: Some(42.5),
            color: Some("#ff0000".to_string()),
        };

        let msg = cluster_event_to_server_message(&event, "room_test");
        assert!(msg.is_some());
        let msg = msg.unwrap();
        match msg.message {
            Some(Message::Chat(chat)) => {
                assert_eq!(chat.room_id, "room_test");
                assert_eq!(chat.user_id, "user_test");
                assert_eq!(chat.username, "alice");
                assert_eq!(chat.content, "hello world");
                assert_eq!(chat.position, Some(42.5));
                assert_eq!(chat.color, Some("#ff0000".to_string()));
            }
            other => panic!("Expected Chat message, got: {other:?}"),
        }
    }

    #[test]
    fn test_playback_state_changed_event_conversion() {
        let state = RoomPlaybackState {
            room_id: room_id(),
            playing_media_id: Some(media_id()),
            playing_playlist_id: None,
            relative_path: String::new(),
            current_time: 123.456,
            speed: 1.5,
            is_playing: true,
            updated_at: now(),
            version: 7,
        };
        let event = ClusterEvent::PlaybackStateChanged {
            event_id: "evt2".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "bob".to_string(),
            state: state.clone(),
            timestamp: now(),
        };

        let msg = cluster_event_to_server_message(&event, "room_test").unwrap();
        match msg.message {
            Some(Message::PlaybackState(ps)) => {
                assert_eq!(ps.room_id, "room_test");
                let s = ps.state.unwrap();
                assert_eq!(s.current_time, 123.456);
                assert_eq!(s.speed, 1.5);
                assert!(s.is_playing);
                assert_eq!(s.playing_media_id, "media_test");
                assert_eq!(s.version, 7);
            }
            other => panic!("Expected PlaybackState, got: {other:?}"),
        }
    }

    #[test]
    fn test_user_joined_event_conversion() {
        let event = ClusterEvent::UserJoined {
            event_id: "evt3".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "carol".to_string(),
            permissions: PermissionBits(PermissionBits::DEFAULT_MEMBER),
            role: 3,
            timestamp: now(),
        };

        let msg = cluster_event_to_server_message(&event, "room_test").unwrap();
        match msg.message {
            Some(Message::UserJoined(uj)) => {
                assert_eq!(uj.room_id, "room_test");
                let member = uj.member.unwrap();
                assert_eq!(member.user_id, "user_test");
                assert_eq!(member.username, "carol");
                assert_eq!(member.role, 3);
                assert!(member.is_online);
            }
            other => panic!("Expected UserJoined, got: {other:?}"),
        }
    }

    #[test]
    fn test_user_left_event_conversion() {
        let event = ClusterEvent::UserLeft {
            event_id: "evt4".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "dave".to_string(),
            timestamp: now(),
        };

        let msg = cluster_event_to_server_message(&event, "room_test").unwrap();
        match msg.message {
            Some(Message::UserLeft(ul)) => {
                assert_eq!(ul.room_id, "room_test");
                assert_eq!(ul.user_id, "user_test");
            }
            other => panic!("Expected UserLeft, got: {other:?}"),
        }
    }

    #[test]
    fn test_media_added_event_conversion() {
        let event = ClusterEvent::MediaAdded {
            event_id: "evt5".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "eve".to_string(),
            media_id: media_id(),
            media_title: "Test Video".to_string(),
            timestamp: now(),
        };

        let msg = cluster_event_to_server_message(&event, "room_test").unwrap();
        match msg.message {
            Some(Message::MediaAdded(ma)) => {
                assert_eq!(ma.room_id, "room_test");
                assert_eq!(ma.media_id, "media_test");
                assert_eq!(ma.title, "Test Video");
                assert_eq!(ma.added_by, "eve");
            }
            other => panic!("Expected MediaAdded, got: {other:?}"),
        }
    }

    #[test]
    fn test_media_removed_event_conversion() {
        let event = ClusterEvent::MediaRemoved {
            event_id: "evt6".to_string(),
            room_id: room_id(),
            user_id: user_id(),
            username: "frank".to_string(),
            media_id: media_id(),
            timestamp: now(),
        };

        let msg = cluster_event_to_server_message(&event, "room_test").unwrap();
        match msg.message {
            Some(Message::MediaRemoved(mr)) => {
                assert_eq!(mr.room_id, "room_test");
                assert_eq!(mr.media_id, "media_test");
                assert_eq!(mr.removed_by, "frank");
            }
            other => panic!("Expected MediaRemoved, got: {other:?}"),
        }
    }

    #[test]
    fn test_webrtc_offer_event_conversion() {
        let event = ClusterEvent::WebRTCSignaling {
            event_id: "evt7".to_string(),
            room_id: room_id(),
            message_type: "offer".to_string(),
            from: "conn_a".to_string(),
            to: "conn_b".to_string(),
            data: "sdp_data".to_string(),
            timestamp: now(),
        };

        let msg = cluster_event_to_server_message(&event, "room_test").unwrap();
        match msg.message {
            Some(Message::WebrtcOffer(o)) => {
                assert_eq!(o.from, "conn_a");
                assert_eq!(o.to, "conn_b");
                assert_eq!(o.data, "sdp_data");
            }
            other => panic!("Expected WebrtcOffer, got: {other:?}"),
        }
    }

    #[test]
    fn test_webrtc_answer_event_conversion() {
        let event = ClusterEvent::WebRTCSignaling {
            event_id: "evt8".to_string(),
            room_id: room_id(),
            message_type: "answer".to_string(),
            from: "conn_b".to_string(),
            to: "conn_a".to_string(),
            data: "answer_sdp".to_string(),
            timestamp: now(),
        };

        let msg = cluster_event_to_server_message(&event, "room_test").unwrap();
        match msg.message {
            Some(Message::WebrtcAnswer(a)) => {
                assert_eq!(a.from, "conn_b");
                assert_eq!(a.to, "conn_a");
            }
            other => panic!("Expected WebrtcAnswer, got: {other:?}"),
        }
    }

    #[test]
    fn test_webrtc_ice_candidate_event_conversion() {
        let event = ClusterEvent::WebRTCSignaling {
            event_id: "evt9".to_string(),
            room_id: room_id(),
            message_type: "ice_candidate".to_string(),
            from: "conn_a".to_string(),
            to: "conn_b".to_string(),
            data: "candidate_data".to_string(),
            timestamp: now(),
        };

        let msg = cluster_event_to_server_message(&event, "room_test").unwrap();
        assert!(matches!(msg.message, Some(Message::WebrtcIceCandidate(_))));
    }

    #[test]
    fn test_webrtc_unknown_type_returns_none() {
        let event = ClusterEvent::WebRTCSignaling {
            event_id: "evt10".to_string(),
            room_id: room_id(),
            message_type: "unknown_type".to_string(),
            from: "conn_a".to_string(),
            to: "conn_b".to_string(),
            data: "data".to_string(),
            timestamp: now(),
        };

        let msg = cluster_event_to_server_message(&event, "room_test");
        assert!(msg.is_none());
    }

    #[test]
    fn test_room_deleted_event_conversion() {
        let event = ClusterEvent::RoomDeleted {
            event_id: "evt11".to_string(),
            room_id: room_id(),
            deleted_by: user_id(),
            timestamp: now(),
        };

        let msg = cluster_event_to_server_message(&event, "room_test").unwrap();
        match msg.message {
            Some(Message::Error(e)) => {
                assert!(e.message.contains("deleted"));
                assert_eq!(e.code, crate::impls::error_codes::NOT_FOUND);
            }
            other => panic!("Expected Error message for RoomDeleted, got: {other:?}"),
        }
    }

    #[test]
    fn test_system_notification_event_conversion() {
        let event = ClusterEvent::SystemNotification {
            event_id: "evt12".to_string(),
            message: "Server maintenance in 5 minutes".to_string(),
            level: NotificationLevel::Warning,
            timestamp: now(),
        };

        let msg = cluster_event_to_server_message(&event, "room_test").unwrap();
        match msg.message {
            Some(Message::Error(e)) => {
                assert_eq!(e.message, "Server maintenance in 5 minutes");
                assert_eq!(e.code, 0);
            }
            other => panic!("Expected Error message for SystemNotification, got: {other:?}"),
        }
    }

    #[test]
    fn test_admin_events_return_none() {
        let event = ClusterEvent::KickPublisher {
            event_id: "evt13".to_string(),
            room_id: room_id(),
            media_id: media_id(),
            reason: "test".to_string(),
            timestamp: now(),
        };
        assert!(cluster_event_to_server_message(&event, "room_test").is_none());

        let event = ClusterEvent::KickUser {
            event_id: "evt14".to_string(),
            user_id: user_id(),
            reason: "banned".to_string(),
            timestamp: now(),
        };
        assert!(cluster_event_to_server_message(&event, "room_test").is_none());
    }

    // ========== ProtoCodec Tests ==========

    #[test]
    fn test_server_message_encode_decode_roundtrip() {
        let msg = ServerMessage {
            message: Some(Message::UserLeft(crate::proto::client::UserLeftRoom {
                room_id: "room1".to_string(),
                user_id: "user1".to_string(),
            })),
        };

        let encoded = ProtoCodec::encode_server_message(&msg).unwrap();
        let decoded = ProtoCodec::decode_server_message(&encoded).unwrap();
        match decoded.message {
            Some(Message::UserLeft(ul)) => {
                assert_eq!(ul.room_id, "room1");
                assert_eq!(ul.user_id, "user1");
            }
            other => panic!("Expected UserLeft after roundtrip, got: {other:?}"),
        }
    }

    #[test]
    fn test_client_message_decode_invalid_data() {
        let result = ProtoCodec::decode_client_message(&[0xFF, 0xFF, 0xFF]);
        assert!(result.is_err());
    }

    #[test]
    fn test_server_message_decode_invalid_data() {
        let result = ProtoCodec::decode_server_message(&[0xFF, 0xFF, 0xFF]);
        assert!(result.is_err());
    }

    #[test]
    fn test_server_message_encode_empty() {
        let msg = ServerMessage { message: None };
        let encoded = ProtoCodec::encode_server_message(&msg).unwrap();
        let decoded = ProtoCodec::decode_server_message(&encoded).unwrap();
        assert!(decoded.message.is_none());
    }
}
