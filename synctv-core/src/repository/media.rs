//! Media repository for database operations
//!
//! Design reference: external design doc 04-database-design.md §2.4.2

use super::{query_builder::ilike_contains_pattern, sqlx_types::ProviderTypeName};
use sqlx::PgPool;
use std::collections::{BTreeSet, HashMap};

use crate::{
    models::{
        normalize_provider_instance_name, DeletionSource, Media, MediaId, MediaListQuery,
        PageParams, PlaylistId, RoomId, UserId,
    },
    Result,
};

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
    source_config: crate::models::MediaSourceConfig,
    provider_instance_name: Option<String>,
    cover_file_reference_id: Option<i64>,
    thumbnail_file_reference_id: Option<i64>,
    added_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    version: i32,
}

impl From<MediaRow> for Media {
    fn from(row: MediaRow) -> Self {
        let source_provider = row.source_provider.0;
        Self {
            id: row.id,
            playlist_id: row.playlist_id,
            room_id: row.room_id,
            creator_id: row.creator_id,
            name: row.name,
            description: row.description,
            position: row.position,
            source_provider,
            source_config: row.source_config,
            provider_instance_name: row.provider_instance_name,
            cover_file_reference_id: row.cover_file_reference_id,
            thumbnail_file_reference_id: row.thumbnail_file_reference_id,
            added_at: row.added_at,
            updated_at: row.updated_at,
            version: row.version,
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct MediaListRow {
    id: MediaId,
    playlist_id: Option<PlaylistId>,
    room_id: RoomId,
    creator_id: Option<crate::models::UserId>,
    name: String,
    description: String,
    position: f64,
    source_provider: ProviderTypeName,
    source_config: crate::models::MediaSourceConfig,
    provider_instance_name: Option<String>,
    cover_file_reference_id: Option<i64>,
    thumbnail_file_reference_id: Option<i64>,
    added_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    version: i32,
    is_available: bool,
}

impl From<MediaListRow> for MediaListItem {
    fn from(row: MediaListRow) -> Self {
        let source_provider = row.source_provider.0;
        Self {
            media: Media {
                id: row.id,
                playlist_id: row.playlist_id,
                room_id: row.room_id,
                creator_id: row.creator_id,
                name: row.name,
                description: row.description,
                position: row.position,
                source_provider,
                source_config: row.source_config,
                provider_instance_name: row.provider_instance_name,
                cover_file_reference_id: row.cover_file_reference_id,
                thumbnail_file_reference_id: row.thumbnail_file_reference_id,
                added_at: row.added_at,
                updated_at: row.updated_at,
                version: row.version,
            },
            is_available: row.is_available,
        }
    }
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

fn rows_affected_to_usize(value: u64, operation: &'static str) -> Result<usize> {
    usize::try_from(value)
        .map_err(|_| crate::Error::Internal(format!("{operation} affected rows exceed usize::MAX")))
}

fn usize_to_f64(value: usize) -> Option<f64> {
    u32::try_from(value).ok().map(f64::from)
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

    fn push_media_list_order_by(
        builder: &mut sqlx::QueryBuilder<sqlx::Postgres>,
        query: &MediaListQuery,
    ) {
        use crate::models::{MediaListSortBy, SortDirection};

        let order_by = match (query.sort_by, query.sort_direction) {
            (MediaListSortBy::Name, SortDirection::Asc) => {
                " ORDER BY m.name ASC, m.position ASC, m.id ASC"
            }
            (MediaListSortBy::Name, SortDirection::Desc) => {
                " ORDER BY m.name DESC, m.position DESC, m.id DESC"
            }
            (MediaListSortBy::AddedAt, SortDirection::Asc) => {
                " ORDER BY m.added_at ASC, m.position ASC, m.id ASC"
            }
            (MediaListSortBy::AddedAt, SortDirection::Desc) => {
                " ORDER BY m.added_at DESC, m.position DESC, m.id DESC"
            }
            (MediaListSortBy::UpdatedAt, SortDirection::Asc) => {
                " ORDER BY m.updated_at ASC, m.position ASC, m.id ASC"
            }
            (MediaListSortBy::UpdatedAt, SortDirection::Desc) => {
                " ORDER BY m.updated_at DESC, m.position DESC, m.id DESC"
            }
            (MediaListSortBy::SourceProvider, SortDirection::Asc) => {
                " ORDER BY m.source_provider ASC, m.name ASC, m.id ASC"
            }
            (MediaListSortBy::SourceProvider, SortDirection::Desc) => {
                " ORDER BY m.source_provider DESC, m.name DESC, m.id DESC"
            }
            (MediaListSortBy::ProviderInstanceName, SortDirection::Asc) => {
                " ORDER BY NULLIF(m.provider_instance_name, '') ASC, m.name ASC, m.id ASC"
            }
            (MediaListSortBy::ProviderInstanceName, SortDirection::Desc) => {
                " ORDER BY NULLIF(m.provider_instance_name, '') DESC, m.name DESC, m.id DESC"
            }
            (MediaListSortBy::Position, SortDirection::Asc) => {
                " ORDER BY m.position ASC, m.name ASC, m.id ASC"
            }
            (MediaListSortBy::Position, SortDirection::Desc) => {
                " ORDER BY m.position DESC, m.name DESC, m.id DESC"
            }
        };
        builder.push(order_by);
    }

    fn push_media_scope_filters(
        builder: &mut sqlx::QueryBuilder<sqlx::Postgres>,
        room_id: &RoomId,
        playlist_id: Option<&PlaylistId>,
        query: &MediaListQuery,
    ) -> Result<()> {
        builder.push(" FROM media m LEFT JOIN users u ON m.creator_id = u.id AND u.deleted_at IS NULL WHERE m.room_id = ");
        builder.push_bind(room_id.as_i64());
        builder.push(" AND m.deleted_at IS NULL AND (m.creator_id IS NULL OR u.id IS NOT NULL)");
        builder.push(
            " AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = m.room_id AND r.deleted_at IS NULL)",
        );
        builder.push(" AND (m.playlist_id IS NULL OR EXISTS (SELECT 1 FROM playlists p WHERE p.id = m.playlist_id AND p.deleted_at IS NULL AND (p.creator_id IS NULL OR EXISTS (SELECT 1 FROM users pu WHERE pu.id = p.creator_id AND pu.deleted_at IS NULL))))");
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
            if let Some(pattern) = ilike_contains_pattern(search) {
                builder.push(" AND m.name ILIKE ");
                builder.push_bind(pattern);
                builder.push(" ESCAPE '\\'");
            }
        }
        if let Some(source_provider) = &query.source_provider {
            builder.push(" AND m.source_provider = ");
            builder.push_bind(source_provider.as_i16());
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
                    m.thumbnail_file_reference_id,
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
        Self::push_media_list_order_by(&mut builder, query);
        builder.push(" LIMIT ");
        builder.push_bind(limit);
        builder.push(" OFFSET ");
        builder.push_bind(offset);

        let rows = builder
            .build_query_as::<MediaListRow>()
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().map(Into::into).collect())
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
        let source_config = sqlx::types::Json(&media.source_config);
        let row = sqlx::query_as!(
            MediaRow,
            r#"
            INSERT INTO media (playlist_id, room_id, creator_id, name, description, position,
                              source_provider, source_config, provider_instance_name, added_at, updated_at, version)
            SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10, 0
            WHERE $3::BIGINT IS NULL OR EXISTS (
                SELECT 1
                FROM users
                WHERE users.id = $3::BIGINT AND users.deleted_at IS NULL
                FOR KEY SHARE
            )
             RETURNING id as "id: MediaId",
                       playlist_id as "playlist_id: PlaylistId",
                       room_id as "room_id: RoomId",
                       creator_id as "creator_id: UserId",
                       name,
                       description,
                       position,
                       source_provider as "source_provider: ProviderTypeName",
                       source_config as "source_config: crate::models::MediaSourceConfig",
                       NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                       cover_file_reference_id,
    thumbnail_file_reference_id,
                       added_at, updated_at, version
            "#,
            media.playlist_id.as_ref().map(PlaylistId::as_i64),
            media.room_id as RoomId,
            media.creator_id.as_ref().map(UserId::as_i64),
            media.name,
            media.description,
            media.position,
            media.source_provider.as_i16(),
            source_config as _,
            normalize_provider_instance_name(media.provider_instance_name.as_deref()),
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
        if items.is_empty() {
            return Ok(Vec::new());
        }
        Self::validate_create_batch_chunk_len(items.len())?;

        let provider_codes = items
            .iter()
            .map(|item| item.source_provider.as_i16())
            .collect::<Vec<_>>();
        let mut query_builder = sqlx::QueryBuilder::<sqlx::Postgres>::new(
            "INSERT INTO media (playlist_id, room_id, creator_id, name, description, position,
                               source_provider, source_config, provider_instance_name, added_at, updated_at, version) ",
        );
        query_builder.push_values(
            items.iter().zip(provider_codes.iter()),
            |mut row, (item, provider_code)| {
                row.push_bind(item.playlist_id.as_ref().map(PlaylistId::as_i64))
                    .push_bind(item.room_id)
                    .push_bind(item.creator_id.as_ref().map(UserId::as_i64))
                    .push_bind(&item.name)
                    .push_bind(&item.description)
                    .push_bind(item.position)
                    .push_bind(provider_code)
                    .push_bind(sqlx::types::Json(&item.source_config))
                    .push_bind(normalize_provider_instance_name(
                        item.provider_instance_name.as_deref(),
                    ))
                    .push_bind(item.added_at)
                    .push_bind(item.updated_at)
                    .push_bind(0_i32);
            },
        );
        query_builder.push(
            " RETURNING id, playlist_id, room_id, creator_id, name, description, position,
                       source_provider, source_config, NULLIF(provider_instance_name, '') AS provider_instance_name,
                       cover_file_reference_id,
    thumbnail_file_reference_id,
                       added_at, updated_at, version",
        );

        let rows = query_builder
            .build_query_as::<MediaRow>()
            .fetch_all(executor)
            .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    fn validate_create_batch_chunk_len(len: usize) -> Result<()> {
        /// Number of bind parameters per row.
        const PARAMS_PER_ROW: usize = 12;
        /// Maximum rows per INSERT statement (well within the 65535 parameter limit).
        const MAX_ROWS_PER_CHUNK: usize = 1000;

        if len > MAX_ROWS_PER_CHUNK {
            return Err(crate::Error::InvalidInput(format!(
                "Batch insert chunk too large: {len} rows exceed the {MAX_ROWS_PER_CHUNK} row limit \
                 ({} bind parameters). Use create_batch_chunked to split automatically.",
                len * PARAMS_PER_ROW,
            )));
        }

        Ok(())
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
        let row = sqlx::query_as!(
            MediaRow,
            r#"
            UPDATE media
            SET name = $2, description = $3, position = $4
             WHERE id = $1
               AND deleted_at IS NULL
               AND (creator_id IS NULL OR EXISTS (
                   SELECT 1 FROM users WHERE users.id = media.creator_id AND users.deleted_at IS NULL
               ))
             RETURNING id as "id: MediaId",
                       playlist_id as "playlist_id: PlaylistId",
                       room_id as "room_id: RoomId",
                       creator_id as "creator_id: UserId",
                       name,
                       description,
                       position,
                       source_provider as "source_provider: ProviderTypeName",
                       source_config as "source_config: crate::models::MediaSourceConfig",
                       NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                       cover_file_reference_id,
    thumbnail_file_reference_id,
                       added_at, updated_at, version
            "#,
            media.id as MediaId,
            media.name,
            media.description,
            media.position,
        )
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
    /// let Some(media) = repo.get_by_id(&id).await? else {
    ///     return Ok(());
    /// };
    /// let mut updated = media.clone();
    /// updated.name = "new_name".to_string();
    /// let Some(result) = repo.update_with_version(&updated, media.version).await? else {
    ///     return Ok(());
    /// };
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
        let row = sqlx::query_as!(
            MediaRow,
            r#"
            UPDATE media
            SET name = $2, description = $3, position = $4, version = version + 1
             WHERE id = $1
               AND deleted_at IS NULL
               AND version = $5
               AND (creator_id IS NULL OR EXISTS (
                   SELECT 1 FROM users u
                   WHERE u.id = media.creator_id AND u.deleted_at IS NULL
               ))
             RETURNING id as "id: MediaId",
                       playlist_id as "playlist_id: PlaylistId",
                       room_id as "room_id: RoomId",
                       creator_id as "creator_id: UserId",
                       name,
                       description,
                       position,
                       source_provider as "source_provider: ProviderTypeName",
                       source_config as "source_config: crate::models::MediaSourceConfig",
                       NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                       cover_file_reference_id,
    thumbnail_file_reference_id,
                       added_at, updated_at, version
            "#,
            media.id as MediaId,
            media.name,
            media.description,
            media.position,
            expected_version,
        )
        .fetch_optional(executor)
        .await?;

        Ok(row.map(Into::into))
    }

    /// Get media by ID
    pub async fn get_by_id(&self, media_id: &MediaId) -> Result<Option<Media>> {
        let row = sqlx::query_as!(
            MediaRow,
            r#"
            SELECT id as "id: MediaId",
                   playlist_id as "playlist_id: PlaylistId",
                   room_id as "room_id: RoomId",
                   creator_id as "creator_id: UserId",
                   name,
                   description,
                   position,
                   source_provider as "source_provider: ProviderTypeName",
                   source_config as "source_config: crate::models::MediaSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   cover_file_reference_id,
    thumbnail_file_reference_id,
                   added_at, updated_at, version
             FROM media m
             WHERE m.id = $1
               AND m.deleted_at IS NULL
               AND EXISTS (
                   SELECT 1 FROM rooms r
                   WHERE r.id = m.room_id AND r.deleted_at IS NULL
               )
               AND (m.creator_id IS NULL OR EXISTS (
                   SELECT 1 FROM users u
                   WHERE u.id = m.creator_id AND u.deleted_at IS NULL
               ))
               AND (m.playlist_id IS NULL OR EXISTS (
                   SELECT 1
                   FROM playlists p
                   WHERE p.id = m.playlist_id
                     AND p.deleted_at IS NULL
                     AND (p.creator_id IS NULL OR EXISTS (
                         SELECT 1 FROM users u
                         WHERE u.id = p.creator_id AND u.deleted_at IS NULL
                     ))
               ))
            "#,
            media_id as &MediaId,
        )
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
        let row = sqlx::query_as!(
            MediaRow,
            r#"
            SELECT id as "id: MediaId",
                   playlist_id as "playlist_id: PlaylistId",
                   room_id as "room_id: RoomId",
                   creator_id as "creator_id: UserId",
                   name,
                   description,
                   position,
                   source_provider as "source_provider: ProviderTypeName",
                   source_config as "source_config: crate::models::MediaSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   cover_file_reference_id,
    thumbnail_file_reference_id,
                   added_at, updated_at, version
             FROM media m
             WHERE m.room_id = $1
               AND m.id = $2
               AND m.deleted_at IS NULL
               AND EXISTS (
                   SELECT 1 FROM rooms r
                   WHERE r.id = m.room_id AND r.deleted_at IS NULL
               )
               AND (m.creator_id IS NULL OR EXISTS (
                   SELECT 1 FROM users u
                   WHERE u.id = m.creator_id AND u.deleted_at IS NULL
               ))
               AND (m.playlist_id IS NULL OR EXISTS (
                   SELECT 1
                   FROM playlists p
                   WHERE p.id = m.playlist_id
                     AND p.deleted_at IS NULL
                     AND (p.creator_id IS NULL OR EXISTS (
                         SELECT 1 FROM users u
                         WHERE u.id = p.creator_id AND u.deleted_at IS NULL
                     ))
               ))
            "#,
            room_id as &RoomId,
            media_id as &MediaId,
        )
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
        let row = sqlx::query_as!(
            MediaRow,
            r#"
            SELECT id as "id: MediaId",
                   playlist_id as "playlist_id: PlaylistId",
                   room_id as "room_id: RoomId",
                   creator_id as "creator_id: UserId",
                   name,
                   description,
                   position,
                   source_provider as "source_provider: ProviderTypeName",
                   source_config as "source_config: crate::models::MediaSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   cover_file_reference_id,
    thumbnail_file_reference_id,
                   added_at, updated_at, version
             FROM media m
             WHERE m.room_id = $1
               AND m.id = $2
               AND m.deleted_at IS NULL
               AND EXISTS (
                   SELECT 1 FROM rooms r
                   WHERE r.id = m.room_id AND r.deleted_at IS NULL
               )
               AND (m.creator_id IS NULL OR EXISTS (
                   SELECT 1 FROM users u
                   WHERE u.id = m.creator_id AND u.deleted_at IS NULL
               ))
               AND (m.playlist_id IS NULL OR EXISTS (
                   SELECT 1
                   FROM playlists p
                   WHERE p.id = m.playlist_id
                     AND p.deleted_at IS NULL
                     AND (p.creator_id IS NULL OR EXISTS (
                         SELECT 1 FROM users u
                         WHERE u.id = p.creator_id AND u.deleted_at IS NULL
                     ))
               ))
             FOR UPDATE
            "#,
            room_id as &RoomId,
            media_id as &MediaId,
        )
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
        let row = sqlx::query_as!(
            MediaRow,
            r#"
            UPDATE media
            SET cover_file_reference_id = $3,
                version = version + 1
             WHERE room_id = $1
               AND id = $2
               AND deleted_at IS NULL
               AND version = $4
               AND (creator_id IS NULL OR EXISTS (
                   SELECT 1 FROM users u
                   WHERE u.id = media.creator_id AND u.deleted_at IS NULL
               ))
             RETURNING id as "id: MediaId",
                       playlist_id as "playlist_id: PlaylistId",
                       room_id as "room_id: RoomId",
                       creator_id as "creator_id: UserId",
                       name,
                       description,
                       position,
                       source_provider as "source_provider: ProviderTypeName",
                       source_config as "source_config: crate::models::MediaSourceConfig",
                       NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                       cover_file_reference_id,
    thumbnail_file_reference_id,
                       added_at, updated_at, version
            "#,
            room_id as &RoomId,
            media_id as &MediaId,
            cover_file_reference_id,
            expected_version,
        )
        .fetch_optional(executor)
        .await?;

        Ok(row.map(Into::into))
    }

    pub async fn update_thumbnail_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        media_id: &MediaId,
        thumbnail_file_reference_id: Option<i64>,
        expected_version: i32,
        executor: E,
    ) -> Result<Option<Media>>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let row = sqlx::query_as!(
            MediaRow,
            r#"
            UPDATE media
            SET thumbnail_file_reference_id = $3,
                version = version + 1
             WHERE room_id = $1
               AND id = $2
               AND deleted_at IS NULL
               AND version = $4
               AND (creator_id IS NULL OR EXISTS (
                   SELECT 1 FROM users u
                   WHERE u.id = media.creator_id AND u.deleted_at IS NULL
               ))
             RETURNING id as "id: MediaId",
                       playlist_id as "playlist_id: PlaylistId",
                       room_id as "room_id: RoomId",
                       creator_id as "creator_id: UserId",
                       name,
                       description,
                       position,
                       source_provider as "source_provider: ProviderTypeName",
                       source_config as "source_config: crate::models::MediaSourceConfig",
                       NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                       cover_file_reference_id,
                       thumbnail_file_reference_id,
                       added_at, updated_at, version
            "#,
            room_id as &RoomId,
            media_id as &MediaId,
            thumbnail_file_reference_id,
            expected_version,
        )
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
        let rows = sqlx::query_as!(
            MediaRow,
            r#"
            SELECT id as "id: MediaId",
                   playlist_id as "playlist_id: PlaylistId",
                   room_id as "room_id: RoomId",
                   creator_id as "creator_id: UserId",
                   name,
                   description,
                   position,
                   source_provider as "source_provider: ProviderTypeName",
                   source_config as "source_config: crate::models::MediaSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   cover_file_reference_id,
    thumbnail_file_reference_id,
                   added_at, updated_at, version
             FROM media
            WHERE media.id = ANY($1)
              AND media.deleted_at IS NULL
              AND EXISTS (
                  SELECT 1 FROM rooms r
                  WHERE r.id = media.room_id AND r.deleted_at IS NULL
              )
              AND (media.creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u
                  WHERE u.id = media.creator_id AND u.deleted_at IS NULL
              ))
              AND (media.playlist_id IS NULL OR EXISTS (
                  SELECT 1
                  FROM playlists p
                  WHERE p.id = media.playlist_id
                    AND p.deleted_at IS NULL
                    AND (p.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u
                        WHERE u.id = p.creator_id AND u.deleted_at IS NULL
                    ))
              ))
            "#,
            &id_strs,
        )
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
        let rows = sqlx::query_as!(
            MediaRow,
            r#"
            SELECT id as "id: MediaId",
                   playlist_id as "playlist_id: PlaylistId",
                   room_id as "room_id: RoomId",
                   creator_id as "creator_id: UserId",
                   name,
                   description,
                   position,
                   source_provider as "source_provider: ProviderTypeName",
                   source_config as "source_config: crate::models::MediaSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   cover_file_reference_id,
    thumbnail_file_reference_id,
                   added_at, updated_at, version
             FROM media
             WHERE media.room_id = $1
               AND media.id = ANY($2)
               AND media.deleted_at IS NULL
               AND EXISTS (
                   SELECT 1 FROM rooms r
                   WHERE r.id = media.room_id AND r.deleted_at IS NULL
               )
               AND (media.creator_id IS NULL OR EXISTS (
                   SELECT 1 FROM users u
                   WHERE u.id = media.creator_id AND u.deleted_at IS NULL
               ))
               AND (media.playlist_id IS NULL OR EXISTS (
                   SELECT 1
                   FROM playlists p
                   WHERE p.id = media.playlist_id
                     AND p.deleted_at IS NULL
                     AND (p.creator_id IS NULL OR EXISTS (
                         SELECT 1 FROM users u
                         WHERE u.id = p.creator_id AND u.deleted_at IS NULL
                     ))
               ))
            "#,
            room_id as &RoomId,
            &id_strs,
        )
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
        let rows = sqlx::query_as!(
            MediaRow,
            r#"
            SELECT id as "id: MediaId",
                   playlist_id as "playlist_id: PlaylistId",
                   room_id as "room_id: RoomId",
                   creator_id as "creator_id: UserId",
                   name,
                   description,
                   position,
                   source_provider as "source_provider: ProviderTypeName",
                   source_config as "source_config: crate::models::MediaSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   cover_file_reference_id,
    thumbnail_file_reference_id,
                   added_at, updated_at, version
            FROM media
            WHERE media.room_id = $1
              AND media.deleted_at IS NULL
              AND EXISTS (
                  SELECT 1 FROM rooms r
                  WHERE r.id = media.room_id AND r.deleted_at IS NULL
              )
              AND (media.creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u
                  WHERE u.id = media.creator_id AND u.deleted_at IS NULL
              ))
              AND (media.playlist_id IS NULL OR EXISTS (
                  SELECT 1
                  FROM playlists p
                  WHERE p.id = media.playlist_id
                    AND p.deleted_at IS NULL
                    AND (p.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u
                        WHERE u.id = p.creator_id AND u.deleted_at IS NULL
                    ))
              ))
              AND media.playlist_id IS NOT DISTINCT FROM $2
            ORDER BY position ASC, id ASC
            "#,
            room_id as &RoomId,
            playlist_id.map(PlaylistId::as_i64),
        )
        .fetch_all(executor)
        .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Get media directly under the room root.
    pub async fn get_room_root(&self, room_id: &RoomId) -> Result<Vec<Media>> {
        let rows = sqlx::query_as!(
            MediaRow,
            r#"
            SELECT id as "id: MediaId",
                   playlist_id as "playlist_id: PlaylistId",
                   room_id as "room_id: RoomId",
                   creator_id as "creator_id: UserId",
                   name,
                   description,
                   position,
                   source_provider as "source_provider: ProviderTypeName",
                   source_config as "source_config: crate::models::MediaSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   cover_file_reference_id,
    thumbnail_file_reference_id,
                   added_at, updated_at, version
            FROM media
            WHERE media.room_id = $1
               AND media.deleted_at IS NULL
               AND EXISTS (
                   SELECT 1 FROM rooms r
                   WHERE r.id = media.room_id AND r.deleted_at IS NULL
               )
               AND (media.creator_id IS NULL OR EXISTS (
                   SELECT 1 FROM users u
                   WHERE u.id = media.creator_id AND u.deleted_at IS NULL
               ))
               AND media.playlist_id IS NULL
             ORDER BY position ASC
            "#,
            room_id as &RoomId,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Get media in a specific playlist.
    pub async fn get_by_playlist(&self, playlist_id: &PlaylistId) -> Result<Vec<Media>> {
        let rows = sqlx::query_as!(
            MediaRow,
            r#"
            SELECT id as "id: MediaId",
                   playlist_id as "playlist_id: PlaylistId",
                   room_id as "room_id: RoomId",
                   creator_id as "creator_id: UserId",
                   name,
                   description,
                   position,
                   source_provider as "source_provider: ProviderTypeName",
                   source_config as "source_config: crate::models::MediaSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   cover_file_reference_id,
    thumbnail_file_reference_id,
                   added_at, updated_at, version
             FROM media
             WHERE media.playlist_id = $1
               AND media.deleted_at IS NULL
               AND EXISTS (
                   SELECT 1 FROM rooms r
                   WHERE r.id = media.room_id AND r.deleted_at IS NULL
               )
               AND (media.creator_id IS NULL OR EXISTS (
                   SELECT 1 FROM users u
                   WHERE u.id = media.creator_id AND u.deleted_at IS NULL
               ))
               AND EXISTS (
                   SELECT 1
                   FROM playlists p
                   WHERE p.id = media.playlist_id
                     AND p.deleted_at IS NULL
                     AND (p.creator_id IS NULL OR EXISTS (
                         SELECT 1 FROM users u
                         WHERE u.id = p.creator_id AND u.deleted_at IS NULL
                     ))
               )
             ORDER BY position ASC
            "#,
            playlist_id as &PlaylistId,
        )
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
        let rows = sqlx::query_as!(
            MediaRow,
            r#"
            SELECT id as "id: MediaId",
                   playlist_id as "playlist_id: PlaylistId",
                   room_id as "room_id: RoomId",
                   creator_id as "creator_id: UserId",
                   name,
                   description,
                   position,
                   source_provider as "source_provider: ProviderTypeName",
                   source_config as "source_config: crate::models::MediaSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   cover_file_reference_id,
    thumbnail_file_reference_id,
                   added_at, updated_at, version
             FROM media
             WHERE media.room_id = $1
               AND media.playlist_id = $2
               AND media.deleted_at IS NULL
               AND EXISTS (
                   SELECT 1 FROM rooms r
                   WHERE r.id = media.room_id AND r.deleted_at IS NULL
               )
               AND (media.creator_id IS NULL OR EXISTS (
                   SELECT 1 FROM users u
                   WHERE u.id = media.creator_id AND u.deleted_at IS NULL
               ))
               AND EXISTS (
                   SELECT 1
                   FROM playlists p
                   WHERE p.id = media.playlist_id
                     AND p.deleted_at IS NULL
                     AND (p.creator_id IS NULL OR EXISTS (
                         SELECT 1 FROM users u
                         WHERE u.id = p.creator_id AND u.deleted_at IS NULL
                     ))
               )
             ORDER BY position ASC
            "#,
            room_id as &RoomId,
            playlist_id as &PlaylistId,
        )
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
        let limit = pagination.limit_i64()?;
        let offset = pagination.offset_i64()?;

        // Get total count
        let total = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM media
            WHERE media.playlist_id = $1
              AND media.deleted_at IS NULL
              AND EXISTS (
                  SELECT 1 FROM rooms r
                  WHERE r.id = media.room_id AND r.deleted_at IS NULL
              )
              AND (media.creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u
                  WHERE u.id = media.creator_id AND u.deleted_at IS NULL
              ))
              AND EXISTS (
                  SELECT 1
                  FROM playlists p
                  WHERE p.id = media.playlist_id
                    AND p.deleted_at IS NULL
                    AND (p.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u
                        WHERE u.id = p.creator_id AND u.deleted_at IS NULL
                    ))
              )
            "#,
            playlist_id.as_i64(),
        )
        .fetch_one(&self.pool)
        .await?;

        let rows = sqlx::query_as!(
            MediaRow,
            r#"
            SELECT id as "id: MediaId",
                   playlist_id as "playlist_id: PlaylistId",
                   room_id as "room_id: RoomId",
                   creator_id as "creator_id: UserId",
                   name,
                   description,
                   position,
                   source_provider as "source_provider: ProviderTypeName",
                   source_config as "source_config: crate::models::MediaSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   cover_file_reference_id,
    thumbnail_file_reference_id,
                   added_at, updated_at, version
             FROM media
             WHERE media.playlist_id = $1
               AND media.deleted_at IS NULL
               AND EXISTS (
                   SELECT 1 FROM rooms r
                   WHERE r.id = media.room_id AND r.deleted_at IS NULL
               )
               AND (media.creator_id IS NULL OR EXISTS (
                   SELECT 1 FROM users u
                   WHERE u.id = media.creator_id AND u.deleted_at IS NULL
               ))
               AND EXISTS (
                   SELECT 1
                   FROM playlists p
                   WHERE p.id = media.playlist_id
                     AND p.deleted_at IS NULL
                     AND (p.creator_id IS NULL OR EXISTS (
                         SELECT 1 FROM users u
                         WHERE u.id = p.creator_id AND u.deleted_at IS NULL
                     ))
               )
             ORDER BY position ASC
             LIMIT $2 OFFSET $3
            "#,
            playlist_id as &PlaylistId,
            limit,
            offset,
        )
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
        let limit = pagination.limit_i64()?;
        let offset = pagination.offset_i64()?;

        let total = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM media
            WHERE media.room_id = $1
              AND media.deleted_at IS NULL
              AND EXISTS (
                  SELECT 1 FROM rooms r
                  WHERE r.id = media.room_id AND r.deleted_at IS NULL
              )
              AND (media.creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u
                  WHERE u.id = media.creator_id AND u.deleted_at IS NULL
              ))
              AND media.playlist_id IS NULL
            "#,
            room_id.as_i64(),
        )
        .fetch_one(&self.pool)
        .await?;

        let rows = sqlx::query_as!(
            MediaRow,
            r#"
            SELECT id as "id: MediaId",
                   playlist_id as "playlist_id: PlaylistId",
                   room_id as "room_id: RoomId",
                   creator_id as "creator_id: UserId",
                   name,
                   description,
                   position,
                   source_provider as "source_provider: ProviderTypeName",
                   source_config as "source_config: crate::models::MediaSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   cover_file_reference_id,
    thumbnail_file_reference_id,
                   added_at, updated_at, version
            FROM media
            WHERE media.room_id = $1
               AND media.deleted_at IS NULL
               AND EXISTS (
                   SELECT 1 FROM rooms r
                   WHERE r.id = media.room_id AND r.deleted_at IS NULL
               )
               AND (media.creator_id IS NULL OR EXISTS (
                   SELECT 1 FROM users u
                   WHERE u.id = media.creator_id AND u.deleted_at IS NULL
               ))
               AND media.playlist_id IS NULL
             ORDER BY position ASC
             LIMIT $2 OFFSET $3
            "#,
            room_id as &RoomId,
            limit,
            offset,
        )
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
        let rows = sqlx::query_as!(
            MediaRow,
            r#"
            SELECT id as "id: MediaId",
                   playlist_id as "playlist_id: PlaylistId",
                   room_id as "room_id: RoomId",
                   creator_id as "creator_id: UserId",
                   name,
                   description,
                   position,
                   source_provider as "source_provider: ProviderTypeName",
                   source_config as "source_config: crate::models::MediaSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   cover_file_reference_id,
    thumbnail_file_reference_id,
                   added_at, updated_at, version
             FROM media
             WHERE media.playlist_id = $1
               AND media.deleted_at IS NULL
               AND EXISTS (
                   SELECT 1 FROM rooms r
                   WHERE r.id = media.room_id AND r.deleted_at IS NULL
               )
               AND (media.creator_id IS NULL OR EXISTS (
                   SELECT 1 FROM users u
                   WHERE u.id = media.creator_id AND u.deleted_at IS NULL
               ))
               AND EXISTS (
                   SELECT 1
                   FROM playlists p
                   WHERE p.id = media.playlist_id
                     AND p.deleted_at IS NULL
                     AND (p.creator_id IS NULL OR EXISTS (
                         SELECT 1 FROM users u
                         WHERE u.id = p.creator_id AND u.deleted_at IS NULL
                     ))
               )
             ORDER BY position ASC
             LIMIT $2 OFFSET $3
            "#,
            playlist_id as &PlaylistId,
            limit,
            offset,
        )
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
        let rows = sqlx::query_as!(
            MediaRow,
            r#"
            SELECT id as "id: MediaId",
                   playlist_id as "playlist_id: PlaylistId",
                   room_id as "room_id: RoomId",
                   creator_id as "creator_id: UserId",
                   name,
                   description,
                   position,
                   source_provider as "source_provider: ProviderTypeName",
                   source_config as "source_config: crate::models::MediaSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   cover_file_reference_id,
    thumbnail_file_reference_id,
                   added_at, updated_at, version
            FROM media
            WHERE media.room_id = $1
               AND media.deleted_at IS NULL
               AND EXISTS (
                   SELECT 1 FROM rooms r
                   WHERE r.id = media.room_id AND r.deleted_at IS NULL
               )
               AND (media.creator_id IS NULL OR EXISTS (
                   SELECT 1 FROM users u
                   WHERE u.id = media.creator_id AND u.deleted_at IS NULL
               ))
               AND media.playlist_id IS NULL
             ORDER BY position ASC
             LIMIT $2 OFFSET $3
            "#,
            room_id as &RoomId,
            limit,
            offset,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Delete media from playlist
    pub async fn delete(&self, media_id: &MediaId) -> Result<bool> {
        let result = sqlx::query!(
            r#"
            UPDATE media
               SET deleted_at = CURRENT_TIMESTAMP,
                   deletion_source = $2,
                   version = version + 1
             WHERE id = $1 AND deleted_at IS NULL
            "#,
            media_id.as_i64(),
            DeletionSource::User as DeletionSource,
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Delete all media in a playlist.
    pub async fn delete_playlist(&self, playlist_id: &PlaylistId) -> Result<usize> {
        let result = sqlx::query!(
            r#"
            UPDATE media
               SET deleted_at = CURRENT_TIMESTAMP,
                   deletion_source = $2,
                   version = version + 1
             WHERE playlist_id = $1 AND deleted_at IS NULL
            "#,
            playlist_id.as_i64(),
            DeletionSource::User as DeletionSource,
        )
        .execute(&self.pool)
        .await?;

        rows_affected_to_usize(result.rows_affected(), "delete playlist media")
    }

    /// Delete all media directly under the room root.
    pub async fn delete_room_root(&self, room_id: &RoomId) -> Result<usize> {
        let result = sqlx::query!(
            r#"
            UPDATE media
               SET deleted_at = CURRENT_TIMESTAMP,
                   deletion_source = $2,
                   version = version + 1
             WHERE room_id = $1 AND deleted_at IS NULL
               AND playlist_id IS NULL
            "#,
            room_id.as_i64(),
            DeletionSource::User as DeletionSource,
        )
        .execute(&self.pool)
        .await?;

        rows_affected_to_usize(result.rows_affected(), "delete room root media")
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
            UPDATE media
               SET deleted_at = CURRENT_TIMESTAMP,
                   deletion_source = $2,
                   version = version + 1
             WHERE id = ANY($1) AND deleted_at IS NULL
            "#,
            &id_strs,
            DeletionSource::User as DeletionSource,
        )
        .execute(executor)
        .await?;

        rows_affected_to_usize(result.rows_affected(), "delete media batch")
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
                let step = gap / usize_to_f64(count + 1)?;
                if !step.is_finite() || step <= Self::MIN_ORDER_GAP {
                    return None;
                }
                let mut positions = Vec::with_capacity(count);
                for index in 1..=count {
                    let position = previous + step * usize_to_f64(index)?;
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
                let start = Self::ORDER_STEP.mul_add(-usize_to_f64(count)?, next);
                if !start.is_finite() {
                    return None;
                }
                let mut positions = Vec::with_capacity(count);
                for index in 0..count {
                    let position = Self::ORDER_STEP.mul_add(usize_to_f64(index)?, start);
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
                    let position = Self::ORDER_STEP.mul_add(usize_to_f64(index)?, previous);
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
                    positions.push(Self::ORDER_STEP * usize_to_f64(index)?);
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
        let moved_rows = sqlx::query_as!(
            MediaRow,
            r#"
            SELECT id as "id: MediaId",
                   playlist_id as "playlist_id: PlaylistId",
                   room_id as "room_id: RoomId",
                   creator_id as "creator_id: UserId",
                   name,
                   description,
                   position,
                   source_provider as "source_provider: ProviderTypeName",
                   source_config as "source_config: crate::models::MediaSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   cover_file_reference_id,
    thumbnail_file_reference_id,
                   added_at, updated_at, version
            FROM media
            WHERE media.room_id = $1
              AND media.id = ANY($2)
              AND media.deleted_at IS NULL
              AND (media.creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u
                  WHERE u.id = media.creator_id AND u.deleted_at IS NULL
              ))
              AND (media.playlist_id IS NULL OR EXISTS (
                  SELECT 1
                  FROM playlists p
                  WHERE p.id = media.playlist_id
                    AND p.deleted_at IS NULL
                    AND (p.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u
                        WHERE u.id = p.creator_id AND u.deleted_at IS NULL
                    ))
              ))
            FOR UPDATE
            "#,
            room_id as &RoomId,
            &media_id_strs,
        )
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
            let anchor_media: Media = sqlx::query_as!(
                MediaRow,
                r#"
                SELECT id as "id: MediaId",
                       playlist_id as "playlist_id: PlaylistId",
                       room_id as "room_id: RoomId",
                       creator_id as "creator_id: UserId",
                       name,
                       description,
                       position,
                       source_provider as "source_provider: ProviderTypeName",
                       source_config as "source_config: crate::models::MediaSourceConfig",
                       NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                       cover_file_reference_id,
    thumbnail_file_reference_id,
                       added_at, updated_at, version
                FROM media
                WHERE room_id = $1 AND id = $2 AND deleted_at IS NULL
                FOR UPDATE
                "#,
                room_id as &RoomId,
                anchor_id as &MediaId,
            )
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
            (None, None) => None,
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
                  AND deleted_at IS NULL
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
                let row = sqlx::query_as!(
                    MediaRow,
                    r#"
                    UPDATE media
                    SET playlist_id = $2,
                        position = $3,
                        version = version + 1
                    WHERE id = $1 AND deleted_at IS NULL
                    RETURNING id as "id: MediaId",
                              playlist_id as "playlist_id: PlaylistId",
                              room_id as "room_id: RoomId",
                              creator_id as "creator_id: UserId",
                              name,
                              description,
                              position,
                              source_provider as "source_provider: ProviderTypeName",
                              source_config as "source_config: crate::models::MediaSourceConfig",
                              NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                              cover_file_reference_id,
    thumbnail_file_reference_id,
                              added_at, updated_at, version
                    "#,
                    media.id as MediaId,
                    effective_target_playlist_id
                        .as_ref()
                        .map(PlaylistId::as_i64),
                    position,
                )
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
              AND deleted_at IS NULL
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
              AND deleted_at IS NULL
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
              AND deleted_at IS NULL
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
            let position = Self::ORDER_STEP
                * usize_to_f64(index + 1).ok_or_else(|| {
                    crate::Error::Internal("media order index exceeds u32::MAX".to_string())
                })?;
            sqlx::query!(
                "UPDATE media SET position = $2, version = version + 1 WHERE id = $1 AND deleted_at IS NULL",
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
              AND deleted_at IS NULL
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

        let moved: Media = sqlx::query_as!(
            MediaRow,
            r#"
            SELECT id as "id: MediaId",
                   playlist_id as "playlist_id: PlaylistId",
                   room_id as "room_id: RoomId",
                   creator_id as "creator_id: UserId",
                   name,
                   description,
                   position,
                   source_provider as "source_provider: ProviderTypeName",
                   source_config as "source_config: crate::models::MediaSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   cover_file_reference_id,
    thumbnail_file_reference_id,
                   added_at, updated_at, version
            FROM media
            WHERE media.room_id = $1
              AND media.id = $2
              AND media.deleted_at IS NULL
              AND (media.creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u
                  WHERE u.id = media.creator_id AND u.deleted_at IS NULL
              ))
              AND (media.playlist_id IS NULL OR EXISTS (
                  SELECT 1
                  FROM playlists p
                  WHERE p.id = media.playlist_id
                    AND p.deleted_at IS NULL
                    AND (p.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u
                        WHERE u.id = p.creator_id AND u.deleted_at IS NULL
                    ))
              ))
            FOR UPDATE
            "#,
            room_id as &RoomId,
            media_id as &MediaId,
        )
        .fetch_optional(&mut **tx)
        .await?
        .map(Into::into)
        .ok_or_else(|| crate::Error::NotFound("Media not found".to_string()))?;

        let anchor: Media = sqlx::query_as!(
            MediaRow,
            r#"
            SELECT id as "id: MediaId",
                   playlist_id as "playlist_id: PlaylistId",
                   room_id as "room_id: RoomId",
                   creator_id as "creator_id: UserId",
                   name,
                   description,
                   position,
                   source_provider as "source_provider: ProviderTypeName",
                   source_config as "source_config: crate::models::MediaSourceConfig",
                   NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                   cover_file_reference_id,
    thumbnail_file_reference_id,
                   added_at, updated_at, version
            FROM media
            WHERE media.room_id = $1
              AND media.id = $2
              AND media.deleted_at IS NULL
              AND (media.creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u
                  WHERE u.id = media.creator_id AND u.deleted_at IS NULL
              ))
              AND (media.playlist_id IS NULL OR EXISTS (
                  SELECT 1
                  FROM playlists p
                  WHERE p.id = media.playlist_id
                    AND p.deleted_at IS NULL
                    AND (p.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u
                        WHERE u.id = p.creator_id AND u.deleted_at IS NULL
                    ))
              ))
            FOR UPDATE
            "#,
            room_id as &RoomId,
            anchor_id as &MediaId,
        )
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
                r#"SELECT position AS "position!" FROM media WHERE id = $1 AND deleted_at IS NULL FOR UPDATE"#,
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
                let row = sqlx::query_as!(
                    MediaRow,
                    r#"
                    UPDATE media
                    SET position = $2, version = version + 1
                    WHERE id = $1 AND deleted_at IS NULL
                    RETURNING id as "id: MediaId",
                              playlist_id as "playlist_id: PlaylistId",
                              room_id as "room_id: RoomId",
                              creator_id as "creator_id: UserId",
                              name,
                              description,
                              position,
                              source_provider as "source_provider: ProviderTypeName",
                              source_config as "source_config: crate::models::MediaSourceConfig",
                              NULLIF(provider_instance_name, '') AS "provider_instance_name?",
                              cover_file_reference_id,
    thumbnail_file_reference_id,
                              added_at, updated_at, version
                    "#,
                    moved.id as MediaId,
                    position,
                )
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
            SELECT COUNT(*) AS "count!"
            FROM media m
            WHERE m.playlist_id = $1
              AND m.deleted_at IS NULL
              AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = m.room_id AND r.deleted_at IS NULL)
              AND (m.creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u WHERE u.id = m.creator_id AND u.deleted_at IS NULL
              ))
              AND EXISTS (
                  SELECT 1 FROM playlists p
                  WHERE p.id = m.playlist_id
                    AND p.deleted_at IS NULL
                    AND (p.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u WHERE u.id = p.creator_id AND u.deleted_at IS NULL
                    ))
              )
            "#,
            playlist_id.as_i64(),
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(count)
    }

    /// Count all media items.
    pub async fn count_all(&self) -> Result<i64> {
        let count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM media m
            WHERE m.deleted_at IS NULL
              AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = m.room_id AND r.deleted_at IS NULL)
              AND (m.creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u WHERE u.id = m.creator_id AND u.deleted_at IS NULL
              ))
              AND (m.playlist_id IS NULL OR EXISTS (
                  SELECT 1
                  FROM playlists p
                  WHERE p.id = m.playlist_id
                    AND p.deleted_at IS NULL
                    AND (p.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u WHERE u.id = p.creator_id AND u.deleted_at IS NULL
                    ))
              ))
            "#
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(count)
    }

    /// Count media items in a playlist, scoped to a room.
    pub async fn count_by_room_and_playlist(
        &self,
        room_id: &RoomId,
        playlist_id: &PlaylistId,
    ) -> Result<i64> {
        let count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM media m
            WHERE m.room_id = $1
              AND m.playlist_id = $2
              AND m.deleted_at IS NULL
              AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = m.room_id AND r.deleted_at IS NULL)
              AND (m.creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u WHERE u.id = m.creator_id AND u.deleted_at IS NULL
              ))
              AND EXISTS (
                  SELECT 1
                  FROM playlists p
                  WHERE p.id = m.playlist_id
                    AND p.deleted_at IS NULL
                    AND (p.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u WHERE u.id = p.creator_id AND u.deleted_at IS NULL
                    ))
              )
            "#,
            room_id.as_i64(),
            playlist_id.as_i64()
        )
        .fetch_one(&self.pool)
        .await?;

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
              AND m.deleted_at IS NULL
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
            FROM media m
            WHERE m.room_id = $1
              AND m.deleted_at IS NULL
              AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = m.room_id AND r.deleted_at IS NULL)
              AND (m.creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u WHERE u.id = m.creator_id AND u.deleted_at IS NULL
              ))
              AND m.playlist_id IS NULL
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
            SELECT m.playlist_id AS "playlist_id!: PlaylistId", COUNT(*) AS "cnt!"
            FROM media m
            WHERE m.playlist_id = ANY($1)
              AND m.deleted_at IS NULL
              AND EXISTS (SELECT 1 FROM rooms r WHERE r.id = m.room_id AND r.deleted_at IS NULL)
              AND (m.creator_id IS NULL OR EXISTS (
                  SELECT 1 FROM users u WHERE u.id = m.creator_id AND u.deleted_at IS NULL
              ))
              AND EXISTS (
                  SELECT 1
                  FROM playlists p
                  WHERE p.id = m.playlist_id
                    AND p.deleted_at IS NULL
                    AND (p.creator_id IS NULL OR EXISTS (
                        SELECT 1 FROM users u WHERE u.id = p.creator_id AND u.deleted_at IS NULL
                    ))
              )
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
              AND m.deleted_at IS NULL
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
#[path = "media_tests.rs"]
mod tests;
