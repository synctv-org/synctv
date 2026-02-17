//! Redis-backed session affinity registry for multi-replica SFU deployments.
//!
//! Stores `sfu:session:{conn_id} -> replica_id` mappings in Redis with a TTL,
//! enabling load balancers and API gateways to route signaling requests to the
//! correct SFU replica that owns a given WebRTC `PeerConnection`.

use crate::session_manager::SessionAffinityRegistry;
use anyhow::Result;
use redis::AsyncCommands;

/// Redis-backed implementation of [`SessionAffinityRegistry`].
///
/// Each session is stored as a Redis key with an expiration TTL. The cleanup
/// task in [`SfuSessionManager`](crate::session_manager::SfuSessionManager)
/// periodically refreshes the TTL for active sessions.
pub struct RedisSessionAffinityRegistry {
    conn: redis::aio::ConnectionManager,
    key_prefix: String,
}

impl RedisSessionAffinityRegistry {
    /// Create a new Redis session affinity registry.
    ///
    /// - `conn`: A Redis `ConnectionManager` (supports automatic reconnection).
    /// - `key_prefix`: Prefix for all Redis keys (e.g., `"synctv:"` or `""`).
    pub fn new(conn: redis::aio::ConnectionManager, key_prefix: String) -> Self {
        Self { conn, key_prefix }
    }

    /// Build the Redis key for a given connection ID.
    fn key(&self, conn_id: &str) -> String {
        format!("{}sfu:session:{}", self.key_prefix, conn_id)
    }
}

#[async_trait::async_trait]
impl SessionAffinityRegistry for RedisSessionAffinityRegistry {
    async fn register(&self, conn_id: &str, replica_id: &str, ttl_secs: u64) -> Result<()> {
        let mut conn = self.conn.clone();
        conn.set_ex::<_, _, ()>(&self.key(conn_id), replica_id, ttl_secs)
            .await?;
        Ok(())
    }

    async fn lookup(&self, conn_id: &str) -> Result<Option<String>> {
        let mut conn = self.conn.clone();
        let result: Option<String> = conn.get(&self.key(conn_id)).await?;
        Ok(result)
    }

    async fn unregister(&self, conn_id: &str) -> Result<()> {
        let mut conn = self.conn.clone();
        conn.del::<_, ()>(&self.key(conn_id)).await?;
        Ok(())
    }
}
