use std::future::Future;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use redis::AsyncCommands;
use serde::{de::DeserializeOwned, Serialize};

use crate::{cache::KeyBuilder, Error, InternalExt, RedisConnectionRuntime, Result};

static CONSUME_REDIS_VALUE_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
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

pub(crate) struct RedisJsonSessionStore {
    runtime: Arc<dyn RedisConnectionRuntime>,
    key_builder: KeyBuilder,
}

impl RedisJsonSessionStore {
    #[must_use]
    pub(crate) fn new(
        runtime: Arc<dyn RedisConnectionRuntime>,
        key_prefix: impl Into<String>,
    ) -> Self {
        Self {
            runtime,
            key_builder: KeyBuilder::new(key_prefix),
        }
    }

    fn redis_key(&self, namespace: &str, session_id: &str) -> String {
        self.key_builder.session(namespace, session_id)
    }

    async fn run_redis_op<T, F>(&self, operation: &'static str, future: F) -> Result<T>
    where
        F: Future<Output = std::result::Result<T, redis::RedisError>>,
    {
        tokio::time::timeout(self.runtime.operation_timeout(), future)
            .await
            .map_err(|_| Error::Timeout(format!("Redis timeout: {operation}")))?
            .internal_with_err(&format!("Failed to {operation}"))
    }

    pub(crate) async fn store<T>(
        &self,
        namespace: &'static str,
        session_id: &str,
        session: &T,
        ttl: Duration,
        serialize_context: &'static str,
        operation: &'static str,
    ) -> Result<()>
    where
        T: Serialize,
    {
        let key = self.redis_key(namespace, session_id);
        let value = serde_json::to_string(session).internal_with_err(serialize_context)?;
        let mut conn = crate::redis_runtime_snapshot(&*self.runtime, operation).await?;
        let _: () = self
            .run_redis_op(operation, conn.set_ex(key, value, ttl.as_secs()))
            .await?;
        Ok(())
    }

    pub(crate) async fn get<T>(
        &self,
        namespace: &'static str,
        session_id: &str,
        deserialize_context: &'static str,
        operation: &'static str,
    ) -> Result<Option<T>>
    where
        T: DeserializeOwned,
    {
        let key = self.redis_key(namespace, session_id);
        let mut conn = crate::redis_runtime_snapshot(&*self.runtime, operation).await?;
        let value: Option<String> = self.run_redis_op(operation, conn.get(key)).await?;
        Self::decode_optional(value, deserialize_context)
    }

    pub(crate) async fn consume<T>(
        &self,
        namespace: &'static str,
        session_id: &str,
        deserialize_context: &'static str,
        operation: &'static str,
    ) -> Result<Option<T>>
    where
        T: DeserializeOwned,
    {
        let key = self.redis_key(namespace, session_id);
        let mut conn = crate::redis_runtime_snapshot(&*self.runtime, operation).await?;
        let value: Option<String> = self
            .run_redis_op(
                operation,
                CONSUME_REDIS_VALUE_SCRIPT.key(key).invoke_async(&mut conn),
            )
            .await?;
        Self::decode_optional(value, deserialize_context)
    }

    fn decode_optional<T>(
        value: Option<String>,
        deserialize_context: &'static str,
    ) -> Result<Option<T>>
    where
        T: DeserializeOwned,
    {
        value
            .map(|json| serde_json::from_str(&json).internal_with_err(deserialize_context))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[test]
    fn redis_json_session_store_normalizes_key_prefix() {
        struct NoopRuntime;

        #[async_trait::async_trait]
        impl RedisConnectionRuntime for NoopRuntime {
            async fn snapshot(&self) -> redis::RedisResult<redis::aio::ConnectionManager> {
                std::panic::panic_any("snapshot should not be needed for key formatting");
            }
        }

        let store = RedisJsonSessionStore::new(Arc::new(NoopRuntime), "synctv");
        assert_eq!(
            store.redis_key("auth:test", "sess1"),
            "synctv:auth:test:sess1"
        );
        assert_eq!(
            store.redis_key("auth:test", "sess:1"),
            "synctv:auth:test:sess_1"
        );

        let store = RedisJsonSessionStore::new(Arc::new(NoopRuntime), "synctv:");
        assert_eq!(
            store.redis_key("auth:test", "sess1"),
            "synctv:auth:test:sess1"
        );

        let store = RedisJsonSessionStore::new(Arc::new(NoopRuntime), "");
        assert_eq!(store.redis_key("auth:test", "sess1"), "auth:test:sess1");
    }

    #[test]
    fn decode_optional_deserializes_present_value() {
        #[derive(Debug, Deserialize, PartialEq, Eq)]
        struct Session {
            user_id: u64,
        }

        let decoded = match RedisJsonSessionStore::decode_optional::<Session>(
            Some(r#"{"user_id":42}"#.to_string()),
            "decode session",
        ) {
            Ok(decoded) => decoded,
            Err(error) => std::panic::panic_any(format!("valid json should decode: {error}")),
        };

        assert_eq!(decoded, Some(Session { user_id: 42 }));
    }

    #[test]
    fn decode_optional_returns_none_without_value() {
        let decoded = match RedisJsonSessionStore::decode_optional::<serde_json::Value>(
            None,
            "decode session",
        ) {
            Ok(decoded) => decoded,
            Err(error) => std::panic::panic_any(format!("missing value should decode: {error}")),
        };

        assert_eq!(decoded, None);
    }
}
