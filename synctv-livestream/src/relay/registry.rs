use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use redis::aio::ConnectionManager as RedisConnectionManager;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tracing::{debug, info};

use super::registry_trait::{ActivePublisherEntry, PublisherRefreshOutcome};
use crate::util::{validate_stream_id_component, validate_stream_ids};

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

/// Redis key for the global epoch counter used for fencing tokens.
/// Format: "`stream:epoch:{room_id}:{media_id`}"
/// Each publisher registration increments this counter atomically.
const EPOCH_KEY_PREFIX: &str = "stream:epoch";
const PUBLISHER_KEY_PREFIX: &str = "stream:publisher";
const USER_PUBLISHERS_KEY_PREFIX: &str = "stream:user_publishers";
const NODE_PUBLISHERS_KEY_PREFIX: &str = "stream:node_publishers";

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
const ROOM_PUBLISHERS_KEY_PREFIX: &str = "stream:room_publishers";
const ACTIVE_PUBLISHERS_KEY: &str = "stream:active_publishers";
const ACTIVE_PUBLISHER_FETCH_BATCH_SIZE: usize = 128;

static REGISTER_PUBLISHER_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"
        local epoch_key = KEYS[1]
        local hash_key = KEYS[2]
        local info_json_template = ARGV[1]
        local ttl = tonumber(ARGV[2])
        local user_key = ARGV[3]
        local user_member = ARGV[4]
        local node_key = ARGV[5]
        local node_member = ARGV[6]
        local room_key = ARGV[7]
        local room_member = ARGV[8]
        local active_key = ARGV[9]
        local active_member = ARGV[10]

        -- Check HSETNX FIRST before touching the epoch.
        -- Use a placeholder JSON with epoch=0 for the initial slot reservation.
        -- Try to reserve the publisher slot (HSETNX returns 1 if set, 0 if exists)
        local reserved = redis.call('HSETNX', hash_key, 'publisher', info_json_template)

        if reserved == 0 then
            -- Slot already taken: another publisher is active.
            -- Read current epoch for the caller's information (no modification).
            local current_epoch = redis.call('GET', epoch_key)
            return {0, tonumber(current_epoch) or 0}
        end

        -- Slot reserved: now increment epoch atomically.
        -- Only now does the epoch change, so other nodes never see a spurious increment.
        local epoch = redis.call('INCR', epoch_key)

        -- Set the actual epoch via cjson (robust, unlike fragile string.gsub).
        local parsed = cjson.decode(info_json_template)
        parsed.epoch = epoch
        local info_json = cjson.encode(parsed)

        -- Overwrite the placeholder entry with the fully-populated JSON.
        -- HSET (not HSETNX) because we already own the slot.
        redis.call('HSET', hash_key, 'publisher', info_json)

        -- Set TTL on the publisher hash
        redis.call('EXPIRE', hash_key, ttl)

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

        if active_key ~= '' then
            redis.call('SADD', active_key, active_member)
            redis.call('EXPIRE', active_key, ttl)
        end

        return {1, epoch}
        ",
    )
});

static REFRESH_PUBLISHER_TTL_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"
        local hash_key = KEYS[1]
        local user_key = KEYS[2]
        local node_key = KEYS[3]
        local room_key = KEYS[4]
        local active_key = KEYS[5]
        local ttl = tonumber(ARGV[1])
        local expected_user_id = ARGV[2]
        local expected_node_id = ARGV[3]
        local expected_epoch = tonumber(ARGV[4])
        local member = ARGV[5]

        local info_json = redis.call('HGET', hash_key, 'publisher')
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

        local stored_epoch = tonumber(parsed.epoch or 0)
        if expected_epoch >= 0 and stored_epoch ~= expected_epoch then
            return -1
        end

        redis.call('EXPIRE', hash_key, ttl)

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
        redis.call('SADD', active_key, member)
        redis.call('EXPIRE', active_key, ttl)

        return 1
        ",
    )
});

static UNREGISTER_PUBLISHER_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"
        local hash_key = KEYS[1]
        local check_epoch = tonumber(ARGV[1])

        -- Get current publisher info
        local info_json = redis.call('HGET', hash_key, 'publisher')
        if not info_json then
            return {0, '', ''}
        end

        -- Parse JSON robustly using cjson instead of fragile regex
        local ok, parsed = pcall(cjson.decode, info_json)
        if not ok or not parsed then
            -- JSON is corrupt; delete the entry but return empty reverse-index metadata
            redis.call('HDEL', hash_key, 'publisher')
            return {1, '', ''}
        end

        -- If epoch check is requested, validate before deleting
        if check_epoch >= 0 then
            local stored_epoch = tonumber(parsed.epoch)
            if stored_epoch and stored_epoch ~= check_epoch then
                -- Epoch mismatch: a newer publisher registered, do NOT delete
                return {-1, '', ''}
            end
        end

        -- Extract user_id for reverse-index cleanup
        local user_id = parsed.user_id or ''
        local node_id = parsed.node_id or ''

        -- Delete the publisher entry
        redis.call('HDEL', hash_key, 'publisher')

        return {1, user_id, node_id}
        ",
    )
});

static CLEANUP_NODE_PUBLISHER_SCRIPT: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"
        local hash_key = KEYS[1]
        local expected_node_id = ARGV[1]
        local expected_epoch = tonumber(ARGV[2])

        local info_json = redis.call('HGET', hash_key, 'publisher')
        if not info_json then
            return {0, ''}
        end

        -- Parse JSON robustly using cjson instead of fragile regex
        local ok, parsed = pcall(cjson.decode, info_json)
        if not ok or not parsed then
            -- JSON is corrupt; delete the entry but return empty user_id
            redis.call('HDEL', hash_key, 'publisher')
            return {1, ''}
        end

        -- Verify node_id matches
        local stored_node_id = parsed.node_id
        if not stored_node_id or stored_node_id ~= expected_node_id then
            return {0, ''}
        end

        -- Verify epoch matches (a newer registration would have a higher epoch)
        local stored_epoch = tonumber(parsed.epoch)
        if stored_epoch and stored_epoch ~= expected_epoch then
            return {-1, ''}
        end

        -- Extract user_id for reverse-index cleanup
        local user_id = parsed.user_id or ''

        -- Delete the publisher entry
        redis.call('HDEL', hash_key, 'publisher')

        return {1, user_id}
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
pub struct PublisherInfo {
    /// Node ID of the publisher
    pub node_id: String,
    /// Shared API address of the publisher node (e.g., "10.0.0.1:8080").
    /// Used by pull streams to connect to the publisher over gRPC on the
    /// shared single-port listener.
    ///
    /// **Must not be empty** when the publisher is used for cross-node proxying.
    /// Use [`PublisherInfo::validate_api_address`] before connecting.
    #[serde(default)]
    pub api_address: String,
    /// RTMP app name
    pub app_name: String,
    /// User ID of the publisher (for reverse-index lookups)
    #[serde(default)]
    pub user_id: String,
    /// When the stream started
    pub started_at: DateTime<Utc>,
    /// Fencing token (monotonically increasing epoch) for split-brain prevention
    /// When a new publisher registers (after TTL expiry), this counter increments.
    /// Pull streams must validate their token matches to prevent stale connections.
    #[serde(default)]
    pub epoch: u64,
}

impl PublisherInfo {
    /// Validate that `api_address` is set and non-empty.
    ///
    /// Returns `Err` if the address is empty, which would happen if the publisher
    /// registered without configuring its shared API listen address
    /// (misconfiguration).
    pub fn validate_api_address(&self) -> Result<&str> {
        if self.api_address.trim().is_empty() {
            return Err(anyhow!(
                "PublisherInfo for node={} has empty api_address (room/media stream cannot be proxied)",
                self.node_id
            ));
        }
        Ok(&self.api_address)
    }
}

/// Publisher Registry for tracking active publishers via Redis.
///
/// **Role**: Publisher Ownership -- enforces single-publisher-per-media and provides
/// publisher discovery for cross-node gRPC relay. Used by the livestream layer to:
/// 1. Atomically register a publisher (prevents duplicate publishers for the same media)
/// 2. Look up the publisher's node/API address for cross-node relay
/// 3. Manage publisher TTL via heartbeat for crash detection
///
/// **Distinction from realtime room/connection state**:
/// - This registry tracks *publisher ownership* (who is publishing, on which node,
///   with what API address, at what epoch) using `room_id/media_id` keys.
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

    fn publisher_key(&self, room_id: &str, media_id: &str) -> String {
        self.prefixed(&format!("{PUBLISHER_KEY_PREFIX}:{room_id}:{media_id}"))
    }

    fn epoch_key(&self, room_id: &str, media_id: &str) -> String {
        self.prefixed(&format!("{EPOCH_KEY_PREFIX}:{room_id}:{media_id}"))
    }

    fn user_publishers_key(&self, user_id: &str) -> String {
        self.prefixed(&format!("{USER_PUBLISHERS_KEY_PREFIX}:{user_id}"))
    }

    fn node_publishers_key(&self, node_id: &str) -> String {
        self.prefixed(&format!("{NODE_PUBLISHERS_KEY_PREFIX}:{node_id}"))
    }

    fn room_publishers_key(&self, room_id: &str) -> String {
        self.prefixed(&format!("{ROOM_PUBLISHERS_KEY_PREFIX}:{room_id}"))
    }

    fn active_publishers_key(&self) -> String {
        self.prefixed(ACTIVE_PUBLISHERS_KEY)
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

    async fn remove_reverse_index_member(&self, set_key: &str, member: &str) -> Result<()> {
        let mut conn = self.conn().await?;
        let _: () = redis::cmd("SREM")
            .arg(set_key)
            .arg(member)
            .query_async(&mut conn)
            .await
            .map_err(|e| anyhow!(e.to_string()))?;
        Ok(())
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

    async fn load_publishers_from_index(&self, set_key: &str) -> Result<Vec<ActivePublisherEntry>> {
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
        let mut missing_publishers = Vec::new();

        for chunk in parsed_members.chunks(ACTIVE_PUBLISHER_FETCH_BATCH_SIZE) {
            let publisher_jsons: Vec<Option<String>> = with_redis_timeout(|| async {
                let mut conn = self.conn().await?;
                let mut pipeline = redis::pipe();
                for (_, room_id, media_id) in chunk {
                    pipeline
                        .cmd("HGET")
                        .arg(self.publisher_key(room_id, media_id))
                        .arg("publisher");
                }
                pipeline
                    .query_async(&mut conn)
                    .await
                    .map_err(|e| anyhow!(e.to_string()))
            })
            .await?;

            for ((member, room_id, media_id), publisher_json) in chunk.iter().zip(publisher_jsons) {
                let Some(publisher_json) = publisher_json else {
                    missing_publishers.push(member.clone());
                    continue;
                };

                match serde_json::from_str::<PublisherInfo>(&publisher_json) {
                    Ok(publisher) => publishers.push(ActivePublisherEntry {
                        room_id: room_id.clone(),
                        media_id: media_id.clone(),
                        publisher,
                    }),
                    Err(error) => {
                        debug!(
                            set_key = %set_key,
                            room_id = %room_id,
                            media_id = %media_id,
                            error = %error,
                            "Failed to deserialize publisher info from indexed lookup; pruning stale index member"
                        );
                        missing_publishers.push(member.clone());
                    }
                }
            }
        }

        if !missing_publishers.is_empty() {
            self.prune_index_members(set_key, &missing_publishers)
                .await?;
        }

        Ok(publishers)
    }

    /// Try to register as publisher with `user_id`
    /// Returns true if registered successfully, false if already exists
    ///
    /// Uses an atomic Lua script to prevent epoch races.
    /// The script ensures INCR + HSETNX are atomic; if HSETNX fails, epoch is rolled back.
    ///
    /// # Errors
    ///
    /// Returns an error if `api_address` is empty, as cross-node proxying requires
    /// a valid shared API address.
    pub async fn try_register_publisher_with_user(
        &self,
        room_id: &str,
        media_id: &str,
        node_id: &str,
        user_id: &str,
        api_address: &str,
    ) -> anyhow::Result<bool> {
        validate_stream_ids(room_id, media_id)?;
        // Validate api_address at registration time (not usage time)
        // This ensures publishers cannot register without a valid shared API address.
        if api_address.trim().is_empty() {
            return Err(anyhow!(
                "Cannot register publisher for node={node_id} with empty api_address (room={room_id}, media={media_id})"
            ));
        }

        let key = self.publisher_key(room_id, media_id);
        let epoch_key = self.epoch_key(room_id, media_id);
        let mut conn = self.conn().await?;

        // Create PublisherInfo template (epoch will be filled by Lua script)
        let info = PublisherInfo {
            node_id: node_id.to_string(),
            api_address: api_address.to_string(),
            app_name: "live".to_string(),
            user_id: user_id.to_string(),
            started_at: Utc::now(),
            epoch: 0, // Placeholder, will be replaced by actual epoch in Lua script
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
                .key(&epoch_key)
                .key(&key)
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
            .map_err(|_| anyhow!("Lua script returned invalid epoch: {}", result[1]))?;

        if registered {
            info!(
                "Publisher registered atomically: room={}, media={}, node={}, epoch={}",
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
    pub async fn refresh_publisher_ttl_with_owner(
        &self,
        room_id: &str,
        media_id: &str,
        user_id: &str,
        node_id: &str,
        expected_epoch: Option<u64>,
    ) -> Result<PublisherRefreshOutcome> {
        validate_stream_ids(room_id, media_id)?;
        let key = self.publisher_key(room_id, media_id);
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
        let epoch_arg =
            expected_epoch.map_or(-1_i64, |epoch| i64::try_from(epoch).unwrap_or(i64::MAX));

        with_redis_timeout(|| async {
            let mut conn = self.conn().await?;

            let status: i64 = REFRESH_PUBLISHER_TTL_SCRIPT
                .key(&key)
                .key(&user_key)
                .key(&node_key)
                .key(&room_key)
                .key(&active_key)
                .arg(PUBLISHER_TTL_SECS)
                .arg(user_id)
                .arg(node_id)
                .arg(epoch_arg)
                .arg(&member)
                .invoke_async(&mut conn)
                .await
                .map_err(anyhow::Error::from)?;

            Ok(match status {
                1 => PublisherRefreshOutcome::Refreshed,
                -1 => PublisherRefreshOutcome::OwnershipChanged,
                _ => PublisherRefreshOutcome::Missing,
            })
        })
        .await
    }

    /// Unregister a publisher.
    pub async fn unregister_publisher(&self, room_id: &str, media_id: &str) -> Result<()> {
        self.unregister_publisher_with_epoch(room_id, media_id, None)
            .await
    }

    /// Epoch-validated unregister: only deletes if the stored epoch matches the expected epoch.
    /// If `expected_epoch` is None, deletes unconditionally.
    ///
    /// This prevents a race where publisher A dies, publisher B registers, then
    /// publisher A's delayed cleanup incorrectly removes publisher B's entry.
    pub async fn unregister_publisher_with_epoch(
        &self,
        room_id: &str,
        media_id: &str,
        expected_epoch: Option<u64>,
    ) -> Result<()> {
        validate_stream_ids(room_id, media_id)?;
        let key = self.publisher_key(room_id, media_id);

        // Use -1 to mean "no epoch check" (unconditional delete)
        let epoch_arg: i64 = match expected_epoch {
            Some(e) => {
                i64::try_from(e).map_err(|_| anyhow!("Epoch {e} exceeds Redis Lua i64 range"))?
            }
            None => -1,
        };

        with_redis_timeout(|| async {
            let mut conn = self.conn().await?;

            let result: Vec<redis::Value> = UNREGISTER_PUBLISHER_SCRIPT
                .key(&key)
                .arg(epoch_arg)
                .invoke_async(&mut conn)
                .await
                .map_err(|e| anyhow!("Unregister Lua script failed: {e}"))?;

            // Parse result: [status, user_id, node_id]
            let status = match &result[0] {
                redis::Value::Int(v) => *v,
                _ => 0,
            };
            let user_id = match &result[1] {
                redis::Value::BulkString(s) => String::from_utf8_lossy(s).to_string(),
                redis::Value::SimpleString(s) => s.clone(),
                _ => String::new(),
            };
            let node_id = match result.get(2) {
                Some(redis::Value::BulkString(s)) => String::from_utf8_lossy(s).to_string(),
                Some(redis::Value::SimpleString(s)) => s.clone(),
                _ => String::new(),
            };

            if status == -1 {
                info!(
                    "Skipped unregister for room={}, media={}: epoch mismatch (newer publisher exists)",
                    room_id, media_id
                );
                return Ok(());
            }

            if status == 1 {
                let member = Self::publisher_member(room_id, media_id);
                let room_key = self.room_publishers_key(room_id);
                let active_key = self.active_publishers_key();

                if !user_id.is_empty() {
                    let user_key = self.user_publishers_key(&user_id);
                    let _: () = redis::cmd("SREM")
                        .arg(&user_key)
                        .arg(&member)
                        .query_async(&mut conn)
                        .await
                        .map_err(|e| anyhow!(e.to_string()))?;
                }

                if !node_id.is_empty() {
                    let node_key = self.node_publishers_key(&node_id);
                    let _: () = redis::cmd("SREM")
                        .arg(&node_key)
                        .arg(&member)
                        .query_async(&mut conn)
                        .await
                        .map_err(|e| anyhow!(e.to_string()))?;
                }

                let _: () = redis::cmd("SREM")
                    .arg(&room_key)
                    .arg(&member)
                    .query_async(&mut conn)
                    .await
                    .map_err(|e| anyhow!(e.to_string()))?;
                let _: () = redis::cmd("SREM")
                    .arg(&active_key)
                    .arg(&member)
                    .query_async(&mut conn)
                    .await
                    .map_err(|e| anyhow!(e.to_string()))?;
            }

            Ok(())
        }).await
    }

    /// Get all active publishers for a user (via reverse index)
    /// Returns list of (`room_id`, `media_id`) pairs
    pub async fn get_user_publishers(&self, user_id: &str) -> Result<Vec<(String, String)>> {
        Ok(self
            .load_publishers_from_index(&self.user_publishers_key(user_id))
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
    pub async fn get_publisher(
        &self,
        room_id: &str,
        media_id: &str,
    ) -> Result<Option<PublisherInfo>> {
        validate_stream_ids(room_id, media_id)?;
        let key = self.publisher_key(room_id, media_id);

        with_redis_timeout(|| async {
            let mut conn = self.conn().await?;
            let info_json: Option<String> = redis::cmd("HGET")
                .arg(&key)
                .arg("publisher")
                .query_async(&mut conn)
                .await
                .map_err(|e| anyhow!(e.to_string()))?;

            match info_json {
                Some(json) => {
                    let info: PublisherInfo = serde_json::from_str(&json)?;
                    Ok(Some(info))
                }
                None => Ok(None),
            }
        })
        .await
    }

    /// Check if a stream is active (has a publisher).
    pub async fn is_stream_active(&self, room_id: &str, media_id: &str) -> anyhow::Result<bool> {
        validate_stream_ids(room_id, media_id)?;
        let key = self.publisher_key(room_id, media_id);

        with_redis_timeout(|| async {
            let mut conn = self.conn().await?;
            let exists: bool = redis::cmd("HEXISTS")
                .arg(&key)
                .arg("publisher")
                .query_async(&mut conn)
                .await
                .map_err(|e| anyhow!(e.to_string()))?;
            Ok(exists)
        })
        .await
    }

    pub async fn list_active_publishers(&self) -> Result<Vec<ActivePublisherEntry>> {
        self.load_publishers_from_index(&self.active_publishers_key())
            .await
    }

    /// List active streams for a specific room, returning only the `media_id` values.
    pub async fn list_streams_for_room(&self, room_id: &str) -> Result<Vec<String>> {
        crate::util::validate_stream_id_component(room_id, "room_id")?;
        Ok(self
            .load_publishers_from_index(&self.room_publishers_key(room_id))
            .await?
            .into_iter()
            .map(|entry| entry.media_id)
            .collect())
    }

    /// Validate that the given epoch matches the current publisher's epoch.
    /// Returns Ok(true) if the epoch is valid, Ok(false) if stale/invalid.
    /// Used by pull streams to detect split-brain scenarios.
    pub async fn validate_epoch(&self, room_id: &str, media_id: &str, epoch: u64) -> Result<bool> {
        validate_stream_ids(room_id, media_id)?;
        let key = self.publisher_key(room_id, media_id);

        with_redis_timeout(|| async {
            let mut conn = self.conn().await?;

            // Get current publisher info
            let info_json: Option<String> = redis::cmd("HGET")
                .arg(&key)
                .arg("publisher")
                .query_async(&mut conn)
                .await
                .map_err(|e| anyhow!(e.to_string()))?;

            match info_json {
                Some(json) => {
                    let info: PublisherInfo = serde_json::from_str(&json)?;
                    // Epoch is valid if it matches the current publisher's epoch
                    Ok(info.epoch == epoch)
                }
                None => {
                    // Publisher no longer exists, epoch is invalid
                    Ok(false)
                }
            }
        })
        .await
    }

    /// Clean up publisher registrations tracked by the node reverse index.
    /// Epoch validation protects newer publishers for the same stream.
    pub async fn cleanup_all_publishers_for_node(&self, node_id: &str) -> Result<()> {
        if node_id.is_empty() {
            return Ok(());
        }

        let members = self
            .load_index_members(&self.node_publishers_key(node_id))
            .await?;

        self.cleanup_publisher_members_for_node(node_id, members)
            .await
    }

    async fn cleanup_publisher_members_for_node(
        &self,
        node_id: &str,
        members: Vec<String>,
    ) -> Result<()> {
        let node_key = self.node_publishers_key(node_id);
        for chunk in members.chunks(ACTIVE_PUBLISHER_FETCH_BATCH_SIZE) {
            let mut entries = Vec::with_capacity(chunk.len());
            for member in chunk {
                match Self::parse_publisher_member(member) {
                    Some((room_id, media_id)) => entries.push((
                        member.clone(),
                        room_id.clone(),
                        media_id.clone(),
                        self.publisher_key(&room_id, &media_id),
                    )),
                    None => {
                        self.remove_reverse_index_member(&node_key, member).await?;
                    }
                }
            }

            if entries.is_empty() {
                continue;
            }

            let publisher_jsons: Vec<Option<String>> = with_redis_timeout(|| async {
                let mut conn = self.conn().await?;
                let mut pipeline = redis::pipe();
                for (_, _, _, publisher_key) in &entries {
                    pipeline.cmd("HGET").arg(publisher_key).arg("publisher");
                }
                pipeline
                    .query_async(&mut conn)
                    .await
                    .map_err(|e| anyhow!(e.to_string()))
            })
            .await?;

            for ((member, room_id, media_id, publisher_key), publisher_json) in
                entries.into_iter().zip(publisher_jsons)
            {
                let Some(publisher_json) = publisher_json else {
                    self.remove_reverse_index_member(&node_key, &member).await?;
                    continue;
                };

                let info = match serde_json::from_str::<PublisherInfo>(&publisher_json) {
                    Ok(info) => info,
                    Err(error) => {
                        debug!(
                            node_id = %node_id,
                            room_id = %room_id,
                            media_id = %media_id,
                            error = %error,
                            "Failed to parse publisher info during node cleanup; pruning reverse-index member"
                        );
                        self.remove_reverse_index_member(&node_key, &member).await?;
                        continue;
                    }
                };

                if info.node_id != node_id {
                    self.remove_reverse_index_member(&node_key, &member).await?;
                    continue;
                }

                let cleanup_result: Result<Vec<redis::Value>> = with_redis_timeout(|| async {
                    let mut conn = self.conn().await?;
                    let result: Vec<redis::Value> = CLEANUP_NODE_PUBLISHER_SCRIPT
                        .key(&publisher_key)
                        .arg(node_id)
                        .arg(info.epoch)
                        .invoke_async(&mut conn)
                        .await
                        .map_err(|e| anyhow!("Cleanup Lua script failed: {e}"))?;
                    Ok(result)
                })
                .await;

                if let Ok(result) = cleanup_result {
                    let status = match &result[0] {
                        redis::Value::Int(v) => *v,
                        _ => 0,
                    };

                    if status == 1 {
                        let user_id = match &result[1] {
                            redis::Value::BulkString(s) => String::from_utf8_lossy(s).to_string(),
                            redis::Value::SimpleString(s) => s.clone(),
                            _ => String::new(),
                        };
                        let room_key = self.room_publishers_key(&room_id);
                        let active_key = self.active_publishers_key();

                        if !user_id.is_empty() {
                            let user_key = self.user_publishers_key(&user_id);
                            self.remove_reverse_index_member(&user_key, &member).await?;
                        }
                        self.remove_reverse_index_member(&node_key, &member).await?;
                        self.remove_reverse_index_member(&room_key, &member).await?;
                        self.remove_reverse_index_member(&active_key, &member)
                            .await?;

                        info!(
                            "Cleaned up stale publisher entry for node {} (room: {}, media: {})",
                            node_id, room_id, media_id
                        );
                    } else if status == -1 {
                        info!(
                            "Skipped cleanup for node {} (room: {}, media: {}): epoch mismatch (newer publisher exists)",
                            node_id, room_id, media_id
                        );
                    } else {
                        self.remove_reverse_index_member(&node_key, &member).await?;
                    }
                } else if let Err(error) = cleanup_result {
                    debug!(
                        node_id = %node_id,
                        room_id = %room_id,
                        media_id = %media_id,
                        error = %error,
                        "Failed to cleanup publisher during node cleanup"
                    );
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        publisher: Option<PublisherInfo>,
    ) -> std::result::Result<PublisherInfo, Box<dyn std::error::Error + Send + Sync>> {
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
            .try_register_publisher_with_user(
                "room123",
                "media456",
                "node1",
                "user1",
                "localhost:50051",
            )
            .await?;
        assert!(registered);

        let publisher = registry.get_publisher("room123", "media456").await?;
        let pub_info = require_publisher(publisher)?;
        assert_eq!(pub_info.node_id, "node1");
        assert_eq!(pub_info.api_address, "localhost:50051");

        registry.unregister_publisher("room123", "media456").await?;
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
            .try_register_publisher_with_user(
                "room123",
                "media456",
                "node1",
                "user1",
                "localhost:50051",
            )
            .await?;

        let mut verify_conn = RedisConnectionManager::new(client).await?;
        let namespaced_exists: bool = verify_conn
            .exists(format!("{prefix}stream:publisher:room123:media456"))
            .await?;
        let unprefixed_exists: bool = verify_conn
            .exists("stream:publisher:room123:media456")
            .await?;

        assert!(
            namespaced_exists,
            "registry must honor configured key prefix"
        );
        assert!(
            !unprefixed_exists,
            "registry must not leak publisher keys into the global Redis namespace"
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_refresh_publisher_ttl_repairs_missing_reverse_indexes() -> TestResult {
        use redis::AsyncCommands;

        let (_container, client, redis, prefix) = setup_redis().await;
        let registry = test_registry(redis, prefix);

        registry
            .try_register_publisher_with_user(
                "room1",
                "media1",
                "node1",
                "user1",
                "localhost:50051",
            )
            .await?;

        let publisher = require_publisher(registry.get_publisher("room1", "media1").await?)?;

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
            .refresh_publisher_ttl_with_owner(
                "room1",
                "media1",
                "user1",
                "node1",
                Some(publisher.epoch),
            )
            .await?;
        assert_eq!(outcome, PublisherRefreshOutcome::Refreshed);

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
    async fn test_shared_redis_handle_hot_swap_keeps_registry_operational() -> TestResult {
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let (_container, client, redis, prefix) = setup_redis().await;
        let shared = Arc::new(RwLock::new(redis));
        let registry = shared_test_registry(shared.clone(), prefix);

        let registered = registry
            .try_register_publisher_with_user(
                "room1",
                "media1",
                "node1",
                "user1",
                "localhost:50051",
            )
            .await?;
        assert!(registered);

        let replacement = RedisConnectionManager::new(client.clone()).await?;
        *shared.write().await = replacement;

        let publisher = require_publisher(registry.get_publisher("room1", "media1").await?)?;
        assert_eq!(publisher.node_id, "node1");

        registry.unregister_publisher("room1", "media1").await?;
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_register_publisher_duplicate() -> TestResult {
        let (_container, _client, redis, prefix) = setup_redis().await;
        let registry = test_registry(redis, prefix);

        let registered = registry
            .try_register_publisher_with_user(
                "room123",
                "media456",
                "node1",
                "user1",
                "localhost:50051",
            )
            .await?;
        assert!(registered);

        let registered = registry
            .try_register_publisher_with_user(
                "room123",
                "media456",
                "node2",
                "user2",
                "localhost:50052",
            )
            .await?;
        assert!(!registered);

        registry.unregister_publisher("room123", "media456").await?;
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_try_register_publisher() -> TestResult {
        let (_container, _client, redis, prefix) = setup_redis().await;
        let registry = test_registry(redis, prefix);

        let result = registry
            .try_register_publisher_with_user(
                "room123",
                "media456",
                "node1",
                "user1",
                "localhost:50051",
            )
            .await?;
        assert!(result);

        let result = registry
            .try_register_publisher_with_user(
                "room123",
                "media456",
                "node2",
                "user2",
                "localhost:50052",
            )
            .await?;
        assert!(!result);

        registry.unregister_publisher("room123", "media456").await?;
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_unregister_publisher() -> TestResult {
        let (_container, _client, redis, prefix) = setup_redis().await;
        let registry = test_registry(redis, prefix);

        registry
            .try_register_publisher_with_user(
                "room123",
                "media456",
                "node1",
                "user1",
                "localhost:50051",
            )
            .await?;

        assert!(registry.is_stream_active("room123", "media456").await?);

        registry.unregister_publisher("room123", "media456").await?;

        assert!(!registry.is_stream_active("room123", "media456").await?);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_publisher_not_found() -> TestResult {
        let (_container, _client, redis, prefix) = setup_redis().await;
        let registry = test_registry(redis, prefix);

        let result = registry.get_publisher("nonexistent", "media").await?;
        assert!(result.is_none());
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_unregister_publisher_cleans_node_reverse_index() -> TestResult {
        let (_container, client, redis, prefix) = setup_redis().await;
        let registry = test_registry(redis, prefix);

        registry
            .try_register_publisher_with_user(
                "room1",
                "media1",
                "node1",
                "user1",
                "localhost:50051",
            )
            .await?;

        registry.unregister_publisher("room1", "media1").await?;

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
    async fn test_cleanup_all_publishers_for_node_prunes_stale_reverse_index_members() -> TestResult
    {
        let (_container, client, redis, prefix) = setup_redis().await;
        let registry = test_registry(redis, prefix);

        registry
            .try_register_publisher_with_user(
                "room1",
                "media1",
                "node1",
                "user1",
                "localhost:50051",
            )
            .await?;
        registry
            .try_register_publisher_with_user(
                "room2",
                "media2",
                "node2",
                "user2",
                "localhost:50052",
            )
            .await?;

        let mut verify_conn = RedisConnectionManager::new(client).await?;
        let _: () = redis::cmd("SADD")
            .arg(registry.node_publishers_key("node1"))
            .arg("room2:media2")
            .query_async(&mut verify_conn)
            .await?;

        registry.cleanup_all_publishers_for_node("node1").await?;

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
            .try_register_publisher_with_user(
                "room1",
                "media1",
                "node1",
                "user1",
                "localhost:50051",
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

        let active = registry.list_active_publishers().await?;
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
            .try_register_publisher_with_user(
                "room123",
                "media456",
                "node1",
                "user1",
                "localhost:50051",
            )
            .await?;

        let publisher = require_publisher(registry.get_publisher("room123", "media456").await?)?;

        assert_eq!(publisher.node_id, "node1");
        assert_eq!(publisher.api_address, "localhost:50051");
        assert!(publisher.started_at <= chrono::Utc::now());

        registry.unregister_publisher("room123", "media456").await?;
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
