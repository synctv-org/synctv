//! Read and reconcile room subscriptions across replicas.

use super::{ConnectionId, RoomMessageHub};
use redis::{AsyncCommands, aio::ConnectionManager};
use std::sync::LazyLock;
use synctv_core::{
    models::{RealtimeActor, RoomId},
    redis_runtime::index::{IndexKind, StaleMember, prune_stale_members},
};
use tracing::{info, warn};

struct StaleSubscriber {
    connection_id: String,
    mapping_key: String,
    actor: Vec<u8>,
    mapped_room: Option<Vec<u8>>,
}

// Each candidate came from two separate reads. Revalidate both observations
// before dropping the hash field, including when the old mapping was absent.
static PRUNE_SUBSCRIBERS: LazyLock<redis::Script> = LazyLock::new(|| {
    redis::Script::new(
        r"
        for i = 3, #KEYS do
            local arg = (i - 3) * 4 + 1
            local id, actor = ARGV[arg], ARGV[arg + 1]
            local mapping_matches
            if ARGV[arg + 2] == 'missing' then
                mapping_matches = redis.call('EXISTS', KEYS[i]) == 0
            else
                mapping_matches = redis.call('MGET', KEYS[i])[1] == ARGV[arg + 3]
            end
            if mapping_matches and redis.call('HGET', KEYS[1], id) == actor then
                redis.call('HDEL', KEYS[1], id)
            end
        end
        if redis.call('EXISTS', KEYS[1]) == 0 then
            redis.call('SREM', KEYS[2], KEYS[1])
        end
        return 1
        ",
    )
});

async fn prune_subscribers(
    conn: &mut ConnectionManager,
    room_key: &str,
    directory: &str,
    stale: &[StaleSubscriber],
) -> redis::RedisResult<()> {
    for batch in stale.chunks(128) {
        let mut script = PRUNE_SUBSCRIBERS.prepare_invoke();
        script.key(room_key).key(directory);
        for sub in batch {
            script
                .key(&sub.mapping_key)
                .arg(&sub.connection_id)
                .arg(&sub.actor)
                .arg(if sub.mapped_room.is_some() {
                    "value"
                } else {
                    "missing"
                })
                .arg(sub.mapped_room.as_deref().unwrap_or_default());
        }
        script.invoke_async::<()>(conn).await?;
    }
    Ok(())
}

impl RoomMessageHub {
    async fn prune_room_directory(
        &self,
        conn: &mut ConnectionManager,
        directory: &str,
        candidates: &[StaleMember],
    ) {
        if let Err(error) = self
            .redis_op(
                "prune empty rooms from directory",
                prune_stale_members(conn, IndexKind::Set, directory, candidates),
            )
            .await
        {
            warn!(%error, "Failed to prune empty rooms from subscription directory");
        }
    }

    /// Load all subscribers across replicas, or local subscribers on a local-only hub.
    pub async fn get_room_subscribers_distributed(
        &self,
        room_id: &RoomId,
    ) -> crate::Result<Vec<(RealtimeActor, ConnectionId)>> {
        let Some(mut conn) = self
            .redis_conn_clone("load distributed room subscribers")
            .await
            .map_err(crate::Error::Redis)?
        else {
            return Ok(self.get_room_subscribers(room_id));
        };
        let room_key = self.room_key(room_id);
        let directory = self.room_index_directory_key();
        let entries: Vec<(String, Vec<u8>)> = self
            .redis_op("load distributed room subscribers", conn.hgetall(&room_key))
            .await
            .map_err(crate::Error::Redis)?;
        if entries.is_empty() {
            self.prune_room_directory(
                &mut conn,
                &directory,
                &[StaleMember::missing_indexed_key(room_key)],
            )
            .await;
            return Ok(Vec::new());
        }

        let keys: Vec<_> = entries.iter().map(|(id, _)| self.conn_key(id)).collect();
        let mappings: Vec<Option<Vec<u8>>> = self
            .redis_op("load connection room mappings", conn.mget(&keys))
            .await
            .map_err(crate::Error::Redis)?;
        let mut subscribers = Vec::with_capacity(entries.len());
        let mut stale = Vec::new();
        for (((id, actor), mapping_key), mapped_room) in entries.into_iter().zip(keys).zip(mappings)
        {
            let parsed_room = mapped_room
                .as_deref()
                .and_then(|raw| std::str::from_utf8(raw).ok())
                .and_then(|value| value.parse::<RoomId>().ok());
            if parsed_room == Some(*room_id)
                && let Ok(parsed) = serde_json::from_slice::<RealtimeActor>(&actor)
            {
                subscribers.push((parsed, ConnectionId::new(id)));
            } else {
                stale.push(StaleSubscriber {
                    connection_id: id,
                    mapping_key,
                    actor,
                    mapped_room,
                });
            }
        }
        if !stale.is_empty()
            && let Err(error) = self
                .redis_op(
                    "prune stale room subscribers",
                    prune_subscribers(&mut conn, &room_key, &directory, &stale),
                )
                .await
        {
            warn!(%room_id, %error, "Failed to prune stale distributed room subscribers");
        }
        Ok(subscribers)
    }

    /// Audit replica-wide subscription state from Redis (observability only).
    ///
    /// Scans Redis for persisted subscription relationships and logs room/subscriber
    /// counts. This is an **observability tool**, not a recovery mechanism.
    ///
    /// This method does **not** populate the local `rooms` or `connections` DashMaps
    /// because:
    ///
    /// 1. **`MessageSender` cannot be recovered.** Each subscriber's `mpsc::Sender`
    ///    is only meaningful to the original WebSocket connection. Without a live
    ///    sender, messages cannot be routed, so inserting a synthetic `Subscriber` would
    ///    create a broken entry that either panics or silently drops events.
    ///
    /// 2. **Stale data from crashed replicas.** Redis may contain subscriptions from
    ///    nodes that crashed without unsubscribing. These will expire via TTL, but
    ///    recovering them into the local cache would create phantom subscribers.
    ///
    /// 3. **No lifecycle events.** Populating the local cache would emit spurious
    ///    `RoomActivated` events to the Redis Pub/Sub subscriber task, causing it
    ///    to subscribe to channels for rooms that have no real local subscribers.
    ///
    /// **Clients must reconnect** after a replica restart to re-establish their
    /// subscriptions via `subscribe()`. This method only logs what Redis knows
    /// about for dashboards and debugging.
    pub async fn audit_redis_subscriptions(&self) -> Result<usize, String> {
        info!(
            "Auditing cluster subscription state from Redis (observability only, clients must reconnect for message routing)"
        );
        let Some(mut conn_clone) = self.redis_conn_clone("audit room subscriptions").await? else {
            return Err("Redis not configured".to_string());
        };

        let room_index_directory_key = self.room_index_directory_key();
        let mut recovered = 0;
        let keys: Vec<String> = self
            .redis_op(
                "load room subscription index directory",
                conn_clone.smembers(&room_index_directory_key),
            )
            .await?;
        let mut stale_directory_members = Vec::with_capacity(keys.len());
        let room_key_prefix = self.room_key_prefix();

        for key in keys {
            // Extract room_id from key
            let room_id_str = key.trim_start_matches(&room_key_prefix);
            let Ok(room_id) = room_id_str.parse::<RoomId>() else {
                tracing::warn!(room_id = %room_id_str, "Ignoring invalid room hub room key");
                continue;
            };

            // Fetch all subscribers for this room
            let entries: Vec<(String, Vec<u8>)> = match self
                .redis_op("load audited room subscribers", conn_clone.hgetall(&key))
                .await
            {
                Ok(entries) => entries,
                Err(e) => {
                    warn!(
                        room_id = %room_id,
                        error = %e,
                        "Failed to fetch audited room subscribers from Redis"
                    );
                    continue;
                }
            };
            if entries.is_empty() {
                stale_directory_members.push(StaleMember::missing_indexed_key(key));
                continue;
            }

            let valid_subscriber_count = entries
                .iter()
                .filter(|(_, actor)| serde_json::from_slice::<RealtimeActor>(actor).is_ok())
                .count();
            recovered += valid_subscriber_count;

            info!(
                room_id = %room_id,
                subscriber_count = valid_subscriber_count,
                "Audited room subscription state from Redis (observability only)"
            );
        }

        self.prune_room_directory(
            &mut conn_clone,
            &room_index_directory_key,
            &stale_directory_members,
        )
        .await;

        Ok(recovered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use synctv_core_testing::{start_redis, test_redis_key_prefix};

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn prune_subscribers_preserves_repaired_actors_and_mappings() -> redis::RedisResult<()> {
        let (_redis, mut conn) = start_redis().await;
        let prefix = test_redis_key_prefix("prune-room");
        let directory = format!("{prefix}index");
        let room = format!("{prefix}room");
        let mut stale = Vec::new();
        let mut write = redis::pipe();
        write.sadd(&directory, &room).ignore();
        for (id, observed, current_actor, current_room) in [
            ("absent", None, "old", None),
            ("wrong-room", Some(2), "old", Some(2)),
            ("rejoined", None, "old", Some(1)),
            ("moved-back", Some(2), "old", Some(1)),
            ("repaired-actor", Some(1), "new", Some(1)),
        ] {
            let key = format!("{prefix}{id}");
            stale.push(StaleSubscriber {
                connection_id: id.to_owned(),
                mapping_key: key.clone(),
                actor: b"old".to_vec(),
                mapped_room: observed.map(|id: i64| id.to_string().into_bytes()),
            });
            write.hset(&room, id, current_actor).ignore();
            if let Some(current_room) = current_room {
                write.set(key, current_room).ignore();
            }
        }
        write.query_async::<()>(&mut conn).await?;
        prune_subscribers(&mut conn, &room, &directory, &stale).await?;
        let remaining: HashSet<String> = conn.hkeys(&room).await?;
        assert_eq!(
            remaining,
            HashSet::from(["rejoined", "moved-back", "repaired-actor"].map(String::from))
        );
        assert!(conn.sismember::<_, _, bool>(&directory, &room).await?);

        // Expired mappings leave no live subscribers, so the directory goes too.
        for sub in &mut stale {
            conn.del::<_, ()>(&sub.mapping_key).await?;
            sub.mapped_room = None;
            if sub.connection_id == "repaired-actor" {
                sub.actor = b"new".to_vec();
            }
        }
        prune_subscribers(&mut conn, &room, &directory, &stale).await?;
        assert!(!conn.exists::<_, bool>(&room).await?);
        assert!(!conn.sismember::<_, _, bool>(&directory, &room).await?);
        Ok(())
    }
}
