use std::time::{Duration, Instant};

use redis::AsyncCommands;
use tracing::{debug, info, warn};

use super::model::{
    i64_to_u64_saturating, i64_to_usize_saturating, usize_to_i64_saturating,
    usize_to_u64_saturating, ConnectionInfo, ConnectionInfoPersistent,
};
use super::redis_state::{
    BATCH_REFRESH_TTLS_SCRIPT, CONNECTION_METADATA_TTL_SECONDS, DECR_DELETE_NEGATIVE_SCRIPT,
    DISTRIBUTED_COUNTER_TTL_SECONDS, INCR_EXPIRE_SCRIPT, SYNC_COUNTER_MIN_SCRIPT,
    TTL_REFRESH_BATCH_SIZE, VOICE_RTC_JOIN_SCRIPT,
};
use super::{ConnectionManager, VoiceRtcJoinOutcome};
use synctv_core::models::id::{RoomId, UserId};

impl ConnectionManager {
    /// Resolve connection metadata across replicas.
    pub async fn get_connection_distributed(
        &self,
        connection_id: &str,
    ) -> Result<Option<ConnectionInfo>, String> {
        if let Some(connection) = self.get_connection(connection_id) {
            return Ok(Some(connection));
        }
        let Some(mut redis) = self
            .redis_conn_snapshot_required(
                "Distributed connection lookup unavailable while Redis is degraded",
            )
            .await?
        else {
            return Ok(None);
        };
        let value: Option<String> = self
            .redis_op(
                "load distributed connection metadata",
                redis.get(self.conn_metadata_key(connection_id)),
            )
            .await
            .map_err(|error| format!("Distributed connection lookup failed: {error}"))?;
        value
            .map(|json| {
                serde_json::from_str::<ConnectionInfoPersistent>(&json)
                    .map(ConnectionInfoPersistent::into_connection_info)
                    .map_err(|error| format!("Distributed connection metadata is invalid: {error}"))
            })
            .transpose()
    }

    /// Persist one local connection immediately after security-sensitive state changes.
    pub async fn sync_connection_metadata_distributed(
        &self,
        connection_id: &str,
    ) -> Result<(), String> {
        let Some(connection) = self.get_connection(connection_id) else {
            return Err("Connection not found".to_string());
        };
        let Some(mut redis) = self
            .redis_conn_snapshot_required(
                "Distributed connection metadata update unavailable while Redis is degraded",
            )
            .await?
        else {
            return Ok(());
        };
        let value = serde_json::to_string(&ConnectionInfoPersistent::from(&connection))
            .map_err(|error| format!("Failed to encode connection metadata: {error}"))?;
        self.redis_op(
            "sync distributed connection metadata",
            redis.set_ex(
                self.conn_metadata_key(connection_id),
                value,
                i64_to_u64_saturating(CONNECTION_METADATA_TTL_SECONDS),
            ),
        )
        .await
        .map_err(|error| format!("Distributed connection metadata update failed: {error}"))
    }

    /// Refresh TTLs on all active distributed connection counters and metadata in Redis.
    ///
    /// Long-lived connections (up to 24 hours) outlive the crash-safety TTL
    /// (`DISTRIBUTED_COUNTER_TTL_SECONDS`). Without periodic refreshes, the
    /// counter expires while the connection is still alive, causing distributed
    /// rate limiting to silently stop working.
    ///
    /// Also refreshes TTLs on connection metadata keys (`conn_mgr:conn:*`,
    /// `conn_mgr:actor:*`, `conn_mgr:room:*`) to prevent them from expiring
    /// while the connection is still active.
    ///
    /// Additionally, synchronizes local connection counts to Redis counters to handle
    /// cases where Redis was temporarily unavailable during connection registration.
    /// This ensures eventual consistency between local and distributed counters.
    ///
    /// # Performance
    ///
    /// Uses a Lua script to batch refresh TTLs in groups of `TTL_REFRESH_BATCH_SIZE`
    /// keys at a time, reducing memory pressure and network round-trips compared to
    /// refreshing all keys at once.
    async fn refresh_distributed_counter_ttls(&self) {
        let Some(mut conn) = self.redis_conn_snapshot().await else {
            return;
        };

        // Collect unique actor and room keys from active connections
        let mut counter_keys = std::collections::HashSet::new();
        let mut metadata_keys = std::collections::HashSet::new();
        let mut has_actor_metadata = false;
        let mut has_room_metadata = false;

        for entry in self.actor_connections.iter() {
            if !entry.value().is_empty() {
                counter_keys.insert(self.actor_counter_key(entry.key()));
                metadata_keys.insert(self.actor_index_key(entry.key()));
                has_actor_metadata = true;
            }
        }
        for entry in self.room_connections.iter() {
            if !entry.value().is_empty() {
                counter_keys.insert(self.room_counter_key(entry.key()));
                metadata_keys.insert(self.room_index_key(entry.key()));
                has_room_metadata = true;
            }
        }

        if has_actor_metadata {
            metadata_keys.insert(self.actor_index_directory_key());
        }
        if has_room_metadata {
            metadata_keys.insert(self.room_index_directory_key());
        }

        // Refresh per-connection metadata TTLs alongside aggregate counters.
        for entry in self.connections.iter() {
            metadata_keys.insert(self.conn_metadata_key(entry.key()));
        }

        // Also refresh the total connections counter TTL
        if self.connection_count() > 0 {
            let total_key = self.total_counter_key();
            counter_keys.insert(total_key);
        }

        let total_keys = counter_keys.len() + metadata_keys.len();
        if total_keys == 0 {
            return;
        }

        let mut failure_count = 0u64;
        let mut success_count = 0u64;

        // Use batched Lua script for efficient TTL refresh
        // This reduces network round-trips compared to individual EXPIRE commands
        let result = self
            .batch_refresh_ttls_with_lua(&mut conn, &counter_keys, &metadata_keys)
            .await;

        match result {
            Ok(refreshed) => {
                success_count = usize_to_u64_saturating(refreshed);
            }
            Err(e) => {
                failure_count = usize_to_u64_saturating(total_keys);
                warn!("Failed to refresh TTLs via Lua script ({total_keys} keys): {e}");
            }
        }

        // Update monitoring metrics
        if success_count > 0 {
            synctv_core::metrics::cluster::DISTRIBUTED_COUNTER_TTL_REFRESHES
                .with_label_values(&["success"])
                .inc_by(success_count);
        }
        if failure_count > 0 {
            synctv_core::metrics::cluster::DISTRIBUTED_COUNTER_TTL_REFRESHES
                .with_label_values(&["failure"])
                .inc_by(failure_count);
            let consecutive =
                synctv_core::metrics::cluster::DISTRIBUTED_COUNTER_TTL_CONSECUTIVE_FAILURES.get()
                    + 1;
            synctv_core::metrics::cluster::DISTRIBUTED_COUNTER_TTL_CONSECUTIVE_FAILURES
                .set(consecutive);
            if consecutive >= 3 {
                warn!(
                    consecutive_failures = consecutive,
                    "ALERT: Distributed counter TTL refresh has failed {} consecutive times. \
                     Connection rate limiting across replicas may stop working if counters expire.",
                    consecutive
                );
            }
        } else if !counter_keys.is_empty() || !metadata_keys.is_empty() {
            // Reset consecutive failure counter on full success
            synctv_core::metrics::cluster::DISTRIBUTED_COUNTER_TTL_CONSECUTIVE_FAILURES.set(0);
        }

        let total_refreshed =
            usize_to_i64_saturating(counter_keys.len().saturating_add(metadata_keys.len()));
        synctv_core::metrics::cluster::DISTRIBUTED_COUNTER_TTL_KEYS_REFRESHED.set(total_refreshed);

        if !counter_keys.is_empty() || !metadata_keys.is_empty() {
            debug!(
                counter_keys = counter_keys.len(),
                metadata_keys = metadata_keys.len(),
                failures = failure_count,
                "Refreshed TTLs on distributed counters and connection metadata"
            );
        }

        // Repair this node's local contribution after the TTL refresh.
        // Global stale-index cleanup is intentionally not run on every tick:
        // crashed-pod state now drains via short metadata/index TTLs plus
        // lazy pruning on distributed read paths.
        self.sync_local_counts_to_redis(&mut conn).await;
        self.sync_connection_metadata_to_redis(&mut conn).await;
    }

    /// Batch refresh TTLs using a Lua script for efficiency.
    ///
    /// Processes keys in batches of `TTL_REFRESH_BATCH_SIZE` to avoid
    /// excessive memory usage and network payload sizes.
    async fn batch_refresh_ttls_with_lua(
        &self,
        conn: &mut redis::aio::ConnectionManager,
        counter_keys: &std::collections::HashSet<String>,
        metadata_keys: &std::collections::HashSet<String>,
    ) -> Result<usize, String> {
        let counter_keys_vec: Vec<&String> = counter_keys.iter().collect();
        let metadata_keys_vec: Vec<&String> = metadata_keys.iter().collect();
        let total_keys = counter_keys_vec.len() + metadata_keys_vec.len();
        let mut total_refreshed = 0usize;

        // Process in batches to avoid oversized Lua script payloads
        let mut counter_offset = 0usize;
        let mut metadata_offset = 0usize;

        while counter_offset < counter_keys_vec.len() || metadata_offset < metadata_keys_vec.len() {
            // Collect a batch of keys
            let mut batch_keys: Vec<&String> = Vec::with_capacity(TTL_REFRESH_BATCH_SIZE);
            let mut batch_counter_count = 0usize;
            let mut batch_metadata_count = 0usize;

            // Add counter keys to batch
            while counter_offset < counter_keys_vec.len()
                && batch_keys.len() < TTL_REFRESH_BATCH_SIZE
            {
                batch_keys.push(counter_keys_vec[counter_offset]);
                batch_counter_count += 1;
                counter_offset += 1;
            }

            // Add metadata keys to batch
            while metadata_offset < metadata_keys_vec.len()
                && batch_keys.len() < TTL_REFRESH_BATCH_SIZE
            {
                batch_keys.push(metadata_keys_vec[metadata_offset]);
                batch_metadata_count += 1;
                metadata_offset += 1;
            }

            if batch_keys.is_empty() {
                break;
            }

            // Build and execute the Lua script for this batch
            let mut script_invocation = BATCH_REFRESH_TTLS_SCRIPT.prepare_invoke();
            for key in &batch_keys {
                script_invocation.key(*key);
            }
            script_invocation
                .arg(DISTRIBUTED_COUNTER_TTL_SECONDS)
                .arg(CONNECTION_METADATA_TTL_SECONDS)
                .arg(usize_to_i64_saturating(batch_counter_count))
                .arg(usize_to_i64_saturating(batch_metadata_count));

            let refreshed: i64 = self
                .redis_op("refresh distributed counter TTL batch", async {
                    script_invocation.invoke_async(conn).await
                })
                .await?;
            total_refreshed += i64_to_usize_saturating(refreshed);

            debug!(
                batch_size = batch_keys.len(),
                refreshed = refreshed,
                total_refreshed = total_refreshed,
                remaining = total_keys.saturating_sub(total_refreshed),
                "Batch TTL refresh completed"
            );
        }

        Ok(total_refreshed)
    }

    /// Synchronize local connection counts to Redis distributed counters.
    ///
    /// This method compares local connection counts with Redis counter values
    /// and corrects any discrepancies. This is important for recovering from
    /// situations where Redis was temporarily unavailable during connection
    /// registration or unregistration.
    ///
    /// The synchronization is intentionally one-sided: it repairs counters that
    /// are missing or lower than this node's local contribution, but never
    /// decreases a Redis counter based only on local state. Lowering a
    /// distributed counter from one replica would overwrite connections that are
    /// still legitimately active on other replicas.
    async fn sync_local_counts_to_redis(&self, conn: &mut redis::aio::ConnectionManager) {
        // Collect local counts first (avoid holding locks during Redis operations)
        let mut actor_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for entry in self.actor_connections.iter() {
            let count = entry.value().len();
            if count > 0 {
                let key = self.actor_counter_key(entry.key());
                actor_counts.insert(key, count);
            }
        }

        let mut room_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for entry in self.room_connections.iter() {
            let count = entry.value().len();
            if count > 0 {
                let key = self.room_counter_key(entry.key());
                room_counts.insert(key, count);
            }
        }

        let local_total = self.connection_count();
        let total_key = self.total_counter_key();

        // Lua script to atomically repair counters that are missing or lower than
        // this node's observed minimum contribution. It never decreases the
        // current Redis value because other replicas may have active
        // connections that are not visible from this node's local memory.
        // Returns `{current_value, 1}` when the counter was raised and
        // `{current_value, 0}` when no change was needed.
        let mut sync_count = 0u64;
        let mut sync_errors = 0u64;

        // Sync actor counters
        for (key, local_count) in &actor_counts {
            let script_result: Result<Vec<i64>, _> = self
                .redis_op("sync actor connection counter", async {
                    SYNC_COUNTER_MIN_SCRIPT
                        .key(key)
                        .arg(usize_to_i64_saturating(*local_count))
                        .arg(DISTRIBUTED_COUNTER_TTL_SECONDS)
                        .invoke_async(conn)
                        .await
                })
                .await;
            match script_result {
                Ok(result) if result.len() >= 2 => {
                    let old_value = result[0];
                    let was_changed = result[1];
                    if was_changed == 1 {
                        sync_count += 1;
                        debug!(
                            key = %key,
                            old_value = old_value,
                            new_value = *local_count,
                            "Raised actor connection counter in Redis to cover local connections"
                        );
                    }
                }
                Ok(_) => {
                    // Unexpected result format
                    warn!(key = %key, "Unexpected result format from Redis sync script");
                }
                Err(e) => {
                    sync_errors += 1;
                    warn!(key = %key, error = %e, "Failed to sync actor counter to Redis");
                }
            }
        }

        // Sync room counters
        for (key, local_count) in &room_counts {
            let script_result: Result<Vec<i64>, _> = self
                .redis_op("sync room connection counter", async {
                    SYNC_COUNTER_MIN_SCRIPT
                        .key(key)
                        .arg(usize_to_i64_saturating(*local_count))
                        .arg(DISTRIBUTED_COUNTER_TTL_SECONDS)
                        .invoke_async(conn)
                        .await
                })
                .await;
            match script_result {
                Ok(result) if result.len() >= 2 => {
                    let old_value = result[0];
                    let was_changed = result[1];
                    if was_changed == 1 {
                        sync_count += 1;
                        debug!(
                            key = %key,
                            old_value = old_value,
                            new_value = *local_count,
                            "Raised room connection counter in Redis to cover local connections"
                        );
                    }
                }
                Ok(_) => {
                    warn!(key = %key, "Unexpected result format from Redis sync script");
                }
                Err(e) => {
                    sync_errors += 1;
                    warn!(key = %key, error = %e, "Failed to sync room counter to Redis");
                }
            }
        }

        let script_result: Result<Vec<i64>, _> = self
            .redis_op("sync total connection counter", async {
                SYNC_COUNTER_MIN_SCRIPT
                    .key(&total_key)
                    .arg(usize_to_i64_saturating(local_total))
                    .arg(DISTRIBUTED_COUNTER_TTL_SECONDS)
                    .invoke_async(conn)
                    .await
            })
            .await;
        match script_result {
            Ok(result) if result.len() >= 2 => {
                let old_value = result[0];
                let was_changed = result[1];
                if was_changed == 1 {
                    sync_count += 1;
                    warn!(
                        key = %total_key,
                        old_value = old_value,
                        new_value = local_total,
                        "Raised total connection counter in Redis to cover local connections"
                    );
                }
            }
            Ok(_) => {
                warn!(key = %total_key, "Unexpected result format from Redis sync script");
            }
            Err(e) => {
                sync_errors += 1;
                warn!(key = %total_key, error = %e, "Failed to sync total counter to Redis");
            }
        }

        if sync_count > 0 || sync_errors > 0 {
            info!(
                counters_synced = sync_count,
                sync_errors = sync_errors,
                "Completed distributed counter synchronization"
            );
        }
    }

    /// Reconcile in-memory connection state with Redis after an outage recovery.
    ///
    /// This method performs a full reconciliation between local state and Redis:
    /// 1. Syncs local connection counts to Redis counters
    /// 2. Writes missing connection metadata to Redis
    /// 3. Cleans up stale Redis actor/room index members that reference missing metadata
    ///
    /// # When to Call
    ///
    /// This method should be called:
    /// - Periodically by a background task (every 60s by default)
    /// - After detecting Redis has recovered from an outage
    /// - On startup to recover from previous unclean shutdowns
    ///
    /// # Trade-offs
    ///
    /// - **Pros**: Ensures eventual consistency, handles partial failures
    /// - **Cons**: Can be expensive with many connections; uses Redis round-trips
    ///
    /// # Errors
    ///
    /// Errors are logged but do not propagate. The method is designed to be
    /// eventually consistent - failures are retried on the next call.
    pub async fn reconcile_with_redis(&self) {
        let Some(mut conn) = self.redis_conn_snapshot().await else {
            // No Redis configured - nothing to reconcile
            return;
        };

        // Step 1: Sync connection counters (existing logic)
        self.sync_local_counts_to_redis(&mut conn).await;

        // Step 2: Sync connection metadata to Redis
        self.sync_connection_metadata_to_redis(&mut conn).await;

        // Step 3: Clean up stale Redis actor/room index members that point to
        // missing connection metadata.
        // Important: this must NOT delete `conn_mgr:conn:*` keys globally just
        // because this replica does not know about them. Those keys may belong
        // to healthy connections on other replicas.
        self.cleanup_stale_redis_indexes(&mut conn).await;
    }

    /// Sync local connection metadata to Redis.
    ///
    /// Writes metadata for all active connections. Uses SET with TTL to ensure
    /// keys are eventually cleaned up even if the node crashes.
    async fn sync_connection_metadata_to_redis(&self, conn: &mut redis::aio::ConnectionManager) {
        use redis::AsyncCommands;

        let mut synced = 0u64;
        let mut errors = 0u64;
        let actor_index_directory_key = self.actor_index_directory_key();
        let room_index_directory_key = self.room_index_directory_key();
        let mut has_actor_index = false;
        let mut has_room_index = false;

        for entry in self.connections.iter() {
            let conn_info = entry.value();
            let key = self.conn_metadata_key(&conn_info.connection_id);
            let actor_key = conn_info.actor.connection_key();
            let actor_index_key = self.actor_index_key(&actor_key);
            let room_index_key = conn_info
                .room_id
                .as_ref()
                .map(|room_id| self.room_index_key(room_id));
            let persistent = ConnectionInfoPersistent::from(conn_info);

            match serde_json::to_string(&persistent) {
                Ok(json_data) => {
                    let result: Result<(), _> = self
                        .redis_op(
                            "sync connection metadata",
                            conn.set_ex(
                                &key,
                                json_data,
                                i64_to_u64_saturating(CONNECTION_METADATA_TTL_SECONDS),
                            ),
                        )
                        .await;

                    match result {
                        Ok(()) => {
                            synced += 1;
                        }
                        Err(e) => {
                            errors += 1;
                            warn!(
                                connection_id = %conn_info.connection_id,
                                error = %e,
                                "Failed to sync connection metadata to Redis"
                            );
                        }
                    }
                }
                Err(e) => {
                    errors += 1;
                    warn!(
                        connection_id = %conn_info.connection_id,
                        error = %e,
                        "Failed to serialize connection metadata"
                    );
                }
            }

            if let Err(e) = self
                .redis_op(
                    "repair actor connection index membership",
                    conn.sadd::<_, _, ()>(&actor_index_key, &conn_info.connection_id),
                )
                .await
            {
                errors += 1;
                warn!(
                    connection_id = %conn_info.connection_id,
                    actor = %conn_info.actor,
                    error = %e,
                    "Failed to repair actor connection index membership in Redis"
                );
            }
            let _: Result<(), _> = self
                .redis_op(
                    "repair actor connection index directory",
                    conn.sadd(&actor_index_directory_key, &actor_index_key),
                )
                .await;
            has_actor_index = true;
            let _: Result<(), _> = self
                .redis_op(
                    "refresh actor connection index TTL",
                    conn.expire(&actor_index_key, CONNECTION_METADATA_TTL_SECONDS),
                )
                .await;

            if let Some(room_index_key) = room_index_key.as_ref() {
                if let Err(e) = self
                    .redis_op(
                        "repair room connection index membership",
                        conn.sadd::<_, _, ()>(room_index_key, &conn_info.connection_id),
                    )
                    .await
                {
                    errors += 1;
                    warn!(
                        connection_id = %conn_info.connection_id,
                        room_id = %room_index_key,
                        error = %e,
                        "Failed to repair room connection index membership in Redis"
                    );
                }
                let _: Result<(), _> = self
                    .redis_op(
                        "repair room connection index directory",
                        conn.sadd(&room_index_directory_key, room_index_key),
                    )
                    .await;
                has_room_index = true;
                let _: Result<(), _> = self
                    .redis_op(
                        "refresh room connection index TTL",
                        conn.expire(room_index_key, CONNECTION_METADATA_TTL_SECONDS),
                    )
                    .await;
            }
        }

        if has_actor_index {
            let _: Result<(), _> = self
                .redis_op(
                    "refresh actor connection index directory TTL",
                    conn.expire(&actor_index_directory_key, CONNECTION_METADATA_TTL_SECONDS),
                )
                .await;
        }
        if has_room_index {
            let _: Result<(), _> = self
                .redis_op(
                    "refresh room connection index directory TTL",
                    conn.expire(&room_index_directory_key, CONNECTION_METADATA_TTL_SECONDS),
                )
                .await;
        }

        if synced > 0 || errors > 0 {
            debug!(
                metadata_synced = synced,
                metadata_errors = errors,
                "Synced connection metadata to Redis"
            );
        }
    }

    async fn load_index_directory_members(
        &self,
        conn: &mut redis::aio::ConnectionManager,
        directory_key: &str,
    ) -> Result<Vec<String>, String> {
        use redis::AsyncCommands;

        self.redis_op(
            "fetch distributed index directory",
            conn.smembers(directory_key),
        )
        .await
    }

    async fn prune_index_directory_members(
        &self,
        conn: &mut redis::aio::ConnectionManager,
        directory_key: &str,
        index_keys: &[String],
    ) -> Result<(), String> {
        if index_keys.is_empty() {
            return Ok(());
        }

        let mut pipe = redis::pipe();
        for index_key in index_keys {
            pipe.srem(directory_key, index_key).ignore();
        }

        self.redis_op("prune distributed index directory members", async {
            pipe.query_async::<()>(&mut *conn).await
        })
        .await
    }

    pub(super) async fn load_valid_connection_ids_from_index(
        &self,
        conn: &mut redis::aio::ConnectionManager,
        index_key: &str,
        expected_user_id: Option<&UserId>,
        expected_room_id: Option<&RoomId>,
    ) -> Result<Vec<String>, String> {
        use redis::AsyncCommands;

        let conn_ids: Vec<String> = self
            .redis_op("fetch distributed connection index", async {
                conn.smembers(index_key).await
            })
            .await?;
        if conn_ids.is_empty() {
            return Ok(Vec::new());
        }

        let metadata_keys: Vec<String> = conn_ids
            .iter()
            .map(|conn_id| format!("{}conn_mgr:conn:{conn_id}", self.redis_key_prefix))
            .collect();
        let metadata: Vec<Option<String>> = self
            .redis_op("fetch distributed connection metadata", async {
                conn.mget(metadata_keys).await
            })
            .await?;

        let mut valid_conn_ids = Vec::with_capacity(conn_ids.len());
        let mut stale_members = Vec::new();

        for (conn_id, metadata_json) in conn_ids.into_iter().zip(metadata) {
            match metadata_json {
                Some(metadata_json) => {
                    match serde_json::from_str::<ConnectionInfoPersistent>(&metadata_json) {
                        Ok(info) => {
                            let matches_user = expected_user_id
                                .is_none_or(|user_id| info.actor.user_id() == Some(*user_id));
                            let matches_room = expected_room_id
                                .is_none_or(|room_id| info.room_id.as_ref() == Some(room_id));

                            if matches_user && matches_room {
                                valid_conn_ids.push(conn_id);
                            } else {
                                stale_members.push(conn_id);
                            }
                        }
                        Err(error) => {
                            warn!(
                                index_key = %index_key,
                                connection_id = %conn_id,
                                error = %error,
                                "Failed to deserialize distributed connection metadata; pruning index member"
                            );
                            stale_members.push(conn_id);
                        }
                    }
                }
                None => {
                    stale_members.push(conn_id);
                }
            }
        }

        if !stale_members.is_empty() {
            let mut pipe = redis::pipe();
            for connection_id in &stale_members {
                pipe.srem(index_key, connection_id).ignore();
            }

            match self
                .redis_op("prune stale distributed connection index members", async {
                    pipe.query_async::<()>(&mut *conn).await
                })
                .await
            {
                Ok(()) => {
                    debug!(
                        index_key = %index_key,
                        removed_members = stale_members.len(),
                        "Pruned stale distributed connection index members on read"
                    );
                }
                Err(error) => {
                    warn!(
                        index_key = %index_key,
                        removed_members = stale_members.len(),
                        error = %error,
                        "Failed to prune stale distributed connection index members on read"
                    );
                }
            }
        }

        Ok(valid_conn_ids)
    }

    /// Clean up stale Redis actor/room index members whose metadata key is gone.
    ///
    /// This only removes index members that are provably invalid:
    /// - `conn_mgr:actor:*` set members without a matching `conn_mgr:conn:*`
    /// - `conn_mgr:room:*` set members without a matching `conn_mgr:conn:*`
    ///
    /// It deliberately does not delete arbitrary `conn_mgr:conn:*` keys by
    /// scanning Redis and comparing against local memory. In a multi-replica
    /// cluster, metadata for connections on other replicas is valid and must
    /// not be removed by this node.
    async fn cleanup_stale_redis_indexes(&self, conn: &mut redis::aio::ConnectionManager) {
        use redis::AsyncCommands;

        let directories = [
            self.actor_index_directory_key(),
            self.room_index_directory_key(),
        ];
        let mut cleaned = 0u64;
        let mut errors = 0u64;

        for directory_key in directories {
            let index_keys = match self
                .load_index_directory_members(conn, &directory_key)
                .await
            {
                Ok(index_keys) => index_keys,
                Err(error) => {
                    errors += 1;
                    warn!(
                        directory_key = %directory_key,
                        error = %error,
                        "Failed to load distributed connection index directory during reconciliation"
                    );
                    continue;
                }
            };

            let mut stale_directory_members = Vec::new();

            for key in index_keys {
                let members: Result<Vec<String>, _> = self
                    .redis_op("fetch Redis index members", conn.smembers(&key))
                    .await;
                let members = match members {
                    Ok(members) => members,
                    Err(e) => {
                        errors += 1;
                        warn!(
                            key = %key,
                            error = %e,
                            "Failed to fetch Redis index members during reconciliation"
                        );
                        continue;
                    }
                };

                for conn_id in members {
                    let conn_key = self.conn_metadata_key(&conn_id);
                    let exists: Result<bool, _> = self
                        .redis_op(
                            "verify distributed connection metadata",
                            conn.exists(&conn_key),
                        )
                        .await;
                    match exists {
                        Ok(true) => {}
                        Ok(false) => {
                            let remove_result: Result<(), _> = self
                                .redis_op(
                                    "remove stale distributed connection index member",
                                    conn.srem(&key, &conn_id),
                                )
                                .await;
                            match remove_result {
                                Ok(()) => {
                                    cleaned += 1;
                                    debug!(
                                        index_key = %key,
                                        connection_id = %conn_id,
                                        "Removed stale distributed connection index member"
                                    );
                                }
                                Err(e) => {
                                    errors += 1;
                                    warn!(
                                        index_key = %key,
                                        connection_id = %conn_id,
                                        error = %e,
                                        "Failed to remove stale distributed connection index member"
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            errors += 1;
                            warn!(
                                index_key = %key,
                                connection_id = %conn_id,
                                error = %e,
                                "Failed to verify distributed connection metadata during reconciliation"
                            );
                        }
                    }
                }

                let key_is_empty: Result<bool, _> = self
                    .redis_op(
                        "check Redis index cardinality",
                        conn.scard::<_, usize>(&key),
                    )
                    .await
                    .map(|count| count == 0);
                match key_is_empty {
                    Ok(true) => {
                        let _: Result<(), _> = self
                            .redis_op("delete empty Redis index", conn.del(&key))
                            .await;
                        stale_directory_members.push(key);
                    }
                    Ok(false) => {}
                    Err(e) => {
                        errors += 1;
                        warn!(
                            key = %key,
                            error = %e,
                            "Failed to check Redis index cardinality during reconciliation"
                        );
                    }
                }
            }

            if let Err(error) = self
                .prune_index_directory_members(conn, &directory_key, &stale_directory_members)
                .await
            {
                errors += 1;
                warn!(
                    directory_key = %directory_key,
                    error = %error,
                    "Failed to prune stale distributed connection directory members"
                );
            }
        }

        if cleaned > 0 || errors > 0 {
            info!(
                stale_index_members_cleaned = cleaned,
                cleanup_errors = errors,
                "Cleaned up stale distributed connection indexes from Redis"
            );
        }
    }

    /// Spawn a background task that periodically refreshes TTLs on distributed
    /// connection counters in Redis.
    ///
    /// This prevents the crash-safety TTL from expiring while long-lived
    /// connections are still active. Runs every 60 seconds by default.
    #[must_use]
    pub fn spawn_ttl_refresh_task(
        &self,
        interval: Duration,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let manager = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // Skip the first immediate tick
            ticker.tick().await;
            loop {
                tokio::select! {
                    () = cancel_token.cancelled() => {
                        info!("Distributed counter TTL refresh task shutting down");
                        return;
                    }
                    _ = ticker.tick() => {
                        manager.refresh_distributed_counter_ttls().await;
                    }
                }
            }
        })
    }

    /// Get connection ID for a user in a specific room
    ///
    /// Returns the first active connection ID found for the user in the room.
    /// For WebRTC, this allows us to identify which connection a user is using in a room.
    #[must_use]
    pub fn get_connection_id(&self, room_id: &RoomId, user_id: &UserId) -> Option<String> {
        // Collect IDs first to avoid holding cross-DashMap locks
        let conn_ids: Vec<String> = self
            .actor_connections
            .get(&Self::user_actor_key(user_id))
            .map(|ids| ids.clone())
            .unwrap_or_default();

        // Find the first connection that's in the specified room
        for conn_id in &conn_ids {
            if let Some(conn) = self.connections.get(conn_id) {
                if conn.room_id.as_ref() == Some(room_id) {
                    return Some(conn.connection_id.clone());
                }
            }
        }
        None
    }

    pub async fn try_join_voice_rtc(
        &self,
        room_id: &RoomId,
        actor: &synctv_core::models::RealtimeActor,
        conn_id: &str,
        max_participants: usize,
    ) -> Result<VoiceRtcJoinOutcome, String> {
        let room_lock = self.voice_room_lock(room_id);
        let _room_guard = room_lock.lock().await;
        let current = self
            .get_connection(conn_id)
            .ok_or_else(|| "Connection not found".to_string())?;
        if &current.actor != actor || current.room_id.as_ref() != Some(room_id) {
            return Err("Connection is not in this room".to_string());
        }
        if current.voice_rtc_joined {
            return Ok(VoiceRtcJoinOutcome::AlreadyJoined);
        }

        let distributed_reserved = if let Some(mut redis) = self
            .redis_conn_snapshot_required(
                "Distributed voice chat capacity check unavailable while Redis is degraded",
            )
            .await?
        {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let session_seconds = self.limits.webrtc_session_timeout.as_secs().max(1);
            let expires_at = now.saturating_add(session_seconds);
            let key_ttl = session_seconds.saturating_add(60);
            let result: i64 = self
                .redis_op(
                    "reserve distributed voice chat participant",
                    VOICE_RTC_JOIN_SCRIPT
                        .key(self.voice_room_key(room_id))
                        .arg(conn_id)
                        .arg(now)
                        .arg(expires_at)
                        .arg(max_participants)
                        .arg(key_ttl)
                        .invoke_async(&mut redis),
                )
                .await
                .map_err(|error| {
                    format!("Distributed voice chat capacity check failed: {error}")
                })?;
            if result == 0 {
                return Ok(VoiceRtcJoinOutcome::RoomAtCapacity);
            }
            true
        } else {
            if self.get_voice_rtc_connections(room_id).len() >= max_participants {
                return Ok(VoiceRtcJoinOutcome::RoomAtCapacity);
            }
            false
        };

        self.mark_voice_rtc_joined(room_id, actor, conn_id, true);
        if let Err(error) = self.sync_connection_metadata_distributed(conn_id).await {
            self.mark_voice_rtc_joined(room_id, actor, conn_id, false);
            if distributed_reserved {
                self.release_voice_rtc_slot_best_effort(room_id, conn_id)
                    .await;
            }
            return Err(error);
        }
        Ok(VoiceRtcJoinOutcome::Joined)
    }

    pub async fn leave_voice_rtc(
        &self,
        room_id: &RoomId,
        actor: &synctv_core::models::RealtimeActor,
        conn_id: &str,
    ) -> Result<bool, String> {
        let room_lock = self.voice_room_lock(room_id);
        let _room_guard = room_lock.lock().await;
        let current = self
            .get_connection(conn_id)
            .ok_or_else(|| "Connection not found".to_string())?;
        if &current.actor != actor || current.room_id.as_ref() != Some(room_id) {
            return Err("Connection is not in this room".to_string());
        }
        if !current.voice_rtc_joined {
            return Ok(false);
        }

        self.mark_voice_rtc_joined(room_id, actor, conn_id, false);
        if let Err(error) = self.sync_connection_metadata_distributed(conn_id).await {
            self.mark_voice_rtc_joined(room_id, actor, conn_id, true);
            return Err(error);
        }
        self.release_voice_rtc_slot_best_effort(room_id, conn_id)
            .await;
        Ok(true)
    }

    pub(super) async fn release_voice_rtc_slot_best_effort(&self, room_id: &RoomId, conn_id: &str) {
        let Some(mut redis) = self.redis_conn_snapshot().await else {
            return;
        };
        if let Err(error) = self
            .redis_op(
                "release distributed voice chat participant",
                redis.zrem::<_, _, ()>(self.voice_room_key(room_id), conn_id),
            )
            .await
        {
            warn!(
                room_id = %room_id,
                connection_id = %conn_id,
                error = %error,
                "Failed to release distributed voice chat participant; expiry remains as safety net"
            );
        }
    }

    /// Mark a connection as joined or left WebRTC session
    ///
    /// This is used to track which connections are actively participating in WebRTC calls.
    pub fn mark_voice_rtc_joined(
        &self,
        room_id: &RoomId,
        actor: &synctv_core::models::RealtimeActor,
        conn_id: &str,
        joined: bool,
    ) {
        // Verify the connection belongs to the user and room
        if let Some(mut conn) = self.connections.get_mut(conn_id) {
            if &conn.actor == actor && conn.room_id.as_ref() == Some(room_id) {
                conn.voice_rtc_joined = joined;
                // Set or clear the RTC join timestamp
                conn.voice_rtc_joined_at = if joined { Some(Instant::now()) } else { None };
                if let Some(voice_rtc_joined_at) = conn.voice_rtc_joined_at {
                    self.schedule_voice_rtc_timeout(conn_id, voice_rtc_joined_at);
                } else {
                    self.clear_voice_rtc_timeout(conn_id);
                }
                debug!(
                    connection_id = %conn_id,
                    actor = %actor,
                    room_id = %room_id,
                    joined = joined,
                    "WebRTC join status updated"
                );
            }
        }
    }

    /// Replace the connection's voice and media P2P presence atomically.
    /// Get all connections in a room that have joined WebRTC
    #[must_use]
    pub fn get_voice_rtc_connections(&self, room_id: &RoomId) -> Vec<ConnectionInfo> {
        // Collect IDs first to avoid holding cross-DashMap locks
        let conn_ids: Vec<String> = self
            .room_connections
            .get(room_id)
            .map(|ids| ids.clone())
            .unwrap_or_default();

        conn_ids
            .iter()
            .filter_map(|id| self.connections.get(id).map(|c| c.clone()))
            .filter(|conn| conn.voice_rtc_joined)
            .collect()
    }

    /// Spawn a background task that periodically checks for idle/expired connections
    /// and sends disconnect signals for them.
    ///
    /// The task runs every `interval` and stops gracefully when `cancel_token` is cancelled.
    #[must_use]
    pub fn spawn_cleanup_task(
        &self,
        interval: Duration,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let manager = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // Skip the first immediate tick
            ticker.tick().await;
            loop {
                tokio::select! {
                    () = cancel_token.cancelled() => {
                        info!("Connection cleanup task shutting down");
                        return;
                    }
                    _ = ticker.tick() => {
                        let stale = manager.check_timeouts();
                        if !stale.is_empty() {
                            info!(
                                count = stale.len(),
                                "Cleaning up stale connections"
                            );
                            for conn_id in &stale {
                                manager.disconnect_connection(conn_id);
                                manager.unregister(conn_id).await;
                            }
                        }
                    }
                }
            }
        })
    }

    /// Get all connection IDs for a user across all replicas (from Redis).
    ///
    /// Returns connection IDs from Redis, which includes connections from
    /// all replicas in the cluster.
    ///
    /// When Redis-backed distributed state is unavailable, this fails closed
    /// instead of silently degrading to local-only state. In distributed mode,
    /// a local fallback would return a partial view and break admin/security
    /// operations that require a global connection set.
    pub async fn get_user_connections_distributed(
        &self,
        user_id: &UserId,
    ) -> Result<Vec<String>, String> {
        if let Some(mut conn) = self
            .redis_conn_snapshot_required(
                "Distributed user connection lookup unavailable while Redis is degraded",
            )
            .await?
        {
            let user_index_key = self.actor_index_key(Self::user_actor_key(user_id));

            match self
                .load_valid_connection_ids_from_index(
                    &mut conn,
                    &user_index_key,
                    Some(user_id),
                    None,
                )
                .await
            {
                Ok(conn_ids) => return Ok(conn_ids),
                Err(e) => {
                    warn!("Failed to fetch user connections from Redis: {e}");
                    return Err(
                        "Distributed user connection lookup unavailable while Redis is degraded"
                            .to_string(),
                    );
                }
            }
        }

        Ok(self
            .get_user_connections(user_id)
            .into_iter()
            .map(|c| c.connection_id)
            .collect())
    }

    /// Get the total number of active connections for a user across all replicas.
    ///
    /// In standalone mode this uses local in-memory state. In distributed mode it
    /// derives the count from the Redis-backed distributed connection index.
    pub async fn user_connection_count_distributed(
        &self,
        user_id: &UserId,
    ) -> Result<usize, String> {
        Ok(self.get_user_connections_distributed(user_id).await?.len())
    }

    /// Get all connections in a room across all replicas (from Redis).
    ///
    /// Returns connection IDs from Redis, which includes connections from
    /// all replicas in the cluster.
    ///
    /// When Redis-backed distributed state is unavailable, this fails closed
    /// instead of silently degrading to local-only state.
    pub async fn get_room_connections_distributed(
        &self,
        room_id: &RoomId,
    ) -> Result<Vec<String>, String> {
        if let Some(mut conn) = self
            .redis_conn_snapshot_required(
                "Distributed room connection lookup unavailable while Redis is degraded",
            )
            .await?
        {
            let room_index_key = format!("{}conn_mgr:room:{}", self.redis_key_prefix, room_id);

            match self
                .load_valid_connection_ids_from_index(
                    &mut conn,
                    &room_index_key,
                    None,
                    Some(room_id),
                )
                .await
            {
                Ok(conn_ids) => return Ok(conn_ids),
                Err(e) => {
                    warn!("Failed to fetch room connections from Redis: {e}");
                    return Err(
                        "Distributed room connection lookup unavailable while Redis is degraded"
                            .to_string(),
                    );
                }
            }
        }

        Ok(self
            .get_room_connections(room_id)
            .into_iter()
            .map(|c| c.connection_id)
            .collect())
    }

    /// Get the number of active client connections for a user in a room across all replicas.
    ///
    /// This counts every client connection for the specific user in the room.
    pub async fn user_connection_count_in_room_distributed(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
    ) -> Result<usize, String> {
        if let Some(mut conn) = self
            .redis_conn_snapshot_required(
                "Distributed user room connection count unavailable while Redis is degraded",
            )
            .await?
        {
            let user_index_key = self.actor_index_key(Self::user_actor_key(user_id));
            let conn_ids = self
                .load_valid_connection_ids_from_index(
                    &mut conn,
                    &user_index_key,
                    Some(user_id),
                    Some(room_id),
                )
                .await?;
            return Ok(conn_ids.len());
        }

        Ok(self
            .get_user_connections(user_id)
            .into_iter()
            .filter(|conn| conn.room_id.as_ref() == Some(room_id))
            .count())
    }

    /// Atomically increment a Redis counter, set its TTL, and check if the new
    /// value exceeds the limit.
    ///
    /// Uses a Lua script to make INCR + EXPIRE atomic, preventing a crash between
    /// the two operations from leaving a key without a TTL.
    ///
    /// Returns `Ok(true)` if the counter was incremented and is within the limit,
    /// `Ok(false)` if the limit was exceeded (counter was still incremented and must be rolled back),
    /// or `Err` on Redis failure.
    pub(super) async fn redis_incr_and_check(&self, key: &str, max: usize) -> Result<bool, String> {
        let Some(mut conn) = self.redis_conn_snapshot().await else {
            return Err("Redis not configured".to_string());
        };

        // Lua script: atomically INCR the key and set TTL in a single round-trip.
        // Returns the new counter value after increment.
        let count: i64 = self
            .redis_op("increment distributed counter", async {
                INCR_EXPIRE_SCRIPT
                    .key(key)
                    .arg(DISTRIBUTED_COUNTER_TTL_SECONDS)
                    .invoke_async(&mut conn)
                    .await
            })
            .await?;

        Ok(count <= usize_to_i64_saturating(max))
    }

    /// Decrement a Redis counter atomically (best-effort, errors are logged but not propagated).
    ///
    /// Uses a Lua script to atomically DECR and DEL if the result is negative,
    /// avoiding a race where a concurrent INCR between DECR and SET(0) would be lost.
    pub(super) async fn redis_decr(&self, key: &str) -> Result<(), String> {
        let Some(mut conn) = self.redis_conn_snapshot().await else {
            return Err("Redis not configured".to_string());
        };
        self.redis_op("decrement distributed counter", async {
            DECR_DELETE_NEGATIVE_SCRIPT
                .key(key)
                .invoke_async::<i64>(&mut conn)
                .await
        })
        .await?;
        Ok(())
    }

    /// Test-only accessor for `refresh_distributed_counter_ttls`.
    ///
    /// **WARNING**: This method is for internal testing only. Do not use in production code.
    /// It exposes the internal TTL refresh mechanism for integration tests that verify
    /// the distributed counter TTL refresh behavior.
    #[doc(hidden)]
    pub async fn test_refresh_distributed_counter_ttls(&self) {
        self.refresh_distributed_counter_ttls().await;
    }
}
