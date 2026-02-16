//! Cache invalidation service for multi-replica deployments
//!
//! Uses Redis Pub/Sub to broadcast cache invalidation messages across all nodes.
//! When one node invalidates a cache entry, all other nodes receive the message
//! and invalidate their local L1 caches accordingly.

use redis::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use crate::models::RoomId;
use crate::{Error, Result};

/// Cache invalidation message types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InvalidationMessage {
    /// Invalidate permission cache for a specific user in a room
    UserPermission {
        room_id: String,
        user_id: String,
    },
    /// Invalidate permission cache for all users in a room
    RoomPermission {
        room_id: String,
    },
    /// Invalidate user cache
    User {
        user_id: String,
    },
    /// Invalidate username cache
    Username {
        user_id: String,
    },
    /// Invalidate room cache
    Room {
        room_id: String,
    },
    /// Invalidate playback state cache for a room
    PlaybackState {
        room_id: String,
    },
    /// Update bloom filter: mark keys as existing on all replicas
    BloomFilterUpdate {
        keys: Vec<String>,
    },
    /// Invalidate room settings cache for a specific room
    RoomSettings {
        room_id: String,
    },
    /// Invalidate all caches
    All,
}

/// Service for broadcasting and receiving cache invalidation messages
///
/// Uses Redis Streams instead of Pub/Sub for reliable message delivery:
/// - Messages are persisted and won't be lost
/// - Each node uses its own consumer group for broadcast semantics
/// - Automatic message acknowledgment and retry on failure
pub struct CacheInvalidationService {
    /// Redis client for streams
    redis_client: Option<Client>,
    /// Local broadcast sender for invalidation events
    local_sender: broadcast::Sender<InvalidationMessage>,
    /// Node identifier for logging and consumer group
    node_id: String,
    /// Redis stream key for cache invalidation
    stream_key: String,
    /// Consumer group name
    consumer_group: String,
    /// Shutdown flag
    shutdown: Arc<std::sync::atomic::AtomicBool>,
}

impl Clone for CacheInvalidationService {
    fn clone(&self) -> Self {
        Self {
            redis_client: self.redis_client.clone(),
            local_sender: self.local_sender.clone(),
            node_id: self.node_id.clone(),
            stream_key: self.stream_key.clone(),
            consumer_group: self.consumer_group.clone(),
            shutdown: self.shutdown.clone(),
        }
    }
}

impl CacheInvalidationService {
    /// Create a new cache invalidation service
    ///
    /// # Arguments
    /// * `redis_client` - Optional Redis client. If None, only local invalidation is used.
    /// * `node_id` - Unique identifier for this node (for consumer group and logging)
    /// * `stream_key` - Redis stream key for cache invalidation (e.g., "synctv:cache:invalidate:stream")
    #[must_use]
    pub fn new(redis_client: Option<Client>, node_id: String, stream_key: String) -> Self {
        let (local_sender, _) = broadcast::channel(1024);
        let consumer_group = format!("cache-invalidation-{node_id}");

        Self {
            redis_client,
            local_sender,
            node_id,
            stream_key,
            consumer_group,
            shutdown: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Start listening for cache invalidation messages from Redis
    ///
    /// This spawns a background task that reads from the invalidation stream.
    /// When a message is received, it's broadcast locally to all cache instances.
    pub async fn start(&self) -> Result<()> {
        let Some(client) = self.redis_client.clone() else {
            info!("Redis not configured, cache invalidation is local-only");
            return Ok(());
        };

        // Create consumer group if it doesn't exist
        if let Err(e) = self.create_consumer_group(&client).await {
            warn!(
                error = %e,
                "Failed to create consumer group (may already exist)"
            );
        }

        let local_sender = self.local_sender.clone();
        let node_id = self.node_id.clone();
        let stream_key = self.stream_key.clone();
        let consumer_group = self.consumer_group.clone();
        let shutdown = self.shutdown.clone();

        crate::spawn::spawn_monitored("cache_invalidation_subscriber", async move {
            let mut backoff_secs: u64 = 5;
            const MAX_BACKOFF_SECS: u64 = 60;

            loop {
                if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                    debug!("Cache invalidation listener shutting down");
                    break;
                }

                match Self::run_subscriber(&client, &local_sender, &node_id, &stream_key, &consumer_group, shutdown.clone()).await {
                    Ok(()) => {
                        // Normal shutdown
                        break;
                    }
                    Err(e) => {
                        error!(
                            error = %e,
                            backoff_seconds = backoff_secs,
                            "Cache invalidation subscriber error, reconnecting..."
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
                        // Exponential backoff: 5 -> 10 -> 20 -> 40 -> 60 (capped)
                        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
                    }
                }
            }
            info!("Cache invalidation listener stopped");
        });

        Ok(())
    }

    /// Create the consumer group for the invalidation stream
    async fn create_consumer_group(&self, client: &Client) -> Result<()> {

        let mut conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| Error::Internal(format!("Failed to get Redis connection: {e}")))?;

        // XGROUP CREATE <stream> <group> <id> MKSTREAM
        // Use "0" to read from the beginning, "$" to read only new messages
        let _: () = redis::cmd("XGROUP")
            .arg("CREATE")
            .arg(&self.stream_key)
            .arg(&self.consumer_group)
            .arg("$") // Only read new messages (not historical)
            .arg("MKSTREAM") // Create stream if it doesn't exist
            .query_async(&mut conn)
            .await
            .map_err(|e| Error::Internal(format!("Failed to create consumer group: {e}")))?;

        info!(
            stream = %self.stream_key,
            group = %self.consumer_group,
            "Created consumer group for cache invalidation"
        );

        Ok(())
    }

    /// Run the Redis subscriber loop using Streams
    async fn run_subscriber(
        client: &Client,
        local_sender: &broadcast::Sender<InvalidationMessage>,
        node_id: &str,
        stream_key: &str,
        consumer_group: &str,
        shutdown: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<()> {

        // Get async connection
        let mut conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| Error::Internal(format!("Failed to get Redis connection: {e}")))?;

        info!(
            node_id = %node_id,
            stream = %stream_key,
            group = %consumer_group,
            "Started cache invalidation stream consumer"
        );

        loop {
            if shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }

            // XREADGROUP GROUP <group> <consumer> COUNT <count> BLOCK <ms> STREAMS <stream> >
            // ">" means read new messages not yet delivered to any consumer in the group
            let result: redis::RedisResult<redis::streams::StreamReadReply> = redis::cmd("XREADGROUP")
                .arg("GROUP")
                .arg(consumer_group)
                .arg(node_id) // Use node_id as consumer name
                .arg("COUNT")
                .arg(10) // Read up to 10 messages at once
                .arg("BLOCK")
                .arg(1000) // Block for 1 second
                .arg("STREAMS")
                .arg(stream_key)
                .arg(">") // Read new messages
                .query_async(&mut conn)
                .await;

            match result {
                Ok(reply) => {
                    for stream_key in &reply.keys {
                        for stream_id in &stream_key.ids {
                            // Extract the message payload
                            if let Some(payload_value) = stream_id.map.get("payload") {
                                // Try to convert payload to string
                                let payload_str = match payload_value {
                                    redis::Value::BulkString(bytes) => {
                                        std::str::from_utf8(bytes).ok()
                                    }
                                    redis::Value::SimpleString(s) => Some(s.as_str()),
                                    _ => None,
                                };

                                if let Some(payload_str) = payload_str {
                                    match serde_json::from_str::<InvalidationMessage>(payload_str) {
                                        Ok(invalidation) => {
                                            debug!(
                                                node_id = %node_id,
                                                message_id = %stream_id.id,
                                                ?invalidation,
                                                "Received cache invalidation message"
                                            );

                                            // Broadcast locally
                                            if let Err(e) = local_sender.send(invalidation) {
                                                warn!(
                                                    error = %e,
                                                    "Failed to broadcast invalidation locally"
                                                );
                                            }

                                            // Acknowledge the message
                                            let _: redis::RedisResult<()> = redis::cmd("XACK")
                                                .arg(stream_key.key.as_str())
                                                .arg(consumer_group)
                                                .arg(&stream_id.id)
                                                .query_async(&mut conn)
                                                .await;
                                        }
                                        Err(e) => {
                                            warn!(
                                                error = %e,
                                                json = %payload_str,
                                                "Failed to parse invalidation message"
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    error!(
                        error = %e,
                        "Failed to read from Redis stream"
                    );
                    return Err(Error::Internal(format!("Failed to read from Redis stream: {e}")));
                }
            }
        }

        Ok(())
    }

    /// Stop the cache invalidation service
    pub fn stop(&self) {
        self.shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Subscribe to local cache invalidation events
    ///
    /// Returns a receiver that will receive invalidation messages from all nodes.
    #[must_use] 
    pub fn subscribe(&self) -> broadcast::Receiver<InvalidationMessage> {
        self.local_sender.subscribe()
    }

    /// Broadcast a cache invalidation message to all OTHER nodes (remote only)
    ///
    /// This sends the message via Redis Streams (if configured).
    /// Note: This does NOT broadcast locally, as the caller is expected to
    /// invalidate its own local cache after calling this method.
    ///
    /// For local-only invalidation (when Redis is not configured), this is a no-op.
    pub async fn broadcast_remote(&self, message: InvalidationMessage) -> Result<()> {
        // Broadcast via Redis Streams if available
        if let Some(ref client) = self.redis_client {
            let json = serde_json::to_string(&message).map_err(|e| {
                Error::Internal(format!("Failed to serialize invalidation message: {e}"))
            })?;

            let mut conn = client
                .get_multiplexed_async_connection()
                .await
                .map_err(|e| Error::Internal(format!("Redis connection failed: {e}")))?;

            // XADD <stream> MAXLEN ~ <max_len> * payload <json>
            // "~" makes MAXLEN approximate for better performance
            let _: String = redis::cmd("XADD")
                .arg(&self.stream_key)
                .arg("MAXLEN")
                .arg("~") // Approximate trimming for performance
                .arg(10000) // Keep last ~10000 messages
                .arg("*") // Auto-generate message ID
                .arg("payload")
                .arg(json)
                .query_async(&mut conn)
                .await
                .map_err(|e| Error::Internal(format!("Failed to add message to stream: {e}")))?;

            debug!(
                node_id = %self.node_id,
                ?message,
                "Published cache invalidation message to stream"
            );
        }

        Ok(())
    }

    /// Broadcast a cache invalidation message locally only (no Redis).
    ///
    /// Use this when the invalidation event was already received from a remote
    /// source (e.g., a cluster `CacheInvalidate` event from Redis Pub/Sub) and
    /// only the local caches on this node need to be notified.
    pub fn broadcast_local(&self, message: InvalidationMessage) -> Result<()> {
        if let Err(e) = self.local_sender.send(message) {
            warn!(error = %e, "Failed to broadcast invalidation locally");
        }
        Ok(())
    }

    /// Broadcast a cache invalidation message to ALL nodes including this one
    ///
    /// This sends the message via Redis Pub/Sub (if configured) and also
    /// broadcasts locally via the local channel.
    /// Use this when you want all nodes (including this one) to invalidate
    /// their caches via the subscription mechanism.
    pub async fn broadcast_all(&self, message: InvalidationMessage) -> Result<()> {
        // Broadcast locally first
        if let Err(e) = self.local_sender.send(message.clone()) {
            warn!(error = %e, "Failed to broadcast invalidation locally");
        }

        // Then broadcast via Redis
        self.broadcast_remote(message).await
    }

    /// Invalidate permission cache for a specific user in a room
    pub async fn invalidate_user_permission(
        &self,
        room_id: &RoomId,
        user_id: &crate::models::UserId,
    ) -> Result<()> {
        self.broadcast_remote(InvalidationMessage::UserPermission {
            room_id: room_id.as_str().to_string(),
            user_id: user_id.as_str().to_string(),
        }).await
    }

    /// Invalidate permission cache for all users in a room
    pub async fn invalidate_room_permission(&self, room_id: &RoomId) -> Result<()> {
        self.broadcast_remote(InvalidationMessage::RoomPermission {
            room_id: room_id.as_str().to_string(),
        }).await
    }

    /// Invalidate user cache
    pub async fn invalidate_user(&self, user_id: &crate::models::UserId) -> Result<()> {
        self.broadcast_remote(InvalidationMessage::User {
            user_id: user_id.as_str().to_string(),
        }).await
    }

    /// Invalidate username cache
    pub async fn invalidate_username(&self, user_id: &crate::models::UserId) -> Result<()> {
        self.broadcast_remote(InvalidationMessage::Username {
            user_id: user_id.as_str().to_string(),
        }).await
    }

    /// Invalidate room cache
    pub async fn invalidate_room(&self, room_id: &RoomId) -> Result<()> {
        self.broadcast_remote(InvalidationMessage::Room {
            room_id: room_id.as_str().to_string(),
        }).await
    }

    /// Invalidate playback state cache for a room
    pub async fn invalidate_playback_state(&self, room_id: &RoomId) -> Result<()> {
        self.broadcast_remote(InvalidationMessage::PlaybackState {
            room_id: room_id.as_str().to_string(),
        }).await
    }

    /// Invalidate room settings cache for a specific room
    pub async fn invalidate_room_settings(&self, room_id: &RoomId) -> Result<()> {
        self.broadcast_remote(InvalidationMessage::RoomSettings {
            room_id: room_id.as_str().to_string(),
        }).await
    }

    /// Invalidate all caches
    pub async fn invalidate_all(&self) -> Result<()> {
        self.broadcast_remote(InvalidationMessage::All).await
    }

    /// Broadcast bloom filter updates to other replicas
    ///
    /// When a new entity is created on this replica, call this so that other
    /// replicas mark the key as existing in their bloom filters, preventing
    /// false "definitely not exists" responses.
    pub async fn broadcast_bloom_filter_update(&self, keys: Vec<String>) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        self.broadcast_remote(InvalidationMessage::BloomFilterUpdate { keys }).await
    }
}

impl std::fmt::Debug for CacheInvalidationService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheInvalidationService")
            .field("redis_enabled", &self.redis_client.is_some())
            .field("node_id", &self.node_id)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invalidation_message_serialization() {
        let msg = InvalidationMessage::UserPermission {
            room_id: "room123".to_string(),
            user_id: "user456".to_string(),
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("user_permission"));

        let decoded: InvalidationMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_room_permission_message() {
        let msg = InvalidationMessage::RoomPermission {
            room_id: "room123".to_string(),
        };

        let json = serde_json::to_string(&msg).unwrap();
        let decoded: InvalidationMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, decoded);
    }

    #[tokio::test]
    async fn test_local_broadcast() {
        let service = CacheInvalidationService::new(None, "test-node".to_string(), "synctv:cache:invalidate:stream".to_string());
        let mut receiver = service.subscribe();

        let msg = InvalidationMessage::User {
            user_id: "user123".to_string(),
        };

        // broadcast_all sends to local + Redis; broadcast only sends to Redis
        service.broadcast_all(msg.clone()).await.unwrap();

        let received = receiver.recv().await.unwrap();
        assert_eq!(msg, received);
    }

    #[tokio::test]
    async fn test_broadcast_without_redis_is_noop() {
        let service = CacheInvalidationService::new(None, "test-node".to_string(), "synctv:cache:invalidate:stream".to_string());

        // broadcast_remote() without Redis should be a no-op (no local broadcast)
        let msg = InvalidationMessage::All;
        service.broadcast_remote(msg).await.unwrap();
    }
}
