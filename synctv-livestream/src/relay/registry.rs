use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use redis::aio::ConnectionManager as RedisConnectionManager;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use synctv_xiu::hls::DEFAULT_HLS_GENERATION_RETENTION;
use tracing::{debug, info};

use super::registry_trait::{
    ActiveStreamGeneration, LeaseRefreshOutcome, LeaseRefreshRequest, PUBLISHER_REFRESH_BATCH_SIZE,
};
use crate::util::{
    validate_publisher_cluster_address, validate_stream_generation_id,
    validate_stream_id_component, validate_stream_ids,
};

/// Heartbeat interval in seconds for publisher liveness.
/// The publisher manager sends a heartbeat every this many seconds.
pub(crate) const HEARTBEAT_INTERVAL_SECS: u64 = 60;

/// TTL multiplier: TTL = `HEARTBEAT_INTERVAL_SECS` * `TTL_MULTIPLIER`.
/// A multiplier of 5 means up to 4 consecutive missed heartbeats are tolerated
/// before the registry entry expires.
const HEARTBEAT_INTERVAL_SECS_I64: i64 = 60;
const TTL_MULTIPLIER_U64: u64 = 5;
const TTL_MULTIPLIER_I64: i64 = 5;

/// Publisher TTL in seconds, derived from heartbeat interval.
/// This is the Redis key expiration set on publisher entries.
const PUBLISHER_TTL_SECS_U64: u64 = HEARTBEAT_INTERVAL_SECS * TTL_MULTIPLIER_U64;
pub const PUBLISHER_TTL_SECS: i64 = HEARTBEAT_INTERVAL_SECS_I64 * TTL_MULTIPLIER_I64;

/// Default timeout for Redis operations (5 seconds).
/// This prevents indefinite blocking on Redis server issues or network problems.
const REDIS_OPERATION_TIMEOUT_SECS: u64 = 5;

/// Redis key for the global lease_epoch counter used for fencing tokens.
/// Format: "`stream:lease_epoch:{room_id}:{media_id`}"
/// Each publisher registration increments this counter atomically.
const LEASE_EPOCH_KEY_PREFIX: &str = "stream:lease_epoch";
const ACTIVE_GENERATION_KEY_PREFIX: &str = "stream:active_generation";
const GENERATION_KEY_PREFIX: &str = "stream:generation";
pub(crate) const HLS_GENERATION_RETENTION: Duration = DEFAULT_HLS_GENERATION_RETENTION;
const USER_ACTIVE_STREAMS_KEY_PREFIX: &str = "stream:user_active_streams";
const NODE_ACTIVE_STREAMS_KEY_PREFIX: &str = "stream:node_active_streams";

#[derive(Debug, thiserror::Error)]
#[error("Redis operation timed out after {timeout_secs}s")]
pub(crate) struct RedisOperationTimeout {
    timeout_secs: u64,
}

impl RedisOperationTimeout {
    pub(crate) const fn new(timeout_secs: u64) -> Self {
        Self { timeout_secs }
    }
}
const ROOM_ACTIVE_STREAMS_KEY_PREFIX: &str = "stream:room_active_streams";
const ACTIVE_STREAMS_KEY: &str = "stream:active_streams";
const ACTIVE_PUBLISHER_FETCH_BATCH_SIZE: usize = 128;
static REGISTER_PUBLISHER_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"
        local lease_epoch_key = KEYS[1]
        local active_generation_key = KEYS[2]
        local generation_key = KEYS[3]
        local generation_id = ARGV[1]
        local info_json_template = ARGV[2]
        local ttl = tonumber(ARGV[3])
        local user_key = ARGV[4]
        local user_member = ARGV[5]
        local node_key = ARGV[6]
        local node_member = ARGV[7]
        local room_key = ARGV[8]
        local room_member = ARGV[9]
        local active_streams_key = ARGV[10]
        local active_member = ARGV[11]

        local reserved = redis.call(
            'SET', active_generation_key, generation_id, 'NX', 'EX', ttl
        )
        if not reserved then
            local current_epoch = redis.call('GET', lease_epoch_key)
            return {0, tonumber(current_epoch) or 0}
        end

        local lease_epoch = redis.call('INCR', lease_epoch_key)
        local parsed = cjson.decode(info_json_template)
        parsed.lease_epoch = lease_epoch
        local info_json = cjson.encode(parsed)
        redis.call('SET', generation_key, info_json, 'EX', ttl)

        -- Add to user reverse index if provided
        if user_key ~= '' then
            redis.call('SADD', user_key, user_member)
            redis.call('EXPIRE', user_key, ttl)
        end

        if node_key ~= '' then
            redis.call('SADD', node_key, node_member)
            redis.call('EXPIRE', node_key, ttl)
        end

        if room_key ~= '' then
            redis.call('SADD', room_key, room_member)
            redis.call('EXPIRE', room_key, ttl)
        end

        if active_streams_key ~= '' then
            redis.call('SADD', active_streams_key, active_member)
            redis.call('EXPIRE', active_streams_key, ttl)
        end

        return {1, lease_epoch}
        ",
    )
});

static REFRESH_GENERATION_LEASE_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"
        local active_generation_key = KEYS[1]
        local generation_key = KEYS[2]
        local user_key = KEYS[3]
        local node_key = KEYS[4]
        local room_key = KEYS[5]
        local active_streams_key = KEYS[6]
        local expected_generation_id = ARGV[1]
        local ttl = tonumber(ARGV[2])
        local expected_user_id = ARGV[3]
        local expected_node_id = ARGV[4]
        local expected_lease_epoch = tonumber(ARGV[5])
        local member = ARGV[6]

        local active_generation_id = redis.call('GET', active_generation_key)
        if not active_generation_id then
            return 0
        end
        if active_generation_id ~= expected_generation_id then
            return -1
        end

        local info_json = redis.call('GET', generation_key)
        if not info_json then
            return 0
        end

        local ok, parsed = pcall(cjson.decode, info_json)
        if not ok or not parsed then
            return 0
        end

        local stored_user_id = parsed.user_id or ''
        if expected_user_id ~= '' and stored_user_id ~= expected_user_id then
            return -1
        end

        local stored_node_id = parsed.node_id or ''
        if expected_node_id ~= '' and stored_node_id ~= expected_node_id then
            return -1
        end

        local stored_epoch = tonumber(parsed.lease_epoch or 0)
        if expected_lease_epoch >= 0 and stored_epoch ~= expected_lease_epoch then
            return -1
        end

        redis.call('EXPIRE', active_generation_key, ttl)
        redis.call('EXPIRE', generation_key, ttl)

        if user_key ~= '' then
            redis.call('SADD', user_key, member)
            redis.call('EXPIRE', user_key, ttl)
        end

        if node_key ~= '' then
            redis.call('SADD', node_key, member)
            redis.call('EXPIRE', node_key, ttl)
        end

        redis.call('SADD', room_key, member)
        redis.call('EXPIRE', room_key, ttl)
        redis.call('SADD', active_streams_key, member)
        redis.call('EXPIRE', active_streams_key, ttl)

        return 1
        ",
    )
});

static DEACTIVATE_GENERATION_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"
        local active_generation_key = KEYS[1]
        local generation_key = KEYS[2]
        local expected_generation_id = ARGV[1]
        local expected_lease_epoch = tonumber(ARGV[2])
        local retain_generation = tonumber(ARGV[3])
        local retention_ttl = tonumber(ARGV[4])
        local ended_at = ARGV[5]
        local user_key_prefix = ARGV[6]
        local node_key_prefix = ARGV[7]
        local room_key = ARGV[8]
        local active_streams_key = ARGV[9]
        local member = ARGV[10]

        local function remove_reverse_indexes(parsed)
            if parsed then
                local user_id = parsed.user_id or ''
                local node_id = parsed.node_id or ''
                if user_id ~= '' then
                    redis.call('SREM', user_key_prefix .. ':' .. user_id, member)
                end
                if node_id ~= '' then
                    redis.call('SREM', node_key_prefix .. ':' .. node_id, member)
                end
            end
            redis.call('SREM', room_key, member)
            redis.call('SREM', active_streams_key, member)
        end

        local active_generation_id = redis.call('GET', active_generation_key)
        if not active_generation_id then
            return 0
        end
        if active_generation_id ~= expected_generation_id then
            return -1
        end

        local info_json = redis.call('GET', generation_key)
        if not info_json then
            remove_reverse_indexes(nil)
            redis.call('DEL', active_generation_key)
            return 0
        end

        local ok, parsed = pcall(cjson.decode, info_json)
        if not ok or not parsed then
            remove_reverse_indexes(nil)
            redis.call('DEL', active_generation_key)
            redis.call('DEL', generation_key)
            return 1
        end

        local stored_epoch = tonumber(parsed.lease_epoch)
        if stored_epoch ~= expected_lease_epoch then
            return -1
        end

        remove_reverse_indexes(parsed)
        redis.call('DEL', active_generation_key)
        if retain_generation == 1 then
            parsed.ended_at = ended_at
            redis.call('SET', generation_key, cjson.encode(parsed), 'EX', retention_ttl)
        else
            redis.call('DEL', generation_key)
        end

        return 1
        ",
    )
});

static GET_ACTIVE_GENERATION_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"
        local generation_id = redis.call('GET', KEYS[1])
        if not generation_id then
            return nil
        end
        local generation = redis.call('GET', ARGV[1] .. generation_id)
        if not generation then
            redis.call('DEL', KEYS[1])
            return nil
        end
        return generation
        ",
    )
});

static GET_INDEXED_ACTIVE_GENERATION_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"
        local active_generation_key = KEYS[1]
        local index_key = KEYS[2]
        local generation_key_prefix = ARGV[1]
        local member = ARGV[2]
        local expected_user_id = ARGV[3]
        local expected_node_id = ARGV[4]

        local generation_id = redis.call('GET', active_generation_key)
        if not generation_id then
            redis.call('SREM', index_key, member)
            return nil
        end

        local info_json = redis.call('GET', generation_key_prefix .. generation_id)
        if not info_json then
            redis.call('SREM', index_key, member)
            return nil
        end

        local ok, parsed = pcall(cjson.decode, info_json)
        if not ok or not parsed or parsed.generation_id ~= generation_id then
            redis.call('SREM', index_key, member)
            return nil
        end
        if expected_user_id ~= '' and (parsed.user_id or '') ~= expected_user_id then
            redis.call('SREM', index_key, member)
            return nil
        end
        if expected_node_id ~= '' and (parsed.node_id or '') ~= expected_node_id then
            redis.call('SREM', index_key, member)
            return nil
        end

        return info_json
        ",
    )
});

#[async_trait]
pub trait RegistryConnectionRuntime: Send + Sync {
    async fn snapshot(&self) -> redis::RedisResult<RedisConnectionManager>;
}

/// Helper function to wrap async Redis operations with a timeout.
/// Returns an error if the operation exceeds the specified duration.
async fn with_redis_timeout<T, F, Fut>(future: F) -> Result<T>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    match tokio::time::timeout(Duration::from_secs(REDIS_OPERATION_TIMEOUT_SECS), future()).await {
        Ok(result) => result,
        Err(_) => Err(RedisOperationTimeout::new(REDIS_OPERATION_TIMEOUT_SECS).into()),
    }
}

// Compile-time safety check: TTL must be at least 3x the heartbeat interval
// to tolerate transient network issues.
const _: () = assert!(
    PUBLISHER_TTL_SECS_U64 >= HEARTBEAT_INTERVAL_SECS * 3,
    "PUBLISHER_TTL_SECS must be at least 3x HEARTBEAT_INTERVAL_SECS"
);

/// Publisher information stored in Redis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamGeneration {
    /// Node ID of the publisher
    pub node_id: String,
    /// Dedicated cluster listener address of the publisher node
    /// (e.g., "10.0.0.1:50051").
    /// Used by pull streams to connect to internal relay services over gRPC.
    ///
    /// **Must not be empty** when the publisher is used for cross-node proxying.
    /// Use [`StreamGeneration::validate_cluster_address`] before connecting.
    pub cluster_address: String,
    /// RTMP app name
    pub app_name: String,
    /// User ID of the publisher (for reverse-index lookups)
    #[serde(default)]
    pub user_id: String,
    /// When the stream started
    pub started_at: DateTime<Utc>,
    /// When this generation released the active stream slot.
    pub ended_at: Option<DateTime<Utc>>,
    /// Fencing token (monotonically increasing lease_epoch) for split-brain prevention
    /// When a new publisher registers (after TTL expiry), this counter increments.
    /// Pull streams must validate their token matches to prevent stale connections.
    #[serde(default)]
    pub lease_epoch: u64,
    /// Stable StreamHub publication generation used by public HLS URLs.
    pub generation_id: String,
}

impl StreamGeneration {
    /// Validate that `cluster_address` is set and non-empty.
    ///
    /// Returns `Err` if the address is empty, which would happen if the publisher
    /// registered without configuring its advertised cluster address.
    pub fn validate_cluster_address(&self) -> Result<&str> {
        if self.cluster_address.trim().is_empty() {
            return Err(anyhow!(
                "StreamGeneration for node={} has empty cluster_address (room/media stream cannot be proxied)",
                self.node_id
            ));
        }
        Ok(&self.cluster_address)
    }
}

/// Publisher Registry for tracking active publishers via Redis.
///
/// **Role**: Publisher Ownership -- enforces single-publisher-per-media and provides
/// publisher discovery for cross-node gRPC relay. Used by the livestream layer to:
/// 1. Atomically register a publisher (prevents duplicate publishers for the same media)
/// 2. Look up the publisher's cluster address for cross-node relay
/// 3. Manage publisher TTL via heartbeat for crash detection
///
/// **Distinction from realtime room/connection state**:
/// - This registry tracks *publisher ownership* (who is publishing, on which node,
///   with what cluster address, at what lease_epoch) using `room_id/media_id` keys.
/// - Realtime room/connection state tracks websocket presence and fan-out
///   delivery, not livestream publisher ownership.
/// - Both use Redis; this one is Redis-only (no local cache) because publisher
///   ownership must always be authoritative from Redis.
#[derive(Clone)]
pub(crate) struct StreamRegistry {
    redis: Arc<dyn RegistryConnectionRuntime>,
    key_prefix: String,
}

impl StreamRegistry {
    /// Create a new stream registry from an abstract Redis runtime provider.
    #[must_use]
    pub(crate) fn from_runtime(
        redis: Arc<dyn RegistryConnectionRuntime>,
        key_prefix: impl Into<String>,
    ) -> Self {
        Self {
            redis,
            key_prefix: key_prefix.into(),
        }
    }

    async fn conn(&self) -> Result<RedisConnectionManager> {
        self.redis.snapshot().await.map_err(Into::into)
    }

    fn prefixed(&self, key: &str) -> String {
        format!("{}{}", self.key_prefix, key)
    }

    fn active_generation_key(&self, room_id: &str, media_id: &str) -> String {
        self.prefixed(&format!(
            "{ACTIVE_GENERATION_KEY_PREFIX}:{room_id}:{media_id}"
        ))
    }

    fn lease_epoch_key(&self, room_id: &str, media_id: &str) -> String {
        self.prefixed(&format!("{LEASE_EPOCH_KEY_PREFIX}:{room_id}:{media_id}"))
    }

    fn generation_key(&self, room_id: &str, media_id: &str, generation_id: &str) -> String {
        self.prefixed(&format!(
            "{GENERATION_KEY_PREFIX}:{room_id}:{media_id}:{generation_id}"
        ))
    }

    fn generation_key_prefix(&self, room_id: &str, media_id: &str) -> String {
        self.prefixed(&format!("{GENERATION_KEY_PREFIX}:{room_id}:{media_id}:"))
    }

    fn user_publishers_key(&self, user_id: &str) -> String {
        self.prefixed(&format!("{USER_ACTIVE_STREAMS_KEY_PREFIX}:{user_id}"))
    }

    fn node_publishers_key(&self, node_id: &str) -> String {
        self.prefixed(&format!("{NODE_ACTIVE_STREAMS_KEY_PREFIX}:{node_id}"))
    }

    fn room_publishers_key(&self, room_id: &str) -> String {
        self.prefixed(&format!("{ROOM_ACTIVE_STREAMS_KEY_PREFIX}:{room_id}"))
    }

    fn active_publishers_key(&self) -> String {
        self.prefixed(ACTIVE_STREAMS_KEY)
    }

    fn publisher_member(room_id: &str, media_id: &str) -> String {
        format!("{room_id}:{media_id}")
    }

    fn parse_publisher_member(member: &str) -> Option<(String, String)> {
        let (room_id, media_id) = member.split_once(':')?;
        if media_id.contains(':') || validate_stream_ids(room_id, media_id).is_err() {
            return None;
        }
        Some((room_id.to_string(), media_id.to_string()))
    }

    async fn load_index_members(&self, set_key: &str) -> Result<Vec<String>> {
        with_redis_timeout(|| async {
            let mut conn = self.conn().await?;
            let members: Vec<String> = redis::cmd("SMEMBERS")
                .arg(set_key)
                .query_async(&mut conn)
                .await
                .map_err(|e| anyhow!(e.to_string()))?;
            Ok(members)
        })
        .await
    }

    async fn prune_index_members(&self, set_key: &str, members: &[String]) -> Result<()> {
        if members.is_empty() {
            return Ok(());
        }

        with_redis_timeout(|| async {
            let mut conn = self.conn().await?;
            let mut pipeline = redis::pipe();
            for member in members {
                pipeline.cmd("SREM").arg(set_key).arg(member);
            }
            pipeline
                .query_async::<()>(&mut conn)
                .await
                .map_err(|e| anyhow!(e.to_string()))?;
            Ok(())
        })
        .await
    }

    async fn load_publishers_from_index(
        &self,
        set_key: &str,
        expected_user_id: Option<&str>,
        expected_node_id: Option<&str>,
    ) -> Result<Vec<ActiveStreamGeneration>> {
        let members = self.load_index_members(set_key).await?;
        if members.is_empty() {
            return Ok(Vec::new());
        }

        let mut parsed_members = Vec::with_capacity(members.len());
        let mut stale_members = Vec::new();
        for member in members {
            match Self::parse_publisher_member(&member) {
                Some((room_id, media_id)) => parsed_members.push((member, room_id, media_id)),
                None => stale_members.push(member),
            }
        }

        if !stale_members.is_empty() {
            self.prune_index_members(set_key, &stale_members).await?;
        }

        let mut publishers = Vec::with_capacity(parsed_members.len());
        let expected_user_id = expected_user_id.unwrap_or_default();
        let expected_node_id = expected_node_id.unwrap_or_default();

        for chunk in parsed_members.chunks(ACTIVE_PUBLISHER_FETCH_BATCH_SIZE) {
            let generation_jsons: Vec<Option<String>> = with_redis_timeout(|| async {
                let mut conn = self.conn().await?;
                let mut pipeline = redis::pipe();
                pipeline
                    .load_script(&GET_INDEXED_ACTIVE_GENERATION_SCRIPT)
                    .ignore();
                for (member, room_id, media_id) in chunk {
                    let mut invocation = GET_INDEXED_ACTIVE_GENERATION_SCRIPT.prepare_invoke();
                    invocation
                        .key(self.active_generation_key(room_id, media_id))
                        .key(set_key)
                        .arg(self.generation_key_prefix(room_id, media_id))
                        .arg(member)
                        .arg(expected_user_id)
                        .arg(expected_node_id);
                    pipeline.invoke_script(&invocation);
                }
                pipeline
                    .query_async(&mut conn)
                    .await
                    .map_err(|e| anyhow!(e.to_string()))
            })
            .await?;

            for ((_member, room_id, media_id), generation_json) in
                chunk.iter().zip(generation_jsons)
            {
                let Some(generation_json) = generation_json else {
                    continue;
                };

                match serde_json::from_str::<StreamGeneration>(&generation_json) {
                    Ok(generation) => publishers.push(ActiveStreamGeneration {
                        room_id: room_id.clone(),
                        media_id: media_id.clone(),
                        generation,
                    }),
                    Err(error) => {
                        debug!(
                            set_key = %set_key,
                            room_id = %room_id,
                            media_id = %media_id,
                            error = %error,
                            "Failed to deserialize publisher info from indexed lookup"
                        );
                    }
                }
            }
        }

        Ok(publishers)
    }

    /// Try to register as publisher with `user_id`
    /// Returns true if registered successfully, false if already exists
    ///
    /// Uses an atomic Lua script to prevent lease_epoch races.
    /// The script ensures INCR + HSETNX are atomic; if HSETNX fails, lease_epoch is rolled back.
    ///
    /// # Errors
    ///
    /// Returns an error if `cluster_address` is empty, as cross-node proxying requires
    /// a valid cluster listener address.
    pub async fn try_activate_generation_with_user(
        &self,
        room_id: &str,
        media_id: &str,
        node_id: &str,
        user_id: &str,
        cluster_address: &str,
        generation_id: &str,
    ) -> anyhow::Result<bool> {
        validate_stream_ids(room_id, media_id)?;
        validate_stream_generation_id(generation_id)?;
        validate_publisher_cluster_address(cluster_address, node_id, room_id, media_id)?;

        let active_generation_key = self.active_generation_key(room_id, media_id);
        let generation_key = self.generation_key(room_id, media_id, generation_id);
        let lease_epoch_key = self.lease_epoch_key(room_id, media_id);
        let mut conn = self.conn().await?;

        // Create StreamGeneration template (lease_epoch will be filled by Lua script)
        let info = StreamGeneration {
            node_id: node_id.to_string(),
            cluster_address: cluster_address.to_string(),
            app_name: "live".to_string(),
            user_id: user_id.to_string(),
            started_at: synctv_core::SystemClock.now(),
            ended_at: None,
            lease_epoch: 0, // Placeholder, will be replaced by actual lease_epoch in Lua script
            generation_id: generation_id.to_string(),
        };
        let info_json = serde_json::to_string(&info)?;

        let user_key = if user_id.is_empty() {
            String::new()
        } else {
            self.user_publishers_key(user_id)
        };
        let user_member = Self::publisher_member(room_id, media_id);
        let node_key = if node_id.is_empty() {
            String::new()
        } else {
            self.node_publishers_key(node_id)
        };
        let node_member = user_member.clone();
        let room_key = self.room_publishers_key(room_id);
        let active_key = self.active_publishers_key();

        // Add timeout for Redis Lua script execution (5 seconds)
        // Prevents indefinite blocking on Redis server issues or slow Lua execution
        let result: Vec<i64> = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            REGISTER_PUBLISHER_SCRIPT
                .key(&lease_epoch_key)
                .key(&active_generation_key)
                .key(&generation_key)
                .arg(generation_id)
                .arg(&info_json)
                .arg(PUBLISHER_TTL_SECS)
                .arg(&user_key)
                .arg(&user_member)
                .arg(&node_key)
                .arg(&node_member)
                .arg(&room_key)
                .arg(&user_member)
                .arg(&active_key)
                .arg(&user_member)
                .invoke_async(&mut conn)
                .await
        })
        .await
        .map_err(|_| anyhow!("Lua script execution timed out after 5s"))?
        .map_err(|e| anyhow!("Lua script execution failed: {e}"))?;

        let registered = result[0] == 1;
        let actual_epoch = u64::try_from(result[1])
            .map_err(|_| anyhow!("Lua script returned invalid lease_epoch: {}", result[1]))?;

        if registered {
            info!(
                "Publisher registered atomically: room={}, media={}, node={}, lease_epoch={}",
                room_id, media_id, node_id, actual_epoch
            );
        } else {
            debug!(
                "Publisher already exists: room={}, media={}, attempted_epoch={}",
                room_id, media_id, actual_epoch
            );
        }

        // Note: User reverse index (user_publishers) is already handled atomically
        // by the Lua script above, no additional Redis calls needed

        Ok(registered)
    }

    /// Refresh TTL for a publisher plus its user/node reverse indexes.
    pub async fn refresh_generation_lease_with_owner(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        user_id: &str,
        node_id: &str,
        expected_lease_epoch: Option<u64>,
    ) -> Result<LeaseRefreshOutcome> {
        validate_stream_ids(room_id, media_id)?;
        validate_stream_generation_id(generation_id)?;
        let active_generation_key = self.active_generation_key(room_id, media_id);
        let generation_key = self.generation_key(room_id, media_id, generation_id);
        let user_key = if user_id.is_empty() {
            String::new()
        } else {
            self.user_publishers_key(user_id)
        };
        let node_key = if node_id.is_empty() {
            String::new()
        } else {
            self.node_publishers_key(node_id)
        };
        let member = Self::publisher_member(room_id, media_id);
        let room_key = self.room_publishers_key(room_id);
        let active_key = self.active_publishers_key();
        let epoch_arg = expected_lease_epoch.map_or(-1_i64, |lease_epoch| {
            i64::try_from(lease_epoch).unwrap_or(i64::MAX)
        });

        with_redis_timeout(|| async {
            let mut conn = self.conn().await?;

            let status: i64 = REFRESH_GENERATION_LEASE_SCRIPT
                .key(&active_generation_key)
                .key(&generation_key)
                .key(&user_key)
                .key(&node_key)
                .key(&room_key)
                .key(&active_key)
                .arg(generation_id)
                .arg(PUBLISHER_TTL_SECS)
                .arg(user_id)
                .arg(node_id)
                .arg(epoch_arg)
                .arg(&member)
                .invoke_async(&mut conn)
                .await
                .map_err(anyhow::Error::from)?;

            Ok(match status {
                1 => LeaseRefreshOutcome::Refreshed,
                -1 => LeaseRefreshOutcome::OwnershipChanged,
                _ => LeaseRefreshOutcome::Missing,
            })
        })
        .await
    }

    /// Refresh a bounded batch of publisher leases in one Redis round trip.
    pub async fn refresh_generation_leases(
        &self,
        node_id: &str,
        requests: &[LeaseRefreshRequest],
    ) -> Result<Vec<LeaseRefreshOutcome>> {
        anyhow::ensure!(
            requests.len() <= PUBLISHER_REFRESH_BATCH_SIZE,
            "publisher refresh batch contains {} entries; maximum is {}",
            requests.len(),
            PUBLISHER_REFRESH_BATCH_SIZE
        );
        if requests.is_empty() {
            return Ok(Vec::new());
        }

        for request in requests {
            validate_stream_ids(&request.room_id, &request.media_id)?;
            validate_stream_generation_id(&request.generation_id)?;
        }

        with_redis_timeout(|| async {
            let mut conn = self.conn().await?;
            let mut pipeline = redis::pipe();
            pipeline
                .load_script(&REFRESH_GENERATION_LEASE_SCRIPT)
                .ignore();
            let node_key = if node_id.is_empty() {
                String::new()
            } else {
                self.node_publishers_key(node_id)
            };
            let active_key = self.active_publishers_key();

            for request in requests {
                let active_generation_key =
                    self.active_generation_key(&request.room_id, &request.media_id);
                let generation_key = self.generation_key(
                    &request.room_id,
                    &request.media_id,
                    &request.generation_id,
                );
                let user_key = if request.user_id.is_empty() {
                    String::new()
                } else {
                    self.user_publishers_key(&request.user_id)
                };
                let room_key = self.room_publishers_key(&request.room_id);
                let member = Self::publisher_member(&request.room_id, &request.media_id);
                let epoch_arg = i64::try_from(request.expected_lease_epoch).unwrap_or(i64::MAX);
                let mut invocation = REFRESH_GENERATION_LEASE_SCRIPT.prepare_invoke();
                invocation
                    .key(active_generation_key)
                    .key(generation_key)
                    .key(user_key)
                    .key(&node_key)
                    .key(room_key)
                    .key(&active_key)
                    .arg(&request.generation_id)
                    .arg(PUBLISHER_TTL_SECS)
                    .arg(&request.user_id)
                    .arg(node_id)
                    .arg(epoch_arg)
                    .arg(member);
                pipeline.invoke_script(&invocation);
            }

            let statuses: Vec<i64> = pipeline
                .query_async(&mut conn)
                .await
                .map_err(anyhow::Error::from)?;
            anyhow::ensure!(
                statuses.len() == requests.len(),
                "publisher refresh pipeline returned {} statuses for {} requests",
                statuses.len(),
                requests.len()
            );

            Ok(statuses
                .into_iter()
                .map(|status| match status {
                    1 => LeaseRefreshOutcome::Refreshed,
                    -1 => LeaseRefreshOutcome::OwnershipChanged,
                    _ => LeaseRefreshOutcome::Missing,
                })
                .collect())
        })
        .await
    }

    /// Unregister a publisher.
    pub async fn deactivate_current_generation(&self, room_id: &str, media_id: &str) -> Result<()> {
        let Some(generation) = self.get_active_generation(room_id, media_id).await? else {
            return Ok(());
        };
        self.deactivate_generation_with_lease(
            room_id,
            media_id,
            &generation.generation_id,
            generation.lease_epoch,
            false,
        )
        .await
    }

    /// Release a publisher and retain its route while the final HLS generation is readable.
    pub async fn deactivate_generation_with_hls_grace(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        expected_lease_epoch: u64,
    ) -> Result<()> {
        self.deactivate_generation_with_lease(
            room_id,
            media_id,
            generation_id,
            expected_lease_epoch,
            true,
        )
        .await
    }

    /// Generation- and lease-validated deactivation.
    pub async fn deactivate_generation_with_lease(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        expected_lease_epoch: u64,
        retain_generation: bool,
    ) -> Result<()> {
        validate_stream_ids(room_id, media_id)?;
        validate_stream_generation_id(generation_id)?;
        let active_generation_key = self.active_generation_key(room_id, media_id);
        let generation_key = self.generation_key(room_id, media_id, generation_id);
        let lease_epoch = i64::try_from(expected_lease_epoch)
            .map_err(|_| anyhow!("Lease epoch {expected_lease_epoch} exceeds Redis range"))?;
        let retention_ttl = i64::try_from(HLS_GENERATION_RETENTION.as_secs())
            .map_err(|_| anyhow!("HLS generation retention exceeds Redis range"))?;
        let ended_at = synctv_core::SystemClock.now().to_rfc3339();
        let user_key_prefix = self.prefixed(USER_ACTIVE_STREAMS_KEY_PREFIX);
        let node_key_prefix = self.prefixed(NODE_ACTIVE_STREAMS_KEY_PREFIX);
        let room_key = self.room_publishers_key(room_id);
        let active_key = self.active_publishers_key();
        let member = Self::publisher_member(room_id, media_id);

        with_redis_timeout(|| async {
            let mut conn = self.conn().await?;
            let status: i64 = DEACTIVATE_GENERATION_SCRIPT
                .key(&active_generation_key)
                .key(&generation_key)
                .arg(generation_id)
                .arg(lease_epoch)
                .arg(i64::from(retain_generation))
                .arg(retention_ttl)
                .arg(&ended_at)
                .arg(&user_key_prefix)
                .arg(&node_key_prefix)
                .arg(&room_key)
                .arg(&active_key)
                .arg(&member)
                .invoke_async(&mut conn)
                .await
                .map_err(|e| anyhow!("Generation deactivation script failed: {e}"))?;

            if status == -1 {
                info!(
                    "Skipped generation deactivation for room={}, media={}, generation={}: ownership changed",
                    room_id, media_id, generation_id
                );
            }

            Ok(())
        }).await
    }

    /// Get all active publishers for a user (via reverse index)
    /// Returns list of (`room_id`, `media_id`) pairs
    pub async fn get_user_publishers(&self, user_id: &str) -> Result<Vec<(String, String)>> {
        Ok(self
            .load_publishers_from_index(&self.user_publishers_key(user_id), Some(user_id), None)
            .await?
            .into_iter()
            .map(|entry| (entry.room_id, entry.media_id))
            .collect())
    }

    /// Get active publishers for a user in one room.
    pub async fn get_user_publishers_for_room(
        &self,
        room_id: &str,
        user_id: &str,
    ) -> Result<Vec<(String, String)>> {
        validate_stream_id_component(room_id, "room_id")?;
        let publishers = self.get_user_publishers(user_id).await?;
        Ok(publishers
            .into_iter()
            .filter(|(publisher_room_id, _)| publisher_room_id == room_id)
            .collect())
    }

    /// Get publisher info for a media in a room.
    pub async fn get_active_generation(
        &self,
        room_id: &str,
        media_id: &str,
    ) -> Result<Option<StreamGeneration>> {
        validate_stream_ids(room_id, media_id)?;
        let active_generation_key = self.active_generation_key(room_id, media_id);
        let generation_key_prefix = self.generation_key_prefix(room_id, media_id);

        with_redis_timeout(|| async {
            let mut conn = self.conn().await?;
            let info_json: Option<String> = GET_ACTIVE_GENERATION_SCRIPT
                .key(&active_generation_key)
                .arg(&generation_key_prefix)
                .invoke_async(&mut conn)
                .await
                .map_err(|e| anyhow!(e.to_string()))?;

            match info_json {
                Some(json) => {
                    let info: StreamGeneration = serde_json::from_str(&json)?;
                    Ok(Some(info))
                }
                None => Ok(None),
            }
        })
        .await
    }

    /// Get one exact active or retained stream generation.
    pub async fn get_generation(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
    ) -> Result<Option<StreamGeneration>> {
        validate_stream_ids(room_id, media_id)?;
        validate_stream_generation_id(generation_id)?;
        let generation_key = self.generation_key(room_id, media_id, generation_id);

        with_redis_timeout(|| async {
            let mut conn = self.conn().await?;
            let route_json: Option<String> = redis::cmd("GET")
                .arg(&generation_key)
                .query_async(&mut conn)
                .await
                .map_err(|error| anyhow!(error.to_string()))?;

            route_json
                .map(|json| serde_json::from_str(&json).map_err(anyhow::Error::from))
                .transpose()
        })
        .await
    }

    /// Check if a stream is active (has a publisher).
    pub async fn is_stream_active(&self, room_id: &str, media_id: &str) -> anyhow::Result<bool> {
        validate_stream_ids(room_id, media_id)?;
        let key = self.active_generation_key(room_id, media_id);

        with_redis_timeout(|| async {
            let mut conn = self.conn().await?;
            let exists: bool = redis::cmd("EXISTS")
                .arg(&key)
                .query_async(&mut conn)
                .await
                .map_err(|e| anyhow!(e.to_string()))?;
            Ok(exists)
        })
        .await
    }

    pub async fn list_active_generations(&self) -> Result<Vec<ActiveStreamGeneration>> {
        self.load_publishers_from_index(&self.active_publishers_key(), None, None)
            .await
    }

    /// List active streams for a specific room, returning only the `media_id` values.
    pub async fn list_streams_for_room(&self, room_id: &str) -> Result<Vec<String>> {
        crate::util::validate_stream_id_component(room_id, "room_id")?;
        Ok(self
            .load_publishers_from_index(&self.room_publishers_key(room_id), None, None)
            .await?
            .into_iter()
            .map(|entry| entry.media_id)
            .collect())
    }

    /// Clean up publisher registrations tracked by the node reverse index.
    /// Epoch validation protects newer publishers for the same stream.
    pub async fn cleanup_all_generations_for_node(&self, node_id: &str) -> Result<()> {
        if node_id.is_empty() {
            return Ok(());
        }

        let node_key = self.node_publishers_key(node_id);
        let generations = self
            .load_publishers_from_index(&node_key, None, Some(node_id))
            .await?;
        for active in generations {
            self.deactivate_generation_with_lease(
                &active.room_id,
                &active.media_id,
                &active.generation.generation_id,
                active.generation.lease_epoch,
                false,
            )
            .await?;
        }

        // Re-check after fenced deactivation so a concurrent owner change is
        // reflected atomically in the node index.
        self.load_publishers_from_index(&node_key, None, Some(node_id))
            .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::TEST_GENERATION_ID;
    use synctv_core_testing::{
        start_redis_client_manager_with_label, test_redis_key_prefix, RedisContainer,
    };
    use tokio::sync::RwLock;

    async fn setup_redis() -> (
        RedisContainer,
        redis::Client,
        RedisConnectionManager,
        String,
    ) {
        let (container, client, manager) =
            start_redis_client_manager_with_label("livestream-registry").await;
        (
            container,
            client,
            manager,
            test_redis_key_prefix("livestream-registry"),
        )
    }

    type TestResult = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn test_error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
        anyhow::anyhow!(message.into()).into()
    }

    fn require_publisher(
        publisher: Option<StreamGeneration>,
    ) -> std::result::Result<StreamGeneration, Box<dyn std::error::Error + Send + Sync>> {
        publisher.ok_or_else(|| test_error("publisher should exist"))
    }

    struct TestRegistryConnectionRuntime {
        redis: RwLock<RedisConnectionManager>,
    }

    #[async_trait]
    impl RegistryConnectionRuntime for TestRegistryConnectionRuntime {
        async fn snapshot(&self) -> redis::RedisResult<RedisConnectionManager> {
            Ok(self.redis.read().await.clone())
        }
    }

    fn test_registry(redis: RedisConnectionManager, prefix: impl Into<String>) -> StreamRegistry {
        StreamRegistry::from_runtime(
            Arc::new(TestRegistryConnectionRuntime {
                redis: RwLock::new(redis),
            }),
            prefix,
        )
    }

    struct SharedTestRegistryConnectionRuntime {
        redis: Arc<RwLock<RedisConnectionManager>>,
    }

    #[async_trait]
    impl RegistryConnectionRuntime for SharedTestRegistryConnectionRuntime {
        async fn snapshot(&self) -> redis::RedisResult<RedisConnectionManager> {
            Ok(self.redis.read().await.clone())
        }
    }

    fn shared_test_registry(
        redis: Arc<RwLock<RedisConnectionManager>>,
        prefix: impl Into<String>,
    ) -> StreamRegistry {
        StreamRegistry::from_runtime(
            Arc::new(SharedTestRegistryConnectionRuntime { redis }),
            prefix,
        )
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_register_publisher_success() -> TestResult {
        let (_container, _client, redis, prefix) = setup_redis().await;
        let registry = test_registry(redis, prefix);

        let registered = registry
            .try_activate_generation_with_user(
                "room123",
                "media456",
                "node1",
                "user1",
                "localhost:50051",
                TEST_GENERATION_ID,
            )
            .await?;
        assert!(registered);

        let publisher = registry
            .get_active_generation("room123", "media456")
            .await?;
        let pub_info = require_publisher(publisher)?;
        assert_eq!(pub_info.node_id, "node1");
        assert_eq!(pub_info.cluster_address, "localhost:50051");

        registry
            .deactivate_current_generation("room123", "media456")
            .await?;
        Ok(())
    }

    #[test]
    fn test_parse_publisher_member_rejects_ambiguous_components() {
        assert_eq!(
            StreamRegistry::parse_publisher_member("room1:media1"),
            Some(("room1".to_string(), "media1".to_string()))
        );
        assert!(
            StreamRegistry::parse_publisher_member("room:1:media").is_none(),
            "member parser must reject extra delimiters"
        );
        assert!(
            StreamRegistry::parse_publisher_member("room1:../media").is_none(),
            "member parser must reject path-like media ids"
        );
        assert!(
            StreamRegistry::parse_publisher_member("room/1:media").is_none(),
            "member parser must reject path-like room ids"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_key_prefix_isolation_prevents_cross_instance_pollution() -> TestResult {
        use redis::AsyncCommands;

        let (_container, client, redis, prefix) = setup_redis().await;
        let registry = test_registry(redis, prefix.clone());

        registry
            .try_activate_generation_with_user(
                "room123",
                "media456",
                "node1",
                "user1",
                "localhost:50051",
                TEST_GENERATION_ID,
            )
            .await?;

        let mut verify_conn = RedisConnectionManager::new(client).await?;
        let namespaced_active_exists: bool = verify_conn
            .exists(registry.active_generation_key("room123", "media456"))
            .await?;
        let namespaced_generation_exists: bool = verify_conn
            .exists(registry.generation_key("room123", "media456", TEST_GENERATION_ID))
            .await?;
        let unprefixed_exists: bool = verify_conn
            .exists("stream:active_generation:room123:media456")
            .await?;

        assert!(
            namespaced_active_exists && namespaced_generation_exists,
            "active and generation keys must honor the configured key prefix"
        );
        assert!(
            !unprefixed_exists,
            "registry must not leak publisher keys into the global Redis namespace"
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_refresh_generation_lease_repairs_missing_reverse_indexes() -> TestResult {
        use redis::AsyncCommands;

        let (_container, client, redis, prefix) = setup_redis().await;
        let registry = test_registry(redis, prefix);

        registry
            .try_activate_generation_with_user(
                "room1",
                "media1",
                "node1",
                "user1",
                "localhost:50051",
                TEST_GENERATION_ID,
            )
            .await?;

        let publisher =
            require_publisher(registry.get_active_generation("room1", "media1").await?)?;

        let member = StreamRegistry::publisher_member("room1", "media1");
        let user_key = registry.user_publishers_key("user1");
        let node_key = registry.node_publishers_key("node1");
        let room_key = registry.room_publishers_key("room1");
        let active_key = registry.active_publishers_key();

        let mut conn = client.get_multiplexed_async_connection().await?;
        let _: () = conn.srem(&user_key, &member).await?;
        let _: () = conn.srem(&node_key, &member).await?;
        let _: () = conn.srem(&room_key, &member).await?;
        let _: () = conn.srem(&active_key, &member).await?;

        let outcome = registry
            .refresh_generation_lease_with_owner(
                "room1",
                "media1",
                TEST_GENERATION_ID,
                "user1",
                "node1",
                Some(publisher.lease_epoch),
            )
            .await?;
        assert_eq!(outcome, LeaseRefreshOutcome::Refreshed);

        let user_indexed: bool = conn.sismember(&user_key, &member).await?;
        let node_indexed: bool = conn.sismember(&node_key, &member).await?;
        let room_indexed: bool = conn.sismember(&room_key, &member).await?;
        let active_indexed: bool = conn.sismember(&active_key, &member).await?;

        assert!(
            user_indexed,
            "refresh must restore missing user reverse index"
        );
        assert!(
            node_indexed,
            "refresh must restore missing node reverse index"
        );
        assert!(
            room_indexed,
            "refresh must restore missing room reverse index"
        );
        assert!(
            active_indexed,
            "refresh must restore missing global active index"
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_batch_refresh_preserves_outcome_order() -> TestResult {
        let (_container, _client, redis, prefix) = setup_redis().await;
        let registry = test_registry(redis, prefix);

        registry
            .try_activate_generation_with_user(
                "room1",
                "media1",
                "node1",
                "user1",
                "localhost:50051",
                TEST_GENERATION_ID,
            )
            .await?;
        registry
            .try_activate_generation_with_user(
                "room2",
                "media2",
                "node1",
                "user2",
                "localhost:50051",
                TEST_GENERATION_ID,
            )
            .await?;

        let first = require_publisher(registry.get_active_generation("room1", "media1").await?)?;
        let second = require_publisher(registry.get_active_generation("room2", "media2").await?)?;
        let requests = vec![
            LeaseRefreshRequest {
                room_id: "room1".to_string(),
                media_id: "media1".to_string(),
                generation_id: first.generation_id.clone(),
                user_id: "user1".to_string(),
                expected_lease_epoch: first.lease_epoch,
            },
            LeaseRefreshRequest {
                room_id: "room2".to_string(),
                media_id: "media2".to_string(),
                generation_id: second.generation_id.clone(),
                user_id: "user2".to_string(),
                expected_lease_epoch: second.lease_epoch.saturating_add(1),
            },
            LeaseRefreshRequest {
                room_id: "room3".to_string(),
                media_id: "media3".to_string(),
                generation_id: TEST_GENERATION_ID.to_string(),
                user_id: "user3".to_string(),
                expected_lease_epoch: 1,
            },
        ];

        assert_eq!(
            registry
                .refresh_generation_leases("node1", &requests)
                .await?,
            vec![
                LeaseRefreshOutcome::Refreshed,
                LeaseRefreshOutcome::OwnershipChanged,
                LeaseRefreshOutcome::Missing,
            ]
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_shared_redis_handle_hot_swap_keeps_registry_operational() -> TestResult {
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let (_container, client, redis, prefix) = setup_redis().await;
        let shared = Arc::new(RwLock::new(redis));
        let registry = shared_test_registry(shared.clone(), prefix);

        let registered = registry
            .try_activate_generation_with_user(
                "room1",
                "media1",
                "node1",
                "user1",
                "localhost:50051",
                TEST_GENERATION_ID,
            )
            .await?;
        assert!(registered);

        let replacement = RedisConnectionManager::new(client.clone()).await?;
        *shared.write().await = replacement;

        let publisher =
            require_publisher(registry.get_active_generation("room1", "media1").await?)?;
        assert_eq!(publisher.node_id, "node1");

        registry
            .deactivate_current_generation("room1", "media1")
            .await?;
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_register_publisher_duplicate() -> TestResult {
        let (_container, _client, redis, prefix) = setup_redis().await;
        let registry = test_registry(redis, prefix);

        let registered = registry
            .try_activate_generation_with_user(
                "room123",
                "media456",
                "node1",
                "user1",
                "localhost:50051",
                TEST_GENERATION_ID,
            )
            .await?;
        assert!(registered);

        let registered = registry
            .try_activate_generation_with_user(
                "room123",
                "media456",
                "node2",
                "user2",
                "localhost:50052",
                TEST_GENERATION_ID,
            )
            .await?;
        assert!(!registered);

        registry
            .deactivate_current_generation("room123", "media456")
            .await?;
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_try_activate_generation() -> TestResult {
        let (_container, _client, redis, prefix) = setup_redis().await;
        let registry = test_registry(redis, prefix);

        let result = registry
            .try_activate_generation_with_user(
                "room123",
                "media456",
                "node1",
                "user1",
                "localhost:50051",
                TEST_GENERATION_ID,
            )
            .await?;
        assert!(result);

        let result = registry
            .try_activate_generation_with_user(
                "room123",
                "media456",
                "node2",
                "user2",
                "localhost:50052",
                TEST_GENERATION_ID,
            )
            .await?;
        assert!(!result);

        registry
            .deactivate_current_generation("room123", "media456")
            .await?;
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_deactivate_current_generation() -> TestResult {
        let (_container, _client, redis, prefix) = setup_redis().await;
        let registry = test_registry(redis, prefix);

        registry
            .try_activate_generation_with_user(
                "room123",
                "media456",
                "node1",
                "user1",
                "localhost:50051",
                TEST_GENERATION_ID,
            )
            .await?;

        assert!(registry.is_stream_active("room123", "media456").await?);

        registry
            .deactivate_current_generation("room123", "media456")
            .await?;

        assert!(!registry.is_stream_active("room123", "media456").await?);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn replacement_keeps_the_ended_generation_addressable() -> TestResult {
        const REPLACEMENT_GENERATION_ID: &str = "00000000-0000-4000-8000-000000000002";

        let (_container, client, redis, prefix) = setup_redis().await;
        let registry = test_registry(redis, prefix);

        assert!(
            registry
                .try_activate_generation_with_user(
                    "room-route",
                    "media-route",
                    "node-a",
                    "user-a",
                    "127.0.0.1:50051",
                    TEST_GENERATION_ID,
                )
                .await?
        );
        let first = require_publisher(
            registry
                .get_active_generation("room-route", "media-route")
                .await?,
        )?;

        registry
            .deactivate_generation_with_hls_grace(
                "room-route",
                "media-route",
                TEST_GENERATION_ID,
                first.lease_epoch,
            )
            .await?;
        assert!(registry
            .get_active_generation("room-route", "media-route")
            .await?
            .is_none());
        let retained_generation = require_publisher(
            registry
                .get_generation("room-route", "media-route", TEST_GENERATION_ID)
                .await?,
        )?;
        assert_eq!(retained_generation.node_id, "node-a");
        assert_eq!(retained_generation.lease_epoch, first.lease_epoch);
        assert_eq!(retained_generation.generation_id, TEST_GENERATION_ID);

        assert!(
            registry
                .try_activate_generation_with_user(
                    "room-route",
                    "media-route",
                    "node-b",
                    "user-b",
                    "127.0.0.1:50052",
                    REPLACEMENT_GENERATION_ID,
                )
                .await?
        );
        let replacement = require_publisher(
            registry
                .get_active_generation("room-route", "media-route")
                .await?,
        )?;
        assert_eq!(replacement.node_id, "node-b");
        assert!(replacement.lease_epoch > first.lease_epoch);
        assert_eq!(replacement.generation_id, REPLACEMENT_GENERATION_ID);
        assert_eq!(
            registry.get_user_publishers("user-b").await?,
            vec![("room-route".to_string(), "media-route".to_string())]
        );

        let first_generation = require_publisher(
            registry
                .get_generation("room-route", "media-route", TEST_GENERATION_ID)
                .await?,
        )?;
        assert_eq!(first_generation.node_id, "node-a");
        assert_eq!(first_generation.lease_epoch, first.lease_epoch);

        let mut verify_conn = RedisConnectionManager::new(client).await?;
        let retained_generation_json: Option<String> = redis::cmd("GET")
            .arg(registry.generation_key("room-route", "media-route", TEST_GENERATION_ID))
            .query_async(&mut verify_conn)
            .await?;
        assert!(retained_generation_json.is_some());

        registry
            .deactivate_current_generation("room-route", "media-route")
            .await?;
        let retained = require_publisher(
            registry
                .get_generation("room-route", "media-route", TEST_GENERATION_ID)
                .await?,
        )?;
        assert_eq!(retained.lease_epoch, first.lease_epoch);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_active_generation_not_found() -> TestResult {
        let (_container, _client, redis, prefix) = setup_redis().await;
        let registry = test_registry(redis, prefix);

        let result = registry
            .get_active_generation("nonexistent", "media")
            .await?;
        assert!(result.is_none());
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_deactivate_current_generation_cleans_node_reverse_index() -> TestResult {
        let (_container, client, redis, prefix) = setup_redis().await;
        let registry = test_registry(redis, prefix);

        registry
            .try_activate_generation_with_user(
                "room1",
                "media1",
                "node1",
                "user1",
                "localhost:50051",
                TEST_GENERATION_ID,
            )
            .await?;

        registry
            .deactivate_current_generation("room1", "media1")
            .await?;

        let mut verify_conn = RedisConnectionManager::new(client).await?;
        let members: Vec<String> = redis::cmd("SMEMBERS")
            .arg(registry.node_publishers_key("node1"))
            .query_async(&mut verify_conn)
            .await?;
        assert!(
            members.is_empty(),
            "node reverse index should be empty after unregister"
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn user_index_does_not_resolve_a_replacement_owned_by_another_user() -> TestResult {
        const REPLACEMENT_GENERATION_ID: &str = "00000000-0000-4000-8000-000000000002";

        let (_container, client, redis, prefix) = setup_redis().await;
        let registry = test_registry(redis, prefix);
        registry
            .try_activate_generation_with_user(
                "room1",
                "media1",
                "node1",
                "user1",
                "localhost:50051",
                TEST_GENERATION_ID,
            )
            .await?;
        registry
            .deactivate_current_generation("room1", "media1")
            .await?;
        registry
            .try_activate_generation_with_user(
                "room1",
                "media1",
                "node2",
                "user2",
                "localhost:50052",
                REPLACEMENT_GENERATION_ID,
            )
            .await?;

        let member = StreamRegistry::publisher_member("room1", "media1");
        let stale_user_key = registry.user_publishers_key("user1");
        let mut verify_conn = RedisConnectionManager::new(client).await?;
        let _: () = redis::cmd("SADD")
            .arg(&stale_user_key)
            .arg(&member)
            .query_async(&mut verify_conn)
            .await?;

        assert!(registry.get_user_publishers("user1").await?.is_empty());
        assert_eq!(
            registry.get_user_publishers("user2").await?,
            vec![("room1".to_string(), "media1".to_string())]
        );
        let stale_member_remains: bool = redis::cmd("SISMEMBER")
            .arg(stale_user_key)
            .arg(member)
            .query_async(&mut verify_conn)
            .await?;
        assert!(!stale_member_remains);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_cleanup_all_generations_for_node_prunes_stale_reverse_index_members() -> TestResult
    {
        let (_container, client, redis, prefix) = setup_redis().await;
        let registry = test_registry(redis, prefix);

        registry
            .try_activate_generation_with_user(
                "room1",
                "media1",
                "node1",
                "user1",
                "localhost:50051",
                TEST_GENERATION_ID,
            )
            .await?;
        registry
            .try_activate_generation_with_user(
                "room2",
                "media2",
                "node2",
                "user2",
                "localhost:50052",
                TEST_GENERATION_ID,
            )
            .await?;

        let mut verify_conn = RedisConnectionManager::new(client).await?;
        let _: () = redis::cmd("SADD")
            .arg(registry.node_publishers_key("node1"))
            .arg("room2:media2")
            .query_async(&mut verify_conn)
            .await?;

        registry.cleanup_all_generations_for_node("node1").await?;

        assert!(!registry.is_stream_active("room1", "media1").await?);
        assert!(registry.is_stream_active("room2", "media2").await?);

        let members: Vec<String> = redis::cmd("SMEMBERS")
            .arg(registry.node_publishers_key("node1"))
            .query_async(&mut verify_conn)
            .await?;
        assert!(
            members.is_empty(),
            "cleanup should prune stale node reverse-index members"
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_list_queries_prune_stale_active_room_and_user_indexes() -> TestResult {
        let (_container, client, redis, prefix) = setup_redis().await;
        let registry = test_registry(redis, prefix);

        registry
            .try_activate_generation_with_user(
                "room1",
                "media1",
                "node1",
                "user1",
                "localhost:50051",
                TEST_GENERATION_ID,
            )
            .await?;

        let mut verify_conn = RedisConnectionManager::new(client).await?;
        let stale_member = "room1:media-stale";
        let invalid_member = "malformed-member";

        let _: () = redis::cmd("SADD")
            .arg(registry.active_publishers_key())
            .arg(stale_member)
            .arg(invalid_member)
            .query_async(&mut verify_conn)
            .await?;
        let _: () = redis::cmd("SADD")
            .arg(registry.room_publishers_key("room1"))
            .arg(stale_member)
            .arg(invalid_member)
            .query_async(&mut verify_conn)
            .await?;
        let _: () = redis::cmd("SADD")
            .arg(registry.user_publishers_key("user1"))
            .arg(stale_member)
            .arg(invalid_member)
            .query_async(&mut verify_conn)
            .await?;

        let active = registry.list_active_generations().await?;
        assert_eq!(active.len(), 1, "stale active index members must be pruned");
        assert_eq!(active[0].room_id, "room1");
        assert_eq!(active[0].media_id, "media1");

        let room_streams = registry.list_streams_for_room("room1").await?;
        assert_eq!(
            room_streams,
            vec!["media1".to_string()],
            "stale room index members must be pruned"
        );
        let user_publishers = registry.get_user_publishers("user1").await?;
        assert_eq!(
            user_publishers,
            vec![("room1".to_string(), "media1".to_string())],
            "stale user index members must be pruned"
        );

        let active_members: Vec<String> = redis::cmd("SMEMBERS")
            .arg(registry.active_publishers_key())
            .query_async(&mut verify_conn)
            .await?;
        let room_members: Vec<String> = redis::cmd("SMEMBERS")
            .arg(registry.room_publishers_key("room1"))
            .query_async(&mut verify_conn)
            .await?;
        let user_members: Vec<String> = redis::cmd("SMEMBERS")
            .arg(registry.user_publishers_key("user1"))
            .query_async(&mut verify_conn)
            .await?;

        assert_eq!(
            active_members,
            vec!["room1:media1".to_string()],
            "active publisher index should retain only valid members"
        );
        assert_eq!(
            room_members,
            vec!["room1:media1".to_string()],
            "room publisher index should retain only valid members"
        );
        assert_eq!(
            user_members,
            vec!["room1:media1".to_string()],
            "user publisher index should retain only valid members"
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_publisher_info_serialization() -> TestResult {
        let (_container, _client, redis, prefix) = setup_redis().await;
        let registry = test_registry(redis, prefix);

        registry
            .try_activate_generation_with_user(
                "room123",
                "media456",
                "node1",
                "user1",
                "localhost:50051",
                TEST_GENERATION_ID,
            )
            .await?;

        let publisher = require_publisher(
            registry
                .get_active_generation("room123", "media456")
                .await?,
        )?;

        assert_eq!(publisher.node_id, "node1");
        assert_eq!(publisher.cluster_address, "localhost:50051");
        assert!(publisher.started_at <= synctv_core::SystemClock.now());

        registry
            .deactivate_current_generation("room123", "media456")
            .await?;
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn test_with_redis_timeout_returns_timeout_error() {
        let future = with_redis_timeout(|| async {
            tokio::time::sleep(Duration::from_secs(REDIS_OPERATION_TIMEOUT_SECS + 1)).await;
            Ok::<(), anyhow::Error>(())
        });

        tokio::time::advance(Duration::from_secs(REDIS_OPERATION_TIMEOUT_SECS + 1)).await;
        let err = future.await.expect_err("slow Redis op should time out");
        assert!(
            err.to_string().contains("timed out"),
            "timeout error should mention timeout: {err}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_with_redis_timeout_does_not_block_fast_operations() -> TestResult {
        let fast = with_redis_timeout(|| async { Ok::<u32, anyhow::Error>(7) });
        let slow = with_redis_timeout(|| async {
            tokio::time::sleep(Duration::from_secs(REDIS_OPERATION_TIMEOUT_SECS + 1)).await;
            Ok::<u32, anyhow::Error>(9)
        });

        tokio::time::advance(Duration::from_secs(REDIS_OPERATION_TIMEOUT_SECS + 1)).await;
        let (fast_result, slow_result) = tokio::join!(fast, slow);

        assert_eq!(fast_result?, 7);
        assert!(slow_result.is_err(), "slow operation should time out");
        Ok(())
    }
}
