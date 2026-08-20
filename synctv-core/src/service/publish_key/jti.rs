use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

use crate::{cache::KeyBuilder, Error, RedisConnectionRuntime, Result};

pub(super) async fn run_publish_key_redis_op<T, F>(
    timeout: Duration,
    operation: &'static str,
    future: F,
) -> Result<T>
where
    F: std::future::Future<Output = std::result::Result<T, redis::RedisError>>,
{
    tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| Error::Timeout(format!("Redis timeout: {operation}")))?
        .map_err(Error::Redis)
}

#[async_trait]
pub trait JtiStore: Send + Sync {
    async fn try_claim(&self, jti: &str, ttl_secs: u64) -> Result<bool>;

    async fn is_claimed(&self, jti: &str) -> bool;

    fn supports_cross_node_single_use(&self) -> bool {
        false
    }

    fn fail_closed(&self) -> bool {
        false
    }
}

pub struct RedisJtiStore {
    pub(super) redis_runtime: Arc<dyn RedisConnectionRuntime>,
    key_builder: KeyBuilder,
    local_cache: moka::future::Cache<String, u64>,
    fail_closed: bool,
}

struct JtiExpiry;

impl moka::Expiry<String, u64> for JtiExpiry {
    fn expire_after_create(
        &self,
        _key: &String,
        ttl_secs: &u64,
        _created_at: std::time::Instant,
    ) -> Option<Duration> {
        Some(Duration::from_secs(*ttl_secs))
    }
}

impl RedisJtiStore {
    #[must_use]
    pub fn from_runtime(
        redis_runtime: Arc<dyn RedisConnectionRuntime>,
        key_prefix: String,
        _cache_ttl_secs: u64,
    ) -> Self {
        Self {
            redis_runtime,
            key_builder: KeyBuilder::new(key_prefix),
            local_cache: moka::future::Cache::builder()
                .max_capacity(100_000)
                .expire_after(JtiExpiry)
                .build(),
            fail_closed: false,
        }
    }

    #[must_use]
    pub fn from_runtime_fail_closed(
        redis_runtime: Arc<dyn RedisConnectionRuntime>,
        key_prefix: String,
        _cache_ttl_secs: u64,
    ) -> Self {
        Self {
            redis_runtime,
            key_builder: KeyBuilder::new(key_prefix),
            local_cache: moka::future::Cache::builder()
                .max_capacity(100_000)
                .expire_after(JtiExpiry)
                .build(),
            fail_closed: true,
        }
    }

    #[must_use]
    pub fn new(
        conn: redis::aio::ConnectionManager,
        key_prefix: String,
        cache_ttl_secs: u64,
    ) -> Self {
        Self::from_runtime(
            crate::shared_runtime(Arc::new(tokio::sync::RwLock::new(conn))),
            key_prefix,
            cache_ttl_secs,
        )
    }
}

#[async_trait]
impl JtiStore for RedisJtiStore {
    async fn try_claim(&self, jti: &str, ttl_secs: u64) -> Result<bool> {
        if self.local_cache.contains_key(jti) {
            return Ok(false);
        }

        let redis_key = self.key_builder.publish_key_jti(jti);
        let mut conn =
            crate::redis_runtime_snapshot(&*self.redis_runtime, "claim publish-key JTI").await?;
        let ttl_ms = ttl_secs.saturating_mul(1000);
        let set_result: std::result::Result<Option<String>, _> = run_publish_key_redis_op(
            self.redis_runtime.operation_timeout(),
            "claim publish-key JTI",
            redis::cmd("SET")
                .arg(&redis_key)
                .arg(1i64)
                .arg("PX")
                .arg(ttl_ms)
                .arg("NX")
                .query_async(&mut conn),
        )
        .await;

        match set_result {
            Ok(Some(_)) => {
                self.local_cache.insert(jti.to_string(), ttl_secs).await;
                Ok(true)
            }
            Ok(None) => {
                self.local_cache.insert(jti.to_string(), ttl_secs).await;
                Ok(false)
            }
            Err(error) => {
                if self.fail_closed {
                    tracing::error!(
                        jti = %jti,
                        "Redis unavailable for JTI dedup and fail_closed=true, rejecting claim: {error}"
                    );
                    return Err(Error::Internal(format!(
                        "Redis unavailable for JTI dedup and fail_closed is enabled: {error}"
                    )));
                }

                tracing::warn!(
                    jti = %jti,
                    "Redis unavailable for JTI dedup, using local-only enforcement: {error}"
                );
                if self.local_cache.contains_key(jti) {
                    Ok(false)
                } else {
                    self.local_cache.insert(jti.to_string(), ttl_secs).await;
                    Ok(true)
                }
            }
        }
    }

    async fn is_claimed(&self, jti: &str) -> bool {
        self.local_cache.contains_key(jti)
    }

    fn fail_closed(&self) -> bool {
        self.fail_closed
    }

    fn supports_cross_node_single_use(&self) -> bool {
        true
    }
}

pub struct InMemoryJtiStore {
    cache: moka::future::Cache<String, u64>,
}

impl InMemoryJtiStore {
    #[must_use]
    pub fn new(_cache_ttl_secs: u64) -> Self {
        Self {
            cache: moka::future::Cache::builder()
                .max_capacity(100_000)
                .expire_after(JtiExpiry)
                .build(),
        }
    }
}

#[async_trait]
impl JtiStore for InMemoryJtiStore {
    async fn try_claim(&self, jti: &str, ttl_secs: u64) -> Result<bool> {
        use moka::ops::compute::Op;
        let entry = self
            .cache
            .entry_by_ref(jti)
            .and_compute_with(|maybe_entry| async move {
                if maybe_entry.is_some() {
                    Op::Nop
                } else {
                    Op::Put(ttl_secs)
                }
            })
            .await;

        match entry {
            moka::ops::compute::CompResult::Inserted(_)
            | moka::ops::compute::CompResult::ReplacedWith(_) => Ok(true),
            moka::ops::compute::CompResult::Unchanged(_)
            | moka::ops::compute::CompResult::Removed(_)
            | moka::ops::compute::CompResult::StillNone(_) => Ok(false),
        }
    }

    async fn is_claimed(&self, jti: &str) -> bool {
        self.cache.contains_key(jti)
    }
}
