//! Media repository for database operations
//!
//! Design reference: external design doc 04-database-design.md §2.4.2

use super::query_builder::escape_ilike;
use sqlx::{postgres::PgRow, PgPool, Row};
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;

use crate::{
    models::{
        normalize_provider_instance_name, provider_type_code_from_name, Media, MediaId,
        MediaListQuery, PageParams, PlaylistId, ProviderTypeName, RoomId, UserId,
    },
    Result,
};

const MEDIA_ROW_COLUMNS: &str = "id,
                   playlist_id,
                   room_id,
                   creator_id,
                   name,
                   description,
                   position,
                   source_provider,
                   source_config,
                   NULLIF(provider_instance_name, '') AS provider_instance_name,
                   cover_file_reference_id,
                   added_at,
                   updated_at,
                   version";

#[derive(Debug, sqlx::FromRow)]
struct MediaRow {
    id: MediaId,
    playlist_id: Option<PlaylistId>,
    room_id: RoomId,
    creator_id: Option<crate::models::UserId>,
    name: String,
    description: String,
    position: f64,
    source_provider: ProviderTypeName,
    source_config: serde_json::Value,
    provider_instance_name: Option<String>,
    cover_file_reference_id: Option<i64>,
    added_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    version: i32,
}

impl From<MediaRow> for Media {
    fn from(row: MediaRow) -> Self {
        Self {
            id: row.id,
            playlist_id: row.playlist_id,
            room_id: row.room_id,
            creator_id: row.creator_id,
            name: row.name,
            description: row.description,
            position: row.position,
            source_provider: row.source_provider.0,
            source_config: row.source_config,
            provider_instance_name: row.provider_instance_name,
            cover_file_reference_id: row.cover_file_reference_id,
            added_at: row.added_at,
            updated_at: row.updated_at,
            version: row.version,
        }
    }
}

fn media_from_pg_row(row: &PgRow) -> Result<Media> {
    Ok(Media {
        id: row.try_get("id")?,
        playlist_id: row.try_get("playlist_id")?,
        room_id: row.try_get("room_id")?,
        creator_id: row.try_get("creator_id")?,
        name: row.try_get("name")?,
        description: row.try_get("description")?,
        position: row.try_get("position")?,
        source_provider: row.try_get::<ProviderTypeName, _>("source_provider")?.0,
        source_config: row.try_get("source_config")?,
        provider_instance_name: row.try_get("provider_instance_name")?,
        cover_file_reference_id: row.try_get("cover_file_reference_id")?,
        added_at: row.try_get("added_at")?,
        updated_at: row.try_get("updated_at")?,
        version: row.try_get("version")?,
    })
}

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

const CREATE_BATCH_CHUNK_SIZE: usize = 1000;

fn pagination_u64_to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn rows_affected_to_usize(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

fn usize_to_f64(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
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
        super::stable_scope_lock_key(room_id.as_i64(), playlist_id.map(PlaylistId::as_i64))
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
                "NULLIF(m.provider_instance_name, '') {direction}, m.name {direction}, m.id {direction}"
            ),
            crate::models::MediaListSortBy::Position => {
                format!("m.position {direction}, m.name {direction}, m.id {direction}")
            }
        }
    }

    fn normalize_provider_instance_name_for_db(
        provider_instance_name: Option<&str>,
    ) -> Option<&str> {
        normalize_provider_instance_name(provider_instance_name)
    }

    fn provider_type_code(provider: &str) -> Result<i16> {
        provider_type_code_from_name(provider).map_err(crate::Error::InvalidInput)
    }

    fn push_media_scope_filters(
        builder: &mut sqlx::QueryBuilder<'_, sqlx::Postgres>,
        room_id: &RoomId,
        playlist_id: Option<&PlaylistId>,
        query: &MediaListQuery,
    ) -> Result<()> {
        builder.push(" FROM media m LEFT JOIN users u ON m.creator_id = u.id AND u.deleted_at IS NULL WHERE m.room_id = ");
        builder.push_bind(room_id.as_i64());
        match playlist_id {
            Some(playlist_id) => {
                builder.push(" AND m.playlist_id = ");
                builder.push_bind(playlist_id.as_i64());
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
            builder.push_bind(Self::provider_type_code(source_provider)?);
        }
        if let Some(provider_instance_name) = &query.provider_instance_name {
            if let Some(trimmed) = normalize_provider_instance_name(Some(provider_instance_name)) {
                builder.push(" AND m.provider_instance_name = ");
                builder.push_bind(trimmed.to_owned());
            } else {
                builder.push(" AND NULLIF(m.provider_instance_name, '') IS NULL");
            }
        }
        match query.availability {
            Some(true) => {
                builder.push(
                    " AND (m.creator_id IS NULL OR (u.id IS NOT NULL AND NOT EXISTS (
                    SELECT 1 FROM user_bans ub
                    WHERE ub.user_id = u.id
                      AND ub.revoked_at IS NULL
                      AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
                )))",
                );
            }
            Some(false) => {
                builder.push(
                    " AND m.creator_id IS NOT NULL AND (u.id IS NULL OR EXISTS (
                    SELECT 1 FROM user_bans ub
                    WHERE ub.user_id = u.id
                      AND ub.revoked_at IS NULL
                      AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
                ))",
                );
            }
            None => {}
        }
        Ok(())
    }

    pub async fn count_filtered_by_scope(
        &self,
        room_id: &RoomId,
        playlist_id: Option<&PlaylistId>,
        query: &MediaListQuery,
    ) -> Result<i64> {
        let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new("SELECT COUNT(*)");
        Self::push_media_scope_filters(&mut builder, room_id, playlist_id, query)?;
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
        let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new(
            "SELECT m.id, m.playlist_id, m.room_id, m.creator_id, m.name, m.description, m.position,
                    m.source_provider, m.source_config, NULLIF(m.provider_instance_name, '') AS provider_instance_name,
                    m.cover_file_reference_id,
                    m.added_at, m.updated_at, m.version,
                    CASE
                      WHEN m.creator_id IS NULL THEN TRUE
                      WHEN u.id IS NOT NULL AND NOT EXISTS (
                          SELECT 1 FROM user_bans ub
                          WHERE ub.user_id = u.id
                            AND ub.revoked_at IS NULL
                            AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
                      ) THEN TRUE
                      ELSE FALSE
                    END AS is_available",
        );
        Self::push_media_scope_filters(&mut builder, room_id, playlist_id, query)?;
        let order_by = Self::build_media_list_order_by(query);
        builder.push(format!(" ORDER BY {order_by} LIMIT "));
        builder.push_bind(limit);
        builder.push(" OFFSET ");
        builder.push_bind(offset);

        let rows = builder.build().fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                Ok(MediaListItem {
                    media: media_from_pg_row(&row)?,
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

        let row = sqlx::query_as_unchecked!(
            MediaRow,
            r"
            INSERT INTO media (playlist_id, room_id, creator_id, name, description, position,
                              source_provider, source_config, provider_instance_name, added_at, updated_at, version)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10, 0)
             RETURNING id,
                       playlist_id,
                       room_id,
                       creator_id,
                       name,
                       description,
                       position,
                       source_provider,
                       source_config,
                       NULLIF(provider_instance_name, '') AS provider_instance_name,
                       cover_file_reference_id,
                       added_at, updated_at, version
            ",
            media.playlist_id.as_ref().map(PlaylistId::as_i64),
            media.room_id.as_i64(),
            media.creator_id.as_ref().map(UserId::as_i64),
            &media.name,
            &media.description,
            media.position,
            Self::provider_type_code(&media.source_provider)?,
            &source_config_json,
            Self::normalize_provider_instance_name_for_db(media.provider_instance_name.as_deref(),),
            media.added_at
        )
        .fetch_one(executor)
        .await?;

        Ok(row.into())
    }

    /// Batch insert media items.
    ///
    /// Automatically chunks large batches to stay within `PostgreSQL`'s 65535
    /// bind-parameter limit.
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
    /// Each row occupies 9 bind parameters. `PostgreSQL`'s hard limit is 65535
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
            "INSERT INTO media (playlist_id, room_id, creator_id, name, description, position,
                               source_provider, source_config, provider_instance_name, added_at, updated_at, version)
             VALUES "
        );
        let mut binds = Vec::new();
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                query_builder.push_str(", ");
            }
            let base = i * PARAMS_PER_ROW;
            write!(
                query_builder,
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
            )
            .expect("writing SQL into String should not fail");
            binds.push(serde_json::to_value(&item.source_config)?);
        }
        query_builder.push_str(
            " RETURNING id, playlist_id, room_id, creator_id, name, description, position,
                       source_provider, source_config, NULLIF(provider_instance_name, '') AS provider_instance_name,
                       cover_file_reference_id,
                       added_at, updated_at, version",
        );

        let mut query = sqlx::query(&query_builder);
        for (i, item) in items.iter().enumerate() {
            query = query
                .bind(item.playlist_id.as_ref().map(PlaylistId::as_i64))
                .bind(item.room_id)
                .bind(
                    item.creator_id
                        .as_ref()
                        .map(super::super::models::id::UserId::as_i64),
                )
                .bind(&item.name)
                .bind(&item.description)
                .bind(item.position)
                .bind(Self::provider_type_code(&item.source_provider)?)
                .bind(&binds[i])
                .bind(Self::normalize_provider_instance_name_for_db(
                    item.provider_instance_name.as_deref(),
                ))
                .bind(item.added_at);
        }

        let rows = query.fetch_all(executor).await?;
        for row in rows {
            results.push(media_from_pg_row(&row)?);
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

        let mut all_results = Vec::with_capacity(items.len());

        for chunk in items.chunks(CREATE_BATCH_CHUNK_SIZE) {
            let results = Self::create_batch_chunk(chunk, &mut **tx).await?;
            all_results.extend(results);
        }

        Ok(all_results)
    }

    /// Update media
    pub async fn update(&self, media: &Media) -> Result<Media> {
        let sql = format!(
            r"
            UPDATE media
            SET name = $2, description = $3, position = $4
             WHERE id = $1
             RETURNING {MEDIA_ROW_COLUMNS}
            "
        );
        let row = sqlx::query_as::<_, MediaRow>(&sql)
            .bind(media.id.as_i64())
            .bind(&media.name)
            .bind(&media.description)
            .bind(media.position)
            .fetch_one(&self.pool)
            .await?;

        Ok(row.into())
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
        self.update_with_version_with_executor(media, expected_version, &self.pool)
            .await
    }

    pub async fn update_with_version_with_executor<'e, E>(
        &self,
        media: &Media,
        expected_version: i32,
        executor: E,
    ) -> Result<Option<Media>>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let sql = format!(
            r"
            UPDATE media
            SET name = $2, description = $3, position = $4, version = version + 1
             WHERE id = $1 AND version = $5
             RETURNING {MEDIA_ROW_COLUMNS}
            "
        );
        let row = sqlx::query_as::<_, MediaRow>(&sql)
            .bind(media.id.as_i64())
            .bind(&media.name)
            .bind(&media.description)
            .bind(media.position)
            .bind(expected_version)
            .fetch_optional(executor)
            .await?;

        Ok(row.map(Into::into))
    }

    /// Get media by ID
    pub async fn get_by_id(&self, media_id: &MediaId) -> Result<Option<Media>> {
        let sql = format!(
            r"
            SELECT {MEDIA_ROW_COLUMNS}
             FROM media
             WHERE id = $1
            "
        );
        let row = sqlx::query_as::<_, MediaRow>(&sql)
            .bind(media_id.as_i64())
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.map(Into::into))
    }

    /// Get media by ID, scoped to a room.
    pub async fn get_by_room_and_id(
        &self,
        room_id: &RoomId,
        media_id: &MediaId,
    ) -> Result<Option<Media>> {
        let sql = format!(
            r"
            SELECT {MEDIA_ROW_COLUMNS}
             FROM media
             WHERE room_id = $1 AND id = $2
            "
        );
        let row = sqlx::query_as::<_, MediaRow>(&sql)
            .bind(room_id.as_i64())
            .bind(media_id.as_i64())
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.map(Into::into))
    }

    pub async fn get_by_room_and_id_for_update_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        media_id: &MediaId,
        executor: E,
    ) -> Result<Option<Media>>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let sql = format!(
            r"
            SELECT {MEDIA_ROW_COLUMNS}
             FROM media
             WHERE room_id = $1 AND id = $2
             FOR UPDATE
            "
        );
        let row = sqlx::query_as::<_, MediaRow>(&sql)
            .bind(room_id.as_i64())
            .bind(media_id.as_i64())
            .fetch_optional(executor)
            .await?;

        Ok(row.map(Into::into))
    }

    pub async fn update_cover_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        media_id: &MediaId,
        cover_file_reference_id: Option<i64>,
        expected_version: i32,
        executor: E,
    ) -> Result<Option<Media>>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let sql = format!(
            r"
            UPDATE media
            SET cover_file_reference_id = $3,
                version = version + 1
             WHERE room_id = $1 AND id = $2 AND version = $4
             RETURNING {MEDIA_ROW_COLUMNS}
            "
        );
        let row = sqlx::query_as::<_, MediaRow>(&sql)
            .bind(room_id.as_i64())
            .bind(media_id.as_i64())
            .bind(cover_file_reference_id)
            .bind(expected_version)
            .fetch_optional(executor)
            .await?;

        Ok(row.map(Into::into))
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

        let id_strs: Vec<i64> = media_ids.iter().map(MediaId::as_i64).collect();
        let sql = format!(
            r"
            SELECT {MEDIA_ROW_COLUMNS}
             FROM media
             WHERE id = ANY($1)
            "
        );
        let rows = sqlx::query_as::<_, MediaRow>(&sql)
            .bind(&id_strs)
            .fetch_all(executor)
            .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Get multiple media items by IDs, scoped to a room.
    pub async fn get_by_room_and_ids_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        media_ids: &[MediaId],
        executor: E,
    ) -> Result<Vec<Media>>
    where
        E: sqlx::Executor<'e, Database = sqlx::Postgres>,
    {
        if media_ids.is_empty() {
            return Ok(Vec::new());
        }

        let id_strs: Vec<i64> = media_ids.iter().map(MediaId::as_i64).collect();
        let sql = format!(
            r"
            SELECT {MEDIA_ROW_COLUMNS}
             FROM media
             WHERE room_id = $1 AND id = ANY($2)
            "
        );
        let rows = sqlx::query_as::<_, MediaRow>(&sql)
            .bind(room_id.as_i64())
            .bind(&id_strs)
            .fetch_all(executor)
            .await?;

        Ok(rows.into_iter().map(Into::into).collect())
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
        let sql = format!(
            r"
            SELECT {MEDIA_ROW_COLUMNS}
            FROM media
            WHERE room_id = $1
              AND playlist_id IS NOT DISTINCT FROM $2
            ORDER BY position ASC, id ASC
            "
        );
        let rows = sqlx::query_as::<_, MediaRow>(&sql)
            .bind(room_id.as_i64())
            .bind(playlist_id.map(PlaylistId::as_i64))
            .fetch_all(executor)
            .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Get media directly under the room root.
    pub async fn get_room_root(&self, room_id: &RoomId) -> Result<Vec<Media>> {
        let sql = format!(
            r"
            SELECT {MEDIA_ROW_COLUMNS}
             FROM media
             WHERE room_id = $1
               AND playlist_id IS NULL
             ORDER BY position ASC
            "
        );
        let rows = sqlx::query_as::<_, MediaRow>(&sql)
            .bind(room_id.as_i64())
            .fetch_all(&self.pool)
            .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Get media in a specific playlist.
    pub async fn get_by_playlist(&self, playlist_id: &PlaylistId) -> Result<Vec<Media>> {
        let sql = format!(
            r"
            SELECT {MEDIA_ROW_COLUMNS}
             FROM media
             WHERE playlist_id = $1
             ORDER BY position ASC
            "
        );
        let rows = sqlx::query_as::<_, MediaRow>(&sql)
            .bind(playlist_id.as_i64())
            .fetch_all(&self.pool)
            .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Get media in a specific playlist, scoped to a room.
    pub async fn get_by_room_and_playlist(
        &self,
        room_id: &RoomId,
        playlist_id: &PlaylistId,
    ) -> Result<Vec<Media>> {
        let sql = format!(
            r"
            SELECT {MEDIA_ROW_COLUMNS}
             FROM media
             WHERE room_id = $1 AND playlist_id = $2
             ORDER BY position ASC
            "
        );
        let rows = sqlx::query_as::<_, MediaRow>(&sql)
            .bind(room_id.as_i64())
            .bind(playlist_id.as_i64())
            .fetch_all(&self.pool)
            .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Get paginated media for a specific playlist.
    pub async fn get_playlist_paginated(
        &self,
        playlist_id: &PlaylistId,
        pagination: PageParams,
    ) -> Result<(Vec<Media>, i64)> {
        let limit = pagination_u64_to_i64(pagination.limit());
        let offset = pagination_u64_to_i64(pagination.offset());

        // Get total count
        let total = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!" FROM media WHERE playlist_id = $1
            "#,
            playlist_id.as_i64(),
        )
        .fetch_one(&self.pool)
        .await?;

        // Get paginated results
        let sql = format!(
            r"
            SELECT {MEDIA_ROW_COLUMNS}
             FROM media
             WHERE playlist_id = $1
             ORDER BY position ASC
             LIMIT $2 OFFSET $3
            "
        );
        let rows = sqlx::query_as::<_, MediaRow>(&sql)
            .bind(playlist_id.as_i64())
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?;

        let items = rows.into_iter().map(Into::into).collect();

        Ok((items, total))
    }

    /// Get paginated media directly under the room root.
    pub async fn get_room_root_paginated(
        &self,
        room_id: &RoomId,
        pagination: PageParams,
    ) -> Result<(Vec<Media>, i64)> {
        let limit = pagination_u64_to_i64(pagination.limit());
        let offset = pagination_u64_to_i64(pagination.offset());

        let total = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM media
            WHERE room_id = $1
              AND playlist_id IS NULL
            "#,
            room_id.as_i64(),
        )
        .fetch_one(&self.pool)
        .await?;

        let sql = format!(
            r"
            SELECT {MEDIA_ROW_COLUMNS}
             FROM media
             WHERE room_id = $1
               AND playlist_id IS NULL
             ORDER BY position ASC
             LIMIT $2 OFFSET $3
            "
        );
        let rows = sqlx::query_as::<_, MediaRow>(&sql)
            .bind(room_id.as_i64())
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?;

        let items = rows.into_iter().map(Into::into).collect();

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
        let sql = format!(
            r"
            SELECT {MEDIA_ROW_COLUMNS}
             FROM media
             WHERE playlist_id = $1
             ORDER BY position ASC
             LIMIT $2 OFFSET $3
            "
        );
        let rows = sqlx::query_as::<_, MediaRow>(&sql)
            .bind(playlist_id.as_i64())
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Get room-root media items with limit and offset (no count query).
    pub async fn get_room_root_limit_offset(
        &self,
        room_id: &RoomId,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Media>> {
        let sql = format!(
            r"
            SELECT {MEDIA_ROW_COLUMNS}
             FROM media
             WHERE room_id = $1
               AND playlist_id IS NULL
             ORDER BY position ASC
             LIMIT $2 OFFSET $3
            "
        );
        let rows = sqlx::query_as::<_, MediaRow>(&sql)
            .bind(room_id.as_i64())
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Delete media from playlist
    pub async fn delete(&self, media_id: &MediaId) -> Result<bool> {
        let result = sqlx::query!(
            r#"
            DELETE FROM media
             WHERE id = $1
            "#,
            media_id.as_i64(),
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Delete all media in a playlist.
    pub async fn delete_playlist(&self, playlist_id: &PlaylistId) -> Result<usize> {
        let result = sqlx::query!(
            r#"
            DELETE FROM media
             WHERE playlist_id = $1
            "#,
            playlist_id.as_i64(),
        )
        .execute(&self.pool)
        .await?;

        Ok(rows_affected_to_usize(result.rows_affected()))
    }

    /// Delete all media directly under the room root.
    pub async fn delete_room_root(&self, room_id: &RoomId) -> Result<usize> {
        let result = sqlx::query!(
            r#"
            DELETE FROM media
             WHERE room_id = $1
               AND playlist_id IS NULL
            "#,
            room_id.as_i64(),
        )
        .execute(&self.pool)
        .await?;

        Ok(rows_affected_to_usize(result.rows_affected()))
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

        let id_strs: Vec<i64> = media_ids.iter().map(MediaId::as_i64).collect();

        let result = sqlx::query!(
            r#"
            DELETE FROM media
             WHERE id = ANY($1)
            "#,
            &id_strs,
        )
        .execute(executor)
        .await?;

        Ok(rows_affected_to_usize(result.rows_affected()))
    }

    async fn lock_scope_with_tx(
        &self,
        room_id: &RoomId,
        playlist_id: Option<&PlaylistId>,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<()> {
        sqlx::query!(
            "SELECT pg_advisory_xact_lock($1)",
            Self::scope_lock_key(room_id, playlist_id)
        )
        .fetch_one(&mut **tx)
        .await?;
        Ok(())
    }

    async fn lock_scopes_with_tx(
        &self,
        scopes: &[(RoomId, Option<PlaylistId>)],
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<()> {
        let unique_scopes: BTreeSet<(RoomId, Option<PlaylistId>)> = scopes
            .iter()
            .map(|(room_id, playlist_id)| (*room_id, *playlist_id))
            .collect();

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
                let step = gap / usize_to_f64(count + 1);
                if !step.is_finite() || step <= Self::MIN_ORDER_GAP {
                    return None;
                }
                let mut positions = Vec::with_capacity(count);
                for index in 1..=count {
                    let position = previous + step * usize_to_f64(index);
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
                let start = Self::ORDER_STEP.mul_add(-usize_to_f64(count), next);
                if !start.is_finite() {
                    return None;
                }
                let mut positions = Vec::with_capacity(count);
                for index in 0..count {
                    let position = Self::ORDER_STEP.mul_add(usize_to_f64(index), start);
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
                    let position = Self::ORDER_STEP.mul_add(usize_to_f64(index), previous);
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
                    positions.push(Self::ORDER_STEP * usize_to_f64(index));
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

        let media_id_strs: Vec<i64> = media_ids.iter().map(MediaId::as_i64).collect();
        let sql = format!(
            r"
            SELECT {MEDIA_ROW_COLUMNS}
            FROM media
            WHERE room_id = $1 AND id = ANY($2)
            FOR UPDATE
            "
        );
        let moved_rows = sqlx::query_as::<_, MediaRow>(&sql)
            .bind(room_id.as_i64())
            .bind(&media_id_strs)
            .fetch_all(&mut **tx)
            .await?;

        if moved_rows.len() != media_ids.len() {
            return Err(crate::Error::NotFound("Media not found".to_string()));
        }

        let mut moved_map = HashMap::with_capacity(moved_rows.len());
        for row in moved_rows {
            let media: Media = row.into();
            moved_map.insert(media.id, media);
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
            let sql = format!(
                r"
                SELECT {MEDIA_ROW_COLUMNS}
                FROM media
                WHERE room_id = $1 AND id = $2
                FOR UPDATE
                "
            );
            let anchor_media: Media = sqlx::query_as::<_, MediaRow>(&sql)
                .bind(room_id.as_i64())
                .bind(anchor_id.as_i64())
                .fetch_optional(&mut **tx)
                .await?
                .map(Into::into)
                .ok_or_else(|| crate::Error::NotFound("Anchor media not found".to_string()))?;

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
                Some(*target_playlist_id)
            }
            (None, Some(anchor_media)) => anchor_media.playlist_id,
            (Some(target_playlist_id), None) => Some(*target_playlist_id),
            (None, None) => {
                let first_scope = moved_media[0].playlist_id;
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
            .map(|media| (media.room_id, media.playlist_id))
            .collect();
        affected_scopes.push((*room_id, effective_target_playlist_id));
        self.lock_scopes_with_tx(&affected_scopes, tx).await?;

        for _ in 0..2 {
            let target_rows = sqlx::query!(
                r#"
                SELECT id AS "id: MediaId", position
                FROM media
                WHERE room_id = $1
                  AND playlist_id IS NOT DISTINCT FROM $2
                  AND NOT (id = ANY($3))
                ORDER BY position ASC, id ASC
                FOR UPDATE
                "#,
                room_id.as_i64(),
                effective_target_playlist_id
                    .as_ref()
                    .map(PlaylistId::as_i64),
                &media_id_strs,
            )
            .fetch_all(&mut **tx)
            .await?;

            let target_rows: Vec<(MediaId, f64)> = target_rows
                .into_iter()
                .map(|row| (row.id, row.position))
                .collect();

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
                let sql = format!(
                    r"
                    UPDATE media
                    SET playlist_id = $2,
                        position = $3,
                        version = version + 1
                    WHERE id = $1
                    RETURNING {MEDIA_ROW_COLUMNS}
                    "
                );
                let row = sqlx::query_as::<_, MediaRow>(&sql)
                    .bind(media.id.as_i64())
                    .bind(
                        effective_target_playlist_id
                            .as_ref()
                            .map(PlaylistId::as_i64),
                    )
                    .bind(position)
                    .fetch_one(&mut **tx)
                    .await?;
                updated_media.push(row.into());
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
        sqlx::query_scalar!(
            r#"
            SELECT position AS "position!"
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
            "#,
            room_id.as_i64(),
            playlist_id.map(PlaylistId::as_i64),
            exclude_media_id.as_i64(),
            anchor_position,
            anchor_media_id.as_i64(),
        )
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
        sqlx::query_scalar!(
            r#"
            SELECT position AS "position!"
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
            "#,
            room_id.as_i64(),
            playlist_id.map(PlaylistId::as_i64),
            exclude_media_id.as_i64(),
            anchor_position,
            anchor_media_id.as_i64(),
        )
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
        let rows = sqlx::query!(
            r#"
            SELECT id AS "id: MediaId"
            FROM media
            WHERE room_id = $1
              AND playlist_id IS NOT DISTINCT FROM $2
            ORDER BY position ASC, id ASC
            FOR UPDATE
            "#,
            room_id.as_i64(),
            playlist_id.map(PlaylistId::as_i64),
        )
        .fetch_all(&mut **tx)
        .await?;

        for (index, row) in rows.into_iter().enumerate() {
            let position = Self::ORDER_STEP * usize_to_f64(index + 1);
            sqlx::query!(
                "UPDATE media SET position = $2, version = version + 1 WHERE id = $1",
                row.id.as_i64(),
                position,
            )
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

        let max_pos = sqlx::query_scalar!(
            r#"
            SELECT MAX(position)
            FROM media
            WHERE room_id = $1
              AND playlist_id IS NOT DISTINCT FROM $2
            "#,
            room_id.as_i64(),
            playlist_id.map(PlaylistId::as_i64),
        )
        .fetch_one(&mut **tx)
        .await?;

        match max_pos {
            Some(position) if position.is_finite() => Ok(position + Self::ORDER_STEP),
            _ => Ok(Self::ORDER_STEP),
        }
    }

    pub async fn move_with_tx(
        &self,
        room_id: &RoomId,
        media_id: &MediaId,
        before_media_id: Option<&MediaId>,
        after_media_id: Option<&MediaId>,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<Media> {
        let ((Some(anchor_id), None) | (None, Some(anchor_id))) = (before_media_id, after_media_id)
        else {
            return Err(crate::Error::InvalidInput(
                "Exactly one of before_media_id or after_media_id must be set".to_string(),
            ));
        };

        if media_id == anchor_id {
            return Err(crate::Error::InvalidInput(
                "Cannot move media relative to itself".to_string(),
            ));
        }

        let sql = format!(
            r"
            SELECT {MEDIA_ROW_COLUMNS}
            FROM media
            WHERE room_id = $1 AND id = $2
            FOR UPDATE
            "
        );
        let moved: Media = sqlx::query_as::<_, MediaRow>(&sql)
            .bind(room_id.as_i64())
            .bind(media_id.as_i64())
            .fetch_optional(&mut **tx)
            .await?
            .map(Into::into)
            .ok_or_else(|| crate::Error::NotFound("Media not found".to_string()))?;

        let sql = format!(
            r"
            SELECT {MEDIA_ROW_COLUMNS}
            FROM media
            WHERE room_id = $1 AND id = $2
            FOR UPDATE
            "
        );
        let anchor: Media = sqlx::query_as::<_, MediaRow>(&sql)
            .bind(room_id.as_i64())
            .bind(anchor_id.as_i64())
            .fetch_optional(&mut **tx)
            .await?
            .map(Into::into)
            .ok_or_else(|| crate::Error::NotFound("Anchor media not found".to_string()))?;

        if moved.playlist_id != anchor.playlist_id {
            return Err(crate::Error::InvalidInput(
                "Media can only be moved relative to a sibling in the same playlist scope"
                    .to_string(),
            ));
        }

        self.lock_scope_with_tx(&moved.room_id, moved.playlist_id.as_ref(), tx)
            .await?;

        for _ in 0..2 {
            let anchor_position = sqlx::query_scalar!(
                r#"SELECT position AS "position!" FROM media WHERE id = $1 FOR UPDATE"#,
                anchor.id.as_i64(),
            )
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
                let sql = format!(
                    r"
                    UPDATE media
                    SET position = $2, version = version + 1
                    WHERE id = $1
                    RETURNING {MEDIA_ROW_COLUMNS}
                    "
                );
                let row = sqlx::query_as::<_, MediaRow>(&sql)
                    .bind(moved.id.as_i64())
                    .bind(position)
                    .fetch_one(&mut **tx)
                    .await?;

                return Ok(row.into());
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
        let count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!" FROM media WHERE playlist_id = $1
            "#,
            playlist_id.as_i64(),
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(count)
    }

    /// Count all media items.
    pub async fn count_all(&self) -> Result<i64> {
        let count = sqlx::query_scalar_unchecked!(
            r"
            SELECT COUNT(*) FROM media
            "
        )
        .fetch_one(&self.pool)
        .await?
        .unwrap_or(0);

        Ok(count)
    }

    /// Count media items in a playlist, scoped to a room.
    pub async fn count_by_room_and_playlist(
        &self,
        room_id: &RoomId,
        playlist_id: &PlaylistId,
    ) -> Result<i64> {
        let count = sqlx::query_scalar_unchecked!(
            r"
            SELECT COUNT(*) FROM media WHERE room_id = $1 AND playlist_id = $2
            ",
            room_id.as_i64(),
            playlist_id.as_i64()
        )
        .fetch_one(&self.pool)
        .await?
        .unwrap_or(0);

        Ok(count)
    }

    /// Count only media whose creator is still active (or media without a creator).
    pub async fn count_by_playlist_accessible(&self, playlist_id: &PlaylistId) -> Result<i64> {
        let count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM media m
            LEFT JOIN users u
              ON m.creator_id = u.id
             AND u.deleted_at IS NULL
            WHERE m.playlist_id = $1
              AND (m.creator_id IS NULL OR (
                  u.id IS NOT NULL AND NOT EXISTS (
                      SELECT 1 FROM user_bans ub
                      WHERE ub.user_id = u.id
                        AND ub.revoked_at IS NULL
                        AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
                  )
              ))
            "#,
            playlist_id.as_i64(),
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(count)
    }

    /// Count media items directly under the room root.
    pub async fn count_room_root(&self, room_id: &RoomId) -> Result<i64> {
        let count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM media
            WHERE room_id = $1
              AND playlist_id IS NULL
            "#,
            room_id.as_i64(),
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(count)
    }

    /// Batch count media items across multiple playlists
    pub async fn count_by_playlists_batch(
        &self,
        playlist_ids: &[PlaylistId],
    ) -> Result<std::collections::HashMap<PlaylistId, i64>> {
        let ids: Vec<i64> = playlist_ids.iter().map(PlaylistId::as_i64).collect();
        let rows = sqlx::query!(
            r#"
            SELECT playlist_id AS "playlist_id!: PlaylistId", COUNT(*) AS "cnt!"
            FROM media
            WHERE playlist_id = ANY($1)
            GROUP BY playlist_id
            "#,
            &ids,
        )
        .fetch_all(&self.pool)
        .await?;

        let mut result = std::collections::HashMap::new();
        for row in rows {
            result.insert(row.playlist_id, row.cnt);
        }
        Ok(result)
    }

    /// Batch count only media whose creator is still active (or media without a creator).
    pub async fn count_by_playlists_batch_accessible(
        &self,
        playlist_ids: &[PlaylistId],
    ) -> Result<std::collections::HashMap<PlaylistId, i64>> {
        if playlist_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let ids: Vec<i64> = playlist_ids.iter().map(PlaylistId::as_i64).collect();

        let rows = sqlx::query!(
            r#"
            SELECT m.playlist_id AS "playlist_id!: PlaylistId", COUNT(*) AS "cnt!"
            FROM media m
            LEFT JOIN users u
              ON m.creator_id = u.id
             AND u.deleted_at IS NULL
            WHERE m.playlist_id = ANY($1)
              AND (m.creator_id IS NULL OR (
                  u.id IS NOT NULL AND NOT EXISTS (
                      SELECT 1 FROM user_bans ub
                      WHERE ub.user_id = u.id
                        AND ub.revoked_at IS NULL
                        AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
                  )
              ))
            GROUP BY m.playlist_id
            "#,
            &ids,
        )
        .fetch_all(&self.pool)
        .await?;

        let mut result = std::collections::HashMap::new();
        for row in rows {
            result.insert(row.playlist_id, row.cnt);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::id::{MediaId, PlaylistId, RoomId, UserId};
    use crate::models::ProviderInstance;
    use crate::repository::ProviderInstanceRepository;
    use sqlx::Execute;
    use synctv_core_testing::create_test_pool;

    async fn insert_test_provider_instance(pool: &PgPool, name: &str, provider: &str) {
        let now = chrono::Utc::now();
        let instance = ProviderInstance {
            name: name.to_string(),
            endpoint: "http://localhost:50051".to_string(),
            comment: Some("test provider instance".to_string()),
            jwt_secret: None,
            custom_ca: None,
            timeout: "10s".to_string(),
            tls: false,
            insecure_tls: false,
            providers: vec![provider.to_string()],
            enabled: true,
            created_at: now,
            updated_at: now,
        };
        ProviderInstanceRepository::new(pool.clone())
            .create(&instance)
            .await
            .unwrap();
    }

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
            None,
            0.0,
        );

        assert_eq!(media.name, "Test Video");
        assert!((media.position - 0.0).abs() < f64::EPSILON);
        assert_eq!(media.source_provider, "direct_url");
    }

    #[test]
    fn test_push_media_scope_filters_treats_empty_provider_instance_as_default() {
        let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new("SELECT m.id FROM media m");
        let query = MediaListQuery {
            provider_instance_name: Some("   ".to_string()),
            ..MediaListQuery::default()
        };
        let room_id = RoomId::expect_positive(123_456_678);

        MediaRepository::push_media_scope_filters(&mut builder, &room_id, None, &query).unwrap();

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
        assert!((media.position - 5.0).abs() < f64::EPSILON);
        assert!(media.provider_instance_name.is_none());
        assert_eq!(
            media.source_config["url"],
            serde_json::json!("https://example.com/video.mp4")
        );
        assert!(media.source_config.get("playback_infos").is_none());
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
            &playback_infos,
            "direct",
            &metadata,
            10.0,
        );

        assert_eq!(media.name, "Multimode Video");
        assert!((media.position - 10.0).abs() < f64::EPSILON);
        assert!(media.provider_instance_name.is_none());
        assert_eq!(
            media.source_config["url"],
            serde_json::json!("https://example.com/video.mp4")
        );
        assert!(media.source_config.get("playback_infos").is_none());
        assert!(media.source_config.get("metadata").is_none());
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
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
            &playlist_repo,
            room.id,
            "Test Playlist",
        )
        .await;

        // Create media
        let media = Media::from_provider(
            Some(playlist.id),
            room.id,
            Some(owner.id),
            "Test Video".to_string(),
            serde_json::json!({"url": "https://example.com/video.mp4"}),
            "direct_url",
            None,
            0.0,
        );

        let created = media_repo.create(&media).await.unwrap();
        assert_eq!(created.name, "Test Video");
        assert!((created.position - 0.0).abs() < f64::EPSILON);

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
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
            &playlist_repo,
            room.id,
            "Test Playlist",
        )
        .await;

        let media = Media::from_provider(
            Some(playlist.id),
            room.id,
            Some(owner.id),
            "Original Name".to_string(),
            serde_json::json!({}),
            "direct_url",
            None,
            0.0,
        );
        let created = media_repo.create(&media).await.unwrap();

        // Update
        let mut updated = created.clone();
        updated.name = "Updated Name".to_string();
        updated.position = 5.0;
        updated.source_config = serde_json::json!({"url": "https://example.com/changed.mp4"});
        updated.provider_instance_name = Some("changed-instance".to_string());

        let result = media_repo.update(&updated).await.unwrap();
        assert_eq!(result.name, "Updated Name");
        assert!((result.position - 5.0).abs() < f64::EPSILON);
        assert_eq!(result.source_config, created.source_config);
        assert_eq!(
            result.provider_instance_name,
            created.provider_instance_name
        );
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
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
            &playlist_repo,
            room.id,
            "Test Playlist",
        )
        .await;

        let media = Media::from_provider(
            Some(playlist.id),
            room.id,
            Some(owner.id),
            "To Delete".to_string(),
            serde_json::json!({}),
            "direct_url",
            None,
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
                    .with_owner(owner.id)
                    .build(),
            )
            .await
            .unwrap();

        let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
            &playlist_repo,
            room.id,
            "Default Instance Playlist",
        )
        .await;

        let default_media = Media::from_provider(
            Some(playlist.id),
            room.id,
            Some(owner.id),
            "Default Backend".to_string(),
            serde_json::json!({"url": "https://example.com/default.mp4"}),
            "direct_url",
            None,
            0.0,
        );
        let explicit_media = Media::from_provider(
            Some(playlist.id),
            room.id,
            Some(owner.id),
            "Explicit Backend".to_string(),
            serde_json::json!({"url": "https://example.com/explicit.mp4"}),
            "direct_url",
            Some("direct_url_remote".to_string()),
            1.0,
        );

        insert_test_provider_instance(&pool, "direct_url_remote", "direct_url").await;
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
        assert!(rows[0].media.provider_instance_name.is_none());
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
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
            &playlist_repo,
            room.id,
            "Batch Playlist",
        )
        .await;

        // Create batch
        let items: Vec<Media> = (0..5)
            .map(|i| {
                Media::from_provider(
                    Some(playlist.id),
                    room.id,
                    Some(owner.id),
                    format!("Video {i}"),
                    serde_json::json!({"url": format!("https://example.com/{}.mp4", i)}),
                    "direct_url",
                    None,
                    f64::from(i),
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
                    Some(playlist_id),
                    room_id,
                    Some(owner_id),
                    format!("Video {i}"),
                    serde_json::json!({"url": format!("https://example.com/{}.mp4", i)}),
                    "direct_url",
                    None,
                    f64::from(i),
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
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
            &playlist_repo,
            room.id,
            "Swap Playlist",
        )
        .await;

        // Create two media items
        let media1 = Media::from_provider(
            Some(playlist.id),
            room.id,
            Some(owner.id),
            "Video 1".to_string(),
            serde_json::json!({}),
            "direct_url",
            None,
            1024.0,
        );
        let media2 = Media::from_provider(
            Some(playlist.id),
            room.id,
            Some(owner.id),
            "Video 2".to_string(),
            serde_json::json!({}),
            "direct_url",
            None,
            2048.0,
        );

        let created1 = media_repo.create(&media1).await.unwrap();
        let created2 = media_repo.create(&media2).await.unwrap();

        assert!((created1.position - 1024.0).abs() < f64::EPSILON);
        assert!((created2.position - 2048.0).abs() < f64::EPSILON);

        let mut tx = pool.begin().await.unwrap();
        media_repo
            .move_with_tx(&room.id, &created2.id, Some(&created1.id), None, &mut tx)
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
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
            &playlist_repo,
            room.id,
            "Count Playlist",
        )
        .await;

        // Initially empty
        let count = media_repo.count_by_playlist(&playlist.id).await.unwrap();
        assert_eq!(count, 0);

        // Add 3 items
        for i in 0..3 {
            let media = Media::from_provider(
                Some(playlist.id),
                room.id,
                Some(owner.id),
                format!("Video {i}"),
                serde_json::json!({}),
                "direct_url",
                None,
                f64::from(i),
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
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
            &playlist_repo,
            room.id,
            "Paginate Playlist",
        )
        .await;

        // Create 15 items
        for i in 0..15 {
            let media = Media::from_provider(
                Some(playlist.id),
                room.id,
                Some(owner.id),
                format!("Video {i}"),
                serde_json::json!({}),
                "direct_url",
                None,
                f64::from(i),
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
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
            &playlist_repo,
            room.id,
            "Batch Delete Playlist",
        )
        .await;

        // Create 5 items
        let mut ids: Vec<MediaId> = Vec::new();
        for i in 0..5 {
            let media = Media::from_provider(
                Some(playlist.id),
                room.id,
                Some(owner.id),
                format!("Video {i}"),
                serde_json::json!({}),
                "direct_url",
                None,
                f64::from(i),
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
            .with_owner(owner.id)
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_top_level_playlist_hierarchy(
            &playlist_repo,
            room.id,
            "Get IDs Playlist",
        )
        .await;

        // Create 3 items
        let mut ids: Vec<MediaId> = Vec::new();
        for i in 0..3 {
            let media = Media::from_provider(
                Some(playlist.id),
                room.id,
                Some(owner.id),
                format!("Video {i}"),
                serde_json::json!({}),
                "direct_url",
                None,
                f64::from(i),
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
