use async_trait::async_trait;
use std::future::Future;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use crate::cache::KeyBuilder;
use crate::models::RoomId;
use crate::{Error, RedisConnectionRuntime, Result};

use super::{TicketStore, WsTicketData};

static CLAIM_TICKET_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r#"
        local value = redis.call("GET", KEYS[1])
        if not value then
            return 0
        end
        if value ~= ARGV[1] then
            return 0
        end
        redis.call("DEL", KEYS[1])
        return 1
        "#,
    )
});

static CONSUME_TICKET_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r#"
        local value = redis.call("GET", KEYS[1])
        if value then
            redis.call("DEL", KEYS[1])
        end
        return value
        "#,
    )
});

/// Redis-backed ticket store for multi-replica deployments.
///
/// Uses a shared `Arc<RwLock<ConnectionManager>>` so that in Sentinel mode the
/// background health check can hot-swap the inner connection on failover and
/// this store automatically picks up the new master.
pub struct RedisTicketStore {
    pub(super) redis_runtime: Arc<dyn RedisConnectionRuntime>,
    key_builder: KeyBuilder,
}

impl RedisTicketStore {
    async fn run_redis_op<T, F>(&self, operation: &'static str, future: F) -> Result<T>
    where
        F: Future<Output = std::result::Result<T, redis::RedisError>>,
    {
        run_ws_ticket_redis_op(self.redis_runtime.operation_timeout(), operation, future).await
    }

    #[must_use]
    pub fn new(
        shared_conn: Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
        key_prefix: impl Into<String>,
    ) -> Self {
        Self::from_runtime(crate::shared_runtime(shared_conn), key_prefix)
    }

    #[must_use]
    pub fn from_runtime(
        redis_runtime: Arc<dyn RedisConnectionRuntime>,
        key_prefix: impl Into<String>,
    ) -> Self {
        Self {
            redis_runtime,
            key_builder: KeyBuilder::new(key_prefix),
        }
    }

    async fn conn(&self, operation: &'static str) -> Result<redis::aio::ConnectionManager> {
        crate::redis_runtime_snapshot(&*self.redis_runtime, operation).await
    }

    fn redis_key(&self, ticket: &str) -> String {
        self.key_builder.ws_ticket(ticket)
    }
}

pub(super) async fn run_ws_ticket_redis_op<T, F>(
    timeout: Duration,
    operation: &'static str,
    future: F,
) -> Result<T>
where
    F: Future<Output = std::result::Result<T, redis::RedisError>>,
{
    tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| Error::Timeout(format!("Redis timeout: {operation}")))?
        .map_err(|error| {
            tracing::warn!(operation, error = %error, "WebSocket ticket Redis operation failed");
            Error::ServiceUnavailable(
                "WebSocket ticket service is temporarily unavailable. Please try again later."
                    .to_string(),
            )
        })
}

#[async_trait]
impl TicketStore for RedisTicketStore {
    async fn store(&self, ticket: &str, data: &WsTicketData, ttl_secs: u64) -> Result<()> {
        use redis::AsyncCommands;

        let key = self.redis_key(ticket);
        let json = serde_json::to_string(data)
            .map_err(|e| Error::Internal(format!("Failed to serialize ticket data: {e}")))?;

        let mut conn = self.conn("store ticket").await?;
        let _: () = self
            .run_redis_op("store ticket", conn.set_ex(&key, json, ttl_secs))
            .await?;

        Ok(())
    }

    async fn load(&self, ticket: &str, _expected_room_id: &RoomId) -> Result<Option<WsTicketData>> {
        use redis::AsyncCommands;

        let key = self.redis_key(ticket);
        let mut conn = self.conn("load ticket").await?;

        let json: Option<String> = self.run_redis_op("load ticket", conn.get(&key)).await?;

        let Some(json) = json else {
            return Ok(None);
        };

        let data: WsTicketData = serde_json::from_str(&json)
            .map_err(|e| Error::Internal(format!("Failed to deserialize ticket data: {e}")))?;

        Ok(Some(data))
    }

    async fn claim(
        &self,
        ticket: &str,
        _expected_room_id: &RoomId,
        expected_ticket: &WsTicketData,
    ) -> Result<bool> {
        let key = self.redis_key(ticket);
        let mut conn = self.conn("claim ticket").await?;
        let expected_json = serde_json::to_string(expected_ticket)
            .map_err(|e| Error::Internal(format!("Failed to serialize ticket data: {e}")))?;

        let deleted: i64 = self
            .run_redis_op(
                "claim ticket",
                CLAIM_TICKET_SCRIPT
                    .key(&key)
                    .arg(&expected_json)
                    .invoke_async(&mut conn),
            )
            .await?;

        Ok(deleted > 0)
    }

    async fn consume(
        &self,
        ticket: &str,
        _expected_room_id: &RoomId,
    ) -> Result<Option<WsTicketData>> {
        let key = self.redis_key(ticket);
        let mut conn = self.conn("validate ticket").await?;

        let json: Option<String> = self
            .run_redis_op(
                "validate ticket",
                CONSUME_TICKET_SCRIPT.key(&key).invoke_async(&mut conn),
            )
            .await?;

        let Some(json) = json else {
            return Ok(None);
        };

        let data: WsTicketData = serde_json::from_str(&json)
            .map_err(|e| Error::Internal(format!("Failed to deserialize ticket data: {e}")))?;

        Ok(Some(data))
    }

    fn supports_cluster_runtime(&self) -> bool {
        true
    }
}
