use redis::AsyncCommands;
use std::future::Future;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tracing::debug;

use crate::{
    cache::KeyBuilder, service::oauth2::OAuth2State, Error, InternalExt, RedisConnectionRuntime,
    Result, SharedStateMode, SharedStateProfile,
};

static CONSUME_OAUTH2_STATE_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
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

#[async_trait::async_trait]
pub trait OAuthStateStore: Send + Sync {
    async fn store(&self, token_id: &str, state: &OAuth2State, ttl: Duration) -> Result<()>;

    async fn consume(&self, token_id: &str) -> Result<Option<OAuth2State>>;

    fn supports_cross_node_single_use(&self) -> bool;
}

pub fn state_store_from_shared_state_profile(
    profile: &SharedStateProfile,
) -> Result<Arc<dyn OAuthStateStore>> {
    match profile.state_mode() {
        SharedStateMode::SharedRequired => {
            let shared_runtime =
                profile.require_shared_runtime("single-use OAuth2 state storage")?;
            Ok(shared_oauth_state_store(
                shared_runtime,
                profile.key_prefix().to_string(),
            ))
        }
        SharedStateMode::SharedBestEffort => Ok(shared_oauth_state_store(
            profile.best_effort_shared_runtime("single-use OAuth2 state storage")?,
            profile.key_prefix().to_string(),
        )),
        SharedStateMode::LocalOnly => Ok(local_oauth_state_store()),
    }
}

#[must_use]
pub fn local_oauth_state_store() -> Arc<dyn OAuthStateStore> {
    Arc::new(InMemoryOAuthStateStore::new())
}

fn shared_oauth_state_store(
    runtime: Arc<dyn RedisConnectionRuntime>,
    key_prefix: impl Into<String>,
) -> Arc<dyn OAuthStateStore> {
    Arc::new(RedisOAuthStateStore::from_runtime(runtime, key_prefix))
}

pub struct RedisOAuthStateStore {
    conn: Arc<dyn RedisConnectionRuntime>,
    key_builder: KeyBuilder,
}

impl RedisOAuthStateStore {
    async fn run_redis_op<T, F>(&self, operation: &'static str, future: F) -> Result<T>
    where
        F: Future<Output = std::result::Result<T, redis::RedisError>>,
    {
        run_oauth_state_redis_op(self.conn.operation_timeout(), operation, future).await
    }

    #[must_use]
    pub fn from_runtime(
        conn: Arc<dyn RedisConnectionRuntime>,
        key_prefix: impl Into<String>,
    ) -> Self {
        Self {
            conn,
            key_builder: KeyBuilder::new(key_prefix),
        }
    }

    async fn get_conn(&self, operation: &'static str) -> Result<redis::aio::ConnectionManager> {
        crate::redis_runtime_snapshot(&*self.conn, operation).await
    }

    fn redis_key(&self, token_id: &str) -> String {
        self.key_builder.oauth2_state(token_id)
    }

    #[cfg(test)]
    pub(crate) fn runtime_ptr_eq(&self, runtime: &Arc<dyn RedisConnectionRuntime>) -> bool {
        Arc::ptr_eq(&self.conn, runtime)
    }
}

pub(crate) async fn run_oauth_state_redis_op<T, F>(
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
        .internal_with_err(&format!("Failed to {operation}"))
}

#[async_trait::async_trait]
impl OAuthStateStore for RedisOAuthStateStore {
    fn supports_cross_node_single_use(&self) -> bool {
        true
    }

    async fn store(&self, token_id: &str, state: &OAuth2State, ttl: Duration) -> Result<()> {
        let key = self.redis_key(token_id);
        let value =
            serde_json::to_string(state).internal_with_err("Failed to serialize OAuth2 state")?;

        let mut conn = self.get_conn("store OAuth2 state in Redis").await?;
        let _: () = self
            .run_redis_op(
                "store OAuth2 state in Redis",
                conn.set_ex(&key, value, ttl.as_secs()),
            )
            .await?;

        debug!(
            "Stored OAuth2 state in Redis for token {}",
            &token_id[..8.min(token_id.len())]
        );
        Ok(())
    }

    async fn consume(&self, token_id: &str) -> Result<Option<OAuth2State>> {
        let key = self.redis_key(token_id);
        let mut conn = self.get_conn("consume OAuth2 state from Redis").await?;

        let value: Option<String> = self
            .run_redis_op(
                "consume OAuth2 state from Redis",
                CONSUME_OAUTH2_STATE_SCRIPT
                    .key(&key)
                    .invoke_async(&mut conn),
            )
            .await?;

        match value {
            Some(json) => {
                let state: OAuth2State = serde_json::from_str(&json)
                    .internal_with_err("Failed to deserialize OAuth2 state")?;
                debug!(
                    "Retrieved OAuth2 state from Redis for token {}",
                    &token_id[..8.min(token_id.len())]
                );
                Ok(Some(state))
            }
            None => Ok(None),
        }
    }
}

#[derive(Clone)]
struct OAuthStateEntry {
    state: OAuth2State,
    ttl: Duration,
}

struct PerEntryTtl;

impl moka::Expiry<String, OAuthStateEntry> for PerEntryTtl {
    fn expire_after_create(
        &self,
        _key: &String,
        value: &OAuthStateEntry,
        _now: std::time::Instant,
    ) -> Option<Duration> {
        Some(value.ttl)
    }
}

pub struct InMemoryOAuthStateStore {
    entries: moka::sync::Cache<String, OAuthStateEntry>,
}

const DEFAULT_CAPACITY: u64 = 10_000;

impl InMemoryOAuthStateStore {
    #[must_use]
    pub fn new() -> Self {
        Self::new_with_capacity(DEFAULT_CAPACITY)
    }

    #[must_use]
    pub fn new_with_capacity(max_capacity: u64) -> Self {
        Self {
            entries: moka::sync::Cache::builder()
                .max_capacity(max_capacity)
                .expire_after(PerEntryTtl)
                .build(),
        }
    }
}

impl Default for InMemoryOAuthStateStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl OAuthStateStore for InMemoryOAuthStateStore {
    fn supports_cross_node_single_use(&self) -> bool {
        false
    }

    async fn store(&self, token_id: &str, state: &OAuth2State, ttl: Duration) -> Result<()> {
        self.entries.insert(
            token_id.to_string(),
            OAuthStateEntry {
                state: state.clone(),
                ttl,
            },
        );
        Ok(())
    }

    async fn consume(&self, token_id: &str) -> Result<Option<OAuth2State>> {
        if self.entries.get(token_id).is_none() {
            return Ok(None);
        }
        Ok(self.entries.remove(token_id).map(|e| e.state))
    }
}
