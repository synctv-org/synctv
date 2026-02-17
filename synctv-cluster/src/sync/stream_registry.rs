use dashmap::DashMap;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

/// Identifier for a stream (app_name/stream_name)
pub type StreamIdentifier = String;

/// Metadata about a published stream
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamMetadata {
    /// Stream identifier (app/stream)
    pub identifier: String,
    /// Replica/node ID hosting this stream
    pub replica_id: String,
    /// Publisher type (rtmp, webrtc, etc.)
    pub pub_type: String,
    /// Unix timestamp when stream was published
    pub published_at: u64,
    /// Publisher remote address
    pub publisher_addr: Option<String>,
}

/// TTL for stream metadata in Redis (seconds).
/// Acts as a crash-safety mechanism: if a node crashes without unpublishing,
/// the stream metadata will expire after this duration.
const STREAM_METADATA_TTL_SECONDS: i64 = 300; // 5 minutes

/// Registry for tracking active streams across all replicas in the cluster.
///
/// Uses Redis for distributed state, with local DashMap as a cache for
/// streams hosted on this replica. Provides cross-replica stream discovery
/// and routing capabilities.
#[derive(Clone)]
pub struct StreamRegistry {
    /// Local cache of streams hosted on this replica
    local_streams: Arc<DashMap<StreamIdentifier, StreamMetadata>>,

    /// Optional Redis connection for distributed stream registry
    redis_conn: Option<redis::aio::ConnectionManager>,

    /// Key prefix for Redis keys (e.g., "synctv:")
    redis_key_prefix: String,

    /// This replica's ID
    replica_id: String,
}

impl StreamRegistry {
    /// Create a new StreamRegistry
    #[must_use]
    pub fn new(replica_id: String) -> Self {
        Self {
            local_streams: Arc::new(DashMap::new()),
            redis_conn: None,
            redis_key_prefix: String::new(),
            replica_id,
        }
    }

    /// Enable distributed stream registry via Redis.
    ///
    /// When Redis is configured, stream metadata is persisted for cross-replica
    /// visibility and discovery. Without Redis, the registry is local-only.
    #[must_use]
    pub fn with_redis(mut self, conn: redis::aio::ConnectionManager, key_prefix: &str) -> Self {
        self.redis_conn = Some(conn);
        self.redis_key_prefix = key_prefix.to_string();
        self
    }

    /// Register a published stream
    ///
    /// Stores the stream in local cache and persists to Redis for cross-replica visibility.
    pub async fn register_stream(
        &self,
        identifier: &str,
        pub_type: &str,
        publisher_addr: Option<String>,
    ) -> Result<(), String> {
        let metadata = StreamMetadata {
            identifier: identifier.to_string(),
            replica_id: self.replica_id.clone(),
            pub_type: pub_type.to_string(),
            published_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            publisher_addr,
        };

        // Store in local cache
        self.local_streams.insert(identifier.to_string(), metadata.clone());

        // Persist to Redis (best-effort)
        if let Some(ref conn) = self.redis_conn {
            let active_key = format!("{}streams:active", self.redis_key_prefix);
            let meta_key = format!("{}streams:meta:{}", self.redis_key_prefix, identifier);

            let json = serde_json::to_string(&metadata)
                .map_err(|e| format!("Failed to serialize stream metadata: {e}"))?;

            let mut conn_clone = conn.clone();
            let identifier_owned = identifier.to_string();

            tokio::spawn(async move {
                // Add to active streams set
                if let Err(e) = conn_clone.sadd::<_, _, ()>(&active_key, &identifier_owned).await {
                    warn!("Failed to add stream to active set: {e}");
                }

                // Store metadata with TTL for crash safety
                if let Err(e) = conn_clone.set::<_, _, ()>(&meta_key, &json).await {
                    warn!("Failed to persist stream metadata: {e}");
                } else if let Err(e) = conn_clone.expire::<_, ()>(&meta_key, STREAM_METADATA_TTL_SECONDS).await {
                    warn!("Failed to set stream metadata TTL: {e}");
                }
            });
        }

        info!(
            stream = %identifier,
            replica_id = %self.replica_id,
            pub_type = %pub_type,
            "Stream registered"
        );

        Ok(())
    }

    /// Unregister a stream (called on unpublish)
    ///
    /// Removes from local cache and Redis.
    pub async fn unregister_stream(&self, identifier: &str) {
        self.local_streams.remove(identifier);

        // Remove from Redis (best-effort)
        if let Some(ref conn) = self.redis_conn {
            let active_key = format!("{}streams:active", self.redis_key_prefix);
            let meta_key = format!("{}streams:meta:{}", self.redis_key_prefix, identifier);

            let mut conn_clone = conn.clone();
            let identifier_owned = identifier.to_string();

            tokio::spawn(async move {
                let _: Result<(), _> = conn_clone.srem(&active_key, &identifier_owned).await;
                let _: Result<(), _> = conn_clone.del(&meta_key).await;
            });
        }

        info!(
            stream = %identifier,
            replica_id = %self.replica_id,
            "Stream unregistered"
        );
    }

    /// Get metadata for a specific stream (local or from Redis)
    ///
    /// Checks local cache first, then queries Redis if available.
    pub async fn get_stream(&self, identifier: &str) -> Option<StreamMetadata> {
        // Check local cache first
        if let Some(meta) = self.local_streams.get(identifier) {
            return Some(meta.clone());
        }

        // Query Redis if available
        if let Some(ref conn) = self.redis_conn {
            let meta_key = format!("{}streams:meta:{}", self.redis_key_prefix, identifier);
            let mut conn_clone = conn.clone();

            if let Ok(json) = conn_clone.get::<_, String>(&meta_key).await {
                if let Ok(metadata) = serde_json::from_str::<StreamMetadata>(&json) {
                    return Some(metadata);
                }
            }
        }

        None
    }

    /// Get all active streams across all replicas (from Redis)
    ///
    /// Returns the full list of active streams from Redis, which includes
    /// streams from all replicas in the cluster. Falls back to local-only
    /// if Redis is not available.
    pub async fn get_all_streams(&self) -> Vec<StreamMetadata> {
        if let Some(ref conn) = self.redis_conn {
            let active_key = format!("{}streams:active", self.redis_key_prefix);
            let mut conn_clone = conn.clone();

            match conn_clone.smembers::<_, Vec<String>>(&active_key).await {
                Ok(identifiers) => {
                    let mut results = Vec::new();
                    for id in identifiers {
                        if let Some(meta) = self.get_stream(&id).await {
                            results.push(meta);
                        }
                    }
                    return results;
                }
                Err(e) => {
                    warn!("Failed to fetch active streams from Redis, falling back to local: {e}");
                }
            }
        }

        // Fallback to local-only
        self.local_streams
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Get all streams hosted on this replica
    pub fn get_local_streams(&self) -> Vec<StreamMetadata> {
        self.local_streams
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Check if a stream is active on this replica
    pub fn is_local_stream(&self, identifier: &str) -> bool {
        self.local_streams.contains_key(identifier)
    }

    /// Spawn a background task to periodically refresh stream TTLs in Redis.
    ///
    /// This prevents streams from expiring while they are still active.
    /// The task runs every `interval` and stops when `cancel_token` is cancelled.
    #[must_use]
    pub fn spawn_ttl_refresh_task(
        &self,
        interval: Duration,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let registry = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.tick().await; // Skip first immediate tick

            loop {
                tokio::select! {
                    () = cancel_token.cancelled() => {
                        info!("Stream registry TTL refresh task shutting down");
                        return;
                    }
                    _ = ticker.tick() => {
                        registry.refresh_ttls().await;
                    }
                }
            }
        })
    }

    /// Spawn a background task to periodically clean up stale entries from the
    /// Redis `streams:active` set.
    ///
    /// When a node crashes without unpublishing, its stream metadata keys expire
    /// via TTL but the corresponding entry in the `streams:active` set is never
    /// removed. This task scans the active set and removes any entry whose
    /// metadata key no longer exists in Redis.
    #[must_use]
    pub fn spawn_active_set_cleanup_task(
        &self,
        interval: Duration,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let registry = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.tick().await; // Skip first immediate tick

            loop {
                tokio::select! {
                    () = cancel_token.cancelled() => {
                        info!("Stream registry active set cleanup task shutting down");
                        return;
                    }
                    _ = ticker.tick() => {
                        registry.cleanup_stale_active_entries().await;
                    }
                }
            }
        })
    }

    /// Remove entries from the `streams:active` set whose metadata keys have expired.
    async fn cleanup_stale_active_entries(&self) {
        let Some(ref conn) = self.redis_conn else {
            return;
        };

        let active_key = format!("{}streams:active", self.redis_key_prefix);
        let mut conn_clone = conn.clone();

        let identifiers: Vec<String> = match conn_clone.smembers(&active_key).await {
            Ok(ids) => ids,
            Err(e) => {
                warn!("Failed to read active streams set for cleanup: {e}");
                return;
            }
        };

        let mut removed = 0u64;
        for identifier in &identifiers {
            let meta_key = format!("{}streams:meta:{}", self.redis_key_prefix, identifier);
            let exists: bool = match conn_clone.exists(&meta_key).await {
                Ok(v) => v,
                Err(e) => {
                    warn!("Failed to check stream metadata existence for cleanup: {e}");
                    continue;
                }
            };

            if !exists {
                // Metadata expired (node crashed) -- remove from active set
                let result: Result<(), _> = conn_clone.srem(&active_key, identifier).await;
                if let Err(e) = result {
                    warn!("Failed to remove stale stream from active set: {e}");
                } else {
                    removed += 1;
                    debug!(
                        stream = %identifier,
                        "Removed stale stream from active set (metadata expired)"
                    );
                }
            }
        }

        if removed > 0 {
            info!(
                removed_count = removed,
                total_checked = identifiers.len(),
                "Cleaned up stale entries from streams:active set"
            );
        }
    }

    /// Refresh TTLs for all local streams in Redis
    async fn refresh_ttls(&self) {
        if let Some(ref conn) = self.redis_conn {
            let local_streams: Vec<String> = self
                .local_streams
                .iter()
                .map(|entry| entry.key().clone())
                .collect();

            if local_streams.is_empty() {
                return;
            }

            let stream_count = local_streams.len();
            let mut conn_clone = conn.clone();
            let prefix = self.redis_key_prefix.clone();

            tokio::spawn(async move {
                for identifier in local_streams {
                    let meta_key = format!("{}streams:meta:{}", prefix, identifier);
                    let _: Result<(), _> = conn_clone.expire(&meta_key, STREAM_METADATA_TTL_SECONDS).await;
                }
            });

            debug!(
                count = stream_count,
                "Refreshed stream metadata TTLs"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_register_and_get_stream() {
        let registry = StreamRegistry::new("replica1".to_string());

        registry
            .register_stream("test_app/test_stream", "rtmp", None)
            .await
            .unwrap();

        assert!(registry.is_local_stream("test_app/test_stream"));

        let meta = registry.get_stream("test_app/test_stream").await.unwrap();
        assert_eq!(meta.identifier, "test_app/test_stream");
        assert_eq!(meta.replica_id, "replica1");
        assert_eq!(meta.pub_type, "rtmp");
    }

    #[tokio::test]
    async fn test_unregister_stream() {
        let registry = StreamRegistry::new("replica1".to_string());

        registry
            .register_stream("test_app/test_stream", "rtmp", None)
            .await
            .unwrap();
        assert!(registry.is_local_stream("test_app/test_stream"));

        registry.unregister_stream("test_app/test_stream").await;
        assert!(!registry.is_local_stream("test_app/test_stream"));
    }

    #[tokio::test]
    async fn test_get_local_streams() {
        let registry = StreamRegistry::new("replica1".to_string());

        registry
            .register_stream("app1/stream1", "rtmp", None)
            .await
            .unwrap();
        registry
            .register_stream("app2/stream2", "webrtc", None)
            .await
            .unwrap();

        let streams = registry.get_local_streams();
        assert_eq!(streams.len(), 2);
    }
}
