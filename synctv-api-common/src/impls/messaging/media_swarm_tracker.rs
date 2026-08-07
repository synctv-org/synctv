use std::{collections::BTreeMap, future::Future, sync::Arc, time::Duration};

use dashmap::DashMap;
use rand::seq::IteratorRandom;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use synctv_core::{models::RoomId, RedisConnectionRuntime};

const MEMBERSHIP_TTL: Duration = Duration::from_secs(90);
const LOCAL_PRUNE_INTERVAL: Duration = Duration::from_secs(30);
const REDIS_KEY_TTL_SECONDS: i64 = 180;
pub const MAX_MEDIA_SWARM_PEERS: usize = 16;

async fn redis_operation<T>(
    timeout: Duration,
    operation: &str,
    future: impl Future<Output = redis::RedisResult<T>>,
) -> Result<T, String> {
    tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| format!("Media swarm tracker Redis timeout during {operation}"))?
        .map_err(|error| format!("Media swarm tracker Redis error during {operation}: {error}"))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaSwarmPeer {
    pub actor_id: String,
    pub connection_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    struct HangingRedisRuntime;

    #[async_trait::async_trait]
    impl RedisConnectionRuntime for HangingRedisRuntime {
        async fn snapshot(&self) -> redis::RedisResult<redis::aio::ConnectionManager> {
            std::future::pending().await
        }

        fn operation_timeout(&self) -> Duration {
            Duration::from_millis(10)
        }
    }

    #[tokio::test]
    async fn local_tracker_bounds_candidates_and_excludes_announcing_connection() {
        let tracker = MediaSwarmTracker::new(None, "test:");
        let room_id = RoomId::expect_positive(1);
        for index in 0..(MAX_MEDIA_SWARM_PEERS + 5) {
            tracker
                .announce(
                    room_id,
                    format!("usr_{index}"),
                    format!("conn_{index}"),
                    "swarm-a",
                )
                .await
                .expect("announce should succeed");
        }
        let peers = tracker
            .announce(
                room_id,
                "usr_current".to_string(),
                "conn_current".to_string(),
                "swarm-a",
            )
            .await
            .expect("discovery should succeed");

        assert_eq!(peers.len(), MAX_MEDIA_SWARM_PEERS);
        assert!(peers
            .iter()
            .all(|peer| peer.connection_id != "conn_current"));
    }

    #[tokio::test]
    async fn local_tracker_isolates_rooms_and_swarms_and_removes_leaves() {
        let tracker = MediaSwarmTracker::new(None, "test:");
        let room_a = RoomId::expect_positive(1);
        let room_b = RoomId::expect_positive(2);
        tracker
            .announce(room_a, "usr_a".into(), "conn_a".into(), "swarm-a")
            .await
            .expect("first announce should succeed");

        assert!(tracker
            .announce(room_a, "usr_b".into(), "conn_b".into(), "swarm-b")
            .await
            .expect("other swarm announce should succeed")
            .is_empty());
        assert!(tracker
            .announce(room_b, "usr_c".into(), "conn_c".into(), "swarm-a")
            .await
            .expect("other room announce should succeed")
            .is_empty());
        tracker
            .leave(room_a, "usr_a", "conn_a", "swarm-a")
            .await
            .expect("leave should succeed");
        assert!(tracker
            .announce(room_a, "usr_d".into(), "conn_d".into(), "swarm-a")
            .await
            .expect("post-leave discovery should succeed")
            .is_empty());
    }

    #[tokio::test]
    async fn local_membership_lookup_requires_exact_actor_connection_and_swarm() {
        let tracker = MediaSwarmTracker::new(None, "test:");
        let room_id = RoomId::expect_positive(1);
        tracker
            .announce(
                room_id,
                "usr_a".to_string(),
                "conn_a".to_string(),
                "swarm-a",
            )
            .await
            .expect("membership announcement should succeed");

        assert!(tracker
            .contains(room_id, "usr_a", "conn_a", "swarm-a")
            .await
            .expect("membership lookup should succeed"));
        assert!(!tracker
            .contains(room_id, "usr_b", "conn_a", "swarm-a")
            .await
            .expect("actor mismatch lookup should succeed"));
        assert!(!tracker
            .contains(room_id, "usr_a", "conn_a", "swarm-b")
            .await
            .expect("swarm mismatch lookup should succeed"));
    }

    #[tokio::test]
    async fn redis_snapshot_is_bounded_by_runtime_operation_timeout() {
        let tracker = MediaSwarmTracker::new(Some(Arc::new(HangingRedisRuntime)), "test:");
        let error = tracker
            .announce(
                RoomId::expect_positive(1),
                "usr_a".to_string(),
                "conn_a".to_string(),
                "swarm-a",
            )
            .await
            .expect_err("hanging Redis snapshot should time out");

        assert!(error.contains("Redis timeout during connection snapshot"));
    }
}

#[derive(Clone)]
struct LocalMembership {
    peer: MediaSwarmPeer,
    last_seen: std::time::Instant,
}

pub struct MediaSwarmTracker {
    redis_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
    redis_key_prefix: String,
    local: DashMap<(RoomId, String), BTreeMap<String, LocalMembership>>,
    last_local_prune: parking_lot::Mutex<std::time::Instant>,
}

impl MediaSwarmTracker {
    #[must_use]
    pub fn new(
        redis_runtime: Option<Arc<dyn RedisConnectionRuntime>>,
        redis_key_prefix: impl Into<String>,
    ) -> Self {
        Self {
            redis_runtime,
            redis_key_prefix: redis_key_prefix.into(),
            local: DashMap::new(),
            last_local_prune: parking_lot::Mutex::new(std::time::Instant::now()),
        }
    }

    pub async fn announce(
        &self,
        room_id: RoomId,
        actor_id: String,
        connection_id: String,
        swarm_id: &str,
    ) -> Result<Vec<MediaSwarmPeer>, String> {
        let peer = MediaSwarmPeer {
            actor_id,
            connection_id: connection_id.clone(),
        };
        let Some(runtime) = &self.redis_runtime else {
            self.prune_local();
            self.announce_local(room_id, swarm_id, peer);
            return Ok(self.local_peers(room_id, swarm_id, &connection_id));
        };

        let timeout = runtime.operation_timeout();
        let mut redis = redis_operation(timeout, "connection snapshot", runtime.snapshot()).await?;
        let key = self.redis_key(room_id, swarm_id);
        let now = chrono::Utc::now().timestamp();
        let cutoff =
            now.saturating_sub(i64::try_from(MEMBERSHIP_TTL.as_secs()).unwrap_or(i64::MAX));
        let encoded = serde_json::to_string(&peer)
            .map_err(|error| format!("Failed to encode media swarm peer: {error}"))?;
        let mut pipeline = redis::pipe();
        pipeline
            .atomic()
            .cmd("ZREMRANGEBYSCORE")
            .arg(&key)
            .arg("-inf")
            .arg(cutoff)
            .ignore()
            .cmd("ZADD")
            .arg(&key)
            .arg(now)
            .arg(&encoded)
            .ignore()
            .cmd("EXPIRE")
            .arg(&key)
            .arg(REDIS_KEY_TTL_SECONDS)
            .ignore();
        redis_operation(
            timeout,
            "membership announcement",
            pipeline.query_async::<()>(&mut redis),
        )
        .await?;

        let mut command = redis::cmd("ZRANDMEMBER");
        command.arg(&key).arg(MAX_MEDIA_SWARM_PEERS + 1);
        let encoded_peers: Vec<String> =
            redis_operation(timeout, "peer discovery", command.query_async(&mut redis)).await?;
        Ok(encoded_peers
            .into_iter()
            .filter_map(|value| serde_json::from_str::<MediaSwarmPeer>(&value).ok())
            .filter(|candidate| candidate.connection_id != connection_id)
            .take(MAX_MEDIA_SWARM_PEERS)
            .collect())
    }

    pub async fn leave(
        &self,
        room_id: RoomId,
        actor_id: &str,
        connection_id: &str,
        swarm_id: &str,
    ) -> Result<(), String> {
        let key = (room_id, swarm_id.to_string());
        if let Some(mut memberships) = self.local.get_mut(&key) {
            memberships.remove(connection_id);
            if memberships.is_empty() {
                drop(memberships);
                self.local.remove(&key);
            }
        }
        let Some(runtime) = &self.redis_runtime else {
            return Ok(());
        };
        let timeout = runtime.operation_timeout();
        let mut redis = redis_operation(timeout, "connection snapshot", runtime.snapshot()).await?;
        let encoded = serde_json::to_string(&MediaSwarmPeer {
            actor_id: actor_id.to_string(),
            connection_id: connection_id.to_string(),
        })
        .map_err(|error| format!("Failed to encode media swarm peer: {error}"))?;
        redis_operation(
            timeout,
            "membership removal",
            redis.zrem::<_, _, ()>(self.redis_key(room_id, swarm_id), encoded),
        )
        .await
    }

    pub async fn contains(
        &self,
        room_id: RoomId,
        actor_id: &str,
        connection_id: &str,
        swarm_id: &str,
    ) -> Result<bool, String> {
        let Some(runtime) = &self.redis_runtime else {
            let now = std::time::Instant::now();
            return Ok(self
                .local
                .get(&(room_id, swarm_id.to_string()))
                .and_then(|memberships| memberships.get(connection_id).cloned())
                .is_some_and(|membership| {
                    membership.peer.actor_id == actor_id
                        && now.duration_since(membership.last_seen) < MEMBERSHIP_TTL
                }));
        };

        let timeout = runtime.operation_timeout();
        let mut redis = redis_operation(timeout, "connection snapshot", runtime.snapshot()).await?;
        let encoded = serde_json::to_string(&MediaSwarmPeer {
            actor_id: actor_id.to_string(),
            connection_id: connection_id.to_string(),
        })
        .map_err(|error| format!("Failed to encode media swarm peer: {error}"))?;
        let score: Option<i64> = redis_operation(
            timeout,
            "membership lookup",
            redis.zscore(self.redis_key(room_id, swarm_id), encoded),
        )
        .await?;
        let cutoff = chrono::Utc::now()
            .timestamp()
            .saturating_sub(i64::try_from(MEMBERSHIP_TTL.as_secs()).unwrap_or(i64::MAX));
        Ok(score.is_some_and(|last_seen| last_seen > cutoff))
    }

    fn announce_local(&self, room_id: RoomId, swarm_id: &str, peer: MediaSwarmPeer) {
        let now = std::time::Instant::now();
        let mut memberships = self
            .local
            .entry((room_id, swarm_id.to_string()))
            .or_default();
        memberships
            .retain(|_, membership| now.duration_since(membership.last_seen) < MEMBERSHIP_TTL);
        memberships.insert(
            peer.connection_id.clone(),
            LocalMembership {
                peer,
                last_seen: now,
            },
        );
    }

    fn prune_local(&self) {
        let now = std::time::Instant::now();
        let mut last_prune = self.last_local_prune.lock();
        if now.duration_since(*last_prune) < LOCAL_PRUNE_INTERVAL {
            return;
        }
        *last_prune = now;
        drop(last_prune);
        self.local.retain(|_, memberships| {
            memberships
                .retain(|_, membership| now.duration_since(membership.last_seen) < MEMBERSHIP_TTL);
            !memberships.is_empty()
        });
    }

    fn local_peers(
        &self,
        room_id: RoomId,
        swarm_id: &str,
        connection_id: &str,
    ) -> Vec<MediaSwarmPeer> {
        let Some(memberships) = self.local.get(&(room_id, swarm_id.to_string())) else {
            return Vec::new();
        };
        memberships
            .values()
            .filter(|membership| membership.peer.connection_id != connection_id)
            .sample(&mut rand::rng(), MAX_MEDIA_SWARM_PEERS)
            .into_iter()
            .map(|membership| membership.peer.clone())
            .collect()
    }

    fn redis_key(&self, room_id: RoomId, swarm_id: &str) -> String {
        format!(
            "{}media_swarm:{}:{}",
            self.redis_key_prefix,
            room_id,
            hex::encode(swarm_id)
        )
    }
}
