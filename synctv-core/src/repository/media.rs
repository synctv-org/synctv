//! Media repository for database operations
//!
//! Design reference: /Volumes/workspace/rust/design/04-数据库设计.md §2.4.2

use sqlx::{PgPool, FromRow};

use crate::{
    models::{Media, MediaId, PageParams, PlaylistId, RoomId},
    Result,
};

/// Media repository for database operations
#[derive(Clone)]
pub struct MediaRepository {
    pool: PgPool,
}

impl MediaRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Get a reference to the connection pool
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Add media to playlist
    pub async fn create(&self, media: &Media) -> Result<Media> {
        self.create_with_executor(media, &self.pool).await
    }

    /// Add media to playlist using a provided executor (for transaction support)
    pub async fn create_with_executor<'e, E>(&self, media: &Media, executor: E) -> Result<Media>
    where
        E: sqlx::Executor<'e, Database = sqlx::Postgres>,
    {
        let source_config_json = serde_json::to_value(&media.source_config)?;

        let row = sqlx::query(
            r"
            INSERT INTO media (id, playlist_id, room_id, creator_id, name, position,
                              source_provider, source_config, provider_instance_name, added_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
             RETURNING id, playlist_id, room_id, creator_id, name, position,
                       source_provider, source_config, provider_instance_name,
                       added_at
            "
        )
        .bind(media.id.as_str())
        .bind(media.playlist_id.as_str())
        .bind(media.room_id.as_str())
        .bind(media.creator_id.as_ref().map(super::super::models::id::UserId::as_str))
        .bind(&media.name)
        .bind(media.position)
        .bind(media.source_provider.as_str())
        .bind(&source_config_json)
        .bind(&media.provider_instance_name)
        .bind(media.added_at)
        .fetch_one(executor)
        .await?;

        Ok(Media::from_row(&row)?)
    }

    /// Batch insert media items.
    ///
    /// Automatically chunks large batches to stay within `PostgreSQL`'s 65535
    /// bind-parameter limit (each row uses 10 parameters, so we chunk at 1000
    /// rows = 10000 parameters per statement).
    pub async fn create_batch(&self, items: &[Media]) -> Result<Vec<Media>> {
        let mut tx = self.pool.begin().await?;
        let results = self.create_batch_chunked(items, &mut tx).await?;
        tx.commit().await?;
        Ok(results)
    }

    /// Batch insert media items using a provided executor (for transaction support).
    ///
    /// Inserts all items in a single statement. For large batches (>1000 items),
    /// prefer `create_batch_chunked` which automatically splits into chunks.
    pub async fn create_batch_with_executor<'e, E>(&self, items: &[Media], executor: E) -> Result<Vec<Media>>
    where
        E: sqlx::Executor<'e, Database = sqlx::Postgres>,
    {
        if items.is_empty() {
            return Ok(Vec::new());
        }

        Self::create_batch_chunk(items, executor).await
    }

    /// Internal: insert a single chunk of media items (max 1000).
    ///
    /// Each row occupies 10 bind parameters. `PostgreSQL`'s hard limit is 65535
    /// parameters per statement, giving a safe ceiling of 6553 rows. We enforce
    /// a tighter 1000-row limit so callers of `create_batch_with_executor`
    /// receive a clear error rather than a cryptic protocol failure in production.
    async fn create_batch_chunk<'e, E>(items: &[Media], executor: E) -> Result<Vec<Media>>
    where
        E: sqlx::Executor<'e, Database = sqlx::Postgres>,
    {
        /// Number of bind parameters per row.
        const PARAMS_PER_ROW: usize = 10;
        /// Maximum rows per INSERT statement (well within the 65535 parameter limit).
        const MAX_ROWS_PER_CHUNK: usize = 1000;

        if items.is_empty() {
            return Ok(Vec::new());
        }
        if items.len() > MAX_ROWS_PER_CHUNK {
            return Err(crate::Error::InvalidInput(format!(
                "Batch insert chunk too large: {} rows exceed the {} row limit \
                 ({} bind parameters). Use create_batch_chunked to split automatically.",
                items.len(),
                MAX_ROWS_PER_CHUNK,
                items.len() * PARAMS_PER_ROW,
            )));
        }

        let mut results = Vec::with_capacity(items.len());

        let mut query_builder = String::from(
            "INSERT INTO media (id, playlist_id, room_id, creator_id, name, position,
                               source_provider, source_config, provider_instance_name, added_at)
             VALUES "
        );
        let mut binds = Vec::new();
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                query_builder.push_str(", ");
            }
            let base = i * 10;
            query_builder.push_str(&format!(
                "(${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${})",
                base + 1, base + 2, base + 3, base + 4, base + 5,
                base + 6, base + 7, base + 8, base + 9, base + 10
            ));
            binds.push(serde_json::to_value(&item.source_config)?);
        }
        query_builder.push_str(
            " RETURNING id, playlist_id, room_id, creator_id, name, position,
                       source_provider, source_config, provider_instance_name,
                       added_at"
        );

        let mut query = sqlx::query(&query_builder);
        for (i, item) in items.iter().enumerate() {
            query = query
                .bind(item.id.as_str())
                .bind(item.playlist_id.as_str())
                .bind(item.room_id.as_str())
                .bind(item.creator_id.as_ref().map(super::super::models::id::UserId::as_str))
                .bind(&item.name)
                .bind(item.position)
                .bind(item.source_provider.as_str())
                .bind(&binds[i])
                .bind(&item.provider_instance_name)
                .bind(item.added_at);
        }

        let rows = query.fetch_all(executor).await?;
        for row in rows {
            results.push(Media::from_row(&row)?);
        }

        Ok(results)
    }

    /// Batch insert media items within a transaction, automatically chunking
    /// to stay within `PostgreSQL`'s bind-parameter limit.
    pub async fn create_batch_chunked(
        &self,
        items: &[Media],
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<Vec<Media>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }

        const CHUNK_SIZE: usize = 1000;
        let mut all_results = Vec::with_capacity(items.len());

        for chunk in items.chunks(CHUNK_SIZE) {
            let results = Self::create_batch_chunk(chunk, &mut **tx).await?;
            all_results.extend(results);
        }

        Ok(all_results)
    }

    /// Update media
    pub async fn update(&self, media: &Media) -> Result<Media> {
        let source_config_json = serde_json::to_value(&media.source_config)?;

        let row = sqlx::query(
            r"
            UPDATE media
            SET name = $2, position = $3, source_config = $4,
                provider_instance_name = $5
             WHERE id = $1             RETURNING id, playlist_id, room_id, creator_id, name, position,
                       source_provider, source_config, provider_instance_name,
                       added_at
            "
        )
        .bind(media.id.as_str())
        .bind(&media.name)
        .bind(media.position)
        .bind(&source_config_json)
        .bind(&media.provider_instance_name)
        .fetch_one(&self.pool)
        .await?;

        Ok(Media::from_row(&row)?)
    }

    /// Conditional update: only succeeds if the row's name and position still
    /// match `old_name`/`old_position`, providing optimistic locking without a
    /// dedicated version column. Returns `Ok(None)` on conflict (no rows updated).
    pub async fn update_if_unchanged(
        &self,
        media: &Media,
        old_name: &str,
        old_position: i32,
    ) -> Result<Option<Media>> {
        let source_config_json = serde_json::to_value(&media.source_config)?;

        let row = sqlx::query(
            r"
            UPDATE media
            SET name = $2, position = $3, source_config = $4,
                provider_instance_name = $5
             WHERE id = $1 AND name = $6 AND position = $7
             RETURNING id, playlist_id, room_id, creator_id, name, position,
                       source_provider, source_config, provider_instance_name,
                       added_at
            "
        )
        .bind(media.id.as_str())
        .bind(&media.name)
        .bind(media.position)
        .bind(&source_config_json)
        .bind(&media.provider_instance_name)
        .bind(old_name)
        .bind(old_position)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => Ok(Some(Media::from_row(&row)?)),
            None => Ok(None),
        }
    }

    /// Get media by ID
    pub async fn get_by_id(&self, media_id: &MediaId) -> Result<Option<Media>> {
        let row = sqlx::query(
            r"
            SELECT id, playlist_id, room_id, creator_id, name, position,
                   source_provider, source_config, provider_instance_name,
                   added_at
             FROM media
             WHERE id = $1            "
        )
        .bind(media_id.as_str())
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => Ok(Some(Media::from_row(&row)?)),
            None => Ok(None),
        }
    }

    /// Get multiple media items by IDs in a single query
    pub async fn get_by_ids(&self, media_ids: &[MediaId]) -> Result<Vec<Media>> {
        self.get_by_ids_with_executor(media_ids, &self.pool).await
    }

    /// Get multiple media items by IDs using a specific executor (for transaction support)
    pub async fn get_by_ids_with_executor<'e, E>(&self, media_ids: &[MediaId], executor: E) -> Result<Vec<Media>>
    where
        E: sqlx::Executor<'e, Database = sqlx::Postgres>,
    {
        if media_ids.is_empty() {
            return Ok(Vec::new());
        }

        let id_strs: Vec<&str> = media_ids.iter().map(MediaId::as_str).collect();
        let rows = sqlx::query(
            r"
            SELECT id, playlist_id, room_id, creator_id, name, position,
                   source_provider, source_config, provider_instance_name,
                   added_at
             FROM media
             WHERE id = ANY($1)            "
        )
        .bind(&id_strs)
        .fetch_all(executor)
        .await?;

        rows.into_iter().map(|row| Ok(Media::from_row(&row)?)).collect()
    }

    /// Get playlist for a room (all media in room's root playlist and sub-playlists)
    pub async fn get_playlist(&self, room_id: &RoomId) -> Result<Vec<Media>> {
        let rows = sqlx::query(
            r"
            SELECT id, playlist_id, room_id, creator_id, name, position,
                   source_provider, source_config, provider_instance_name,
                   added_at
             FROM media
             WHERE room_id = $1             ORDER BY playlist_id, position ASC
            "
        )
        .bind(room_id.as_str())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(|row| Ok(Media::from_row(&row)?)).collect()
    }

    /// Get media in a specific playlist
    pub async fn get_by_playlist(&self, playlist_id: &PlaylistId) -> Result<Vec<Media>> {
        let rows = sqlx::query(
            r"
            SELECT id, playlist_id, room_id, creator_id, name, position,
                   source_provider, source_config, provider_instance_name,
                   added_at
             FROM media
             WHERE playlist_id = $1             ORDER BY position ASC
            "
        )
        .bind(playlist_id.as_str())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(|row| Ok(Media::from_row(&row)?)).collect()
    }

    /// Get paginated playlist
    pub async fn get_playlist_paginated(
        &self,
        playlist_id: &PlaylistId,
        pagination: PageParams,
    ) -> Result<(Vec<Media>, i64)> {
        let limit = pagination.limit() as i64;
        let offset = pagination.offset() as i64;

        // Get total count
        let total: i64 = sqlx::query_scalar(
            r"
            SELECT COUNT(*) FROM media WHERE playlist_id = $1            "
        )
        .bind(playlist_id.as_str())
        .fetch_one(&self.pool)
        .await?;

        // Get paginated results
        let rows = sqlx::query(
            r"
            SELECT id, playlist_id, room_id, creator_id, name, position,
                   source_provider, source_config, provider_instance_name,
                   added_at
             FROM media
             WHERE playlist_id = $1             ORDER BY position ASC
             LIMIT $2 OFFSET $3
            "
        )
        .bind(playlist_id.as_str())
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        let items: Vec<Media> = rows.into_iter().map(|row| Ok(Media::from_row(&row)?)).collect::<Result<Vec<Media>>>()?;

        Ok((items, total))
    }

    /// Delete media from playlist
    pub async fn delete(&self, media_id: &MediaId) -> Result<bool> {
        let result = sqlx::query(
            r"
            DELETE FROM media
             WHERE id = $1
            "
        )
        .bind(media_id.as_str())
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Delete all media in a playlist
    pub async fn delete_by_playlist(&self, playlist_id: &PlaylistId) -> Result<usize> {
        let result = sqlx::query(
            r"
            DELETE FROM media
             WHERE playlist_id = $1
            "
        )
        .bind(playlist_id.as_str())
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() as usize)
    }

    /// Bulk delete media items by IDs
    pub async fn delete_batch(&self, media_ids: &[MediaId]) -> Result<usize> {
        self.delete_batch_with_executor(media_ids, &self.pool).await
    }

    /// Bulk delete media items by IDs using a specific executor (for transaction support)
    pub async fn delete_batch_with_executor<'e, E>(&self, media_ids: &[MediaId], executor: E) -> Result<usize>
    where
        E: sqlx::Executor<'e, Database = sqlx::Postgres>,
    {
        if media_ids.is_empty() {
            return Ok(0);
        }

        let id_strs: Vec<&str> = media_ids.iter().map(MediaId::as_str).collect();

        let result = sqlx::query(
            r"
            DELETE FROM media
             WHERE id = ANY($1)
            "
        )
        .bind(&id_strs)
        .execute(executor)
        .await?;

        Ok(result.rows_affected() as usize)
    }

    /// Swap positions of two media
    pub async fn swap_positions(&self, media_id1: &MediaId, media_id2: &MediaId) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        self.swap_positions_with_tx(media_id1, media_id2, &mut tx).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Swap positions of two media using a provided transaction.
    ///
    /// Uses a two-phase sentinel approach to avoid violating the
    /// `UNIQUE(playlist_id`, position) constraint. `PostgreSQL` evaluates UNIQUE
    /// constraints per-row during multi-row UPDATEs, so a single-statement
    /// CTE swap can trigger a violation. Instead:
    ///   1. Lock both rows with FOR UPDATE (ordered by id to prevent deadlocks).
    ///   2. Move both to negative sentinel positions (clearing the constraint space).
    ///   3. Set them to their swapped final positions.
    pub async fn swap_positions_with_tx(
        &self,
        media_id1: &MediaId,
        media_id2: &MediaId,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<()> {
        // Lock both rows in id order to prevent deadlocks
        let rows = sqlx::query(
            r"
            SELECT id, position
            FROM media
            WHERE id IN ($1, $2)
            ORDER BY id
            FOR UPDATE
            "
        )
        .bind(media_id1.as_str())
        .bind(media_id2.as_str())
        .fetch_all(&mut **tx)
        .await?;

        if rows.len() != 2 {
            return Err(crate::Error::NotFound(
                "One or both media items not found for swap".to_string(),
            ));
        }

        use sqlx::Row;
        let id1_str: String = rows[0].try_get("id")?;
        let pos1: i32 = rows[0].try_get("position")?;
        let id2_str: String = rows[1].try_get("id")?;
        let pos2: i32 = rows[1].try_get("position")?;

        // Phase 1: Move both to negative sentinel positions
        sqlx::query("UPDATE media SET position = $2 WHERE id = $1")
            .bind(&id1_str)
            .bind(-1000 - pos1)
            .execute(&mut **tx)
            .await?;
        sqlx::query("UPDATE media SET position = $2 WHERE id = $1")
            .bind(&id2_str)
            .bind(-1000 - pos2)
            .execute(&mut **tx)
            .await?;

        // Phase 2: Set swapped final positions
        sqlx::query("UPDATE media SET position = $2 WHERE id = $1")
            .bind(&id1_str)
            .bind(pos2)
            .execute(&mut **tx)
            .await?;
        sqlx::query("UPDATE media SET position = $2 WHERE id = $1")
            .bind(&id2_str)
            .bind(pos1)
            .execute(&mut **tx)
            .await?;

        Ok(())
    }

    /// Bulk reorder media items with new positions
    /// Takes a list of (`media_id`, `new_position`) tuples and updates them in a transaction.
    /// Uses FOR UPDATE locks to prevent concurrent reordering race conditions.
    pub async fn reorder_batch(&self, updates: &[(MediaId, i32)]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        self.reorder_batch_with_tx(updates, &mut tx).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Bulk reorder media items using a provided transaction
    ///
    /// Sorts updates by `media_id` before acquiring FOR UPDATE locks to prevent
    /// deadlocks when concurrent transactions lock the same rows in different order.
    /// Uses a two-phase approach (sentinel then final values) to avoid violating
    /// the `UNIQUE(playlist_id`, position) constraint during intermediate states.
    pub async fn reorder_batch_with_tx(
        &self,
        updates: &[(MediaId, i32)],
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<()> {
        if updates.is_empty() {
            return Ok(());
        }

        // Sort by media_id to ensure consistent lock ordering across concurrent transactions.
        // Without this, two transactions locking [A, B] and [B, A] can deadlock.
        let mut sorted_updates: Vec<_> = updates.to_vec();
        sorted_updates.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));

        // Lock all affected rows first to prevent concurrent modification
        for (media_id, _) in &sorted_updates {
            sqlx::query("SELECT id FROM media WHERE id = $1 FOR UPDATE")
                .bind(media_id.as_str())
                .fetch_optional(&mut **tx)
                .await?;
        }

        // Phase 1: Move all affected rows to negative sentinel positions to
        // clear the UNIQUE constraint space for the final positions.
        for (i, (media_id, _)) in sorted_updates.iter().enumerate() {
            let sentinel = -(i as i32) - 1; // -1, -2, -3, ...
            sqlx::query("UPDATE media SET position = $2 WHERE id = $1")
                .bind(media_id.as_str())
                .bind(sentinel)
                .execute(&mut **tx)
                .await?;
        }

        // Phase 2: Set the final positions (now safe since no collisions)
        for (media_id, new_position) in &sorted_updates {
            sqlx::query("UPDATE media SET position = $2 WHERE id = $1")
                .bind(media_id.as_str())
                .bind(new_position)
                .execute(&mut **tx)
                .await?;
        }

        Ok(())
    }

    /// Get the next available position in a playlist (read-only, no locking).
    ///
    /// **WARNING**: This method does NOT hold a lock, so concurrent inserts may
    /// produce duplicate positions. Use [`get_next_position_with_tx`] inside an
    /// existing transaction for any write path (e.g. `add_media`, `add_batch`).
    ///
    /// This method is only safe for read-only / advisory purposes (e.g. UI hints).
    #[deprecated(note = "Use get_next_position_with_tx inside a transaction for write paths")]
    pub async fn get_next_position(&self, playlist_id: &PlaylistId) -> Result<i32> {
        let next_pos: i32 = sqlx::query_scalar(
            r"
            SELECT COALESCE(MAX(position), -1) + 1
            FROM media
            WHERE playlist_id = $1            "
        )
        .bind(playlist_id.as_str())
        .fetch_one(&self.pool)
        .await?;

        Ok(next_pos)
    }

    /// Get the next available position in a playlist within a transaction.
    ///
    /// Locks all rows in the playlist with `FOR UPDATE` via a subquery, then
    /// computes `MAX(position) + 1` over the locked set. This avoids the
    /// `PostgreSQL` restriction that forbids `FOR UPDATE` with aggregate functions
    /// while still preventing concurrent inserts from assigning duplicate positions.
    /// The lock is held until the caller commits or rolls back the transaction.
    pub async fn get_next_position_with_tx(
        &self,
        playlist_id: &PlaylistId,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<i32> {
        let next_pos: i32 = sqlx::query_scalar(
            r"
            SELECT COALESCE(MAX(position), -1) + 1
            FROM (SELECT position FROM media WHERE playlist_id = $1 FOR UPDATE) sub
            "
        )
        .bind(playlist_id.as_str())
        .fetch_one(&mut **tx)
        .await?;

        Ok(next_pos)
    }

    /// Count media items in a playlist
    pub async fn count_by_playlist(&self, playlist_id: &PlaylistId) -> Result<i64> {
        let count: i64 = sqlx::query_scalar(
            r"
            SELECT COUNT(*) FROM media WHERE playlist_id = $1            "
        )
        .bind(playlist_id.as_str())
        .fetch_one(&self.pool)
        .await?;

        Ok(count)
    }

    /// Batch count media items across multiple playlists
    pub async fn count_by_playlists_batch(&self, playlist_ids: &[&str]) -> Result<std::collections::HashMap<String, i64>> {
        use sqlx::Row;
        let rows = sqlx::query(
            r"
            SELECT playlist_id, COUNT(*) as cnt
            FROM media
            WHERE playlist_id = ANY($1)            GROUP BY playlist_id
            "
        )
        .bind(playlist_ids)
        .fetch_all(&self.pool)
        .await?;

        let mut result = std::collections::HashMap::new();
        for row in rows {
            let pid: String = row.try_get("playlist_id")?;
            let cnt: i64 = row.try_get("cnt")?;
            result.insert(pid, cnt);
        }
        Ok(result)
    }

}

