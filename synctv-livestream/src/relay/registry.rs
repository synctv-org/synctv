use anyhow::{Result, anyhow};
use redis::aio::ConnectionManager as RedisConnectionManager;
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};
use tracing::{debug, info};

/// Heartbeat interval in seconds for publisher liveness.
/// The publisher manager sends a heartbeat every this many seconds.
pub const HEARTBEAT_INTERVAL_SECS: u64 = 60;

/// TTL multiplier: TTL = `HEARTBEAT_INTERVAL_SECS` * `TTL_MULTIPLIER`.
/// A multiplier of 5 means up to 4 consecutive missed heartbeats are tolerated
/// before the registry entry expires.
const TTL_MULTIPLIER: u64 = 5;

/// Publisher TTL in seconds, derived from heartbeat interval.
/// This is the Redis key expiration set on publisher entries.
pub const PUBLISHER_TTL_SECS: i64 = (HEARTBEAT_INTERVAL_SECS * TTL_MULTIPLIER) as i64;

/// Redis key for the global epoch counter used for fencing tokens.
/// Format: "`stream:epoch:{room_id}:{media_id`}"
/// Each publisher registration increments this counter atomically.
const EPOCH_KEY_PREFIX: &str = "stream:epoch";

// Compile-time safety check: TTL must be at least 3x the heartbeat interval
// to tolerate transient network issues.
const _: () = assert!(
    PUBLISHER_TTL_SECS as u64 >= HEARTBEAT_INTERVAL_SECS * 3,
    "PUBLISHER_TTL_SECS must be at least 3x HEARTBEAT_INTERVAL_SECS"
);

/// Publisher information stored in Redis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublisherInfo {
    /// Node ID of the publisher
    pub node_id: String,
    /// gRPC address of the publisher node (e.g., "10.0.0.1:50051").
    /// Used by pull streams to connect to the publisher.
    ///
    /// **Must not be empty** when the publisher is used for cross-node proxying.
    /// Use [`PublisherInfo::validate_grpc_address`] before connecting.
    #[serde(default)]
    pub grpc_address: String,
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
    /// Validate that `grpc_address` is set and non-empty.
    ///
    /// Returns `Err` if the address is empty, which would happen if the publisher
    /// registered without configuring a gRPC listen address (misconfiguration).
    pub fn validate_grpc_address(&self) -> Result<&str> {
        if self.grpc_address.trim().is_empty() {
            return Err(anyhow!(
                "PublisherInfo for node={} has empty grpc_address (room/media stream cannot be proxied)",
                self.node_id
            ));
        }
        Ok(&self.grpc_address)
    }
}

/// Publisher Registry for tracking active publishers via Redis.
///
/// **Role**: Publisher Ownership -- enforces single-publisher-per-media and provides
/// publisher discovery for cross-node gRPC relay. Used by the livestream layer to:
/// 1. Atomically register a publisher (prevents duplicate publishers for the same media)
/// 2. Look up the publisher's node/gRPC address for cross-node relay
/// 3. Manage publisher TTL via heartbeat for crash detection
///
/// **Distinction from `synctv_cluster::sync::StreamRegistry`**:
/// - This registry tracks *publisher ownership* (who is publishing, on which node,
///   with what gRPC address, at what epoch) using `room_id/media_id` keys.
/// - The cluster stream registry tracks *stream presence* for routing/discovery
///   using app/stream identifiers.
/// - Both use Redis; this one is Redis-only (no local cache) because publisher
///   ownership must always be authoritative from Redis.
#[derive(Clone)]
pub struct StreamRegistry {
    redis: RedisConnectionManager,
}

impl StreamRegistry {
    /// Create a new stream registry
    #[must_use] 
    pub const fn new(redis: RedisConnectionManager) -> Self {
        Self { redis }
    }

    /// Register a publisher for a media in a room (atomic operation).
    /// Returns `true` if registered successfully, `false` if already exists.
    ///
    /// Delegates to the atomic Lua-based `try_register_publisher_with_user()`
    /// to prevent epoch inflation on failed registration attempts.
    pub async fn register_publisher(
        &self,
        room_id: &str,
        media_id: &str,
        node_id: &str,
        _app_name: &str,
    ) -> anyhow::Result<bool> {
        self.try_register_publisher_with_user(room_id, media_id, node_id, "", "").await
    }

    /// Try to register as publisher (simplified version for `PublisherManager`)
    /// Returns true if registered successfully, false if already exists
    pub async fn try_register_publisher(
        &self,
        room_id: &str,
        media_id: &str,
        node_id: &str,
    ) -> anyhow::Result<bool> {
        self.try_register_publisher_with_user(room_id, media_id, node_id, "", "").await
    }

    /// Try to register as publisher with `user_id`
    /// Returns true if registered successfully, false if already exists
    ///
    /// FIXED: P0.5 - Uses atomic Lua script to prevent epoch race condition
    /// The script ensures INCR + HSETNX are atomic - if HSETNX fails, epoch is rolled back
    ///
    /// # Errors
    ///
    /// Returns an error if `grpc_address` is empty, as cross-node proxying requires
    /// a valid gRPC address.
    pub async fn try_register_publisher_with_user(
        &self,
        room_id: &str,
        media_id: &str,
        node_id: &str,
        user_id: &str,
        grpc_address: &str,
    ) -> anyhow::Result<bool> {
        // Validate grpc_address at registration time (not usage time)
        // This ensures publishers cannot register without a valid gRPC address
        if grpc_address.trim().is_empty() {
            return Err(anyhow!(
                "Cannot register publisher for node={} with empty grpc_address (room={}, media={})",
                node_id, room_id, media_id
            ));
        }

        let key = format!("stream:publisher:{room_id}:{media_id}");
        let epoch_key = format!("{EPOCH_KEY_PREFIX}:{room_id}:{media_id}");
        let mut conn = self.redis.clone();

        // Create PublisherInfo template (epoch will be filled by Lua script)
        let info = PublisherInfo {
            node_id: node_id.to_string(),
            grpc_address: grpc_address.to_string(),
            app_name: "live".to_string(),
            user_id: user_id.to_string(),
            started_at: Utc::now(),
            epoch: 0, // Placeholder, will be replaced by actual epoch in Lua script
        };
        let info_json = serde_json::to_string(&info)?;

        // Atomic Lua script to prevent epoch TOCTOU race condition.
        //
        // Issue #51: The original script incremented the epoch BEFORE the HSETNX
        // check, so other nodes could briefly observe a spuriously inflated epoch
        // during the window between INCR and a failed HSETNX (followed by DECR).
        //
        // Fix: HSETNX first, then increment epoch ONLY if registration succeeded.
        // This ensures the epoch counter only changes when ownership actually changes,
        // eliminating the intermediate-epoch window entirely.
        //
        // Returns: {registered (1 or 0), epoch}
        //   - registered=1: new publisher registered; epoch is the new epoch value.
        //   - registered=0: another publisher already exists; epoch is the current epoch.
        let lua_script = r#"
            local epoch_key = KEYS[1]
            local hash_key = KEYS[2]
            local info_json_template = ARGV[1]
            local ttl = tonumber(ARGV[2])
            local user_key = ARGV[3]
            local user_member = ARGV[4]

            -- Issue #51: Check HSETNX FIRST before touching the epoch.
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

            return {1, epoch}
        "#;

        let user_key = if user_id.is_empty() {
            String::new()
        } else {
            format!("stream:user_publishers:{user_id}")
        };
        let user_member = format!("{room_id}:{media_id}");

        // Fixed #114: Add timeout for Redis Lua script execution (5 seconds)
        // Prevents indefinite blocking on Redis server issues or slow Lua execution
        let result: Vec<i64> = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            async {
                redis::Script::new(lua_script)
                    .key(&epoch_key)
                    .key(&key)
                    .arg(&info_json)
                    .arg(PUBLISHER_TTL_SECS)
                    .arg(&user_key)
                    .arg(&user_member)
                    .invoke_async(&mut conn)
                    .await
            }
        )
            .await
            .map_err(|_| anyhow!("Lua script execution timed out after 5s"))?
            .map_err(|e| anyhow!("Lua script execution failed: {e}"))?;

        let registered = result[0] == 1;
        let actual_epoch = result[1] as u64;

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

    /// Refresh TTL for a publisher (called by heartbeat)
    pub async fn refresh_publisher_ttl(&self, room_id: &str, media_id: &str) -> Result<()> {
        self.refresh_publisher_ttl_with_user(room_id, media_id, "").await
    }

    /// Refresh TTL for a publisher and its user reverse-index (called by heartbeat)
    pub async fn refresh_publisher_ttl_with_user(&self, room_id: &str, media_id: &str, user_id: &str) -> Result<()> {
        let key = format!("stream:publisher:{room_id}:{media_id}");
        let mut conn = self.redis.clone();

        // Refresh publisher key TTL (derived from HEARTBEAT_INTERVAL_SECS * TTL_MULTIPLIER)
        // Preserve the redis::RedisError type so callers can classify errors structurally
        let _: () = redis::cmd("EXPIRE")
            .arg(&key)
            .arg(PUBLISHER_TTL_SECS)
            .query_async(&mut conn)
            .await
            .map_err(anyhow::Error::from)?;

        // Also refresh user reverse-index TTL if user_id is provided
        if !user_id.is_empty() {
            let user_key = format!("stream:user_publishers:{user_id}");
            let _: () = redis::cmd("EXPIRE")
                .arg(&user_key)
                .arg(PUBLISHER_TTL_SECS)
                .query_async(&mut conn)
                .await
                .map_err(anyhow::Error::from)?;
        }

        Ok(())
    }

    /// Unregister a publisher.
    ///
    /// Delegates to `unregister_publisher_immut` which correctly cleans up
    /// both the publisher entry and the user reverse index.
    pub async fn unregister_publisher(&self, room_id: &str, media_id: &str) -> Result<()> {
        self.unregister_publisher_immut(room_id, media_id).await
    }

    /// Unregister a publisher (non-mut version for `PublisherManager`)
    pub async fn unregister_publisher_immut(&self, room_id: &str, media_id: &str) -> Result<()> {
        self.unregister_publisher_with_epoch(room_id, media_id, None).await
    }

    /// Epoch-validated unregister: only deletes if the stored epoch matches the expected epoch.
    /// If `expected_epoch` is None, deletes unconditionally (backwards compatible).
    ///
    /// This prevents a race where publisher A dies, publisher B registers, then
    /// publisher A's delayed cleanup incorrectly removes publisher B's entry.
    pub async fn unregister_publisher_with_epoch(
        &self,
        room_id: &str,
        media_id: &str,
        expected_epoch: Option<u64>,
    ) -> Result<()> {
        let key = format!("stream:publisher:{room_id}:{media_id}");
        let mut conn = self.redis.clone();

        // Atomic Lua script: check epoch (if provided), delete publisher, clean up user index
        let lua_script = r"
            local hash_key = KEYS[1]
            local check_epoch = tonumber(ARGV[1])

            -- Get current publisher info
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

            -- If epoch check is requested, validate before deleting
            if check_epoch >= 0 then
                local stored_epoch = tonumber(parsed.epoch)
                if stored_epoch and stored_epoch ~= check_epoch then
                    -- Epoch mismatch: a newer publisher registered, do NOT delete
                    return {-1, ''}
                end
            end

            -- Extract user_id for reverse-index cleanup
            local user_id = parsed.user_id or ''

            -- Delete the publisher entry
            redis.call('HDEL', hash_key, 'publisher')

            return {1, user_id}
        ";

        // Use -1 to mean "no epoch check" (unconditional delete)
        let epoch_arg: i64 = match expected_epoch {
            Some(e) => e as i64,
            None => -1,
        };

        let result: Vec<redis::Value> = redis::Script::new(lua_script)
            .key(&key)
            .arg(epoch_arg)
            .invoke_async(&mut conn)
            .await
            .map_err(|e| anyhow!("Unregister Lua script failed: {e}"))?;

        // Parse result: [status, user_id]
        let status = match &result[0] {
            redis::Value::Int(v) => *v,
            _ => 0,
        };
        let user_id = match &result[1] {
            redis::Value::BulkString(s) => String::from_utf8_lossy(s).to_string(),
            redis::Value::SimpleString(s) => s.clone(),
            _ => String::new(),
        };

        if status == -1 {
            info!(
                "Skipped unregister for room={}, media={}: epoch mismatch (newer publisher exists)",
                room_id, media_id
            );
            return Ok(());
        }

        // Clean up user reverse index if user_id was present
        if status == 1 && !user_id.is_empty() {
            let user_key = format!("stream:user_publishers:{user_id}");
            let member = format!("{room_id}:{media_id}");
            let _: () = redis::cmd("SREM")
                .arg(&user_key)
                .arg(&member)
                .query_async(&mut conn)
                .await
                .map_err(|e| anyhow!(e.to_string()))?;
        }

        Ok(())
    }

    /// Get all active publishers for a user (via reverse index)
    /// Returns list of (`room_id`, `media_id`) pairs
    pub async fn get_user_publishers(&self, user_id: &str) -> Result<Vec<(String, String)>> {
        let user_key = format!("stream:user_publishers:{user_id}");
        let mut conn = self.redis.clone();

        let members: Vec<String> = redis::cmd("SMEMBERS")
            .arg(&user_key)
            .query_async(&mut conn)
            .await
            .map_err(|e| anyhow!(e.to_string()))?;

        Ok(members
            .into_iter()
            .filter_map(|m| {
                m.split_once(':')
                    .map(|(r, m)| (r.to_string(), m.to_string()))
            })
            .collect())
    }

    /// Remove all publisher entries for a user (via reverse index)
    pub async fn unregister_all_user_publishers(&self, user_id: &str) -> Result<()> {
        let publishers = self.get_user_publishers(user_id).await?;
        for (room_id, media_id) in publishers {
            self.unregister_publisher_immut(&room_id, &media_id).await?;
        }
        Ok(())
    }

    /// Get publisher info for a media in a room.
    pub async fn get_publisher(&self, room_id: &str, media_id: &str) -> Result<Option<PublisherInfo>> {
        self.get_publisher_immut(room_id, media_id).await
    }

    /// Get publisher info for a media in a room (immutable version)
    pub async fn get_publisher_immut(&self, room_id: &str, media_id: &str) -> Result<Option<PublisherInfo>> {
        let key = format!("stream:publisher:{room_id}:{media_id}");
        let mut conn = self.redis.clone();
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
    }


    /// Check if a stream is active (has a publisher).
    pub async fn is_stream_active(&self, room_id: &str, media_id: &str) -> anyhow::Result<bool> {
        self.is_stream_active_immut(room_id, media_id).await
    }

    /// Check if a stream is active (immutable version)
    pub async fn is_stream_active_immut(&self, room_id: &str, media_id: &str) -> anyhow::Result<bool> {
        let key = format!("stream:publisher:{room_id}:{media_id}");
        let mut conn = self.redis.clone();
        let exists: bool = redis::cmd("HEXISTS")
            .arg(&key)
            .arg("publisher")
            .query_async(&mut conn)
            .await
            .map_err(|e| anyhow!(e.to_string()))?;
        Ok(exists)
    }

    /// List all active streams (returns tuples of (`room_id`, `media_id`)).
    pub async fn list_active_streams(&self) -> Result<Vec<(String, String)>> {
        self.list_active_streams_immut().await
    }

    /// List all active streams (immutable version)
    ///
    /// Uses SCAN instead of KEYS to avoid blocking Redis on large datasets.
    /// SCAN iterates through keys incrementally without blocking the server.
    pub async fn list_active_streams_immut(&self) -> Result<Vec<(String, String)>> {
        let mut conn = self.redis.clone();
        let mut streams = Vec::new();
        let mut cursor: u64 = 0;

        loop {
            // SCAN returns (new_cursor, keys)
            let (new_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg("stream:publisher:*")
                .arg("COUNT")
                .arg(100) // Scan 100 keys per iteration for better performance
                .query_async(&mut conn)
                .await
                .map_err(|e| anyhow!(e.to_string()))?;

            // Parse keys into (room_id, media_id) tuples.
            // Use split_once(':') instead of split(':').collect() to correctly handle
            // room_ids that contain ':' characters — split(':').collect() + len==2 check
            // would silently drop any such key, and indexing [0]/[1] would give wrong results.
            // split_once splits only on the FIRST ':': room_id gets everything before it,
            // media_id gets everything after (including any embedded colons in media_id).
            for k in keys {
                if let Some(s) = k.strip_prefix("stream:publisher:") {
                    if let Some((room_id, media_id)) = s.split_once(':') {
                        streams.push((room_id.to_string(), media_id.to_string()));
                    }
                }
            }

            cursor = new_cursor;
            // cursor returns to 0 when scan is complete
            if cursor == 0 {
                break;
            }
        }

        Ok(streams)
    }

    /// List active streams for a specific room, returning only the `media_id` values.
    /// More efficient than `list_active_streams_immut` followed by a filter because
    /// the SCAN pattern is scoped to the room's key prefix.
    pub async fn list_streams_for_room(&self, room_id: &str) -> Result<Vec<String>> {
        let mut conn = self.redis.clone();
        let mut media_ids = Vec::new();
        let mut cursor: u64 = 0;
        let pattern = format!("stream:publisher:{room_id}:*");
        let prefix = format!("stream:publisher:{room_id}:");

        loop {
            let (new_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(100)
                .query_async(&mut conn)
                .await
                .map_err(|e| anyhow!(e.to_string()))?;

            for k in keys {
                if let Some(media_id) = k.strip_prefix(&prefix) {
                    media_ids.push(media_id.to_string());
                }
            }

            cursor = new_cursor;
            if cursor == 0 {
                break;
            }
        }

        Ok(media_ids)
    }

    /// Validate that the given epoch matches the current publisher's epoch.
    /// Returns Ok(true) if the epoch is valid, Ok(false) if stale/invalid.
    /// Used by pull streams to detect split-brain scenarios.
    pub async fn validate_epoch(&self, room_id: &str, media_id: &str, epoch: u64) -> Result<bool> {
        let key = format!("stream:publisher:{room_id}:{media_id}");
        let mut conn = self.redis.clone();

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
    }

    /// Get the current epoch for a stream without publisher info.
    /// Returns None if no publisher exists.
    pub async fn get_current_epoch(&self, room_id: &str, media_id: &str) -> Result<Option<u64>> {
        let publisher = self.get_publisher_immut(room_id, media_id).await?;
        Ok(publisher.map(|p| p.epoch))
    }

    /// Clean up all publisher registrations for a specific node.
    /// Used when a node restarts to remove stale entries from Redis.
    ///
    /// This uses SCAN to iterate through all publisher keys and removes
    /// those belonging to the specified `node_id`, using epoch validation
    /// to avoid deleting publishers that were re-registered by a new node
    /// between the SCAN and the delete (TOCTOU race).
    pub async fn cleanup_all_publishers_for_node(&self, node_id: &str) -> Result<()> {
        let mut conn = self.redis.clone();
        let mut cursor: u64 = 0;

        // Atomic Lua script: check node_id AND epoch before deleting.
        // This prevents a race where a new publisher registers between
        // our SCAN (which reads epoch) and the delete.
        let cleanup_script = r"
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
        ";

        loop {
            // SCAN for publisher keys
            let (new_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg("stream:publisher:*")
                .arg("COUNT")
                .arg(100)
                .query_async(&mut conn)
                .await
                .map_err(|e| anyhow!(e.to_string()))?;

            // Check each key and remove if it belongs to the node
            for key in keys {
                // Extract room_id and media_id from key: "stream:publisher:{room_id}:{media_id}"
                let key_suffix = match key.strip_prefix("stream:publisher:") {
                    Some(s) => s,
                    None => continue,
                };
                let (room_id, media_id) = match key_suffix.split_once(':') {
                    Some((r, m)) => (r.to_string(), m.to_string()),
                    None => continue,
                };

                // Get publisher info to read its epoch for validation
                let info_json: Option<String> = redis::cmd("HGET")
                    .arg(&key)
                    .arg("publisher")
                    .query_async(&mut conn)
                    .await
                    .map_err(|e| anyhow!(e.to_string()))?;

                if let Some(json) = &info_json {
                    if let Ok(info) = serde_json::from_str::<PublisherInfo>(json) {
                        if info.node_id == node_id {
                            // Atomically delete only if node_id AND epoch still match
                            let result: Vec<redis::Value> =
                                redis::Script::new(cleanup_script)
                                    .key(&key)
                                    .arg(node_id)
                                    .arg(info.epoch)
                                    .invoke_async(&mut conn)
                                    .await
                                    .map_err(|e| anyhow!("Cleanup Lua script failed: {e}"))?;

                            let status = match &result[0] {
                                redis::Value::Int(v) => *v,
                                _ => 0,
                            };

                            if status == 1 {
                                // Successfully deleted; clean up user reverse index
                                let user_id = match &result[1] {
                                    redis::Value::BulkString(s) => {
                                        String::from_utf8_lossy(s).to_string()
                                    }
                                    redis::Value::SimpleString(s) => s.clone(),
                                    _ => String::new(),
                                };

                                if !user_id.is_empty() {
                                    let user_key =
                                        format!("stream:user_publishers:{user_id}");
                                    let member = format!("{room_id}:{media_id}");
                                    let _: () = redis::cmd("SREM")
                                        .arg(&user_key)
                                        .arg(&member)
                                        .query_async(&mut conn)
                                        .await
                                        .map_err(|e| anyhow!(e.to_string()))?;
                                }

                                info!(
                                    "Cleaned up stale publisher entry for node {} (room: {}, media: {})",
                                    node_id, room_id, media_id
                                );
                            } else if status == -1 {
                                info!(
                                    "Skipped cleanup for node {} (room: {}, media: {}): epoch mismatch (newer publisher exists)",
                                    node_id, room_id, media_id
                                );
                            }
                        }
                    }
                }
            }

            cursor = new_cursor;
            if cursor == 0 {
                break;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use testcontainers::core::ImageExt;
    use testcontainers::runners::AsyncRunner;
    use testcontainers_modules::redis::Redis;

    /// Default Redis version for test containers
    const REDIS_VERSION: &str = "7-alpine";

    /// Type alias for the Redis container type
    type RedisContainer = testcontainers::ContainerAsync<Redis>;

    async fn setup_redis() -> (RedisContainer, redis::Client, RedisConnectionManager) {
        let redis_container = Redis::default()
            .with_tag(REDIS_VERSION)
            .start()
            .await
            .expect("Failed to start Redis container");

        let redis_host = redis_container.get_host().await.expect("Failed to get Redis host");
        let redis_port = redis_container
            .get_host_port_ipv4(6379)
            .await
            .expect("Failed to get Redis port");

        let redis_url = format!("redis://{}:{}", redis_host, redis_port);
        let redis_client = redis::Client::open(redis_url.as_str()).expect("Failed to create Redis client");
        let conn_mgr = RedisConnectionManager::new(redis_client.clone())
            .await
            .expect("Failed to create ConnectionManager");

        (redis_container, redis_client, conn_mgr)
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_register_publisher_success() {
        let (_container, _client, redis) = setup_redis().await;
        let registry = StreamRegistry::new(redis);

        // First registration should succeed (use with_user variant with grpc_address)
        let registered = registry
            .try_register_publisher_with_user("room123", "media456", "node1", "user1", "localhost:50051")
            .await
            .unwrap();
        assert!(registered);

        // Verify publisher exists
        let publisher = registry.get_publisher("room123", "media456").await.unwrap();
        assert!(publisher.is_some());

        let pub_info = publisher.unwrap();
        assert_eq!(pub_info.node_id, "node1");
        assert_eq!(pub_info.grpc_address, "localhost:50051");

        // Cleanup
        registry.unregister_publisher("room123", "media456").await.unwrap();
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_register_publisher_duplicate() {
        let (_container, _client, redis) = setup_redis().await;
        let registry = StreamRegistry::new(redis);

        // First registration should succeed
        let registered = registry
            .try_register_publisher_with_user("room123", "media456", "node1", "user1", "localhost:50051")
            .await
            .unwrap();
        assert!(registered);

        // Second registration should fail (already exists)
        let registered = registry
            .try_register_publisher_with_user("room123", "media456", "node2", "user2", "localhost:50052")
            .await
            .unwrap();
        assert!(!registered);

        // Cleanup
        registry.unregister_publisher("room123", "media456").await.unwrap();
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_try_register_publisher() {
        let (_container, _client, redis) = setup_redis().await;
        let registry = StreamRegistry::new(redis);

        // First try_register should succeed
        let result = registry
            .try_register_publisher_with_user("room123", "media456", "node1", "user1", "localhost:50051")
            .await
            .unwrap();
        assert!(result);

        // Second try_register should return false (already exists)
        let result = registry
            .try_register_publisher_with_user("room123", "media456", "node2", "user2", "localhost:50052")
            .await
            .unwrap();
        assert!(!result);

        // Cleanup
        registry.unregister_publisher("room123", "media456").await.unwrap();
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_unregister_publisher() {
        let (_container, _client, redis) = setup_redis().await;
        let registry = StreamRegistry::new(redis);

        // Register publisher
        registry
            .try_register_publisher_with_user("room123", "media456", "node1", "user1", "localhost:50051")
            .await
            .unwrap();

        // Verify exists
        assert!(registry.is_stream_active("room123", "media456").await.unwrap());

        // Unregister
        registry.unregister_publisher("room123", "media456").await.unwrap();

        // Verify removed
        assert!(!registry.is_stream_active("room123", "media456").await.unwrap());
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_publisher_not_found() {
        let (_container, _client, redis) = setup_redis().await;
        let registry = StreamRegistry::new(redis);

        // Non-existent publisher should return None
        let result = registry.get_publisher("nonexistent", "media").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_list_active_streams() {
        let (_container, _client, redis) = setup_redis().await;
        let registry = StreamRegistry::new(redis);

        // Register multiple publishers
        registry
            .try_register_publisher_with_user("room1", "media1", "node1", "user1", "localhost:50051")
            .await
            .unwrap();
        registry
            .try_register_publisher_with_user("room2", "media2", "node1", "user1", "localhost:50051")
            .await
            .unwrap();

        // List active streams
        let streams = registry.list_active_streams().await.unwrap();
        assert_eq!(streams.len(), 2);
        assert!(streams.contains(&(String::from("room1"), String::from("media1"))));
        assert!(streams.contains(&(String::from("room2"), String::from("media2"))));

        // Cleanup
        registry.unregister_publisher("room1", "media1").await.unwrap();
        registry.unregister_publisher("room2", "media2").await.unwrap();
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_publisher_info_serialization() {
        let (_container, _client, redis) = setup_redis().await;
        let registry = StreamRegistry::new(redis);

        // Register publisher
        registry
            .try_register_publisher_with_user("room123", "media456", "node1", "user1", "localhost:50051")
            .await
            .unwrap();

        // Get publisher and verify serialization/deserialization
        let publisher = registry.get_publisher("room123", "media456").await.unwrap().unwrap();

        assert_eq!(publisher.node_id, "node1");
        assert_eq!(publisher.grpc_address, "localhost:50051");
        assert!(publisher.started_at <= chrono::Utc::now());

        // Cleanup
        registry.unregister_publisher("room123", "media456").await.unwrap();
    }
}
