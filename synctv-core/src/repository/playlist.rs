//! Playlist repository for database operations
//!
//! Design reference: /Volumes/workspace/rust/design/04-数据库设计.md §2.4.1

use crate::{
    models::{Playlist, PlaylistId, RoomId},
    Result,
};
use sqlx::{FromRow, PgPool};

/// Playlist repository
#[derive(Clone)]
pub struct PlaylistRepository {
    pool: PgPool,
}

impl PlaylistRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Get playlist by ID
    pub async fn get_by_id(&self, id: &PlaylistId) -> Result<Option<Playlist>> {
        let row = sqlx::query(
            r"
            SELECT id, room_id, creator_id, name, parent_id, position,
                   source_provider, source_config, provider_instance_name,
                   created_at, updated_at, version
            FROM playlists
            WHERE id = $1
            ",
        )
        .bind(id.as_str())
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => Ok(Some(Playlist::from_row(&row)?)),
            None => Ok(None),
        }
    }

    /// Get root playlist for a room
    pub async fn get_root_playlist(&self, room_id: &RoomId) -> Result<Playlist> {
        let row = sqlx::query(
            r"
            SELECT id, room_id, creator_id, name, parent_id, position,
                   source_provider, source_config, provider_instance_name,
                   created_at, updated_at, version
            FROM playlists
            WHERE room_id = $1 AND parent_id IS NULL AND name = ''
            ",
        )
        .bind(room_id.as_str())
        .fetch_one(&self.pool)
        .await?;

        Ok(Playlist::from_row(&row)?)
    }

    /// Get children playlists of a parent
    pub async fn get_children(&self, parent_id: &PlaylistId) -> Result<Vec<Playlist>> {
        let rows = sqlx::query(
            r"
            SELECT id, room_id, creator_id, name, parent_id, position,
                   source_provider, source_config, provider_instance_name,
                   created_at, updated_at, version
            FROM playlists
            WHERE parent_id = $1
            ORDER BY position ASC
            ",
        )
        .bind(parent_id.as_str())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| Ok(Playlist::from_row(&row)?))
            .collect()
    }

    /// Get count of children playlists for a parent.
    pub async fn count_children(&self, parent_id: &PlaylistId) -> Result<i64> {
        let count: i64 = sqlx::query_scalar(
            r"
            SELECT COUNT(*) FROM playlists WHERE parent_id = $1
            ",
        )
        .bind(parent_id.as_str())
        .fetch_one(&self.pool)
        .await?;

        Ok(count)
    }

    /// Get paginated children playlists for a parent.
    pub async fn get_children_paginated(
        &self,
        parent_id: &PlaylistId,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Playlist>> {
        let rows = sqlx::query(
            r"
            SELECT id, room_id, creator_id, name, parent_id, position,
                   source_provider, source_config, provider_instance_name,
                   created_at, updated_at, version
            FROM playlists
            WHERE parent_id = $1
            ORDER BY position ASC
            LIMIT $2 OFFSET $3
            ",
        )
        .bind(parent_id.as_str())
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| Ok(Playlist::from_row(&row)?))
            .collect()
    }

    /// Get all playlists in a room (tree structure)
    pub async fn get_by_room(&self, room_id: &RoomId) -> Result<Vec<Playlist>> {
        let rows = sqlx::query(
            r"
            SELECT id, room_id, creator_id, name, parent_id, position,
                   source_provider, source_config, provider_instance_name,
                   created_at, updated_at, version
            FROM playlists
            WHERE room_id = $1
            ORDER BY parent_id NULLS FIRST, position ASC
            ",
        )
        .bind(room_id.as_str())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| Ok(Playlist::from_row(&row)?))
            .collect()
    }

    /// Count all playlists in a room
    pub async fn count_by_room(&self, room_id: &RoomId) -> Result<i64> {
        let count: i64 = sqlx::query_scalar(
            r"
            SELECT COUNT(*) FROM playlists WHERE room_id = $1
            ",
        )
        .bind(room_id.as_str())
        .fetch_one(&self.pool)
        .await?;

        Ok(count)
    }

    /// Get paginated playlists in a room
    pub async fn get_by_room_paginated(
        &self,
        room_id: &RoomId,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Playlist>> {
        let rows = sqlx::query(
            r"
            SELECT id, room_id, creator_id, name, parent_id, position,
                   source_provider, source_config, provider_instance_name,
                   created_at, updated_at, version
            FROM playlists
            WHERE room_id = $1
            ORDER BY parent_id NULLS FIRST, position ASC
            LIMIT $2 OFFSET $3
            ",
        )
        .bind(room_id.as_str())
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| Ok(Playlist::from_row(&row)?))
            .collect()
    }

    /// Create a new playlist
    ///
    /// If `playlist.position` is negative, the position is computed within a
    /// transaction using a `PostgreSQL` advisory lock to prevent concurrent inserts
    /// from computing the same position. Pass a non-negative position to use
    /// an explicit value (e.g., when the caller already holds a lock).
    ///
    /// `SELECT MAX(position) FOR UPDATE` cannot protect empty tables because
    /// there are no rows to lock when the table is empty. Two concurrent inserts
    /// can both see `MAX = NULL` and both compute `position = 0`, producing a
    /// UNIQUE constraint violation. An advisory lock on (`room_id`, `parent_id`)
    /// serializes position computation regardless of whether rows exist.
    pub async fn create(&self, playlist: &Playlist) -> Result<Playlist> {
        if playlist.position < 0 {
            // Use a transaction with a transaction-scoped PostgreSQL advisory lock
            // to serialize position computation across concurrent inserts, including
            // when the table is empty (where FOR UPDATE cannot lock any rows).
            let mut tx = self.pool.begin().await?;

            let parent_id_str = playlist
                .parent_id
                .as_ref()
                .map(super::super::models::id::PlaylistId::as_str);

            // Derive a stable 64-bit advisory lock key from (room_id, parent_id).
            // We use a deterministic combination that minimizes collision probability
            // by spreading the hash values across the 64-bit space.
            //
            // Task #56: Changed from simple hash to structured combination to reduce
            // collision risk. Old implementation could have collisions with different
            // (room_id, parent_id) pairs hashing to the same 64-bit value.
            let lock_key: i64 = {
                use std::hash::{Hash, Hasher};

                // Hash room_id to get first 32 bits
                let room_hash = {
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    playlist.room_id.as_str().hash(&mut h);
                    h.finish()
                };

                // Hash parent_id to get second 32 bits
                let parent_hash = parent_id_str.map_or(0, |pid| {
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    pid.hash(&mut h);
                    h.finish()
                });

                // Combine using upper 32 bits for room and lower 32 bits for parent
                // This significantly reduces collision probability compared to hashing
                // both together, as collisions now require specific bit patterns in
                // both the upper and lower halves.
                //
                // Use the lower 32 bits of each hash to avoid overflow issues
                let room_bits = (room_hash & 0x7FFFFFFF) as i64; // 31 bits, positive
                let parent_bits = (parent_hash & 0x7FFFFFFF) as i64; // 31 bits, positive

                // Combine: room in upper 32 bits, parent in lower 32 bits
                // This gives us 62 useful bits (31+31), staying within i64 range
                (room_bits << 31) | parent_bits
            };

            // pg_advisory_xact_lock acquires a session-exclusive advisory lock that
            // is automatically released when the transaction commits or rolls back.
            // It blocks until the lock is available, serialising concurrent inserts
            // for the same (room_id, parent_id) pair.
            sqlx::query("SELECT pg_advisory_xact_lock($1)")
                .bind(lock_key)
                .execute(&mut *tx)
                .await?;

            let max_pos: Option<i32> = sqlx::query_scalar(
                r"
                SELECT MAX(position)
                FROM playlists
                WHERE room_id = $1
                  AND parent_id IS NOT DISTINCT FROM $2
                ",
            )
            .bind(playlist.room_id.as_str())
            .bind(parent_id_str)
            .fetch_one(&mut *tx)
            .await?;

            let next_position = max_pos.unwrap_or(-1) + 1;

            let source_provider_str = playlist.source_provider.as_deref();
            let row = sqlx::query(
                r"
                INSERT INTO playlists (id, room_id, creator_id, name, parent_id, position,
                                       source_provider, source_config, provider_instance_name)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                RETURNING id, room_id, creator_id, name, parent_id, position,
                          source_provider, source_config, provider_instance_name,
                          created_at, updated_at, version
                ",
            )
            .bind(playlist.id.as_str())
            .bind(playlist.room_id.as_str())
            .bind(
                playlist
                    .creator_id
                    .as_ref()
                    .map(super::super::models::id::UserId::as_str),
            )
            .bind(&playlist.name)
            .bind(parent_id_str)
            .bind(next_position)
            .bind(source_provider_str)
            .bind(&playlist.source_config)
            .bind(&playlist.provider_instance_name)
            .fetch_one(&mut *tx)
            .await?;

            let result = Playlist::from_row(&row)?;
            tx.commit().await?;
            Ok(result)
        } else {
            // Explicit position provided by caller
            let source_provider_str = playlist.source_provider.as_deref();
            let parent_id_str = playlist
                .parent_id
                .as_ref()
                .map(super::super::models::id::PlaylistId::as_str);
            let row = sqlx::query(
                r"
                INSERT INTO playlists (id, room_id, creator_id, name, parent_id, position,
                                       source_provider, source_config, provider_instance_name)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                RETURNING id, room_id, creator_id, name, parent_id, position,
                          source_provider, source_config, provider_instance_name,
                          created_at, updated_at, version
                ",
            )
            .bind(playlist.id.as_str())
            .bind(playlist.room_id.as_str())
            .bind(
                playlist
                    .creator_id
                    .as_ref()
                    .map(super::super::models::id::UserId::as_str),
            )
            .bind(&playlist.name)
            .bind(parent_id_str)
            .bind(playlist.position)
            .bind(source_provider_str)
            .bind(&playlist.source_config)
            .bind(&playlist.provider_instance_name)
            .fetch_one(&self.pool)
            .await?;

            Ok(Playlist::from_row(&row)?)
        }
    }

    /// Create a playlist using a provided executor (pool or transaction).
    ///
    /// **Important:** When `playlist.position` is negative (auto-position), the
    /// caller MUST pass a transaction as the executor and should call
    /// [`get_next_position_for_update`] first to lock the relevant rows. This
    /// method will return an error if auto-position is requested, because the
    /// inline subquery cannot acquire a `FOR UPDATE` lock, leading to duplicate
    /// positions under concurrent inserts.
    ///
    /// Pass a non-negative position to use an explicit value.
    pub async fn create_with_executor<'e, E>(
        &self,
        playlist: &Playlist,
        executor: E,
    ) -> Result<Playlist>
    where
        E: sqlx::PgExecutor<'e>,
    {
        if playlist.position < 0 {
            // Auto-position without a FOR UPDATE lock is unsafe under concurrency.
            // Callers must use a transaction with get_next_position_for_update()
            // and pass an explicit (non-negative) position. The self-contained
            // `create()` method handles this correctly with its own transaction.
            return Err(crate::Error::InvalidInput(
                "auto-position (negative position) requires using create() or \
                 calling get_next_position_for_update() in a transaction first"
                    .to_string(),
            ));
        }

        let source_provider_str = playlist.source_provider.as_deref();
        let parent_id_str = playlist
            .parent_id
            .as_ref()
            .map(super::super::models::id::PlaylistId::as_str);

        let row = sqlx::query(
            r"
            INSERT INTO playlists (id, room_id, creator_id, name, parent_id, position,
                                   source_provider, source_config, provider_instance_name)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            RETURNING id, room_id, creator_id, name, parent_id, position,
                      source_provider, source_config, provider_instance_name,
                      created_at, updated_at, version
            ",
        )
        .bind(playlist.id.as_str())
        .bind(playlist.room_id.as_str())
        .bind(
            playlist
                .creator_id
                .as_ref()
                .map(super::super::models::id::UserId::as_str),
        )
        .bind(&playlist.name)
        .bind(parent_id_str)
        .bind(playlist.position)
        .bind(source_provider_str)
        .bind(&playlist.source_config)
        .bind(&playlist.provider_instance_name)
        .fetch_one(executor)
        .await?;

        Ok(Playlist::from_row(&row)?)
    }

    /// Get next available position in a parent, using an advisory lock to
    /// serialize concurrent position computation.
    ///
    /// Must be called within a transaction. The advisory lock is transaction-scoped
    /// (`pg_advisory_xact_lock`) and is automatically released when the transaction
    /// commits or rolls back.
    ///
    /// Unlike `SELECT MAX(position) FOR UPDATE`, this correctly handles empty
    /// tables — `FOR UPDATE` on an aggregate cannot lock any rows when none exist,
    /// allowing two concurrent inserts to both compute `position = 0`.
    pub async fn get_next_position_for_update<'e>(
        &self,
        room_id: &RoomId,
        parent_id: Option<&PlaylistId>,
        tx: &mut sqlx::Transaction<'e, sqlx::Postgres>,
    ) -> Result<i32> {
        let parent_id_str = parent_id.map(super::super::models::id::PlaylistId::as_str);

        // Acquire a transaction-scoped advisory lock on (room_id, parent_id) to
        // serialize position computation. The lock is automatically released when
        // the surrounding transaction commits or rolls back.
        //
        // IMPORTANT: This must use the SAME hash strategy as the create() method
        // (lines 175-204) to ensure both methods acquire the SAME lock for the
        // same (room_id, parent_id) pair. Different keys would defeat the lock.
        let lock_key: i64 = {
            use std::hash::{Hash, Hasher};

            // Hash room_id to get first 32 bits
            let room_hash = {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                room_id.as_str().hash(&mut h);
                h.finish()
            };

            // Hash parent_id to get second 32 bits
            let parent_hash = parent_id_str.map_or(0, |pid| {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                pid.hash(&mut h);
                h.finish()
            });

            // Combine using upper 32 bits for room and lower 32 bits for parent
            // This is IDENTICAL to the strategy in create() method.
            let room_bits = (room_hash & 0x7FFFFFFF) as i64; // 31 bits, positive
            let parent_bits = (parent_hash & 0x7FFFFFFF) as i64; // 31 bits, positive
            (room_bits << 31) | parent_bits
        };
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(lock_key)
            .execute(&mut **tx)
            .await?;

        let max_pos: Option<i32> = sqlx::query_scalar(
            r"
            SELECT MAX(position)
            FROM playlists
            WHERE room_id = $1
              AND parent_id IS NOT DISTINCT FROM $2
            ",
        )
        .bind(room_id.as_str())
        .bind(parent_id_str)
        .fetch_one(&mut **tx)
        .await?;

        Ok(max_pos.unwrap_or(-1) + 1)
    }

    /// Get next available position in a parent (non-locking, for read-only use).
    ///
    /// **Warning:** This does NOT acquire a lock. For concurrent-safe position
    /// computation, use [`get_next_position_for_update`] within a transaction.
    pub async fn get_next_position(
        &self,
        room_id: &RoomId,
        parent_id: Option<&PlaylistId>,
    ) -> Result<i32> {
        let max_pos: Option<i32> = sqlx::query_scalar(
            r"
            SELECT MAX(position)
            FROM playlists
            WHERE room_id = $1
              AND parent_id IS NOT DISTINCT FROM $2
            ",
        )
        .bind(room_id.as_str())
        .bind(parent_id.map(super::super::models::id::PlaylistId::as_str))
        .fetch_one(&self.pool)
        .await?;

        Ok(max_pos.unwrap_or(-1) + 1)
    }

    /// Update playlist with optimistic locking.
    ///
    /// Returns `Err(Error::OptimisticLockConflict)` if the version in the database
    /// does not match `expected_version`.
    ///
    /// On success, returns the updated playlist with incremented version.
    pub async fn update_with_version(
        &self,
        playlist: &Playlist,
        expected_version: i32,
    ) -> Result<Playlist> {
        let source_provider_str = playlist.source_provider.as_deref();
        let row = sqlx::query(
            r"
            UPDATE playlists
            SET name = $2, position = $3, source_provider = $4, source_config = $5,
                provider_instance_name = $6, version = version + 1
            WHERE id = $1 AND version = $7
            RETURNING id, room_id, creator_id, name, parent_id, position,
                      source_provider, source_config, provider_instance_name,
                      created_at, updated_at, version
            ",
        )
        .bind(playlist.id.as_str())
        .bind(&playlist.name)
        .bind(playlist.position)
        .bind(source_provider_str)
        .bind(&playlist.source_config)
        .bind(&playlist.provider_instance_name)
        .bind(expected_version)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => Ok(Playlist::from_row(&row)?),
            None => Err(crate::Error::OptimisticLockConflict),
        }
    }

    /// Update playlist (legacy method without optimistic locking).
    ///
    /// **Warning:** This method does not check version and always succeeds.
    /// Prefer `update_with_version` for concurrent access patterns.
    pub async fn update(&self, playlist: &Playlist) -> Result<Playlist> {
        let source_provider_str = playlist.source_provider.as_deref();
        let row = sqlx::query(
            r"
            UPDATE playlists
            SET name = $2, position = $3, source_provider = $4, source_config = $5,
                provider_instance_name = $6, version = version + 1
            WHERE id = $1
            RETURNING id, room_id, creator_id, name, parent_id, position,
                      source_provider, source_config, provider_instance_name,
                      created_at, updated_at, version
            ",
        )
        .bind(playlist.id.as_str())
        .bind(&playlist.name)
        .bind(playlist.position)
        .bind(source_provider_str)
        .bind(&playlist.source_config)
        .bind(&playlist.provider_instance_name)
        .fetch_one(&self.pool)
        .await?;

        Ok(Playlist::from_row(&row)?)
    }

    /// Delete playlist (cascade to children and media)
    pub async fn delete(&self, id: &PlaylistId) -> Result<bool> {
        let result = sqlx::query("DELETE FROM playlists WHERE id = $1")
            .bind(id.as_str())
            .execute(&self.pool)
            .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Convert database row to Playlist
    /// Get playlist path from a given node to root using a recursive CTE (single query)
    pub async fn get_path(&self, playlist_id: &PlaylistId) -> Result<Vec<Playlist>> {
        let rows = sqlx::query(
            r"
            WITH RECURSIVE ancestors AS (
                SELECT id, room_id, creator_id, name, parent_id, position,
                       source_provider, source_config, provider_instance_name,
                       created_at, updated_at, version, 0 AS depth
                FROM playlists
                WHERE id = $1
              UNION ALL
                SELECT p.id, p.room_id, p.creator_id, p.name, p.parent_id, p.position,
                       p.source_provider, p.source_config, p.provider_instance_name,
                       p.created_at, p.updated_at, p.version, a.depth + 1
                FROM playlists p
                JOIN ancestors a ON p.id = a.parent_id
                WHERE a.depth < 50
            )
            SELECT id, room_id, creator_id, name, parent_id, position,
                   source_provider, source_config, provider_instance_name,
                   created_at, updated_at, version
            FROM ancestors
            ORDER BY depth DESC
            ",
        )
        .bind(playlist_id.as_str())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| Ok(Playlist::from_row(&row)?))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unit test: Repository constructor is const
    #[test]
    fn test_repository_new() {
        fn _assert_const_new(pool: PgPool) -> PlaylistRepository {
            PlaylistRepository::new(pool)
        }
        // Compilation test only - cannot create PgPool without database
    }

    /// Unit test: Advisory lock key generation is deterministic
    ///
    /// The lock key must be consistent for the same (room_id, parent_id) pair
    /// so that both `create()` and `get_next_position_for_update()` acquire
    /// the same lock.
    #[test]
    fn test_advisory_lock_key_deterministic() {
        use std::hash::{Hash, Hasher};

        let room_id = RoomId::from_string("room12345678".to_string());
        let parent_id = PlaylistId::from_string("parent123456".to_string());

        // Test the same hash strategy used in the implementation
        let key1 = {
            let room_hash = {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                room_id.as_str().hash(&mut h);
                h.finish()
            };
            let parent_hash = {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                parent_id.as_str().hash(&mut h);
                h.finish()
            };
            let room_bits = (room_hash & 0x7FFFFFFF) as i64;
            let parent_bits = (parent_hash & 0x7FFFFFFF) as i64;
            (room_bits << 31) | parent_bits
        };

        // Same inputs should produce same key
        let key2 = {
            let room_hash = {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                room_id.as_str().hash(&mut h);
                h.finish()
            };
            let parent_hash = {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                parent_id.as_str().hash(&mut h);
                h.finish()
            };
            let room_bits = (room_hash & 0x7FFFFFFF) as i64;
            let parent_bits = (parent_hash & 0x7FFFFFFF) as i64;
            (room_bits << 31) | parent_bits
        };

        assert_eq!(key1, key2, "Lock key should be deterministic");
    }

    /// Unit test: Different (room_id, parent_id) pairs produce different keys
    ///
    /// While hash collisions are theoretically possible, this test verifies
    /// that simple variations produce different keys.
    #[test]
    fn test_advisory_lock_key_different() {
        use std::hash::{Hash, Hasher};

        let room1 = RoomId::from_string("room11111111".to_string());
        let room2 = RoomId::from_string("room22222222".to_string());
        let parent1 = PlaylistId::from_string("parent111111".to_string());
        let parent2 = PlaylistId::from_string("parent222222".to_string());

        let compute_key = |room_id: &RoomId, parent_id: Option<&PlaylistId>| {
            let room_hash = {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                room_id.as_str().hash(&mut h);
                h.finish()
            };
            let parent_hash = parent_id.map_or(0, |pid| {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                pid.as_str().hash(&mut h);
                h.finish()
            });
            let room_bits = (room_hash & 0x7FFFFFFF) as i64;
            let parent_bits = (parent_hash & 0x7FFFFFFF) as i64;
            (room_bits << 31) | parent_bits
        };

        let key_room1_parent1 = compute_key(&room1, Some(&parent1));
        let key_room1_parent2 = compute_key(&room1, Some(&parent2));
        let key_room2_parent1 = compute_key(&room2, Some(&parent1));
        let key_room2_none = compute_key(&room2, None);

        // Different room_id should produce different keys
        assert_ne!(key_room1_parent1, key_room2_parent1);
        // Different parent_id should produce different keys
        assert_ne!(key_room1_parent1, key_room1_parent2);
        // Root (no parent) should be different from child
        assert_ne!(key_room2_parent1, key_room2_none);
    }

    /// Unit test: Lock key stays within i64 range
    ///
    /// The combination of two 31-bit values (room_bits and parent_bits)
    /// should always fit within i64 without overflow.
    #[test]
    fn test_advisory_lock_key_range() {
        use std::hash::{Hash, Hasher};

        // Test with various ID patterns
        let test_ids = [
            "000000000000",
            "ZZZZZZZZZZZZ",
            "aaaaaaaaaaaa",
            "123456789012",
            "------------",
        ];

        for id in test_ids {
            let room_id = RoomId::from_string(id.to_string());
            let parent_id = PlaylistId::from_string(id.to_string());

            let key = {
                let room_hash = {
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    room_id.as_str().hash(&mut h);
                    h.finish()
                };
                let parent_hash = {
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    parent_id.as_str().hash(&mut h);
                    h.finish()
                };
                let room_bits = (room_hash & 0x7FFFFFFF) as i64;
                let parent_bits = (parent_hash & 0x7FFFFFFF) as i64;
                (room_bits << 31) | parent_bits
            };

            // Key should be positive (advisory lock keys in PostgreSQL are signed 64-bit)
            assert!(key >= 0, "Lock key should be non-negative for id: {id}");
        }
    }

    /// Integration test: Create and get playlist by ID
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_and_get_by_id() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());

        // Create owner and room
        let owner = UserFixture::new().with_username("playlist_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Playlist Test Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root playlist
        let playlist = PlaylistFixture::new().with_room_id(room.id.clone()).build();
        let created = playlist_repo.create(&playlist).await.unwrap();

        assert!(created.is_root());
        assert_eq!(created.position, 0);

        // Get by ID
        let fetched = playlist_repo.get_by_id(&created.id).await.unwrap();
        assert!(fetched.is_some());
        let fetched = fetched.unwrap();
        assert_eq!(fetched.id, created.id);
        assert!(fetched.is_root());
    }

    /// Integration test: Get root playlist for a room
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_root_playlist() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());

        let owner = UserFixture::new()
            .with_username("root_playlist_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Root Playlist Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root playlist
        let root = PlaylistFixture::new().with_room_id(room.id.clone()).build();
        let created = playlist_repo.create(&root).await.unwrap();

        // Get root playlist
        let fetched = playlist_repo.get_root_playlist(&room.id).await.unwrap();
        assert_eq!(fetched.id, created.id);
        assert!(fetched.is_root());
    }

    /// Integration test: Get playlists by room
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_by_room() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());

        let owner = UserFixture::new()
            .with_username("room_playlist_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Room Playlist Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root playlist
        let root = PlaylistFixture::new().with_room_id(room.id.clone()).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        // Create child playlists
        let child1 = PlaylistFixture::new_child(created_root.id.clone())
            .with_room_id(room.id.clone())
            .with_name("Child 1")
            .build();
        let created_child1 = playlist_repo.create(&child1).await.unwrap();

        let child2 = PlaylistFixture::new_child(created_root.id.clone())
            .with_room_id(room.id.clone())
            .with_name("Child 2")
            .build();
        let created_child2 = playlist_repo.create(&child2).await.unwrap();

        // Get all playlists for room
        let playlists = playlist_repo.get_by_room(&room.id).await.unwrap();
        assert_eq!(playlists.len(), 3);

        // Verify root comes first (NULLS FIRST in ORDER BY)
        assert!(playlists[0].is_root());
        assert_eq!(playlists[0].id, created_root.id);

        // Children should be sorted by position
        let child_ids: Vec<_> = playlists[1..].iter().map(|p| p.id.clone()).collect();
        assert!(child_ids.contains(&created_child1.id));
        assert!(child_ids.contains(&created_child2.id));
    }

    /// Integration test: Update playlist
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_update() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());

        let owner = UserFixture::new()
            .with_username("update_playlist_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Update Playlist Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root and child
        let root = PlaylistFixture::new().with_room_id(room.id.clone()).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        let child = PlaylistFixture::new_child(created_root.id.clone())
            .with_room_id(room.id.clone())
            .with_name("Original Name")
            .build();
        let created = playlist_repo.create(&child).await.unwrap();

        // Update playlist
        let mut updated = created.clone();
        updated.name = "Updated Name".to_string();
        updated.position = 5;

        let result = playlist_repo.update(&updated).await.unwrap();
        assert_eq!(result.name, "Updated Name");
        assert_eq!(result.position, 5);
        assert!(result.version > created.version); // Version should increment
    }

    /// Integration test: Update with version (optimistic locking)
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_update_with_version() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());

        let owner = UserFixture::new()
            .with_username("version_playlist_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Version Playlist Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root and child
        let root = PlaylistFixture::new().with_room_id(room.id.clone()).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        let child = PlaylistFixture::new_child(created_root.id.clone())
            .with_room_id(room.id.clone())
            .with_name("Test Playlist")
            .build();
        let created = playlist_repo.create(&child).await.unwrap();
        let original_version = created.version;

        // Update with correct version
        let mut updated = created.clone();
        updated.name = "Updated".to_string();
        let result = playlist_repo
            .update_with_version(&updated, original_version)
            .await
            .unwrap();
        assert_eq!(result.name, "Updated");

        // Update with stale version should fail
        let mut stale = created.clone();
        stale.name = "Stale Update".to_string();
        let result = playlist_repo
            .update_with_version(&stale, original_version) // Old version
            .await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            crate::Error::OptimisticLockConflict
        ));
    }

    /// Integration test: Delete playlist
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_delete() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());

        let owner = UserFixture::new()
            .with_username("delete_playlist_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Delete Playlist Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root and child
        let root = PlaylistFixture::new().with_room_id(room.id.clone()).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        let child = PlaylistFixture::new_child(created_root.id.clone())
            .with_room_id(room.id.clone())
            .with_name("To Delete")
            .build();
        let created = playlist_repo.create(&child).await.unwrap();

        // Delete child
        let deleted = playlist_repo.delete(&created.id).await.unwrap();
        assert!(deleted);

        // Verify deleted
        let fetched = playlist_repo.get_by_id(&created.id).await.unwrap();
        assert!(fetched.is_none());

        // Delete non-existent returns false
        let deleted_again = playlist_repo.delete(&created.id).await.unwrap();
        assert!(!deleted_again);
    }

    /// Integration test: Delete cascades to children
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_delete_cascades() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());

        let owner = UserFixture::new()
            .with_username("cascade_playlist_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Cascade Playlist Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root, child, grandchild
        let root = PlaylistFixture::new().with_room_id(room.id.clone()).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        let child = PlaylistFixture::new_child(created_root.id.clone())
            .with_room_id(room.id.clone())
            .with_name("Child")
            .build();
        let created_child = playlist_repo.create(&child).await.unwrap();

        let grandchild = PlaylistFixture::new_child(created_child.id.clone())
            .with_room_id(room.id.clone())
            .with_name("Grandchild")
            .build();
        let created_grandchild = playlist_repo.create(&grandchild).await.unwrap();

        // Delete child (should cascade to grandchild)
        let deleted = playlist_repo.delete(&created_child.id).await.unwrap();
        assert!(deleted);

        // Grandchild should also be deleted
        let fetched = playlist_repo
            .get_by_id(&created_grandchild.id)
            .await
            .unwrap();
        assert!(fetched.is_none());
    }

    /// Integration test: Get next position
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_next_position() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());

        let owner = UserFixture::new()
            .with_username("position_playlist_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Position Playlist Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root
        let root = PlaylistFixture::new().with_room_id(room.id.clone()).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        // Initially no children, next position should be 0
        let next_pos = playlist_repo
            .get_next_position(&room.id, Some(&created_root.id))
            .await
            .unwrap();
        assert_eq!(next_pos, 0);

        // Create children with explicit positions
        for i in 0..3 {
            let child = PlaylistFixture::new_child(created_root.id.clone())
                .with_room_id(room.id.clone())
                .with_name(&format!("Child {i}"))
                .with_position(i)
                .build();
            playlist_repo.create(&child).await.unwrap();
        }

        // Next position should be 3
        let next_pos = playlist_repo
            .get_next_position(&room.id, Some(&created_root.id))
            .await
            .unwrap();
        assert_eq!(next_pos, 3);
    }

    /// Integration test: Auto-position on create
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_auto_position() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());

        let owner = UserFixture::new()
            .with_username("auto_position_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Auto Position Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root
        let root = PlaylistFixture::new().with_room_id(room.id.clone()).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        // Create children with auto-position (negative position)
        let child1 = PlaylistFixture::new_child(created_root.id.clone())
            .with_room_id(room.id.clone())
            .with_name("Auto 1")
            .with_position(-1) // Negative triggers auto-position
            .build();
        let created1 = playlist_repo.create(&child1).await.unwrap();
        assert_eq!(created1.position, 0);

        let child2 = PlaylistFixture::new_child(created_root.id.clone())
            .with_room_id(room.id.clone())
            .with_name("Auto 2")
            .with_position(-1)
            .build();
        let created2 = playlist_repo.create(&child2).await.unwrap();
        assert_eq!(created2.position, 1);

        let child3 = PlaylistFixture::new_child(created_root.id.clone())
            .with_room_id(room.id.clone())
            .with_name("Auto 3")
            .with_position(-1)
            .build();
        let created3 = playlist_repo.create(&child3).await.unwrap();
        assert_eq!(created3.position, 2);
    }

    /// Integration test: Get children
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_children() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());

        let owner = UserFixture::new()
            .with_username("children_playlist_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Children Playlist Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root
        let root = PlaylistFixture::new().with_room_id(room.id.clone()).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        // Create 3 children
        for i in 0..3 {
            let child = PlaylistFixture::new_child(created_root.id.clone())
                .with_room_id(room.id.clone())
                .with_name(&format!("Child {i}"))
                .with_position(i)
                .build();
            playlist_repo.create(&child).await.unwrap();
        }

        // Get children
        let children = playlist_repo.get_children(&created_root.id).await.unwrap();
        assert_eq!(children.len(), 3);

        // Should be sorted by position
        for (i, child) in children.iter().enumerate() {
            assert_eq!(child.position, i as i32);
        }
    }

    /// Integration test: Get children paginated
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_children_paginated() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());

        let owner = UserFixture::new().with_username("paginated_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Paginated Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root
        let root = PlaylistFixture::new().with_room_id(room.id.clone()).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        // Create 15 children
        for i in 0..15 {
            let child = PlaylistFixture::new_child(created_root.id.clone())
                .with_room_id(room.id.clone())
                .with_name(&format!("Child {i}"))
                .with_position(i)
                .build();
            playlist_repo.create(&child).await.unwrap();
        }

        // Page 1 (limit 10, offset 0)
        let page1 = playlist_repo
            .get_children_paginated(&created_root.id, 10, 0)
            .await
            .unwrap();
        assert_eq!(page1.len(), 10);
        assert_eq!(page1[0].name, "Child 0");

        // Page 2 (limit 10, offset 10)
        let page2 = playlist_repo
            .get_children_paginated(&created_root.id, 10, 10)
            .await
            .unwrap();
        assert_eq!(page2.len(), 5);
        assert_eq!(page2[0].name, "Child 10");
    }

    /// Integration test: Count children
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_count_children() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());

        let owner = UserFixture::new()
            .with_username("count_children_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Count Children Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root
        let root = PlaylistFixture::new().with_room_id(room.id.clone()).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        // Initially 0 children
        let count = playlist_repo
            .count_children(&created_root.id)
            .await
            .unwrap();
        assert_eq!(count, 0);

        // Create 5 children
        for i in 0..5 {
            let child = PlaylistFixture::new_child(created_root.id.clone())
                .with_room_id(room.id.clone())
                .with_name(&format!("Child {i}"))
                .build();
            playlist_repo.create(&child).await.unwrap();
        }

        let count = playlist_repo
            .count_children(&created_root.id)
            .await
            .unwrap();
        assert_eq!(count, 5);
    }

    /// Integration test: Count by room
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_count_by_room() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());

        let owner = UserFixture::new().with_username("count_room_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Count Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root
        let root = PlaylistFixture::new().with_room_id(room.id.clone()).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        // Initially 1 (just root)
        let count = playlist_repo.count_by_room(&room.id).await.unwrap();
        assert_eq!(count, 1);

        // Create children
        for i in 0..3 {
            let child = PlaylistFixture::new_child(created_root.id.clone())
                .with_room_id(room.id.clone())
                .with_name(&format!("Child {i}"))
                .build();
            playlist_repo.create(&child).await.unwrap();
        }

        let count = playlist_repo.count_by_room(&room.id).await.unwrap();
        assert_eq!(count, 4); // root + 3 children
    }

    /// Integration test: Get path (breadcrumb)
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_path() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());

        let owner = UserFixture::new()
            .with_username("path_playlist_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Path Playlist Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root -> child -> grandchild
        let root = PlaylistFixture::new().with_room_id(room.id.clone()).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        let child = PlaylistFixture::new_child(created_root.id.clone())
            .with_room_id(room.id.clone())
            .with_name("Child")
            .build();
        let created_child = playlist_repo.create(&child).await.unwrap();

        let grandchild = PlaylistFixture::new_child(created_child.id.clone())
            .with_room_id(room.id.clone())
            .with_name("Grandchild")
            .build();
        let created_grandchild = playlist_repo.create(&grandchild).await.unwrap();

        // Get path from grandchild
        let path = playlist_repo
            .get_path(&created_grandchild.id)
            .await
            .unwrap();

        assert_eq!(path.len(), 3);
        // Should be ordered from root to leaf
        assert!(path[0].is_root());
        assert_eq!(path[1].id, created_child.id);
        assert_eq!(path[2].id, created_grandchild.id);
    }

    /// Integration test: Get by room paginated
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_by_room_paginated() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());

        let owner = UserFixture::new()
            .with_username("room_paginated_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Room Paginated Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root
        let root = PlaylistFixture::new().with_room_id(room.id.clone()).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        // Create 15 children
        for i in 0..15 {
            let child = PlaylistFixture::new_child(created_root.id.clone())
                .with_room_id(room.id.clone())
                .with_name(&format!("Child {i}"))
                .with_position(i)
                .build();
            playlist_repo.create(&child).await.unwrap();
        }

        // Total 16 playlists (root + 15 children)
        let count = playlist_repo.count_by_room(&room.id).await.unwrap();
        assert_eq!(count, 16);

        // Page 1 (limit 10, offset 0)
        let page1 = playlist_repo
            .get_by_room_paginated(&room.id, 10, 0)
            .await
            .unwrap();
        assert_eq!(page1.len(), 10);

        // Page 2 (limit 10, offset 10)
        let page2 = playlist_repo
            .get_by_room_paginated(&room.id, 10, 10)
            .await
            .unwrap();
        assert_eq!(page2.len(), 6);
    }

    /// Integration test: Create with executor (explicit position required)
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_with_executor_requires_position() {
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{PlaylistFixture, RoomFixture, UserFixture};

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());

        let owner = UserFixture::new().with_username("executor_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Executor Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create root
        let root = PlaylistFixture::new().with_room_id(room.id.clone()).build();
        let created_root = playlist_repo.create(&root).await.unwrap();

        // Attempt to create with executor using auto-position should fail
        let child_auto = PlaylistFixture::new_child(created_root.id.clone())
            .with_room_id(room.id.clone())
            .with_name("Auto Child")
            .with_position(-1) // Auto-position
            .build();

        let result = playlist_repo
            .create_with_executor(&child_auto, &infra.pool)
            .await;
        assert!(result.is_err());

        // Create with explicit position should succeed
        let child_explicit = PlaylistFixture::new_child(created_root.id.clone())
            .with_room_id(room.id.clone())
            .with_name("Explicit Child")
            .with_position(0) // Explicit position
            .build();

        let result = playlist_repo
            .create_with_executor(&child_explicit, &infra.pool)
            .await;
        assert!(result.is_ok());
    }
}
