//! Media repository for database operations
//!
//! Design reference: /Volumes/workspace/rust/design/04-数据库设计.md §2.4.2

use super::query_builder::escape_ilike;
use sqlx::{FromRow, PgPool, Row};
use std::collections::HashMap;

use crate::{
    models::{Media, MediaId, MediaListQuery, PageParams, PlaylistId, RoomId, UserStatus},
    Result,
};

#[derive(Debug, Clone)]
pub struct MediaListItem {
    pub media: Media,
    pub is_available: bool,
}

/// Media repository for database operations
#[derive(Clone)]
pub struct MediaRepository {
    pool: PgPool,
}

impl MediaRepository {
    const ORDER_STEP: f64 = 1024.0;
    const MIN_ORDER_GAP: f64 = 1e-9;

    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Get a reference to the connection pool
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    fn scope_lock_key(room_id: &RoomId, playlist_id: Option<&PlaylistId>) -> i64 {
        super::stable_scope_lock_key(
            room_id.as_str(),
            playlist_id.map(crate::models::PlaylistId::as_str),
        )
    }

    fn build_media_list_order_by(query: &MediaListQuery) -> String {
        let direction = query.sort_direction.as_sql();
        match query.sort_by {
            crate::models::MediaListSortBy::Name => {
                format!("m.name {direction}, m.position {direction}, m.id {direction}")
            }
            crate::models::MediaListSortBy::AddedAt => {
                format!("m.added_at {direction}, m.position {direction}, m.id {direction}")
            }
            crate::models::MediaListSortBy::UpdatedAt => {
                format!("m.updated_at {direction}, m.position {direction}, m.id {direction}")
            }
            crate::models::MediaListSortBy::SourceProvider => {
                format!("m.source_provider {direction}, m.name {direction}, m.id {direction}")
            }
            crate::models::MediaListSortBy::ProviderInstanceName => format!(
                "m.provider_instance_name {direction}, m.name {direction}, m.id {direction}"
            ),
            crate::models::MediaListSortBy::Position => {
                format!("m.position {direction}, m.name {direction}, m.id {direction}")
            }
        }
    }

    fn normalize_provider_instance_name_for_db(provider_instance_name: &str) -> Option<&str> {
        let trimmed = provider_instance_name.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    }

    fn push_media_scope_filters(
        builder: &mut sqlx::QueryBuilder<'_, sqlx::Postgres>,
        room_id: &RoomId,
        playlist_id: Option<&PlaylistId>,
        query: &MediaListQuery,
    ) {
        const ACTIVE_STATUS_SQL: i16 = UserStatus::Active as i16;

        builder.push(" FROM media m LEFT JOIN users u ON m.creator_id = u.id AND u.deleted_at IS NULL WHERE m.room_id = ");
        builder.push_bind(room_id.as_str().to_owned());
        match playlist_id {
            Some(playlist_id) => {
                builder.push(" AND m.playlist_id = ");
                builder.push_bind(playlist_id.as_str().to_owned());
            }
            None => {
                builder.push(" AND m.playlist_id IS NULL");
            }
        }

        if let Some(search) = &query.search {
            let pattern = escape_ilike(search);
            builder.push(" AND m.name ILIKE ");
            builder.push_bind(pattern);
            builder.push(" ESCAPE '\\'");
        }
        if let Some(source_provider) = &query.source_provider {
            builder.push(" AND m.source_provider = ");
            builder.push_bind(source_provider.clone());
        }
        if let Some(provider_instance_name) = &query.provider_instance_name {
            let trimmed = provider_instance_name.trim();
            if trimmed.is_empty() {
                builder.push(" AND NULLIF(m.provider_instance_name, '') IS NULL");
            } else {
                builder.push(" AND m.provider_instance_name = ");
                builder.push_bind(trimmed.to_owned());
            }
        }
        match query.availability {
            Some(true) => {
                builder.push(" AND (m.creator_id IS NULL OR (u.id IS NOT NULL AND u.status = ");
                builder.push_bind(ACTIVE_STATUS_SQL);
                builder.push("))");
            }
            Some(false) => {
                builder.push(" AND m.creator_id IS NOT NULL AND (u.id IS NULL OR u.status <> ");
                builder.push_bind(ACTIVE_STATUS_SQL);
                builder.push(")");
            }
            None => {}
        }
    }

    pub async fn count_filtered_by_scope(
        &self,
        room_id: &RoomId,
        playlist_id: Option<&PlaylistId>,
        query: &MediaListQuery,
    ) -> Result<i64> {
        let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new("SELECT COUNT(*)");
        Self::push_media_scope_filters(&mut builder, room_id, playlist_id, query);
        builder
            .build_query_scalar()
            .fetch_one(&self.pool)
            .await
            .map_err(Into::into)
    }

    pub async fn list_filtered_by_scope(
        &self,
        room_id: &RoomId,
        playlist_id: Option<&PlaylistId>,
        query: &MediaListQuery,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<MediaListItem>> {
        const ACTIVE_STATUS_SQL: i16 = UserStatus::Active as i16;

        let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new(
            "SELECT m.id, m.playlist_id, m.room_id, m.creator_id, m.name, m.position,
                    m.source_provider, m.source_config, COALESCE(m.provider_instance_name, '') AS provider_instance_name,
                    m.added_at, m.updated_at, m.version,
                    CASE
                      WHEN m.creator_id IS NULL THEN TRUE
                      WHEN u.id IS NOT NULL AND u.status = ",
        );
        builder.push_bind(ACTIVE_STATUS_SQL);
        builder.push(
            " THEN TRUE
                      ELSE FALSE
                    END AS is_available",
        );
        Self::push_media_scope_filters(&mut builder, room_id, playlist_id, query);
        let order_by = Self::build_media_list_order_by(query);
        builder.push(format!(" ORDER BY {order_by} LIMIT "));
        builder.push_bind(limit);
        builder.push(" OFFSET ");
        builder.push_bind(offset);

        let rows = builder.build().fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                Ok(MediaListItem {
                    media: Media::from_row(&row)?,
                    is_available: row.try_get("is_available")?,
                })
            })
            .collect()
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
                              source_provider, source_config, provider_instance_name, added_at, updated_at, version)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10, 0)
             RETURNING id, playlist_id, room_id, creator_id, name, position,
                       source_provider, source_config, COALESCE(provider_instance_name, '') AS provider_instance_name,
                       added_at, updated_at, version
            "
        )
        .bind(media.id.as_str())
        .bind(media.playlist_id.as_ref().map(PlaylistId::as_str))
        .bind(media.room_id.as_str())
        .bind(media.creator_id.as_ref().map(super::super::models::id::UserId::as_str))
        .bind(&media.name)
        .bind(media.position)
        .bind(media.source_provider.as_str())
        .bind(&source_config_json)
        .bind(Self::normalize_provider_instance_name_for_db(
            &media.provider_instance_name,
        ))
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
    pub async fn create_batch_with_executor<'e, E>(
        &self,
        items: &[Media],
        executor: E,
    ) -> Result<Vec<Media>>
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
                               source_provider, source_config, provider_instance_name, added_at, updated_at, version)
             VALUES "
        );
        let mut binds = Vec::new();
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                query_builder.push_str(", ");
            }
            let base = i * 10;
            query_builder.push_str(&format!(
                "(${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, ${}, 0)",
                base + 1,
                base + 2,
                base + 3,
                base + 4,
                base + 5,
                base + 6,
                base + 7,
                base + 8,
                base + 9,
                base + 10,
                base + 10
            ));
            binds.push(serde_json::to_value(&item.source_config)?);
        }
        query_builder.push_str(
            " RETURNING id, playlist_id, room_id, creator_id, name, position,
                       source_provider, source_config, COALESCE(provider_instance_name, '') AS provider_instance_name,
                       added_at, updated_at, version",
        );

        let mut query = sqlx::query(&query_builder);
        for (i, item) in items.iter().enumerate() {
            query = query
                .bind(item.id.as_str())
                .bind(item.playlist_id.as_ref().map(PlaylistId::as_str))
                .bind(item.room_id.as_str())
                .bind(
                    item.creator_id
                        .as_ref()
                        .map(super::super::models::id::UserId::as_str),
                )
                .bind(&item.name)
                .bind(item.position)
                .bind(item.source_provider.as_str())
                .bind(&binds[i])
                .bind(Self::normalize_provider_instance_name_for_db(
                    &item.provider_instance_name,
                ))
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
                       source_provider, source_config, COALESCE(provider_instance_name, '') AS provider_instance_name,
                       added_at, updated_at, version
            "
        )
        .bind(media.id.as_str())
        .bind(&media.name)
        .bind(media.position)
        .bind(&source_config_json)
        .bind(Self::normalize_provider_instance_name_for_db(
            &media.provider_instance_name,
        ))
        .fetch_one(&self.pool)
        .await?;

        Ok(Media::from_row(&row)?)
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
                       source_provider, source_config, COALESCE(provider_instance_name, '') AS provider_instance_name,
                       added_at, updated_at, version
            ",
        )
        .bind(media.id.as_str())
        .bind(&media.name)
        .bind(media.position)
        .bind(&source_config_json)
        .bind(Self::normalize_provider_instance_name_for_db(
            &media.provider_instance_name,
        ))
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
                   source_provider, source_config, COALESCE(provider_instance_name, '') AS provider_instance_name,
                   added_at, updated_at, version
             FROM media
             WHERE id = $1            ",
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
    pub async fn get_by_ids_with_executor<'e, E>(
        &self,
        media_ids: &[MediaId],
        executor: E,
    ) -> Result<Vec<Media>>
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
                   source_provider, source_config, COALESCE(provider_instance_name, '') AS provider_instance_name,
                   added_at, updated_at, version
             FROM media
             WHERE id = ANY($1)            ",
        )
        .bind(&id_strs)
        .fetch_all(executor)
        .await?;

        rows.into_iter()
            .map(|row| Ok(Media::from_row(&row)?))
            .collect()
    }

    /// Get all media inside a scope ordered by position.
    pub async fn get_scope_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        playlist_id: Option<&PlaylistId>,
        executor: E,
    ) -> Result<Vec<Media>>
    where
        E: sqlx::Executor<'e, Database = sqlx::Postgres>,
    {
        let rows = sqlx::query(
            r"
            SELECT id, playlist_id, room_id, creator_id, name, position,
                   source_provider, source_config, COALESCE(provider_instance_name, '') AS provider_instance_name,
                   added_at, updated_at, version
            FROM media
            WHERE room_id = $1
              AND playlist_id IS NOT DISTINCT FROM $2
            ORDER BY position ASC, id ASC
            ",
        )
        .bind(room_id.as_str())
        .bind(playlist_id.map(PlaylistId::as_str))
        .fetch_all(executor)
        .await?;

        rows.into_iter()
            .map(|row| Ok(Media::from_row(&row)?))
            .collect()
    }

    /// Get media directly under the room root.
    pub async fn get_room_root(&self, room_id: &RoomId) -> Result<Vec<Media>> {
        let rows = sqlx::query(
            r"
            SELECT id, playlist_id, room_id, creator_id, name, position,
                   source_provider, source_config, COALESCE(provider_instance_name, '') AS provider_instance_name,
                   added_at, updated_at, version
             FROM media
             WHERE room_id = $1
               AND playlist_id IS NULL
             ORDER BY position ASC
            ",
        )
        .bind(room_id.as_str())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| Ok(Media::from_row(&row)?))
            .collect()
    }

    /// Get media in a specific playlist.
    pub async fn get_by_playlist(&self, playlist_id: &PlaylistId) -> Result<Vec<Media>> {
        let rows = sqlx::query(
            r"
            SELECT id, playlist_id, room_id, creator_id, name, position,
                   source_provider, source_config, COALESCE(provider_instance_name, '') AS provider_instance_name,
                   added_at, updated_at, version
             FROM media
             WHERE playlist_id = $1
             ORDER BY position ASC
            ",
        )
        .bind(playlist_id.as_str())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| Ok(Media::from_row(&row)?))
            .collect()
    }

    /// Get paginated media for a specific playlist.
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
            SELECT COUNT(*) FROM media WHERE playlist_id = $1",
        )
        .bind(playlist_id.as_str())
        .fetch_one(&self.pool)
        .await?;

        // Get paginated results
        let rows = sqlx::query(
            r"
            SELECT id, playlist_id, room_id, creator_id, name, position,
                   source_provider, source_config, COALESCE(provider_instance_name, '') AS provider_instance_name,
                   added_at, updated_at, version
             FROM media
             WHERE playlist_id = $1
             ORDER BY position ASC
             LIMIT $2 OFFSET $3
            ",
        )
        .bind(playlist_id.as_str())
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        let items: Vec<Media> = rows
            .into_iter()
            .map(|row| Ok(Media::from_row(&row)?))
            .collect::<Result<Vec<Media>>>()?;

        Ok((items, total))
    }

    /// Get paginated media directly under the room root.
    pub async fn get_room_root_paginated(
        &self,
        room_id: &RoomId,
        pagination: PageParams,
    ) -> Result<(Vec<Media>, i64)> {
        let limit = pagination.limit() as i64;
        let offset = pagination.offset() as i64;

        let total: i64 = sqlx::query_scalar(
            r"
            SELECT COUNT(*)
            FROM media
            WHERE room_id = $1
              AND playlist_id IS NULL
            ",
        )
        .bind(room_id.as_str())
        .fetch_one(&self.pool)
        .await?;

        let rows = sqlx::query(
            r"
            SELECT id, playlist_id, room_id, creator_id, name, position,
                   source_provider, source_config, COALESCE(provider_instance_name, '') AS provider_instance_name,
                   added_at, updated_at, version
             FROM media
             WHERE room_id = $1
               AND playlist_id IS NULL
             ORDER BY position ASC
             LIMIT $2 OFFSET $3
            ",
        )
        .bind(room_id.as_str())
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        let items: Vec<Media> = rows
            .into_iter()
            .map(|row| Ok(Media::from_row(&row)?))
            .collect::<Result<Vec<Media>>>()?;

        Ok((items, total))
    }

    /// Get media items from a playlist with limit and offset (no count query).
    ///
    /// This is a simpler version of `get_playlist_paginated` that doesn't return
    /// the total count, useful when you only need the items.
    pub async fn get_by_playlist_limit_offset(
        &self,
        playlist_id: &PlaylistId,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Media>> {
        let rows = sqlx::query(
            r"
            SELECT id, playlist_id, room_id, creator_id, name, position,
                   source_provider, source_config, COALESCE(provider_instance_name, '') AS provider_instance_name,
                   added_at, updated_at, version
             FROM media
             WHERE playlist_id = $1
             ORDER BY position ASC
             LIMIT $2 OFFSET $3
            ",
        )
        .bind(playlist_id.as_str())
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| Ok(Media::from_row(&row)?))
            .collect()
    }

    /// Get room-root media items with limit and offset (no count query).
    pub async fn get_room_root_limit_offset(
        &self,
        room_id: &RoomId,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Media>> {
        let rows = sqlx::query(
            r"
            SELECT id, playlist_id, room_id, creator_id, name, position,
                   source_provider, source_config, COALESCE(provider_instance_name, '') AS provider_instance_name,
                   added_at, updated_at, version
             FROM media
             WHERE room_id = $1
               AND playlist_id IS NULL
             ORDER BY position ASC
             LIMIT $2 OFFSET $3
            ",
        )
        .bind(room_id.as_str())
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| Ok(Media::from_row(&row)?))
            .collect()
    }

    /// Delete media from playlist
    pub async fn delete(&self, media_id: &MediaId) -> Result<bool> {
        let result = sqlx::query(
            r"
            DELETE FROM media
             WHERE id = $1
            ",
        )
        .bind(media_id.as_str())
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Delete all media in a playlist.
    pub async fn delete_playlist(&self, playlist_id: &PlaylistId) -> Result<usize> {
        let result = sqlx::query(
            r"
            DELETE FROM media
             WHERE playlist_id = $1
            ",
        )
        .bind(playlist_id.as_str())
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() as usize)
    }

    /// Delete all media directly under the room root.
    pub async fn delete_room_root(&self, room_id: &RoomId) -> Result<usize> {
        let result = sqlx::query(
            r"
            DELETE FROM media
             WHERE room_id = $1
               AND playlist_id IS NULL
            ",
        )
        .bind(room_id.as_str())
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() as usize)
    }

    /// Bulk delete media items by IDs
    pub async fn delete_batch(&self, media_ids: &[MediaId]) -> Result<usize> {
        self.delete_batch_with_executor(media_ids, &self.pool).await
    }

    /// Bulk delete media items by IDs using a specific executor (for transaction support)
    pub async fn delete_batch_with_executor<'e, E>(
        &self,
        media_ids: &[MediaId],
        executor: E,
    ) -> Result<usize>
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
            ",
        )
        .bind(&id_strs)
        .execute(executor)
        .await?;

        Ok(result.rows_affected() as usize)
    }

    async fn lock_scope_with_tx(
        &self,
        room_id: &RoomId,
        playlist_id: Option<&PlaylistId>,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<()> {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(Self::scope_lock_key(room_id, playlist_id))
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    async fn lock_scopes_with_tx(
        &self,
        scopes: &[(RoomId, Option<PlaylistId>)],
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<()> {
        let mut unique_scopes: Vec<(RoomId, Option<PlaylistId>)> = Vec::new();
        for (room_id, playlist_id) in scopes {
            if unique_scopes.iter().any(|(seen_room, seen_playlist)| {
                seen_room == room_id && seen_playlist == playlist_id
            }) {
                continue;
            }
            unique_scopes.push((room_id.clone(), playlist_id.clone()));
        }

        unique_scopes.sort_by(|(left_room, left_playlist), (right_room, right_playlist)| {
            left_room.as_str().cmp(right_room.as_str()).then_with(|| {
                left_playlist
                    .as_ref()
                    .map_or("", PlaylistId::as_str)
                    .cmp(
                        right_playlist
                            .as_ref()
                            .map_or("", PlaylistId::as_str),
                    )
            })
        });

        for (room_id, playlist_id) in unique_scopes {
            self.lock_scope_with_tx(&room_id, playlist_id.as_ref(), tx)
                .await?;
        }

        Ok(())
    }

    fn allocate_positions(
        previous: Option<f64>,
        next: Option<f64>,
        count: usize,
    ) -> Option<Vec<f64>> {
        if count == 0 {
            return Some(Vec::new());
        }

        match (previous, next) {
            (Some(previous), Some(next)) => {
                let gap = next - previous;
                if !gap.is_finite() || gap <= Self::MIN_ORDER_GAP {
                    return None;
                }
                let step = gap / ((count + 1) as f64);
                if !step.is_finite() || step <= Self::MIN_ORDER_GAP {
                    return None;
                }
                let mut positions = Vec::with_capacity(count);
                for index in 1..=count {
                    let position = previous + step * (index as f64);
                    if !position.is_finite() || position <= previous || position >= next {
                        return None;
                    }
                    positions.push(position);
                }
                Some(positions)
            }
            (None, Some(next)) => {
                if !next.is_finite() {
                    return None;
                }
                let start = Self::ORDER_STEP.mul_add(-(count as f64), next);
                if !start.is_finite() {
                    return None;
                }
                let mut positions = Vec::with_capacity(count);
                for index in 0..count {
                    let position = Self::ORDER_STEP.mul_add(index as f64, start);
                    if !position.is_finite() || position >= next {
                        return None;
                    }
                    positions.push(position);
                }
                Some(positions)
            }
            (Some(previous), None) => {
                if !previous.is_finite() {
                    return None;
                }
                let mut positions = Vec::with_capacity(count);
                for index in 1..=count {
                    let position = Self::ORDER_STEP.mul_add(index as f64, previous);
                    if !position.is_finite() || position <= previous {
                        return None;
                    }
                    positions.push(position);
                }
                Some(positions)
            }
            (None, None) => {
                let mut positions = Vec::with_capacity(count);
                for index in 1..=count {
                    positions.push(Self::ORDER_STEP * (index as f64));
                }
                Some(positions)
            }
        }
    }

    pub async fn move_batch_to_scope_with_tx(
        &self,
        room_id: &RoomId,
        media_ids: &[MediaId],
        target_playlist_id: Option<&PlaylistId>,
        before_media_id: Option<&MediaId>,
        after_media_id: Option<&MediaId>,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<Vec<Media>> {
        if media_ids.is_empty() {
            return Ok(Vec::new());
        }

        if before_media_id.is_some() && after_media_id.is_some() {
            return Err(crate::Error::InvalidInput(
                "At most one of before_media_id or after_media_id may be set".to_string(),
            ));
        }

        let media_id_strs: Vec<&str> = media_ids.iter().map(MediaId::as_str).collect();
        let moved_rows = sqlx::query(
            r"
            SELECT id, playlist_id, room_id, creator_id, name, position,
                   source_provider, source_config, COALESCE(provider_instance_name, '') AS provider_instance_name,
                   added_at, updated_at, version
            FROM media
            WHERE id = ANY($1)
            FOR UPDATE
            ",
        )
        .bind(&media_id_strs)
        .fetch_all(&mut **tx)
        .await?;

        if moved_rows.len() != media_ids.len() {
            return Err(crate::Error::NotFound("Media not found".to_string()));
        }

        let mut moved_map = HashMap::with_capacity(moved_rows.len());
        for row in moved_rows {
            let media = Media::from_row(&row)?;
            if media.room_id != *room_id {
                return Err(crate::Error::Authorization(
                    "Media does not belong to this room".to_string(),
                ));
            }
            moved_map.insert(media.id.clone(), media);
        }

        let moved_media: Vec<Media> = media_ids
            .iter()
            .map(|media_id| {
                moved_map
                    .remove(media_id)
                    .ok_or_else(|| crate::Error::NotFound("Media not found".to_string()))
            })
            .collect::<Result<Vec<_>>>()?;

        let anchor_id = match (before_media_id, after_media_id) {
            (Some(anchor_id), None) | (None, Some(anchor_id)) => Some(anchor_id),
            (None, None) => None,
            _ => {
                return Err(crate::Error::InvalidInput(
                    "At most one of before_media_id or after_media_id may be set".to_string(),
                ))
            }
        };

        if let Some(anchor_id) = anchor_id {
            if media_ids.iter().any(|media_id| media_id == anchor_id) {
                return Err(crate::Error::InvalidInput(
                    "Cannot move media relative to itself".to_string(),
                ));
            }
        }

        let anchor_media = if let Some(anchor_id) = anchor_id {
            let anchor_media = sqlx::query(
                r"
                SELECT id, playlist_id, room_id, creator_id, name, position,
                       source_provider, source_config, COALESCE(provider_instance_name, '') AS provider_instance_name,
                       added_at, updated_at, version
                FROM media
                WHERE id = $1
                FOR UPDATE
                ",
            )
            .bind(anchor_id.as_str())
            .fetch_optional(&mut **tx)
            .await?
            .map(|row| Media::from_row(&row))
            .transpose()?
            .ok_or_else(|| crate::Error::NotFound("Anchor media not found".to_string()))?;

            if anchor_media.room_id != *room_id {
                return Err(crate::Error::Authorization(
                    "Anchor media does not belong to this room".to_string(),
                ));
            }
            Some(anchor_media)
        } else {
            None
        };

        let effective_target_playlist_id = match (target_playlist_id, anchor_media.as_ref()) {
            (Some(target_playlist_id), Some(anchor_media)) => {
                if anchor_media.playlist_id.as_ref() != Some(target_playlist_id) {
                    return Err(crate::Error::InvalidInput(
                        "Anchor media must belong to the target playlist scope".to_string(),
                    ));
                }
                Some(target_playlist_id.clone())
            }
            (None, Some(anchor_media)) => anchor_media.playlist_id.clone(),
            (Some(target_playlist_id), None) => Some(target_playlist_id.clone()),
            (None, None) => {
                let first_scope = moved_media[0].playlist_id.clone();
                if moved_media
                    .iter()
                    .any(|media| media.playlist_id != first_scope)
                {
                    return Err(crate::Error::InvalidInput(
                        "Moving media from multiple scopes requires target_playlist_id or an anchor"
                            .to_string(),
                    ));
                }
                first_scope
            }
        };

        let mut affected_scopes: Vec<(RoomId, Option<PlaylistId>)> = moved_media
            .iter()
            .map(|media| (media.room_id.clone(), media.playlist_id.clone()))
            .collect();
        affected_scopes.push((room_id.clone(), effective_target_playlist_id.clone()));
        self.lock_scopes_with_tx(&affected_scopes, tx).await?;

        for _ in 0..2 {
            let target_rows = sqlx::query(
                r"
                SELECT id, position
                FROM media
                WHERE room_id = $1
                  AND playlist_id IS NOT DISTINCT FROM $2
                  AND NOT (id = ANY($3))
                ORDER BY position ASC, id ASC
                FOR UPDATE
                ",
            )
            .bind(room_id.as_str())
            .bind(
                effective_target_playlist_id
                    .as_ref()
                    .map(PlaylistId::as_str),
            )
            .bind(&media_id_strs)
            .fetch_all(&mut **tx)
            .await?;

            let target_rows: Vec<(MediaId, f64)> = target_rows
                .into_iter()
                .map(|row| {
                    Ok((
                        MediaId::from_string(row.try_get::<String, _>("id")?),
                        row.try_get::<f64, _>("position")?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?;

            let insertion_index = if let Some(anchor_media) = anchor_media.as_ref() {
                let anchor_index = target_rows
                    .iter()
                    .position(|(media_id, _)| media_id == &anchor_media.id)
                    .ok_or_else(|| {
                        crate::Error::InvalidInput(
                            "Anchor media must remain in the target playlist scope".to_string(),
                        )
                    })?;
                if before_media_id.is_some() {
                    anchor_index
                } else {
                    anchor_index + 1
                }
            } else {
                target_rows.len()
            };

            let previous = insertion_index
                .checked_sub(1)
                .map(|index| target_rows[index].1);
            let next = target_rows
                .get(insertion_index)
                .map(|(_, position)| *position);
            let Some(positions) = Self::allocate_positions(previous, next, moved_media.len())
            else {
                self.rebalance_scope_with_tx(room_id, effective_target_playlist_id.as_ref(), tx)
                    .await?;
                continue;
            };

            let mut updated_media = Vec::with_capacity(moved_media.len());
            for (media, position) in moved_media.iter().zip(positions) {
                let row = sqlx::query(
                    r"
                    UPDATE media
                    SET playlist_id = $2,
                        position = $3,
                        version = version + 1
                    WHERE id = $1
                    RETURNING id, playlist_id, room_id, creator_id, name, position,
                              source_provider, source_config, COALESCE(provider_instance_name, '') AS provider_instance_name,
                              added_at, updated_at, version
                    ",
                )
                .bind(media.id.as_str())
                .bind(
                    effective_target_playlist_id
                        .as_ref()
                        .map(PlaylistId::as_str),
                )
                .bind(position)
                .fetch_one(&mut **tx)
                .await?;
                updated_media.push(Media::from_row(&row)?);
            }

            return Ok(updated_media);
        }

        Err(crate::Error::Internal(
            "Failed to compute stable positions for moved media".to_string(),
        ))
    }

    async fn get_scope_previous_position_with_tx(
        &self,
        room_id: &RoomId,
        playlist_id: Option<&PlaylistId>,
        exclude_media_id: &MediaId,
        anchor_position: f64,
        anchor_media_id: &MediaId,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<Option<f64>> {
        sqlx::query_scalar(
            r"
            SELECT position
            FROM media
            WHERE room_id = $1
              AND playlist_id IS NOT DISTINCT FROM $2
              AND id <> $3
              AND (
                    position < $4
                 OR (position = $4 AND id < $5)
              )
            ORDER BY position DESC, id DESC
            LIMIT 1
            ",
        )
        .bind(room_id.as_str())
        .bind(playlist_id.map(PlaylistId::as_str))
        .bind(exclude_media_id.as_str())
        .bind(anchor_position)
        .bind(anchor_media_id.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(Into::into)
    }

    async fn get_scope_next_position_with_tx(
        &self,
        room_id: &RoomId,
        playlist_id: Option<&PlaylistId>,
        exclude_media_id: &MediaId,
        anchor_position: f64,
        anchor_media_id: &MediaId,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<Option<f64>> {
        sqlx::query_scalar(
            r"
            SELECT position
            FROM media
            WHERE room_id = $1
              AND playlist_id IS NOT DISTINCT FROM $2
              AND id <> $3
              AND (
                    position > $4
                 OR (position = $4 AND id > $5)
              )
            ORDER BY position ASC, id ASC
            LIMIT 1
            ",
        )
        .bind(room_id.as_str())
        .bind(playlist_id.map(PlaylistId::as_str))
        .bind(exclude_media_id.as_str())
        .bind(anchor_position)
        .bind(anchor_media_id.as_str())
        .fetch_optional(&mut **tx)
        .await
        .map_err(Into::into)
    }

    async fn rebalance_scope_with_tx(
        &self,
        room_id: &RoomId,
        playlist_id: Option<&PlaylistId>,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<()> {
        let rows = sqlx::query(
            r"
            SELECT id
            FROM media
            WHERE room_id = $1
              AND playlist_id IS NOT DISTINCT FROM $2
            ORDER BY position ASC, id ASC
            FOR UPDATE
            ",
        )
        .bind(room_id.as_str())
        .bind(playlist_id.map(PlaylistId::as_str))
        .fetch_all(&mut **tx)
        .await?;

        for (index, row) in rows.into_iter().enumerate() {
            let media_id: String = row.try_get("id")?;
            let position = Self::ORDER_STEP * ((index + 1) as f64);
            sqlx::query("UPDATE media SET position = $2, version = version + 1 WHERE id = $1")
                .bind(media_id)
                .bind(position)
                .execute(&mut **tx)
                .await?;
        }

        Ok(())
    }

    fn midpoint(previous: f64, next: f64) -> Option<f64> {
        let gap = next - previous;
        if !gap.is_finite() || gap <= Self::MIN_ORDER_GAP {
            return None;
        }
        let midpoint = previous + gap / 2.0;
        if !midpoint.is_finite() || midpoint <= previous || midpoint >= next {
            return None;
        }
        Some(midpoint)
    }

    pub async fn get_next_append_position_with_tx(
        &self,
        room_id: &RoomId,
        playlist_id: Option<&PlaylistId>,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<f64> {
        self.lock_scope_with_tx(room_id, playlist_id, tx).await?;

        let max_pos: Option<f64> = sqlx::query_scalar(
            r"
            SELECT MAX(position)
            FROM media
            WHERE room_id = $1
              AND playlist_id IS NOT DISTINCT FROM $2
            ",
        )
        .bind(room_id.as_str())
        .bind(playlist_id.map(PlaylistId::as_str))
        .fetch_one(&mut **tx)
        .await?;

        match max_pos {
            Some(position) if position.is_finite() => Ok(position + Self::ORDER_STEP),
            _ => Ok(Self::ORDER_STEP),
        }
    }

    pub async fn move_with_tx(
        &self,
        media_id: &MediaId,
        before_media_id: Option<&MediaId>,
        after_media_id: Option<&MediaId>,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<Media> {
        let anchor_id = match (before_media_id, after_media_id) {
            (Some(anchor_id), None) | (None, Some(anchor_id)) => anchor_id,
            _ => {
                return Err(crate::Error::InvalidInput(
                    "Exactly one of before_media_id or after_media_id must be set".to_string(),
                ))
            }
        };

        if media_id == anchor_id {
            return Err(crate::Error::InvalidInput(
                "Cannot move media relative to itself".to_string(),
            ));
        }

        let moved = sqlx::query(
            r"
            SELECT id, playlist_id, room_id, creator_id, name, position,
                   source_provider, source_config, COALESCE(provider_instance_name, '') AS provider_instance_name,
                   added_at, updated_at, version
            FROM media
            WHERE id = $1
            FOR UPDATE
            ",
        )
        .bind(media_id.as_str())
        .fetch_optional(&mut **tx)
        .await?
        .map(|row| Media::from_row(&row))
        .transpose()?
        .ok_or_else(|| crate::Error::NotFound("Media not found".to_string()))?;

        let anchor = sqlx::query(
            r"
            SELECT id, playlist_id, room_id, creator_id, name, position,
                   source_provider, source_config, COALESCE(provider_instance_name, '') AS provider_instance_name,
                   added_at, updated_at, version
            FROM media
            WHERE id = $1
            FOR UPDATE
            ",
        )
        .bind(anchor_id.as_str())
        .fetch_optional(&mut **tx)
        .await?
        .map(|row| Media::from_row(&row))
        .transpose()?
        .ok_or_else(|| crate::Error::NotFound("Anchor media not found".to_string()))?;

        if moved.room_id != anchor.room_id || moved.playlist_id != anchor.playlist_id {
            return Err(crate::Error::InvalidInput(
                "Media can only be moved relative to a sibling in the same playlist scope"
                    .to_string(),
            ));
        }

        self.lock_scope_with_tx(&moved.room_id, moved.playlist_id.as_ref(), tx)
            .await?;

        for _ in 0..2 {
            let anchor_position: f64 =
                sqlx::query_scalar("SELECT position FROM media WHERE id = $1 FOR UPDATE")
                    .bind(anchor.id.as_str())
                    .fetch_one(&mut **tx)
                    .await?;

            let new_position = if before_media_id.is_some() {
                match self
                    .get_scope_previous_position_with_tx(
                        &moved.room_id,
                        moved.playlist_id.as_ref(),
                        &moved.id,
                        anchor_position,
                        &anchor.id,
                        tx,
                    )
                    .await?
                {
                    Some(previous) => Self::midpoint(previous, anchor_position),
                    None => Some(anchor_position - Self::ORDER_STEP),
                }
            } else {
                match self
                    .get_scope_next_position_with_tx(
                        &moved.room_id,
                        moved.playlist_id.as_ref(),
                        &moved.id,
                        anchor_position,
                        &anchor.id,
                        tx,
                    )
                    .await?
                {
                    Some(next) => Self::midpoint(anchor_position, next),
                    None => Some(anchor_position + Self::ORDER_STEP),
                }
            };

            if let Some(position) = new_position.filter(|position| position.is_finite()) {
                let row = sqlx::query(
                    r"
                    UPDATE media
                    SET position = $2, version = version + 1
                    WHERE id = $1
                    RETURNING id, playlist_id, room_id, creator_id, name, position,
                              source_provider, source_config, COALESCE(provider_instance_name, '') AS provider_instance_name,
                              added_at, updated_at, version
                    ",
                )
                .bind(moved.id.as_str())
                .bind(position)
                .fetch_one(&mut **tx)
                .await?;

                return Ok(Media::from_row(&row)?);
            }

            self.rebalance_scope_with_tx(&moved.room_id, moved.playlist_id.as_ref(), tx)
                .await?;
        }

        Err(crate::Error::Internal(
            "Failed to compute a stable media order position".to_string(),
        ))
    }

    /// Count media items in a playlist.
    pub async fn count_by_playlist(&self, playlist_id: &PlaylistId) -> Result<i64> {
        let count: i64 = sqlx::query_scalar(
            r"
            SELECT COUNT(*) FROM media WHERE playlist_id = $1            ",
        )
        .bind(playlist_id.as_str())
        .fetch_one(&self.pool)
        .await?;

        Ok(count)
    }

    /// Count only media whose creator is still active (or media without a creator).
    pub async fn count_by_playlist_accessible(&self, playlist_id: &PlaylistId) -> Result<i64> {
        let count: i64 = sqlx::query_scalar(
            r"
            SELECT COUNT(*)
            FROM media m
            LEFT JOIN users u
              ON m.creator_id = u.id
             AND u.deleted_at IS NULL
            WHERE m.playlist_id = $1
              AND (m.creator_id IS NULL OR u.status = $2)
            ",
        )
        .bind(playlist_id.as_str())
        .bind(UserStatus::Active)
        .fetch_one(&self.pool)
        .await?;

        Ok(count)
    }

    /// Count media items directly under the room root.
    pub async fn count_room_root(&self, room_id: &RoomId) -> Result<i64> {
        let count: i64 = sqlx::query_scalar(
            r"
            SELECT COUNT(*)
            FROM media
            WHERE room_id = $1
              AND playlist_id IS NULL
            ",
        )
        .bind(room_id.as_str())
        .fetch_one(&self.pool)
        .await?;

        Ok(count)
    }

    /// Batch count media items across multiple playlists
    pub async fn count_by_playlists_batch(
        &self,
        playlist_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, i64>> {
        use sqlx::Row;
        let rows = sqlx::query(
            r"
            SELECT playlist_id, COUNT(*) as cnt
            FROM media
            WHERE playlist_id = ANY($1)            GROUP BY playlist_id
            ",
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

    /// Batch count only media whose creator is still active (or media without a creator).
    pub async fn count_by_playlists_batch_accessible(
        &self,
        playlist_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, i64>> {
        use sqlx::Row;

        if playlist_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        let rows = sqlx::query(
            r"
            SELECT m.playlist_id, COUNT(*) as cnt
            FROM media m
            LEFT JOIN users u
              ON m.creator_id = u.id
             AND u.deleted_at IS NULL
            WHERE m.playlist_id = ANY($1)
              AND (m.creator_id IS NULL OR u.status = $2)
            GROUP BY m.playlist_id
            ",
        )
        .bind(playlist_ids)
        .bind(UserStatus::Active)
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
    use sqlx::Execute;
    use synctv_core_testing::create_test_pool;

    /// Unit test: Media builder pattern
    #[test]
    fn test_media_from_provider() {
        let playlist_id = PlaylistId::new();
        let room_id = RoomId::new();
        let creator_id = UserId::new();

        let media = Media::from_provider(
            Some(playlist_id),
            room_id,
            Some(creator_id),
            "Test Video".to_string(),
            serde_json::json!({"url": "https://example.com/video.mp4"}),
            "direct_url",
            "default".to_string(),
            0.0,
        );

        assert_eq!(media.name, "Test Video");
        assert_eq!(media.position, 0.0);
        assert_eq!(media.source_provider, "direct_url");
    }

    #[test]
    fn test_push_media_scope_filters_treats_empty_provider_instance_as_default() {
        let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new("SELECT m.id FROM media m");
        let query = MediaListQuery {
            provider_instance_name: Some("   ".to_string()),
            ..MediaListQuery::default()
        };
        let room_id = RoomId::from_string("room12345678".to_string());

        MediaRepository::push_media_scope_filters(&mut builder, &room_id, None, &query);

        let built = builder.build();
        assert!(built
            .sql()
            .contains("NULLIF(m.provider_instance_name, '') IS NULL"));
    }

    /// Unit test: `Media::from_direct_single_mode`
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
            Some(playlist_id),
            room_id,
            Some(creator_id),
            "Single Mode Video".to_string(),
            "direct",
            playback_info,
            5.0,
        );

        assert_eq!(media.name, "Single Mode Video");
        assert_eq!(media.position, 5.0);
        assert_eq!(media.provider_instance_name, "direct_url");
        assert!(media.source_config.get("playback_infos").is_some());
    }

    /// Unit test: `Media::from_direct_multimode`
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
            Some(playlist_id),
            room_id,
            None,
            "Multimode Video".to_string(),
            playback_infos,
            "direct".to_string(),
            metadata,
            10.0,
        );

        assert_eq!(media.name, "Multimode Video");
        assert_eq!(media.position, 10.0);
        assert_eq!(media.provider_instance_name, "direct_url");
        assert!(media.source_config.get("playback_infos").is_some());
        assert!(media.source_config.get("metadata").is_some());
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
        use crate::repository::playlist::PlaylistRepository;
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());
        let media_repo = MediaRepository::new(pool.clone());

        // Create owner and room
        let owner = UserFixture::new().with_username("media_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Media Test Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
            &playlist_repo,
            room.id.clone(),
            "Test Playlist",
        )
        .await;

        // Create media
        let media = Media::from_provider(
            Some(playlist.id.clone()),
            room.id.clone(),
            Some(owner.id.clone()),
            "Test Video".to_string(),
            serde_json::json!({"url": "https://example.com/video.mp4"}),
            "direct_url",
            "default".to_string(),
            0.0,
        );

        let created = media_repo.create(&media).await.unwrap();
        assert_eq!(created.name, "Test Video");
        assert_eq!(created.position, 0.0);

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
        use crate::repository::playlist::PlaylistRepository;
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());
        let media_repo = MediaRepository::new(pool.clone());

        // Setup
        let owner = UserFixture::new()
            .with_username("media_update_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Media Update Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
            &playlist_repo,
            room.id.clone(),
            "Test Playlist",
        )
        .await;

        let media = Media::from_provider(
            Some(playlist.id.clone()),
            room.id.clone(),
            Some(owner.id.clone()),
            "Original Name".to_string(),
            serde_json::json!({}),
            "direct_url",
            "default".to_string(),
            0.0,
        );
        let created = media_repo.create(&media).await.unwrap();

        // Update
        let mut updated = created.clone();
        updated.name = "Updated Name".to_string();
        updated.position = 5.0;

        let result = media_repo.update(&updated).await.unwrap();
        assert_eq!(result.name, "Updated Name");
        assert_eq!(result.position, 5.0);
    }

    /// Integration test: Delete media
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_delete_media() {
        use crate::repository::playlist::PlaylistRepository;
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());
        let media_repo = MediaRepository::new(pool.clone());

        // Setup
        let owner = UserFixture::new()
            .with_username("media_delete_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Media Delete Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
            &playlist_repo,
            room.id.clone(),
            "Test Playlist",
        )
        .await;

        let media = Media::from_provider(
            Some(playlist.id.clone()),
            room.id.clone(),
            Some(owner.id.clone()),
            "To Delete".to_string(),
            serde_json::json!({}),
            "direct_url",
            "default".to_string(),
            0.0,
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

    /// Integration test: empty/default provider-instance filter must match rows
    /// stored as NULL in the database.
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_list_filtered_by_scope_matches_default_provider_instance_name() {
        use crate::models::{MediaListQuery, MediaListSortBy};
        use crate::repository::playlist::PlaylistRepository;
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());
        let media_repo = MediaRepository::new(pool.clone());

        let owner = user_repo
            .create(
                &UserFixture::new()
                    .with_username("media_default_instance_owner")
                    .build(),
            )
            .await
            .unwrap();

        let room = room_repo
            .create(
                &RoomFixture::new()
                    .with_name("Media Default Instance Room")
                    .with_owner(owner.id.clone())
                    .build(),
            )
            .await
            .unwrap();

        let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
            &playlist_repo,
            room.id.clone(),
            "Default Instance Playlist",
        )
        .await;

        let default_media = Media::from_provider(
            Some(playlist.id.clone()),
            room.id.clone(),
            Some(owner.id.clone()),
            "Default Backend".to_string(),
            serde_json::json!({"url": "https://example.com/default.mp4"}),
            "direct_url",
            String::new(),
            0.0,
        );
        let explicit_media = Media::from_provider(
            Some(playlist.id.clone()),
            room.id.clone(),
            Some(owner.id.clone()),
            "Explicit Backend".to_string(),
            serde_json::json!({"url": "https://example.com/explicit.mp4"}),
            "direct_url",
            "direct_url_remote".to_string(),
            1.0,
        );

        let created_default = media_repo.create(&default_media).await.unwrap();
        media_repo.create(&explicit_media).await.unwrap();

        let query = MediaListQuery {
            provider_instance_name: Some(String::new()),
            sort_by: MediaListSortBy::Position,
            ..MediaListQuery::default()
        };

        let count = media_repo
            .count_filtered_by_scope(&room.id, Some(&playlist.id), &query)
            .await
            .unwrap();
        assert_eq!(count, 1);

        let rows = media_repo
            .list_filtered_by_scope(&room.id, Some(&playlist.id), &query, 50, 0)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].media.id, created_default.id);
        assert!(rows[0].media.provider_instance_name.is_empty());
    }

    /// Integration test: Batch create media
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_batch() {
        use crate::repository::playlist::PlaylistRepository;
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());
        let media_repo = MediaRepository::new(pool.clone());

        // Setup
        let owner = UserFixture::new().with_username("batch_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Batch Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
            &playlist_repo,
            room.id.clone(),
            "Batch Playlist",
        )
        .await;

        // Create batch
        let items: Vec<Media> = (0..5)
            .map(|i| {
                Media::from_provider(
                    Some(playlist.id.clone()),
                    room.id.clone(),
                    Some(owner.id.clone()),
                    format!("Video {i}"),
                    serde_json::json!({"url": format!("https://example.com/{}.mp4", i)}),
                    "direct_url",
                    "default".to_string(),
                    i as f64,
                )
            })
            .collect();

        let created = media_repo.create_batch(&items).await.unwrap();
        assert_eq!(created.len(), 5);

        // Verify all created
        let fetched = media_repo.get_by_playlist(&playlist.id).await.unwrap();
        assert_eq!(fetched.len(), 5);
    }

    /// Unit test: Oversized chunks are rejected before any database I/O.
    #[tokio::test]
    async fn test_create_batch_chunk_too_large() {
        let pool = PgPool::connect_lazy("postgresql://unused:unused@localhost/unused")
            .expect("lazy pool should accept a syntactically valid URL");
        let playlist_id = PlaylistId::new();
        let room_id = RoomId::new();
        let owner_id = UserId::new();
        let items: Vec<Media> = (0..1001)
            .map(|i| {
                Media::from_provider(
                    Some(playlist_id.clone()),
                    room_id.clone(),
                    Some(owner_id.clone()),
                    format!("Video {i}"),
                    serde_json::json!({"url": format!("https://example.com/{}.mp4", i)}),
                    "direct_url",
                    "default".to_string(),
                    i as f64,
                )
            })
            .collect();

        let err = MediaRepository::create_batch_chunk(&items, &pool)
            .await
            .expect_err("oversized chunks should be rejected before touching the database");

        match err {
            crate::Error::InvalidInput(message) => {
                assert!(
                    message.contains("1000 row limit"),
                    "unexpected message: {message}"
                );
                assert!(
                    message.contains("10010 bind parameters"),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected invalid input error, got {other:?}"),
        }
    }

    /// Integration test: Move media within a scope using anchor-based ordering.
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_move_with_tx_reorders_scope() {
        use crate::repository::playlist::PlaylistRepository;
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());
        let media_repo = MediaRepository::new(pool.clone());

        // Setup
        let owner = UserFixture::new().with_username("swap_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Swap Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
            &playlist_repo,
            room.id.clone(),
            "Swap Playlist",
        )
        .await;

        // Create two media items
        let media1 = Media::from_provider(
            Some(playlist.id.clone()),
            room.id.clone(),
            Some(owner.id.clone()),
            "Video 1".to_string(),
            serde_json::json!({}),
            "direct_url",
            "default".to_string(),
            1024.0,
        );
        let media2 = Media::from_provider(
            Some(playlist.id.clone()),
            room.id.clone(),
            Some(owner.id.clone()),
            "Video 2".to_string(),
            serde_json::json!({}),
            "direct_url",
            "default".to_string(),
            2048.0,
        );

        let created1 = media_repo.create(&media1).await.unwrap();
        let created2 = media_repo.create(&media2).await.unwrap();

        assert_eq!(created1.position, 1024.0);
        assert_eq!(created2.position, 2048.0);

        let mut tx = pool.begin().await.unwrap();
        media_repo
            .move_with_tx(&created2.id, Some(&created1.id), None, &mut tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        // Verify ordering changed and only the moved item crossed the anchor.
        let fetched1 = media_repo.get_by_id(&created1.id).await.unwrap().unwrap();
        let fetched2 = media_repo.get_by_id(&created2.id).await.unwrap().unwrap();

        assert!(fetched2.position < fetched1.position);
    }

    /// Integration test: Count by playlist
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_count_by_playlist() {
        use crate::repository::playlist::PlaylistRepository;
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());
        let media_repo = MediaRepository::new(pool.clone());

        // Setup
        let owner = UserFixture::new().with_username("count_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Count Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
            &playlist_repo,
            room.id.clone(),
            "Count Playlist",
        )
        .await;

        // Initially empty
        let count = media_repo.count_by_playlist(&playlist.id).await.unwrap();
        assert_eq!(count, 0);

        // Add 3 items
        for i in 0..3 {
            let media = Media::from_provider(
                Some(playlist.id.clone()),
                room.id.clone(),
                Some(owner.id.clone()),
                format!("Video {i}"),
                serde_json::json!({}),
                "direct_url",
                "default".to_string(),
                i as f64,
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
        use crate::repository::playlist::PlaylistRepository;
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());
        let media_repo = MediaRepository::new(pool.clone());

        // Setup
        let owner = UserFixture::new().with_username("paginate_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Paginate Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
            &playlist_repo,
            room.id.clone(),
            "Paginate Playlist",
        )
        .await;

        // Create 15 items
        for i in 0..15 {
            let media = Media::from_provider(
                Some(playlist.id.clone()),
                room.id.clone(),
                Some(owner.id.clone()),
                format!("Video {i}"),
                serde_json::json!({}),
                "direct_url",
                "default".to_string(),
                i as f64,
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
        use crate::repository::playlist::PlaylistRepository;
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());
        let media_repo = MediaRepository::new(pool.clone());

        // Setup
        let owner = UserFixture::new()
            .with_username("batch_delete_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Batch Delete Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
            &playlist_repo,
            room.id.clone(),
            "Batch Delete Playlist",
        )
        .await;

        // Create 5 items
        let mut ids: Vec<MediaId> = Vec::new();
        for i in 0..5 {
            let media = Media::from_provider(
                Some(playlist.id.clone()),
                room.id.clone(),
                Some(owner.id.clone()),
                format!("Video {i}"),
                serde_json::json!({}),
                "direct_url",
                "default".to_string(),
                i as f64,
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
        use crate::repository::playlist::PlaylistRepository;
        use crate::repository::room::RoomRepository;
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let playlist_repo = PlaylistRepository::new(pool.clone());
        let media_repo = MediaRepository::new(pool.clone());

        // Setup
        let owner = UserFixture::new().with_username("get_ids_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Get IDs Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
            &playlist_repo,
            room.id.clone(),
            "Get IDs Playlist",
        )
        .await;

        // Create 3 items
        let mut ids: Vec<MediaId> = Vec::new();
        for i in 0..3 {
            let media = Media::from_provider(
                Some(playlist.id.clone()),
                room.id.clone(),
                Some(owner.id.clone()),
                format!("Video {i}"),
                serde_json::json!({}),
                "direct_url",
                "default".to_string(),
                i as f64,
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
