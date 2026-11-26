//! Replica-wide actor and room connection indexes.

use redis::{AsyncCommands, aio::ConnectionManager as RedisConnection};
use synctv_core::{
    models::id::{RoomId, UserId},
    redis_runtime::index::{IndexKind, StaleMember, prune_stale_members},
};
use tracing::{debug, info, warn};

use super::{ConnectionManager, model::ConnectionInfoPersistent};

impl ConnectionManager {
    pub(super) async fn load_valid_connection_ids_from_index(
        &self,
        conn: &mut RedisConnection,
        index_key: &str,
        expected_user_id: Option<&UserId>,
        expected_room_id: Option<&RoomId>,
    ) -> Result<Vec<String>, String> {
        let ids: Vec<String> = self
            .redis_op(
                "fetch distributed connection index",
                conn.smembers(index_key),
            )
            .await?;
        if ids.is_empty() {
            return Ok(ids);
        }

        let keys: Vec<_> = ids.iter().map(|id| self.conn_metadata_key(id)).collect();
        let metadata: Vec<Option<Vec<u8>>> = self
            .redis_op("fetch distributed connection metadata", conn.mget(&keys))
            .await?;
        let mut valid = Vec::with_capacity(ids.len());
        let mut stale = Vec::new();
        for ((id, key), value) in ids.into_iter().zip(keys).zip(metadata) {
            if let Some(json) = &value {
                match serde_json::from_slice::<ConnectionInfoPersistent>(json) {
                    Ok(info)
                        if info.connection_id == id
                            && expected_user_id
                                .is_none_or(|user| info.actor.user_id() == Some(*user))
                            && expected_room_id
                                .is_none_or(|room| info.room_id.as_ref() == Some(room)) =>
                    {
                        valid.push(id);
                        continue;
                    }
                    Ok(_) => {}
                    Err(error) => {
                        warn!(index_key, connection_id = %id, %error, "Invalid distributed connection metadata");
                    }
                }
            }
            stale.push(StaleMember {
                member: id,
                metadata_key: key,
                observed_value: value,
            });
        }
        if !stale.is_empty() {
            match self
                .redis_op(
                    "prune stale distributed connection index members",
                    prune_stale_members(conn, IndexKind::Set, index_key, &stale),
                )
                .await
            {
                Ok(removed) => debug!(index_key, removed, "Pruned distributed connection index"),
                Err(error) => {
                    warn!(index_key, %error, "Failed to prune distributed connection index");
                }
            }
        }
        Ok(valid)
    }

    async fn reconcile_connection_index(
        &self,
        conn: &mut RedisConnection,
        key: &str,
    ) -> Result<usize, String> {
        let members: Vec<String> = self
            .redis_op("fetch Redis index members", conn.smembers(key))
            .await?;
        let candidates: Vec<_> = members
            .into_iter()
            .map(|id| StaleMember {
                metadata_key: self.conn_metadata_key(&id),
                member: id,
                observed_value: None,
            })
            .collect();
        self.redis_op(
            "remove connections with missing metadata",
            prune_stale_members(conn, IndexKind::Set, key, &candidates),
        )
        .await
    }

    /// Retain connections owned by any replica as long as their metadata exists.
    pub(super) async fn cleanup_stale_redis_indexes(&self, conn: &mut RedisConnection) {
        let mut cleaned = 0;
        for directory in [
            self.actor_index_directory_key(),
            self.room_index_directory_key(),
        ] {
            let indexes: Vec<String> = match self
                .redis_op(
                    "fetch distributed index directory",
                    conn.smembers(&directory),
                )
                .await
            {
                Ok(keys) => keys,
                Err(error) => {
                    warn!(directory, %error, "Failed to load distributed connection index directory");
                    continue;
                }
            };
            let mut directories_to_check = Vec::new();
            for key in indexes {
                match self.reconcile_connection_index(conn, &key).await {
                    Ok(removed) => {
                        cleaned += removed;
                        directories_to_check.push(StaleMember::missing_indexed_key(key));
                    }
                    Err(error) => {
                        warn!(key, %error, "Failed to reconcile distributed connection index");
                    }
                }
            }
            if let Err(error) = self
                .redis_op(
                    "prune distributed index directory members",
                    prune_stale_members(conn, IndexKind::Set, &directory, &directories_to_check),
                )
                .await
            {
                warn!(directory, %error, "Failed to prune distributed connection index directory");
            }
        }
        if cleaned > 0 {
            info!(
                stale_index_members_cleaned = cleaned,
                "Cleaned up stale distributed connection indexes from Redis"
            );
        }
    }
}
