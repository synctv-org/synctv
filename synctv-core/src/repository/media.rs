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
                              source_provider, source_config, provider_instance_name, added_at, version)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 0)
             RETURNING id, playlist_id, room_id, creator_id, name, position,
                       source_provider, source_config, provider_instance_name,
                       added_at, version
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
                               source_provider, source_config, provider_instance_name, added_at, version)
             VALUES "
        );
        let mut binds = Vec::new();
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                query_builder.push_str(", ");
            }
            let base = i * 10;
            query_builder.push_str(&format!(
                "(${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, 0)",
                base + 1, base + 2, base + 3, base + 4, base + 5,
                base + 6, base + 7, base + 8, base + 9, base + 10
            ));
            binds.push(serde_json::to_value(&item.source_config)?);
        }
        query_builder.push_str(
            " RETURNING id, playlist_id, room_id, creator_id, name, position,
                       source_provider, source_config, provider_instance_name,
                       added_at, version"
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
                       added_at, version
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
    #[deprecated(note = "Use update_with_version for proper optimistic locking")]
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
                provider_instance_name = $5, version = version + 1
             WHERE id = $1 AND name = $6 AND position = $7
             RETURNING id, playlist_id, room_id, creator_id, name, position,
                       source_provider, source_config, provider_instance_name,
                       added_at, version
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

    /// Optimistic locking update: only succeeds if the row's version matches
    /// the provided `expected_version`. Returns `Ok(Some(Media))` with the updated
    /// row (version incremented) on success, or `Ok(None)` if the version doesn't
    /// match (indicating a concurrent modification).
    ///
    /// # Example
    /// ```text
    /// let media = repo.get_by_id(&id).await?.unwrap();
    /// let mut updated = media.clone();
    /// updated.name = "new_name".to_string();
    /// match repo.update_with_version(&updated, media.version).await? {
    ///     Some(result) => println!("Updated to version {}", result.version),
    ///     None => println!("Conflict! Someone else modified this media."),
    /// }
    /// ```
    pub async fn update_with_version(
        &self,
        media: &Media,
        expected_version: i32,
    ) -> Result<Option<Media>> {
        let source_config_json = serde_json::to_value(&media.source_config)?;

        let row = sqlx::query(
            r"
            UPDATE media
            SET name = $2, position = $3, source_config = $4,
                provider_instance_name = $5, version = version + 1
             WHERE id = $1 AND version = $6
             RETURNING id, playlist_id, room_id, creator_id, name, position,
                       source_provider, source_config, provider_instance_name,
                       added_at, version
            "
        )
        .bind(media.id.as_str())
        .bind(&media.name)
        .bind(media.position)
        .bind(&source_config_json)
        .bind(&media.provider_instance_name)
        .bind(expected_version)
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
                   added_at, version
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
                   added_at, version
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
                   added_at, version
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
                   added_at, version
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
                   added_at, version
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

        // Phase 1: Move both to negative sentinel positions.
        // M-6: Use i32::MIN + offset to avoid collisions with normal
        // positions (which could be negative near -1000).
        sqlx::query("UPDATE media SET position = $2 WHERE id = $1")
            .bind(&id1_str)
            .bind(i32::MIN.wrapping_add(1))
            .execute(&mut **tx)
            .await?;
        sqlx::query("UPDATE media SET position = $2 WHERE id = $1")
            .bind(&id2_str)
            .bind(i32::MIN.wrapping_add(2))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::id::{MediaId, PlaylistId, RoomId, UserId};

    /// Unit test: Media builder pattern
    #[test]
    fn test_media_from_provider() {
        let playlist_id = PlaylistId::new();
        let room_id = RoomId::new();
        let creator_id = UserId::new();

        let media = Media::from_provider(
            playlist_id.clone(),
            room_id.clone(),
            Some(creator_id.clone()),
            "Test Video".to_string(),
            serde_json::json!({"url": "https://example.com/video.mp4"}),
            "direct_url",
            "default".to_string(),
            0,
        );

        assert_eq!(media.name, "Test Video");
        assert_eq!(media.position, 0);
        assert_eq!(media.source_provider, "direct_url");
        assert!(media.is_direct());
    }

    /// Unit test: is_direct() check
    #[test]
    fn test_media_is_direct() {
        let playlist_id = PlaylistId::new();
        let room_id = RoomId::new();

        let direct_media = Media::from_provider(
            playlist_id.clone(),
            room_id.clone(),
            None,
            "Direct Video".to_string(),
            serde_json::json!({}),
            "direct_url",
            "default".to_string(),
            0,
        );
        assert!(direct_media.is_direct());

        let bilibili_media = Media::from_provider(
            playlist_id,
            room_id,
            None,
            "Bilibili Video".to_string(),
            serde_json::json!({"bvid": "BV1234567890"}),
            "bilibili",
            "bilibili_main".to_string(),
            1,
        );
        assert!(!bilibili_media.is_direct());
    }

    /// Unit test: Media::from_direct_single_mode
    #[test]
    fn test_media_from_direct_single_mode() {
        let playlist_id = PlaylistId::new();
        let room_id = RoomId::new();
        let creator_id = UserId::new();

        let playback_info = crate::models::media::PlaybackInfo::single_url(
            "https://example.com/video.mp4".to_string(),
            "1080P".to_string(),
        );

        let media = Media::from_direct_single_mode(
            playlist_id.clone(),
            room_id.clone(),
            Some(creator_id.clone()),
            "Single Mode Video".to_string(),
            "direct",
            playback_info,
            5,
        );

        assert_eq!(media.name, "Single Mode Video");
        assert_eq!(media.position, 5);
        assert!(media.is_direct());
        assert!(media.source_config.get("playback_infos").is_some());
    }

    /// Unit test: Media::from_direct_multimode
    #[test]
    fn test_media_from_direct_multimode() {
        let playlist_id = PlaylistId::new();
        let room_id = RoomId::new();

        let mut playback_infos = std::collections::HashMap::new();
        playback_infos.insert(
            "direct".to_string(),
            crate::models::media::PlaybackInfo::single_url(
                "https://example.com/video.mp4".to_string(),
                "1080P".to_string(),
            ),
        );
        playback_infos.insert(
            "proxied".to_string(),
            crate::models::media::PlaybackInfo::single_url(
                "https://proxy.example.com/video.mp4".to_string(),
                "720P".to_string(),
            ),
        );

        let mut metadata = std::collections::HashMap::new();
        metadata.insert("duration".to_string(), serde_json::json!(3600));

        let media = Media::from_direct_multimode(
            playlist_id,
            room_id,
            None,
            "Multimode Video".to_string(),
            playback_infos,
            "direct".to_string(),
            metadata,
            10,
        );

        assert_eq!(media.name, "Multimode Video");
        assert_eq!(media.position, 10);
        assert!(media.is_direct());
        assert!(media.source_config.get("playback_infos").is_some());
        assert!(media.source_config.get("metadata").is_some());
    }

    /// Unit test: get_playback_result for direct media
    #[test]
    fn test_get_playback_result_direct() {
        let playlist_id = PlaylistId::new();
        let room_id = RoomId::new();

        let playback_info = crate::models::media::PlaybackInfo::single_url(
            "https://example.com/video.mp4".to_string(),
            "1080P".to_string(),
        );

        let media = Media::from_direct_single_mode(
            playlist_id,
            room_id,
            None,
            "Test Video".to_string(),
            "direct",
            playback_info,
            0,
        );

        let result = media.get_playback_result();
        assert!(result.is_some());

        let playback = result.unwrap();
        assert_eq!(playback.name, "Test Video");
        assert!(playback.playback_infos.contains_key("direct"));
    }

    /// Unit test: get_playback_result returns None for non-direct media
    #[test]
    fn test_get_playback_result_non_direct() {
        let playlist_id = PlaylistId::new();
        let room_id = RoomId::new();

        let media = Media::from_provider(
            playlist_id,
            room_id,
            None,
            "Bilibili Video".to_string(),
            serde_json::json!({"bvid": "BV1234567890"}),
            "bilibili",
            "bilibili_main".to_string(),
            0,
        );

        let result = media.get_playback_result();
        assert!(result.is_none());
    }

    /// Unit test: Repository constructor
    #[test]
    fn test_repository_new() {
        fn _assert_const_new(pool: PgPool) -> MediaRepository {
            MediaRepository::new(pool)
        }
        // Compilation test only - cannot create PgPool without database
    }

    /// Integration test: Create and get media
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_and_get_media() {
        use crate::test_helpers::{RoomFixture, UserFixture, PlaylistFixture};
        use crate::repository::user::UserRepository;
        use crate::repository::room::RoomRepository;
        use crate::repository::playlist::PlaylistRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());
        let media_repo = MediaRepository::new(infra.pool.clone());

        // Create owner and room
        let owner = UserFixture::new().with_username("media_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Media Test Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_media_playlist_hierarchy(
            &playlist_repo,
            room.id.clone(),
            "Test Playlist",
        ).await;

        // Create media
        let media = Media::from_provider(
            playlist.id.clone(),
            room.id.clone(),
            Some(owner.id.clone()),
            "Test Video".to_string(),
            serde_json::json!({"url": "https://example.com/video.mp4"}),
            "direct_url",
            "default".to_string(),
            0,
        );

        let created = media_repo.create(&media).await.unwrap();
        assert_eq!(created.name, "Test Video");
        assert_eq!(created.position, 0);

        // Get by ID
        let fetched = media_repo.get_by_id(&created.id).await.unwrap();
        assert!(fetched.is_some());
        let fetched = fetched.unwrap();
        assert_eq!(fetched.name, "Test Video");
    }

    /// Integration test: Update media
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_update_media() {
        use crate::test_helpers::{RoomFixture, UserFixture, PlaylistFixture};
        use crate::repository::user::UserRepository;
        use crate::repository::room::RoomRepository;
        use crate::repository::playlist::PlaylistRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());
        let media_repo = MediaRepository::new(infra.pool.clone());

        // Setup
        let owner = UserFixture::new().with_username("media_update_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Media Update Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_media_playlist_hierarchy(
            &playlist_repo,
            room.id.clone(),
            "Test Playlist",
        ).await;

        let media = Media::from_provider(
            playlist.id.clone(),
            room.id.clone(),
            Some(owner.id.clone()),
            "Original Name".to_string(),
            serde_json::json!({}),
            "direct_url",
            "default".to_string(),
            0,
        );
        let created = media_repo.create(&media).await.unwrap();

        // Update
        let mut updated = created.clone();
        updated.name = "Updated Name".to_string();
        updated.position = 5;

        let result = media_repo.update(&updated).await.unwrap();
        assert_eq!(result.name, "Updated Name");
        assert_eq!(result.position, 5);
    }

    /// Integration test: Delete media
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_delete_media() {
        use crate::test_helpers::{RoomFixture, UserFixture, PlaylistFixture};
        use crate::repository::user::UserRepository;
        use crate::repository::room::RoomRepository;
        use crate::repository::playlist::PlaylistRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());
        let media_repo = MediaRepository::new(infra.pool.clone());

        // Setup
        let owner = UserFixture::new().with_username("media_delete_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Media Delete Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_media_playlist_hierarchy(
            &playlist_repo,
            room.id.clone(),
            "Test Playlist",
        ).await;

        let media = Media::from_provider(
            playlist.id.clone(),
            room.id.clone(),
            Some(owner.id.clone()),
            "To Delete".to_string(),
            serde_json::json!({}),
            "direct_url",
            "default".to_string(),
            0,
        );
        let created = media_repo.create(&media).await.unwrap();

        // Delete
        let deleted = media_repo.delete(&created.id).await.unwrap();
        assert!(deleted);

        // Verify deleted
        let fetched = media_repo.get_by_id(&created.id).await.unwrap();
        assert!(fetched.is_none());

        // Delete non-existent returns false
        let deleted_again = media_repo.delete(&created.id).await.unwrap();
        assert!(!deleted_again);
    }

    /// Integration test: Batch create media
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_batch() {
        use crate::test_helpers::{RoomFixture, UserFixture, PlaylistFixture};
        use crate::repository::user::UserRepository;
        use crate::repository::room::RoomRepository;
        use crate::repository::playlist::PlaylistRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());
        let media_repo = MediaRepository::new(infra.pool.clone());

        // Setup
        let owner = UserFixture::new().with_username("batch_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Batch Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_media_playlist_hierarchy(
            &playlist_repo,
            room.id.clone(),
            "Batch Playlist",
        ).await;

        // Create batch
        let items: Vec<Media> = (0..5)
            .map(|i| {
                Media::from_provider(
                    playlist.id.clone(),
                    room.id.clone(),
                    Some(owner.id.clone()),
                    format!("Video {}", i),
                    serde_json::json!({"url": format!("https://example.com/{}.mp4", i)}),
                    "direct_url",
                    "default".to_string(),
                    i,
                )
            })
            .collect();

        let created = media_repo.create_batch(&items).await.unwrap();
        assert_eq!(created.len(), 5);

        // Verify all created
        let fetched = media_repo.get_by_playlist(&playlist.id).await.unwrap();
        assert_eq!(fetched.len(), 5);
    }

    /// Integration test: Create batch exceeds limit
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_batch_chunk_too_large() {
        use crate::test_helpers::{RoomFixture, UserFixture, PlaylistFixture};
        use crate::repository::user::UserRepository;
        use crate::repository::room::RoomRepository;
        use crate::repository::playlist::PlaylistRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());
        let media_repo = MediaRepository::new(infra.pool.clone());

        // Setup
        let owner = UserFixture::new().with_username("chunk_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Chunk Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_media_playlist_hierarchy(
            &playlist_repo,
            room.id.clone(),
            "Chunk Playlist",
        ).await;

        // Create batch that exceeds chunk limit (1001 items)
        let items: Vec<Media> = (0..1001)
            .map(|i| {
                Media::from_provider(
                    playlist.id.clone(),
                    room.id.clone(),
                    Some(owner.id.clone()),
                    format!("Video {}", i),
                    serde_json::json!({"url": format!("https://example.com/{}.mp4", i)}),
                    "direct_url",
                    "default".to_string(),
                    i,
                )
            })
            .collect();

        // create_batch_chunk should fail
        // Note: create_batch will succeed because it uses chunking internally
        let result = media_repo.create_batch(&items).await;
        assert!(result.is_ok()); // Should succeed with automatic chunking
        assert_eq!(result.unwrap().len(), 1001);
    }

    /// Integration test: update_if_unchanged (optimistic locking)
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_update_if_unchanged() {
        use crate::test_helpers::{RoomFixture, UserFixture, PlaylistFixture};
        use crate::repository::user::UserRepository;
        use crate::repository::room::RoomRepository;
        use crate::repository::playlist::PlaylistRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());
        let media_repo = MediaRepository::new(infra.pool.clone());

        // Setup
        let owner = UserFixture::new().with_username("optimistic_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Optimistic Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_media_playlist_hierarchy(
            &playlist_repo,
            room.id.clone(),
            "Optimistic Playlist",
        ).await;

        let media = Media::from_provider(
            playlist.id.clone(),
            room.id.clone(),
            Some(owner.id.clone()),
            "Original".to_string(),
            serde_json::json!({}),
            "direct_url",
            "default".to_string(),
            0,
        );
        let created = media_repo.create(&media).await.unwrap();

        // Update with correct old values
        let mut updated = created.clone();
        updated.name = "Updated".to_string();
        updated.position = 5;

        let result = media_repo
            .update_if_unchanged(&updated, "Original", 0)
            .await
            .unwrap();
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.name, "Updated");
        assert_eq!(result.position, 5);

        // Try update with stale old values (should return None)
        let mut stale = created.clone();
        stale.name = "Stale Update".to_string();

        let result = media_repo
            .update_if_unchanged(&stale, "Original", 0) // Old name and position
            .await
            .unwrap();
        assert!(result.is_none()); // Conflict detected
    }

    /// Integration test: Swap positions
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_swap_positions() {
        use crate::test_helpers::{RoomFixture, UserFixture, PlaylistFixture};
        use crate::repository::user::UserRepository;
        use crate::repository::room::RoomRepository;
        use crate::repository::playlist::PlaylistRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());
        let media_repo = MediaRepository::new(infra.pool.clone());

        // Setup
        let owner = UserFixture::new().with_username("swap_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Swap Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_media_playlist_hierarchy(
            &playlist_repo,
            room.id.clone(),
            "Swap Playlist",
        ).await;

        // Create two media items
        let media1 = Media::from_provider(
            playlist.id.clone(),
            room.id.clone(),
            Some(owner.id.clone()),
            "Video 1".to_string(),
            serde_json::json!({}),
            "direct_url",
            "default".to_string(),
            0,
        );
        let media2 = Media::from_provider(
            playlist.id.clone(),
            room.id.clone(),
            Some(owner.id.clone()),
            "Video 2".to_string(),
            serde_json::json!({}),
            "direct_url",
            "default".to_string(),
            1,
        );

        let created1 = media_repo.create(&media1).await.unwrap();
        let created2 = media_repo.create(&media2).await.unwrap();

        assert_eq!(created1.position, 0);
        assert_eq!(created2.position, 1);

        // Swap positions
        media_repo
            .swap_positions(&created1.id, &created2.id)
            .await
            .unwrap();

        // Verify swap
        let fetched1 = media_repo.get_by_id(&created1.id).await.unwrap().unwrap();
        let fetched2 = media_repo.get_by_id(&created2.id).await.unwrap().unwrap();

        assert_eq!(fetched1.position, 1);
        assert_eq!(fetched2.position, 0);
    }

    /// Integration test: Count by playlist
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_count_by_playlist() {
        use crate::test_helpers::{RoomFixture, UserFixture, PlaylistFixture};
        use crate::repository::user::UserRepository;
        use crate::repository::room::RoomRepository;
        use crate::repository::playlist::PlaylistRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());
        let media_repo = MediaRepository::new(infra.pool.clone());

        // Setup
        let owner = UserFixture::new().with_username("count_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Count Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_media_playlist_hierarchy(
            &playlist_repo,
            room.id.clone(),
            "Count Playlist",
        ).await;

        // Initially empty
        let count = media_repo.count_by_playlist(&playlist.id).await.unwrap();
        assert_eq!(count, 0);

        // Add 3 items
        for i in 0..3 {
            let media = Media::from_provider(
                playlist.id.clone(),
                room.id.clone(),
                Some(owner.id.clone()),
                format!("Video {}", i),
                serde_json::json!({}),
                "direct_url",
                "default".to_string(),
                i,
            );
            media_repo.create(&media).await.unwrap();
        }

        let count = media_repo.count_by_playlist(&playlist.id).await.unwrap();
        assert_eq!(count, 3);
    }

    /// Integration test: Get playlist paginated
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_playlist_paginated() {
        use crate::test_helpers::{RoomFixture, UserFixture, PlaylistFixture};
        use crate::repository::user::UserRepository;
        use crate::repository::room::RoomRepository;
        use crate::repository::playlist::PlaylistRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());
        let media_repo = MediaRepository::new(infra.pool.clone());

        // Setup
        let owner = UserFixture::new().with_username("paginate_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Paginate Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_media_playlist_hierarchy(
            &playlist_repo,
            room.id.clone(),
            "Paginate Playlist",
        ).await;

        // Create 15 items
        for i in 0..15 {
            let media = Media::from_provider(
                playlist.id.clone(),
                room.id.clone(),
                Some(owner.id.clone()),
                format!("Video {}", i),
                serde_json::json!({}),
                "direct_url",
                "default".to_string(),
                i,
            );
            media_repo.create(&media).await.unwrap();
        }

        // Page 1 (limit 10, offset 0)
        let page1 = PageParams::new(Some(1), Some(10));
        let (items, total) = media_repo
            .get_playlist_paginated(&playlist.id, page1)
            .await
            .unwrap();
        assert_eq!(items.len(), 10);
        assert_eq!(total, 15);

        // Page 2 (limit 10, offset 10)
        let page2 = PageParams::new(Some(2), Some(10));
        let (items, total) = media_repo
            .get_playlist_paginated(&playlist.id, page2)
            .await
            .unwrap();
        assert_eq!(items.len(), 5);
        assert_eq!(total, 15);
    }

    /// Integration test: Delete batch
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_delete_batch() {
        use crate::test_helpers::{RoomFixture, UserFixture, PlaylistFixture};
        use crate::repository::user::UserRepository;
        use crate::repository::room::RoomRepository;
        use crate::repository::playlist::PlaylistRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());
        let media_repo = MediaRepository::new(infra.pool.clone());

        // Setup
        let owner = UserFixture::new().with_username("batch_delete_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Batch Delete Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_media_playlist_hierarchy(
            &playlist_repo,
            room.id.clone(),
            "Batch Delete Playlist",
        ).await;

        // Create 5 items
        let mut ids: Vec<MediaId> = Vec::new();
        for i in 0..5 {
            let media = Media::from_provider(
                playlist.id.clone(),
                room.id.clone(),
                Some(owner.id.clone()),
                format!("Video {}", i),
                serde_json::json!({}),
                "direct_url",
                "default".to_string(),
                i,
            );
            let created = media_repo.create(&media).await.unwrap();
            ids.push(created.id);
        }

        // Delete 3 items
        let deleted = media_repo.delete_batch(&ids[0..3]).await.unwrap();
        assert_eq!(deleted, 3);

        // Verify remaining
        let remaining = media_repo.get_by_playlist(&playlist.id).await.unwrap();
        assert_eq!(remaining.len(), 2);
    }

    /// Integration test: Get by IDs
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_by_ids() {
        use crate::test_helpers::{RoomFixture, UserFixture, PlaylistFixture};
        use crate::repository::user::UserRepository;
        use crate::repository::room::RoomRepository;
        use crate::repository::playlist::PlaylistRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());
        let media_repo = MediaRepository::new(infra.pool.clone());

        // Setup
        let owner = UserFixture::new().with_username("get_ids_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Get IDs Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_media_playlist_hierarchy(
            &playlist_repo,
            room.id.clone(),
            "Get IDs Playlist",
        ).await;

        // Create 3 items
        let mut ids: Vec<MediaId> = Vec::new();
        for i in 0..3 {
            let media = Media::from_provider(
                playlist.id.clone(),
                room.id.clone(),
                Some(owner.id.clone()),
                format!("Video {}", i),
                serde_json::json!({}),
                "direct_url",
                "default".to_string(),
                i,
            );
            let created = media_repo.create(&media).await.unwrap();
            ids.push(created.id);
        }

        // Get by IDs
        let fetched = media_repo.get_by_ids(&ids).await.unwrap();
        assert_eq!(fetched.len(), 3);

        // Get with non-existent ID
        let mut mixed_ids = ids.clone();
        mixed_ids.push(MediaId::new());
        let fetched = media_repo.get_by_ids(&mixed_ids).await.unwrap();
        assert_eq!(fetched.len(), 3); // Only existing ones returned

        // Empty IDs returns empty
        let fetched = media_repo.get_by_ids(&[]).await.unwrap();
        assert!(fetched.is_empty());
    }
}

