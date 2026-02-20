//! Playlist repository for database operations
//!
//! Design reference: /Volumes/workspace/rust/design/04-数据库设计.md §2.4.1

use sqlx::{PgPool, FromRow};
use crate::{
    models::{Playlist, PlaylistId, RoomId},
    Result,
};

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
                   created_at, updated_at
            FROM playlists
            WHERE id = $1
            "
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
                   created_at, updated_at
            FROM playlists
            WHERE room_id = $1 AND parent_id IS NULL AND name = ''
            "
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
                   created_at, updated_at
            FROM playlists
            WHERE parent_id = $1
            ORDER BY position ASC
            "
        )
        .bind(parent_id.as_str())
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
                   created_at, updated_at
            FROM playlists
            WHERE room_id = $1
            ORDER BY parent_id NULLS FIRST, position ASC
            "
        )
        .bind(room_id.as_str())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| Ok(Playlist::from_row(&row)?))
            .collect()
    }

    /// Create a new playlist
    ///
    /// If `playlist.position` is negative, the position is computed within a
    /// transaction using a PostgreSQL advisory lock to prevent concurrent inserts
    /// from computing the same position. Pass a non-negative position to use
    /// an explicit value (e.g., when the caller already holds a lock).
    ///
    /// `SELECT MAX(position) FOR UPDATE` cannot protect empty tables because
    /// there are no rows to lock when the table is empty. Two concurrent inserts
    /// can both see `MAX = NULL` and both compute `position = 0`, producing a
    /// UNIQUE constraint violation. An advisory lock on (room_id, parent_id)
    /// serializes position computation regardless of whether rows exist.
    pub async fn create(&self, playlist: &Playlist) -> Result<Playlist> {
        if playlist.position < 0 {
            // Use a transaction with a transaction-scoped PostgreSQL advisory lock
            // to serialize position computation across concurrent inserts, including
            // when the table is empty (where FOR UPDATE cannot lock any rows).
            let mut tx = self.pool.begin().await?;

            let parent_id_str = playlist.parent_id.as_ref().map(super::super::models::id::PlaylistId::as_str);

            // Derive a stable 64-bit advisory lock key from (room_id, parent_id).
            // We fold the room_id hash and an optional parent_id hash into a single
            // i64 so that different (room, parent) pairs use distinct locks.
            let lock_key: i64 = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                playlist.room_id.as_str().hash(&mut h);
                parent_id_str.hash(&mut h);
                // Cast the u64 to i64 via bitwise reinterpretation so PostgreSQL
                // receives a valid bigint (PostgreSQL bigint = i64).
                h.finish() as i64
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
                "
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
                          created_at, updated_at
                "
            )
            .bind(playlist.id.as_str())
            .bind(playlist.room_id.as_str())
            .bind(playlist.creator_id.as_ref().map(super::super::models::id::UserId::as_str))
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
            let parent_id_str = playlist.parent_id.as_ref().map(super::super::models::id::PlaylistId::as_str);
            let row = sqlx::query(
                r"
                INSERT INTO playlists (id, room_id, creator_id, name, parent_id, position,
                                       source_provider, source_config, provider_instance_name)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                RETURNING id, room_id, creator_id, name, parent_id, position,
                          source_provider, source_config, provider_instance_name,
                          created_at, updated_at
                "
            )
            .bind(playlist.id.as_str())
            .bind(playlist.room_id.as_str())
            .bind(playlist.creator_id.as_ref().map(super::super::models::id::UserId::as_str))
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
    pub async fn create_with_executor<'e, E>(&self, playlist: &Playlist, executor: E) -> Result<Playlist>
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
        let parent_id_str = playlist.parent_id.as_ref().map(super::super::models::id::PlaylistId::as_str);

        let row = sqlx::query(
            r"
            INSERT INTO playlists (id, room_id, creator_id, name, parent_id, position,
                                   source_provider, source_config, provider_instance_name)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            RETURNING id, room_id, creator_id, name, parent_id, position,
                      source_provider, source_config, provider_instance_name,
                      created_at, updated_at
            "
        )
        .bind(playlist.id.as_str())
        .bind(playlist.room_id.as_str())
        .bind(playlist.creator_id.as_ref().map(super::super::models::id::UserId::as_str))
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
        let lock_key: i64 = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            room_id.as_str().hash(&mut h);
            parent_id_str.hash(&mut h);
            h.finish() as i64
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
            "
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
    pub async fn get_next_position(&self, room_id: &RoomId, parent_id: Option<&PlaylistId>) -> Result<i32> {
        let max_pos: Option<i32> = sqlx::query_scalar(
            r"
            SELECT MAX(position)
            FROM playlists
            WHERE room_id = $1
              AND parent_id IS NOT DISTINCT FROM $2
            "
        )
        .bind(room_id.as_str())
        .bind(parent_id.map(super::super::models::id::PlaylistId::as_str))
        .fetch_one(&self.pool)
        .await?;

        Ok(max_pos.unwrap_or(-1) + 1)
    }

    /// Update playlist
    pub async fn update(&self, playlist: &Playlist) -> Result<Playlist> {
        let source_provider_str = playlist.source_provider.as_deref();
        let row = sqlx::query(
            r"
            UPDATE playlists
            SET name = $2, position = $3, source_provider = $4, source_config = $5,
                provider_instance_name = $6
            WHERE id = $1
            RETURNING id, room_id, creator_id, name, parent_id, position,
                      source_provider, source_config, provider_instance_name,
                      created_at, updated_at
            "
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
                       created_at, updated_at, 0 AS depth
                FROM playlists
                WHERE id = $1
              UNION ALL
                SELECT p.id, p.room_id, p.creator_id, p.name, p.parent_id, p.position,
                       p.source_provider, p.source_config, p.provider_instance_name,
                       p.created_at, p.updated_at, a.depth + 1
                FROM playlists p
                JOIN ancestors a ON p.id = a.parent_id
                WHERE a.depth < 50
            )
            SELECT id, room_id, creator_id, name, parent_id, position,
                   source_provider, source_config, provider_instance_name,
                   created_at, updated_at
            FROM ancestors
            ORDER BY depth DESC
            "
        )
        .bind(playlist_id.as_str())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(|row| Ok(Playlist::from_row(&row)?)).collect()
    }

}
